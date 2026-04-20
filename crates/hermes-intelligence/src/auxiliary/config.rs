//! Per-task auxiliary configuration.
//!
//! Resolution priority (matches the Python `_resolve_task_provider_model`):
//!
//! 1. Explicit fields on [`AuxiliaryRequest`](super::client::AuxiliaryRequest)
//! 2. Env var overrides — `AUXILIARY_{TASK}_PROVIDER`, `..._MODEL`,
//!    `..._BASE_URL`, `..._API_KEY`, `..._TIMEOUT`
//! 3. Loaded [`AuxiliaryConfig`] (typically materialised from the user's
//!    `config.yaml` `auxiliary.{task}.*` section)
//! 4. Task defaults from [`super::task::AuxiliaryTask`]

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::task::AuxiliaryTask;

/// Per-task overrides loaded from a configuration file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Per-task timeout in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// Top-level configuration consumed by [`super::client::AuxiliaryClient`].
///
/// Typically populated from the user's `config.yaml` `auxiliary` section but
/// can also be assembled programmatically.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuxiliaryConfig {
    /// Per-task overrides keyed by [`AuxiliaryTask::as_key`].
    #[serde(default)]
    pub tasks: HashMap<String, TaskOverride>,
    /// Whether to consult environment variables. Useful to disable in tests.
    #[serde(default = "default_true")]
    pub consult_env: bool,
}

fn default_true() -> bool {
    true
}

impl AuxiliaryConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert / replace overrides for a specific task.
    pub fn set_task(&mut self, task: &AuxiliaryTask, override_: TaskOverride) {
        self.tasks.insert(task.as_key().to_string(), override_);
    }

    pub fn task_override(&self, task: &AuxiliaryTask) -> Option<&TaskOverride> {
        self.tasks.get(task.as_key())
    }
}

/// Output of [`resolve_task_settings`] — the fully merged routing intent for a
/// single auxiliary call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTaskSettings {
    /// `"auto"` if the auto-detection chain should be walked.
    /// Otherwise an explicit provider label (`"openrouter"`, `"anthropic"`,
    /// `"custom"`, `"zai"`, ...).
    pub provider: String,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub timeout: Duration,
}

/// Resolve the effective routing settings for a single call.
///
/// `explicit` carries any per-call overrides (from `AuxiliaryRequest`); they
/// always win over env / config / defaults.
pub fn resolve_task_settings(
    task: &AuxiliaryTask,
    explicit: &ExplicitOverrides,
    config: &AuxiliaryConfig,
) -> ResolvedTaskSettings {
    let env_lookup = |suffix: &str| -> Option<String> {
        if !config.consult_env {
            return None;
        }
        let key = format!("AUXILIARY_{}_{}", task.env_suffix(), suffix);
        std::env::var(&key)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|s| !s.is_empty())
    };

    let cfg_override = config.task_override(task);

    // model: explicit > env > config > task default (None)
    let model = explicit
        .model
        .clone()
        .or_else(|| env_lookup("MODEL"))
        .or_else(|| cfg_override.and_then(|o| o.model.clone()));

    // base_url forces provider="custom".
    if let Some(base_url) = explicit
        .base_url
        .clone()
        .or_else(|| env_lookup("BASE_URL"))
        .or_else(|| cfg_override.and_then(|o| o.base_url.clone()))
    {
        let api_key = explicit
            .api_key
            .clone()
            .or_else(|| env_lookup("API_KEY"))
            .or_else(|| cfg_override.and_then(|o| o.api_key.clone()));
        return ResolvedTaskSettings {
            provider: "custom".into(),
            model,
            base_url: Some(base_url),
            api_key,
            timeout: resolve_timeout(task, explicit, cfg_override, &env_lookup),
        };
    }

    // explicit provider override
    let provider = explicit
        .provider
        .clone()
        .or_else(|| env_lookup("PROVIDER"))
        .or_else(|| cfg_override.and_then(|o| o.provider.clone()))
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "auto".to_string());

    ResolvedTaskSettings {
        provider,
        model,
        base_url: None,
        api_key: explicit.api_key.clone(),
        timeout: resolve_timeout(task, explicit, cfg_override, &env_lookup),
    }
}

fn resolve_timeout<F: Fn(&str) -> Option<String>>(
    task: &AuxiliaryTask,
    explicit: &ExplicitOverrides,
    cfg_override: Option<&TaskOverride>,
    env_lookup: &F,
) -> Duration {
    if let Some(d) = explicit.timeout {
        return d;
    }
    if let Some(secs) = env_lookup("TIMEOUT").and_then(|s| s.parse::<u64>().ok()) {
        return Duration::from_secs(secs);
    }
    if let Some(secs) = cfg_override.and_then(|o| o.timeout_secs) {
        return Duration::from_secs(secs);
    }
    task.default_timeout()
}

/// Per-call overrides that bypass env vars and config files.
#[derive(Debug, Clone, Default)]
pub struct ExplicitOverrides {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub timeout: Option<Duration>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated_env_resolve(
        task: AuxiliaryTask,
        explicit: ExplicitOverrides,
        cfg: AuxiliaryConfig,
    ) -> ResolvedTaskSettings {
        // Disable env consultation so tests stay hermetic regardless of host env.
        let mut cfg = cfg;
        cfg.consult_env = false;
        resolve_task_settings(&task, &explicit, &cfg)
    }

    #[test]
    fn explicit_wins_over_config() {
        let mut cfg = AuxiliaryConfig::new();
        cfg.set_task(
            &AuxiliaryTask::Compression,
            TaskOverride {
                provider: Some("nous".into()),
                model: Some("model-cfg".into()),
                ..Default::default()
            },
        );
        let r = isolated_env_resolve(
            AuxiliaryTask::Compression,
            ExplicitOverrides {
                provider: Some("openrouter".into()),
                model: Some("model-explicit".into()),
                ..Default::default()
            },
            cfg,
        );
        assert_eq!(r.provider, "openrouter");
        assert_eq!(r.model.as_deref(), Some("model-explicit"));
    }

    #[test]
    fn base_url_forces_custom() {
        let r = isolated_env_resolve(
            AuxiliaryTask::Vision,
            ExplicitOverrides {
                base_url: Some("https://example.com/v1".into()),
                api_key: Some("sk-abc".into()),
                ..Default::default()
            },
            AuxiliaryConfig::new(),
        );
        assert_eq!(r.provider, "custom");
        assert_eq!(r.base_url.as_deref(), Some("https://example.com/v1"));
        assert_eq!(r.api_key.as_deref(), Some("sk-abc"));
    }

    #[test]
    fn defaults_to_auto_when_nothing_set() {
        let r = isolated_env_resolve(
            AuxiliaryTask::Title,
            ExplicitOverrides::default(),
            AuxiliaryConfig::new(),
        );
        assert_eq!(r.provider, "auto");
        assert_eq!(r.model, None);
        assert_eq!(r.timeout, AuxiliaryTask::Title.default_timeout());
    }

    #[test]
    fn env_override_promoted_when_enabled() {
        let key = "AUXILIARY_TITLE_PROVIDER";
        std::env::set_var(key, "anthropic");
        let r = resolve_task_settings(
            &AuxiliaryTask::Title,
            &ExplicitOverrides::default(),
            &AuxiliaryConfig {
                consult_env: true,
                ..Default::default()
            },
        );
        std::env::remove_var(key);
        assert_eq!(r.provider, "anthropic");
    }

    #[test]
    fn config_timeout_used_when_explicit_absent() {
        let mut cfg = AuxiliaryConfig::new();
        cfg.set_task(
            &AuxiliaryTask::Compression,
            TaskOverride {
                timeout_secs: Some(120),
                ..Default::default()
            },
        );
        let r = isolated_env_resolve(
            AuxiliaryTask::Compression,
            ExplicitOverrides::default(),
            cfg,
        );
        assert_eq!(r.timeout, Duration::from_secs(120));
    }
}
