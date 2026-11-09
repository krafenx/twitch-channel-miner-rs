use serde::{Deserialize, Serialize};
use tracing::{debug, info};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NotifyConfig {
    pub telegram: Option<TelegramConfig>,
    pub discord: Option<DiscordConfig>,
    pub webhook: Option<WebhookConfig>,
    pub gotify: Option<GotifyConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub chat_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordConfig {
    pub webhook_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GotifyConfig {
    pub url: String,
    pub token: String,
}

pub struct Notifier {
    config: NotifyConfig,
    client: reqwest::Client,
}

impl Notifier {
    pub fn new(config: NotifyConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    pub async fn send(&self, message: &str) {
        if let Some(tg) = &self.config.telegram {
            let url = format!("https://api.telegram.org/bot{}/sendMessage", tg.bot_token);
            let _ = self
                .client
                .post(&url)
                .json(&serde_json::json!({
                    "chat_id": tg.chat_id,
                    "text": message,
                    "parse_mode": "HTML"
                }))
                .send()
                .await;
            debug!("Telegram notification sent");
        }

        if let Some(dc) = &self.config.discord {
            let _ = self
                .client
                .post(&dc.webhook_url)
                .json(&serde_json::json!({
                    "content": message
                }))
                .send()
                .await;
            debug!("Discord notification sent");
        }

        if let Some(wh) = &self.config.webhook {
            let _ = self
                .client
                .post(&wh.url)
                .json(&serde_json::json!({
                    "message": message
                }))
                .send()
                .await;
            debug!("Webhook notification sent");
        }

        if let Some(g) = &self.config.gotify {
            let _ = self
                .client
                .post(format!("{}/message", g.url.trim_end_matches('/')))
                .header("X-Gotify-Key", &g.token)
                .json(&serde_json::json!({
                    "title": "Twitch Points Miner",
                    "message": message,
                    "priority": 5
                }))
                .send()
                .await;
            debug!("Gotify notification sent");
        }
    }

    pub async fn points_earned(&self, streamer: &str, points: i64, reason: &str) {
        let msg = format!("💰 <b>{}</b>: +{} points ({})", streamer, points, reason);
        self.send(&msg).await;
    }

    pub async fn drop_claimed(&self, name: &str) {
        let msg = format!("🎁 Drop claimed: <b>{}</b>", name);
        self.send(&msg).await;
    }

    pub async fn prediction(&self, title: &str, amount: u64, outcome: &str) {
        let msg = format!(
            "🎲 Prediction <b>{}</b>: bet {} on {}",
            title, amount, outcome
        );
        self.send(&msg).await;
    }
}
