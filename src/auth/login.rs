use crate::constants::{CLIENT_ID, TV_USER_AGENT, USER_AGENT};
use crate::error::{MinerError, Result};
use crate::logger::SessionLogger;
use chrono::{DateTime, Duration, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tracing::{error, info, warn};
use uuid::Uuid;

#[derive(Clone)]
pub struct TwitchAuth {
    pub client_id: String,
    pub device_id: String,
    pub access_token: Option<String>,
    pub user_id: Option<String>,
    pub username: String,
    pub client: Client,
    pub disable_ssl_cert_verification: bool,
    pub logger: Option<Arc<SessionLogger>>,
}

impl std::fmt::Debug for TwitchAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TwitchAuth")
            .field("client_id", &self.client_id)
            .field("device_id", &self.device_id)
            .field("user_id", &self.user_id)
            .field("username", &self.username)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: Option<String>,
    expires_in: u64,
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[allow(dead_code)]
    refresh_token: Option<String>,
    #[allow(dead_code)]
    expires_in: Option<u64>,
    #[allow(dead_code)]
    token_type: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SavedSession {
    username: String,
    access_token: String,
    user_id: String,
    device_id: String,
    saved_at: DateTime<Utc>,
}

impl TwitchAuth {
    pub fn new(username: &str) -> Result<Self> {
        Self::with_options(username, false, None)
    }

    pub fn with_options(
        username: &str,
        disable_ssl_cert_verification: bool,
        logger: Option<Arc<SessionLogger>>,
    ) -> Result<Self> {
        let device_id = Uuid::new_v4().to_string().replace('-', "");
        if disable_ssl_cert_verification {
            if let Some(log) = &logger {
                log.warn("SSL certificate verification is disabled! Be aware!");
            } else {
                warn!("SSL certificate verification is disabled! Be aware!");
            }
        }
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .cookie_store(true)
            .danger_accept_invalid_certs(disable_ssl_cert_verification)
            .build()?;

        Ok(Self {
            client_id: CLIENT_ID.to_string(),
            device_id,
            access_token: None,
            user_id: None,
            username: username.to_string(),
            client,
            disable_ssl_cert_verification,
            logger,
        })
    }

    fn log_info(&self, emoji: &str, msg: &str) {
        if let Some(log) = &self.logger {
            log.info(emoji, msg);
        } else {
            info!("{msg}");
        }
    }

    fn log_warn(&self, msg: &str) {
        if let Some(log) = &self.logger {
            log.warn(msg);
        } else {
            warn!("{msg}");
        }
    }

    fn session_path(username: &str) -> PathBuf {
        let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        path.push("twitch-points-miner");
        std::fs::create_dir_all(&path).ok();
        path.push(format!("{}.json", username.to_lowercase()));
        path
    }

    pub fn load_session(&mut self) -> Result<bool> {
        let path = Self::session_path(&self.username);
        if !path.exists() {
            return Ok(false);
        }

        let data = std::fs::read_to_string(&path)?;
        let session: SavedSession = serde_json::from_str(&data)?;

        if Utc::now() - session.saved_at > Duration::days(60) {
            self.log_warn("Saved session is older than 60 days, will re-validate / re-login");
            return Ok(false);
        }

        self.access_token = Some(session.access_token);
        self.user_id = Some(session.user_id);
        self.device_id = session.device_id;
        self.log_info("🍪", &format!("Loaded saved session for {}", self.username));
        Ok(true)
    }

    pub fn save_session(&self) -> Result<()> {
        let token = self
            .access_token
            .as_ref()
            .ok_or_else(|| MinerError::Auth("No token to save".into()))?;
        let user_id = self
            .user_id
            .as_ref()
            .ok_or_else(|| MinerError::Auth("No user_id to save".into()))?;

        let session = SavedSession {
            username: self.username.clone(),
            access_token: token.clone(),
            user_id: user_id.clone(),
            device_id: self.device_id.clone(),
            saved_at: Utc::now(),
        };

        let path = Self::session_path(&self.username);
        let data = serde_json::to_string_pretty(&session)?;
        std::fs::write(path, data)?;
        self.log_info("💾", &format!("Session saved for {}", self.username));
        Ok(())
    }

    /// Device Code OAuth flow (same as original TV login in `TwitchLogin.py`)
    pub async fn login(&mut self) -> Result<()> {
        if self.load_session()? {
            if self.validate_token().await.is_ok() {
                return Ok(());
            }
            self.log_warn("Saved token is invalid or expired, starting new TV device login");
        }

        self.log_info(
            "🔑",
            &format!("Starting Twitch TV device code login for {}", self.username),
        );

        let device_resp = self
            .client
            .post("https://id.twitch.tv/oauth2/device")
            .header("Accept", "application/json")
            .header("Client-Id", &self.client_id)
            .header("Origin", "https://android.tv.twitch.tv")
            .header("Referer", "https://android.tv.twitch.tv/")
            .header("User-Agent", TV_USER_AGENT)
            .header("X-Device-Id", &self.device_id)
            .form(&[
                ("client_id", self.client_id.as_str()),
                (
                    "scopes",
                    "channel_read chat:read user_blocks_edit user_blocks_read user_follows_edit user_read",
                ),
            ])
            .send()
            .await?
            .error_for_status()?
            .json::<DeviceCodeResponse>()
            .await?;

        let verification = device_resp
            .verification_uri
            .unwrap_or_else(|| "https://www.twitch.tv/activate".to_string());

        self.log_info("", "======================================================");
        self.log_info("🌐", &format!("Open {verification} in your browser"));
        self.log_info("🔢", &format!("And enter this code: {}", device_resp.user_code));
        self.log_info(
            "⏳",
            &format!("Hurry up! It will expire in {} minutes!", device_resp.expires_in / 60),
        );
        self.log_info("", "======================================================");

        let expires_at = Utc::now() + Duration::seconds(device_resp.expires_in as i64);
        let interval = StdDuration::from_secs(device_resp.interval.max(5));

        loop {
            if Utc::now() >= expires_at {
                return Err(MinerError::Auth("Device code expired".into()));
            }

            tokio::time::sleep(interval).await;

            let token_resp = self
                .client
                .post("https://id.twitch.tv/oauth2/token")
                .header("Accept", "application/json")
                .header("Client-Id", &self.client_id)
                .header("Origin", "https://android.tv.twitch.tv")
                .header("Referer", "https://android.tv.twitch.tv/")
                .header("User-Agent", TV_USER_AGENT)
                .header("X-Device-Id", &self.device_id)
                .form(&[
                    ("client_id", self.client_id.as_str()),
                    ("device_code", device_resp.device_code.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .send()
                .await?;

            if token_resp.status().is_success() {
                let token: TokenResponse = token_resp.json().await?;
                self.access_token = Some(token.access_token);
                self.log_info("✅", "Successfully obtained Twitch access token!");
                break;
            } else {
                let status = token_resp.status();
                let body = token_resp.text().await.unwrap_or_default();
                if !body.contains("authorization_pending") && !body.contains("slow_down") {
                    error!("Token request failed ({}): {}", status, body);
                }
            }
        }

        self.fetch_user_id().await?;
        self.save_session()?;
        Ok(())
    }

    async fn validate_token(&self) -> Result<()> {
        let token = self
            .access_token
            .as_ref()
            .ok_or_else(|| MinerError::Auth("No token".into()))?;

        let resp = self
            .client
            .get("https://id.twitch.tv/oauth2/validate")
            .header("Authorization", format!("OAuth {}", token))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(MinerError::Auth("Token validation failed".into()))
        }
    }

    async fn fetch_user_id(&mut self) -> Result<()> {
        let token = self
            .access_token
            .as_ref()
            .ok_or_else(|| MinerError::Auth("No token".into()))?;

        let resp = self
            .client
            .get("https://id.twitch.tv/oauth2/validate")
            .header("Authorization", format!("OAuth {}", token))
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;

        if let Some(uid) = resp.get("user_id").and_then(|v| v.as_str()) {
            self.user_id = Some(uid.to_string());
            self.log_info(
                "👤",
                &format!("Logged in as {} (user_id={})", self.username, uid),
            );
        } else {
            return Err(MinerError::Auth(
                "Could not get user_id from validate".into(),
            ));
        }
        Ok(())
    }

    pub fn auth_header(&self) -> Result<String> {
        self.access_token
            .as_ref()
            .map(|t| format!("OAuth {}", t))
            .ok_or_else(|| MinerError::Auth("Not authenticated".into()))
    }

    pub fn bearer_header(&self) -> Result<String> {
        self.access_token
            .as_ref()
            .map(|t| format!("Bearer {}", t))
            .ok_or_else(|| MinerError::Auth("Not authenticated".into()))
    }
}
