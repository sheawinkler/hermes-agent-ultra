//! Cron completion → `webhooks.json` HTTP delivery (shared by gateway sidecar and tests).

use std::path::Path;
use std::time::Duration;

use hermes_core::AgentError;
use hermes_cron::CronCompletionEvent;
use serde::{Deserialize, Serialize};

/// Local webhook registry shape (same as `hermes webhook` CLI).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebhookStore {
    pub webhooks: Vec<WebhookRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookRecord {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub created_at: String,
}

pub fn load_webhook_store(path: &Path) -> Result<WebhookStore, AgentError> {
    if !path.exists() {
        return Ok(WebhookStore::default());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_json::from_str(&raw)
        .map_err(|e| AgentError::Io(format!("parse {}: {}", path.display(), e)))
}

pub fn save_webhook_store(path: &Path, store: &WebhookStore) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AgentError::Io(format!("mkdir: {}", e)))?;
    }
    let raw = serde_json::to_string_pretty(store)
        .map_err(|e| AgentError::Io(format!("serialize webhooks: {}", e)))?;
    std::fs::write(path, raw)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

/// POST `event` as JSON to every URL in `webhooks.json` (best-effort; logs like the gateway loop).
pub async fn deliver_cron_completion_to_webhooks(
    webhooks_json: &Path,
    event: &CronCompletionEvent,
    client: &reqwest::Client,
) -> Result<(), AgentError> {
    let body =
        serde_json::to_value(event).map_err(|e| AgentError::Io(format!("webhook json: {e}")))?;

    let raw = tokio::fs::read_to_string(webhooks_json)
        .await
        .map_err(|e| AgentError::Io(format!("read {}: {}", webhooks_json.display(), e)))?;

    let store: WebhookStore = serde_json::from_str(&raw)
        .map_err(|e| AgentError::Io(format!("parse {}: {}", webhooks_json.display(), e)))?;

    if store.webhooks.is_empty() {
        tracing::debug!(
            "no webhooks in {}; skip HTTP delivery",
            webhooks_json.display()
        );
        return Ok(());
    }

    for w in store.webhooks {
        match client.post(&w.url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!(url = %w.url, id = %w.id, "cron completion webhook delivered");
            }
            Ok(resp) => {
                tracing::warn!(
                    url = %w.url,
                    id = %w.id,
                    status = %resp.status(),
                    "cron completion webhook non-success response"
                );
            }
            Err(e) => {
                tracing::warn!(
                    url = %w.url,
                    id = %w.id,
                    "cron completion webhook request failed: {e}"
                );
            }
        }
    }
    Ok(())
}

/// Default HTTP client for gateway cron webhook sidecar.
pub fn webhook_http_client() -> Result<reqwest::Client, AgentError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|e| AgentError::Io(format!("webhook HTTP client: {e}")))
}
