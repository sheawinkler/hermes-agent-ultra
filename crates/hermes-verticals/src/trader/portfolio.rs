use serde::{Deserialize, Serialize};

use hermes_tasks::types::UserId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol: String,
    pub quantity: f64,
    pub avg_cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Portfolio {
    pub user_id: UserId,
    pub name: String,
    pub positions: Vec<Position>,
    pub cash: f64,
    pub currency: String,
}

impl Portfolio {
    pub fn new(user_id: UserId, name: impl Into<String>) -> Self {
        Self {
            user_id,
            name: name.into(),
            positions: Vec::new(),
            cash: 0.0,
            currency: "CNY".to_string(),
        }
    }
}
