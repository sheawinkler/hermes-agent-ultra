//! Normalize Python Hermes `config.yaml` into shapes `GatewayConfig` can deserialize.
//!
//! Python 版常用：嵌套 `model:`、`toolsets:`、`agent.max_turns`、`session_reset`、`providers`、
//! 根级 `telegram` / `discord` / `weixin` 等平台块（而非 `platforms:` 下）等；
//! Rust 字段能直接对齐的保留，其余在归一化层映射；平台专有键经 [`crate::platform::PlatformConfig`]
//! 的 `#[serde(flatten)] extra` 保留。

use serde_yaml::{Mapping, Value};
use std::collections::HashSet;

fn key(s: &str) -> Value {
    Value::String(s.to_string())
}

fn as_str(v: &Value) -> Option<&str> {
    v.as_str()
}

fn as_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_i64().map(|i| i as u64))
}

fn scalar_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn normalized_base_url(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
}

fn canonical_custom_provider_name(provider: &str) -> String {
    provider
        .trim()
        .strip_prefix("custom:")
        .unwrap_or_else(|| provider.trim())
        .trim()
        .to_string()
}

/// Lift `agent.max_turns` to root `max_turns` when the latter is absent.
fn lift_agent_max_turns(map: &mut Mapping) {
    let max_key = key("max_turns");
    if map.contains_key(&max_key) {
        return;
    }
    let Some(Value::Mapping(agent)) = map.get(&key("agent")) else {
        return;
    };
    let Some(mt) = agent.get(&key("max_turns")) else {
        return;
    };
    map.insert(max_key, mt.clone());
}

/// `toolsets: [a, b]` → `tools: [a, b]` when root `tools` is absent or empty.
fn lift_toolsets_to_tools(map: &mut Mapping) {
    let tools_key = key("tools");
    let keep_existing = match map.get(&tools_key) {
        Some(Value::Sequence(s)) => !s.is_empty(),
        Some(Value::String(st)) => !st.is_empty(),
        _ => false,
    };
    if keep_existing {
        return;
    }
    let Some(Value::Sequence(ts)) = map.remove(&key("toolsets")) else {
        return;
    };
    let out: Vec<Value> = ts
        .iter()
        .filter_map(|v| v.as_str().map(|s| Value::String(s.to_string())))
        .collect();
    if !out.is_empty() {
        map.insert(tools_key, Value::Sequence(out));
    }
}

/// Python `model: { default, provider, base_url, ... }` → `model: "provider:default"` + `llm_providers`.
fn normalize_model_block(map: &mut Mapping) {
    let model_key = key("model");
    let Some(raw) = map.remove(&model_key) else {
        return;
    };

    match raw {
        Value::String(s) => {
            map.insert(model_key, Value::String(s));
        }
        Value::Mapping(m) => {
            let default = m
                .get(&key("default"))
                .and_then(as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let provider = m
                .get(&key("provider"))
                .and_then(as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let base_url = m.get(&key("base_url")).and_then(normalized_base_url);
            let api_key = m
                .get(&key("api_key"))
                .and_then(as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let api_key_env = m
                .get(&key("api_key_env"))
                .and_then(as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());

            let model_str = match (provider, default) {
                (Some(p), Some(d)) => format!("{}:{d}", canonical_custom_provider_name(p)),
                (None, Some(d)) => d.to_string(),
                (Some(p), None) => format!("{}:", canonical_custom_provider_name(p)),
                (None, None) => {
                    map.insert(model_key, Value::Mapping(m));
                    return;
                }
            };
            map.insert(model_key, Value::String(model_str));

            if let Some(p) = provider
                .filter(|_| base_url.is_some() || api_key.is_some() || api_key_env.is_some())
            {
                let provider_name = canonical_custom_provider_name(p);
                let llm_key = key("llm_providers");
                let mut llm = match map.get(&llm_key).cloned() {
                    Some(Value::Mapping(x)) => x,
                    _ => Mapping::new(),
                };
                let prov_entry = llm
                    .entry(Value::String(provider_name))
                    .or_insert_with(|| Value::Mapping(Mapping::new()));
                if let Value::Mapping(ref mut em) = prov_entry {
                    if let Some(bu) = base_url {
                        em.insert(key("base_url"), Value::String(bu));
                    }
                    if let Some(key_value) = api_key {
                        em.insert(key("api_key"), Value::String(key_value.to_string()));
                    }
                    if let Some(env_name) = api_key_env {
                        em.insert(key("api_key_env"), Value::String(env_name.to_string()));
                    }
                }
                map.insert(llm_key, Value::Mapping(llm));
            }
        }
        other => {
            map.insert(model_key, other);
        }
    }
}

/// `custom_providers: [{ name, base_url, ... }]` → merge into `llm_providers`.
fn merge_custom_providers_into_llm(map: &mut Mapping) {
    let Some(Value::Sequence(providers)) = map.remove(&key("custom_providers")) else {
        return;
    };
    if providers.is_empty() {
        return;
    }

    let llm_key = key("llm_providers");
    let mut llm = match map.get(&llm_key).cloned() {
        Some(Value::Mapping(x)) => x,
        _ => Mapping::new(),
    };

    for entry in providers {
        let Value::Mapping(src) = entry else {
            continue;
        };
        let Some(provider_name) = src
            .get(&key("name"))
            .and_then(as_str)
            .map(canonical_custom_provider_name)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let slot = llm
            .entry(Value::String(provider_name))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if let Value::Mapping(provider_cfg) = slot {
            for field in [
                "api_key",
                "api_key_env",
                "base_url",
                "command",
                "args",
                "model",
                "max_tokens",
                "temperature",
                "extra_body",
                "rate_limit",
                "credential_pool",
                "oauth_token_url",
                "oauth_client_id",
            ] {
                let field_key = key(field);
                let Some(value) = src.get(&field_key) else {
                    continue;
                };
                let normalized = if field == "base_url" {
                    normalized_base_url(value).map(Value::String)
                } else {
                    Some(value.clone())
                };
                if let Some(value) = normalized {
                    provider_cfg.insert(field_key, value);
                }
            }
        }
    }

    map.insert(llm_key, Value::Mapping(llm));
}

/// `providers: { openai: { api_key: ... } }` → merge into `llm_providers`.
fn merge_providers_into_llm(map: &mut Mapping) {
    let Some(Value::Mapping(providers)) = map.remove(&key("providers")) else {
        return;
    };
    if providers.is_empty() {
        return;
    }
    let llm_key = key("llm_providers");
    let mut llm = match map.get(&llm_key).cloned() {
        Some(Value::Mapping(x)) => x,
        _ => Mapping::new(),
    };
    for (pk, pv) in providers {
        let pname = match pk.as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let slot = llm
            .entry(Value::String(pname))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if let Value::Mapping(em) = slot {
            if let Value::Mapping(src) = pv {
                for (k, v) in src {
                    em.insert(k, v);
                }
            }
        }
    }
    map.insert(llm_key, Value::Mapping(llm));
}

fn merge_fallback_provider_metadata(map: &mut Mapping, provider: &str, entry: &Mapping) {
    let provider = provider.trim();
    if provider.is_empty() {
        return;
    }

    let llm_key = key("llm_providers");
    let mut llm = match map.get(&llm_key).cloned() {
        Some(Value::Mapping(x)) => x,
        _ => Mapping::new(),
    };
    let slot = llm
        .entry(Value::String(provider.to_string()))
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    if let Value::Mapping(provider_cfg) = slot {
        for field in [
            "api_key",
            "api_key_env",
            "base_url",
            "command",
            "args",
            "oauth_token_url",
            "oauth_client_id",
        ] {
            let field_key = key(field);
            let Some(value) = entry.get(&field_key) else {
                continue;
            };
            let normalized = if field == "base_url" {
                normalized_base_url(value).map(Value::String)
            } else {
                Some(value.clone())
            };
            if let Some(value) = normalized {
                provider_cfg.insert(field_key, value);
            }
        }
    }
    map.insert(llm_key, Value::Mapping(llm));
}

fn push_fallback_spec(spec: String, chain: &mut Vec<Value>, seen: &mut HashSet<String>) {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return;
    }
    let identity = trimmed.to_ascii_lowercase();
    if seen.insert(identity) {
        chain.push(Value::String(trimmed.to_string()));
    }
}

fn collect_fallback_entries(
    raw: Value,
    map: &mut Mapping,
    chain: &mut Vec<Value>,
    seen: &mut HashSet<String>,
) {
    match raw {
        Value::String(spec) => push_fallback_spec(spec, chain, seen),
        Value::Sequence(seq) => {
            for entry in seq {
                collect_fallback_entries(entry, map, chain, seen);
            }
        }
        Value::Mapping(entry) => {
            let provider = entry
                .get(&key("provider"))
                .and_then(as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let model = entry
                .get(&key("model"))
                .and_then(as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());

            let Some(model) = model else {
                return;
            };

            let spec = match provider {
                Some(provider) if !model.contains(':') => {
                    merge_fallback_provider_metadata(map, provider, &entry);
                    format!("{provider}:{model}")
                }
                Some(provider) => {
                    merge_fallback_provider_metadata(map, provider, &entry);
                    model.to_string()
                }
                None => model.to_string(),
            };
            push_fallback_spec(spec, chain, seen);
        }
        _ => {}
    }
}

/// Python supports `fallback_providers` plus legacy `fallback_model`; Rust uses
/// string model specs in `fallback_models`, so normalize and merge the effective
/// chain before deserializing.
fn normalize_fallback_chain(map: &mut Mapping) {
    let fallback_models_key = key("fallback_models");
    let mut chain: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    if let Some(existing) = map.remove(&fallback_models_key) {
        collect_fallback_entries(existing, map, &mut chain, &mut seen);
    }
    if let Some(providers) = map.remove(&key("fallback_providers")) {
        collect_fallback_entries(providers, map, &mut chain, &mut seen);
    }
    if let Some(legacy) = map.remove(&key("fallback_model")) {
        collect_fallback_entries(legacy, map, &mut chain, &mut seen);
    }

    if let Some(Value::String(first)) = chain.first().cloned() {
        map.insert(key("fallback_model"), Value::String(first));
    }
    if !chain.is_empty() {
        map.insert(fallback_models_key, Value::Sequence(chain));
    }
}

/// Python `session_reset: { mode, idle_minutes, at_hour }` → `session.reset_policy` (tagged enum shape).
fn normalize_session_reset(map: &mut Mapping) {
    let Some(Value::Mapping(sr)) = map.get(&key("session_reset")).cloned() else {
        return;
    };
    let mode = sr.get(&key("mode")).and_then(as_str).map(str::to_lowercase);
    let idle_minutes = sr.get(&key("idle_minutes")).and_then(as_u64);
    let at_hour = sr
        .get(&key("at_hour"))
        .and_then(as_u64)
        .map(|h| h.min(23) as u8);

    let reset_policy = match mode.as_deref() {
        Some("daily") => {
            let h = at_hour.unwrap_or(0);
            let mut m = Mapping::new();
            m.insert(key("type"), Value::String("daily".into()));
            m.insert(key("at_hour"), Value::Number(serde_yaml::Number::from(h)));
            Value::Mapping(m)
        }
        Some("idle") => {
            let tm = idle_minutes.unwrap_or(30);
            let mut m = Mapping::new();
            m.insert(key("type"), Value::String("idle".into()));
            m.insert(
                key("timeout_minutes"),
                Value::Number(serde_yaml::Number::from(tm)),
            );
            Value::Mapping(m)
        }
        Some("both") => {
            let mut daily = Mapping::new();
            daily.insert(
                key("at_hour"),
                Value::Number(serde_yaml::Number::from(at_hour.unwrap_or(0))),
            );
            let mut idle = Mapping::new();
            idle.insert(
                key("timeout_minutes"),
                Value::Number(serde_yaml::Number::from(idle_minutes.unwrap_or(30))),
            );
            let mut m = Mapping::new();
            m.insert(key("type"), Value::String("both".into()));
            m.insert(key("daily"), Value::Mapping(daily));
            m.insert(key("idle"), Value::Mapping(idle));
            Value::Mapping(m)
        }
        Some("none") => {
            let mut m = Mapping::new();
            m.insert(key("type"), Value::String("none".into()));
            Value::Mapping(m)
        }
        _ => return,
    };

    map.remove(&key("session_reset"));
    let session_key = key("session");
    let mut session = match map.get(&session_key).cloned() {
        Some(Value::Mapping(x)) => x,
        _ => Mapping::new(),
    };
    session.insert(key("reset_policy"), reset_policy);
    map.insert(session_key, Value::Mapping(session));
}

/// Normalize `platform_toolsets` entries so mixed scalar YAML values (e.g.
/// bare numbers like `12306`) deserialize into `Vec<String>` safely.
fn normalize_platform_toolsets(map: &mut Mapping) {
    let ptk = key("platform_toolsets");
    let Some(Value::Mapping(existing)) = map.remove(&ptk) else {
        return;
    };

    let mut normalized = Mapping::new();
    for (platform, raw_values) in existing {
        let Some(platform_name) = platform.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };

        let mut out = Vec::new();
        match raw_values {
            Value::Sequence(seq) => {
                for entry in seq {
                    if let Some(s) = scalar_to_string(&entry) {
                        let trimmed = s.trim();
                        if !trimmed.is_empty() {
                            out.push(Value::String(trimmed.to_string()));
                        }
                    }
                }
            }
            other => {
                if let Some(s) = scalar_to_string(&other) {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() {
                        out.push(Value::String(trimmed.to_string()));
                    }
                }
            }
        }

        normalized.insert(key(platform_name), Value::Sequence(out));
    }

    map.insert(ptk, Value::Mapping(normalized));
}

/// Python often declares `telegram:` / `discord:` at the **root** instead of under `platforms:`.
fn lift_root_platform_blocks(map: &mut Mapping) {
    const NAMES: &[&str] = &[
        "telegram",
        "discord",
        "slack",
        "whatsapp",
        "signal",
        "weixin",
        "matrix",
        "mattermost",
        "dingtalk",
        "feishu",
        "wecom",
        "email",
        "sms",
        "homeassistant",
        "bluebubbles",
    ];

    let plat_key = key("platforms");
    let mut platforms = match map.get(&plat_key).cloned() {
        Some(Value::Mapping(x)) => x,
        _ => Mapping::new(),
    };

    for name in NAMES {
        let nk = key(name);
        if !map.contains_key(&nk) {
            continue;
        }
        if platforms.contains_key(&nk) {
            continue;
        }
        let Some(block) = map.remove(&nk) else {
            continue;
        };
        platforms.insert(nk, block);
    }

    if !platforms.is_empty() {
        map.insert(plat_key, Value::Mapping(platforms));
    }
}

/// Apply in-place transforms so Python-style Hermes YAML deserializes into [`crate::config::GatewayConfig`].
pub(crate) fn normalize_config_yaml_root(map: &mut Mapping) {
    // Order matters: model before merge_providers (may touch llm_providers)
    normalize_model_block(map);
    merge_custom_providers_into_llm(map);
    merge_providers_into_llm(map);
    normalize_fallback_chain(map);
    lift_agent_max_turns(map);
    lift_toolsets_to_tools(map);
    normalize_platform_toolsets(map);
    normalize_session_reset(map);
    lift_root_platform_blocks(map);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_model_block_to_string_and_llm() {
        let raw = r#"
model:
  default: z-ai/glm-5.1
  provider: openrouter
  base_url: https://openrouter.ai/api/v1
max_turns: 99
"#;
        let mut root: Value = serde_yaml::from_str(raw).unwrap();
        let Value::Mapping(ref mut m) = root else {
            panic!();
        };
        normalize_config_yaml_root(m);
        let cfg: crate::config::GatewayConfig = serde_yaml::from_value(root).unwrap();
        assert_eq!(cfg.model.as_deref(), Some("openrouter:z-ai/glm-5.1"));
        assert_eq!(cfg.max_turns, 99);
        let or = cfg.llm_providers.get("openrouter").expect("openrouter");
        assert_eq!(or.base_url.as_deref(), Some("https://openrouter.ai/api/v1"));
    }

    #[test]
    fn python_model_block_lifts_provider_credentials() {
        let raw = r#"
model:
  default: deepseek-chat
  provider: deepseek
  base_url: https://api.deepseek.com/
  api_key: sk-local
  api_key_env: DEEPSEEK_API_KEY
"#;
        let mut root: Value = serde_yaml::from_str(raw).unwrap();
        let Value::Mapping(ref mut m) = root else {
            panic!();
        };
        normalize_config_yaml_root(m);
        let cfg: crate::config::GatewayConfig = serde_yaml::from_value(root).unwrap();

        assert_eq!(cfg.model.as_deref(), Some("deepseek:deepseek-chat"));
        let provider = cfg.llm_providers.get("deepseek").expect("deepseek");
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://api.deepseek.com")
        );
        assert_eq!(provider.api_key.as_deref(), Some("sk-local"));
        assert_eq!(provider.api_key_env.as_deref(), Some("DEEPSEEK_API_KEY"));
    }

    #[test]
    fn python_custom_providers_merge_into_llm_provider_config() {
        let raw = r#"
model:
  default: my-model
  provider: custom:beans
custom_providers:
  - name: beans
    base_url: http://beans.local/v1/
    api_key: sk-beans
    api_key_env: BEANS_API_KEY
    model: fallback-beans-model
  - name: local
    base_url: http://localhost:8080/v1/
"#;
        let mut root: Value = serde_yaml::from_str(raw).unwrap();
        let Value::Mapping(ref mut m) = root else {
            panic!();
        };
        normalize_config_yaml_root(m);
        let cfg: crate::config::GatewayConfig = serde_yaml::from_value(root).unwrap();

        assert_eq!(cfg.model.as_deref(), Some("beans:my-model"));
        let beans = cfg.llm_providers.get("beans").expect("beans provider");
        assert_eq!(beans.base_url.as_deref(), Some("http://beans.local/v1"));
        assert_eq!(beans.api_key.as_deref(), Some("sk-beans"));
        assert_eq!(beans.api_key_env.as_deref(), Some("BEANS_API_KEY"));
        assert_eq!(beans.model.as_deref(), Some("fallback-beans-model"));

        let local = cfg.llm_providers.get("local").expect("local provider");
        assert_eq!(local.base_url.as_deref(), Some("http://localhost:8080/v1"));
        assert!(local.api_key.is_none());
    }

    #[test]
    fn toolsets_lifted_to_tools() {
        let raw = r#"
toolsets:
  - hermes-cli
  - web
"#;
        let mut root: Value = serde_yaml::from_str(raw).unwrap();
        let Value::Mapping(ref mut m) = root else {
            panic!();
        };
        normalize_config_yaml_root(m);
        let cfg: crate::config::GatewayConfig = serde_yaml::from_value(root).unwrap();
        assert_eq!(cfg.tools, vec!["hermes-cli", "web"]);
    }

    #[test]
    fn root_discord_lifted_under_platforms() {
        let raw = r#"
discord:
  require_mention: true
  free_response_channels: ""
  auto_thread: true
"#;
        let mut root: Value = serde_yaml::from_str(raw).unwrap();
        let Value::Mapping(ref mut m) = root else {
            panic!();
        };
        normalize_config_yaml_root(m);
        let cfg: crate::config::GatewayConfig = serde_yaml::from_value(root).unwrap();
        let d = cfg.platforms.get("discord").expect("discord");
        assert_eq!(d.require_mention, Some(true));
        assert!(d.extra.contains_key("free_response_channels"));
        assert!(d.extra.contains_key("auto_thread"));
    }

    #[test]
    fn session_reset_both_to_session() {
        let raw = r#"
session_reset:
  mode: both
  idle_minutes: 1440
  at_hour: 4
"#;
        let mut root: Value = serde_yaml::from_str(raw).unwrap();
        let Value::Mapping(ref mut m) = root else {
            panic!();
        };
        normalize_config_yaml_root(m);
        let cfg: crate::config::GatewayConfig = serde_yaml::from_value(root).unwrap();
        match cfg.session.reset_policy {
            crate::session::SessionResetPolicy::Both { daily, idle } => {
                assert_eq!(daily.at_hour, 4);
                assert_eq!(idle.timeout_minutes, 1440);
            }
            other => panic!("expected Both, got {:?}", other),
        }
    }

    #[test]
    fn platform_toolsets_numeric_entries_normalized_to_strings() {
        let raw = r#"
platform_toolsets:
  cli:
    - hermes-cli
    - 12306
    - true
  cron: 42
"#;
        let mut root: Value = serde_yaml::from_str(raw).unwrap();
        let Value::Mapping(ref mut m) = root else {
            panic!();
        };
        normalize_config_yaml_root(m);
        let cfg: crate::config::GatewayConfig = serde_yaml::from_value(root).unwrap();

        let cli = cfg.platform_toolsets.get("cli").expect("cli");
        assert!(cli.contains(&"hermes-cli".to_string()));
        assert!(cli.contains(&"12306".to_string()));
        assert!(cli.contains(&"true".to_string()));

        let cron = cfg.platform_toolsets.get("cron").expect("cron");
        assert_eq!(cron, &vec!["42".to_string()]);
    }

    #[test]
    fn fallback_providers_and_legacy_model_are_merged() {
        let raw = r#"
fallback_providers:
  - provider: openrouter
    model: anthropic/claude-sonnet-4.6
    base_url: https://openrouter.ai/api/v1/
  - provider: openrouter
    model: anthropic/claude-sonnet-4.6
fallback_model:
  provider: nous
  model: Hermes-4
  api_key_env: NOUS_API_KEY
"#;
        let mut root: Value = serde_yaml::from_str(raw).unwrap();
        let Value::Mapping(ref mut m) = root else {
            panic!();
        };
        normalize_config_yaml_root(m);
        let cfg: crate::config::GatewayConfig = serde_yaml::from_value(root).unwrap();

        assert_eq!(
            cfg.fallback_models,
            vec![
                "openrouter:anthropic/claude-sonnet-4.6".to_string(),
                "nous:Hermes-4".to_string()
            ]
        );
        assert_eq!(
            cfg.fallback_model.as_deref(),
            Some("openrouter:anthropic/claude-sonnet-4.6")
        );
        assert_eq!(
            cfg.llm_providers
                .get("openrouter")
                .and_then(|p| p.base_url.as_deref()),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(
            cfg.llm_providers
                .get("nous")
                .and_then(|p| p.api_key_env.as_deref()),
            Some("NOUS_API_KEY")
        );
    }
}
