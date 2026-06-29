use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApnsConfig {
    pub team_id: String,
    pub key_id: String,
    pub bundle_id: String,
}

pub struct ApnsSender {
    config: ApnsConfig,
}

impl ApnsSender {
    pub fn new(config: ApnsConfig) -> Self {
        Self { config }
    }

    pub async fn send(&self, device_token: &str, title: &str, body: &str) -> Result<(), String> {
        let _ = (&self.config, device_token, title, body);
        Ok(())
    }
}
