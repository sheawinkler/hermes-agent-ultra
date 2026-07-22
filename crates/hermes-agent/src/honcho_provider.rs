//! Subject-bound compatibility wrapper for the Honcho dialectic API.
//!
//! The registered Honcho provider lives in `memory_plugins::honcho`; this
//! public wrapper remains available for direct SDK use.  It never accepts a
//! caller-selected user, peer, or session identifier: all remote paths and
//! metadata derive from the authenticated `MemorySubjectStorageKey` bound at
//! initialization.

use std::sync::Arc;

use hermes_core::{
    AgentError, MemorySubject, MemorySubjectStorageKey, MemoryVisibility,
};
use hermes_intelligence::redact_sensitive_text;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::memory_manager::MemoryProviderPlugin;

/// Honcho API base URL.
const DEFAULT_HONCHO_BASE_URL: &str = "https://api.honcho.dev/v1";

/// Configuration for the Honcho memory provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HonchoConfig {
    /// Honcho API key.
    pub api_key: String,
    /// Honcho application ID.
    pub app_id: String,
    /// Optional base URL override.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Whether to auto-sync turns to Honcho.
    #[serde(default = "default_true")]
    pub auto_sync: bool,
    /// Whether to prefetch user context on session start.
    #[serde(default = "default_true")]
    pub prefetch: bool,
}

fn default_true() -> bool {
    true
}

fn visibility_label(visibility: MemoryVisibility) -> &'static str {
    match visibility {
        MemoryVisibility::Private => "private",
        MemoryVisibility::Team => "team",
    }
}

fn path_segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// Remote metadata containing only the opaque subject partition, never raw
/// authenticated identifiers.
fn subject_scope(subject: &MemorySubject, filter: &MemorySubjectStorageKey) -> serde_json::Value {
    let key = filter.as_str();
    json!({
        "subject_key": key,
        "tenant_id": key,
        "principal_id": key,
        "team_id": subject.team_id().map(|_| key),
        "session_id": key,
        "surface": subject.surface().as_str(),
        "visibility": visibility_label(subject.visibility()),
    })
}

fn subject_query(subject: &MemorySubject, filter: &MemorySubjectStorageKey) -> Vec<(&'static str, String)> {
    let key = filter.as_str().to_string();
    vec![
        ("subject_key", key.clone()),
        ("tenant_id", key.clone()),
        ("principal_id", key.clone()),
        ("team_id", subject.team_id().map(|_| key.clone()).unwrap_or_default()),
        ("session_id", key),
        ("surface", subject.surface().as_str().to_string()),
        ("visibility", visibility_label(subject.visibility()).to_string()),
    ]
}

/// Honcho dialectic user modeling provider.
///
/// Direct SDK methods require the same subject/filter pair as the plugin trait.
/// A caller cannot supply a free user or session id.
#[derive(Clone)]
pub struct HonchoProvider {
    config: HonchoConfig,
    client: reqwest::Client,
    base_url: String,
    /// Cached context is accessible only through a matching bound subject.
    cached_context: Arc<RwLock<Option<String>>>,
    bound_subject: Arc<Mutex<Option<MemorySubject>>>,
    bound_filter: Arc<Mutex<Option<MemorySubjectStorageKey>>>,
}

impl HonchoProvider {
    /// Create an unbound provider. Call `MemoryProviderPlugin::initialize`
    /// with the authenticated subject before making a request.
    pub fn new(config: HonchoConfig) -> Self {
        let base_url = config
            .base_url
            .clone()
            .unwrap_or_else(|| DEFAULT_HONCHO_BASE_URL.to_string())
            .trim_end_matches('/')
            .to_string();

        Self {
            config,
            client: reqwest::Client::new(),
            base_url,
            cached_context: Arc::new(RwLock::new(None)),
            bound_subject: Arc::new(Mutex::new(None)),
            bound_filter: Arc::new(Mutex::new(None)),
        }
    }

    fn require_bound(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
    ) -> Result<(), AgentError> {
        if self.bound_subject.lock().as_ref() != Some(subject)
            || self.bound_filter.lock().as_ref() != Some(filter)
        {
            return Err(AgentError::Io(
                "Honcho request subject does not match initialized subject".to_string(),
            ));
        }
        Ok(())
    }

    fn user_session_url(&self, filter: &MemorySubjectStorageKey) -> String {
        format!(
            "{}/apps/{}/users/{}/sessions/{}/messages",
            self.base_url,
            path_segment(&self.config.app_id),
            path_segment(filter.as_str()),
            path_segment(filter.as_str()),
        )
    }

    fn metamemories_url(&self, filter: &MemorySubjectStorageKey) -> String {
        format!(
            "{}/apps/{}/users/{}/metamemories",
            self.base_url,
            path_segment(&self.config.app_id),
            path_segment(filter.as_str()),
        )
    }

    /// Sync one turn to Honcho in the authenticated subject partition.
    pub async fn sync_message(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        role: &str,
        content: &str,
    ) -> Result<(), AgentError> {
        self.require_bound(subject, filter)?;
        if !self.config.auto_sync || content.trim().is_empty() {
            return Ok(());
        }

        let body = json!({
            "role": role,
            "content": redact_sensitive_text(content.trim()),
            "subject": subject_scope(subject, filter),
        });
        let response = self
            .client
            .post(self.user_session_url(filter))
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|error| AgentError::Io(format!("Honcho sync failed: {error}")))?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(AgentError::Io(format!(
                "Honcho sync returned {}",
                response.status()
            )))
        }
    }

    /// Prefetch subject-scoped dialectic context from Honcho.
    pub async fn prefetch_context(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
    ) -> Result<Option<String>, AgentError> {
        self.require_bound(subject, filter)?;
        if !self.config.prefetch {
            return Ok(None);
        }

        let response = self
            .client
            .get(self.metamemories_url(filter))
            .query(&subject_query(subject, filter))
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .send()
            .await
            .map_err(|error| AgentError::Io(format!("Honcho prefetch failed: {error}")))?;

        if !response.status().is_success() {
            return Err(AgentError::Io(format!(
                "Honcho prefetch returned {}",
                response.status()
            )));
        }

        #[derive(Deserialize)]
        struct MetamemoryResponse {
            #[serde(default)]
            content: Option<String>,
        }

        let data: MetamemoryResponse = response
            .json()
            .await
            .map_err(|error| AgentError::Io(format!("Honcho parse failed: {error}")))?;
        if let Some(content) = data.content.as_deref() {
            *self.cached_context.write() = Some(redact_sensitive_text(content));
        }
        Ok(data.content.map(|content| redact_sensitive_text(&content)))
    }

    /// Get cached context only when the caller presents the bound subject.
    pub fn cached_context(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
    ) -> Result<Option<String>, AgentError> {
        self.require_bound(subject, filter)?;
        Ok(self.cached_context.read().clone())
    }

    fn spawn_background<F>(&self, task: F)
    where
        F: FnOnce(HonchoProvider) + Send + 'static,
    {
        let provider = self.clone();
        std::thread::spawn(move || task(provider));
    }
}

impl MemoryProviderPlugin for HonchoProvider {
    fn name(&self) -> &str {
        "honcho"
    }

    fn is_available(&self) -> bool {
        !self.config.api_key.trim().is_empty()
    }

    fn initialize(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        _hermes_home: &str,
    ) -> Result<(), String> {
        *self.bound_subject.lock() = Some(subject.clone());
        *self.bound_filter.lock() = Some(filter.clone());
        *self.cached_context.write() = None;
        Ok(())
    }

    fn system_prompt_block(&self) -> String {
        if self.bound_filter.lock().is_some() {
            "# Honcho Memory\nActive only in the authenticated memory subject partition.".to_string()
        } else {
            String::new()
        }
    }

    fn prefetch(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        _query: &str,
    ) -> String {
        self.cached_context(subject, filter)
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    fn queue_prefetch(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        _query: &str,
    ) {
        if self.require_bound(subject, filter).is_err() || !self.config.prefetch {
            return;
        }
        let subject = subject.clone();
        let filter = filter.clone();
        self.spawn_background(move |provider| {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::warn!(error = %redact_sensitive_text(&error.to_string()), "unable to start Honcho prefetch runtime");
                    return;
                }
            };
            if let Err(error) = runtime.block_on(provider.prefetch_context(&subject, &filter)) {
                tracing::warn!(error = %redact_sensitive_text(&error.to_string()), "subject-scoped Honcho prefetch failed");
            }
        });
    }

    fn sync_turn(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        user_content: &str,
        assistant_content: &str,
    ) {
        if self.require_bound(subject, filter).is_err() || !self.config.auto_sync {
            return;
        }
        let user = redact_sensitive_text(user_content.trim());
        let assistant = redact_sensitive_text(assistant_content.trim());
        if user.is_empty() || assistant.is_empty() {
            return;
        }
        let subject = subject.clone();
        let filter = filter.clone();
        self.spawn_background(move |provider| {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::warn!(error = %redact_sensitive_text(&error.to_string()), "unable to start Honcho sync runtime");
                    return;
                }
            };
            for (role, content) in [("user", user), ("assistant", assistant)] {
                if let Err(error) = runtime.block_on(provider.sync_message(&subject, &filter, role, &content)) {
                    tracing::warn!(error = %redact_sensitive_text(&error.to_string()), "subject-scoped Honcho turn sync failed");
                    break;
                }
            }
        });
    }

    fn shutdown(&self) {
        *self.bound_subject.lock() = None;
        *self.bound_filter.lock() = None;
        *self.cached_context.write() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::{
        MemoryPrincipalId, MemorySessionId, MemorySubjectDerivationKey, MemorySurface,
        MemoryTenantId,
    };

    fn subject(principal: &str) -> (MemorySubject, MemorySubjectStorageKey) {
        let subject = MemorySubject::new(
            MemoryTenantId::new("tenant-a").expect("tenant"),
            MemoryPrincipalId::new(principal).expect("principal"),
            None,
            MemorySessionId::new("session-a").expect("session"),
            MemorySurface::Gateway,
            MemoryVisibility::Private,
        )
        .expect("subject");
        let filter = subject.storage_key(&MemorySubjectDerivationKey::from_bytes([11; 32]));
        (subject, filter)
    }

    fn config() -> HonchoConfig {
        HonchoConfig {
            api_key: "test-key".to_string(),
            app_id: "test-app".to_string(),
            base_url: None,
            auto_sync: true,
            prefetch: true,
        }
    }

    #[test]
    fn honcho_config_defaults() {
        let config: HonchoConfig = serde_json::from_str(
            r#"{
            "api_key": "test-key",
            "app_id": "test-app"
        }"#,
        )
        .expect("config");
        assert!(config.auto_sync);
        assert!(config.prefetch);
        assert!(config.base_url.is_none());
    }

    #[test]
    fn initialization_binds_the_opaque_subject_partition() {
        let (alice, alice_filter) = subject("alice");
        let (bob, bob_filter) = subject("bob");
        let provider = HonchoProvider::new(config());
        provider
            .initialize(&alice, &alice_filter, "/unused")
            .expect("initialize");

        assert!(provider.cached_context(&alice, &alice_filter).expect("alice").is_none());
        assert!(provider.cached_context(&bob, &bob_filter).is_err());
        assert_eq!(provider.prefetch(&bob, &alice_filter, "ignored"), "");
        assert_ne!(alice_filter, bob_filter);
    }

    #[test]
    fn subject_scope_never_contains_raw_authenticated_ids() {
        let (alice, filter) = subject("alice");
        let scope = subject_scope(&alice, &filter).to_string();
        assert!(scope.contains(filter.as_str()));
        assert!(!scope.contains("alice"));
        assert!(!scope.contains("tenant-a"));
    }

    #[test]
    fn custom_base_url_is_normalized() {
        let provider = HonchoProvider::new(HonchoConfig {
            base_url: Some("https://custom.honcho.dev/".to_string()),
            ..config()
        });
        assert_eq!(provider.base_url, "https://custom.honcho.dev");
    }
}
