//! Model metadata, context lengths, and token estimation utilities.
//!
//! Pure utility functions with no agent dependency. Used by context
//! compression and run_agent for pre-flight context checks.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Descending tiers for context length probing when the model is unknown.
pub const CONTEXT_PROBE_TIERS: &[u64] = &[128_000, 64_000, 32_000, 16_000, 8_000];

/// Default context length when no detection method succeeds.
pub const DEFAULT_FALLBACK_CONTEXT: u64 = 128_000;

/// Minimum context length for Hermes Agent tool-calling workflows.
pub const MINIMUM_CONTEXT_LENGTH: u64 = 64_000;

// ---------------------------------------------------------------------------
// ModelMetadataEntry
// ---------------------------------------------------------------------------

/// Metadata about a model from a provider catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetadataEntry {
    pub context_length: u64,
    #[serde(default)]
    pub max_completion_tokens: Option<u64>,
    pub name: String,
    #[serde(default)]
    pub pricing: Option<HashMap<String, f64>>,
    #[serde(default)]
    pub supports_vision: bool,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_streaming: bool,
}

// ---------------------------------------------------------------------------
// Static default context lengths (thin fallback)
// ---------------------------------------------------------------------------

/// Known model context lengths — broad family patterns only.
/// Provider-specific resolution should use live APIs first.
static DEFAULT_CONTEXT_LENGTHS: &[(&str, u64)] = &[
    // Claude 4.6 (1M context)
    ("claude-opus-4-6", 1_000_000),
    ("claude-sonnet-4-6", 1_000_000),
    ("claude-opus-4.6", 1_000_000),
    ("claude-sonnet-4.6", 1_000_000),
    // Older Claude
    ("claude", 200_000),
    // OpenAI
    ("gpt-4.1", 1_047_576),
    ("gpt-5", 128_000),
    ("gpt-4", 128_000),
    // Google
    ("gemini", 1_048_576),
    ("gemma-4", 256_000),
    ("gemma4", 256_000),
    ("gemma-4-31b", 256_000),
    ("gemma-3", 131_072),
    ("gemma", 8_192),
    // DeepSeek
    ("deepseek", 128_000),
    // Meta
    ("llama", 131_072),
    // Qwen
    ("qwen3-coder-plus", 1_000_000),
    ("qwen3-coder", 262_144),
    ("qwen", 131_072),
    // MiniMax
    ("minimax", 204_800),
    // GLM
    ("glm", 202_752),
    // xAI Grok
    ("grok-4-1-fast", 2_000_000),
    ("grok-4-fast", 2_000_000),
    ("grok-4.20", 2_000_000),
    ("grok-code-fast", 256_000),
    ("grok-4", 256_000),
    ("grok-3", 131_072),
    ("grok-2", 131_072),
    ("grok", 131_072),
    // Kimi
    ("kimi", 262_144),
    // Xiaomi MIMO
    ("mimo-v2.5-pro", 1_000_000),
    ("mimo-v2.5", 1_000_000),
];

// ---------------------------------------------------------------------------
// Model info (enriched)
// ---------------------------------------------------------------------------

/// Comprehensive model information including capabilities and pricing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    pub provider: String,
    pub context_window: u64,
    pub max_output_tokens: Option<u64>,
    pub supports_vision: bool,
    pub supports_tools: bool,
    pub supports_streaming: bool,
    pub supports_reasoning: bool,
    pub input_cost_per_million: Option<f64>,
    pub output_cost_per_million: Option<f64>,
}

/// Static database of well-known models.
pub fn known_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            name: "gpt-4o".into(),
            provider: "openai".into(),
            context_window: 128_000,
            max_output_tokens: Some(16_384),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(2.50),
            output_cost_per_million: Some(10.00),
        },
        ModelInfo {
            name: "gpt-4o-mini".into(),
            provider: "openai".into(),
            context_window: 128_000,
            max_output_tokens: Some(16_384),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
            input_cost_per_million: Some(0.15),
            output_cost_per_million: Some(0.60),
        },
        ModelInfo {
            name: "gpt-4.1".into(),
            provider: "openai".into(),
            context_window: 1_047_576,
            max_output_tokens: Some(32_768),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(2.00),
            output_cost_per_million: Some(8.00),
        },
        ModelInfo {
            name: "gpt-4.1-mini".into(),
            provider: "openai".into(),
            context_window: 1_047_576,
            max_output_tokens: Some(32_768),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
            input_cost_per_million: Some(0.40),
            output_cost_per_million: Some(1.60),
        },
        ModelInfo {
            name: "gpt-4.1-nano".into(),
            provider: "openai".into(),
            context_window: 1_047_576,
            max_output_tokens: Some(32_768),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
            input_cost_per_million: Some(0.10),
            output_cost_per_million: Some(0.40),
        },
        ModelInfo {
            name: "o3".into(),
            provider: "openai".into(),
            context_window: 200_000,
            max_output_tokens: Some(100_000),
            supports_vision: false,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(10.00),
            output_cost_per_million: Some(40.00),
        },
        ModelInfo {
            name: "o3-mini".into(),
            provider: "openai".into(),
            context_window: 200_000,
            max_output_tokens: Some(100_000),
            supports_vision: false,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(1.10),
            output_cost_per_million: Some(4.40),
        },
        ModelInfo {
            name: "claude-opus-4-6".into(),
            provider: "anthropic".into(),
            context_window: 1_000_000,
            max_output_tokens: Some(128_000),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(15.00),
            output_cost_per_million: Some(75.00),
        },
        ModelInfo {
            name: "claude-sonnet-4-6".into(),
            provider: "anthropic".into(),
            context_window: 1_000_000,
            max_output_tokens: Some(64_000),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(3.00),
            output_cost_per_million: Some(15.00),
        },
        ModelInfo {
            name: "claude-sonnet-4".into(),
            provider: "anthropic".into(),
            context_window: 200_000,
            max_output_tokens: Some(64_000),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(3.00),
            output_cost_per_million: Some(15.00),
        },
        ModelInfo {
            name: "claude-3-5-haiku".into(),
            provider: "anthropic".into(),
            context_window: 200_000,
            max_output_tokens: Some(8_192),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
            input_cost_per_million: Some(0.80),
            output_cost_per_million: Some(4.00),
        },
        ModelInfo {
            name: "gemini-2.5-pro".into(),
            provider: "google".into(),
            context_window: 1_048_576,
            max_output_tokens: Some(65_536),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(1.25),
            output_cost_per_million: Some(10.00),
        },
        ModelInfo {
            name: "gemini-2.5-flash".into(),
            provider: "google".into(),
            context_window: 1_048_576,
            max_output_tokens: Some(65_536),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(0.15),
            output_cost_per_million: Some(0.60),
        },
        ModelInfo {
            name: "gemini-2.0-flash".into(),
            provider: "google".into(),
            context_window: 1_048_576,
            max_output_tokens: Some(8_192),
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
            input_cost_per_million: Some(0.10),
            output_cost_per_million: Some(0.40),
        },
        ModelInfo {
            name: "deepseek-chat".into(),
            provider: "deepseek".into(),
            context_window: 128_000,
            max_output_tokens: Some(8_192),
            supports_vision: false,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
            input_cost_per_million: Some(0.14),
            output_cost_per_million: Some(0.28),
        },
        ModelInfo {
            name: "deepseek-reasoner".into(),
            provider: "deepseek".into(),
            context_window: 128_000,
            max_output_tokens: Some(8_192),
            supports_vision: false,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: true,
            input_cost_per_million: Some(0.55),
            output_cost_per_million: Some(2.19),
        },
    ]
}

// ---------------------------------------------------------------------------
// Lookup functions
// ---------------------------------------------------------------------------

/// Look up model info by name (substring match, longest wins).
pub fn get_model_info(model_name: &str) -> Option<ModelInfo> {
    let lower = model_name.to_lowercase();
    let models = known_models();
    let mut best: Option<&ModelInfo> = None;
    let mut best_len = 0;

    for info in &models {
        let key = info.name.to_lowercase();
        if lower.contains(&key) && key.len() > best_len {
            best = Some(info);
            best_len = key.len();
        }
    }

    best.cloned()
}

/// Check if a model supports vision.
pub fn supports_vision(model: &str) -> bool {
    get_model_info(model)
        .map(|m| m.supports_vision)
        .unwrap_or(false)
}

/// Check if a model supports tool/function calling.
pub fn supports_tools(model: &str) -> bool {
    get_model_info(model)
        .map(|m| m.supports_tools)
        .unwrap_or(true)
}

/// Get the maximum output tokens for a model.
pub fn max_output_tokens(model: &str) -> Option<u64> {
    get_model_info(model).and_then(|m| m.max_output_tokens)
}

/// Get the context window for a model using the hardcoded fallback table.
pub fn get_model_context_length(model: &str) -> u64 {
    let model_lower = model.to_lowercase();

    // Check known models first
    if let Some(info) = get_model_info(model) {
        return info.context_window;
    }

    // Hardcoded defaults (fuzzy match, longest key first)
    let mut entries: Vec<_> = DEFAULT_CONTEXT_LENGTHS.to_vec();
    entries.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    for (key, length) in entries {
        if model_lower.contains(key) {
            return length;
        }
    }

    DEFAULT_FALLBACK_CONTEXT
}

/// Get the next lower probe tier for context length probing.
pub fn get_next_probe_tier(current: u64) -> Option<u64> {
    CONTEXT_PROBE_TIERS
        .iter()
        .copied()
        .find(|&tier| tier < current)
}

// ---------------------------------------------------------------------------
// Token estimation
// ---------------------------------------------------------------------------

/// Rough token estimate (~4 chars/token) for pre-flight checks.
/// Uses ceiling division so short texts never estimate as 0.
pub fn estimate_tokens_rough(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    ((text.len() as u64) + 3) / 4
}

/// Rough token estimate for a serialized message list.
pub fn estimate_messages_tokens_rough(messages: &[serde_json::Value]) -> u64 {
    let total_chars: usize = messages.iter().map(|m| m.to_string().len()).sum();
    ((total_chars as u64) + 3) / 4
}

/// Rough token estimate for a full request (messages + system + tools).
pub fn estimate_request_tokens_rough(
    messages: &[serde_json::Value],
    system_prompt: &str,
    tools: Option<&[serde_json::Value]>,
) -> u64 {
    let mut total_chars = system_prompt.len();
    total_chars += messages.iter().map(|m| m.to_string().len()).sum::<usize>();
    if let Some(tools) = tools {
        total_chars += tools.iter().map(|t| t.to_string().len()).sum::<usize>();
    }
    ((total_chars as u64) + 3) / 4
}

// ---------------------------------------------------------------------------
// Error parsing
// ---------------------------------------------------------------------------

/// Try to extract the actual context limit from an API error message.
pub fn parse_context_limit_from_error(error_msg: &str) -> Option<u64> {
    let lower = error_msg.to_lowercase();
    let patterns = &[
        r"(?:max(?:imum)?|limit)\s*(?:context\s*)?(?:length|size|window)?\s*(?:is|of|:)?\s*(\d{4,})",
        r"context\s*(?:length|size|window)\s*(?:is|of|:)?\s*(\d{4,})",
        r"(\d{4,})\s*(?:token)?\s*(?:context|limit)",
    ];

    for pattern in patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            if let Some(caps) = re.captures(&lower) {
                if let Some(m) = caps.get(1) {
                    if let Ok(limit) = m.as_str().parse::<u64>() {
                        if (1024..=10_000_000).contains(&limit) {
                            return Some(limit);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Detect "max_tokens too large" errors and return available output tokens.
pub fn parse_available_output_tokens_from_error(error_msg: &str) -> Option<u64> {
    let lower = error_msg.to_lowercase();
    if !lower.contains("max_tokens") || !lower.contains("available") {
        return None;
    }

    let patterns = &[
        r"available_tokens[:\s]+(\d+)",
        r"available\s+tokens[:\s]+(\d+)",
        r"=\s*(\d+)\s*$",
    ];

    for pattern in patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            if let Some(caps) = re.captures(&lower) {
                if let Some(m) = caps.get(1) {
                    if let Ok(tokens) = m.as_str().parse::<u64>() {
                        if tokens >= 1 {
                            return Some(tokens);
                        }
                    }
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Provider inference
// ---------------------------------------------------------------------------

/// Infer the provider name from a base URL.
pub fn infer_provider_from_url(base_url: &str) -> Option<&'static str> {
    let lower = base_url.to_lowercase();
    let mappings: &[(&str, &str)] = &[
        ("api.openai.com", "openai"),
        ("chatgpt.com", "openai"),
        ("api.anthropic.com", "anthropic"),
        ("api.z.ai", "zai"),
        ("open.bigmodel.cn", "zai"),
        ("api.moonshot.ai", "kimi-coding"),
        ("api.minimax", "minimax"),
        ("dashscope.aliyuncs.com", "alibaba"),
        ("openrouter.ai", "openrouter"),
        ("generativelanguage.googleapis.com", "gemini"),
        ("api.deepseek.com", "deepseek"),
        ("api.githubcopilot.com", "copilot"),
        ("api.x.ai", "xai"),
    ];

    for (pattern, provider) in mappings {
        if lower.contains(pattern) {
            return Some(provider);
        }
    }
    None
}

/// Check if a base URL points to a local machine.
pub fn is_local_endpoint(base_url: &str) -> bool {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return false;
    }

    let parse_target = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let host = reqwest::Url::parse(&parse_target)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| trimmed.to_string());
    let host_lower = host.to_lowercase();

    if host_lower == "localhost"
        || host_lower == "0.0.0.0"
        || host_lower == "::1"
        || host_lower.ends_with(".docker.internal")
        || host_lower.ends_with(".containers.internal")
    {
        return true;
    }

    if let Ok(addr) = host.parse::<IpAddr>() {
        return match addr {
            IpAddr::V4(v4) => {
                v4.is_private() || v4.is_loopback() || v4.is_link_local() || is_tailscale_cgnat(v4)
            }
            IpAddr::V6(v6) => {
                v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local()
            }
        };
    }

    // Fallback for dotted IPv4-like strings that fail Url/IP parsing.
    let parts: Vec<_> = host_lower.split('.').collect();
    if parts.len() == 4 {
        if let (Ok(first), Ok(second)) = (parts[0].parse::<u8>(), parts[1].parse::<u8>()) {
            return first == 10
                || (first == 172 && (16..=31).contains(&second))
                || (first == 192 && second == 168)
                || (first == 100 && (64..=127).contains(&second));
        }
    }

    false
}

fn is_tailscale_cgnat(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 100 && (64..=127).contains(&octets[1])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_model_context_length() {
        assert_eq!(get_model_context_length("gpt-4o"), 128_000);
        assert_eq!(get_model_context_length("claude-opus-4-6"), 1_000_000);
        assert_eq!(get_model_context_length("gemini-2.0-flash"), 1_048_576);
        assert_eq!(get_model_context_length("gemma4:31b-cloud"), 256_000);
        assert_eq!(get_model_context_length("xiaomi/mimo-v2.5-pro"), 1_000_000);
        assert_eq!(
            get_model_context_length("unknown-model"),
            DEFAULT_FALLBACK_CONTEXT
        );
    }

    #[test]
    fn test_estimate_tokens_rough() {
        assert_eq!(estimate_tokens_rough(""), 0);
        assert_eq!(estimate_tokens_rough("hello world"), 3); // 11 chars -> 3
        assert_eq!(estimate_tokens_rough("hi"), 1); // 2 chars -> ceiling(5/4) = 1
    }

    #[test]
    fn test_get_model_info() {
        let info = get_model_info("gpt-4o").unwrap();
        assert_eq!(info.context_window, 128_000);
        assert!(info.supports_vision);
    }

    #[test]
    fn test_supports_vision() {
        assert!(supports_vision("gpt-4o"));
        assert!(supports_vision("claude-opus-4-6"));
        assert!(!supports_vision("deepseek-chat"));
    }

    #[test]
    fn test_get_next_probe_tier() {
        assert_eq!(get_next_probe_tier(128_000), Some(64_000));
        assert_eq!(get_next_probe_tier(64_000), Some(32_000));
        assert_eq!(get_next_probe_tier(8_000), None);
    }

    #[test]
    fn test_infer_provider() {
        assert_eq!(
            infer_provider_from_url("https://api.openai.com/v1"),
            Some("openai")
        );
        assert_eq!(
            infer_provider_from_url("https://api.anthropic.com"),
            Some("anthropic")
        );
        assert_eq!(
            infer_provider_from_url("https://open.bigmodel.cn/api/paas/v4"),
            Some("zai")
        );
        assert_eq!(infer_provider_from_url("http://localhost:8080"), None);
    }

    #[test]
    fn test_is_local_endpoint() {
        assert!(is_local_endpoint("http://localhost:8080"));
        assert!(is_local_endpoint("http://127.0.0.1:11434"));
        assert!(is_local_endpoint("http://100.64.0.0:11434"));
        assert!(is_local_endpoint("http://100.64.0.1:11434/v1"));
        assert!(is_local_endpoint("http://100.77.243.5:11434"));
        assert!(is_local_endpoint("https://100.100.100.100:443"));
        assert!(is_local_endpoint("https://100.127.255.254:443"));
        assert!(is_local_endpoint("http://100.127.255.255:11434"));
        assert!(!is_local_endpoint("http://100.63.255.255:11434"));
        assert!(!is_local_endpoint("http://100.128.0.1:11434"));
        assert!(!is_local_endpoint("http://100.200.0.1:11434"));
        assert!(!is_local_endpoint("http://99.64.0.1:11434"));
        assert!(!is_local_endpoint("https://api.openai.com"));
    }
}
