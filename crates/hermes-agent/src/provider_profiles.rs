//! Rust-native request-profile contracts for OpenAI-compatible providers.
//!
//! Upstream Python keeps this behavior in provider plugin profiles. Hermes
//! Ultra runs the agent loop in Rust, so the request-shaping contracts live
//! here instead of importing Python provider plugins at runtime.

use serde_json::{Map, Value};

pub const NOUS_PRODUCT_TAG: &str = "product=hermes-agent";

pub fn canonical_provider_profile_id(provider: &str) -> Option<&'static str> {
    let normalized = provider.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    match normalized.as_str() {
        "nvidia" | "nvidia-nim" => Some("nvidia"),
        "kimi" | "moonshot" | "moonshot-ai" | "kimi-coding" => Some("kimi-coding"),
        "kimi-coding-cn" | "kimi-cn" | "moonshot-cn" => Some("kimi-coding-cn"),
        "openrouter" | "or" => Some("openrouter"),
        "nous" | "nous-portal" => Some("nous"),
        "qwen" | "qwen-oauth" | "qwen-portal" | "qwen-cli" => Some("qwen-oauth"),
        "xiaomi" | "mimo" | "xiaomi-mimo" => Some("xiaomi"),
        "custom" | "ollama" | "ollama-local" | "llama-cpp" | "vllm" | "mlx" | "apple-ane"
        | "sglang" | "tgi" => Some("custom"),
        _ => None,
    }
}

pub fn default_max_tokens(profile: &str) -> Option<u32> {
    match canonical_provider_profile_id(profile)? {
        "nvidia" => Some(16_384),
        "kimi-coding" | "kimi-coding-cn" => Some(32_000),
        "qwen-oauth" => Some(65_536),
        _ => None,
    }
}

pub fn omit_temperature(profile: &str) -> bool {
    matches!(
        canonical_provider_profile_id(profile),
        Some("kimi-coding" | "kimi-coding-cn")
    )
}

pub fn supports_vision(profile: &str) -> bool {
    matches!(
        canonical_provider_profile_id(profile),
        Some("anthropic" | "nous" | "openrouter" | "qwen-oauth" | "xiaomi")
    )
}

pub fn profile_auth_type(profile: &str) -> Option<&'static str> {
    match canonical_provider_profile_id(profile)? {
        "nous" => Some("oauth_device_code"),
        "qwen-oauth" => Some("oauth_external"),
        "nvidia" | "kimi-coding" | "kimi-coding-cn" | "openrouter" | "xiaomi" | "custom" => {
            Some("api_key")
        }
        _ => None,
    }
}

pub fn profile_base_url(profile: &str) -> Option<&'static str> {
    match canonical_provider_profile_id(profile)? {
        "nvidia" => Some("https://integrate.api.nvidia.com/v1"),
        "kimi-coding" => Some("https://api.moonshot.ai/v1"),
        "kimi-coding-cn" => Some("https://api.moonshot.cn/v1"),
        "openrouter" => Some("https://openrouter.ai/api/v1"),
        "nous" => Some("https://inference-api.nousresearch.com/v1"),
        "qwen-oauth" => Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1"),
        "xiaomi" => Some("https://api.xiaomimimo.com/v1"),
        _ => None,
    }
}

pub fn nous_portal_tags() -> Value {
    Value::Array(vec![Value::String(NOUS_PRODUCT_TAG.to_string())])
}

pub fn local_control_key_for_profile(profile: Option<&str>, key: &str) -> bool {
    if matches!(
        key,
        "provider_profile"
            | "supports_reasoning"
            | "reasoning_config"
            | "provider_preferences"
            | "openrouter_min_coding_score"
            | "supports_vision"
            | "session_id"
            | "qwen_session_metadata"
            | "ollama_num_ctx"
    ) {
        return true;
    }

    match profile.and_then(canonical_provider_profile_id) {
        Some("kimi-coding" | "kimi-coding-cn" | "nous" | "openrouter") => {
            matches!(key, "reasoning" | "reasoning_effort")
        }
        _ => false,
    }
}

pub fn clean_extra_body_for_profile(
    profile: Option<&str>,
    extra_body: Option<&Value>,
) -> Option<Value> {
    let Some(Value::Object(map)) = extra_body else {
        return extra_body.cloned();
    };
    let mut cleaned = Map::new();
    for (key, value) in map {
        if !local_control_key_for_profile(profile, key) {
            cleaned.insert(key.clone(), value.clone());
        }
    }
    Some(Value::Object(cleaned))
}

pub fn normalize_messages_for_profile(profile: Option<&str>, messages: &mut Value) {
    let Some("qwen-oauth") = profile.and_then(canonical_provider_profile_id) else {
        return;
    };
    let Some(items) = messages.as_array_mut() else {
        return;
    };
    for item in items {
        let Some(obj) = item.as_object_mut() else {
            continue;
        };
        let is_system = obj
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|role| role.eq_ignore_ascii_case("system"));
        match obj.get_mut("content") {
            Some(Value::String(text)) => {
                let mut part = Map::new();
                part.insert("type".to_string(), Value::String("text".to_string()));
                part.insert("text".to_string(), Value::String(text.clone()));
                if is_system {
                    part.insert(
                        "cache_control".to_string(),
                        serde_json::json!({"type": "ephemeral"}),
                    );
                }
                *obj.get_mut("content").expect("content exists") =
                    Value::Array(vec![Value::Object(part)]);
            }
            Some(Value::Array(parts)) if is_system => {
                if let Some(last) = parts.last_mut().and_then(Value::as_object_mut) {
                    last.entry("cache_control".to_string())
                        .or_insert_with(|| serde_json::json!({"type": "ephemeral"}));
                }
            }
            _ => {}
        }
    }
}

pub fn apply_profile_to_body(
    profile: Option<&str>,
    body: &mut Value,
    effective_model: &str,
    extra_body: Option<&Value>,
) {
    let Some(profile) = profile.and_then(canonical_provider_profile_id) else {
        return;
    };
    match profile {
        "kimi-coding" | "kimi-coding-cn" => apply_kimi_profile(body, extra_body),
        "openrouter" => apply_openrouter_profile(body, effective_model, extra_body),
        "nous" => apply_nous_profile(body, extra_body),
        "qwen-oauth" => apply_qwen_profile(body, extra_body),
        "custom" => apply_custom_profile(body, extra_body),
        "nvidia" => {}
        _ => {}
    }
}

pub fn extra_headers_for_profile(
    profile: Option<&str>,
    effective_model: &str,
    extra_body: Option<&Value>,
) -> Vec<(String, String)> {
    let Some("openrouter") = profile.and_then(canonical_provider_profile_id) else {
        return Vec::new();
    };
    let Some(session_id) = string_field(extra_body, "session_id") else {
        return Vec::new();
    };
    if is_grok_model(effective_model) {
        vec![("x-grok-conv-id".to_string(), session_id.to_string())]
    } else {
        Vec::new()
    }
}

fn apply_kimi_profile(body: &mut Value, extra_body: Option<&Value>) {
    remove_key(body, "temperature");
    remove_key(body, "reasoning");
    let reasoning = reasoning_config(extra_body);
    let enabled = reasoning
        .and_then(|cfg| cfg.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    body["thinking"] = serde_json::json!({
        "type": if enabled { "enabled" } else { "disabled" },
    });
    if enabled {
        let effort = reasoning
            .and_then(|cfg| cfg.get("effort"))
            .and_then(Value::as_str)
            .or_else(|| string_field(extra_body, "reasoning_effort"))
            .unwrap_or("medium");
        body["reasoning_effort"] = Value::String(effort.to_string());
    } else {
        remove_key(body, "reasoning_effort");
    }
}

fn apply_openrouter_profile(body: &mut Value, effective_model: &str, extra_body: Option<&Value>) {
    if let Some(preferences) = extra_body.and_then(|v| v.get("provider_preferences")) {
        body["provider"] = preferences.clone();
    }
    if let Some(session_id) = string_field(extra_body, "session_id") {
        body["session_id"] = Value::String(session_id.to_string());
    }
    if is_pareto_code_model(effective_model) {
        if let Some(score) = extra_body
            .and_then(|v| v.get("openrouter_min_coding_score"))
            .and_then(parse_score)
        {
            body["plugins"] =
                serde_json::json!([{"id": "pareto-router", "min_coding_score": score}]);
        }
    }

    let supports_reasoning = bool_field(extra_body, "supports_reasoning").unwrap_or(false);
    if let Some(reasoning) = reasoning_config(extra_body) {
        body["reasoning"] = reasoning.clone();
    } else if let Some(effort) = string_field(extra_body, "reasoning_effort") {
        body["reasoning"] = serde_json::json!({"enabled": true, "effort": effort});
    } else if supports_reasoning {
        body["reasoning"] = serde_json::json!({"enabled": true, "effort": "medium"});
    }
}

fn apply_nous_profile(body: &mut Value, extra_body: Option<&Value>) {
    body["tags"] = nous_portal_tags();

    let supports_reasoning = bool_field(extra_body, "supports_reasoning").unwrap_or(false);
    let reasoning = reasoning_config(extra_body);
    let enabled = reasoning
        .and_then(|cfg| cfg.get("enabled"))
        .and_then(Value::as_bool);

    match (supports_reasoning, reasoning, enabled) {
        (_, Some(_), Some(false)) => remove_key(body, "reasoning"),
        (true, Some(cfg), _) => body["reasoning"] = cfg.clone(),
        (true, None, _) => {
            if let Some(effort) = string_field(extra_body, "reasoning_effort") {
                body["reasoning"] = serde_json::json!({"effort": effort});
            }
        }
        _ => remove_key(body, "reasoning"),
    }
}

fn apply_qwen_profile(body: &mut Value, extra_body: Option<&Value>) {
    body["vl_high_resolution_images"] = Value::Bool(true);
    if let Some(metadata) = extra_body.and_then(|v| v.get("qwen_session_metadata")) {
        body["metadata"] = metadata.clone();
    }
}

fn apply_custom_profile(body: &mut Value, extra_body: Option<&Value>) {
    let Some(num_ctx) = extra_body
        .and_then(|v| v.get("ollama_num_ctx"))
        .and_then(|v| v.as_u64())
    else {
        return;
    };
    let options = body
        .as_object_mut()
        .expect("request body should be an object")
        .entry("options".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !options.is_object() {
        *options = Value::Object(Map::new());
    }
    options["num_ctx"] = Value::Number(num_ctx.into());
}

fn reasoning_config(extra_body: Option<&Value>) -> Option<&Value> {
    extra_body
        .and_then(|v| v.get("reasoning_config"))
        .or_else(|| extra_body.and_then(|v| v.get("reasoning")))
}

fn string_field<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a str> {
    value
        .and_then(|v| v.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn bool_field(value: Option<&Value>, key: &str) -> Option<bool> {
    value.and_then(|v| v.get(key)).and_then(Value::as_bool)
}

fn parse_score(value: &Value) -> Option<f64> {
    let score = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|v| v.trim().parse::<f64>().ok()))?;
    if (0.0..=1.0).contains(&score) {
        Some(score)
    } else {
        None
    }
}

fn is_grok_model(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    lower.starts_with("x-ai/grok")
        || lower.starts_with("xai/grok")
        || lower.starts_with("grok")
        || lower.contains("/grok")
}

fn is_pareto_code_model(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    lower == "openrouter/pareto-code" || lower.ends_with("/pareto-code")
}

fn remove_key(body: &mut Value, key: &str) {
    if let Some(obj) = body.as_object_mut() {
        obj.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_match_upstream_provider_profiles() {
        assert_eq!(canonical_provider_profile_id("kimi"), Some("kimi-coding"));
        assert_eq!(
            canonical_provider_profile_id("moonshot"),
            Some("kimi-coding")
        );
        assert_eq!(
            canonical_provider_profile_id("kimi-coding-cn"),
            Some("kimi-coding-cn")
        );
        assert_eq!(canonical_provider_profile_id("or"), Some("openrouter"));
        assert_eq!(canonical_provider_profile_id("nous-portal"), Some("nous"));
        assert_eq!(canonical_provider_profile_id("qwen"), Some("qwen-oauth"));
        assert_eq!(canonical_provider_profile_id("mimo"), Some("xiaomi"));
        assert_eq!(canonical_provider_profile_id("xiaomi-mimo"), Some("xiaomi"));
        assert_eq!(
            canonical_provider_profile_id("qwen-portal"),
            Some("qwen-oauth")
        );
        assert_eq!(canonical_provider_profile_id("missing"), None);
    }

    #[test]
    fn profile_static_metadata_matches_runtime_contracts() {
        assert_eq!(default_max_tokens("nvidia"), Some(16_384));
        assert_eq!(default_max_tokens("kimi"), Some(32_000));
        assert_eq!(default_max_tokens("qwen-oauth"), Some(65_536));
        assert!(omit_temperature("kimi"));
        assert_eq!(profile_auth_type("nous"), Some("oauth_device_code"));
        assert_eq!(profile_auth_type("qwen-oauth"), Some("oauth_external"));
        assert!(profile_base_url("nvidia").unwrap().contains("nvidia.com"));
        assert!(profile_base_url("kimi-coding-cn")
            .unwrap()
            .contains("moonshot.cn"));
        assert!(profile_base_url("mimo").unwrap().contains("xiaomimimo.com"));
        assert!(supports_vision("xiaomi"));
        assert!(!supports_vision("kimi"));
    }

    #[test]
    fn qwen_message_normalization_adds_parts_and_cache_control() {
        let mut messages = serde_json::json!([
            {"role": "system", "content": "Be helpful"},
            {"role": "user", "content": "hello"}
        ]);
        normalize_messages_for_profile(Some("qwen-oauth"), &mut messages);
        assert!(messages[0]["content"].is_array());
        assert_eq!(messages[0]["content"][0]["text"], "Be helpful");
        assert_eq!(
            messages[0]["content"][0]["cache_control"],
            serde_json::json!({"type": "ephemeral"})
        );
        assert_eq!(
            messages[1]["content"][0],
            serde_json::json!({"type": "text", "text": "hello"})
        );
    }

    #[test]
    fn openrouter_grok_session_affinity_header_is_profile_scoped() {
        let extra = serde_json::json!({"session_id": "sess-abc"});
        assert_eq!(
            extra_headers_for_profile(Some("openrouter"), "x-ai/grok-4", Some(&extra)),
            vec![("x-grok-conv-id".to_string(), "sess-abc".to_string())]
        );
        assert!(extra_headers_for_profile(
            Some("openrouter"),
            "anthropic/claude-sonnet-4.6",
            Some(&extra)
        )
        .is_empty());
    }
}
