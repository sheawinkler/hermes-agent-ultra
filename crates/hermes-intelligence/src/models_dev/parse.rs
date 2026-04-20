//! Lossy JSON → typed-struct converters for the models.dev registry.
//!
//! These mirror `_parse_model_info`, `_parse_provider_info`, and
//! `_extract_context` in `agent/models_dev.py`. Every accessor is
//! defensive — missing/wrong-typed fields produce defaults rather than
//! errors, exactly like the Python type-check guards. This is essential
//! because models.dev evolves its schema and we want best-effort metadata
//! rather than a hard failure.

use serde_json::Value;

use super::types::{InterleavedFlag, ModelCapabilities, ModelInfo, ProviderInfo};

// ---------------------------------------------------------------------------
// Primitive extractors
// ---------------------------------------------------------------------------

fn as_bool(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn as_string(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn as_string_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Extract a `>0` positive integer, treating missing / zero / non-numeric
/// as `None`. Mirrors `_extract_context`'s "filters out context=0 entries".
fn as_positive_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => {
            if let Some(i) = n.as_u64() {
                if i > 0 {
                    Some(i)
                } else {
                    None
                }
            } else if let Some(f) = n.as_f64() {
                if f > 0.0 {
                    Some(f as u64)
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

fn as_f64(v: &Value, key: &str) -> f64 {
    v.get(key)
        .and_then(|x| match x {
            Value::Number(n) => n.as_f64(),
            _ => None,
        })
        .unwrap_or(0.0)
}

fn as_optional_f64(v: &Value, key: &str) -> Option<f64> {
    let raw = v.get(key)?;
    if raw.is_null() {
        return None;
    }
    raw.as_f64()
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Extract just the context window from a raw model entry, treating
/// 0/missing/non-numeric as `None`.
pub fn extract_context(entry: &Value) -> Option<u64> {
    let limit = entry.get("limit")?;
    let ctx = limit.get("context")?;
    as_positive_u64(ctx)
}

/// Convert a raw model entry into a [`ModelInfo`].
pub fn parse_model_info(model_id: &str, raw: &Value, provider_id: &str) -> ModelInfo {
    let limit = raw.get("limit").cloned().unwrap_or(Value::Null);
    let cost = raw.get("cost").cloned().unwrap_or(Value::Null);
    let modalities = raw.get("modalities").cloned().unwrap_or(Value::Null);

    let context_window = limit.get("context").and_then(as_positive_u64).unwrap_or(0);
    let max_output = limit.get("output").and_then(as_positive_u64).unwrap_or(0);
    let max_input = limit.get("input").and_then(as_positive_u64);

    let interleaved = match raw.get("interleaved") {
        Some(Value::Bool(b)) => InterleavedFlag::Bool(*b),
        Some(Value::Object(o)) => o
            .get("field")
            .and_then(Value::as_str)
            .map(|s| InterleavedFlag::Field(s.to_string()))
            .unwrap_or_default(),
        _ => InterleavedFlag::Bool(false),
    };

    ModelInfo {
        id: model_id.to_string(),
        name: {
            let n = as_string(raw, "name");
            if n.is_empty() {
                model_id.to_string()
            } else {
                n
            }
        },
        family: as_string(raw, "family"),
        provider_id: provider_id.to_string(),
        reasoning: as_bool(raw, "reasoning"),
        tool_call: as_bool(raw, "tool_call"),
        attachment: as_bool(raw, "attachment"),
        temperature: as_bool(raw, "temperature"),
        structured_output: as_bool(raw, "structured_output"),
        open_weights: as_bool(raw, "open_weights"),
        input_modalities: as_string_list(&modalities, "input"),
        output_modalities: as_string_list(&modalities, "output"),
        context_window,
        max_output,
        max_input,
        cost_input: as_f64(&cost, "input"),
        cost_output: as_f64(&cost, "output"),
        cost_cache_read: as_optional_f64(&cost, "cache_read"),
        cost_cache_write: as_optional_f64(&cost, "cache_write"),
        knowledge_cutoff: as_string(raw, "knowledge"),
        release_date: as_string(raw, "release_date"),
        status: as_string(raw, "status"),
        interleaved,
    }
}

/// Convert a raw provider entry into a [`ProviderInfo`].
pub fn parse_provider_info(provider_id: &str, raw: &Value) -> ProviderInfo {
    let env: Vec<String> = raw
        .get("env")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let model_count = raw
        .get("models")
        .and_then(Value::as_object)
        .map(|m| m.len())
        .unwrap_or(0);
    ProviderInfo {
        id: provider_id.to_string(),
        name: {
            let n = as_string(raw, "name");
            if n.is_empty() {
                provider_id.to_string()
            } else {
                n
            }
        },
        env,
        api: as_string(raw, "api"),
        doc: as_string(raw, "doc"),
        model_count,
    }
}

/// Compact capability extraction with Python-parity defaults
/// (`supports_tools = false` since the Python helper inspects the raw
/// `tool_call` field directly — note this is **different** from
/// `ModelCapabilities::default()` which assumes `true` for unknown models).
pub fn parse_model_capabilities(raw: &Value) -> ModelCapabilities {
    let supports_tools = as_bool(raw, "tool_call");
    let supports_vision = as_bool(raw, "attachment");
    let supports_reasoning = as_bool(raw, "reasoning");

    let limit = raw.get("limit").cloned().unwrap_or(Value::Null);
    let context_window = limit
        .get("context")
        .and_then(as_positive_u64)
        .unwrap_or(200_000);
    let max_output_tokens = limit
        .get("output")
        .and_then(as_positive_u64)
        .unwrap_or(8_192);
    let model_family = as_string(raw, "family");

    ModelCapabilities {
        supports_tools,
        supports_vision,
        supports_reasoning,
        context_window,
        max_output_tokens,
        model_family,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_context_handles_zero_and_missing() {
        assert_eq!(extract_context(&json!({"limit": {"context": 0}})), None);
        assert_eq!(extract_context(&json!({"limit": {}})), None);
        assert_eq!(extract_context(&json!({})), None);
        assert_eq!(extract_context(&json!({"limit": "bad"})), None);
        assert_eq!(
            extract_context(&json!({"limit": {"context": 200000}})),
            Some(200_000)
        );
    }

    #[test]
    fn parse_model_info_handles_full_entry() {
        let raw = json!({
            "name": "Claude Sonnet 4.5",
            "family": "claude",
            "reasoning": true,
            "tool_call": true,
            "attachment": true,
            "temperature": true,
            "structured_output": false,
            "open_weights": false,
            "modalities": {
                "input": ["text", "image"],
                "output": ["text"]
            },
            "limit": {"context": 200000, "output": 8192, "input": 180000},
            "cost": {"input": 3.0, "output": 15.0, "cache_read": 0.3},
            "knowledge": "2025-01",
            "release_date": "2025-04-01",
            "status": "",
            "interleaved": {"field": "reasoning_content"}
        });
        let info = parse_model_info("claude-sonnet-4-5", &raw, "anthropic");
        assert_eq!(info.id, "claude-sonnet-4-5");
        assert_eq!(info.name, "Claude Sonnet 4.5");
        assert!(info.reasoning && info.tool_call && info.attachment);
        assert_eq!(info.input_modalities, vec!["text", "image"]);
        assert_eq!(info.context_window, 200_000);
        assert_eq!(info.max_output, 8_192);
        assert_eq!(info.max_input, Some(180_000));
        assert_eq!(info.cost_input, 3.0);
        assert_eq!(info.cost_cache_read, Some(0.3));
        assert_eq!(info.cost_cache_write, None);
        assert_eq!(
            info.interleaved,
            InterleavedFlag::Field("reasoning_content".into())
        );
        assert!(info.supports_vision());
        assert!(!info.supports_pdf());
    }

    #[test]
    fn parse_model_info_handles_minimal_entry() {
        let info = parse_model_info("foo", &json!({}), "test");
        assert_eq!(info.id, "foo");
        assert_eq!(info.name, "foo"); // falls back to id when name missing
        assert_eq!(info.context_window, 0);
        assert_eq!(info.max_input, None);
        assert!(!info.has_cost_data());
    }

    #[test]
    fn parse_model_info_treats_zero_limits_as_unknown() {
        let raw = json!({"limit": {"context": 0, "output": 0, "input": 0}});
        let info = parse_model_info("x", &raw, "p");
        assert_eq!(info.context_window, 0);
        assert_eq!(info.max_output, 0);
        assert_eq!(info.max_input, None);
    }

    #[test]
    fn parse_model_info_interleaved_bool_variant() {
        let raw = json!({"interleaved": true});
        let info = parse_model_info("x", &raw, "p");
        assert_eq!(info.interleaved, InterleavedFlag::Bool(true));
        assert!(info.interleaved.is_enabled());
    }

    #[test]
    fn parse_provider_info_extracts_env_and_model_count() {
        let raw = json!({
            "name": "Anthropic",
            "env": ["ANTHROPIC_API_KEY"],
            "api": "https://api.anthropic.com/v1",
            "doc": "https://docs.anthropic.com",
            "models": {"a": {}, "b": {}, "c": {}}
        });
        let info = parse_provider_info("anthropic", &raw);
        assert_eq!(info.id, "anthropic");
        assert_eq!(info.name, "Anthropic");
        assert_eq!(info.env, vec!["ANTHROPIC_API_KEY"]);
        assert_eq!(info.model_count, 3);
    }

    #[test]
    fn parse_capabilities_defaults_to_python_constants() {
        let raw = json!({});
        let caps = parse_model_capabilities(&raw);
        assert!(!caps.supports_tools);
        assert_eq!(caps.context_window, 200_000);
        assert_eq!(caps.max_output_tokens, 8_192);
    }

    #[test]
    fn parse_capabilities_reads_explicit_flags() {
        let raw = json!({
            "tool_call": true,
            "attachment": true,
            "reasoning": true,
            "limit": {"context": 1_048_576, "output": 32_768},
            "family": "gemini"
        });
        let caps = parse_model_capabilities(&raw);
        assert!(caps.supports_tools);
        assert!(caps.supports_vision);
        assert!(caps.supports_reasoning);
        assert_eq!(caps.context_window, 1_048_576);
        assert_eq!(caps.max_output_tokens, 32_768);
        assert_eq!(caps.model_family, "gemini");
    }
}
