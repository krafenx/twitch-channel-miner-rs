use crate::settings::MinerEvent;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{debug, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum WebhookMethod {
    #[serde(alias = "get", alias = "Get")]
    Get,
    #[default]
    #[serde(alias = "post", alias = "Post")]
    Post,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct NotifyConfig {
    pub telegram: Option<TelegramConfig>,
    pub discord: Option<DiscordConfig>,
    pub webhook: Option<WebhookConfig>,
    pub gotify: Option<GotifyConfig>,
    pub matrix: Option<MatrixConfig>,
    pub pushover: Option<PushoverConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub chat_id: String,
    pub disable_notification: bool,
    #[serde(default = "MinerEvent::all_vec")]
    pub events: Vec<MinerEvent>,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            bot_token: String::new(),
            chat_id: String::new(),
            disable_notification: false,
            events: MinerEvent::all_vec(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordConfig {
    pub webhook_url: String,
    #[serde(default = "MinerEvent::all_vec")]
    pub events: Vec<MinerEvent>,
}

impl Default for DiscordConfig {
    fn default() -> Self {
        Self {
            webhook_url: String::new(),
            events: MinerEvent::all_vec(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebhookConfig {
    #[serde(alias = "endpoint")]
    pub url: String,
    pub method: WebhookMethod,
    #[serde(default = "MinerEvent::all_vec")]
    pub events: Vec<MinerEvent>,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            method: WebhookMethod::Post,
            events: MinerEvent::all_vec(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GotifyConfig {
    pub url: String,
    pub token: String,
    pub priority: i32,
    #[serde(default = "MinerEvent::all_vec")]
    pub events: Vec<MinerEvent>,
}

impl Default for GotifyConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            token: String::new(),
            priority: 5,
            events: MinerEvent::all_vec(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MatrixConfig {
    pub username: String,
    pub password: String,
    pub homeserver: String,
    pub room_id: String,
    pub access_token: Option<String>,
    #[serde(default = "MinerEvent::all_vec")]
    pub events: Vec<MinerEvent>,
}

impl Default for MatrixConfig {
    fn default() -> Self {
        Self {
            username: String::new(),
            password: String::new(),
            homeserver: "matrix.org".to_string(),
            room_id: String::new(),
            access_token: None,
            events: MinerEvent::all_vec(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PushoverConfig {
    pub userkey: String,
    pub token: String,
    pub priority: i32,
    pub sound: String,
    #[serde(default = "MinerEvent::all_vec")]
    pub events: Vec<MinerEvent>,
}

impl Default for PushoverConfig {
    fn default() -> Self {
        Self {
            userkey: String::new(),
            token: String::new(),
            priority: 0,
            sound: "pushover".to_string(),
            events: MinerEvent::all_vec(),
        }
    }
}

pub struct Notifier {
    config: NotifyConfig,
    client: reqwest::Client,
    username_prefix: Option<String>,
    matrix_token: Mutex<Option<String>>,
}

impl Notifier {
    pub fn new(config: NotifyConfig) -> Self {
        Self::with_options(config, false, None)
    }

    pub fn with_options(
        config: NotifyConfig,
        disable_ssl_cert_verification: bool,
        username_prefix: Option<String>,
    ) -> Self {
        let initial_matrix_token = config
            .matrix
            .as_ref()
            .and_then(|m| m.access_token.clone())
            .filter(|t| !t.trim().is_empty());

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(disable_ssl_cert_verification)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            config,
            client,
            username_prefix,
            matrix_token: Mutex::new(initial_matrix_token),
        }
    }

    fn accepts_event(allowed: &[MinerEvent], event: Option<MinerEvent>) -> bool {
        match event {
            None => true,
            Some(ev) => allowed.contains(&ev),
        }
    }

    fn format_message(&self, raw: &str, keep_html: bool) -> String {
        let prefixed = match &self.username_prefix {
            Some(u) if !u.is_empty() => format!("[{u}] {raw}"),
            _ => raw.to_string(),
        };
        if keep_html {
            escape_telegram_html(&prefixed)
        } else {
            strip_basic_html(&prefixed)
        }
    }

    pub async fn send(&self, message: &str) {
        self.notify_event(None, message).await;
    }

    pub async fn notify_event(&self, event: Option<MinerEvent>, message: &str) {
        let event_name = event.map(|e| e.as_str()).unwrap_or("SYSTEM");

        if let Some(tg) = &self.config.telegram {
            if !tg.bot_token.trim().is_empty()
                && !tg.chat_id.trim().is_empty()
                && Self::accepts_event(&tg.events, event)
            {
                let url = format!("https://api.telegram.org/bot{}/sendMessage", tg.bot_token);
                let text = self.format_message(message, true);
                let _ = self
                    .client
                    .post(&url)
                    .json(&serde_json::json!({
                        "chat_id": tg.chat_id,
                        "text": text,
                        "parse_mode": "HTML",
                        "disable_web_page_preview": true,
                        "disable_notification": tg.disable_notification,
                    }))
                    .send()
                    .await;
                debug!("Telegram notification sent ({event_name})");
            }
        }

        if let Some(dc) = &self.config.discord {
            if !dc.webhook_url.trim().is_empty() && Self::accepts_event(&dc.events, event) {
                let text = self.format_message(message, false);
                let _ = self
                    .client
                    .post(&dc.webhook_url)
                    .json(&serde_json::json!({
                        "content": text,
                        "username": "Twitch Channel Points Miner",
                        "avatar_url": "https://i.imgur.com/X9fEkhT.png",
                    }))
                    .send()
                    .await;
                debug!("Discord notification sent ({event_name})");
            }
        }

        if let Some(wh) = &self.config.webhook {
            if !wh.url.trim().is_empty() && Self::accepts_event(&wh.events, event) {
                let text = self.format_message(message, false);
                let sep = if wh.url.contains('?') { '&' } else { '?' };
                let full_url = format!(
                    "{}{}event_name={}&message={}",
                    wh.url,
                    sep,
                    urlencoding::encode(event_name),
                    urlencoding::encode(&text)
                );
                match wh.method {
                    WebhookMethod::Get => {
                        let _ = self.client.get(&full_url).send().await;
                    }
                    WebhookMethod::Post => {
                        let _ = self
                            .client
                            .post(&full_url)
                            .json(&serde_json::json!({
                                "event_name": event_name,
                                "message": text,
                            }))
                            .send()
                            .await;
                    }
                }
                debug!("Webhook notification sent ({event_name})");
            }
        }

        if let Some(g) = &self.config.gotify {
            if !g.url.trim().is_empty() && Self::accepts_event(&g.events, event) {
                let text = self.format_message(message, false);
                let trimmed = g.url.trim_end_matches('/');
                let endpoint = if trimmed.contains("/message") {
                    trimmed.to_string()
                } else {
                    format!("{trimmed}/message")
                };
                let mut req = self.client.post(&endpoint).json(&serde_json::json!({
                    "title": "Twitch Channel Points Miner",
                    "message": text,
                    "priority": g.priority,
                }));
                if !g.token.trim().is_empty() {
                    req = req.header("X-Gotify-Key", &g.token);
                }
                let _ = req.send().await;
                debug!("Gotify notification sent ({event_name})");
            }
        }

        if let Some(mx) = &self.config.matrix {
            if !mx.homeserver.trim().is_empty()
                && !mx.room_id.trim().is_empty()
                && Self::accepts_event(&mx.events, event)
            {
                let text = self.format_message(message, false);
                self.send_matrix_message(mx, &text, event_name).await;
            }
        }

        if let Some(po) = &self.config.pushover {
            if !po.userkey.trim().is_empty()
                && !po.token.trim().is_empty()
                && Self::accepts_event(&po.events, event)
            {
                let text = self.format_message(message, false);
                let priority_str = po.priority.to_string();
                let mut form = vec![
                    ("user", po.userkey.as_str()),
                    ("token", po.token.as_str()),
                    ("message", text.as_str()),
                    ("title", "Twitch Channel Points Miner"),
                    ("priority", priority_str.as_str()),
                ];
                if !po.sound.trim().is_empty() {
                    form.push(("sound", po.sound.as_str()));
                }
                let _ = self
                    .client
                    .post("https://api.pushover.net/1/messages.json")
                    .form(&form)
                    .send()
                    .await;
                debug!("Pushover notification sent ({event_name})");
            }
        }
    }

    async fn send_matrix_message(&self, mx: &MatrixConfig, text: &str, event_name: &str) {
        let base = normalize_homeserver(&mx.homeserver);
        let encoded_room = urlencoding::encode(&mx.room_id);

        for attempt in 0..2 {
            let Some(token) = self.ensure_matrix_token(mx).await else {
                return;
            };
            let txn_id = uuid::Uuid::new_v4().to_string().replace('-', "");
            let url = format!(
                "{base}/_matrix/client/r0/rooms/{encoded_room}/send/m.room.message/{txn_id}?access_token={}",
                urlencoding::encode(&token)
            );
            match self
                .client
                .put(&url)
                .header("Authorization", format!("Bearer {token}"))
                .json(&serde_json::json!({
                    "body": text,
                    "msgtype": "m.text",
                }))
                .send()
                .await
            {
                Ok(resp) if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 => {
                    if attempt == 0 && !mx.username.trim().is_empty() && !mx.password.is_empty() {
                        *self.matrix_token.lock().await = None;
                        continue;
                    }
                }
                Ok(_) => {
                    debug!("Matrix notification sent ({event_name})");
                }
                Err(_) => {}
            }
            break;
        }
    }

    async fn ensure_matrix_token(&self, mx: &MatrixConfig) -> Option<String> {
        {
            let guard = self.matrix_token.lock().await;
            if let Some(tok) = guard.as_ref() {
                return Some(tok.clone());
            }
        }
        if mx.username.trim().is_empty() || mx.password.is_empty() {
            return None;
        }
        let base = normalize_homeserver(&mx.homeserver);
        let login_url = format!("{base}/_matrix/client/r0/login");
        let resp = self
            .client
            .post(&login_url)
            .json(&serde_json::json!({
                "user": mx.username,
                "password": mx.password,
                "type": "m.login.password",
            }))
            .send()
            .await
            .ok()?;

        let body: serde_json::Value = resp.json().await.ok()?;
        if let Some(token) = body.get("access_token").and_then(|v| v.as_str()) {
            let token = token.to_string();
            *self.matrix_token.lock().await = Some(token.clone());
            Some(token)
        } else {
            warn!("Matrix login failed; notifications will not be sent to Matrix");
            None
        }
    }

    pub async fn points_earned(&self, streamer: &str, points: i64, reason: &str) {
        let msg = format!("<b>{}</b>: +{} points ({})", streamer, points, reason);
        let ev = MinerEvent::from_reason_code(reason);
        self.notify_event(ev, &msg).await;
    }

    pub async fn drop_claimed(&self, name: &str) {
        let msg = format!("Drop claimed: <b>{}</b>", name);
        self.notify_event(Some(MinerEvent::DropClaim), &msg).await;
    }

    pub async fn prediction(&self, title: &str, amount: u64, outcome: &str) {
        let msg = format!("Prediction <b>{}</b>: bet {} on {}", title, amount, outcome);
        self.notify_event(Some(MinerEvent::BetGeneral), &msg).await;
    }
}

fn normalize_homeserver(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

fn escape_telegram_html(s: &str) -> String {
    let escaped = s
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    escaped
        .replace("&lt;b&gt;", "<b>")
        .replace("&lt;/b&gt;", "</b>")
        .replace("&lt;i&gt;", "<i>")
        .replace("&lt;/i&gt;", "</i>")
        .replace("&lt;code&gt;", "<code>")
        .replace("&lt;/code&gt;", "</code>")
}

fn strip_basic_html(s: &str) -> String {
    s.replace("<b>", "")
        .replace("</b>", "")
        .replace("<i>", "")
        .replace("</i>", "")
        .replace("<code>", "")
        .replace("</code>", "")
}
