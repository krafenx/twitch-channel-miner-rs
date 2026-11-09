use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Priority {
    /// Prefer streamers with active watch-streak
    Streak,
    /// Prefer streamers that have drops
    Drops,
    /// Prefer subscribed / multiplier channels
    Subscribed,
    /// Keep the order from the config list
    Order,
    /// Lowest points first
    PointsAscending,
    /// Highest points first
    PointsDescending,
}

impl Default for Priority {
    fn default() -> Self {
        Self::Order
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MinerSettings {
    pub priority: Vec<Priority>,
    pub claim_drops_startup: bool,
    pub max_watch_concurrent: usize,
    pub watch_interval_secs: u64,
    pub status_refresh_secs: u64,
    pub drops_claim_interval_secs: u64,
}

impl Default for MinerSettings {
    fn default() -> Self {
        Self {
            priority: vec![Priority::Streak, Priority::Drops, Priority::Order],
            claim_drops_startup: true,
            max_watch_concurrent: 2,
            watch_interval_secs: 25,
            status_refresh_secs: 120,
            drops_claim_interval_secs: 1800,
        }
    }
}
