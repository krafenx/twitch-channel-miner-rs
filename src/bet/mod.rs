use crate::entities::drop::parse_twitch_datetime;
use crate::settings::{millify_signed, millify_u64};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::info;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Strategy {
    /// Pick the outcome with most users
    MostVoted,
    /// Pick highest odds
    HighOdds,
    /// Pick highest odds percentage (same percentage shown by Twitch)
    Percentage,
    /// Pick the outcome with the highest single top predictor bet (`TOP_POINTS`)
    SmartMoney,
    /// Hybrid: if user% gap >= percentage_gap use MostVoted, else HighOdds
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

impl Default for Strategy {
    fn default() -> Self {
        Self::Smart
    }
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
    #[serde(alias = "PERCENTAGE_USERS", alias = "percentage_users")]
    UsersPercentage,
    #[serde(alias = "ODDS_PERCENTAGE", alias = "odds_percentage")]
    OddsPercentage,
    #[serde(alias = "ODDS", alias = "odds")]
    Odds,
    #[serde(alias = "DECISION_USERS", alias = "decision_users")]
    DecisionUsers,
    #[serde(alias = "DECISION_POINTS", alias = "decision_points")]
    DecisionPoints,
    OutcomeUsers,
    OutcomePoints,
    #[serde(alias = "TOP_POINTS", alias = "top_points")]
    TopPoints,
    #[serde(alias = "TOTAL_USERS", alias = "total_users")]
    TotalUsers,
    #[serde(alias = "TOTAL_POINTS", alias = "total_points")]
    TotalPoints,
}

impl FilterMetric {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UsersPercentage => "PERCENTAGE_USERS",
            Self::OddsPercentage => "ODDS_PERCENTAGE",
            Self::Odds => "ODDS",
            Self::DecisionUsers | Self::OutcomeUsers => "DECISION_USERS",
            Self::DecisionPoints | Self::OutcomePoints => "DECISION_POINTS",
            Self::TopPoints => "TOP_POINTS",
            Self::TotalUsers => "TOTAL_USERS",
            Self::TotalPoints => "TOTAL_POINTS",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Comparison {
    #[serde(alias = "GT")]
    GreaterThan,
    #[serde(alias = "LT")]
    LessThan,
    #[serde(alias = "GTE")]
    GreaterOrEqual,
    #[serde(alias = "LTE")]
    LessOrEqual,
}

impl Comparison {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GreaterThan => "GT",
            Self::LessThan => "LT",
            Self::GreaterOrEqual => "GTE",
            Self::LessOrEqual => "LTE",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterCondition {
    #[serde(alias = "by")]
    pub metric: FilterMetric,
    #[serde(alias = "where")]
    pub comparison: Comparison,
    pub value: f64,
}

impl FilterCondition {
    pub fn summary(&self) -> String {
        format!(
            "FilterCondition(by={}, where={}, value={})",
            self.metric.as_str(),
            self.comparison.as_str(),
            self.value
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BetSettings {
    pub strategy: Strategy,
    /// Percent of current balance to bet (1-100)
    pub percentage: u32,
    /// For Smart strategy: if user% gap < this, use HighOdds; else MostVoted
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
            stealth_mode: false,
            filter_condition: None,
            delay_mode: DelayMode::FromEnd,
            delay: 6.0,
        }
    }
}

impl BetSettings {
    pub fn summary(&self) -> String {
        format!(
            "BetSettings(strategy={:?}, percentage={}, percentage_gap={}, max_points={}, minimum_points={}, stealth_mode={})",
            self.strategy,
            self.percentage,
            self.percentage_gap,
            self.max_points,
            self.minimum_points,
            self.stealth_mode
        )
    }
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub id: String,
    pub title: String,
    pub color: String,
    pub total_users: u64,
    pub total_points: u64,
    pub top_points: u64,
    pub percentage_users: f64,
    pub odds: f64,
    pub odds_percentage: f64,
}

impl Outcome {
    pub fn summary(&self) -> String {
        format!(
            "{} ({}), Points: {}, Users: {} ({}%), Odds: {} ({}%)",
            self.title,
            self.color,
            millify_u64(self.total_points),
            millify_u64(self.total_users),
            self.percentage_users,
            self.odds,
            self.odds_percentage
        )
    }
}

pub fn parse_outcomes_from_json(arr: &[Value]) -> Vec<Outcome> {
    let mut outcomes = Vec::with_capacity(arr.len());
    for o in arr {
        let Some(id) = o.get("id").and_then(|v| v.as_str()).map(String::from) else {
            continue;
        };
        let title = o
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let color = o
            .get("color")
            .and_then(|v| v.as_str())
            .unwrap_or("BLUE")
            .to_string();
        let total_users = o.get("total_users").and_then(|v| v.as_u64()).unwrap_or(0);
        let total_points = o.get("total_points").and_then(|v| v.as_u64()).unwrap_or(0);
        let top_points = o
            .get("top_predictors")
            .and_then(|v| v.as_array())
            .and_then(|predictors| {
                predictors
                    .iter()
                    .filter_map(|p| p.get("points").and_then(|pts| pts.as_u64()))
                    .max()
            })
            .unwrap_or(0);

        outcomes.push(Outcome {
            id,
            title,
            color,
            total_users,
            total_points,
            top_points,
            percentage_users: 0.0,
            odds: 0.0,
            odds_percentage: 0.0,
        });
    }

    recompute_outcome_ratios(&mut outcomes);
    outcomes
}

pub fn recompute_outcome_ratios(outcomes: &mut [Outcome]) {
    let total_users: u64 = outcomes.iter().map(|o| o.total_users).sum();
    let total_points: u64 = outcomes.iter().map(|o| o.total_points).sum();

    if total_users > 0 && total_points > 0 {
        for o in outcomes.iter_mut() {
            o.percentage_users = round2((100.0 * o.total_users as f64) / total_users as f64);
            o.odds = if o.total_points == 0 {
                0.0
            } else {
                round2(total_points as f64 / o.total_points as f64)
            };
            o.odds_percentage = if o.odds == 0.0 {
                0.0
            } else {
                round2(100.0 / o.odds)
            };
        }
    } else {
        for o in outcomes.iter_mut() {
            o.percentage_users = if total_users > 0 {
                round2((100.0 * o.total_users as f64) / total_users as f64)
            } else {
                0.0
            };
            o.odds = 0.0;
            o.odds_percentage = 0.0;
        }
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[derive(Debug, Clone)]
pub struct PredictionEvent {
    pub event_id: String,
    pub title: String,
    pub channel_id: String,
    pub outcomes: Vec<Outcome>,
    pub status: String,
    pub prediction_window_seconds: f64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct BetDecision {
    pub choice_index: usize,
    pub outcome_id: String,
    pub outcome_title: String,
    pub outcome_color: String,
    pub amount: u64,
}

#[derive(Debug, Clone)]
pub struct FilterEvaluation {
    pub skipped: bool,
    pub compared_value: f64,
}

#[derive(Debug, Clone)]
pub enum BetOutcomeEvaluation {
    Ready(BetDecision),
    FilteredOut {
        decision: BetDecision,
        compared_value: f64,
    },
    BelowMinimumBalance {
        balance: u64,
        minimum_points: u64,
    },
    AmountTooSmall {
        decision: BetDecision,
    },
    InvalidEvent,
}

impl PredictionEvent {
    pub fn from_pubsub(data: &Value) -> Option<Self> {
        let event = data
            .get("data")
            .and_then(|d| d.get("event"))
            .or_else(|| data.get("event"))?;
        let event_id = event.get("id")?.as_str()?.to_string();
        let title = event
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Prediction")
            .trim()
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
            .and_then(|v| v.as_f64())
            .unwrap_or(120.0);
        let created_at = event
            .get("created_at")
            .and_then(|v| v.as_str())
            .and_then(parse_twitch_datetime)
            .unwrap_or_else(Utc::now);

        let outcomes = event
            .get("outcomes")
            .and_then(|v| v.as_array())
            .map(|arr| parse_outcomes_from_json(arr))
            .unwrap_or_default();

        Some(Self {
            event_id,
            title,
            channel_id,
            outcomes,
            status,
            prediction_window_seconds: window,
            created_at,
        })
    }

    pub fn select_choice_index(&self, settings: &BetSettings) -> Option<usize> {
        if self.outcomes.len() < 2 {
            return None;
        }
        let max_by_u64 = |f: fn(&Outcome) -> u64| -> usize {
            let mut best = 0usize;
            for (idx, item) in self.outcomes.iter().enumerate().skip(1) {
                if f(item) > f(&self.outcomes[best]) {
                    best = idx;
                }
            }
            best
        };
        let max_by_f64 = |f: fn(&Outcome) -> f64| -> usize {
            let mut best = 0usize;
            for (idx, item) in self.outcomes.iter().enumerate().skip(1) {
                if f(item) > f(&self.outcomes[best]) {
                    best = idx;
                }
            }
            best
        };
        let number_choice = |n: usize| -> usize {
            if n < self.outcomes.len() {
                n
            } else {
                0
            }
        };

        let idx = match settings.strategy {
            Strategy::MostVoted => max_by_u64(|o| o.total_users),
            Strategy::HighOdds => max_by_f64(|o| o.odds),
            Strategy::Percentage => max_by_f64(|o| o.odds_percentage),
            Strategy::SmartMoney => {
                if self.outcomes.iter().any(|o| o.top_points > 0) {
                    max_by_u64(|o| o.top_points)
                } else {
                    max_by_u64(|o| o.total_points)
                }
            }
            Strategy::Number1 => number_choice(0),
            Strategy::Number2 => number_choice(1),
            Strategy::Number3 => number_choice(2),
            Strategy::Number4 => number_choice(3),
            Strategy::Number5 => number_choice(4),
            Strategy::Number6 => number_choice(5),
            Strategy::Number7 => number_choice(6),
            Strategy::Number8 => number_choice(7),
            Strategy::Smart => {
                let difference = if self.outcomes.len() == 2 {
                    (self.outcomes[0].percentage_users - self.outcomes[1].percentage_users).abs()
                } else {
                    let mut pcts: Vec<f64> =
                        self.outcomes.iter().map(|o| o.percentage_users).collect();
                    pcts.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
                    pcts[0] - pcts.get(1).copied().unwrap_or(0.0)
                };
                if difference < settings.percentage_gap as f64 {
                    max_by_f64(|o| o.odds)
                } else {
                    max_by_u64(|o| o.total_users)
                }
            }
        };
        Some(idx)
    }

    pub fn evaluate_filter(
        &self,
        choice_idx: usize,
        filter: &FilterCondition,
    ) -> FilterEvaluation {
        let outcome = &self.outcomes[choice_idx];
        let users_total: u64 = self.outcomes.iter().map(|i| i.total_users).sum();
        let points_total: u64 = self.outcomes.iter().map(|i| i.total_points).sum();

        let actual = match filter.metric {
            FilterMetric::UsersPercentage => outcome.percentage_users,
            FilterMetric::OddsPercentage => outcome.odds_percentage,
            FilterMetric::Odds => outcome.odds,
            FilterMetric::DecisionUsers | FilterMetric::OutcomeUsers => outcome.total_users as f64,
            FilterMetric::DecisionPoints | FilterMetric::OutcomePoints => {
                outcome.total_points as f64
            }
            FilterMetric::TopPoints => outcome.top_points as f64,
            FilterMetric::TotalUsers => users_total as f64,
            FilterMetric::TotalPoints => points_total as f64,
        };

        let accepted = match filter.comparison {
            Comparison::GreaterThan => actual > filter.value,
            Comparison::LessThan => actual < filter.value,
            Comparison::GreaterOrEqual => actual >= filter.value,
            Comparison::LessOrEqual => actual <= filter.value,
        };

        FilterEvaluation {
            skipped: !accepted,
            compared_value: actual,
        }
    }

    pub fn evaluate(&self, balance: u64, settings: &BetSettings) -> BetOutcomeEvaluation {
        if balance < settings.minimum_points {
            return BetOutcomeEvaluation::BelowMinimumBalance {
                balance,
                minimum_points: settings.minimum_points,
            };
        }
        let Some(choice_idx) = self.select_choice_index(settings) else {
            return BetOutcomeEvaluation::InvalidEvent;
        };
        let Some(outcome) = self.outcomes.get(choice_idx) else {
            return BetOutcomeEvaluation::InvalidEvent;
        };

        let mut amount = ((balance as f64) * (settings.percentage as f64 / 100.0)) as u64;
        amount = amount.min(settings.max_points).min(balance);

        // Upstream stealth_mode: if our calculated wager >= top bettor's wager on that outcome,
        // place `top_points - (1..=4)` so we never appear as the #1 top predictor in chat.
        // When `top_points == 0` (nobody bet yet), `saturating_sub` yields `0` (< 10 -> skipped).
        if settings.stealth_mode && amount >= outcome.top_points {
            let reduce = rand_stealth_offset();
            amount = outcome.top_points.saturating_sub(reduce).min(balance);
        }

        let decision = BetDecision {
            choice_index: choice_idx,
            outcome_id: outcome.id.clone(),
            outcome_title: outcome.title.clone(),
            outcome_color: outcome.color.clone(),
            amount,
        };

        if let Some(filter) = &settings.filter_condition {
            let eval = self.evaluate_filter(choice_idx, filter);
            if eval.skipped {
                return BetOutcomeEvaluation::FilteredOut {
                    decision,
                    compared_value: eval.compared_value,
                };
            }
        }

        if decision.amount < 10 {
            return BetOutcomeEvaluation::AmountTooSmall { decision };
        }

        BetOutcomeEvaluation::Ready(decision)
    }

    pub fn decide(&self, balance: u64, settings: &BetSettings) -> Option<BetDecision> {
        match self.evaluate(balance, settings) {
            BetOutcomeEvaluation::Ready(d) => Some(d),
            _ => None,
        }
    }
}

/// Shared mutable state for an active prediction across `event-created`, `event-updated`,
/// `make_prediction`, and `predictions-user-v1` (`prediction-made` / `prediction-result`).
#[derive(Debug, Clone)]
pub struct EventPredictionRecord {
    pub streamer_username: String,
    pub streamer_display: String,
    pub event: PredictionEvent,
    pub settings: BetSettings,
    pub decision: Option<BetDecision>,
    pub bet_placed: bool,
    pub bet_confirmed: bool,
    pub result_string: String,
    pub result_type: Option<String>,
    pub result_gained: i64,
}

#[derive(Debug, Clone)]
pub struct ParsedPredictionResult {
    pub result_type: String,
    pub decision_amount: i64,
    pub placed: i64,
    pub won: i64,
    pub gained: i64,
    pub summary: String,
}

impl EventPredictionRecord {
    pub fn new(
        streamer_username: String,
        streamer_display: String,
        event: PredictionEvent,
        settings: BetSettings,
    ) -> Self {
        Self {
            streamer_username,
            streamer_display,
            event,
            settings,
            decision: None,
            bet_placed: false,
            bet_confirmed: false,
            result_string: String::new(),
            result_type: None,
            result_gained: 0,
        }
    }

    pub fn update_outcomes(&mut self, outcomes_json: &[Value]) {
        if self.bet_placed || self.decision.is_some() {
            return;
        }
        self.event.outcomes = parse_outcomes_from_json(outcomes_json);
    }

    pub fn effective_delay_window(&self) -> f64 {
        let w = self.event.prediction_window_seconds;
        match self.settings.delay_mode {
            DelayMode::FromStart => self.settings.delay.min(w),
            DelayMode::FromEnd => (w - self.settings.delay).max(0.0),
            DelayMode::Percentage => w * self.settings.delay.clamp(0.0, 1.0),
        }
    }

    pub fn closing_bet_after(&self, now: DateTime<Utc>) -> f64 {
        let elapsed = (now - self.event.created_at).num_milliseconds() as f64 / 1000.0;
        let adjusted_window = self.effective_delay_window();
        round2(adjusted_window - elapsed.max(0.0))
    }

    pub fn display_str(&self, less: bool) -> String {
        if less {
            format!(
                "EventPrediction: {} - {}",
                self.streamer_display, self.event.title
            )
        } else {
            format!(
                "EventPrediction(event_id={}, streamer={}, title={})",
                self.event.event_id, self.streamer_display, self.event.title
            )
        }
    }

    pub fn parse_result(&mut self, result_val: &Value) -> ParsedPredictionResult {
        let result_type = result_val
            .get("type")
            .or_else(|| result_val.get("resultType"))
            .and_then(|v| v.as_str())
            .unwrap_or("LOSE")
            .to_uppercase();

        let placed_amt = self.decision.as_ref().map(|d| d.amount as i64).unwrap_or(0);
        let placed = if result_type != "REFUND" {
            placed_amt
        } else {
            0
        };
        let raw_won = result_val
            .get("points_won")
            .or_else(|| result_val.get("pointsWon"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let won = if raw_won > 0 || result_type == "REFUND" {
            raw_won
        } else {
            0
        };
        let gained = if result_type != "REFUND" {
            won - placed
        } else {
            0
        };
        let prefix = if gained >= 0 { "+" } else { "" };
        let action = match result_type.as_str() {
            "LOSE" => "Lost",
            "REFUND" => "Refunded",
            _ => "Gained",
        };
        let summary = format!(
            "{result_type}, {action}: {prefix}{}",
            millify_signed(gained)
        );
        self.result_string = summary.clone();
        self.result_type = Some(result_type.clone());
        self.result_gained = gained;

        ParsedPredictionResult {
            result_type,
            decision_amount: placed_amt,
            placed,
            won,
            gained,
            summary,
        }
    }

    pub fn print_recap(&self, less: bool) -> String {
        let total_users: u64 = self.event.outcomes.iter().map(|o| o.total_users).sum();
        let total_points: u64 = self.event.outcomes.iter().map(|o| o.total_points).sum();
        let mut out = format!(
            "{}\n\t\tBet(total_users={}, total_points={}, decision={:?})",
            self.display_str(less),
            millify_u64(total_users),
            millify_u64(total_points),
            self.decision
        );
        for (idx, outcome) in self.event.outcomes.iter().enumerate() {
            out.push_str(&format!("\n\t\tOutcome {}({})", idx + 1, outcome.summary()));
        }
        out.push_str(&format!("\n\t\tResult: {}", self.result_string));
        out
    }
}

fn rand_stealth_offset() -> u64 {
    use rand::Rng;
    rand::thread_rng().gen_range(1..=4)
}

pub fn log_decision(event: &PredictionEvent, decision: &BetDecision) {
    info!(
        "🎲 Prediction '{}': betting {} points on '{}' ({})",
        event.title, decision.amount, decision.outcome_title, decision.outcome_id
    );
}
