use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Priority {
    /// Prefer streamers with active watch-streak
    Streak,
    /// Prefer streamers that have drops
    Drops,
    /// Prefer subscribed / multiplier channels (sorted by multiplier descending)
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

/// All 19 event types from upstream `Settings.Events`, used for notification filtering
/// and per-event log coloring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MinerEvent {
    #[serde(alias = "StreamerOnline")]
    StreamerOnline,
    #[serde(alias = "StreamerOffline")]
    StreamerOffline,
    #[serde(alias = "GainForRaid")]
    GainForRaid,
    #[serde(alias = "GainForClaim")]
    GainForClaim,
    #[serde(alias = "GainForWatch")]
    GainForWatch,
    #[serde(alias = "GainForWatchStreak")]
    GainForWatchStreak,
    #[serde(alias = "BetWin")]
    BetWin,
    #[serde(alias = "BetLose")]
    BetLose,
    #[serde(alias = "BetRefund")]
    BetRefund,
    #[serde(alias = "BetFilters")]
    BetFilters,
    #[serde(alias = "BetGeneral")]
    BetGeneral,
    #[serde(alias = "BetFailed")]
    BetFailed,
    #[serde(alias = "BetStart")]
    BetStart,
    #[serde(alias = "BonusClaim")]
    BonusClaim,
    #[serde(alias = "MomentClaim")]
    MomentClaim,
    #[serde(alias = "JoinRaid")]
    JoinRaid,
    #[serde(alias = "DropClaim")]
    DropClaim,
    #[serde(alias = "DropStatus")]
    DropStatus,
    #[serde(alias = "ChatMention")]
    ChatMention,
}

impl MinerEvent {
    pub const ALL: [MinerEvent; 19] = [
        Self::StreamerOnline,
        Self::StreamerOffline,
        Self::GainForRaid,
        Self::GainForClaim,
        Self::GainForWatch,
        Self::GainForWatchStreak,
        Self::BetWin,
        Self::BetLose,
        Self::BetRefund,
        Self::BetFilters,
        Self::BetGeneral,
        Self::BetFailed,
        Self::BetStart,
        Self::BonusClaim,
        Self::MomentClaim,
        Self::JoinRaid,
        Self::DropClaim,
        Self::DropStatus,
        Self::ChatMention,
    ];

    pub fn all_vec() -> Vec<Self> {
        Self::ALL.to_vec()
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::StreamerOnline => "STREAMER_ONLINE",
            Self::StreamerOffline => "STREAMER_OFFLINE",
            Self::GainForRaid => "GAIN_FOR_RAID",
            Self::GainForClaim => "GAIN_FOR_CLAIM",
            Self::GainForWatch => "GAIN_FOR_WATCH",
            Self::GainForWatchStreak => "GAIN_FOR_WATCH_STREAK",
            Self::BetWin => "BET_WIN",
            Self::BetLose => "BET_LOSE",
            Self::BetRefund => "BET_REFUND",
            Self::BetFilters => "BET_FILTERS",
            Self::BetGeneral => "BET_GENERAL",
            Self::BetFailed => "BET_FAILED",
            Self::BetStart => "BET_START",
            Self::BonusClaim => "BONUS_CLAIM",
            Self::MomentClaim => "MOMENT_CLAIM",
            Self::JoinRaid => "JOIN_RAID",
            Self::DropClaim => "DROP_CLAIM",
            Self::DropStatus => "DROP_STATUS",
            Self::ChatMention => "CHAT_MENTION",
        }
    }

    pub fn from_reason_code(reason_code: &str) -> Option<Self> {
        match reason_code.to_ascii_uppercase().as_str() {
            "RAID" => Some(Self::GainForRaid),
            "CLAIM" => Some(Self::GainForClaim),
            "WATCH" => Some(Self::GainForWatch),
            "WATCH_STREAK" => Some(Self::GainForWatchStreak),
            _ => None,
        }
    }

    pub fn from_bet_result(result_type: &str) -> Option<Self> {
        match result_type.to_ascii_uppercase().as_str() {
            "WIN" => Some(Self::BetWin),
            "LOSE" => Some(Self::BetLose),
            "REFUND" => Some(Self::BetRefund),
            _ => None,
        }
    }

    pub fn from_name_case_insensitive(name: &str) -> Option<Self> {
        let normalized = name.trim().to_ascii_uppercase().replace('-', "_");
        Self::ALL.iter().copied().find(|ev| ev.as_str() == normalized)
    }
}

impl fmt::Display for MinerEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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
    pub context_refresh_secs: u64,
    pub disable_ssl_cert_verification: bool,
    pub disable_at_in_nickname: bool,
}

impl Default for MinerSettings {
    fn default() -> Self {
        Self {
            priority: vec![Priority::Streak, Priority::Drops, Priority::Order],
            claim_drops_startup: true,
            max_watch_concurrent: 2,
            watch_interval_secs: 20,
            status_refresh_secs: 120,
            drops_claim_interval_secs: 1800,
            context_refresh_secs: 1800,
            disable_ssl_cert_verification: false,
            disable_at_in_nickname: false,
        }
    }
}

/// Format numbers cleanly like upstream `_millify` (e.g. `1250 -> 1.25k`, `-500 -> -500`).
pub fn millify_signed(value: i64) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let abs = value.unsigned_abs() as f64;
    if abs < 1_000.0 {
        format!("{sign}{}", value.unsigned_abs())
    } else if abs < 1_000_000.0 {
        let scaled = (abs / 1_000.0 * 100.0).round() / 100.0;
        format!("{sign}{}", trim_trailing_zeros(scaled)) + "k"
    } else if abs < 1_000_000_000.0 {
        let scaled = (abs / 1_000_000.0 * 100.0).round() / 100.0;
        format!("{sign}{}", trim_trailing_zeros(scaled)) + "M"
    } else {
        let scaled = (abs / 1_000_000_000.0 * 100.0).round() / 100.0;
        format!("{sign}{}", trim_trailing_zeros(scaled)) + "B"
    }
}

pub fn millify_u64(value: u64) -> String {
    millify_signed(value.min(i64::MAX as u64) as i64)
}

fn trim_trailing_zeros(val: f64) -> String {
    let s = format!("{val:.2}");
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    s.to_string()
}
