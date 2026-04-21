use hermes_config::GatewayConfig;
use serde_json::Value;

/// Mirror selected config.yaml values into process environment variables.
///
/// Existing environment variables are never overwritten.
/// Returns number of variables set from configuration.
pub fn hydrate_env_from_config(config: &GatewayConfig) -> usize {
    let mut applied = 0usize;

    applied += set_if_absent("HERMES_MODEL", config.model.as_deref());
    applied += set_if_absent("HERMES_PERSONALITY", config.personality.as_deref());
    applied += set_if_absent_owned("HERMES_MAX_TURNS", config.max_turns.to_string());
    applied += set_if_absent("HERMES_SYSTEM_PROMPT", config.system_prompt.as_deref());

    for (platform_name, platform_cfg) in &config.platforms {
        let platform_prefix = normalize_env_component(platform_name);
        let scoped_prefix = format!("HERMES_PLATFORM_{}", platform_prefix);

        let enabled = if platform_cfg.enabled {
            "true"
        } else {
            "false"
        };
        applied += set_if_absent_owned(format!("{}_ENABLED", scoped_prefix), enabled.to_string());
        applied += set_if_absent_owned(format!("{}_ENABLED", platform_prefix), enabled.to_string());

        applied += set_dual_if_absent(
            &platform_prefix,
            &scoped_prefix,
            "TOKEN",
            platform_cfg.token.as_deref(),
        );
        applied += set_dual_if_absent(
            &platform_prefix,
            &scoped_prefix,
            "WEBHOOK_URL",
            platform_cfg.webhook_url.as_deref(),
        );
        applied += set_dual_if_absent(
            &platform_prefix,
            &scoped_prefix,
            "HOME_CHANNEL",
            platform_cfg.home_channel.as_deref(),
        );

        if let Some(require_mention) = platform_cfg.require_mention {
            let value = if require_mention { "true" } else { "false" }.to_string();
            applied +=
                set_if_absent_owned(format!("{}_REQUIRE_MENTION", scoped_prefix), value.clone());
            applied += set_if_absent_owned(format!("{}_REQUIRE_MENTION", platform_prefix), value);
        }

        if !platform_cfg.allowed_users.is_empty() {
            let value = platform_cfg.allowed_users.join(",");
            applied +=
                set_if_absent_owned(format!("{}_ALLOWED_USERS", scoped_prefix), value.clone());
            applied += set_if_absent_owned(format!("{}_ALLOWED_USERS", platform_prefix), value);
        }
        if !platform_cfg.admin_users.is_empty() {
            let value = platform_cfg.admin_users.join(",");
            applied += set_if_absent_owned(format!("{}_ADMIN_USERS", scoped_prefix), value.clone());
            applied += set_if_absent_owned(format!("{}_ADMIN_USERS", platform_prefix), value);
        }

        for (extra_key, extra_val) in &platform_cfg.extra {
            if let Some(value) = scalarish_json_to_string(extra_val) {
                let key = normalize_env_component(extra_key);
                applied += set_if_absent_owned(format!("{}_{}", scoped_prefix, key), value.clone());
                applied += set_if_absent_owned(format!("{}_{}", platform_prefix, key), value);
            }
        }
    }

    for (provider_name, provider_cfg) in &config.llm_providers {
        let provider_prefix = format!("HERMES_PROVIDER_{}", normalize_env_component(provider_name));
        applied += set_if_absent_owned(
            format!("{}_MODEL", provider_prefix),
            provider_cfg.model.clone().unwrap_or_default(),
        );
        applied += set_if_absent_owned(
            format!("{}_BASE_URL", provider_prefix),
            provider_cfg.base_url.clone().unwrap_or_default(),
        );
        applied += set_if_absent_owned(
            format!("{}_API_KEY", provider_prefix),
            provider_cfg.api_key.clone().unwrap_or_default(),
        );
    }

    applied
}

fn set_dual_if_absent(
    base_prefix: &str,
    scoped_prefix: &str,
    suffix: &str,
    value: Option<&str>,
) -> usize {
    match value {
        Some(v) if !v.is_empty() => {
            let mut count = 0usize;
            count += set_if_absent_owned(format!("{}_{}", scoped_prefix, suffix), v.to_string());
            count += set_if_absent_owned(format!("{}_{}", base_prefix, suffix), v.to_string());
            count
        }
        _ => 0,
    }
}

fn set_if_absent(key: &str, value: Option<&str>) -> usize {
    match value {
        Some(v) if !v.is_empty() => set_if_absent_owned(key.to_string(), v.to_string()),
        _ => 0,
    }
}

fn set_if_absent_owned<K: Into<String>>(key: K, value: String) -> usize {
    let key = key.into();
    if value.is_empty() || std::env::var_os(&key).is_some() {
        return 0;
    }
    std::env::set_var(key, value);
    1
}

fn normalize_env_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

fn scalarish_json_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(if *b { "true" } else { "false" }.to_string()),
        Value::Array(arr) => {
            let mut parts = Vec::new();
            for entry in arr {
                match entry {
                    Value::String(s) => parts.push(s.clone()),
                    Value::Number(n) => parts.push(n.to_string()),
                    Value::Bool(b) => parts.push(if *b { "true" } else { "false" }.to_string()),
                    _ => return None,
                }
            }
            Some(parts.join(","))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::{GatewayConfig, PlatformConfig};
    use std::collections::HashMap;
    use std::sync::Mutex;

    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        pairs: Vec<(String, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        fn capture(keys: &[&str]) -> Self {
            let pairs = keys
                .iter()
                .map(|k| (k.to_string(), std::env::var_os(k)))
                .collect();
            Self { pairs }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, original) in &self.pairs {
                match original {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn hydrate_env_sets_platform_and_top_level_vars() {
        let _lock = ENV_TEST_LOCK.lock().expect("env test lock");
        let _guard = EnvGuard::capture(&[
            "HERMES_MODEL",
            "HERMES_MAX_TURNS",
            "DISCORD_ALLOWED_USERS",
            "HERMES_PLATFORM_DISCORD_ALLOWED_USERS",
            "DISCORD_CUSTOM_FLAG",
        ]);
        std::env::remove_var("HERMES_MODEL");
        std::env::remove_var("HERMES_MAX_TURNS");
        std::env::remove_var("DISCORD_ALLOWED_USERS");
        std::env::remove_var("HERMES_PLATFORM_DISCORD_ALLOWED_USERS");
        std::env::remove_var("DISCORD_CUSTOM_FLAG");

        let mut cfg = GatewayConfig {
            model: Some("openai:gpt-4o".to_string()),
            max_turns: 77,
            ..GatewayConfig::default()
        };
        let mut discord = PlatformConfig {
            enabled: true,
            allowed_users: vec!["123".into(), "456".into()],
            ..PlatformConfig::default()
        };
        discord.extra.insert(
            "custom-flag".to_string(),
            Value::String("enabled".to_string()),
        );
        cfg.platforms.insert("discord".to_string(), discord);

        let applied = hydrate_env_from_config(&cfg);
        assert!(applied > 0);
        assert_eq!(
            std::env::var("HERMES_MODEL").unwrap(),
            "openai:gpt-4o".to_string()
        );
        assert_eq!(std::env::var("HERMES_MAX_TURNS").unwrap(), "77".to_string());
        assert_eq!(
            std::env::var("DISCORD_ALLOWED_USERS").unwrap(),
            "123,456".to_string()
        );
        assert_eq!(
            std::env::var("HERMES_PLATFORM_DISCORD_ALLOWED_USERS").unwrap(),
            "123,456".to_string()
        );
        assert_eq!(
            std::env::var("DISCORD_CUSTOM_FLAG").unwrap(),
            "enabled".to_string()
        );
    }

    #[test]
    fn hydrate_env_never_overwrites_existing_values() {
        let _lock = ENV_TEST_LOCK.lock().expect("env test lock");
        let _guard = EnvGuard::capture(&["HERMES_MODEL", "DISCORD_ALLOWED_USERS"]);
        std::env::set_var("HERMES_MODEL", "existing:model");
        std::env::set_var("DISCORD_ALLOWED_USERS", "from-env");

        let mut cfg = GatewayConfig {
            model: Some("openai:gpt-4o".to_string()),
            ..GatewayConfig::default()
        };
        let discord = PlatformConfig {
            allowed_users: vec!["123".into()],
            ..PlatformConfig::default()
        };
        let mut platforms = HashMap::new();
        platforms.insert("discord".to_string(), discord);
        cfg.platforms = platforms;

        let _ = hydrate_env_from_config(&cfg);

        assert_eq!(std::env::var("HERMES_MODEL").unwrap(), "existing:model");
        assert_eq!(std::env::var("DISCORD_ALLOWED_USERS").unwrap(), "from-env");
    }
}
