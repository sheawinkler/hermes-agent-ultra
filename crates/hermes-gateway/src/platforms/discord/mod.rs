//! Discord Bot API adapter (REST outbound + Gateway WebSocket inbound).

mod allowed_mentions;
mod auth;
mod channel_context;
pub mod command_sync;
mod config;
mod dedup;
mod filter;
mod gateway_loop;
mod media;
mod parse;
mod rest;
mod session;
pub mod slash;
pub mod stream_finalize;
mod text_batch;
mod threads;
mod types;

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{describe_secret, BasePlatformAdapter};
use crate::gateway::IncomingMessage;

pub use channel_context::{resolve_channel_prompt, resolve_channel_skills, ChannelSkillBinding};
pub use config::{
    default_intents, AllowBotsMode, ChannelIdSet, CommandSyncPolicy, DiscordConfig, ReplyToMode,
    DISCORD_API_BASE, MAX_MESSAGE_LENGTH,
};
pub use allowed_mentions::{parse_bool_like, DiscordAllowedMentions};
pub use dedup::MessageDedup;
pub use filter::{should_accept_message, DiscordInboundConfig};
pub use parse::{
    format_slash_command_text, interaction_to_incoming, parse_attachments, parse_autocomplete_interaction,
    parse_dispatch, parse_interaction_create, parse_message_create, parse_message_create_raw,
    parse_message_update, parse_reaction_event, parse_voice_state_update, raw_to_incoming,
    AutocompleteInteraction, DispatchEvent, DiscordAttachment, IncomingDiscordMessage,
    InteractionData, InteractionOption, MessageUpdateEvent, RawDiscordMessage, ReactionEvent,
    VoiceState, INTERACTION_TYPE_APPLICATION_COMMAND, INTERACTION_TYPE_AUTOCOMPLETE,
};
pub use rest::{encode_emoji, is_forum_channel_type, outbound_upload_name, split_message};
pub use types::basic_slash_commands;
pub use gateway_loop::DiscordInner;
pub use session::{
    opcodes, GatewayAction, GatewayPayload, GatewaySession, IdentifyData, IdentifyProperties,
    ResumeData,
};
pub use text_batch::deliver_inbounds;
pub use types::{
    DiscordEmbed, DiscordMessage, DiscordThread, DiscordUser, EmbedAuthor, EmbedField,
    EmbedFooter, EmbedMedia, SlashCommand, SlashCommandChoice, SlashCommandOption,
};

pub use gateway_loop::{fetch_gateway_url, fetch_gateway_url_at};
pub use media::download_attachment_bytes;
use gateway_loop::{
    build_heartbeat_payload, build_identify_payload, build_resume_payload, gateway_loop,
};
use gateway_loop::DiscordInner as Inner;

/// Discord Bot API platform adapter.
pub struct DiscordAdapter {
    inner: Arc<Inner>,
    stop_signal: Arc<tokio::sync::Notify>,
    run_task: RwLock<Option<tokio::task::JoinHandle<()>>>,
}

impl DiscordAdapter {
    pub fn new(config: DiscordConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.token).with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        let inner = Arc::new(Inner {
            config,
            client,
            base,
            inbound_tx: RwLock::new(None),
            bot_user_id: RwLock::new(None),
            dedup: RwLock::new(MessageDedup::new()),
            thread_tracker: Arc::new(threads::ThreadParticipationTracker::load()),
            stop: tokio::sync::Notify::new(),
            deferred_interactions: RwLock::new(std::collections::HashSet::new()),
            inbound_text_pending: RwLock::new(std::collections::HashMap::new()),
            inbound_text_tasks: RwLock::new(std::collections::HashMap::new()),
            channel_topic_cache: RwLock::new(std::collections::HashMap::new()),
        });
        Ok(Self {
            inner,
            stop_signal: Arc::new(tokio::sync::Notify::new()),
            run_task: RwLock::new(None),
        })
    }

    pub fn config(&self) -> &DiscordConfig {
        &self.inner.config
    }

    /// Shared inner state (tests, gateway registration).
    pub fn inner(&self) -> &Arc<Inner> {
        &self.inner
    }

    /// Backfill prior channel messages into an empty session (P2-8).
    pub async fn backfill_session_if_empty(
        &self,
        session_manager: &crate::session::SessionManager,
        session_key: &str,
        channel_id: &str,
    ) -> Result<(), GatewayError> {
        let limit = self.inner.config.history_backfill_limit;
        if limit == 0 {
            return Ok(());
        }
        let existing = session_manager.get_messages(session_key).await;
        if !existing.is_empty() {
            return Ok(());
        }
        let rows = self
            .inner
            .fetch_channel_messages(channel_id, limit)
            .await?;
        for (_id, content) in rows {
            session_manager
                .add_message(session_key, hermes_core::types::Message::user(&content))
                .await;
        }
        Ok(())
    }

    pub async fn send_multiple_image_embeds(
        &self,
        channel_id: &str,
        image_urls: &[&str],
        caption: Option<&str>,
    ) -> Result<Vec<String>, GatewayError> {
        self.inner
            .send_multiple_image_embeds(channel_id, image_urls, caption)
            .await
    }

    pub async fn set_inbound_sender(&self, tx: mpsc::Sender<IncomingMessage>) {
        *self.inner.inbound_tx.write().await = Some(tx);
    }

    pub fn build_identify_payload(&self) -> GatewayPayload {
        build_identify_payload(&self.inner.config)
    }

    pub fn build_heartbeat_payload(sequence: Option<u64>) -> GatewayPayload {
        build_heartbeat_payload(sequence)
    }

    pub fn build_resume_payload(&self, session_id: &str, seq: u64) -> GatewayPayload {
        build_resume_payload(&self.inner.config, session_id, seq)
    }

    pub async fn send_text(
        &self,
        channel_id: &str,
        content: &str,
    ) -> Result<Vec<String>, GatewayError> {
        self.inner.send_text(channel_id, content).await
    }

    pub async fn send_text_with_reply(
        &self,
        channel_id: &str,
        content: &str,
        reply_to_message_id: Option<&str>,
    ) -> Result<Vec<String>, GatewayError> {
        self.inner
            .send_text_with_reply(channel_id, content, reply_to_message_id)
            .await
    }

    pub async fn edit_text(
        &self,
        channel_id: &str,
        message_id: &str,
        content: &str,
    ) -> Result<(), GatewayError> {
        self.inner.edit_text(channel_id, message_id, content).await
    }

    pub async fn delete_message(
        &self,
        channel_id: &str,
        message_id: &str,
    ) -> Result<bool, GatewayError> {
        self.inner.delete_message(channel_id, message_id).await
    }

    pub async fn send_embed(
        &self,
        channel_id: &str,
        content: Option<&str>,
        embeds: &[DiscordEmbed],
    ) -> Result<String, GatewayError> {
        self.inner.send_embed(channel_id, content, embeds).await
    }

    pub async fn upload_file(
        &self,
        channel_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<String, GatewayError> {
        self.inner.upload_file(channel_id, file_path, caption).await
    }

    pub async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        self.inner.add_reaction(channel_id, message_id, emoji).await
    }

    pub async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        self.inner
            .remove_reaction(channel_id, message_id, emoji)
            .await
    }

    pub async fn create_thread(
        &self,
        channel_id: &str,
        message_id: &str,
        name: &str,
        auto_archive_duration: Option<u32>,
    ) -> Result<DiscordThread, GatewayError> {
        self.inner
            .create_thread(channel_id, message_id, name, auto_archive_duration)
            .await
    }

    pub async fn register_slash_commands(&self, commands: &[SlashCommand]) -> Result<(), GatewayError> {
        self.inner.register_slash_commands(commands).await
    }

    pub async fn register_guild_slash_commands(
        &self,
        guild_id: &str,
        commands: &[SlashCommand],
    ) -> Result<(), GatewayError> {
        self.inner
            .register_guild_slash_commands(guild_id, commands)
            .await
    }

    pub async fn respond_to_interaction(
        &self,
        interaction_id: &str,
        interaction_token: &str,
        content: &str,
    ) -> Result<(), GatewayError> {
        self.inner
            .respond_to_interaction(interaction_id, interaction_token, content)
            .await
    }

    pub async fn defer_interaction(
        &self,
        interaction_id: &str,
        interaction_token: &str,
    ) -> Result<(), GatewayError> {
        self.inner
            .defer_interaction(interaction_id, interaction_token)
            .await
    }

    pub fn parse_message_create(data: &serde_json::Value) -> Option<IncomingDiscordMessage> {
        parse_message_create(data)
    }

    pub fn parse_message_update(data: &serde_json::Value) -> Option<MessageUpdateEvent> {
        parse_message_update(data)
    }

    pub fn parse_interaction_create(data: &serde_json::Value) -> Option<InteractionData> {
        parse_interaction_create(data)
    }

    pub fn parse_reaction_event(data: &serde_json::Value) -> Option<ReactionEvent> {
        parse_reaction_event(data)
    }

    pub fn parse_voice_state_update(data: &serde_json::Value) -> Option<VoiceState> {
        parse_voice_state_update(data)
    }

    pub fn parse_dispatch(event_name: &str, data: &serde_json::Value) -> Option<DispatchEvent> {
        parse_dispatch(event_name, data)
    }
}

#[async_trait]
impl PlatformAdapter for DiscordAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        if self.run_task.read().await.is_some() && self.inner.base.is_running() {
            debug!("Discord adapter already running; skipping duplicate start");
            return Ok(());
        }
        info!(
            "Discord adapter starting (token: {})",
            describe_secret(&self.inner.config.token)
        );
        self.inner.base.mark_running();
        if self.inner.config.slash_commands_enabled
            && self.inner.config.application_id.is_some()
            && !matches!(
                self.inner.config.command_sync_policy,
                config::CommandSyncPolicy::Off
            )
        {
            let inner = self.inner.clone();
            let policy = self.inner.config.command_sync_policy;
            tokio::spawn(async move {
                let commands = slash::build_desired_slash_commands().await;
                let result = if let Some(guild_id) = inner.config.slash_guild_id.as_deref() {
                    inner
                        .register_guild_slash_commands(guild_id, &commands)
                        .await
                } else {
                    match policy {
                        config::CommandSyncPolicy::Safe => {
                            inner.safe_sync_slash_commands(&commands).await.map(|_| ())
                        }
                        config::CommandSyncPolicy::Bulk | config::CommandSyncPolicy::Off => {
                            inner.register_slash_commands(&commands).await
                        }
                    }
                };
                if let Err(err) = result {
                    warn!("Discord slash command registration failed: {err}");
                }
            });
        }
        let inner = self.inner.clone();
        let handle = tokio::spawn(async move {
            gateway_loop(inner).await;
        });
        *self.run_task.write().await = Some(handle);
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Discord adapter stopping");
        self.inner.base.mark_stopped();
        self.inner.stop.notify_waiters();
        self.stop_signal.notify_one();
        if let Some(task) = self.run_task.write().await.take() {
            task.abort();
        }
        Ok(())
    }

    async fn send_message_with_id(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<Option<String>, GatewayError> {
        self.send_message_replying(chat_id, text, parse_mode, None)
            .await
    }

    async fn send_message_replying(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
        reply_to_message_id: Option<&str>,
    ) -> Result<Option<String>, GatewayError> {
        let formatted = Self::format_outbound(text, parse_mode);
        let ids = self
            .send_text_with_reply(chat_id, &formatted, reply_to_message_id)
            .await?;
        Ok(ids.into_iter().next())
    }

    async fn edit_message(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        let formatted = Self::format_outbound(text, None);
        self.edit_text(chat_id, message_id, &formatted).await
    }

    async fn delete_message(
        &self,
        chat_id: &str,
        message_id: &str,
    ) -> Result<bool, GatewayError> {
        self.inner.delete_message(chat_id, message_id).await
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.upload_file(chat_id, file_path, caption).await?;
        Ok(())
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let mut embed = DiscordEmbed::new();
        embed.image = Some(EmbedMedia {
            url: image_url.to_string(),
        });
        self.send_embed(chat_id, caption, &[embed]).await?;
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.inner.base.is_running()
    }

    fn platform_name(&self) -> &str {
        "discord"
    }

    fn reactions_enabled(&self) -> bool {
        self.inner.config.reactions_enabled
    }

    async fn add_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        self.inner
            .add_reaction(chat_id, message_id, Self::map_reaction_emoji(emoji))
            .await
    }

    async fn remove_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        self.inner
            .remove_reaction(chat_id, message_id, Self::map_reaction_emoji(emoji))
            .await
    }

    async fn trigger_typing(&self, chat_id: &str) -> Result<(), GatewayError> {
        self.inner.trigger_typing(chat_id).await
    }

    async fn respond_interaction(
        &self,
        interaction_id: &str,
        interaction_token: &str,
        content: &str,
    ) -> Result<(), GatewayError> {
        self.respond_to_interaction(interaction_id, interaction_token, content)
            .await
    }
}

impl DiscordAdapter {
    fn format_outbound(text: &str, parse_mode: Option<ParseMode>) -> String {
        match parse_mode {
            Some(ParseMode::Plain) => text.to_string(),
            _ => crate::markdown_split::to_discord_markdown(text),
        }
    }

    fn map_reaction_emoji(emoji: &str) -> &str {
        match emoji {
            "eyes" => "👀",
            "white_check_mark" => "✅",
            "x" => "❌",
            other => other,
        }
    }
}

#[cfg(test)]
mod adapter_tests {
    use super::*;
    use crate::adapter::AdapterProxyConfig;

    #[test]
    fn gateway_payload_identify() {
        let config = DiscordConfig::for_test("test-token");
        let adapter = DiscordAdapter::new(config).unwrap();
        let payload = adapter.build_identify_payload();
        assert_eq!(payload.op, opcodes::IDENTIFY);
        assert!(payload.d.is_some());
    }

    #[tokio::test]
    async fn l01_start_marks_running() {
        let config = DiscordConfig::for_test("test-token");
        let adapter = DiscordAdapter::new(config).unwrap();
        adapter.start().await.unwrap();
        assert!(adapter.is_running());
        adapter.stop().await.unwrap();
    }

    #[tokio::test]
    async fn l02_stop_clears_running() {
        let config = DiscordConfig::for_test("test-token");
        let adapter = DiscordAdapter::new(config).unwrap();
        adapter.start().await.unwrap();
        adapter.stop().await.unwrap();
        assert!(!adapter.is_running());
    }

    #[tokio::test]
    async fn l03_message_create_without_inbound_tx_no_panic() {
        let mut config = DiscordConfig::for_test("test-token");
        config.require_mention = false;
        let adapter = DiscordAdapter::new(config).unwrap();
        let inner = Arc::clone(&adapter.inner);
        let mut session = GatewaySession::new();
        *inner.bot_user_id.write().await = Some("bot99".into());
        let frame = serde_json::json!({
            "op": 0,
            "t": "MESSAGE_CREATE",
            "d": {
                "id": "m1",
                "channel_id": "c1",
                "content": "hi",
                "author": { "id": "u1", "bot": false }
            }
        })
        .to_string();
        let result = gateway_loop::process_gateway_frame(&mut session, &frame, &inner).await;
        assert_eq!(result.1.len(), 1);
    }
}
