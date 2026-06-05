//! Delivery queue and router for deferred platform sends.
//!
//! Provides a `DeliveryQueue` for queuing messages, a `DeliveryTarget`
//! for specifying where messages should be routed, and a `DeliveryRouter`
//! that holds registered adapters and dispatches messages accordingly.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{debug, error, warn};

use crate::media::validate_media_delivery_path;
use hermes_core::errors::GatewayError;
use hermes_core::traits::PlatformAdapter;

// ---------------------------------------------------------------------------
// DeliveryItem
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DeliveryItem {
    pub platform: String,
    pub chat_id: String,
    pub text: String,
}

// ---------------------------------------------------------------------------
// DeliveryQueue
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct DeliveryQueue {
    queue: Arc<std::sync::Mutex<VecDeque<DeliveryItem>>>,
}

impl DeliveryQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enqueue(&self, item: DeliveryItem) {
        if let Ok(mut q) = self.queue.lock() {
            q.push_back(item);
        }
    }

    pub fn dequeue(&self) -> Option<DeliveryItem> {
        self.queue.lock().ok().and_then(|mut q| q.pop_front())
    }

    pub fn len(&self) -> usize {
        self.queue.lock().map(|q| q.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ---------------------------------------------------------------------------
// DeliveryTarget
// ---------------------------------------------------------------------------

/// Source information used when resolving an `origin` delivery target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryOrigin {
    pub platform: String,
    pub chat_id: String,
    pub thread_id: Option<String>,
}

impl DeliveryOrigin {
    pub fn new(platform: impl Into<String>, chat_id: impl Into<String>) -> Self {
        Self {
            platform: platform.into(),
            chat_id: chat_id.into(),
            thread_id: None,
        }
    }

    pub fn with_thread(
        platform: impl Into<String>,
        chat_id: impl Into<String>,
        thread_id: impl Into<String>,
    ) -> Self {
        Self {
            platform: platform.into(),
            chat_id: chat_id.into(),
            thread_id: Some(thread_id.into()),
        }
    }
}

/// Specifies where a message should be delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryTarget {
    /// Normalized platform name (`local`, `telegram`, `discord`, ...).
    pub platform: String,
    /// Platform channel/chat identifier. `None` means the platform home target.
    pub chat_id: Option<String>,
    /// Optional thread/topic identifier for platforms that support threaded sends.
    pub thread_id: Option<String>,
    /// True when this target was parsed from `origin`.
    pub is_origin: bool,
    /// True when a chat/channel id was explicitly supplied in the target string.
    pub is_explicit: bool,
}

impl DeliveryTarget {
    pub fn local() -> Self {
        Self {
            platform: "local".to_string(),
            chat_id: None,
            thread_id: None,
            is_origin: false,
            is_explicit: false,
        }
    }

    pub fn origin_fallback() -> Self {
        Self {
            is_origin: true,
            ..Self::local()
        }
    }

    pub fn platform_home(name: impl AsRef<str>) -> Self {
        Self {
            platform: normalize_platform(name.as_ref()),
            chat_id: None,
            thread_id: None,
            is_origin: false,
            is_explicit: false,
        }
    }

    pub fn platform_chat(name: impl AsRef<str>, chat_id: impl Into<String>) -> Self {
        Self {
            platform: normalize_platform(name.as_ref()),
            chat_id: Some(chat_id.into()),
            thread_id: None,
            is_origin: false,
            is_explicit: true,
        }
    }

    pub fn platform_thread(
        name: impl AsRef<str>,
        chat_id: impl Into<String>,
        thread_id: impl Into<String>,
    ) -> Self {
        Self {
            platform: normalize_platform(name.as_ref()),
            chat_id: Some(chat_id.into()),
            thread_id: Some(thread_id.into()),
            is_origin: false,
            is_explicit: true,
        }
    }

    pub fn parse(target_str: &str) -> Self {
        Self::parse_with_origin(target_str, None)
    }

    pub fn parse_with_origin(target_str: &str, origin: Option<&DeliveryOrigin>) -> Self {
        let trimmed = target_str.trim();
        if trimmed.eq_ignore_ascii_case("origin") {
            return origin.map_or_else(Self::origin_fallback, |origin| Self {
                platform: normalize_platform(&origin.platform),
                chat_id: Some(origin.chat_id.clone()),
                thread_id: origin.thread_id.clone(),
                is_origin: true,
                is_explicit: false,
            });
        }

        if trimmed.eq_ignore_ascii_case("local") || trimmed.is_empty() {
            return Self::local();
        }

        if let Some((platform_raw, rest)) = trimmed.split_once(':') {
            let platform = normalize_platform(platform_raw);
            if !is_known_platform(&platform) {
                return Self::local();
            }

            let mut parts = rest.splitn(2, ':');
            let chat_id = parts.next().map(str::trim).filter(|s| !s.is_empty());
            let thread_id = parts.next().map(str::trim).filter(|s| !s.is_empty());

            return match (chat_id, thread_id) {
                (Some(chat_id), Some(thread_id)) => {
                    Self::platform_thread(platform, chat_id.to_string(), thread_id.to_string())
                }
                (Some(chat_id), None) => Self::platform_chat(platform, chat_id.to_string()),
                (None, _) => Self::platform_home(platform),
            };
        }

        let platform = normalize_platform(trimmed);
        if is_known_platform(&platform) {
            Self::platform_home(platform)
        } else {
            Self::local()
        }
    }

    pub fn is_local(&self) -> bool {
        self.platform == "local"
    }

    pub fn target_label(&self) -> String {
        if self.is_origin {
            return "origin".to_string();
        }
        if self.is_local() {
            return "local".to_string();
        }
        match (&self.chat_id, &self.thread_id) {
            (Some(chat_id), Some(thread_id)) => {
                format!("{}:{}:{}", self.platform, chat_id, thread_id)
            }
            (Some(chat_id), None) => format!("{}:{}", self.platform, chat_id),
            (None, _) => self.platform.clone(),
        }
    }
}

impl fmt::Display for DeliveryTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.target_label())
    }
}

fn normalize_platform(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('-', "_")
}

fn is_known_platform(name: &str) -> bool {
    matches!(
        name,
        "local"
            | "telegram"
            | "discord"
            | "slack"
            | "whatsapp"
            | "signal"
            | "matrix"
            | "mattermost"
            | "dingtalk"
            | "feishu"
            | "wecom"
            | "wecom_callback"
            | "weixin"
            | "qqbot"
            | "qq"
            | "bluebubbles"
            | "email"
            | "sms"
            | "homeassistant"
            | "ntfy"
            | "api_server"
            | "webhook"
    )
}

/// Parse a target string into a `DeliveryTarget`.
///
/// Supported formats:
/// - `"origin"` -> local fallback unless origin metadata is supplied
/// - `"local"` -> local queue
/// - `"telegram:12345"` -> explicit Telegram chat
/// - `"discord"` -> Discord home target
/// - `"slack:C123ABC:thread123"` -> explicit Slack channel/thread
pub fn parse_target(target_str: &str) -> DeliveryTarget {
    DeliveryTarget::parse(target_str)
}

/// Parse a target string and resolve `origin` using explicit source metadata.
pub fn parse_target_with_origin(
    target_str: &str,
    origin: Option<&DeliveryOrigin>,
) -> DeliveryTarget {
    DeliveryTarget::parse_with_origin(target_str, origin)
}

// ---------------------------------------------------------------------------
// DeliveryRouter
// ---------------------------------------------------------------------------

/// Routes messages to registered platform adapters based on delivery targets.
pub struct DeliveryRouter {
    adapters: RwLock<HashMap<String, Arc<dyn PlatformAdapter>>>,
    fallback_queue: DeliveryQueue,
}

impl DeliveryRouter {
    pub fn new() -> Self {
        Self {
            adapters: RwLock::new(HashMap::new()),
            fallback_queue: DeliveryQueue::new(),
        }
    }

    /// Register a platform adapter with the router.
    pub async fn register_adapter(&self, name: &str, adapter: Arc<dyn PlatformAdapter>) {
        let mut adapters = self.adapters.write().await;
        adapters.insert(name.to_string(), adapter);
        debug!(platform = name, "Registered adapter in delivery router");
    }

    /// Unregister a platform adapter.
    pub async fn unregister_adapter(&self, name: &str) -> bool {
        let mut adapters = self.adapters.write().await;
        adapters.remove(name).is_some()
    }

    /// List all registered adapter names.
    pub async fn registered_adapters(&self) -> Vec<String> {
        let adapters = self.adapters.read().await;
        adapters.keys().cloned().collect()
    }

    /// Route a delivery to the appropriate target.
    ///
    /// - `origin` targets use explicit origin args when supplied, otherwise their parsed source.
    /// - `local` targets enqueue to the fallback queue for internal processing.
    /// - Platform targets with explicit chat IDs send directly to the named adapter.
    pub async fn route_delivery(
        &self,
        target: &DeliveryTarget,
        message: &str,
        origin_platform: Option<&str>,
        origin_chat_id: Option<&str>,
    ) -> Result<(), GatewayError> {
        let platform_name = origin_platform
            .filter(|_| target.is_origin)
            .unwrap_or(target.platform.as_str());
        let chat_id = origin_chat_id
            .filter(|_| target.is_origin)
            .or(target.chat_id.as_deref());

        if platform_name == "local" {
            self.enqueue_local(message);
            return Ok(());
        }

        if let Some(chat_id) = chat_id {
            self.send_to_platform(platform_name, chat_id, message).await
        } else {
            Err(GatewayError::SendFailed(format!(
                "Delivery target '{}' has no chat_id/home channel configured",
                target
            )))
        }
    }

    fn enqueue_local(&self, message: &str) {
        self.fallback_queue.enqueue(DeliveryItem {
            platform: "local".to_string(),
            chat_id: "local".to_string(),
            text: message.to_string(),
        });
        debug!("Message enqueued to local delivery queue");
    }

    /// Send a message to a specific platform adapter.
    async fn send_to_platform(
        &self,
        platform_name: &str,
        chat_id: &str,
        message: &str,
    ) -> Result<(), GatewayError> {
        let adapters = self.adapters.read().await;
        match adapters.get(platform_name) {
            Some(adapter) => {
                if !adapter.is_running() {
                    warn!(
                        platform = platform_name,
                        "Adapter is not running, attempting delivery anyway"
                    );
                }
                adapter.send_message(chat_id, message, None).await
            }
            None => {
                error!(
                    platform = platform_name,
                    "No adapter registered for platform"
                );
                self.fallback_queue.enqueue(DeliveryItem {
                    platform: platform_name.to_string(),
                    chat_id: chat_id.to_string(),
                    text: message.to_string(),
                });
                Err(GatewayError::SendFailed(format!(
                    "No adapter registered for platform '{}'",
                    platform_name
                )))
            }
        }
    }

    /// Route a file delivery to the appropriate target.
    pub async fn route_file_delivery(
        &self,
        target: &DeliveryTarget,
        file_path: &str,
        caption: Option<&str>,
        origin_platform: Option<&str>,
        origin_chat_id: Option<&str>,
    ) -> Result<(), GatewayError> {
        let platform_name = origin_platform
            .filter(|_| target.is_origin)
            .unwrap_or(target.platform.as_str());
        let chat_id = origin_chat_id
            .filter(|_| target.is_origin)
            .or(target.chat_id.as_deref());

        if platform_name == "local" {
            debug!("File delivery to local is not supported, queueing as text");
            let msg = format!(
                "[file:{}]{}",
                file_path,
                caption.map(|c| format!(" {c}")).unwrap_or_default()
            );
            self.enqueue_local(&msg);
            return Ok(());
        }

        let chat_id = chat_id.ok_or_else(|| {
            GatewayError::SendFailed(format!(
                "File delivery target '{}' has no chat_id/home channel configured",
                target
            ))
        })?;

        let validated_path = validate_media_delivery_path(file_path).ok_or_else(|| {
            GatewayError::SendFailed("Refusing to deliver unsafe local file path".to_string())
        })?;
        let validated_path = validated_path.to_str().ok_or_else(|| {
            GatewayError::SendFailed("Refusing to deliver non-UTF-8 local file path".to_string())
        })?;

        let adapters = self.adapters.read().await;
        match adapters.get(platform_name) {
            Some(adapter) => adapter.send_file(chat_id, validated_path, caption).await,
            None => Err(GatewayError::SendFailed(format!(
                "No adapter registered for platform '{}'",
                platform_name
            ))),
        }
    }

    /// Access the fallback queue for local/unroutable messages.
    pub fn fallback_queue(&self) -> &DeliveryQueue {
        &self.fallback_queue
    }
}

impl Default for DeliveryRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use hermes_core::traits::ParseMode;
    use std::sync::Mutex;

    struct FileRecordingAdapter {
        files: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl PlatformAdapter for FileRecordingAdapter {
        async fn start(&self) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn stop(&self) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn send_message(
            &self,
            _chat_id: &str,
            _text: &str,
            _parse_mode: Option<ParseMode>,
        ) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn edit_message(
            &self,
            _chat_id: &str,
            _message_id: &str,
            _text: &str,
        ) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn send_file(
            &self,
            _chat_id: &str,
            file_path: &str,
            _caption: Option<&str>,
        ) -> Result<(), GatewayError> {
            self.files.lock().unwrap().push(file_path.to_string());
            Ok(())
        }

        fn is_running(&self) -> bool {
            true
        }

        fn platform_name(&self) -> &str {
            "test"
        }
    }

    #[test]
    fn test_parse_target_origin() {
        let target = parse_target("origin");
        assert_eq!(target.platform, "local");
        assert!(target.is_origin);
        assert_eq!(target.to_string(), "origin");

        let origin = DeliveryOrigin::with_thread("TELEGRAM", "789", "42");
        let resolved = parse_target_with_origin("ORIGIN", Some(&origin));
        assert_eq!(resolved.platform, "telegram");
        assert_eq!(resolved.chat_id.as_deref(), Some("789"));
        assert_eq!(resolved.thread_id.as_deref(), Some("42"));
        assert!(resolved.is_origin);
    }

    #[test]
    fn test_parse_target_local() {
        let target = parse_target("local");
        assert_eq!(target.platform, "local");
        assert_eq!(target.chat_id, None);
        assert!(!target.is_origin);
    }

    #[test]
    fn test_parse_target_platform() {
        let telegram = parse_target("telegram:12345");
        assert_eq!(telegram.platform, "telegram");
        assert_eq!(telegram.chat_id.as_deref(), Some("12345"));
        assert!(telegram.is_explicit);

        let discord = parse_target("discord");
        assert_eq!(discord.platform, "discord");
        assert_eq!(discord.chat_id, None);
        assert!(!discord.is_explicit);
    }

    #[test]
    fn test_parse_target_platform_preserves_chat_id_case() {
        let target = parse_target("SLACK:ChanID-AbC123");
        assert_eq!(target.platform, "slack");
        assert_eq!(target.chat_id.as_deref(), Some("ChanID-AbC123"));
        assert_eq!(target.to_string(), "slack:ChanID-AbC123");
    }

    #[test]
    fn test_parse_target_thread_preserves_case_and_limits_split() {
        let target = parse_target("slack:C123ABC:thread123");
        assert_eq!(target.platform, "slack");
        assert_eq!(target.chat_id.as_deref(), Some("C123ABC"));
        assert_eq!(target.thread_id.as_deref(), Some("thread123"));
        assert_eq!(target.to_string(), "slack:C123ABC:thread123");

        let matrix = parse_target("matrix:!RoomABC:example.org");
        assert_eq!(matrix.platform, "matrix");
        assert_eq!(matrix.chat_id.as_deref(), Some("!RoomABC"));
        assert_eq!(matrix.thread_id.as_deref(), Some("example.org"));
    }

    #[test]
    fn test_parse_target_fallback() {
        assert_eq!(parse_target("unknown").platform, "local");
        assert_eq!(parse_target("unknown:123").platform, "local");
    }

    #[test]
    fn test_delivery_queue() {
        let q = DeliveryQueue::new();
        assert!(q.is_empty());
        q.enqueue(DeliveryItem {
            platform: "test".into(),
            chat_id: "1".into(),
            text: "hello".into(),
        });
        assert_eq!(q.len(), 1);
        let item = q.dequeue().unwrap();
        assert_eq!(item.text, "hello");
        assert!(q.is_empty());
    }

    #[tokio::test]
    async fn route_file_delivery_validates_and_canonicalizes_path_before_adapter_send() {
        let router = DeliveryRouter::new();
        let files = Arc::new(Mutex::new(Vec::new()));
        router
            .register_adapter(
                "test",
                Arc::new(FileRecordingAdapter {
                    files: files.clone(),
                }),
            )
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let report = tmp.path().join("report.pdf");
        std::fs::write(&report, b"%PDF-1.4").unwrap();
        let wrapped = format!("`{}.`", report.display());

        router
            .route_file_delivery(
                &DeliveryTarget::platform_chat("test", "chat1"),
                &wrapped,
                Some("caption"),
                None,
                None,
            )
            .await
            .expect("safe file should deliver");

        assert_eq!(
            files.lock().unwrap().as_slice(),
            &[std::fs::canonicalize(&report)
                .unwrap()
                .to_string_lossy()
                .to_string()]
        );

        let err = router
            .route_file_delivery(
                &DeliveryTarget::platform_chat("test", "chat1"),
                "/etc/passwd",
                None,
                None,
                None,
            )
            .await
            .expect_err("system file should be rejected");
        assert!(err.to_string().contains("unsafe local file path"));
        assert_eq!(files.lock().unwrap().len(), 1);
    }
}
