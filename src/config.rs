use crate::chat::ChatPresence;
use crate::entities::StreamerSettings;
use crate::logger::LoggerSettings;
use crate::settings::MinerSettings;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FollowersOrder {
    #[default]
    #[serde(alias = "ASC", alias = "asc", alias = "Ascending")]
    Ascending,
    #[serde(alias = "DESC", alias = "desc", alias = "Descending")]
    Descending,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountConfig {
    pub username: String,
    pub streamers: Vec<String>,
    pub default_streamer_settings: StreamerSettings,
    pub streamer_settings: HashMap<String, StreamerSettings>,
    pub followers: bool,
    pub followers_order: FollowersOrder,
    pub blacklist: Vec<String>,
    pub claim_drops: bool,
    pub chat: ChatPresence,
    pub miner: MinerSettings,
    pub logger: LoggerSettings,
    pub notifications: crate::notify::NotifyConfig,
}

impl Default for AccountConfig {
    fn default() -> Self {
        Self {
            username: String::new(),
            streamers: Vec::new(),
            default_streamer_settings: StreamerSettings::default(),
            streamer_settings: HashMap::new(),
            followers: false,
            followers_order: FollowersOrder::Ascending,
            blacklist: Vec::new(),
            claim_drops: true,
            chat: ChatPresence::Online,
            miner: MinerSettings::default(),
            logger: LoggerSettings::default(),
            notifications: crate::notify::NotifyConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub accounts: Vec<AccountConfig>,
    pub analytics: bool,
    pub analytics_bind: String,
    pub analytics_refresh_minutes: u64,
    pub analytics_days_ago: u64,
    // Legacy single-account fields stay readable for existing configs and active GUI editing.
    #[serde(skip_serializing)]
    pub username: String,
    #[serde(skip_serializing)]
    pub streamers: Vec<String>,
    #[serde(skip_serializing)]
    pub default_streamer_settings: StreamerSettings,
    #[serde(skip_serializing)]
    pub streamer_settings: HashMap<String, StreamerSettings>,
    #[serde(skip_serializing)]
    pub followers: bool,
    #[serde(skip_serializing)]
    pub followers_order: FollowersOrder,
    #[serde(skip_serializing)]
    pub blacklist: Vec<String>,
    #[serde(skip_serializing)]
    pub claim_drops: bool,
    #[serde(skip_serializing)]
    pub chat: ChatPresence,
    #[serde(skip_serializing)]
    pub miner: MinerSettings,
    #[serde(skip_serializing)]
    pub logger: LoggerSettings,
    #[serde(skip_serializing)]
    pub notifications: crate::notify::NotifyConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            accounts: Vec::new(),
            analytics: true,
            analytics_bind: "127.0.0.1:9100".into(),
            analytics_refresh_minutes: 5,
            analytics_days_ago: 7,
            username: String::new(),
            streamers: Vec::new(),
            default_streamer_settings: StreamerSettings::default(),
            streamer_settings: HashMap::new(),
            followers: false,
            followers_order: FollowersOrder::Ascending,
            blacklist: Vec::new(),
            claim_drops: true,
            chat: ChatPresence::Online,
            miner: MinerSettings::default(),
            logger: LoggerSettings::default(),
            notifications: crate::notify::NotifyConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn effective_accounts(&self) -> Vec<AccountConfig> {
        if !self.accounts.is_empty() {
            return self
                .accounts
                .iter()
                .filter(|account| !account.username.trim().is_empty())
                .cloned()
                .collect();
        }
        if self.username.trim().is_empty() {
            return Vec::new();
        }
        vec![AccountConfig {
            username: self.username.clone(),
            streamers: self.streamers.clone(),
            default_streamer_settings: self.default_streamer_settings.clone(),
            streamer_settings: self.streamer_settings.clone(),
            followers: self.followers,
            followers_order: self.followers_order,
            blacklist: self.blacklist.clone(),
            claim_drops: self.claim_drops,
            chat: self.chat,
            miner: self.miner.clone(),
            logger: self.logger.clone(),
            notifications: self.notifications.clone(),
        }]
    }

    pub fn promote_legacy_account(&mut self) {
        if self.accounts.is_empty() {
            let mut accounts = self.effective_accounts();
            if accounts.is_empty() {
                accounts.push(AccountConfig::default());
            }
            self.accounts = accounts;
        }
        self.load_legacy_fields(0);
    }

    pub fn load_legacy_fields(&mut self, index: usize) {
        if let Some(account) = self.accounts.get(index) {
            self.username = account.username.clone();
            self.streamers = account.streamers.clone();
            self.default_streamer_settings = account.default_streamer_settings.clone();
            self.streamer_settings = account.streamer_settings.clone();
            self.followers = account.followers;
            self.followers_order = account.followers_order;
            self.blacklist = account.blacklist.clone();
            self.claim_drops = account.claim_drops;
            self.chat = account.chat;
            self.miner = account.miner.clone();
            self.logger = account.logger.clone();
            self.notifications = account.notifications.clone();
        }
    }

    pub fn save_legacy_fields(&mut self, index: usize) {
        if let Some(account) = self.accounts.get_mut(index) {
            account.username = self.username.clone();
            account.streamers = self.streamers.clone();
            account.default_streamer_settings = self.default_streamer_settings.clone();
            account.streamer_settings = self.streamer_settings.clone();
            account.followers = self.followers;
            account.followers_order = self.followers_order;
            account.blacklist = self.blacklist.clone();
            account.claim_drops = self.claim_drops;
            account.chat = self.chat;
            account.miner = self.miner.clone();
            account.logger = self.logger.clone();
            account.notifications = self.notifications.clone();
        }
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&contents)?)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(self)?;
        std::fs::write(path, contents)?;
        Ok(())
    }
}
