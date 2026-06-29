use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ids::{DeviceId, UserId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceRole {
    Primary,
    Secondary,
    Mobile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCapabilities {
    pub can_run_agent: bool,
    pub can_approve: bool,
    pub push_enabled: bool,
}

impl Default for DeviceCapabilities {
    fn default() -> Self {
        Self {
            can_run_agent: true,
            can_approve: true,
            push_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: DeviceId,
    pub owner_user_id: UserId,
    pub name: String,
    pub platform: String,
    pub role: DeviceRole,
    pub capabilities: DeviceCapabilities,
    pub last_seen_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

impl Device {
    pub fn new(
        owner_user_id: UserId,
        name: impl Into<String>,
        platform: impl Into<String>,
        role: DeviceRole,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: DeviceId::new(),
            owner_user_id,
            name: name.into(),
            platform: platform.into(),
            role,
            capabilities: DeviceCapabilities::default(),
            last_seen_at: now,
            created_at: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn w14_device_types_serde_roundtrip() {
        let owner = UserId::new();
        let device = Device::new(owner, "desk", "windows", DeviceRole::Primary);
        let json = serde_json::to_string(&device).unwrap();
        let back: Device = serde_json::from_str(&json).unwrap();
        assert_eq!(back.role, DeviceRole::Primary);
        assert_eq!(back.name, "desk");
    }
}
