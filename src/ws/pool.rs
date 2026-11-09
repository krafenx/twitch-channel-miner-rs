use crate::bet::{log_decision, DelayMode, PredictionEvent};
use crate::constants::WEBSOCKET_URL;
use crate::entities::SharedStreamer;
use crate::error::{MinerError, Result};
use crate::twitch::gql;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

pub struct WebSocketPool {
    auth_token: String,
    user_id: String,
    device_id: String,
    client: reqwest::Client,
    streamers: Arc<Mutex<HashMap<String, SharedStreamer>>>,
    /// event_id -> already bet
    placed_bets: Arc<Mutex<std::collections::HashSet<String>>>,
    pending_bets: Arc<Mutex<HashSet<String>>>,
}

impl WebSocketPool {
    pub fn new(
        auth_token: String,
        user_id: String,
        device_id: String,
        client: reqwest::Client,
    ) -> Self {
        Self {
            auth_token,
            user_id,
            device_id,
            client,
            streamers: Arc::new(Mutex::new(HashMap::new())),
            placed_bets: Arc::new(Mutex::new(std::collections::HashSet::new())),
            pending_bets: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub async fn add_streamer(&self, streamer: SharedStreamer) {
        let mut map = self.streamers.lock().await;
        if let Some(cid) = &streamer.channel_id {
            map.insert(cid.clone(), streamer);
        }
    }

    pub async fn run(&self) -> Result<()> {
        loop {
            match self.connect_and_listen().await {
                Ok(_) => warn!("WebSocket closed, reconnecting in 5s..."),
                Err(e) => error!("WebSocket error: {}, reconnecting in 5s...", e),
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }

    async fn connect_and_listen(&self) -> Result<()> {
        info!("Connecting to Twitch PubSub...");
        let (ws_stream, _) = connect_async(WEBSOCKET_URL)
            .await
            .map_err(|e| MinerError::WebSocket(e.to_string()))?;

        let (mut write, mut read) = ws_stream.split();

        let streamers = self.streamers.lock().await;
        let mut topics = vec![format!("community-points-user-v1.{}", self.user_id)];

        for (channel_id, _) in streamers.iter() {
            topics.push(format!("video-playback-by-id.{}", channel_id));
            topics.push(format!("raid.{}", channel_id));
            topics.push(format!("predictions-channel-v1.{}", channel_id));
            topics.push(format!("community-moments-channel-v1.{}", channel_id));
        }
        drop(streamers);

        let nonce = Uuid::new_v4().to_string();
        let listen_msg = json!({
            "type": "LISTEN",
            "nonce": nonce,
            "data": {
                "topics": topics,
                "auth_token": self.auth_token
            }
        });

        write
            .send(Message::Text(listen_msg.to_string()))
            .await
            .map_err(|e| MinerError::WebSocket(e.to_string()))?;

        info!("Subscribed to {} PubSub topics", topics.len());

        let mut write_ping = write;
        let ping_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                if write_ping
                    .send(Message::Text(r#"{"type":"PING"}"#.to_string()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Ok(data) = serde_json::from_str::<Value>(&text) {
                        self.handle_message(&data).await;
                    }
                }
                Ok(Message::Close(_)) => {
                    info!("WebSocket closed by server");
                    break;
                }
                Err(e) => {
                    error!("WS read error: {}", e);
                    break;
                }
                _ => {}
            }
        }

        ping_handle.abort();
        Ok(())
    }

    async fn handle_message(&self, data: &Value) {
        let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match msg_type {
            "PONG" => debug!("PubSub PONG"),
            "RESPONSE" => {
                if let Some(err) = data.get("error").and_then(|v| v.as_str()) {
                    if !err.is_empty() {
                        error!("PubSub LISTEN error: {}", err);
                    }
                }
            }
            "MESSAGE" => {
                if let Some(payload) = data.get("data") {
                    let topic = payload.get("topic").and_then(|v| v.as_str()).unwrap_or("");
                    let message = payload
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if let Ok(inner) = serde_json::from_str::<Value>(message) {
                        self.handle_pubsub_topic(topic, &inner).await;
                    }
                }
            }
            "RECONNECT" => warn!("PubSub requested RECONNECT"),
            _ => debug!("Unhandled PubSub type: {}", msg_type),
        }
    }

    async fn handle_pubsub_topic(&self, topic: &str, data: &Value) {
        if topic.starts_with("community-points-user-v1.") {
            let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match msg_type {
                "points-earned" => {
                    if let Some(points) = data["data"]["point_gain"]["total_points"].as_i64() {
                        let reason = data["data"]["point_gain"]["reason_code"]
                            .as_str()
                            .unwrap_or("unknown");
                        info!("💰 Earned {} points ({})", points, reason);
                        // Update local balance if we can match channel
                        if let Some(cid) = data["data"]["channel_id"].as_str() {
                            let map = self.streamers.lock().await;
                            if let Some(s) = map.get(cid) {
                                let new_bal = s.channel_points() as i64 + points;
                                if new_bal > 0 {
                                    s.set_channel_points(new_bal as u64);
                                }
                            }
                        }
                    }
                }
                "claim-available" => {
                    info!("🎁 Claim available via PubSub");
                }
                "prediction-result" => {
                    if let Some(result) = data["data"]["prediction"]["resultType"].as_str() {
                        let points = data["data"]["prediction"]["pointsWon"]
                            .as_i64()
                            .unwrap_or(0);
                        info!("🎲 Prediction result: {} (points: {})", result, points);
                    }
                }
                _ => debug!("community-points event: {}", msg_type),
            }
        } else if topic.starts_with("video-playback-by-id.") {
            let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if msg_type == "stream-up" {
                info!("Stream went online ({})", topic);
            } else if msg_type == "stream-down" {
                info!("Stream went offline ({})", topic);
            }
        } else if topic.starts_with("raid.") {
            if let Some(raid_id) = data["raid"]["id"].as_str().or_else(|| data["id"].as_str()) {
                info!("🚀 Raid detected: {}", raid_id);
                // Auto-join raid if any streamer has follow_raid
                let map = self.streamers.lock().await;
                let should_join = map.values().any(|s| s.settings.follow_raid);
                drop(map);
                if should_join {
                    match gql::join_raid(&self.client, &self.auth_token, &self.device_id, raid_id)
                        .await
                    {
                        Ok(_) => info!("Joined raid {}", raid_id),
                        Err(e) => warn!("Failed to join raid: {}", e),
                    }
                }
            }
        } else if topic.starts_with("predictions-channel-v1.") {
            let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if msg_type == "event-created" || msg_type == "event-updated" {
                self.handle_prediction(data).await;
            }
        } else if topic.starts_with("community-moments-channel-v1.") {
            let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let channel_id = topic
                .strip_prefix("community-moments-channel-v1.")
                .unwrap_or("");
            let should_claim = {
                let streamers = self.streamers.lock().await;
                streamers
                    .values()
                    .find(|streamer| streamer.channel_id.as_deref() == Some(channel_id))
                    .map(|streamer| streamer.settings.claim_moments)
                    .unwrap_or(true)
            };
            if msg_type == "active" && should_claim {
                if let Some(moment_id) = data
                    .get("data")
                    .and_then(|d| d.get("moment_id"))
                    .and_then(|v| v.as_str())
                    .or_else(|| data.get("moment_id").and_then(|v| v.as_str()))
                {
                    info!("🎬 Moment active: {}", moment_id);
                    match gql::claim_moment(
                        &self.client,
                        &self.auth_token,
                        &self.device_id,
                        moment_id,
                    )
                    .await
                    {
                        Ok(_) => info!("✅ Claimed moment {}", moment_id),
                        Err(e) => warn!("Failed to claim moment: {}", e),
                    }
                }
            }
        }
    }

    async fn handle_prediction(&self, data: &Value) {
        let Some(event) = PredictionEvent::from_pubsub(data) else {
            return;
        };

        if event.status != "ACTIVE" && event.status != "LOCKED" {
            return;
        }

        // Avoid scheduling or placing a bet more than once for an event.
        if self.placed_bets.lock().await.contains(&event.event_id)
            || !self
                .pending_bets
                .lock()
                .await
                .insert(event.event_id.clone())
        {
            return;
        }

        // Find streamer settings
        let map = self.streamers.lock().await;
        let streamer = match map.get(&event.channel_id) {
            Some(s) if s.settings.make_predictions => s.clone(),
            _ => {
                self.pending_bets.lock().await.remove(&event.event_id);
                return;
            }
        };
        drop(map);

        let settings = streamer.settings.bet.clone();
        let delay_seconds = if event.status == "LOCKED" {
            0.0
        } else {
            match settings.delay_mode {
                DelayMode::FromStart => settings.delay,
                DelayMode::FromEnd => {
                    (event.prediction_window_seconds as f64 - settings.delay).max(0.0)
                }
                DelayMode::Percentage => {
                    event.prediction_window_seconds as f64 * settings.delay.clamp(0.0, 1.0)
                }
            }
        };
        let client = self.client.clone();
        let token = self.auth_token.clone();
        let device_id = self.device_id.clone();
        let placed_bets = self.placed_bets.clone();
        let pending_bets = self.pending_bets.clone();
        let event_id = event.event_id.clone();
        let task = async move {
            if delay_seconds > 0.0 {
                tokio::time::sleep(std::time::Duration::from_secs_f64(delay_seconds)).await;
            }
            Self::place_prediction(
                event,
                streamer,
                settings,
                client,
                token,
                device_id,
                placed_bets,
            )
            .await;
            pending_bets.lock().await.remove(&event_id);
        };
        if delay_seconds > 0.0 {
            tokio::spawn(task);
        } else {
            task.await;
        }
    }

    async fn place_prediction(
        event: PredictionEvent,
        streamer: SharedStreamer,
        settings: crate::bet::BetSettings,
        client: reqwest::Client,
        auth_token: String,
        device_id: String,
        placed_bets: Arc<Mutex<HashSet<String>>>,
    ) {
        let balance = streamer.channel_points();
        if let Some(decision) = event.decide(balance, &settings) {
            log_decision(&event, &decision);

            let tx_id = Uuid::new_v4().to_string().replace('-', "");
            match gql::make_prediction(
                &client,
                &auth_token,
                &device_id,
                &event.event_id,
                &decision.outcome_id,
                decision.amount,
                &tx_id,
            )
            .await
            {
                Ok(resp) => {
                    // mark as placed
                    placed_bets.lock().await.insert(event.event_id.clone());
                    if let Some(err) = resp.get("errors") {
                        warn!("Prediction GQL error: {:?}", err);
                    } else {
                        info!(
                            "✅ Placed bet: {} pts on '{}' for '{}'",
                            decision.amount, decision.outcome_title, event.title
                        );
                        // optimistically reduce local balance
                        let new_bal = balance.saturating_sub(decision.amount);
                        streamer.set_channel_points(new_bal);
                    }
                }
                Err(e) => warn!("Failed to place prediction: {}", e),
            }
        }
    }
}
