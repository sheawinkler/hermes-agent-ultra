//! Read-only authentication and provider gate snapshots.
//!
//! `/auth verify` and `/auth refresh` may hydrate or rotate runtime
//! credentials. This tool intentionally exposes only the non-mutating status
//! and gate evidence agents need before asking the operator to run those flows.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use hermes_core::auth_gate::{
    load_oauth_runtime_gate_manifest_from_path, oauth_runtime_gate_for_provider,
    oauth_runtime_gate_manifest_default, OAuthRuntimeGateManifest,
};
use hermes_core::providers::{canonical_provider_id, known_providers, provider_capability_for};
use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};
use indexmap::IndexMap;
use serde_json::{json, Value};

const TOOL_NAME: &str = "auth_snapshot";

pub struct AuthSnapshotHandler;

#[async_trait]
impl ToolHandler for AuthSnapshotHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("status")
            .trim()
            .to_ascii_lowercase();
        let provider = params
            .get("provider")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let model = params
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let include_known = params
            .get("include_known")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let payload = match action.as_str() {
            "status" => auth_status_snapshot(provider, model, include_known),
            "providers" => json!({
                "status": "ok",
                "providers": provider_status_rows(),
                "secret_values_emitted": false,
            }),
            "gate" => {
                let resolved = resolve_auth_provider(provider, model);
                json!({
                    "status": "ok",
                    "provider": resolved,
                    "oauth": oauth_gate_snapshot(&resolved),
                })
            }
            "help" => json!({
                "status": "ok",
                "tool": TOOL_NAME,
                "actions": ["status", "providers", "gate", "help"],
                "notes": [
                    "read-only auth/provider snapshot",
                    "reports credential presence only; secret values are never emitted",
                    "does not refresh, mint, write, delete, or import credentials"
                ],
            }),
            _ => {
                return Err(ToolError::InvalidParams(format!(
                    "unknown action '{action}'; expected status|providers|gate|help"
                )));
            }
        };

        Ok(payload.to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": ["status", "providers", "gate", "help"],
                "description": "Snapshot action. Defaults to status."
            }),
        );
        props.insert(
            "provider".into(),
            json!({
                "type": "string",
                "description": "Optional provider override. Defaults to auth env/config provider resolution."
            }),
        );
        props.insert(
            "model".into(),
            json!({
                "type": "string",
                "description": "Optional provider:model hint used when provider is omitted."
            }),
        );
        props.insert(
            "include_known".into(),
            json!({
                "type": "boolean",
                "description": "Include all known provider capability rows in status output. Defaults to false."
            }),
        );
        tool_schema(
            TOOL_NAME,
            "Return read-only auth/provider credential presence and OAuth runtime gate diagnostics.",
            JsonSchema::object(props, vec![]),
        )
    }
}

pub fn auth_status_snapshot(
    provider: Option<&str>,
    model: Option<&str>,
    include_known: bool,
) -> Value {
    let resolved = resolve_auth_provider(provider, model);
    let credential = credential_snapshot(&resolved);
    let auth_store = auth_state_presence_snapshot(&resolved);
    json!({
        "status": "ok",
        "provider": resolved,
        "model_hint": effective_model_hint(model),
        "credential": credential,
        "auth_store": auth_store,
        "oauth": oauth_gate_snapshot(&resolved),
        "known_providers": if include_known { Some(provider_status_rows()) } else { None },
        "secret_values_emitted": false,
        "mutable": false,
        "next": {
            "passive_check": "/auth verify",
            "forced_refresh": "/auth refresh",
            "note": "run CLI auth commands for credential hydration or refresh; this tool is read-only"
        },
    })
}

fn provider_status_rows() -> Vec<Value> {
    let mut seen = BTreeSet::new();
    known_providers()
        .into_iter()
        .filter_map(|provider| {
            let cap = provider_capability_for(provider)?;
            if !seen.insert(cap.id.clone()) {
                return None;
            }
            Some(json!({
                "provider": cap.id,
                "oauth_supported": cap.oauth_supported,
                "models_dev_merged": cap.models_dev_merged,
                "managed_tools_supported": cap.managed_tools_supported,
                "credential_present": credential_snapshot(provider)["present"].as_bool().unwrap_or(false),
                "oauth_gate": oauth_gate_snapshot(provider),
            }))
        })
        .collect()
}

fn resolve_auth_provider(provider: Option<&str>, model: Option<&str>) -> String {
    if let Some(provider) = provider {
        return normalize_auth_provider(provider);
    }

    if let Some(pool_provider) = env_nonempty("HERMES_AUTH_PROVIDER_POOL").and_then(|pool| {
        pool.split(',')
            .map(str::trim)
            .find(|item| !item.is_empty())
            .map(ToOwned::to_owned)
    }) {
        return normalize_auth_provider(&pool_provider);
    }

    if let Some(default_provider) = env_nonempty("HERMES_AUTH_DEFAULT_PROVIDER") {
        return normalize_auth_provider(&default_provider);
    }

    if let Some(provider) = effective_model_hint(model).and_then(|hint| {
        hint.split_once(':')
            .map(|(provider, _)| provider.trim().to_string())
            .filter(|provider| !provider.is_empty())
    }) {
        return normalize_auth_provider(&provider);
    }

    "nous".to_string()
}

fn normalize_auth_provider(provider: &str) -> String {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai-oauth" | "openai-cli" => "openai".to_string(),
        "wechat" | "wx" => "weixin".to_string(),
        "qq" => "qqbot".to_string(),
        "tg" => "telegram".to_string(),
        other => canonical_provider_id(other),
    }
}

fn effective_model_hint(model: Option<&str>) -> Option<String> {
    model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| env_nonempty("HERMES_MODEL"))
        .or_else(read_config_model_hint)
}

fn read_config_model_hint() -> Option<String> {
    let path = hermes_config::config_path();
    let raw = std::fs::read_to_string(path).ok()?;
    let yaml = serde_yaml::from_str::<serde_yaml::Value>(&raw).ok()?;
    yaml.get("model")
        .and_then(serde_yaml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn credential_snapshot(provider: &str) -> Value {
    let sources = credential_env_vars(provider)
        .into_iter()
        .map(|name| json!({"env": name, "set": env_nonempty(name).is_some()}))
        .collect::<Vec<_>>();
    let present = sources
        .iter()
        .any(|source| source.get("set").and_then(Value::as_bool).unwrap_or(false))
        || bedrock_credential_present(provider);
    json!({
        "present": present,
        "sources": sources,
        "secret_values_emitted": false,
    })
}

pub fn auth_state_presence_snapshot(provider: &str) -> Value {
    let provider = normalize_auth_provider(provider);
    let mut candidates = auth_store_discovery_paths();
    candidates.dedup();

    let mut stores = Vec::new();
    let mut present = false;
    for path in candidates {
        let provider_present = read_provider_auth_state_from_store_path(&path, &provider);
        present |= provider_present;
        stores.push(json!({
            "path": path.display().to_string(),
            "exists": path.is_file(),
            "provider_present": provider_present,
        }));
    }

    json!({
        "present": present,
        "provider": provider,
        "stores": stores,
        "secret_values_emitted": false,
    })
}

fn auth_store_discovery_paths() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = env_nonempty("HERMES_AUTH_FILE") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(hermes_config::paths::auth_json_path());
    if let Some(home) = user_home_dir() {
        candidates.push(home.join(".hermes").join("auth.json"));
        candidates.push(home.join(".hermes-agent-ultra").join("auth.json"));
    }
    candidates
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::var("USERPROFILE").ok().map(PathBuf::from))
}

fn read_provider_auth_state_from_store_path(path: &Path, provider: &str) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return false;
    };
    parsed
        .get("providers")
        .and_then(Value::as_object)
        .and_then(|providers| providers.get(provider))
        .is_some()
}

fn credential_env_vars(provider: &str) -> Vec<&'static str> {
    match normalize_auth_provider(provider).as_str() {
        "openai" => vec!["HERMES_OPENAI_API_KEY", "OPENAI_API_KEY"],
        "openai-codex" => vec!["HERMES_OPENAI_CODEX_API_KEY"],
        "anthropic" => vec![
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ],
        "google-gemini-cli" => vec![
            "HERMES_GEMINI_OAUTH_API_KEY",
            "GOOGLE_API_KEY",
            "GEMINI_API_KEY",
        ],
        "gemini" => vec!["GOOGLE_API_KEY", "GEMINI_API_KEY"],
        "openrouter" => vec!["OPENROUTER_API_KEY"],
        "qwen" => vec!["DASHSCOPE_API_KEY", "QWEN_API_KEY"],
        "qwen-oauth" => vec!["HERMES_QWEN_OAUTH_API_KEY", "DASHSCOPE_API_KEY"],
        "kimi" => vec![
            "KIMI_API_KEY",
            "KIMI_CODING_API_KEY",
            "MOONSHOT_API_KEY",
            "KIMI_CN_API_KEY",
        ],
        "minimax" => vec!["MINIMAX_API_KEY", "MINIMAX_CN_API_KEY"],
        "minimax-cn" => vec!["MINIMAX_CN_API_KEY", "MINIMAX_API_KEY"],
        "stepfun" => vec!["HERMES_STEPFUN_API_KEY", "STEPFUN_API_KEY"],
        "novita" => vec!["NOVITA_API_KEY"],
        "nous" => vec!["NOUS_API_KEY"],
        "copilot" => vec![
            "COPILOT_GITHUB_TOKEN",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GITHUB_COPILOT_TOKEN",
        ],
        "copilot-acp" => vec!["COPILOT_GITHUB_TOKEN", "GITHUB_COPILOT_TOKEN"],
        "ai-gateway" => vec!["AI_GATEWAY_API_KEY"],
        "arcee" => vec!["ARCEEAI_API_KEY", "ARCEE_API_KEY"],
        "deepseek" => vec!["DEEPSEEK_API_KEY"],
        "huggingface" => vec!["HF_TOKEN", "HUGGINGFACE_API_KEY"],
        "gmi" => vec!["GMI_API_KEY"],
        "kilocode" => vec!["KILOCODE_API_KEY"],
        "nvidia" => vec!["NVIDIA_API_KEY"],
        "ollama-cloud" => vec!["OLLAMA_API_KEY"],
        "ollama-local" => vec!["OLLAMA_LOCAL_API_KEY", "OLLAMA_API_KEY"],
        "llama-cpp" => vec!["LLAMA_CPP_API_KEY"],
        "vllm" => vec!["VLLM_API_KEY"],
        "mlx" => vec!["MLX_API_KEY"],
        "apple-ane" => vec!["APPLE_ANE_API_KEY"],
        "sglang" => vec!["SGLANG_API_KEY"],
        "tgi" => vec!["TGI_API_KEY", "HUGGINGFACE_API_KEY"],
        "opencode-go" => vec!["OPENCODE_GO_API_KEY"],
        "opencode-zen" => vec!["OPENCODE_ZEN_API_KEY"],
        "xai" => vec!["XAI_API_KEY"],
        "xiaomi" => vec!["XIAOMI_API_KEY"],
        "tencent-tokenhub" => vec!["TENCENT_TOKENHUB_API_KEY"],
        "zai" => vec!["ZAI_API_KEY", "ZHIPU_API_KEY"],
        "bedrock" => vec![
            "AWS_ACCESS_KEY_ID",
            "AWS_PROFILE",
            "AWS_WEB_IDENTITY_TOKEN_FILE",
            "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
            "AWS_CONTAINER_CREDENTIALS_FULL_URI",
        ],
        _ => Vec::new(),
    }
}

fn bedrock_credential_present(provider: &str) -> bool {
    normalize_auth_provider(provider) == "bedrock"
        && (env_nonempty("AWS_ACCESS_KEY_ID").is_some()
            || env_nonempty("AWS_PROFILE").is_some()
            || env_nonempty("AWS_WEB_IDENTITY_TOKEN_FILE").is_some()
            || env_nonempty("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI").is_some()
            || env_nonempty("AWS_CONTAINER_CREDENTIALS_FULL_URI").is_some())
}

fn oauth_gate_snapshot(provider: &str) -> Value {
    let Some(cap) = provider_capability_for(provider) else {
        return json!({
            "supported": false,
            "gate": null,
            "reason": "unknown_provider",
        });
    };
    if !cap.oauth_supported {
        return json!({
            "supported": false,
            "gate": null,
            "reason": "provider_not_oauth_capable",
        });
    }
    let (manifest, source) = load_oauth_gate_manifest();
    match oauth_runtime_gate_for_provider(provider, env!("CARGO_PKG_VERSION"), &manifest, &source) {
        Some(gate) => json!({
            "supported": true,
            "gate": gate,
        }),
        None => json!({
            "supported": false,
            "gate": null,
            "reason": "provider_not_oauth_capable",
        }),
    }
}

fn load_oauth_gate_manifest() -> (OAuthRuntimeGateManifest, String) {
    if let Some(path) = oauth_gate_manifest_path() {
        if let Some(manifest) = load_oauth_runtime_gate_manifest_from_path(&path) {
            return (manifest, path.display().to_string());
        }
    }
    (
        oauth_runtime_gate_manifest_default(),
        "builtin-default".to_string(),
    )
}

fn oauth_gate_manifest_path() -> Option<PathBuf> {
    env_nonempty("HERMES_OAUTH_GATE_MANIFEST_PATH")
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .or_else(|| {
            let path = hermes_config::hermes_home().join("oauth-gate-manifest.json");
            path.exists().then_some(path)
        })
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use tempfile::tempdir;

    use super::*;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, old }
        }

        fn remove(key: &'static str) -> Self {
            let old = std::env::var(key).ok();
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(value) = &self.old {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[test]
    fn status_reports_credential_presence_without_secret_value() {
        let _lock = env_lock();
        let _provider = EnvGuard::set("HERMES_AUTH_DEFAULT_PROVIDER", "openrouter");
        let _key = EnvGuard::set("OPENROUTER_API_KEY", "sk-secret-should-not-appear");
        let _pool = EnvGuard::remove("HERMES_AUTH_PROVIDER_POOL");

        let raw = auth_status_snapshot(None, None, false).to_string();
        let payload: Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(payload["provider"], "openrouter");
        assert_eq!(payload["credential"]["present"], true);
        assert_eq!(payload["secret_values_emitted"], false);
        assert!(!raw.contains("sk-secret-should-not-appear"));
    }

    #[test]
    fn model_hint_infers_provider_when_default_is_absent() {
        let _lock = env_lock();
        let _provider = EnvGuard::remove("HERMES_AUTH_DEFAULT_PROVIDER");
        let _pool = EnvGuard::remove("HERMES_AUTH_PROVIDER_POOL");
        let _model = EnvGuard::set("HERMES_MODEL", "anthropic:claude-sonnet-4");

        let payload = auth_status_snapshot(None, None, false);
        assert_eq!(payload["provider"], "anthropic");
        assert_eq!(payload["oauth"]["supported"], true);
    }

    #[test]
    fn gate_manifest_override_blocks_old_runtime() {
        let _lock = env_lock();
        let temp = tempdir().expect("tempdir");
        let manifest = temp.path().join("oauth-manifest.json");
        std::fs::write(
            &manifest,
            r#"{
  "default_min_version": "99.0.0",
  "required_oauth_provider_ids": ["nous"],
  "provider_min_versions": { "nous": "99.0.0" }
}"#,
        )
        .expect("write manifest");
        let _manifest = EnvGuard::set(
            "HERMES_OAUTH_GATE_MANIFEST_PATH",
            manifest.to_str().expect("utf8 path"),
        );

        let oauth = oauth_gate_snapshot("nous");
        assert_eq!(oauth["supported"], true);
        assert_eq!(oauth["gate"]["ok"], false);
        assert!(oauth["gate"]["detail"]
            .as_str()
            .unwrap()
            .contains("required>=99.0.0"));
    }

    #[test]
    fn status_reports_oauth_state_presence_without_secret_value() {
        let _lock = env_lock();
        let temp = tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", temp.path().to_string_lossy().as_ref());
        let _provider = EnvGuard::set("HERMES_AUTH_DEFAULT_PROVIDER", "nous");
        let _pool = EnvGuard::remove("HERMES_AUTH_PROVIDER_POOL");
        std::fs::write(
            temp.path().join("auth.json"),
            r#"{
  "providers": {
    "nous": {
      "access_token": "secret-access-token",
      "refresh_token": "secret-refresh-token"
    }
  }
}"#,
        )
        .expect("write auth store");

        let raw = auth_status_snapshot(None, None, false).to_string();
        let payload: Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(payload["provider"], "nous");
        assert_eq!(payload["auth_store"]["present"], true);
        assert_eq!(payload["auth_store"]["secret_values_emitted"], false);
        assert!(!raw.contains("secret-access-token"));
        assert!(!raw.contains("secret-refresh-token"));
    }
}
