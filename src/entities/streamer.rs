use crate::bet::BetSettings;
use crate::chat::ChatPresence;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamerSettings {
    pub make_predictions: bool,
    pub follow_raid: bool,
    pub claim_drops: bool,
    pub claim_moments: bool,
    pub watch_streak: bool,
    pub community_goals: bool,
    pub chat: Option<ChatPresence>,
    pub bet: BetSettings,
}

impl Default for StreamerSettings {
    fn default() -> Self {
        Self {
            make_predictions: true,
            follow_raid: true,
            claim_drops: true,
            claim_moments: true,
            watch_streak: true,
            community_goals: false,
            chat: None,
            bet: BetSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct StreamInfo {
    pub broadcast_id: Option<String>,
    pub spade_url: Option<String>,
    pub game_name: Option<String>,
    pub game_id: Option<String>,
    pub title: Option<String>,
    pub viewers: u64,
    pub minute_watched: u64,
    /// Watch streak counter (resets when offline for long)
    pub watch_streak: u64,
}

// helper to avoid import issues

#[derive(Debug)]
pub struct Streamer {
    pub username: String,
    pub channel_id: Option<String>,
    pub channel_points: AtomicU64,
    pub is_online: AtomicBool,
    pub settings: StreamerSettings,
    pub stream: std::sync::Mutex<StreamInfo>,
    pub last_watch: std::sync::Mutex<Option<std::time::Instant>>,
    pub online_since: std::sync::Mutex<Option<std::time::Instant>>,
    pub has_watch_streak: AtomicBool,
}

impl Streamer {
    pub fn new(username: impl Into<String>) -> Self {
        Self {
            username: username.into().to_lowercase(),
            channel_id: None,
            channel_points: AtomicU64::new(0),
            is_online: AtomicBool::new(false),
            settings: StreamerSettings::default(),
            stream: std::sync::Mutex::new(StreamInfo::default()),
            last_watch: std::sync::Mutex::new(None),
            online_since: std::sync::Mutex::new(None),
            has_watch_streak: AtomicBool::new(false),
        }
    }

    pub fn with_settings(mut self, settings: StreamerSettings) -> Self {
        self.settings = settings;
        self
    }

    pub fn channel_points(&self) -> u64 {
        self.channel_points.load(Ordering::Relaxed)
    }

    pub fn set_channel_points(&self, points: u64) {
        self.channel_points.store(points, Ordering::Relaxed);
    }

    pub fn is_online(&self) -> bool {
        self.is_online.load(Ordering::Relaxed)
    }

    pub fn set_online(&self, online: bool) {
        let was = self.is_online.swap(online, Ordering::Relaxed);
        if online && !was {
            *self.online_since.lock().unwrap() = Some(std::time::Instant::now());
        } else if !online {
            *self.online_since.lock().unwrap() = None;
            self.has_watch_streak.store(false, Ordering::Relaxed);
        }
    }

    pub fn streamer_url(&self) -> String {
        format!("https://www.twitch.tv/{}", self.username)
    }

    pub fn has_active_streak(&self) -> bool {
        self.settings.watch_streak && self.has_watch_streak.load(Ordering::Relaxed)
    }
}

pub type SharedStreamer = Arc<Streamer>;
