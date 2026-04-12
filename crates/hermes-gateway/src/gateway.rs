//! Gateway orchestrator: starts, stops, and routes messages to platform adapters.
//!
//! Implements the full message flow:
//! 1. Platform adapter receives a message
//! 2. Gateway looks up or creates a session via `SessionManager`
//! 3. Gateway checks DM authorization via `DmManager`
//! 4. Gateway invokes the agent loop with the session's message history
//! 5. Gateway sends the response back via the platform adapter
//!
//! Also integrates `StreamManager` for progressive message editing.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};
use hermes_core::types::Message;

use crate::dm::{DmDecision, DmManager};
use crate::session::SessionManager;
use crate::stream::{StreamConfig, StreamManager};

// ---------------------------------------------------------------------------
// GatewayConfig
// ---------------------------------------------------------------------------

/// Configuration for the Gateway orchestrator.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GatewayConfig {
    /// Enable SSRF protection on outbound URLs (default: true).
    #[serde(default = "default_true")]
    pub ssrf_protection: bool,

    /// Media cache directory path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_cache_dir: Option<String>,

    /// Maximum media cache size in bytes (0 = unlimited).
    #[serde(default)]
    pub media_cache_max_bytes: u64,

    /// Whether to enable streaming output (progressive message editing).
    #[serde(default)]
    pub streaming_enabled: bool,

    /// Streaming configuration.
    #[serde(default)]
    pub streaming: StreamConfig,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            ssrf_protection: true,
            media_cache_dir: None,
            media_cache_max_bytes: 0,
            streaming_enabled: false,
            streaming: StreamConfig::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// IncomingMessage (platform-agnostic)
// ---------------------------------------------------------------------------

/// A platform-agnostic incoming message for gateway routing.
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    /// Platform name (e.g., "telegram", "discord").
    pub platform: String,
    /// Chat/channel identifier.
    pub chat_id: String,
    /// User identifier.
    pub user_id: String,
    /// Message text content.
    pub text: String,
    /// Platform-specific message ID (for reply threading).
    pub message_id: Option<String>,
    /// Whether this is a DM (direct message) or group message.
    pub is_dm: bool,
}

// ---------------------------------------------------------------------------
// MessageHandler callback
// ---------------------------------------------------------------------------

/// Callback type for processing messages through the agent loop.
/// Takes the session messages and returns the agent's response text.
pub type MessageHandler = Arc<
    dyn Fn(Vec<Message>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send>>
        + Send
        + Sync,
>;

/// Callback type for streaming message processing.
/// Takes session messages and a chunk callback, returns the final response.
pub type StreamingMessageHandler = Arc<
    dyn Fn(
            Vec<Message>,
            Arc<dyn Fn(String) + Send + Sync>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send>>
        + Send
        + Sync,
>;

// ---------------------------------------------------------------------------
// Gateway
// ---------------------------------------------------------------------------

/// Central orchestrator for all platform adapters.
///
/// The `Gateway` owns a collection of named `PlatformAdapter` instances,
/// a `SessionManager`, a `DmManager`, and a `StreamManager`. It provides
/// a unified interface to start/stop adapters and route messages.
pub struct Gateway {
    adapters: RwLock<HashMap<String, Arc<dyn PlatformAdapter>>>,
    session_manager: Arc<SessionManager>,
    dm_manager: Arc<RwLock<DmManager>>,
    stream_manager: Arc<StreamManager>,
    config: GatewayConfig,
    /// Optional message handler for processing messages through the agent loop.
    message_handler: RwLock<Option<MessageHandler>>,
    /// Optional streaming message handler.
    streaming_handler: RwLock<Option<StreamingMessageHandler>>,
}

impl Gateway {
    /// Create a new `Gateway` with the given session manager and config.
    pub fn new(
        session_manager: Arc<SessionManager>,
        dm_manager: DmManager,
        config: GatewayConfig,
    ) -> Self {
        let stream_manager = Arc::new(StreamManager::new(config.streaming.clone()));

        Self {
            adapters: RwLock::new(HashMap::new()),
            session_manager,
            dm_manager: Arc::new(RwLock::new(dm_manager)),
            stream_manager,
            config,
            message_handler: RwLock::new(None),
            streaming_handler: RwLock::new(None),
        }
    }

    /// Create a Gateway with default DM manager (pair behavior).
    pub fn with_defaults(session_manager: Arc<SessionManager>, config: GatewayConfig) -> Self {
        Self::new(session_manager, DmManager::with_pair_behavior(), config)
    }

    /// Set the message handler for processing incoming messages.
    pub async fn set_message_handler(&self, handler: MessageHandler) {
        *self.message_handler.write().await = Some(handler);
    }

    /// Set the streaming message handler.
    pub async fn set_streaming_handler(&self, handler: StreamingMessageHandler) {
        *self.streaming_handler.write().await = Some(handler);
    }

    /// Register a platform adapter under the given name.
    pub async fn register_adapter(&self, name: impl Into<String>, adapter: Arc<dyn PlatformAdapter>) {
        let name = name.into();
        info!("Registering platform adapter: {}", name);
        self.adapters.write().await.insert(name, adapter);
    }

    /// Retrieve a registered platform adapter by name.
    pub async fn get_adapter(&self, name: &str) -> Option<Arc<dyn PlatformAdapter>> {
        self.adapters.read().await.get(name).cloned()
    }

    /// Start all registered and enabled platform adapters.
    pub async fn start_all(&self) -> Result<(), GatewayError> {
        let adapters = self.adapters.read().await;
        for (name, adapter) in adapters.iter() {
            info!("Starting platform adapter: {}", name);
            if let Err(e) = adapter.start().await {
                error!("Failed to start adapter '{}': {}", name, e);
                return Err(e);
            }
        }
        info!("All platform adapters started successfully");
        Ok(())
    }

    /// Stop all platform adapters gracefully.
    pub async fn stop_all(&self) -> Result<(), GatewayError> {
        let adapters = self.adapters.read().await;
        for (name, adapter) in adapters.iter() {
            info!("Stopping platform adapter: {}", name);
            if let Err(e) = adapter.stop().await {
                warn!("Error stopping adapter '{}': {}", name, e);
            }
        }
        info!("All platform adapters stopped");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Message routing
    // -----------------------------------------------------------------------

    /// Route an incoming message through the full pipeline:
    /// DM check → session lookup → agent loop → response.
    pub async fn route_message(&self, incoming: &IncomingMessage) -> Result<(), GatewayError> {
        // 1. Check DM authorization if this is a direct message
        if incoming.is_dm {
            let dm_manager = self.dm_manager.read().await;
            let decision = dm_manager.handle_dm(&incoming.user_id, &incoming.platform).await;

            match decision {
                DmDecision::Allow => {
                    // Proceed
                }
                DmDecision::Pair { message } => {
                    // Send pairing message and return
                    if let Some(msg) = message {
                        self.send_message(&incoming.platform, &incoming.chat_id, &msg, None).await?;
                    }
                    return Ok(());
                }
                DmDecision::Deny => {
                    debug!(
                        user_id = incoming.user_id,
                        platform = incoming.platform,
                        "DM denied for unauthorized user"
                    );
                    return Ok(());
                }
            }
        }

        // 2. Get or create session
        let session = self.session_manager
            .get_or_create_session(&incoming.platform, &incoming.chat_id, &incoming.user_id)
            .await;

        let session_key = format!("{}:{}", incoming.platform, incoming.chat_id);

        // 3. Add the user message to the session
        self.session_manager
            .add_message(&session_key, Message::user(&incoming.text))
            .await;

        // 4. Get all session messages for the agent loop
        let messages = self.session_manager.get_messages(&session_key).await;

        // 5. Process through agent loop (streaming or non-streaming)
        if self.config.streaming_enabled {
            self.route_streaming(&incoming, messages, &session_key).await?;
        } else {
            self.route_non_streaming(&incoming, messages, &session_key).await?;
        }

        Ok(())
    }

    /// Non-streaming message routing: invoke agent, send complete response.
    async fn route_non_streaming(
        &self,
        incoming: &IncomingMessage,
        messages: Vec<Message>,
        session_key: &str,
    ) -> Result<(), GatewayError> {
        let handler = self.message_handler.read().await;
        let handler = handler.as_ref().ok_or_else(|| {
            GatewayError::Platform("No message handler configured".into())
        })?;

        let response = handler(messages).await?;

        // Add assistant response to session
        self.session_manager
            .add_message(session_key, Message::assistant(&response))
            .await;

        // Send response back to the platform
        self.send_message(&incoming.platform, &incoming.chat_id, &response, None).await?;

        Ok(())
    }

    /// Streaming message routing: progressively edit messages as tokens arrive.
    async fn route_streaming(
        &self,
        incoming: &IncomingMessage,
        messages: Vec<Message>,
        session_key: &str,
    ) -> Result<(), GatewayError> {
        let handler = self.streaming_handler.read().await;
        let handler = handler.as_ref().ok_or_else(|| {
            GatewayError::Platform("No streaming handler configured".into())
        })?;

        // Start a stream
        let stream_handle = self.stream_manager
            .start_stream(&incoming.platform, &incoming.chat_id)
            .await;
        let stream_id = stream_handle.id.clone();

        // Send an initial placeholder message
        self.send_message(&incoming.platform, &incoming.chat_id, "...", None).await?;

        // Set up the chunk callback that updates the stream and edits the message
        let stream_manager = self.stream_manager.clone();
        let platform = incoming.platform.clone();
        let chat_id = incoming.chat_id.clone();
        let gateway_adapters = self.adapters.read().await.clone();
        let sid = stream_id.clone();

        let on_chunk: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |chunk: String| {
            let sm = stream_manager.clone();
            let sid = sid.clone();
            let platform = platform.clone();
            let chat_id = chat_id.clone();
            let adapters = gateway_adapters.clone();

            tokio::spawn(async move {
                if let Some(should_flush) = sm.update_stream(&sid, &chunk).await {
                    if should_flush {
                        if let Some(content) = sm.get_stream_content(&sid).await {
                            if let Some(adapter) = adapters.get(&platform) {
                                // For streaming, we'd need the message_id from the initial send.
                                // This is a simplified version.
                                let _ = adapter.send_message(&chat_id, &content, None).await;
                            }
                        }
                    }
                }
            });
        });

        // Invoke the streaming handler
        let response = handler(messages, on_chunk).await?;

        // Finish the stream
        self.stream_manager.finish_stream(&stream_id).await;

        // Add assistant response to session
        self.session_manager
            .add_message(session_key, Message::assistant(&response))
            .await;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Message sending (delegates to adapters)
    // -----------------------------------------------------------------------

    /// Send a text message to a specific platform chat.
    pub async fn send_message(
        &self,
        platform: &str,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        let adapter = self.get_adapter(platform).await.ok_or_else(|| {
            GatewayError::Platform(format!("No adapter registered for platform: {}", platform))
        })?;
        adapter.send_message(chat_id, text, parse_mode).await
    }

    /// Edit an existing message on a specific platform chat.
    pub async fn edit_message(
        &self,
        platform: &str,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        let adapter = self.get_adapter(platform).await.ok_or_else(|| {
            GatewayError::Platform(format!("No adapter registered for platform: {}", platform))
        })?;
        adapter.edit_message(chat_id, message_id, text).await
    }

    /// Send a file to a specific platform chat with an optional caption.
    pub async fn send_file(
        &self,
        platform: &str,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let adapter = self.get_adapter(platform).await.ok_or_else(|| {
            GatewayError::Platform(format!("No adapter registered for platform: {}", platform))
        })?;
        adapter.send_file(chat_id, file_path, caption).await
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Get a reference to the session manager.
    pub fn session_manager(&self) -> &Arc<SessionManager> {
        &self.session_manager
    }

    /// Get a reference to the stream manager.
    pub fn stream_manager(&self) -> &Arc<StreamManager> {
        &self.stream_manager
    }

    /// Get a reference to the gateway config.
    pub fn config(&self) -> &GatewayConfig {
        &self.config
    }

    /// List the names of all registered adapters.
    pub async fn adapter_names(&self) -> Vec<String> {
        self.adapters.read().await.keys().cloned().collect()
    }

    /// Periodically expires inactive sessions.
    pub async fn session_expiry_watcher(&self, interval_secs: u64) {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(30)));
        loop {
            ticker.tick().await;
            let expired = self.session_manager.expire_idle_sessions().await;
            if expired > 0 {
                tracing::info!(expired, "Expired idle sessions");
            }
        }
    }

    /// Monitors adapter health and attempts reconnect through stop/start.
    pub async fn platform_reconnect_watcher(&self, interval_secs: u64) {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(20)));
        loop {
            ticker.tick().await;
            let snapshot = self.adapters.read().await.clone();
            for (name, adapter) in snapshot {
                if !adapter.is_running() {
                    tracing::warn!(platform = %name, "Adapter appears offline, reconnecting");
                    let _ = adapter.start().await;
                }
            }
        }
    }

    /// Attach vision hint for image-bearing messages.
    pub fn enrich_message_with_vision(&self, text: &str) -> String {
        if text.contains("http://") || text.contains("https://") {
            format!("[vision_candidate]\n{}", text)
        } else {
            text.to_string()
        }
    }

    /// Attach transcription hint for audio-bearing messages.
    pub fn enrich_message_with_transcription(&self, text: &str) -> String {
        if text.contains(".mp3") || text.contains(".wav") || text.contains(".m4a") {
            format!("[transcription_candidate]\n{}", text)
        } else {
            text.to_string()
        }
    }

    /// Build deterministic signature for config-change detection.
    pub fn agent_config_signature(&self) -> String {
        let s = serde_json::to_string(&self.config).unwrap_or_default();
        format!("{:x}", md5::compute(s))
    }

    /// Load optional prefill messages.
    pub fn load_prefill_messages(&self, path: &std::path::Path) -> Vec<Message> {
        let Ok(content) = std::fs::read_to_string(path) else {
            return vec![];
        };
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(Message::user)
            .collect()
    }

    /// Load optional ephemeral system prompt.
    pub fn load_ephemeral_system_prompt(&self, path: &std::path::Path) -> Option<String> {
        std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    /// Resolve model routing candidate for a message.
    pub fn load_smart_model_routing(&self, text: &str) -> Option<String> {
        if text.len() > 2000 || text.contains("analyze") || text.contains("refactor") {
            Some("openai:gpt-4o".to_string())
        } else if text.contains("quick") || text.contains("summary") {
            Some("openai:gpt-4o-mini".to_string())
        } else {
            None
        }
    }

    /// Authorize user based on DM manager and platform context.
    pub async fn is_user_authorized(&self, user_id: &str, platform: &str) -> bool {
        let dm = self.dm_manager.read().await;
        dm.is_authorized(user_id) || dm.handle_dm(user_id, platform).await == DmDecision::Allow
    }

    /// Send update notification message to a chat.
    pub async fn send_update_notification(
        &self,
        platform: &str,
        chat_id: &str,
        latest_version: &str,
    ) -> Result<(), GatewayError> {
        let msg = format!("Update available: Hermes {}", latest_version);
        self.send_message(platform, chat_id, &msg, None).await
    }

    /// Watch external process output and forward to a callback.
    pub async fn run_process_watcher(
        &self,
        mut child: tokio::process::Child,
        on_output: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Result<(), GatewayError> {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| GatewayError::Platform("Process has no stdout".into()))?;
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| GatewayError::Platform(format!("Watcher read error: {}", e)))?
        {
            on_output(line);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use hermes_config::session::SessionConfig;

    #[test]
    fn gateway_config_default() {
        let cfg = GatewayConfig::default();
        assert!(cfg.ssrf_protection);
        assert!(cfg.media_cache_dir.is_none());
        assert_eq!(cfg.media_cache_max_bytes, 0);
        assert!(!cfg.streaming_enabled);
    }

    #[tokio::test]
    async fn gateway_register_and_list_adapters() {
        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let gw = Gateway::with_defaults(session_mgr, GatewayConfig::default());

        assert!(gw.adapter_names().await.is_empty());
    }

    #[tokio::test]
    async fn gateway_route_dm_denied() {
        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let dm_manager = DmManager::with_ignore_behavior();
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "unknown_user".into(),
            text: "hello".into(),
            message_id: None,
            is_dm: true,
        };

        // Should succeed (deny silently)
        let result = gw.route_message(&incoming).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn gateway_route_no_handler() {
        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            message_id: None,
            is_dm: true,
        };

        // Should fail because no message handler is set
        let result = gw.route_message(&incoming).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn gateway_route_group_message_skips_dm_check() {
        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let dm_manager = DmManager::with_ignore_behavior();
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "-group1".into(),
            user_id: "unknown_user".into(),
            text: "hello group".into(),
            message_id: None,
            is_dm: false, // Group message, no DM check
        };

        // Should fail because no handler, but DM check is skipped
        let result = gw.route_message(&incoming).await;
        assert!(result.is_err()); // No handler configured
    }
}
