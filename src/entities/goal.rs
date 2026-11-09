use serde_json::Value;

#[derive(Debug, Clone)]
pub struct CommunityGoal {
    pub goal_id: String,
    pub title: String,
    pub is_in_stock: bool,
    pub points_contributed: u64,
    pub amount_needed: u64,
    pub per_stream_user_maximum_contribution: u64,
    pub status: String,
}

impl CommunityGoal {
    pub fn amount_left(&self) -> u64 {
        self.amount_needed.saturating_sub(self.points_contributed)
    }

    pub fn from_gql(val: &Value) -> Option<Self> {
        let goal_id = val.get("id")?.as_str()?.to_string();
        let title = val
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Goal")
            .to_string();
        let is_in_stock = val
            .get("isInStock")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let points_contributed = val
            .get("pointsContributed")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let amount_needed = val
            .get("amountNeeded")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let per_stream_user_maximum_contribution = val
            .get("perStreamUserMaximumContribution")
            .and_then(|v| v.as_u64())
            .unwrap_or(u64::MAX);
        let status = val
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        Some(Self {
            goal_id,
            title,
            is_in_stock,
            points_contributed,
            amount_needed,
            per_stream_user_maximum_contribution,
            status,
        })
    }

    pub fn from_pubsub(val: &Value) -> Option<Self> {
        let goal_id = val.get("id")?.as_str()?.to_string();
        let title = val
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Goal")
            .to_string();
        let is_in_stock = val
            .get("is_in_stock")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let points_contributed = val
            .get("points_contributed")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let amount_needed = val
            .get("goal_amount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let per_stream_user_maximum_contribution = val
            .get("per_stream_maximum_user_contribution")
            .and_then(|v| v.as_u64())
            .unwrap_or(u64::MAX);
        let status = val
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        Some(Self {
            goal_id,
            title,
            is_in_stock,
            points_contributed,
            amount_needed,
            per_stream_user_maximum_contribution,
            status,
        })
    }
}
