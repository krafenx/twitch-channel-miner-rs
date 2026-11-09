use crate::bet::BetSettings;
use crate::chat::ChatPresence;
use crate::entities::drop::Campaign;
use crate::entities::goal::CommunityGoal;
use crate::settings::{millify_signed, millify_u64};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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
pub struct HistoryCounter {
    pub counter: i64,
    pub amount: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaidInfo {
    pub raid_id: String,
    pub target_login: String,
}

#[derive(Debug, Clone, Default)]
pub struct StreamInfo {
    pub broadcast_id: Option<String>,
    pub spade_url: Option<String>,
    pub game_name: Option<String>,
    pub game_display_name: Option<String>,
    pub game_id: Option<String>,
    pub title: Option<String>,
    pub tags: Vec<(String, String)>,
    pub drops_tags: bool,
    pub campaigns_ids: Vec<String>,
    pub campaigns: Vec<Campaign>,
    pub viewers: u64,
    pub minute_watched: f64,
    pub minute_watched_requests: u64,
    pub last_minute_watched_ts: Option<DateTime<Utc>>,
    pub last_update_ts: Option<DateTime<Utc>>,
    /// Legacy compatibility field
    pub watch_streak: u64,
}

impl StreamInfo {
    pub fn update_required(&self) -> bool {
        match self.last_update_ts {
            None => true,
            Some(ts) => (Utc::now() - ts) >= Duration::seconds(120),
        }
    }

    pub fn update_elapsed_secs(&self) -> i64 {
        match self.last_update_ts {
            None => 0,
            Some(ts) => (Utc::now() - ts).num_seconds().max(0),
        }
    }

    pub fn record_minute_watched(&mut self) {
        let now = Utc::now();
        if let Some(prev) = self.last_minute_watched_ts {
            let delta_mins = (now - prev).num_milliseconds().max(0) as f64 / 60_000.0;
            self.minute_watched += delta_mins;
        } else {
            self.minute_watched += 0.35;
        }
        self.last_minute_watched_ts = Some(now);
        self.minute_watched_requests += 1;
    }

    pub fn display_str(&self, less: bool) -> String {
        let title = self.title.as_deref().unwrap_or("Untitled");
        if less {
            title.to_string()
        } else {
            let game = self
                .game_display_name
                .as_deref()
                .or(self.game_name.as_deref())
                .unwrap_or("None");
            let tags_str = if self.tags.is_empty() {
                "None".to_string()
            } else {
                self.tags
                    .iter()
                    .map(|(_, name)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!("Stream(title={title}, game={game}, tags={tags_str})")
        }
    }
}

#[derive(Debug)]
pub struct Streamer {
    pub username: String,
    pub channel_id: Option<String>,
    pub channel_points: AtomicU64,
    pub initial_points: AtomicU64,
    pub initial_points_set: AtomicBool,
    pub is_online: AtomicBool,
    pub viewer_is_mod: AtomicBool,
    pub has_watch_streak: AtomicBool,
    pub settings: StreamerSettings,
    pub stream: Mutex<StreamInfo>,
    pub active_multipliers: Mutex<Vec<f64>>,
    pub community_goals: Mutex<HashMap<String, CommunityGoal>>,
    pub last_raid: Mutex<Option<RaidInfo>>,
    pub history: Mutex<HashMap<String, HistoryCounter>>,
    pub last_watch: Mutex<Option<std::time::Instant>>,
    pub online_since: Mutex<Option<std::time::Instant>>,
    pub online_at: Mutex<Option<DateTime<Utc>>>,
    pub offline_at: Mutex<Option<DateTime<Utc>>>,
    pub stream_up_at: Mutex<Option<DateTime<Utc>>>,
}

impl Streamer {
    pub fn new(username: impl Into<String>) -> Self {
        Self {
            username: username.into().trim().to_lowercase(),
            channel_id: None,
            channel_points: AtomicU64::new(0),
            initial_points: AtomicU64::new(0),
            initial_points_set: AtomicBool::new(false),
            is_online: AtomicBool::new(false),
            viewer_is_mod: AtomicBool::new(false),
            has_watch_streak: AtomicBool::new(false),
            settings: StreamerSettings::default(),
            stream: Mutex::new(StreamInfo::default()),
            active_multipliers: Mutex::new(Vec::new()),
            community_goals: Mutex::new(HashMap::new()),
            last_raid: Mutex::new(None),
            history: Mutex::new(HashMap::new()),
            last_watch: Mutex::new(None),
            online_since: Mutex::new(None),
            online_at: Mutex::new(None),
            offline_at: Mutex::new(None),
            stream_up_at: Mutex::new(None),
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
        if !self.initial_points_set.swap(true, Ordering::Relaxed) {
            self.initial_points.store(points, Ordering::Relaxed);
        }
    }

    pub fn initial_points(&self) -> u64 {
        self.initial_points.load(Ordering::Relaxed)
    }

    pub fn is_online(&self) -> bool {
        self.is_online.load(Ordering::Relaxed)
    }

    /// Returns `true` if the online state transitioned (`was_online != online`).
    pub fn set_online_with_transition(&self, online: bool, initial_resolve: bool) -> bool {
        let was = self.is_online.swap(online, Ordering::Relaxed);
        if online && !was {
            *self.online_since.lock().unwrap() = Some(std::time::Instant::now());
            let online_ts = if initial_resolve {
                // Already live when miner started — eligible immediately without 30s wait
                Utc::now() - Duration::seconds(60)
            } else {
                Utc::now()
            };
            *self.online_at.lock().unwrap() = Some(online_ts);
            // Initialize watch streak tracking for this new broadcast
            self.has_watch_streak.store(true, Ordering::Relaxed);
            let mut st = self.stream.lock().unwrap();
            st.minute_watched = 0.0;
            st.minute_watched_requests = 0;
            st.last_minute_watched_ts = None;
            true
        } else if !online && was {
            *self.online_since.lock().unwrap() = None;
            *self.offline_at.lock().unwrap() = Some(Utc::now());
            self.has_watch_streak.store(false, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub fn set_online(&self, online: bool) {
        let _ = self.set_online_with_transition(online, false);
    }

    pub fn is_in_offline_cooldown(&self) -> bool {
        if let Some(off) = *self.offline_at.lock().unwrap() {
            (Utc::now() - off) < Duration::seconds(60)
        } else {
            false
        }
    }

    pub fn ready_for_minute_watched(&self) -> bool {
        if !self.is_online() {
            return false;
        }
        match *self.online_at.lock().unwrap() {
            None => true,
            Some(ts) => (Utc::now() - ts) >= Duration::seconds(30),
        }
    }

    pub fn mark_stream_up(&self) {
        *self.stream_up_at.lock().unwrap() = Some(Utc::now());
    }

    pub fn stream_up_elapsed(&self) -> bool {
        match *self.stream_up_at.lock().unwrap() {
            None => true,
            Some(ts) => (Utc::now() - ts) > Duration::seconds(120),
        }
    }

    pub fn streamer_url(&self) -> String {
        format!("https://www.twitch.tv/{}", self.username)
    }

    /// Upstream `Priority.STREAK`:
    /// - `settings.watch_streak == true`
    /// - `watch_streak_missing == true` (`has_watch_streak`)
    /// - `offline_at` is None OR `(now - offline_at) > 30 minutes`
    /// - `stream.minute_watched < 7.0`
    pub fn has_active_streak(&self) -> bool {
        if !self.settings.watch_streak || !self.has_watch_streak.load(Ordering::Relaxed) {
            return false;
        }
        let offline_ok = match *self.offline_at.lock().unwrap() {
            None => true,
            Some(off) => (Utc::now() - off) >= Duration::minutes(30),
        };
        if !offline_ok {
            return false;
        }
        let mins = self.stream.lock().unwrap().minute_watched;
        mins < 7.0
    }

    /// Upstream `Streamer.drops_condition()`:
    /// Streamer is online, has `claim_drops` enabled, and has active drop campaigns for the stream.
    pub fn drops_condition(&self) -> bool {
        if !self.settings.claim_drops || !self.is_online() {
            return false;
        }
        let st = self.stream.lock().unwrap();
        !st.campaigns.is_empty() || !st.campaigns_ids.is_empty()
    }

    pub fn viewer_has_points_multiplier(&self) -> bool {
        !self.active_multipliers.lock().unwrap().is_empty()
    }

    pub fn total_points_multiplier(&self) -> f64 {
        self.active_multipliers.lock().unwrap().iter().sum()
    }

    pub fn update_history(&self, reason_code: &str, earned: i64, counter_delta: i64) {
        let key = reason_code.to_uppercase();
        if key == "WATCH_STREAK" {
            self.has_watch_streak.store(false, Ordering::Relaxed);
        }
        let mut hist = self.history.lock().unwrap();
        let entry = hist.entry(key).or_default();
        entry.counter += counter_delta;
        entry.amount += earned;
    }

    pub fn has_history(&self) -> bool {
        self.history
            .lock()
            .unwrap()
            .values()
            .any(|v| v.counter != 0)
    }

    pub fn print_history(&self) -> String {
        let hist = self.history.lock().unwrap();
        let mut keys: Vec<&String> = hist
            .iter()
            .filter_map(|(k, v)| if v.counter != 0 { Some(k) } else { None })
            .collect();
        keys.sort();
        keys.into_iter()
            .map(|k| {
                let v = &hist[k];
                format!(
                    "{} ({} times, {} gained)",
                    k,
                    v.counter,
                    millify_signed(v.amount)
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub fn display_str(&self, less: bool) -> String {
        let pts = millify_u64(self.channel_points());
        if less {
            format!("{} ({} points)", self.username, pts)
        } else {
            format!(
                "Streamer(username={}, channel_id={}, channel_points={})",
                self.username,
                self.channel_id.as_deref().unwrap_or("?"),
                pts
            )
        }
    }

    pub fn update_community_goal(&self, goal: CommunityGoal) {
        self.community_goals
            .lock()
            .unwrap()
            .insert(goal.goal_id.clone(), goal);
    }

    pub fn delete_community_goal(&self, goal_id: &str) {
        self.community_goals.lock().unwrap().remove(goal_id);
    }
}

pub type SharedStreamer = Arc<Streamer>;
