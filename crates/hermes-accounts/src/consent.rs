use chrono::{DateTime, Utc};
use hermes_tasks::UserId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentRecord {
    pub user_id: UserId,
    pub vertical_id: String,
    pub provider_ids: Vec<String>,
    pub granted_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

pub struct ConsentStore;

impl ConsentStore {
    pub fn is_granted(&self, _user_id: &UserId, _provider_id: &str) -> bool {
        false
    }
}
