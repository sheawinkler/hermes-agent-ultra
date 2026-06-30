//! Delivery queue and router for deferred platform sends.
//!
//! Provides a `DeliveryQueue` for queuing messages, a `DeliveryTarget`
//! for specifying where messages should be routed, and a `DeliveryRouter`
//! that holds registered adapters and dispatches messages accordingly.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::media::validate_media_delivery_path;
use hermes_core::errors::{GatewayError, SendErrorKind};
use hermes_core::traits::{PlatformAdapter, SendMessageOptions};

/// Cap before gateway-level truncation for adapters that cannot split long text.
pub(crate) const MAX_PLATFORM_OUTPUT: usize = 4000;
static AUDIT_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const DEAD_TARGETS_FILE: &str = "dead_targets.json";

pub(crate) fn prepare_platform_message_for_adapter<'a>(
    adapter: &dyn PlatformAdapter,
    message: &'a str,
    audit_label: Option<&str>,
) -> Result<Cow<'a, str>, GatewayError> {
    prepare_platform_message_for_adapter_at(
        adapter,
        message,
        audit_label,
        &hermes_config::hermes_home(),
    )
}

fn prepare_platform_message_for_adapter_at<'a>(
    adapter: &dyn PlatformAdapter,
    message: &'a str,
    audit_label: Option<&str>,
    hermes_home: &Path,
) -> Result<Cow<'a, str>, GatewayError> {
    if message.chars().count() <= MAX_PLATFORM_OUTPUT {
        return Ok(Cow::Borrowed(message));
    }

    let label = sanitized_audit_label(audit_label, adapter.platform_name());
    if adapter.splits_long_messages() {
        match save_full_platform_output_at(message, &label, hermes_home) {
            Ok(saved_path) => info!(
                platform = adapter.platform_name(),
                path = %saved_path.display(),
                chars = message.chars().count(),
                "Preserved long platform output for chunking adapter"
            ),
            Err(err) => warn!(
                platform = adapter.platform_name(),
                error = %err,
                chars = message.chars().count(),
                "Failed to save long platform output audit copy; delivering full content"
            ),
        }
        return Ok(Cow::Borrowed(message));
    }

    let saved_path = save_full_platform_output_at(message, &label, hermes_home).map_err(|err| {
        GatewayError::SendFailed(format!(
            "Failed to save full platform output before truncation: {err}"
        ))
    })?;
    let footer = format!(
        "\n\n... [truncated, full output saved to {}]",
        saved_path.display()
    );
    let visible = MAX_PLATFORM_OUTPUT.saturating_sub(footer.chars().count());
    let mut truncated = message.chars().take(visible).collect::<String>();
    truncated.push_str(&footer);
    info!(
        platform = adapter.platform_name(),
        path = %saved_path.display(),
        original_chars = message.chars().count(),
        delivered_chars = truncated.chars().count(),
        "Truncated long platform output for non-chunking adapter"
    );
    Ok(Cow::Owned(truncated))
}

fn save_full_platform_output_at(
    content: &str,
    label: &str,
    hermes_home: &Path,
) -> std::io::Result<PathBuf> {
    let output_dir = hermes_home.join("cron").join("output");
    std::fs::create_dir_all(&output_dir)?;
    let now_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let sequence = AUDIT_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = output_dir.join(format!(
        "{}_{}_{}_{}.txt",
        label,
        now_millis,
        std::process::id(),
        sequence
    ));
    std::fs::write(&path, content)?;
    Ok(path)
}

fn sanitized_audit_label(label: Option<&str>, fallback: &str) -> String {
    let raw = label
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback);
    let mut sanitized = String::with_capacity(raw.len().min(64));
    for ch in raw.chars().take(64) {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            sanitized.push(ch);
        } else if !sanitized.ends_with('_') {
            sanitized.push('_');
        }
    }
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "platform-output".to_string()
    } else {
        trimmed.to_string()
    }
}

// ---------------------------------------------------------------------------
// DeadTargetRegistry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct DeadTargetEntry {
    platform: String,
    chat_id: String,
    reason: String,
    marked_at_unix: u64,
}

/// Persistent, best-effort registry of delivery targets that a platform has
/// confirmed unreachable, such as deleted groups or blocked bots.
#[derive(Clone)]
pub struct DeadTargetRegistry {
    path: PathBuf,
    entries: Arc<std::sync::Mutex<HashMap<String, DeadTargetEntry>>>,
}

impl DeadTargetRegistry {
    pub fn new() -> Self {
        Self::with_path(
            hermes_config::hermes_home()
                .join("gateway")
                .join(DEAD_TARGETS_FILE),
        )
    }

    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let entries = load_dead_targets(&path);
        Self {
            path,
            entries: Arc::new(std::sync::Mutex::new(entries)),
        }
    }

    pub fn is_dead_error_kind(kind: SendErrorKind) -> bool {
        matches!(kind, SendErrorKind::Forbidden | SendErrorKind::NotFound)
    }

    pub fn is_dead(&self, platform: &str, chat_id: &str) -> bool {
        let key = dead_target_key(platform, chat_id);
        self.entries
            .lock()
            .map(|entries| entries.contains_key(&key))
            .unwrap_or(false)
    }

    pub fn mark_dead(&self, platform: &str, chat_id: &str, reason: impl Into<String>) -> bool {
        let key = dead_target_key(platform, chat_id);
        let entry = DeadTargetEntry {
            platform: normalize_platform(platform),
            chat_id: chat_id.trim().to_string(),
            reason: reason.into().chars().take(240).collect(),
            marked_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        let Ok(mut entries) = self.entries.lock() else {
            return false;
        };
        let inserted = entries.insert(key.clone(), entry).is_none();
        flush_dead_targets(&self.path, &entries);
        if inserted {
            warn!(
                target = %key,
                "Marked delivery target as confirmed dead; future deliveries will be skipped"
            );
        }
        inserted
    }

    pub fn clear(&self, platform: &str, chat_id: &str) -> bool {
        let key = dead_target_key(platform, chat_id);
        let Ok(mut entries) = self.entries.lock() else {
            return false;
        };
        let removed = entries.remove(&key).is_some();
        if removed {
            flush_dead_targets(&self.path, &entries);
            info!(
                target = %key,
                "Cleared confirmed-dead delivery target after successful send"
            );
        }
        removed
    }

    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .map(|entries| entries.len())
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for DeadTargetRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn dead_target_key(platform: &str, chat_id: &str) -> String {
    format!("{}:{}", normalize_platform(platform), chat_id.trim())
}

fn load_dead_targets(path: &Path) -> HashMap<String, DeadTargetEntry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    serde_json::from_str::<HashMap<String, DeadTargetEntry>>(&text).unwrap_or_default()
}

fn flush_dead_targets(path: &Path, entries: &HashMap<String, DeadTargetEntry>) {
    let Some(parent) = path.parent() else {
        return;
    };
    if let Err(err) = std::fs::create_dir_all(parent) {
        debug!(path = %parent.display(), error = %err, "Failed to create dead-target registry directory");
        return;
    }
    let tmp = path.with_extension("json.tmp");
    match serde_json::to_vec_pretty(entries) {
        Ok(bytes) => {
            if let Err(err) = std::fs::write(&tmp, bytes) {
                debug!(path = %tmp.display(), error = %err, "Failed to write dead-target registry");
                return;
            }
            if let Err(err) = std::fs::rename(&tmp, path) {
                debug!(from = %tmp.display(), to = %path.display(), error = %err, "Failed to replace dead-target registry");
            }
        }
        Err(err) => debug!(error = %err, "Failed to serialize dead-target registry"),
    }
}

fn is_thread_or_message_not_found(error_text: &str) -> bool {
    let lower = error_text.to_ascii_lowercase();
    lower.contains("thread not found")
        || lower.contains("topic_deleted")
        || lower.contains("message to reply not found")
        || lower.contains("message to edit not found")
        || lower.contains("message_id_invalid")
}

fn dead_target_reason_from_error(error: &GatewayError) -> Option<String> {
    let rendered = error.to_string();
    if is_thread_or_message_not_found(&rendered) {
        return None;
    }
    let kind = error.send_error_kind();
    DeadTargetRegistry::is_dead_error_kind(kind).then(|| format!("{kind}: {rendered}"))
}

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
    dead_targets: DeadTargetRegistry,
}

impl DeliveryRouter {
    pub fn new() -> Self {
        Self::with_dead_target_registry(DeadTargetRegistry::new())
    }

    pub fn with_dead_target_registry(dead_targets: DeadTargetRegistry) -> Self {
        Self {
            adapters: RwLock::new(HashMap::new()),
            fallback_queue: DeliveryQueue::new(),
            dead_targets,
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
            self.send_to_platform(
                platform_name,
                chat_id,
                message,
                SendMessageOptions {
                    thread_id: target.thread_id.clone(),
                    explicit_chat_id: target.is_explicit,
                    notify: false,
                    non_conversational: false,
                    delivery_audit_label: None,
                },
            )
            .await
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
        options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        if self.dead_targets.is_dead(platform_name, chat_id) {
            info!(
                platform = platform_name,
                chat_id = chat_id,
                "Skipping delivery to confirmed-dead target"
            );
            return Ok(());
        }

        let adapters = self.adapters.read().await;
        match adapters.get(platform_name) {
            Some(adapter) => {
                if !adapter.is_running() {
                    warn!(
                        platform = platform_name,
                        "Adapter is not running, attempting delivery anyway"
                    );
                }
                let message = prepare_platform_message_for_adapter(
                    adapter.as_ref(),
                    message,
                    options.delivery_audit_label.as_deref(),
                )?;
                let result = adapter
                    .send_message_with_options(chat_id, message.as_ref(), None, options)
                    .await;
                match result {
                    Ok(()) => {
                        self.dead_targets.clear(platform_name, chat_id);
                        Ok(())
                    }
                    Err(err) => {
                        if let Some(reason) = dead_target_reason_from_error(&err) {
                            self.dead_targets.mark_dead(platform_name, chat_id, reason);
                        }
                        Err(err)
                    }
                }
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
            Some(adapter) => {
                if self.dead_targets.is_dead(platform_name, chat_id) {
                    info!(
                        platform = platform_name,
                        chat_id = chat_id,
                        "Skipping file delivery to confirmed-dead target"
                    );
                    return Ok(());
                }
                let result = adapter
                    .send_file_with_options(
                        chat_id,
                        validated_path,
                        caption,
                        SendMessageOptions {
                            thread_id: target.thread_id.clone(),
                            explicit_chat_id: target.is_explicit,
                            notify: false,
                            non_conversational: false,
                            delivery_audit_label: None,
                        },
                    )
                    .await;
                match result {
                    Ok(()) => {
                        self.dead_targets.clear(platform_name, chat_id);
                        Ok(())
                    }
                    Err(err) => {
                        if let Some(reason) = dead_target_reason_from_error(&err) {
                            self.dead_targets.mark_dead(platform_name, chat_id, reason);
                        }
                        Err(err)
                    }
                }
            }
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

    type RecordedMessage = (String, String, Option<String>, bool);

    struct MessageRecordingAdapter {
        messages: Arc<Mutex<Vec<RecordedMessage>>>,
        splits_long_messages: bool,
    }

    struct FailingMessageAdapter {
        calls: Arc<Mutex<Vec<String>>>,
        error_text: &'static str,
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

    #[async_trait]
    impl PlatformAdapter for MessageRecordingAdapter {
        async fn start(&self) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn stop(&self) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn send_message(
            &self,
            chat_id: &str,
            text: &str,
            _parse_mode: Option<ParseMode>,
        ) -> Result<(), GatewayError> {
            self.messages.lock().unwrap().push((
                chat_id.to_string(),
                text.to_string(),
                None,
                false,
            ));
            Ok(())
        }

        async fn send_message_with_options(
            &self,
            chat_id: &str,
            text: &str,
            _parse_mode: Option<ParseMode>,
            options: SendMessageOptions,
        ) -> Result<(), GatewayError> {
            self.messages.lock().unwrap().push((
                chat_id.to_string(),
                text.to_string(),
                options.thread_id,
                options.explicit_chat_id,
            ));
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
            _file_path: &str,
            _caption: Option<&str>,
        ) -> Result<(), GatewayError> {
            Ok(())
        }

        fn is_running(&self) -> bool {
            true
        }

        fn splits_long_messages(&self) -> bool {
            self.splits_long_messages
        }

        fn platform_name(&self) -> &str {
            "ntfy"
        }
    }

    #[async_trait]
    impl PlatformAdapter for FailingMessageAdapter {
        async fn start(&self) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn stop(&self) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn send_message(
            &self,
            chat_id: &str,
            _text: &str,
            _parse_mode: Option<ParseMode>,
        ) -> Result<(), GatewayError> {
            self.calls.lock().unwrap().push(chat_id.to_string());
            Err(GatewayError::SendFailed(self.error_text.to_string()))
        }

        async fn send_message_with_options(
            &self,
            chat_id: &str,
            _text: &str,
            _parse_mode: Option<ParseMode>,
            _options: SendMessageOptions,
        ) -> Result<(), GatewayError> {
            self.calls.lock().unwrap().push(chat_id.to_string());
            Err(GatewayError::SendFailed(self.error_text.to_string()))
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
            _file_path: &str,
            _caption: Option<&str>,
        ) -> Result<(), GatewayError> {
            Err(GatewayError::SendFailed(self.error_text.to_string()))
        }

        fn is_running(&self) -> bool {
            true
        }

        fn platform_name(&self) -> &str {
            "telegram"
        }
    }

    #[test]
    fn prepare_platform_message_truncates_and_saves_for_non_chunking_adapter() {
        let tmp = tempfile::tempdir().unwrap();
        let messages = Arc::new(Mutex::new(Vec::new()));
        let adapter = MessageRecordingAdapter {
            messages,
            splits_long_messages: false,
        };
        let content = "x".repeat(MAX_PLATFORM_OUTPUT + 1200);

        let prepared = prepare_platform_message_for_adapter_at(
            &adapter,
            &content,
            Some("job/unsafe label"),
            tmp.path(),
        )
        .expect("prepare");

        assert!(prepared.chars().count() <= MAX_PLATFORM_OUTPUT);
        assert!(prepared.contains("truncated, full output saved to"));
        assert_ne!(prepared.as_ref(), content);
        let saved: Vec<_> = std::fs::read_dir(tmp.path().join("cron").join("output"))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(saved.len(), 1);
        assert!(saved[0]
            .file_name()
            .to_string_lossy()
            .starts_with("job_unsafe_label_"));
        assert_eq!(std::fs::read_to_string(saved[0].path()).unwrap(), content);
    }

    #[test]
    fn prepare_platform_message_preserves_and_saves_for_chunking_adapter() {
        let tmp = tempfile::tempdir().unwrap();
        let messages = Arc::new(Mutex::new(Vec::new()));
        let adapter = MessageRecordingAdapter {
            messages,
            splits_long_messages: true,
        };
        let content = "x".repeat(MAX_PLATFORM_OUTPUT + 1200);

        let prepared =
            prepare_platform_message_for_adapter_at(&adapter, &content, Some("job2"), tmp.path())
                .expect("prepare");

        assert_eq!(prepared.as_ref(), content);
        let saved: Vec<_> = std::fs::read_dir(tmp.path().join("cron").join("output"))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(saved.len(), 1);
        assert!(saved[0].file_name().to_string_lossy().starts_with("job2_"));
        assert_eq!(std::fs::read_to_string(saved[0].path()).unwrap(), content);
    }

    #[test]
    fn save_full_platform_output_uses_unique_paths_for_same_label() {
        let tmp = tempfile::tempdir().unwrap();

        let first = save_full_platform_output_at("first", "same-job", tmp.path()).unwrap();
        let second = save_full_platform_output_at("second", "same-job", tmp.path()).unwrap();

        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(first).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(second).unwrap(), "second");
    }

    #[test]
    fn dead_target_registry_persists_and_clears() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("dead_targets.json");
        let registry = DeadTargetRegistry::with_path(&path);

        assert!(registry.is_empty());
        assert!(registry.mark_dead("Telegram", "123", "forbidden"));
        assert!(registry.is_dead("telegram", "123"));
        assert!(!registry.mark_dead("telegram", "123", "forbidden again"));

        let reloaded = DeadTargetRegistry::with_path(&path);
        assert!(reloaded.is_dead("telegram", "123"));
        assert!(reloaded.clear("telegram", "123"));
        assert!(!reloaded.is_dead("telegram", "123"));
    }

    #[test]
    fn dead_target_registry_corrupt_store_degrades_to_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("dead_targets.json");
        std::fs::write(&path, "{not json").unwrap();

        let registry = DeadTargetRegistry::with_path(&path);

        assert!(registry.is_empty());
    }

    #[test]
    fn dead_target_reason_excludes_thread_not_found() {
        let err = GatewayError::SendFailed("Bad Request: thread not found".to_string());
        assert!(dead_target_reason_from_error(&err).is_none());

        let err = GatewayError::SendFailed("Forbidden: bot was blocked by the user".to_string());
        assert!(dead_target_reason_from_error(&err)
            .expect("dead target")
            .contains("forbidden"));
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
    fn test_parse_target_ntfy_topic_is_explicit() {
        let target = parse_target("ntfy:alerts-channel");

        assert_eq!(target.platform, "ntfy");
        assert_eq!(target.chat_id.as_deref(), Some("alerts-channel"));
        assert_eq!(target.thread_id, None);
        assert!(target.is_explicit);
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
    async fn route_delivery_preserves_explicit_ntfy_topic() {
        let router = DeliveryRouter::new();
        let messages = Arc::new(Mutex::new(Vec::new()));
        router
            .register_adapter(
                "ntfy",
                Arc::new(MessageRecordingAdapter {
                    messages: messages.clone(),
                    splits_long_messages: false,
                }),
            )
            .await;

        router
            .route_delivery(&parse_target("ntfy:alerts-channel"), "done", None, None)
            .await
            .expect("explicit ntfy target should route");

        assert_eq!(
            messages.lock().unwrap().as_slice(),
            &[("alerts-channel".to_string(), "done".to_string(), None, true,)]
        );
    }

    #[tokio::test]
    async fn route_delivery_marks_dead_target_and_skips_next_attempt() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = DeadTargetRegistry::with_path(tmp.path().join("dead_targets.json"));
        let router = DeliveryRouter::with_dead_target_registry(registry.clone());
        let calls = Arc::new(Mutex::new(Vec::new()));
        router
            .register_adapter(
                "telegram",
                Arc::new(FailingMessageAdapter {
                    calls: calls.clone(),
                    error_text: "Forbidden: bot was blocked by the user",
                }),
            )
            .await;
        let target = parse_target("telegram:42");

        let first = router.route_delivery(&target, "hello", None, None).await;
        assert!(first.is_err());
        assert!(registry.is_dead("telegram", "42"));
        assert_eq!(calls.lock().unwrap().as_slice(), &["42".to_string()]);

        router
            .route_delivery(&target, "hello again", None, None)
            .await
            .expect("dead target short-circuits successfully");
        assert_eq!(calls.lock().unwrap().as_slice(), &["42".to_string()]);
    }

    #[tokio::test]
    async fn route_delivery_transient_and_thread_errors_are_not_marked_dead() {
        for error_text in [
            "http 503 temporarily unavailable",
            "Bad Request: thread not found",
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let registry = DeadTargetRegistry::with_path(tmp.path().join("dead_targets.json"));
            let router = DeliveryRouter::with_dead_target_registry(registry.clone());
            router
                .register_adapter(
                    "telegram",
                    Arc::new(FailingMessageAdapter {
                        calls: Arc::new(Mutex::new(Vec::new())),
                        error_text,
                    }),
                )
                .await;

            let result = router
                .route_delivery(&parse_target("telegram:13"), "hello", None, None)
                .await;

            assert!(result.is_err());
            assert!(
                !registry.is_dead("telegram", "13"),
                "{error_text} should not mark target dead"
            );
        }
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
