use serde::{Deserialize, Serialize};

use hermes_tasks::types::UserId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRule {
    pub kind: String,
    pub threshold: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watchlist {
    pub user_id: UserId,
    pub symbols: Vec<String>,
    pub alert_rules: Vec<AlertRule>,
}

impl Watchlist {
    pub fn new(user_id: UserId) -> Self {
        Self {
            user_id,
            symbols: Vec::new(),
            alert_rules: Vec::new(),
        }
    }
}
