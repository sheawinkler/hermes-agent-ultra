//! Platform adapter modules.
//!
//! Each platform is feature-gated so that only the adapters you need
//! are compiled into the binary.

#[cfg(feature = "telegram")]
pub mod telegram;

#[cfg(feature = "discord")]
pub mod discord;

#[cfg(feature = "slack")]
pub mod slack;

#[cfg(feature = "whatsapp")]
pub mod whatsapp;

#[cfg(feature = "signal")]
pub mod signal;

#[cfg(feature = "matrix")]
pub mod matrix;

#[cfg(feature = "mattermost")]
pub mod mattermost;

#[cfg(feature = "dingtalk")]
pub mod dingtalk;

#[cfg(feature = "feishu")]
pub mod feishu;

#[cfg(feature = "wecom")]
pub mod wecom;

#[cfg(feature = "weixin")]
pub mod weixin;

#[cfg(feature = "bluebubbles")]
pub mod bluebubbles;

#[cfg(feature = "email")]
pub mod email;

#[cfg(feature = "sms")]
pub mod sms;

#[cfg(feature = "homeassistant")]
pub mod homeassistant;

#[cfg(feature = "api-server")]
pub mod api_server;

#[cfg(feature = "webhook")]
pub mod webhook;

pub mod helpers;