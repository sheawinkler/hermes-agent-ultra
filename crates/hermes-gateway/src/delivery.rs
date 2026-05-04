//! Delivery queue and router for deferred platform sends.
//!
//! Provides a `DeliveryQueue` for queuing messages, a `DeliveryTarget` enum
//! for specifying where messages should be routed, and a `DeliveryRouter`
//! that holds registered adapters and dispatches messages accordingly.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{debug, error, warn};

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

/// Specifies where a message should be delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryTarget {
    /// Send back to the originating platform/chat.
    Origin,
    /// Deliver to the local agent (e.g., for internal processing).
    Local,
    /// Deliver to a specific platform and chat.
    Platform { name: String, chat_id: String },
}

/// Parse a target string into a `DeliveryTarget`.
///
/// Supported formats:
/// - `"origin"` -> `DeliveryTarget::Origin`
/// - `"local"` -> `DeliveryTarget::Local`
/// - `"telegram:12345"` -> `DeliveryTarget::Platform { name: "telegram", chat_id: "12345" }`
/// - `"discord:channel_id"` -> `DeliveryTarget::Platform { name: "discord", chat_id: "channel_id" }`
pub fn parse_target(target_str: &str) -> DeliveryTarget {
    let trimmed = target_str.trim();
    if trimmed.eq_ignore_ascii_case("origin") {
        return DeliveryTarget::Origin;
    }
    if trimmed.eq_ignore_ascii_case("local") {
        return DeliveryTarget::Local;
    }
    if let Some((name_raw, chat_id_raw)) = trimmed.split_once(':') {
        DeliveryTarget::Platform {
            name: name_raw.trim().to_ascii_lowercase(),
            chat_id: chat_id_raw.trim().to_string(),
        }
    } else {
        DeliveryTarget::Origin
    }
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
    /// - `Origin` target requires `origin_platform` and `origin_chat_id` to be provided.
    /// - `Local` target enqueues to the fallback queue for internal processing.
    /// - `Platform` target sends directly to the named adapter.
    pub async fn route_delivery(
        &self,
        target: &DeliveryTarget,
        message: &str,
        origin_platform: Option<&str>,
        origin_chat_id: Option<&str>,
    ) -> Result<(), GatewayError> {
        match target {
            DeliveryTarget::Origin => {
                let platform_name = origin_platform.ok_or_else(|| {
                    GatewayError::SendFailed(
                        "Origin target but no origin platform specified".into(),
                    )
                })?;
                let chat_id = origin_chat_id.ok_or_else(|| {
                    GatewayError::SendFailed("Origin target but no origin chat_id specified".into())
                })?;
                self.send_to_platform(platform_name, chat_id, message).await
            }
            DeliveryTarget::Local => {
                self.fallback_queue.enqueue(DeliveryItem {
                    platform: "local".to_string(),
                    chat_id: "local".to_string(),
                    text: message.to_string(),
                });
                debug!("Message enqueued to local delivery queue");
                Ok(())
            }
            DeliveryTarget::Platform { name, chat_id } => {
                self.send_to_platform(name, chat_id, message).await
            }
        }
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
        let (platform_name, chat_id) = match target {
            DeliveryTarget::Origin => {
                let p = origin_platform.ok_or_else(|| {
                    GatewayError::SendFailed("Origin target but no origin platform".into())
                })?;
                let c = origin_chat_id.ok_or_else(|| {
                    GatewayError::SendFailed("Origin target but no origin chat_id".into())
                })?;
                (p, c)
            }
            DeliveryTarget::Local => {
                debug!("File delivery to local is not supported, queueing as text");
                let msg = format!(
                    "[file:{}]{}",
                    file_path,
                    caption.map(|c| format!(" {c}")).unwrap_or_default()
                );
                self.fallback_queue.enqueue(DeliveryItem {
                    platform: "local".to_string(),
                    chat_id: "local".to_string(),
                    text: msg,
                });
                return Ok(());
            }
            DeliveryTarget::Platform { name, chat_id } => (name.as_str(), chat_id.as_str()),
        };

        let adapters = self.adapters.read().await;
        match adapters.get(platform_name) {
            Some(adapter) => adapter.send_file(chat_id, file_path, caption).await,
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

    #[test]
    fn test_parse_target_origin() {
        assert_eq!(parse_target("origin"), DeliveryTarget::Origin);
        assert_eq!(parse_target("ORIGIN"), DeliveryTarget::Origin);
    }

    #[test]
    fn test_parse_target_local() {
        assert_eq!(parse_target("local"), DeliveryTarget::Local);
    }

    #[test]
    fn test_parse_target_platform() {
        assert_eq!(
            parse_target("telegram:12345"),
            DeliveryTarget::Platform {
                name: "telegram".into(),
                chat_id: "12345".into()
            }
        );
        assert_eq!(
            parse_target("discord:channel_abc"),
            DeliveryTarget::Platform {
                name: "discord".into(),
                chat_id: "channel_abc".into()
            }
        );
    }

    #[test]
    fn test_parse_target_platform_preserves_chat_id_case() {
        assert_eq!(
            parse_target("SLACK:ChanID-AbC123"),
            DeliveryTarget::Platform {
                name: "slack".into(),
                chat_id: "ChanID-AbC123".into()
            }
        );
    }

    #[test]
    fn test_parse_target_fallback() {
        assert_eq!(parse_target("unknown"), DeliveryTarget::Origin);
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
}
