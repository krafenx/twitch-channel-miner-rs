use crate::bet::{BetOutcomeEvaluation, EventPredictionRecord, PredictionEvent};
use crate::chat::{ChatPool, ChatPresence};
use crate::constants::{MAX_WEBSOCKET_TOPICS, WEBSOCKET_URL};
use crate::entities::drop::parse_twitch_datetime;
use crate::entities::{CommunityGoal, RaidInfo, SharedStreamer};
use crate::error::{MinerError, Result};
use crate::logger::LogLevel;
use crate::settings::{millify_u64, MinerEvent};
use crate::twitch::gql::{self, GqlHeaders};
use crate::twitch::TwitchClient;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PubsubTopic {
    pub name: String,
    pub target_id: String,
    pub is_user_topic: bool,
}

impl PubsubTopic {
    pub fn user(name: &str, user_id: &str) -> Self {
        Self {
            name: name.to_string(),
            target_id: user_id.to_string(),
            is_user_topic: true,
        }
    }

    pub fn channel(name: &str, channel_id: &str) -> Self {
        Self {
            name: name.to_string(),
            target_id: channel_id.to_string(),
            is_user_topic: false,
        }
    }

    pub fn full_topic(&self) -> String {
        format!("{}.{}", self.name, self.target_id)
    }
}

pub struct WebSocketPool {
    twitch: Arc<TwitchClient>,
    auth_token: String,
    user_id: String,
    username: String,
    streamers: Arc<Mutex<HashMap<String, SharedStreamer>>>,
    chat_pool: Option<Arc<ChatPool>>,
    default_chat_presence: ChatPresence,
    events_predictions: Arc<Mutex<HashMap<String, Arc<StdMutex<EventPredictionRecord>>>>>,
}

impl WebSocketPool {
    /// Legacy constructor preserved for API compatibility
    pub fn new(
        auth_token: String,
        user_id: String,
        device_id: String,
        client: reqwest::Client,
    ) -> Self {
        let mut auth = crate::auth::TwitchAuth::new("user").unwrap();
        auth.access_token = Some(auth_token.clone());
        auth.user_id = Some(user_id.clone());
        auth.device_id = device_id;
        auth.client = client;
        let twitch = Arc::new(TwitchClient::new(auth));
        Self::with_client(twitch, user_id, String::new(), None, ChatPresence::Online)
    }

    pub fn with_client(
        twitch: Arc<TwitchClient>,
        user_id: String,
        username: String,
        chat_pool: Option<Arc<ChatPool>>,
        default_chat_presence: ChatPresence,
    ) -> Self {
        let auth_token = twitch.auth.access_token.clone().unwrap_or_default();
        Self {
            twitch,
            auth_token,
            user_id,
            username,
            streamers: Arc::new(Mutex::new(HashMap::new())),
            chat_pool,
            default_chat_presence,
            events_predictions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn add_streamer(&self, streamer: SharedStreamer) {
        let mut map = self.streamers.lock().await;
        if let Some(cid) = &streamer.channel_id {
            map.insert(cid.clone(), streamer);
        }
    }

    pub async fn prediction_records(&self) -> Vec<EventPredictionRecord> {
        let map = self.events_predictions.lock().await;
        map.values()
            .filter_map(|arc| arc.lock().ok().map(|g| g.clone()))
            .collect()
    }

    async fn build_topics(&self) -> Vec<PubsubTopic> {
        let streamers = self.streamers.lock().await;
        let mut topics = Vec::new();
        let mut seen = std::collections::HashSet::new();

        let mut push_unique = |t: PubsubTopic| {
            if seen.insert(t.full_topic()) {
                topics.push(t);
            }
        };

        push_unique(PubsubTopic::user("community-points-user-v1", &self.user_id));

        let any_predictions = streamers.values().any(|s| s.settings.make_predictions);
        if any_predictions {
            push_unique(PubsubTopic::user("predictions-user-v1", &self.user_id));
        }

        for (channel_id, streamer) in streamers.iter() {
            push_unique(PubsubTopic::channel("video-playback-by-id", channel_id));
            if streamer.settings.follow_raid {
                push_unique(PubsubTopic::channel("raid", channel_id));
            }
            if streamer.settings.make_predictions {
                push_unique(PubsubTopic::channel(
                    "predictions-channel-v1",
                    channel_id,
                ));
            }
            if streamer.settings.claim_moments {
                push_unique(PubsubTopic::channel(
                    "community-moments-channel-v1",
                    channel_id,
                ));
            }
            if streamer.settings.community_goals {
                push_unique(PubsubTopic::channel(
                    "community-points-channel-v1",
                    channel_id,
                ));
            }
        }
        topics
    }

    /// Spawns a pool of WebSocket connections, respecting Twitch's 50-topics-per-connection limit.
    pub async fn run(self: Arc<Self>) -> Result<()> {
        let all_topics = self.build_topics().await;
        if all_topics.is_empty() {
            return Ok(());
        }

        let chunks: Vec<Vec<PubsubTopic>> = all_topics
            .chunks(MAX_WEBSOCKET_TOPICS)
            .map(|c| c.to_vec())
            .collect();

        self.twitch.log_info(
            "🔌",
            &format!(
                "Starting PubSub pool: {} topics across {} WebSocket connection(s) (limit {} per socket)",
                all_topics.len(),
                chunks.len(),
                MAX_WEBSOCKET_TOPICS
            ),
        );

        let mut set = tokio::task::JoinSet::new();
        for (ws_index, chunk) in chunks.into_iter().enumerate() {
            let pool = Arc::clone(&self);
            set.spawn(async move {
                pool.run_single_socket(ws_index, chunk).await;
            });
        }

        while let Some(res) = set.join_next().await {
            if let Err(e) = res {
                error!("PubSub socket task panicked: {}", e);
            }
        }
        Ok(())
    }

    async fn run_single_socket(self: Arc<Self>, ws_index: usize, topics: Vec<PubsubTopic>) {
        loop {
            match self.connect_and_listen_socket(ws_index, &topics).await {
                Ok(()) => warn!(
                    "#{} - WebSocket closed, reconnecting in 10s...",
                    ws_index
                ),
                Err(e) => error!(
                    "#{} - WebSocket error: {}, reconnecting in 15s...",
                    ws_index, e
                ),
            }
            self.twitch.wait_for_internet().await;
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }

    async fn connect_and_listen_socket(
        &self,
        ws_index: usize,
        topics: &[PubsubTopic],
    ) -> Result<()> {
        debug!("#{} - Connecting to Twitch PubSub...", ws_index);
        let (ws_stream, _) = connect_async(WEBSOCKET_URL)
            .await
            .map_err(|e| MinerError::WebSocket(e.to_string()))?;

        let (mut write, mut read) = ws_stream.split();

        // Group user topics (with auth_token) and channel topics, or send auth_token when needed
        let user_topics: Vec<String> = topics
            .iter()
            .filter(|t| t.is_user_topic)
            .map(|t| t.full_topic())
            .collect();
        let channel_topics: Vec<String> = topics
            .iter()
            .filter(|t| !t.is_user_topic)
            .map(|t| t.full_topic())
            .collect();

        if !user_topics.is_empty() {
            let msg = json!({
                "type": "LISTEN",
                "nonce": Uuid::new_v4().to_string().replace('-', ""),
                "data": {
                    "topics": user_topics,
                    "auth_token": self.auth_token
                }
            });
            write
                .send(Message::Text(msg.to_string()))
                .await
                .map_err(|e| MinerError::WebSocket(e.to_string()))?;
        }

        if !channel_topics.is_empty() {
            let msg = json!({
                "type": "LISTEN",
                "nonce": Uuid::new_v4().to_string().replace('-', ""),
                "data": {
                    "topics": channel_topics,
                    "auth_token": self.auth_token
                }
            });
            write
                .send(Message::Text(msg.to_string()))
                .await
                .map_err(|e| MinerError::WebSocket(e.to_string()))?;
        }

        info!(
            "#{} - Subscribed to {} PubSub topics",
            ws_index,
            topics.len()
        );

        let last_pong = Arc::new(StdMutex::new(Instant::now()));
        let last_pong_ping = Arc::clone(&last_pong);
        let (reconnect_tx, mut reconnect_rx) = tokio::sync::mpsc::channel::<()>(1);

        let mut write_ping = write;
        let ping_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(27));
            loop {
                interval.tick().await;
                let pong_elapsed = last_pong_ping.lock().unwrap().elapsed();
                if pong_elapsed > Duration::from_secs(300) {
                    warn!(
                        "#{} - Last PONG was received more than 5 minutes ago, forcing reconnect",
                        ws_index
                    );
                    let _ = reconnect_tx.try_send(());
                    break;
                }
                if write_ping
                    .send(Message::Text(r#"{"type":"PING"}"#.to_string()))
                    .await
                    .is_err()
                {
                    let _ = reconnect_tx.try_send(());
                    break;
                }
            }
        });

        let mut recent_dedup: VecDeque<(String, String)> = VecDeque::with_capacity(32);

        loop {
            tokio::select! {
                _ = reconnect_rx.recv() => {
                    break;
                }
                maybe_msg = read.next() => {
                    let Some(msg) = maybe_msg else {
                        break;
                    };
                    match msg {
                        Ok(Message::Text(text)) => {
                            if let Ok(data) = serde_json::from_str::<Value>(&text) {
                                let should_reconnect = self
                                    .handle_message(ws_index, &data, &last_pong, &mut recent_dedup)
                                    .await;
                                if should_reconnect {
                                    break;
                                }
                            }
                        }
                        Ok(Message::Close(_)) => {
                            info!("#{} - WebSocket closed by server", ws_index);
                            break;
                        }
                        Err(e) => {
                            error!("#{} - WS read error: {}", ws_index, e);
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        ping_handle.abort();
        Ok(())
    }

    /// Returns `true` if the socket should immediately reconnect (`RECONNECT`).
    async fn handle_message(
        &self,
        ws_index: usize,
        data: &Value,
        last_pong: &Arc<StdMutex<Instant>>,
        recent_dedup: &mut VecDeque<(String, String)>,
    ) -> bool {
        let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match msg_type {
            "PONG" => {
                *last_pong.lock().unwrap() = Instant::now();
                debug!("#{} - PubSub PONG", ws_index);
                false
            }
            "RESPONSE" => {
                if let Some(err) = data.get("error").and_then(|v| v.as_str()) {
                    if !err.is_empty() {
                        self.twitch.log_warn(&format!(
                            "#{} - Error while trying to listen for a topic: {}",
                            ws_index, err
                        ));
                        if err.contains("ERR_BADAUTH") {
                            self.twitch.log_warn(
                                "Received ERR_BADAUTH — your saved session token may be invalid.",
                            );
                        }
                    }
                }
                false
            }
            "RECONNECT" => {
                self.twitch.log_info(
                    "🔄",
                    &format!("#{} - PubSub requested RECONNECT", ws_index),
                );
                true
            }
            "MESSAGE" => {
                if let Some(payload) = data.get("data") {
                    let topic = payload.get("topic").and_then(|v| v.as_str()).unwrap_or("");
                    let message = payload
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if let Ok(inner) = serde_json::from_str::<Value>(message) {
                        // Deduplicate identical messages across reconnects / sockets
                        let inner_type = inner
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let ts = inner
                            .get("data")
                            .and_then(|d| d.get("timestamp"))
                            .or_else(|| inner.get("timestamp"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !ts.is_empty() {
                            let sub_id = inner["data"]["claim"]["id"]
                                .as_str()
                                .or_else(|| inner["data"]["point_gain"]["channel_id"].as_str())
                                .or_else(|| inner["data"]["balance"]["channel_id"].as_str())
                                .or_else(|| inner["data"]["channel_id"].as_str())
                                .or_else(|| inner["data"]["event"]["id"].as_str())
                                .or_else(|| inner["data"]["prediction"]["id"].as_str())
                                .unwrap_or("");
                            let ident = format!("{inner_type}.{topic}.{sub_id}");
                            if recent_dedup
                                .iter()
                                .any(|(prev_ts, prev_id)| prev_ts == &ts && prev_id == &ident)
                            {
                                return false;
                            }
                            if recent_dedup.len() >= 32 {
                                recent_dedup.pop_front();
                            }
                            recent_dedup.push_back((ts, ident));
                        }
                        self.handle_pubsub_topic(topic, &inner).await;
                    }
                }
                false
            }
            _ => {
                debug!("#{} - Unhandled PubSub type: {}", ws_index, msg_type);
                false
            }
        }
    }

    async fn find_streamer_by_channel_id(&self, channel_id: &str) -> Option<SharedStreamer> {
        self.streamers.lock().await.get(channel_id).cloned()
    }

    fn sync_streamer_chat(&self, streamer: &SharedStreamer) {
        let Some(chat_pool) = &self.chat_pool else {
            return;
        };
        let online = streamer.is_online();
        match streamer.settings.chat.unwrap_or(self.default_chat_presence) {
            ChatPresence::Always => chat_pool.ensure_joined(&streamer.username),
            ChatPresence::Online if online => chat_pool.ensure_joined(&streamer.username),
            ChatPresence::Offline if !online => chat_pool.ensure_joined(&streamer.username),
            _ => chat_pool.leave(&streamer.username),
        }
    }

    async fn handle_pubsub_topic(&self, topic: &str, data: &Value) {
        let less = self.twitch.is_less();

        if topic.starts_with("community-points-user-v1.") {
            let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let channel_id = data["data"]["channel_id"]
                .as_str()
                .or_else(|| data["data"]["balance"]["channel_id"].as_str())
                .or_else(|| data["data"]["claim"]["channel_id"].as_str())
                .unwrap_or("");
            let Some(streamer) = self.find_streamer_by_channel_id(channel_id).await else {
                return;
            };

            if msg_type == "points-earned" || msg_type == "points-spent" {
                if let Some(exact_bal) = data["data"]["balance"]["balance"].as_u64() {
                    streamer.set_channel_points(exact_bal);
                    if let Some(analytics) = &self.twitch.analytics {
                        let ev_kind = if msg_type == "points-earned" {
                            data["data"]["point_gain"]["reason_code"]
                                .as_str()
                                .unwrap_or("Watch")
                        } else {
                            "Spent"
                        };
                        analytics.record(
                            &self.username,
                            &streamer.username,
                            exact_bal as i64,
                            ev_kind,
                        );
                    }
                }
            }

            match msg_type {
                "points-earned" => {
                    if let Some(earned) = data["data"]["point_gain"]["total_points"].as_i64() {
                        let reason_code = data["data"]["point_gain"]["reason_code"]
                            .as_str()
                            .unwrap_or("UNKNOWN");
                        if data["data"]["balance"]["balance"].as_u64().is_none() {
                            let next = (streamer.channel_points() as i64 + earned).max(0) as u64;
                            streamer.set_channel_points(next);
                        }
                        streamer.update_history(reason_code, earned, 1);

                        let msg = format!(
                            "+{} → {} - Reason: {}.",
                            earned,
                            streamer.display_str(less),
                            reason_code
                        );
                        let ev = MinerEvent::from_reason_code(reason_code);
                        self.twitch.log_event(LogLevel::Info, "🚀", ev, &msg).await;

                        if let Some(analytics) = &self.twitch.analytics {
                            analytics.record_annotation(
                                &self.username,
                                &streamer.username,
                                reason_code,
                                &format!("+{earned} - {reason_code}"),
                            );
                        }
                    }
                }
                "claim-available" => {
                    if let Some(claim_id) = data["data"]["claim"]["id"].as_str().map(String::from) {
                        let twitch = Arc::clone(&self.twitch);
                        let streamer = Arc::clone(&streamer);
                        tokio::spawn(async move {
                            twitch.claim_bonus(&streamer, &claim_id).await;
                        });
                    }
                }
                _ => debug!("community-points-user-v1 event: {}", msg_type),
            }
        } else if topic.starts_with("video-playback-by-id.") {
            let channel_id = topic.strip_prefix("video-playback-by-id.").unwrap_or("");
            let Some(streamer) = self.find_streamer_by_channel_id(channel_id).await else {
                return;
            };
            let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match msg_type {
                "stream-up" => {
                    streamer.mark_stream_up();
                }
                "stream-down" => {
                    if streamer.set_online_with_transition(false, false) {
                        self.sync_streamer_chat(&streamer);
                        let msg = format!("{} is Offline!", streamer.display_str(less));
                        self.twitch
                            .log_event(LogLevel::Info, "😴", Some(MinerEvent::StreamerOffline), &msg)
                            .await;
                    }
                }
                "viewcount" => {
                    if !streamer.is_online()
                        && streamer.stream_up_elapsed()
                        && !streamer.is_in_offline_cooldown()
                    {
                        let twitch = Arc::clone(&self.twitch);
                        let streamer = Arc::clone(&streamer);
                        let chat_pool = self.chat_pool.clone();
                        let default_chat = self.default_chat_presence;
                        tokio::spawn(async move {
                            let _ = twitch.update_stream_status(&streamer).await;
                            if let Some(cp) = chat_pool {
                                let online = streamer.is_online();
                                match streamer.settings.chat.unwrap_or(default_chat) {
                                    ChatPresence::Always => cp.ensure_joined(&streamer.username),
                                    ChatPresence::Online if online => {
                                        cp.ensure_joined(&streamer.username)
                                    }
                                    ChatPresence::Offline if !online => {
                                        cp.ensure_joined(&streamer.username)
                                    }
                                    _ => cp.leave(&streamer.username),
                                }
                            }
                        });
                    }
                }
                _ => {}
            }
        } else if topic.starts_with("raid.") {
            let channel_id = topic.strip_prefix("raid.").unwrap_or("");
            let Some(streamer) = self.find_streamer_by_channel_id(channel_id).await else {
                return;
            };
            if !streamer.settings.follow_raid {
                return;
            }
            let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if msg_type == "raid_update_v2" || data.get("raid").is_some() {
                let raid_obj = data.get("raid").unwrap_or(data);
                let Some(raid_id) = raid_obj.get("id").and_then(|v| v.as_str()).map(String::from)
                else {
                    return;
                };
                let target_login = raid_obj
                    .get("target_login")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();

                let new_raid = RaidInfo {
                    raid_id: raid_id.clone(),
                    target_login: target_login.clone(),
                };
                let is_new = {
                    let mut guard = streamer.last_raid.lock().unwrap();
                    if guard.as_ref() == Some(&new_raid) {
                        false
                    } else {
                        *guard = Some(new_raid);
                        true
                    }
                };
                if is_new {
                    let twitch = Arc::clone(&self.twitch);
                    let streamer = Arc::clone(&streamer);
                    tokio::spawn(async move {
                        let Some(token) = twitch.auth.access_token.as_ref().cloned() else {
                            return;
                        };
                        let ver = twitch.current_client_version();
                        let headers = GqlHeaders {
                            auth_token: &token,
                            device_id: &twitch.auth.device_id,
                            client_version: &ver,
                            client_session_id: Some(&twitch.client_session_id),
                        };
                        match gql::join_raid_ext(&twitch.auth.client, &headers, &raid_id).await {
                            Ok(_) => {
                                let msg = format!(
                                    "Joining raid from {} to {}!",
                                    streamer.display_str(less),
                                    target_login
                                );
                                twitch
                                    .log_event(LogLevel::Info, "🎭", Some(MinerEvent::JoinRaid), &msg)
                                    .await;
                            }
                            Err(e) => twitch
                                .log_warn(&format!("Failed to join raid {}: {}", raid_id, e)),
                        }
                    });
                }
            }
        } else if topic.starts_with("community-moments-channel-v1.") {
            let channel_id = topic
                .strip_prefix("community-moments-channel-v1.")
                .unwrap_or("");
            let Some(streamer) = self.find_streamer_by_channel_id(channel_id).await else {
                return;
            };
            let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if msg_type == "active" && streamer.settings.claim_moments {
                if let Some(moment_id) = data
                    .get("data")
                    .and_then(|d| d.get("moment_id"))
                    .and_then(|v| v.as_str())
                    .or_else(|| data.get("moment_id").and_then(|v| v.as_str()))
                    .map(String::from)
                {
                    let twitch = Arc::clone(&self.twitch);
                    let streamer = Arc::clone(&streamer);
                    tokio::spawn(async move {
                        twitch.claim_moment(&streamer, &moment_id).await;
                    });
                }
            }
        } else if topic.starts_with("predictions-channel-v1.") {
            self.handle_prediction_channel(data).await;
        } else if topic.starts_with("predictions-user-v1.") {
            self.handle_prediction_user(data).await;
        } else if topic.starts_with("community-points-channel-v1.") {
            let channel_id = topic
                .strip_prefix("community-points-channel-v1.")
                .unwrap_or("");
            let Some(streamer) = self.find_streamer_by_channel_id(channel_id).await else {
                return;
            };
            if !streamer.settings.community_goals {
                return;
            }
            let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let goal_val = &data["data"]["community_goal"];
            match msg_type {
                "community-goal-created" | "community-goal-updated" => {
                    if let Some(goal) = CommunityGoal::from_pubsub(goal_val) {
                        streamer.update_community_goal(goal);
                        let twitch = Arc::clone(&self.twitch);
                        let streamer = Arc::clone(&streamer);
                        tokio::spawn(async move {
                            let _ = twitch.contribute_community_goals(&streamer).await;
                        });
                    }
                }
                "community-goal-deleted" => {
                    if let Some(gid) = goal_val.get("id").and_then(|v| v.as_str()) {
                        streamer.delete_community_goal(gid);
                    }
                }
                _ => {}
            }
        }
    }

    async fn handle_prediction_channel(&self, data: &Value) {
        let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let Some(event) = PredictionEvent::from_pubsub(data) else {
            return;
        };
        let less = self.twitch.is_less();

        let current_tmsp = data
            .get("data")
            .and_then(|d| d.get("timestamp"))
            .and_then(|v| v.as_str())
            .and_then(parse_twitch_datetime)
            .unwrap_or_else(Utc::now);

        if msg_type == "event-created" {
            if event.status != "ACTIVE" {
                return;
            }
            let already_tracked = self
                .events_predictions
                .lock()
                .await
                .contains_key(&event.event_id);
            if already_tracked {
                return;
            }
            let Some(streamer) = self.find_streamer_by_channel_id(&event.channel_id).await else {
                return;
            };
            if !streamer.settings.make_predictions || !streamer.is_online() {
                return;
            }

            let bet_settings = streamer.settings.bet.clone();
            let balance = streamer.channel_points();
            if bet_settings.minimum_points > 0 && balance <= bet_settings.minimum_points {
                let msg = format!(
                    "{} have only {} channel points and the minimum for bet is: {}",
                    streamer.display_str(less),
                    balance,
                    bet_settings.minimum_points
                );
                self.twitch
                    .log_event(LogLevel::Info, "📌", Some(MinerEvent::BetFilters), &msg)
                    .await;
                return;
            }

            let record = EventPredictionRecord::new(
                streamer.username.clone(),
                streamer.display_str(less),
                event.clone(),
                bet_settings,
            );
            let start_after = record.closing_bet_after(current_tmsp);
            if start_after <= 0.0 {
                return;
            }
            let event_disp = record.display_str(less);
            let shared_record = Arc::new(StdMutex::new(record));
            self.events_predictions
                .lock()
                .await
                .insert(event.event_id.clone(), Arc::clone(&shared_record));

            let msg = format!("Place the bet after: {start_after}s for: {event_disp}");
            self.twitch
                .log_event(LogLevel::Info, "⏰", Some(MinerEvent::BetStart), &msg)
                .await;

            let twitch = Arc::clone(&self.twitch);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs_f64(start_after)).await;
                Self::execute_scheduled_prediction(twitch, streamer, shared_record).await;
            });
        } else if msg_type == "event-updated" {
            let maybe_record = self
                .events_predictions
                .lock()
                .await
                .get(&event.event_id)
                .cloned();
            if let Some(shared_record) = maybe_record {
                if let Some(outcomes_arr) = data
                    .get("data")
                    .and_then(|d| d.get("event"))
                    .and_then(|e| e.get("outcomes"))
                    .and_then(|v| v.as_array())
                {
                    let mut guard = shared_record.lock().unwrap();
                    guard.event.status = event.status;
                    guard.update_outcomes(outcomes_arr);
                }
            }
        }
    }

    async fn execute_scheduled_prediction(
        twitch: Arc<TwitchClient>,
        streamer: SharedStreamer,
        shared_record: Arc<StdMutex<EventPredictionRecord>>,
    ) {
        let less = twitch.is_less();
        let balance = streamer.channel_points();

        let (event_disp, status, eval, filter_summary_opt, outcome_summary_opt, event_id) = {
            let guard = shared_record.lock().unwrap();
            let disp = guard.display_str(less);
            let status = guard.event.status.clone();
            let eval = guard.event.evaluate(balance, &guard.settings);
            let filter_summary = guard
                .settings
                .filter_condition
                .as_ref()
                .map(|f| f.summary());
            let outcome_summary = match &eval {
                BetOutcomeEvaluation::Ready(d) => guard
                    .event
                    .outcomes
                    .get(d.choice_index)
                    .map(|o| o.summary()),
                _ => None,
            };
            (
                disp,
                status,
                eval,
                filter_summary,
                outcome_summary,
                guard.event.event_id.clone(),
            )
        };

        twitch
            .log_event(
                LogLevel::Info,
                "🍀",
                Some(MinerEvent::BetGeneral),
                &format!("Going to complete bet for {event_disp}"),
            )
            .await;

        if status != "ACTIVE" {
            twitch
                .log_event(
                    LogLevel::Info,
                    "😥",
                    Some(MinerEvent::BetFailed),
                    &format!("Oh no! The event is not active anymore! Current status: {status}"),
                )
                .await;
            return;
        }

        match eval {
            BetOutcomeEvaluation::FilteredOut { compared_value, .. } => {
                twitch
                    .log_event(
                        LogLevel::Info,
                        "📌",
                        Some(MinerEvent::BetFilters),
                        &format!("Skip betting for the event {event_disp}"),
                    )
                    .await;
                if let Some(fs) = filter_summary_opt {
                    twitch
                        .log_event(
                            LogLevel::Info,
                            "📌",
                            Some(MinerEvent::BetFilters),
                            &format!("Skip settings {fs}, current value is: {compared_value}"),
                        )
                        .await;
                }
            }
            BetOutcomeEvaluation::BelowMinimumBalance {
                balance,
                minimum_points,
            } => {
                twitch
                    .log_event(
                        LogLevel::Info,
                        "📌",
                        Some(MinerEvent::BetFilters),
                        &format!(
                            "{} have only {} channel points and the minimum for bet is: {}",
                            streamer.display_str(less),
                            balance,
                            minimum_points
                        ),
                    )
                    .await;
            }
            BetOutcomeEvaluation::AmountTooSmall { decision } => {
                twitch
                    .log_event(
                        LogLevel::Info,
                        "🍀",
                        Some(MinerEvent::BetGeneral),
                        &format!(
                            "Bet won't be placed as the amount {} is less than the minimum required 10",
                            millify_u64(decision.amount)
                        ),
                    )
                    .await;
            }
            BetOutcomeEvaluation::InvalidEvent => {}
            BetOutcomeEvaluation::Ready(decision) => {
                {
                    let mut guard = shared_record.lock().unwrap();
                    guard.decision = Some(decision.clone());
                    guard.bet_placed = true;
                }
                let target_desc = outcome_summary_opt.unwrap_or_else(|| {
                    format!("{} ({})", decision.outcome_title, decision.outcome_color)
                });
                twitch
                    .log_event(
                        LogLevel::Info,
                        "🍀",
                        Some(MinerEvent::BetGeneral),
                        &format!(
                            "Place {} channel points on: {}",
                            millify_u64(decision.amount),
                            target_desc
                        ),
                    )
                    .await;

                let Some(token) = twitch.auth.access_token.as_ref().cloned() else {
                    return;
                };
                let ver = twitch.current_client_version();
                let headers = GqlHeaders {
                    auth_token: &token,
                    device_id: &twitch.auth.device_id,
                    client_version: &ver,
                    client_session_id: Some(&twitch.client_session_id),
                };
                let tx_id = Uuid::new_v4().to_string().replace('-', "");
                match gql::make_prediction_ext(
                    &twitch.auth.client,
                    &headers,
                    &event_id,
                    &decision.outcome_id,
                    decision.amount,
                    &tx_id,
                )
                .await
                {
                    Ok(resp) => {
                        if let Some(err_code) = resp["data"]["makePrediction"]["error"]["code"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                        {
                            {
                                let mut guard = shared_record.lock().unwrap();
                                guard.bet_placed = false;
                                guard.decision = None;
                            }
                            twitch
                                .log_event(
                                    LogLevel::Error,
                                    "🍀",
                                    Some(MinerEvent::BetFailed),
                                    &format!("Failed to place bet, error: {err_code}"),
                                )
                                .await;
                        }
                    }
                    Err(e) => {
                        {
                            let mut guard = shared_record.lock().unwrap();
                            guard.bet_placed = false;
                            guard.decision = None;
                        }
                        twitch
                            .log_event(
                                LogLevel::Error,
                                "🍀",
                                Some(MinerEvent::BetFailed),
                                &format!("Failed to place bet: {e}"),
                            )
                            .await;
                    }
                }
            }
        }
    }

    async fn handle_prediction_user(&self, data: &Value) {
        let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let pred = &data["data"]["prediction"];
        let Some(event_id) = pred.get("event_id").and_then(|v| v.as_str()) else {
            return;
        };
        let Some(shared_record) = self.events_predictions.lock().await.get(event_id).cloned()
        else {
            return;
        };
        let channel_id = pred.get("channel_id").and_then(|v| v.as_str()).unwrap_or("");
        let maybe_streamer = self.find_streamer_by_channel_id(channel_id).await;
        let less = self.twitch.is_less();

        match msg_type {
            "prediction-made" => {
                let (streamer_name, annotation_text) = {
                    let mut guard = shared_record.lock().unwrap();
                    guard.bet_confirmed = true;
                    let choice_txt = guard
                        .decision
                        .as_ref()
                        .map(|d| format!("{} ({})", d.outcome_title, d.outcome_color))
                        .unwrap_or_else(|| "?".to_string());
                    (
                        guard.streamer_username.clone(),
                        format!("Decision: {choice_txt} - {}", guard.event.title),
                    )
                };
                if let Some(analytics) = &self.twitch.analytics {
                    analytics.record_annotation(
                        &self.username,
                        &streamer_name,
                        "PREDICTION_MADE",
                        &annotation_text,
                    );
                }
            }
            "prediction-result" => {
                let result_obj = &pred["result"];
                let info_opt = {
                    let mut guard = shared_record.lock().unwrap();
                    if !guard.bet_confirmed && !guard.bet_placed {
                        None
                    } else {
                        guard.bet_confirmed = true;
                        let parsed = guard.parse_result(result_obj);
                        let dec_str = guard
                            .decision
                            .as_ref()
                            .map(|d| {
                                format!(
                                    "{}: {} ({})",
                                    d.choice_index + 1,
                                    d.outcome_title,
                                    d.outcome_color
                                )
                            })
                            .unwrap_or_else(|| "?".to_string());
                        let line = format!(
                            "{} - Decision: {} - Result: {}",
                            guard.display_str(less),
                            dec_str,
                            parsed.summary
                        );
                        Some((
                            parsed,
                            line,
                            guard.streamer_username.clone(),
                            guard.event.title.clone(),
                        ))
                    }
                };

                if let Some((parsed, log_line, streamer_name, event_title)) = info_opt {
                    let ev = MinerEvent::from_bet_result(&parsed.result_type);
                    self.twitch.log_event(LogLevel::Info, "📊", ev, &log_line).await;

                    if let Some(streamer) = maybe_streamer {
                        streamer.update_history("PREDICTION", parsed.gained, 1);
                        // Deduplicate prior community-points-user-v1 history entries, matching WebSocketsPool.py
                        if parsed.result_type == "REFUND" {
                            streamer.update_history("REFUND", -parsed.decision_amount, -1);
                        } else if parsed.result_type == "WIN" {
                            streamer.update_history("PREDICTION", -parsed.won, -1);
                        }
                    }

                    if let Some(analytics) = &self.twitch.analytics {
                        analytics.record_annotation(
                            &self.username,
                            &streamer_name,
                            &parsed.result_type,
                            &event_title,
                        );
                    }
                }
            }
            _ => {}
        }
    }
}
