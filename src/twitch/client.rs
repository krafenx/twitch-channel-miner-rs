use crate::analytics::Analytics;
use crate::auth::TwitchAuth;
use crate::constants::{CLIENT_VERSION, DROP_ID, FIREFOX_USER_AGENT, TWITCH_URL, USER_AGENT};
use crate::entities::{Campaign, CommunityGoal, DropItem, SharedStreamer, Streamer};
use crate::error::{MinerError, Result};
use crate::logger::{LogLevel, SessionLogger};
use crate::settings::MinerEvent;
use crate::twitch::gql::{self, GqlHeaders};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chrono::Utc;
use regex::Regex;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tracing::{debug, info, warn};
use uuid::Uuid;

pub struct TwitchClient {
    pub auth: TwitchAuth,
    pub client_session_id: String,
    pub client_version: Arc<RwLock<String>>,
    pub logger: Option<Arc<SessionLogger>>,
    pub analytics: Option<Arc<Analytics>>,
}

impl TwitchClient {
    pub fn new(auth: TwitchAuth) -> Self {
        Self::with_context(auth, None, None)
    }

    pub fn with_context(
        auth: TwitchAuth,
        logger: Option<Arc<SessionLogger>>,
        analytics: Option<Arc<Analytics>>,
    ) -> Self {
        Self {
            auth,
            client_session_id: Uuid::new_v4().to_string().replace('-', ""),
            client_version: Arc::new(RwLock::new(CLIENT_VERSION.to_string())),
            logger,
            analytics,
        }
    }

    pub fn current_client_version(&self) -> String {
        self.client_version
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|_| CLIENT_VERSION.to_string())
    }

    pub async fn refresh_client_version(&self) {
        if let Some(ver) = gql::fetch_twilight_client_version(&self.auth.client).await {
            debug!("Updated Twitch Client-Version (__twilightBuildID): {ver}");
            if let Ok(mut guard) = self.client_version.write() {
                *guard = ver;
            }
        }
    }

    pub fn is_less(&self) -> bool {
        self.logger
            .as_ref()
            .map(|l| l.settings.less)
            .unwrap_or(false)
    }

    pub async fn log_event(
        &self,
        level: LogLevel,
        emoji: &str,
        event: Option<MinerEvent>,
        msg: &str,
    ) {
        if let Some(log) = &self.logger {
            log.emit(level, emoji, event, msg).await;
        } else {
            info!("{emoji} {msg}");
        }
    }

    pub fn log_info(&self, emoji: &str, msg: &str) {
        if let Some(log) = &self.logger {
            log.info(emoji, msg);
        } else {
            info!("{emoji} {msg}");
        }
    }

    pub fn log_warn(&self, msg: &str) {
        if let Some(log) = &self.logger {
            log.warn(msg);
        } else {
            warn!("{msg}");
        }
    }

    /// Check internet connectivity and sleep 1–3m until restored (ported from `__check_connection_handler`).
    pub async fn wait_for_internet(&self) {
        loop {
            let ok = self
                .auth
                .client
                .head(TWITCH_URL)
                .timeout(Duration::from_secs(10))
                .send()
                .await
                .is_ok();
            if ok {
                break;
            }
            let wait_mins: u64 = {
                use rand::Rng;
                rand::thread_rng().gen_range(1..=3)
            };
            self.log_warn(&format!(
                "No internet connection available! Retry after {}m",
                wait_mins
            ));
            tokio::time::sleep(Duration::from_secs(wait_mins * 60)).await;
        }
    }

    pub async fn resolve_streamer(&self, streamer: &mut Streamer) -> Result<()> {
        let token = self
            .auth
            .access_token
            .as_ref()
            .ok_or_else(|| MinerError::Auth("Not authenticated".into()))?
            .clone();
        let ver = self.current_client_version();
        let headers = GqlHeaders {
            auth_token: &token,
            device_id: &self.auth.device_id,
            client_version: &ver,
            client_session_id: Some(&self.client_session_id),
        };

        let channel_id =
            gql::get_channel_id_ext(&self.auth.client, &headers, &streamer.username).await?;
        streamer.channel_id = Some(channel_id);

        let is_mod = gql::viewer_is_mod(&self.auth.client, &headers, &streamer.username).await;
        streamer
            .viewer_is_mod
            .store(is_mod, std::sync::atomic::Ordering::Relaxed);

        self.load_channel_points_context(streamer).await;
        self.update_stream_status_inner(streamer, true).await?;
        Ok(())
    }

    /// Ported from `Twitch.load_channel_points_context`:
    /// Loads balance, `activeMultipliers`, `community_goals`, claims `availableClaim`, and contributes to goals.
    pub async fn load_channel_points_context(&self, streamer: &Streamer) {
        let Some(token) = self.auth.access_token.as_ref().cloned() else {
            return;
        };
        let ver = self.current_client_version();
        let headers = GqlHeaders {
            auth_token: &token,
            device_id: &self.auth.device_id,
            client_version: &ver,
            client_session_id: Some(&self.client_session_id),
        };

        let Ok(ctx) =
            gql::get_channel_points_context_ext(&self.auth.client, &headers, &streamer.username)
                .await
        else {
            return;
        };

        let channel = &ctx["data"]["community"]["channel"];
        if channel.is_null() {
            return;
        }
        let cp = &channel["self"]["communityPoints"];
        if let Some(points) = cp["balance"].as_u64() {
            streamer.set_channel_points(points);
        }

        // Parse activeMultipliers (used by Priority::Subscribed)
        let multipliers: Vec<f64> = cp["activeMultipliers"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("factor").and_then(|v| v.as_f64()))
                    .collect()
            })
            .unwrap_or_default();
        *streamer.active_multipliers.lock().unwrap() = multipliers;

        // Parse community goals
        if streamer.settings.community_goals {
            if let Some(goals_arr) = channel["communityPointsSettings"]["goals"].as_array() {
                let mut map = HashMap::new();
                for g in goals_arr {
                    if let Some(parsed) = CommunityGoal::from_gql(g) {
                        map.insert(parsed.goal_id.clone(), parsed);
                    }
                }
                *streamer.community_goals.lock().unwrap() = map;
            }
        }

        // Auto-claim available bonus chest
        if let Some(claim_id) = cp["availableClaim"].get("id").and_then(|v| v.as_str()) {
            self.claim_bonus(streamer, claim_id).await;
        }

        if streamer.settings.community_goals && streamer.is_online() {
            let _ = self.contribute_community_goals(streamer).await;
        }
    }

    pub async fn claim_bonus(&self, streamer: &Streamer, claim_id: &str) {
        let Some(token) = self.auth.access_token.as_ref().cloned() else {
            return;
        };
        let Some(channel_id) = streamer.channel_id.clone() else {
            return;
        };
        let ver = self.current_client_version();
        let headers = GqlHeaders {
            auth_token: &token,
            device_id: &self.auth.device_id,
            client_version: &ver,
            client_session_id: Some(&self.client_session_id),
        };

        let msg = format!("Claiming the bonus for {}!", streamer.display_str(self.is_less()));
        if !self.is_less() {
            self.log_event(LogLevel::Info, "🎁", Some(MinerEvent::BonusClaim), &msg)
                .await;
        } else if let Some(log) = &self.logger {
            log.notifier
                .notify_event(Some(MinerEvent::BonusClaim), &msg)
                .await;
        }

        if let Err(e) = gql::claim_bonus_ext(&self.auth.client, &headers, &channel_id, claim_id).await
        {
            self.log_warn(&format!(
                "Failed to claim bonus for {}: {}",
                streamer.username, e
            ));
        }
    }

    pub async fn claim_moment(&self, streamer: &Streamer, moment_id: &str) {
        let Some(token) = self.auth.access_token.as_ref().cloned() else {
            return;
        };
        let ver = self.current_client_version();
        let headers = GqlHeaders {
            auth_token: &token,
            device_id: &self.auth.device_id,
            client_version: &ver,
            client_session_id: Some(&self.client_session_id),
        };

        let msg = format!(
            "Claiming the moment for {}!",
            streamer.display_str(self.is_less())
        );
        if !self.is_less() {
            self.log_event(LogLevel::Info, "📹", Some(MinerEvent::MomentClaim), &msg)
                .await;
        } else if let Some(log) = &self.logger {
            log.notifier
                .notify_event(Some(MinerEvent::MomentClaim), &msg)
                .await;
        }

        if let Err(e) = gql::claim_moment_ext(&self.auth.client, &headers, moment_id).await {
            self.log_warn(&format!(
                "Failed to claim moment for {}: {}",
                streamer.username, e
            ));
        }
    }

    pub async fn update_stream_status(&self, streamer: &Streamer) -> Result<()> {
        self.update_stream_status_inner(streamer, false).await
    }

    pub async fn update_stream_status_inner(
        &self,
        streamer: &Streamer,
        initial_resolve: bool,
    ) -> Result<()> {
        if !initial_resolve && !streamer.is_online() && streamer.is_in_offline_cooldown() {
            return Ok(());
        }

        let token = self
            .auth
            .access_token
            .as_ref()
            .ok_or_else(|| MinerError::Auth("Not authenticated".into()))?
            .clone();
        let channel_id = streamer
            .channel_id
            .as_ref()
            .ok_or_else(|| MinerError::Other("no channel_id".into()))?
            .clone();
        let ver = self.current_client_version();
        let headers = GqlHeaders {
            auth_token: &token,
            device_id: &self.auth.device_id,
            client_version: &ver,
            client_session_id: Some(&self.client_session_id),
        };

        let live = gql::is_stream_live_ext(&self.auth.client, &headers, &channel_id)
            .await
            .unwrap_or(false);

        if !live {
            streamer.mark_offline_checked();
        }
        let transitioned = streamer.set_online_with_transition(live, initial_resolve);

        if live {
            if let Err(e) = self.refresh_stream_info(streamer).await {
                debug!("refresh_stream_info for {}: {}", streamer.username, e);
            }
            if transitioned {
                let msg = format!("{} is Online!", streamer.display_str(self.is_less()));
                self.log_event(LogLevel::Info, "🥳", Some(MinerEvent::StreamerOnline), &msg)
                    .await;
            }
        } else if transitioned || initial_resolve {
            let msg = format!("{} is Offline!", streamer.display_str(self.is_less()));
            if transitioned {
                self.log_event(LogLevel::Info, "😴", Some(MinerEvent::StreamerOffline), &msg)
                    .await;
            } else {
                self.log_info("😴", &msg);
            }
        }

        Ok(())
    }

    async fn refresh_stream_info(&self, streamer: &Streamer) -> Result<()> {
        let has_spade = streamer.stream.lock().unwrap().spade_url.is_some();
        let needs_update = streamer.stream.lock().unwrap().update_required();
        if !needs_update && has_spade {
            return Ok(());
        }

        let token = self
            .auth
            .access_token
            .as_ref()
            .ok_or_else(|| MinerError::Auth("Not authenticated".into()))?
            .clone();
        let ver = self.current_client_version();
        let headers = GqlHeaders {
            auth_token: &token,
            device_id: &self.auth.device_id,
            client_version: &ver,
            client_session_id: Some(&self.client_session_id),
        };

        let mut fetched_game_id: Option<String> = None;

        if let Ok(info) =
            gql::get_stream_info_ext(&self.auth.client, &headers, &streamer.username).await
        {
            if let Some(user) = info["data"]["user"].as_object() {
                if user.get("stream").map(|s| s.is_null()).unwrap_or(true) {
                    return Err(MinerError::Other("stream is offline".into()));
                }
                let mut stream = streamer.stream.lock().unwrap();
                if let Some(s) = user.get("stream").and_then(|v| v.as_object()) {
                    stream.broadcast_id = s.get("id").and_then(|v| v.as_str()).map(String::from);
                    stream.viewers = s.get("viewersCount").and_then(|v| v.as_u64()).unwrap_or(0);

                    let tags: Vec<(String, String)> = s
                        .get("tags")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|t| {
                                    let tid = t.get("id")?.as_str()?.to_string();
                                    let tname = t
                                        .get("localizedName")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    Some((tid, tname))
                                })
                                .collect()
                        })
                        .unwrap_or_default();

                    let game_obj = user
                        .get("broadcastSettings")
                        .and_then(|b| b.get("game"))
                        .and_then(|v| v.as_object())
                        .or_else(|| s.get("game").and_then(|v| v.as_object()));

                    if let Some(game) = game_obj {
                        stream.game_name =
                            game.get("name").and_then(|v| v.as_str()).map(String::from);
                        stream.game_display_name = game
                            .get("displayName")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                            .or_else(|| stream.game_name.clone());
                        stream.game_id = game.get("id").and_then(|v| v.as_str()).map(String::from);
                        fetched_game_id = stream.game_id.clone();
                    } else {
                        stream.game_name = None;
                        stream.game_display_name = None;
                        stream.game_id = None;
                    }

                    stream.drops_tags = stream.game_id.is_some()
                        && tags.iter().any(|(tid, _)| tid == DROP_ID);
                    stream.tags = tags;
                }
                if let Some(title) = user
                    .get("broadcastSettings")
                    .and_then(|v| v.get("title"))
                    .and_then(|v| v.as_str())
                {
                    stream.title = Some(title.trim().to_string());
                }
                stream.last_update_ts = Some(Utc::now());
            }
        }

        // Update available Drop campaign IDs for this channel (used by sync_campaigns & Priority::Drops)
        if streamer.settings.claim_drops && fetched_game_id.is_some() {
            if let Some(channel_id) = &streamer.channel_id {
                if let Ok(cids) =
                    gql::get_available_drops_for_channel(&self.auth.client, &headers, channel_id)
                        .await
                {
                    streamer.stream.lock().unwrap().campaigns_ids = cids;
                }
            }
        } else {
            streamer.stream.lock().unwrap().campaigns_ids.clear();
        }

        if !has_spade {
            if let Err(e) = self.fetch_spade_url(streamer).await {
                debug!("Could not get spade_url for {}: {}", streamer.username, e);
            }
        }

        Ok(())
    }

    async fn fetch_spade_url(&self, streamer: &Streamer) -> Result<()> {
        let page = self
            .auth
            .client
            .get(streamer.streamer_url())
            .header("User-Agent", FIREFOX_USER_AGENT)
            .send()
            .await?
            .text()
            .await?;

        let re_settings = Regex::new(
            r#"(https://static\.twitchcdn\.net/config/settings.*?\.js|https://assets\.twitch\.tv/config/settings.*?\.js)"#,
        )
        .unwrap();

        let re_spade = Regex::new(r#""spade_url":"(.*?)""#).unwrap();

        let spade = if let Some(direct) = re_spade
            .captures(&page)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().replace("\\u002F", "/"))
        {
            direct
        } else {
            let settings_url = re_settings
                .captures(&page)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
                .ok_or_else(|| MinerError::Other("settings.js not found".into()))?;

            let settings_js = self
                .auth
                .client
                .get(&settings_url)
                .header("User-Agent", FIREFOX_USER_AGENT)
                .send()
                .await?
                .text()
                .await?;

            re_spade
                .captures(&settings_js)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().replace("\\u002F", "/"))
                .ok_or_else(|| MinerError::Other("spade_url not found".into()))?
        };

        streamer.stream.lock().unwrap().spade_url = Some(spade);
        debug!("Got spade_url for {}", streamer.username);
        Ok(())
    }

    /// Simulate watching + send `minute-watched` event to Spade + report Drop progress milestones
    pub async fn send_minute_watched(&self, streamer: &Streamer) -> Result<()> {
        if !streamer.ready_for_minute_watched() {
            return Ok(());
        }

        // Upstream check: if last stream info update was >10 minutes ago, re-verify online status
        let stale_stream = streamer.stream.lock().unwrap().update_elapsed_secs() > 600;
        if stale_stream {
            let _ = self.update_stream_status(streamer).await;
            if !streamer.is_online() {
                return Ok(());
            }
        }

        let token = self
            .auth
            .access_token
            .as_ref()
            .ok_or_else(|| MinerError::Auth("Not authenticated".into()))?
            .clone();
        let user_id = self
            .auth
            .user_id
            .as_ref()
            .ok_or_else(|| MinerError::Auth("Not authenticated".into()))?
            .clone();
        let ver = self.current_client_version();
        let headers = GqlHeaders {
            auth_token: &token,
            device_id: &self.auth.device_id,
            client_version: &ver,
            client_session_id: Some(&self.client_session_id),
        };

        // 1. PlaybackAccessToken
        let (signature, value) =
            gql::get_playback_access_token_ext(&self.auth.client, &headers, &streamer.username)
                .await?;

        // 2. HLS master playlist
        let hls_url = format!(
            "https://usher.ttvnw.net/api/channel/hls/{}.m3u8?sig={}&token={}",
            streamer.username,
            urlencoding::encode(&signature),
            urlencoding::encode(&value)
        );

        let master = self
            .auth
            .client
            .get(&hls_url)
            .header("User-Agent", USER_AGENT)
            .timeout(Duration::from_secs(20))
            .send()
            .await?
            .text()
            .await?;

        let quality_url = master
            .lines()
            .rev()
            .find(|l| l.starts_with("http"))
            .ok_or_else(|| MinerError::Other("no quality URL in master playlist".into()))?;

        // 3. Media playlist
        let media = self
            .auth
            .client
            .get(quality_url)
            .header("User-Agent", USER_AGENT)
            .timeout(Duration::from_secs(20))
            .send()
            .await?
            .text()
            .await?;

        let segment_url = media
            .lines()
            .rev()
            .find(|l| l.starts_with("http"))
            .ok_or_else(|| MinerError::Other("no segment URL".into()))?;

        // 4. HEAD the segment
        let head_resp = self
            .auth
            .client
            .head(segment_url)
            .header("User-Agent", USER_AGENT)
            .timeout(Duration::from_secs(20))
            .send()
            .await?;

        if !head_resp.status().is_success() {
            return Err(MinerError::Other(format!(
                "HLS segment HEAD returned {}",
                head_resp.status()
            )));
        }

        // 5. Build & send minute-watched to Spade
        let (broadcast_id, spade_url, game_name, game_id) = {
            let stream = streamer.stream.lock().unwrap();
            (
                stream.broadcast_id.clone(),
                stream.spade_url.clone(),
                stream.game_name.clone(),
                stream.game_id.clone(),
            )
        };

        let spade_url = spade_url.ok_or_else(|| MinerError::Other("no spade_url".into()))?;
        let broadcast_id =
            broadcast_id.ok_or_else(|| MinerError::Other("no broadcast_id".into()))?;
        let channel_id = streamer
            .channel_id
            .as_ref()
            .ok_or_else(|| MinerError::Other("no channel_id".into()))?
            .clone();

        let mut properties = json!({
            "channel_id": channel_id,
            "broadcast_id": broadcast_id,
            "player": "site",
            "user_id": user_id,
            "live": true,
            "channel": streamer.username,
        });

        if streamer.settings.claim_drops {
            if let (Some(gn), Some(gid)) = (game_name, game_id) {
                properties["game"] = json!(gn);
                properties["game_id"] = json!(gid);
            }
        }

        let payload = json!([{
            "event": "minute-watched",
            "properties": properties
        }]);

        let encoded = B64.encode(payload.to_string().as_bytes());

        let resp = self
            .auth
            .client
            .post(&spade_url)
            .header("User-Agent", USER_AGENT)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!("data={}", encoded))
            .timeout(Duration::from_secs(20))
            .send()
            .await?;

        if resp.status().as_u16() == 204 {
            let less = self.is_less();
            let printable_drops: Vec<String> = {
                let mut stream = streamer.stream.lock().unwrap();
                stream.record_minute_watched();
                let stream_disp = stream.display_str(less);
                let streamer_disp = streamer.display_str(less);
                let mut msgs = Vec::new();
                for campaign in &mut stream.campaigns {
                    let camp_disp = campaign.display_str(less);
                    for drop in &mut campaign.drops {
                        if drop.has_preconditions_met != Some(false) && drop.is_printable {
                            drop.is_printable = false;
                            msgs.push(format!(
                                "{} is streaming {}\nCampaign: {}\nDrop: {}\n{}",
                                streamer_disp,
                                stream_disp,
                                camp_disp,
                                drop.display_str(less),
                                drop.progress_bar()
                            ));
                        }
                    }
                }
                msgs
            };
            *streamer.last_watch.lock().unwrap() = Some(std::time::Instant::now());

            for block in printable_drops {
                self.log_event(LogLevel::Info, "📦", Some(MinerEvent::DropStatus), &block)
                    .await;
            }
        } else {
            debug!(
                "minute-watched for {} returned {}",
                streamer.username,
                resp.status()
            );
        }

        Ok(())
    }

    /// Claim all claimable drops from inventory
    pub async fn claim_all_drops(&self) -> Result<u32> {
        let token = self
            .auth
            .access_token
            .as_ref()
            .ok_or_else(|| MinerError::Auth("Not authenticated".into()))?
            .clone();
        let ver = self.current_client_version();
        let headers = GqlHeaders {
            auth_token: &token,
            device_id: &self.auth.device_id,
            client_version: &ver,
            client_session_id: Some(&self.client_session_id),
        };
        let inv = gql::get_inventory_ext(&self.auth.client, &headers).await?;

        let mut claimed = 0u32;

        if let Some(campaigns) =
            inv["data"]["currentUser"]["inventory"]["dropCampaignsInProgress"].as_array()
        {
            for campaign in campaigns {
                if let Some(drops) = campaign["timeBasedDrops"].as_array() {
                    for drop_json in drops {
                        let Some(mut drop) = DropItem::from_gql(drop_json) else {
                            continue;
                        };
                        if let Some(self_obj) = drop_json.get("self") {
                            drop.update_progress(self_obj);
                        }
                        if !drop.is_claimable {
                            continue;
                        }
                        if let Some(instance_id) = drop.drop_instance_id.as_deref() {
                            match gql::claim_drop_ext(&self.auth.client, &headers, instance_id)
                                .await
                            {
                                Ok(true) => {
                                    let msg = format!(
                                        "Claim {}",
                                        drop.display_str(self.is_less())
                                    );
                                    self.log_event(
                                        LogLevel::Info,
                                        "📦",
                                        Some(MinerEvent::DropClaim),
                                        &msg,
                                    )
                                    .await;
                                    claimed += 1;
                                    tokio::time::sleep(Duration::from_secs(5)).await;
                                }
                                Ok(false) => {
                                    debug!("Drop not eligible yet: {}", drop.name);
                                }
                                Err(e) => {
                                    self.log_warn(&format!(
                                        "Error claiming drop {}: {}",
                                        drop.name, e
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(claimed)
    }

    /// Full port of `Twitch.sync_campaigns`:
    /// Refreshes active Drop campaigns from dashboard, syncs inventory progress, claims ready drops,
    /// and binds matching campaigns onto each online streamer's `stream.campaigns`.
    pub async fn sync_campaigns_once(
        &self,
        streamers: &[SharedStreamer],
        refresh_dashboard: bool,
        campaigns: &mut Vec<Campaign>,
    ) -> Result<()> {
        let token = self
            .auth
            .access_token
            .as_ref()
            .ok_or_else(|| MinerError::Auth("Not authenticated".into()))?
            .clone();
        let user_id = self
            .auth
            .user_id
            .as_ref()
            .ok_or_else(|| MinerError::Auth("Not authenticated".into()))?
            .clone();
        let ver = self.current_client_version();
        let headers = GqlHeaders {
            auth_token: &token,
            device_id: &self.auth.device_id,
            client_version: &ver,
            client_session_id: Some(&self.client_session_id),
        };

        if refresh_dashboard || campaigns.is_empty() {
            let mut prev_progress: HashMap<String, (u64, u64)> = HashMap::new();
            for camp in campaigns.iter() {
                for d in &camp.drops {
                    prev_progress.insert(
                        d.id.clone(),
                        (d.current_minutes_watched, d.percentage_progress),
                    );
                }
            }

            let _ = self.claim_all_drops().await;
            if let Ok(dash) = gql::get_drops_dashboard_ext(&self.auth.client, &headers).await {
                let active_ids: Vec<String> = dash["data"]["currentUser"]["dropCampaigns"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter(|c| {
                                c.get("status")
                                    .and_then(|s| s.as_str())
                                    .map(|s| s.eq_ignore_ascii_case("ACTIVE"))
                                    .unwrap_or(false)
                            })
                            .filter_map(|c| c.get("id").and_then(|v| v.as_str()).map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                if active_ids.is_empty() {
                    campaigns.clear();
                } else if let Ok(details) = gql::get_campaign_details_batch(
                    &self.auth.client,
                    &headers,
                    &active_ids,
                    &user_id,
                )
                .await
                {
                    let mut fresh = Vec::new();
                    for d in &details {
                        if let Some(mut c) = Campaign::from_gql(d) {
                            if c.dt_match {
                                for drop in &mut c.drops {
                                    if let Some(&(mins, pct)) = prev_progress.get(&drop.id) {
                                        if drop.current_minutes_watched == 0 {
                                            drop.current_minutes_watched = mins;
                                            drop.percentage_progress = pct;
                                        }
                                    }
                                }
                                c.clear_drops();
                                if !c.drops.is_empty() {
                                    fresh.push(c);
                                }
                            }
                        }
                    }
                    *campaigns = fresh;
                }
            }
        }

        // Sync progress from Inventory
        if let Ok(inv) = gql::get_inventory_ext(&self.auth.client, &headers).await {
            if let Some(in_prog) =
                inv["data"]["currentUser"]["inventory"]["dropCampaignsInProgress"].as_array()
            {
                for camp in campaigns.iter_mut() {
                    camp.clear_drops();
                    if let Some(prog) = in_prog
                        .iter()
                        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(camp.id.as_str()))
                    {
                        camp.in_inventory = true;
                        if let Some(tb_drops) = prog.get("timeBasedDrops").and_then(|v| v.as_array())
                        {
                            for inv_drop in tb_drops {
                                let Some(did) = inv_drop.get("id").and_then(|v| v.as_str()) else {
                                    continue;
                                };
                                if let Some(target) = camp.drops.iter_mut().find(|d| d.id == did) {
                                    if let Some(self_obj) = inv_drop.get("self") {
                                        target.update_progress(self_obj);
                                    }
                                    if target.is_claimable {
                                        if let Some(inst_id) =
                                            target.drop_instance_id.clone().as_deref()
                                        {
                                            if let Ok(true) = gql::claim_drop_ext(
                                                &self.auth.client,
                                                &headers,
                                                inst_id,
                                            )
                                            .await
                                            {
                                                target.is_claimed = true;
                                                let msg = format!(
                                                    "Claim {}",
                                                    target.display_str(self.is_less())
                                                );
                                                self.log_event(
                                                    LogLevel::Info,
                                                    "📦",
                                                    Some(MinerEvent::DropClaim),
                                                    &msg,
                                                )
                                                .await;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        camp.clear_drops();
                    }
                }
            }
        }

        // Bind matching campaigns to each streamer (delivering each printable milestone at most once)
        let mut printable_delivered = std::collections::HashSet::new();
        for streamer in streamers {
            if !streamer.settings.claim_drops || !streamer.is_online() {
                streamer.stream.lock().unwrap().campaigns.clear();
                continue;
            }
            let cid_opt = streamer.channel_id.clone();
            let mut st = streamer.stream.lock().unwrap();
            let Some(stream_game_id) = st.game_id.clone() else {
                st.campaigns.clear();
                continue;
            };
            let allowed_cids = st.campaigns_ids.clone();
            let has_drops_tag = st.drops_tags;
            let mut matched: Vec<Campaign> = campaigns
                .iter()
                .filter(|c| {
                    !c.drops.is_empty()
                        && c.game_id.as_deref() == Some(stream_game_id.as_str())
                        && (allowed_cids.contains(&c.id)
                            || (c.channels.is_empty() && has_drops_tag)
                            || (!c.channels.is_empty()
                                && cid_opt
                                    .as_ref()
                                    .map(|id| c.channels.contains(id))
                                    .unwrap_or(false)))
                })
                .cloned()
                .collect();

            for camp in &mut matched {
                for d in &mut camp.drops {
                    if d.is_printable && !printable_delivered.insert(d.id.clone()) {
                        d.is_printable = false;
                    }
                }
            }
            st.campaigns = matched;
        }
        // Consume `is_printable` on master cache so next 60s sync doesn't re-emit the same milestone
        for camp in campaigns.iter_mut() {
            for d in &mut camp.drops {
                d.is_printable = false;
            }
        }

        Ok(())
    }

    /// Ported from `Twitch.contribute_to_community_goals`:
    /// Queries `UserPointsContribution`, respects `status == "STARTED"`, `is_in_stock`,
    /// `amount_left()`, and `per_stream_user_maximum_contribution - userPointsContributedThisStream`.
    pub async fn contribute_community_goals(&self, streamer: &Streamer) -> Result<()> {
        if !streamer.settings.community_goals {
            return Ok(());
        }
        let Some(channel_id) = streamer.channel_id.clone() else {
            return Ok(());
        };
        let goals_snapshot: HashMap<String, CommunityGoal> =
            streamer.community_goals.lock().unwrap().clone();

        let has_active_goal = goals_snapshot
            .values()
            .any(|g| g.status.eq_ignore_ascii_case("STARTED") && g.is_in_stock);
        if !has_active_goal {
            return Ok(());
        }

        let token = self
            .auth
            .access_token
            .as_ref()
            .ok_or_else(|| MinerError::Auth("Not authenticated".into()))?
            .clone();
        let ver = self.current_client_version();
        let headers = GqlHeaders {
            auth_token: &token,
            device_id: &self.auth.device_id,
            client_version: &ver,
            client_session_id: Some(&self.client_session_id),
        };

        let contrib_resp = gql::get_user_points_contribution_ext(
            &self.auth.client,
            &headers,
            &streamer.username,
        )
        .await?;

        let Some(user_contribs) = contrib_resp["data"]["user"]["channel"]["self"]
            ["communityPoints"]["goalContributions"]
            .as_array()
        else {
            return Ok(());
        };

        for item in user_contribs {
            let Some(goal_id) = item
                .get("goal")
                .and_then(|g| g.get("id"))
                .and_then(|v| v.as_str())
            else {
                continue;
            };
            let Some(goal) = goals_snapshot.get(goal_id) else {
                continue;
            };
            if !goal.status.eq_ignore_ascii_case("STARTED") || !goal.is_in_stock {
                continue;
            }
            let user_stream_contrib = item
                .get("userPointsContributedThisStream")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let user_left = goal
                .per_stream_user_maximum_contribution
                .saturating_sub(user_stream_contrib);
            let balance = streamer.channel_points();
            let amount = goal.amount_left().min(user_left).min(balance);

            if amount == 0 {
                continue;
            }

            match gql::contribute_community_goal_ext(
                &self.auth.client,
                &headers,
                goal_id,
                &channel_id,
                amount,
            )
            .await
            {
                Ok(resp) => {
                    let err =
                        resp["data"]["contributeCommunityPointsCommunityGoal"]["error"].as_str();
                    if let Some(e_code) = err.filter(|s| !s.is_empty() && *s != "NONE") {
                        self.log_warn(&format!(
                            "Unable to contribute channel points to community goal '{}', reason '{}'",
                            goal.title, e_code
                        ));
                    } else {
                        self.log_info(
                            "🎯",
                            &format!(
                                "Contributed {} channel points to community goal '{}'",
                                amount, goal.title
                            ),
                        );
                        streamer.set_channel_points(balance.saturating_sub(amount));
                    }
                }
                Err(e) => {
                    self.log_warn(&format!(
                        "Community goal '{}' contribution failed: {}",
                        goal.title, e
                    ));
                }
            }
        }

        Ok(())
    }
}
