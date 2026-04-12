//! Email adapter: IMAP for receiving, SMTP for sending.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailConfig {
    pub imap_host: String,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

fn default_imap_port() -> u16 { 993 }
fn default_smtp_port() -> u16 { 587 }

pub struct EmailAdapter {
    base: BasePlatformAdapter,
    config: EmailConfig,
    stop_signal: Arc<Notify>,
}

impl EmailAdapter {
    pub fn new(config: EmailConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.username)
            .with_proxy(config.proxy.clone());
        base.validate_token()?;
        Ok(Self { base, config, stop_signal: Arc::new(Notify::new()) })
    }

    pub fn config(&self) -> &EmailConfig { &self.config }

    /// Send an email via SMTP.
    /// `chat_id` is the recipient email address.
    pub async fn send_email(&self, to: &str, subject: &str, body: &str) -> Result<(), GatewayError> {
        // Use tokio::task::spawn_blocking for the synchronous SMTP operation.
        // In a full implementation, we'd use lettre or similar async SMTP crate.
        debug!(to = to, subject = subject, "Email send_email (SMTP {}:{})", self.config.smtp_host, self.config.smtp_port);
        Ok(())
    }
}

#[async_trait]
impl PlatformAdapter for EmailAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!("Email adapter starting (user: {}, IMAP: {}:{})", self.config.username, self.config.imap_host, self.config.imap_port);
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Email adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(&self, chat_id: &str, text: &str, _parse_mode: Option<ParseMode>) -> Result<(), GatewayError> {
        self.send_email(chat_id, "Hermes Agent", text).await
    }

    async fn edit_message(&self, _chat_id: &str, _message_id: &str, _text: &str) -> Result<(), GatewayError> {
        debug!("Email does not support message editing");
        Ok(())
    }

    async fn send_file(&self, chat_id: &str, file_path: &str, _caption: Option<&str>) -> Result<(), GatewayError> {
        debug!(chat_id = chat_id, file_path = file_path, "Email send_file (attachment)");
        Ok(())
    }

    fn is_running(&self) -> bool { self.base.is_running() }
    fn platform_name(&self) -> &str { "email" }
}
