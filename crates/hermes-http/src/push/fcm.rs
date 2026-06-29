use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FcmConfig {
    pub project_id: String,
}

pub struct FcmSender {
    config: FcmConfig,
}

impl FcmSender {
    pub fn new(config: FcmConfig) -> Self {
        Self { config }
    }

    pub async fn send(&self, device_token: &str, title: &str, body: &str) -> Result<(), String> {
        let _ = (&self.config, device_token, title, body);
        Ok(())
    }
}
