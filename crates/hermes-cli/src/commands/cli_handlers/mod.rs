//! CLI subcommand handlers (dispatched from main.rs / dispatch.rs).

mod acp;
mod auth;
mod chat;
mod claw;
mod contribute;
mod insights;
mod interest;
mod lifecycle;
mod mcp;
mod mcp_profile;
mod media;
mod media_config;
mod meeting;
mod memory;
mod pairing;
mod plugins;
mod server;
mod server_config;
mod sessions;
#[cfg(feature = "talk")]
mod talk;
mod whatsapp;

pub use acp::handle_cli_acp;
pub use auth::{handle_cli_login, handle_cli_logout};
pub use chat::handle_cli_chat;
pub use claw::handle_cli_claw;
pub use contribute::handle_cli_contribute;
pub use insights::handle_cli_insights;
pub use interest::handle_cli_interest;
pub use lifecycle::{handle_cli_backup, handle_cli_import, handle_cli_version};
pub use mcp::handle_cli_mcp;
pub use media::handle_cli_media;
pub use meeting::handle_cli_meeting;
pub use memory::handle_cli_memory;
pub use pairing::handle_cli_pairing;
pub use plugins::{handle_cli_external_plugin_subcommand, handle_cli_plugins};
pub use server::handle_cli_server;
pub use sessions::handle_cli_sessions;
#[cfg(feature = "talk")]
pub use talk::handle_cli_talk;
pub use whatsapp::handle_cli_whatsapp;

pub(crate) use plugins::{discover_plugin_surface, render_plugin_surface_table};
pub(crate) use whatsapp::whatsapp_cloud_setup_impl;

#[cfg(test)]
pub(crate) use acp::{ACP_MULTIMODAL_PREFIX, acp_history_to_messages};
#[cfg(test)]
pub(crate) use chat::query_mode_tools_enabled;
#[cfg(test)]
pub(crate) use chat::{apply_cli_chat_runtime_env, resolve_cli_chat_provider_model};
#[cfg(test)]
pub(crate) use mcp_profile::{remove_sentrux_mcp_profile, upsert_sentrux_mcp_profile};
