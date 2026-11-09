use crate::auth::TwitchAuth;
use crate::entities::Streamer;
use crate::error::{MinerError, Result};
use crate::twitch::gql;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use regex::Regex;
use serde_json::json;
use std::time::Duration;
use tracing::{debug, info, warn};

pub struct TwitchClient {
    pub auth: TwitchAuth,
}

impl TwitchClient {
    pub fn new(auth: TwitchAuth) -> Self {
        Self { auth }
    }

    pub async fn resolve_streamer(&self, streamer: &mut Streamer) -> Result<()> {
        let token = self.auth.access_token.as_ref().unwrap();
        let channel_id = gql::get_channel_id(
            &self.auth.client,
            token,
            &self.auth.device_id,
            &streamer.username,
        )
        .await?;

        streamer.channel_id = Some(channel_id.clone());
        info!(
            "Resolved {} -> channel_id={}",
            streamer.username, channel_id
        );

        // Points + available claim
        if let Ok(ctx) = gql::get_channel_points_context(
            &self.auth.client,
            token,
            &self.auth.device_id,
            &streamer.username,
        )
        .await
        {
            if let Some(points) =
                ctx["data"]["community"]["channel"]["self"]["communityPoints"]["balance"].as_u64()
            {
                streamer.set_channel_points(points);
                info!("{} has {} channel points", streamer.username, points);
            }

            if let Some(claim) = ctx["data"]["community"]["channel"]["self"]["communityPoints"]
                ["availableClaim"]
                .as_object()
            {
                if let Some(claim_id) = claim.get("id").and_then(|v| v.as_str()) {
                    info!(
                        "Found available claim for {}, claiming...",
                        streamer.username
                    );
                    match gql::claim_bonus(
                        &self.auth.client,
                        token,
                        &self.auth.device_id,
                        &channel_id,
                        claim_id,
                    )
                    .await
                    {
                        Ok(_) => info!("Successfully claimed bonus for {}", streamer.username),
                        Err(e) => warn!("Failed to claim bonus: {}", e),
                    }
                }
            }
        }

        self.update_stream_status(streamer).await?;
        Ok(())
    }

    pub async fn update_stream_status(&self, streamer: &Streamer) -> Result<()> {
        let token = self.auth.access_token.as_ref().unwrap();
        let channel_id = streamer
            .channel_id
            .as_ref()
            .ok_or_else(|| MinerError::Other("no channel_id".into()))?;

        let live = gql::is_stream_live(&self.auth.client, token, &self.auth.device_id, channel_id)
            .await
            .unwrap_or(false);

        let was_online = streamer.is_online();
        streamer.set_online(live);

        if live {
            if !was_online {
                info!("🥳 {} is Online!", streamer.username);
            }
            // Refresh stream info + spade url
            if let Err(e) = self.refresh_stream_info(streamer).await {
                warn!(
                    "Failed to refresh stream info for {}: {}",
                    streamer.username, e
                );
            }
        } else if was_online {
            info!("😴 {} is Offline", streamer.username);
        }

        Ok(())
    }

    async fn refresh_stream_info(&self, streamer: &Streamer) -> Result<()> {
        let token = self.auth.access_token.as_ref().unwrap();

        // Stream info via GQL
        if let Ok(info) = gql::get_stream_info(
            &self.auth.client,
            token,
            &self.auth.device_id,
            &streamer.username,
        )
        .await
        {
            let mut stream = streamer.stream.lock().unwrap();
            if let Some(user) = info["data"]["user"].as_object() {
                if let Some(s) = user.get("stream").and_then(|v| v.as_object()) {
                    stream.broadcast_id = s.get("id").and_then(|v| v.as_str()).map(String::from);
                    stream.viewers = s.get("viewersCount").and_then(|v| v.as_u64()).unwrap_or(0);
                    if let Some(game) = s.get("game").and_then(|v| v.as_object()) {
                        stream.game_name =
                            game.get("name").and_then(|v| v.as_str()).map(String::from);
                        stream.game_id = game.get("id").and_then(|v| v.as_str()).map(String::from);
                    }
                }
                if let Some(title) = user
                    .get("broadcastSettings")
                    .and_then(|v| v.get("title"))
                    .and_then(|v| v.as_str())
                {
                    stream.title = Some(title.to_string());
                }
            }
        }

        // Spade URL (needed for minute-watched)
        if let Err(e) = self.fetch_spade_url(streamer).await {
            warn!("Could not get spade_url for {}: {}", streamer.username, e);
        }

        Ok(())
    }

    async fn fetch_spade_url(&self, streamer: &Streamer) -> Result<()> {
        let page = self
            .auth
            .client
            .get(streamer.streamer_url())
            .header("User-Agent", crate::constants::USER_AGENT)
            .send()
            .await?
            .text()
            .await?;

        let re_settings = Regex::new(
            r#"(https://static\.twitchcdn\.net/config/settings.*?\.js|https://assets\.twitch\.tv/config/settings.*?\.js)"#,
        )
        .unwrap();

        let settings_url = re_settings
            .captures(&page)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .ok_or_else(|| MinerError::Other("settings.js not found".into()))?;

        let settings_js = self
            .auth
            .client
            .get(&settings_url)
            .header("User-Agent", crate::constants::USER_AGENT)
            .send()
            .await?
            .text()
            .await?;

        let re_spade = Regex::new(r#""spade_url":"(.*?)""#).unwrap();
        let spade = re_spade
            .captures(&settings_js)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().replace("\\u002F", "/"))
            .ok_or_else(|| MinerError::Other("spade_url not found".into()))?;

        let mut stream = streamer.stream.lock().unwrap();
        stream.spade_url = Some(spade);
        debug!("Got spade_url for {}", streamer.username);
        Ok(())
    }

    /// Simulate watching + send minute-watched event (core of points farming)
    pub async fn send_minute_watched(&self, streamer: &Streamer) -> Result<()> {
        if !streamer.is_online() {
            return Ok(());
        }

        let token = self.auth.access_token.as_ref().unwrap();
        let user_id = self.auth.user_id.as_ref().unwrap();

        // 1. PlaybackAccessToken
        let (signature, value) = gql::get_playback_access_token(
            &self.auth.client,
            token,
            &self.auth.device_id,
            &streamer.username,
        )
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
            .header("User-Agent", crate::constants::USER_AGENT)
            .timeout(Duration::from_secs(15))
            .send()
            .await?
            .text()
            .await?;

        // Take last non-empty line that looks like a URL (lowest quality)
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
            .header("User-Agent", crate::constants::USER_AGENT)
            .timeout(Duration::from_secs(15))
            .send()
            .await?
            .text()
            .await?;

        let segment_url = media
            .lines()
            .rev()
            .find(|l| l.starts_with("http"))
            .ok_or_else(|| MinerError::Other("no segment URL".into()))?;

        // 4. HEAD the segment (simulate watching)
        let _ = self
            .auth
            .client
            .head(segment_url)
            .header("User-Agent", crate::constants::USER_AGENT)
            .timeout(Duration::from_secs(10))
            .send()
            .await?;

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
            .ok_or_else(|| MinerError::Other("no channel_id".into()))?;

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
            .header("User-Agent", crate::constants::USER_AGENT)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!("data={}", encoded))
            .timeout(Duration::from_secs(15))
            .send()
            .await?;

        if resp.status().as_u16() == 204 {
            let mut stream = streamer.stream.lock().unwrap();
            stream.minute_watched += 1;
            info!(
                "👀 minute-watched OK for {} (total this session: {})",
                streamer.username, stream.minute_watched
            );
            *streamer.last_watch.lock().unwrap() = Some(std::time::Instant::now());
        } else {
            warn!(
                "minute-watched for {} returned {}",
                streamer.username,
                resp.status()
            );
        }

        Ok(())
    }

    /// Claim all claimable drops from inventory
    pub async fn claim_all_drops(&self) -> Result<u32> {
        let token = self.auth.access_token.as_ref().unwrap();
        let inv = gql::get_inventory(&self.auth.client, token, &self.auth.device_id).await?;

        let mut claimed = 0u32;

        if let Some(campaigns) =
            inv["data"]["currentUser"]["inventory"]["dropCampaignsInProgress"].as_array()
        {
            for campaign in campaigns {
                if let Some(drops) = campaign["timeBasedDrops"].as_array() {
                    for drop in drops {
                        let is_claimable = drop["self"]["isClaimable"].as_bool().unwrap_or(false);
                        if !is_claimable {
                            continue;
                        }
                        if let Some(instance_id) = drop["self"]["dropInstanceID"].as_str() {
                            let name = drop["name"].as_str().unwrap_or("unknown");
                            match gql::claim_drop(
                                &self.auth.client,
                                token,
                                &self.auth.device_id,
                                instance_id,
                            )
                            .await
                            {
                                Ok(true) => {
                                    info!("🎁 Claimed drop: {}", name);
                                    claimed += 1;
                                    tokio::time::sleep(Duration::from_secs(3)).await;
                                }
                                Ok(false) => {
                                    warn!("Could not claim drop: {}", name);
                                }
                                Err(e) => {
                                    warn!("Error claiming drop {}: {}", name, e);
                                }
                            }
                        }
                    }
                }
            }
        }

        if claimed > 0 {
            info!("Claimed {} drops from inventory", claimed);
        }
        Ok(claimed)
    }
    /// Contribute a small amount to active community goals (if enabled)
    pub async fn contribute_community_goals(&self, streamer: &Streamer) -> Result<()> {
        if !streamer.settings.community_goals {
            return Ok(());
        }
        let token = self.auth.access_token.as_ref().unwrap();
        let channel_id = match &streamer.channel_id {
            Some(id) => id.clone(),
            None => return Ok(()),
        };

        // Re-use channel points context which often includes community goals
        let ctx = gql::get_channel_points_context(
            &self.auth.client,
            token,
            &self.auth.device_id,
            &streamer.username,
        )
        .await?;

        let goals = ctx["data"]["community"]["channel"]["communityPointsSettings"]["goals"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        let balance = streamer.channel_points();
        for goal in goals {
            let goal_id = match goal.get("id").and_then(|v| v.as_str()) {
                Some(id) => id,
                None => continue,
            };
            let title = goal.get("title").and_then(|v| v.as_str()).unwrap_or("goal");
            let is_in_stock = goal
                .get("isInStock")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if !is_in_stock {
                continue;
            }
            // Contribute a modest amount (1% of balance, max 500)
            let amount = ((balance as f64 * 0.01) as u64).min(500).max(10);
            if amount > balance {
                continue;
            }
            match gql::contribute_community_goal(
                &self.auth.client,
                token,
                &self.auth.device_id,
                goal_id,
                &channel_id,
                amount,
            )
            .await
            {
                Ok(resp) => {
                    if let Some(err) =
                        resp["data"]["contributeCommunityPointsCommunityGoal"]["error"].as_str()
                    {
                        if !err.is_empty() && err != "NONE" {
                            warn!("Community goal '{}' error: {}", title, err);
                            continue;
                        }
                    }
                    info!(
                        "🎯 Contributed {} pts to community goal '{}'",
                        amount, title
                    );
                    streamer.set_channel_points(balance.saturating_sub(amount));
                }
                Err(e) => warn!("Community goal contribute failed: {}", e),
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        Ok(())
    }
}
