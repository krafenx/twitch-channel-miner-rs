use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Strategy {
    /// Pick the outcome with most users
    MostVoted,
    /// Pick highest odds
    HighOdds,
    /// Bet a fixed percentage of balance
    Percentage,
    /// Prefer the side with more points already placed (smart money)
    SmartMoney,
    /// Hybrid: if gap between sides is small use MostVoted, else HighOdds
    Smart,
    Number1,
    Number2,
    Number3,
    Number4,
    Number5,
    Number6,
    Number7,
    Number8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DelayMode {
    FromStart,
    FromEnd,
    Percentage,
}

impl Default for DelayMode {
    fn default() -> Self {
        Self::FromEnd
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterMetric {
    UsersPercentage,
    OddsPercentage,
    Odds,
    DecisionUsers,
    DecisionPoints,
    OutcomeUsers,
    OutcomePoints,
    TopPoints,
    TotalUsers,
    TotalPoints,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Comparison {
    GreaterThan,
    LessThan,
    GreaterOrEqual,
    LessOrEqual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterCondition {
    pub metric: FilterMetric,
    pub comparison: Comparison,
    pub value: f64,
}

impl Default for Strategy {
    fn default() -> Self {
        Self::Smart
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BetSettings {
    pub strategy: Strategy,
    /// Percent of current balance to bet (1-100)
    pub percentage: u32,
    /// For Smart strategy: if user% gap < this, use MostVoted
    pub percentage_gap: u32,
    pub max_points: u64,
    pub minimum_points: u64,
    pub stealth_mode: bool,
    pub filter_condition: Option<FilterCondition>,
    pub delay_mode: DelayMode,
    pub delay: f64,
}

impl Default for BetSettings {
    fn default() -> Self {
        Self {
            strategy: Strategy::Smart,
            percentage: 5,
            percentage_gap: 20,
            max_points: 50_000,
            minimum_points: 0,
            stealth_mode: true,
            filter_condition: None,
            delay_mode: DelayMode::FromEnd,
            delay: 6.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub id: String,
    pub title: String,
    pub total_users: u64,
    pub total_points: u64,
    pub odds: f64,
}

#[derive(Debug, Clone)]
pub struct PredictionEvent {
    pub event_id: String,
    pub title: String,
    pub channel_id: String,
    pub outcomes: Vec<Outcome>,
    pub status: String,
    pub prediction_window_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct BetDecision {
    pub outcome_id: String,
    pub outcome_title: String,
    pub amount: u64,
}

impl PredictionEvent {
    pub fn from_pubsub(data: &Value) -> Option<Self> {
        let event = data.get("data")?.get("event")?;
        let event_id = event.get("id")?.as_str()?.to_string();
        let title = event
            .get("title")?
            .as_str()
            .unwrap_or("Prediction")
            .to_string();
        let channel_id = event
            .get("channel_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let status = event
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let window = event
            .get("prediction_window_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(120);

        let mut outcomes = Vec::new();
        if let Some(arr) = event.get("outcomes").and_then(|v| v.as_array()) {
            for o in arr {
                let id = o.get("id")?.as_str()?.to_string();
                let title = o
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                let total_users = o.get("total_users").and_then(|v| v.as_u64()).unwrap_or(0);
                let total_points = o.get("total_points").and_then(|v| v.as_u64()).unwrap_or(0);
                // rough odds estimation
                let odds = if total_points > 0 {
                    // will be refined when we know both sides
                    1.0
                } else {
                    2.0
                };
                outcomes.push(Outcome {
                    id,
                    title,
                    total_users,
                    total_points,
                    odds,
                });
            }
        }

        // compute real odds
        let total_all: u64 = outcomes.iter().map(|o| o.total_points).sum();
        if total_all > 0 {
            for o in &mut outcomes {
                o.odds = total_all as f64 / o.total_points.max(1) as f64;
            }
        }

        Some(Self {
            event_id,
            title,
            channel_id,
            outcomes,
            status,
            prediction_window_seconds: window,
        })
    }

    pub fn decide(&self, balance: u64, settings: &BetSettings) -> Option<BetDecision> {
        if self.outcomes.len() < 2 {
            return None;
        }

        let choice_idx = match settings.strategy {
            Strategy::MostVoted => self
                .outcomes
                .iter()
                .enumerate()
                .max_by_key(|(_, o)| o.total_users)
                .map(|(i, _)| i)?,
            Strategy::HighOdds => self
                .outcomes
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| {
                    a.odds
                        .partial_cmp(&b.odds)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| i)?,
            Strategy::SmartMoney => self
                .outcomes
                .iter()
                .enumerate()
                .max_by_key(|(_, o)| o.total_points)
                .map(|(i, _)| i)?,
            Strategy::Percentage => self
                .outcomes
                .iter()
                .enumerate()
                .max_by_key(|(_, outcome)| outcome.total_points)
                .map(|(index, _)| index)?,
            Strategy::Number1 => 0,
            Strategy::Number2 => 1,
            Strategy::Number3 => 2,
            Strategy::Number4 => 3,
            Strategy::Number5 => 4,
            Strategy::Number6 => 5,
            Strategy::Number7 => 6,
            Strategy::Number8 => 7,
            Strategy::Smart => {
                // Smart: if user percentage gap is small -> MostVoted, else HighOdds
                let total_users: u64 = self.outcomes.iter().map(|o| o.total_users).sum();
                if total_users == 0 {
                    0
                } else {
                    let mut sorted: Vec<_> = self.outcomes.iter().enumerate().collect();
                    sorted.sort_by(|a, b| b.1.total_users.cmp(&a.1.total_users));
                    let top = sorted[0].1.total_users as f64 / total_users as f64 * 100.0;
                    let second = sorted
                        .get(1)
                        .map(|s| s.1.total_users as f64 / total_users as f64 * 100.0)
                        .unwrap_or(0.0);
                    let gap = top - second;
                    if gap > settings.percentage_gap as f64 {
                        sorted[0].0
                    } else {
                        // high odds
                        self.outcomes
                            .iter()
                            .enumerate()
                            .max_by(|(_, a), (_, b)| {
                                a.odds
                                    .partial_cmp(&b.odds)
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            })
                            .map(|(i, _)| i)
                            .unwrap_or(0)
                    }
                }
            }
        };

        let outcome = self.outcomes.get(choice_idx)?;
        if let Some(filter) = &settings.filter_condition {
            let users_total: u64 = self.outcomes.iter().map(|item| item.total_users).sum();
            let points_total: u64 = self.outcomes.iter().map(|item| item.total_points).sum();
            let top_points = self
                .outcomes
                .iter()
                .map(|item| item.total_points)
                .max()
                .unwrap_or(0);
            let actual = match filter.metric {
                FilterMetric::UsersPercentage => {
                    if users_total == 0 {
                        0.0
                    } else {
                        outcome.total_users as f64 / users_total as f64 * 100.0
                    }
                }
                FilterMetric::OddsPercentage => {
                    if outcome.odds == 0.0 {
                        0.0
                    } else {
                        100.0 / outcome.odds
                    }
                }
                FilterMetric::Odds => outcome.odds,
                FilterMetric::DecisionUsers => outcome.total_users as f64,
                FilterMetric::DecisionPoints => outcome.total_points as f64,
                FilterMetric::OutcomeUsers => outcome.total_users as f64,
                FilterMetric::OutcomePoints => outcome.total_points as f64,
                FilterMetric::TopPoints => top_points as f64,
                FilterMetric::TotalUsers => users_total as f64,
                FilterMetric::TotalPoints => points_total as f64,
            };
            let accepted = match filter.comparison {
                Comparison::GreaterThan => actual > filter.value,
                Comparison::LessThan => actual < filter.value,
                Comparison::GreaterOrEqual => actual >= filter.value,
                Comparison::LessOrEqual => actual <= filter.value,
            };
            if !accepted {
                return None;
            }
        }

        let mut amount = (balance as f64 * (settings.percentage as f64 / 100.0)) as u64;
        amount = amount.min(settings.max_points).min(balance);
        // `minimum_points` is the account balance threshold from the original
        // miner settings, not a minimum size for the calculated wager.
        if balance < settings.minimum_points {
            return None;
        }
        if amount == 0 {
            return None;
        }

        // stealth: leave a few points
        if settings.stealth_mode && amount > 10 {
            amount = amount.saturating_sub(rand_small());
        }

        Some(BetDecision {
            outcome_id: outcome.id.clone(),
            outcome_title: outcome.title.clone(),
            amount,
        })
    }
}

fn rand_small() -> u64 {
    use rand::Rng;
    rand::thread_rng().gen_range(1..15)
}

pub fn log_decision(event: &PredictionEvent, decision: &BetDecision) {
    info!(
        "🎲 Prediction '{}': betting {} points on '{}' ({})",
        event.title, decision.amount, decision.outcome_title, decision.outcome_id
    );
}
