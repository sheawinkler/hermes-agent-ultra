//! # hermes-gateway
//!
//! Message gateway and platform adapters for the Hermes Agent system.
//!
//! This crate implements the message gateway system (Requirement 7), providing:
//! - A unified `Gateway` orchestrator for all platform adapters
//! - Session management with configurable reset policies
//! - Streaming output management for progressive message updates
//! - Media caching for images, audio, and documents
//! - SSRF protection for outbound URL validation
//! - DM pairing mechanism for unauthorized user handling
//! - Platform-specific adapters behind feature flags

pub mod adapter;
pub mod background;
pub mod commands;
pub mod channel_directory;
pub mod delivery;
pub mod dm;
pub mod format;
pub mod gateway;
pub mod hooks;
pub mod markdown_split;
pub mod media;
pub mod mirror;
pub mod pairing;
pub mod session;
pub mod sticker_cache;
pub mod ssrf;
pub mod stream;
pub mod voice;
pub mod platforms;

// Re-export core types from hermes-core
pub use hermes_core::errors::GatewayError;
pub use hermes_core::traits::{ParseMode, PlatformAdapter};
pub use hermes_core::types::Message;

// Re-export gateway orchestrator
pub use gateway::Gateway;

// Re-export session management
pub use session::{Session, SessionManager};

// Re-export stream management
pub use stream::{StreamHandle, StreamManager};

// Re-export media caching
pub use media::MediaCache;

// Re-export SSRF protection
pub use ssrf::{is_safe_url, validate_url};

// Re-export DM management
pub use dm::{DmDecision, DmManager};
pub use pairing::{PairingManager, PairingState};
pub use mirror::MirrorManager;
pub use sticker_cache::{StickerCache, StickerMeta};
pub use delivery::{DeliveryItem, DeliveryQueue};
pub use channel_directory::{ChannelDirectory, ChannelEntry};

// Re-export adapter base
pub use adapter::BasePlatformAdapter;

// Re-export platform adapters behind feature flags

#[cfg(feature = "telegram")]
pub use platforms::telegram::TelegramAdapter;

#[cfg(feature = "discord")]
pub use platforms::discord::DiscordAdapter;

#[cfg(feature = "slack")]
pub use platforms::slack::SlackAdapter;

#[cfg(feature = "whatsapp")]
pub use platforms::whatsapp::WhatsAppAdapter;

#[cfg(feature = "signal")]
pub use platforms::signal::SignalAdapter;

#[cfg(feature = "matrix")]
pub use platforms::matrix::MatrixAdapter;

#[cfg(feature = "mattermost")]
pub use platforms::mattermost::MattermostAdapter;

#[cfg(feature = "dingtalk")]
pub use platforms::dingtalk::DingTalkAdapter;

#[cfg(feature = "feishu")]
pub use platforms::feishu::FeishuAdapter;

#[cfg(feature = "wecom")]
pub use platforms::wecom::WeComAdapter;

#[cfg(feature = "weixin")]
pub use platforms::weixin::WeChatAdapter;

#[cfg(feature = "bluebubbles")]
pub use platforms::bluebubbles::BlueBubblesAdapter;

#[cfg(feature = "email")]
pub use platforms::email::EmailAdapter;

#[cfg(feature = "sms")]
pub use platforms::sms::SmsAdapter;

#[cfg(feature = "homeassistant")]
pub use platforms::homeassistant::HomeAssistantAdapter;