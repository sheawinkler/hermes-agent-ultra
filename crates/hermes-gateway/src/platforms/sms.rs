//! SMS gateway adapter (Twilio API).

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

const TWILIO_API_BASE: &str = "https://api.twilio.com/2010-04-01";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsConfig {
    pub provider: String,
    pub account_sid: String,
    pub auth_token: String,
    pub from_number: String,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

pub struct SmsAdapter {
    base: BasePlatformAdapter,
    config: SmsConfig,
    client: Client,
    stop_signal: Arc<Notify>,
}

impl SmsAdapter {
    pub fn new(config: SmsConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.account_sid)
            .with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self { base, config, client, stop_signal: Arc::new(Notify::new()) })
    }

    pub fn config(&self) -> &SmsConfig { &self.config }

    /// Send an SMS via Twilio API.
    pub async fn send_sms(&self, to: &str, body: &str) -> Result<(), GatewayError> {
        let url = format!(
            "{}/Accounts/{}/Messages.json",
            TWILIO_API_BASE, self.config.account_sid
        );

        let params = [
            ("To", to),
            ("From", &self.config.from_number),
            ("Body", body),
        ];

        let resp = self.client.post(&url)
            .basic_auth(&self.config.account_sid, Some(&self.config.auth_token))
            .form(&params)
            .send().await
            .map_err(|e| GatewayError::SendFailed(format!("Twilio send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("Twilio API error: {}", text)));
        }
        Ok(())
    }
}

#[async_trait]
impl PlatformAdapter for SmsAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!("SMS adapter starting (provider: {}, from: {})", self.config.provider, self.config.from_number);
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("SMS adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(&self, chat_id: &str, text: &str, _parse_mode: Option<ParseMode>) -> Result<(), GatewayError> {
        self.send_sms(chat_id, text).await
    }

    async fn edit_message(&self, _chat_id: &str, _message_id: &str, _text: &str) -> Result<(), GatewayError> {
        debug!("SMS does not support message editing");
        Ok(())
    }

    async fn send_file(&self, chat_id: &str, file_path: &str, _caption: Option<&str>) -> Result<(), GatewayError> {
        debug!(chat_id = chat_id, file_path = file_path, "SMS send_file (MMS)");
        Ok(())
    }

    fn is_running(&self) -> bool { self.base.is_running() }
    fn platform_name(&self) -> &str { "sms" }
}
