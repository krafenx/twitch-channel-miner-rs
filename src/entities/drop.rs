use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::Value;

pub fn parse_twitch_datetime(raw: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&Utc));
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S%.fZ", "%Y-%m-%dT%H:%M:%SZ"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct DropItem {
    pub id: String,
    pub name: String,
    pub benefit: String,
    pub minutes_required: u64,
    pub has_preconditions_met: Option<bool>,
    pub current_minutes_watched: u64,
    pub drop_instance_id: Option<String>,
    pub is_claimed: bool,
    pub is_claimable: bool,
    pub percentage_progress: u64,
    pub start_at: Option<DateTime<Utc>>,
    pub end_at: Option<DateTime<Utc>>,
    pub dt_match: bool,
    pub is_printable: bool,
}

impl DropItem {
    pub fn from_gql(val: &Value) -> Option<Self> {
        let id = val.get("id")?.as_str()?.to_string();
        let name = val
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Drop")
            .to_string();
        let minutes_required = val
            .get("requiredMinutesWatched")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let mut benefits = Vec::new();
        if let Some(edges) = val.get("benefitEdges").and_then(|v| v.as_array()) {
            for edge in edges {
                if let Some(bname) = edge
                    .get("benefit")
                    .and_then(|b| b.get("name"))
                    .and_then(|v| v.as_str())
                {
                    if !benefits.iter().any(|x| x == bname) {
                        benefits.push(bname.to_string());
                    }
                }
            }
        }
        let benefit = benefits.join(", ");

        let start_at = val
            .get("startAt")
            .and_then(|v| v.as_str())
            .and_then(parse_twitch_datetime);
        let end_at = val
            .get("endAt")
            .and_then(|v| v.as_str())
            .and_then(parse_twitch_datetime);
        let now = Utc::now();
        let dt_match = match (start_at, end_at) {
            (Some(s), Some(e)) => s <= now && now <= e,
            _ => true,
        };

        let mut item = Self {
            id,
            name,
            benefit,
            minutes_required,
            has_preconditions_met: None,
            current_minutes_watched: 0,
            drop_instance_id: None,
            is_claimed: false,
            is_claimable: false,
            percentage_progress: 0,
            start_at,
            end_at,
            dt_match,
            is_printable: false,
        };
        if let Some(self_obj) = val.get("self") {
            if self_obj.is_object() {
                item.update_progress(self_obj);
            }
        }
        Some(item)
    }

    pub fn update_progress(&mut self, progress: &Value) {
        self.has_preconditions_met = progress.get("hasPreconditionsMet").and_then(|v| v.as_bool());
        let new_minutes = progress
            .get("currentMinutesWatched")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.current_minutes_watched);

        let updated_pct = if self.minutes_required > 0 {
            ((new_minutes * 100) / self.minutes_required).min(100)
        } else {
            0
        };
        let is_quarter = updated_pct > 0 && updated_pct % 25 == 0;

        self.is_printable = new_minutes > self.current_minutes_watched
            && ((updated_pct > self.percentage_progress
                && is_quarter
                && self.current_minutes_watched != 0)
                || (new_minutes == 1 && self.current_minutes_watched == 0));

        self.current_minutes_watched = new_minutes;
        self.drop_instance_id = progress
            .get("dropInstanceID")
            .and_then(|v| v.as_str())
            .map(String::from);
        self.is_claimed = progress
            .get("isClaimed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let explicit_claimable = progress
            .get("isClaimable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        self.is_claimable =
            explicit_claimable || (!self.is_claimed && self.drop_instance_id.is_some());
        self.percentage_progress = updated_pct;
    }

    pub fn display_str(&self, less: bool) -> String {
        if less {
            format!(
                "{} ({}) {}/{} ({}%)",
                self.name,
                self.benefit,
                self.current_minutes_watched,
                self.minutes_required,
                self.percentage_progress
            )
        } else {
            format!(
                "Drop(id={}, name={}, benefit={}, minutes_required={}, has_preconditions_met={:?}, current_minutes_watched={}, percentage_progress={}%, drop_instance_id={:?}, is_claimed={})",
                self.id,
                self.name,
                self.benefit,
                self.minutes_required,
                self.has_preconditions_met,
                self.current_minutes_watched,
                self.percentage_progress,
                self.drop_instance_id,
                self.is_claimed
            )
        }
    }

    pub fn progress_bar(&self) -> String {
        let progress = (self.percentage_progress.min(100) / 2) as usize;
        let remaining = 50usize.saturating_sub(progress);
        format!(
            "|{}{}|\t{}% [{}/{}]",
            "█".repeat(progress),
            " ".repeat(remaining),
            self.percentage_progress,
            self.current_minutes_watched,
            self.minutes_required
        )
    }
}

#[derive(Debug, Clone)]
pub struct Campaign {
    pub id: String,
    pub game_id: Option<String>,
    pub game_name: Option<String>,
    pub game_display_name: Option<String>,
    pub name: String,
    pub status: String,
    pub in_inventory: bool,
    pub channels: Vec<String>,
    pub start_at: Option<DateTime<Utc>>,
    pub end_at: Option<DateTime<Utc>>,
    pub dt_match: bool,
    pub drops: Vec<DropItem>,
}

impl Campaign {
    pub fn from_gql(val: &Value) -> Option<Self> {
        let id = val.get("id")?.as_str()?.to_string();
        let name = val
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Campaign")
            .to_string();
        let status = val
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("ACTIVE")
            .to_string();
        let game_obj = val.get("game");
        let game_id = game_obj
            .and_then(|g| g.get("id"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let game_name = game_obj
            .and_then(|g| g.get("name"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let game_display_name = game_obj
            .and_then(|g| g.get("displayName"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| game_name.clone());

        let channels = val
            .get("allow")
            .and_then(|a| a.get("channels"))
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|ch| ch.get("id").and_then(|v| v.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let start_at = val
            .get("startAt")
            .and_then(|v| v.as_str())
            .and_then(parse_twitch_datetime);
        let end_at = val
            .get("endAt")
            .and_then(|v| v.as_str())
            .and_then(parse_twitch_datetime);
        let now = Utc::now();
        let dt_match = match (start_at, end_at) {
            (Some(s), Some(e)) => s <= now && now <= e,
            _ => true,
        };

        let drops = val
            .get("timeBasedDrops")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(DropItem::from_gql).collect())
            .unwrap_or_default();

        Some(Self {
            id,
            game_id,
            game_name,
            game_display_name,
            name,
            status,
            in_inventory: false,
            channels,
            start_at,
            end_at,
            dt_match,
            drops,
        })
    }

    pub fn clear_drops(&mut self) {
        let now = Utc::now();
        for drop in &mut self.drops {
            if let (Some(s), Some(e)) = (drop.start_at, drop.end_at) {
                drop.dt_match = s <= now && now <= e;
            }
        }
        self.drops.retain(|d| d.dt_match && !d.is_claimed);
    }

    pub fn display_str(&self, less: bool) -> String {
        let gname = self.game_display_name.as_deref().unwrap_or("Unknown");
        if less {
            format!(
                "{}, Game: {} - Drops: {} pcs. - In inventory: {}",
                self.name,
                gname,
                self.drops.len(),
                self.in_inventory
            )
        } else {
            format!(
                "Campaign(id={}, name={}, game={}, in_inventory={})",
                self.id, self.name, gname, self.in_inventory
            )
        }
    }
}
