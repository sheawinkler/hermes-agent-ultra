//! Provider-aware model identifier normalization.
//!
//! The CLI accepts user-facing model names, catalog slugs, and provider-prefixed
//! IDs. Runtime providers need the exact shape each upstream API expects.

use hermes_intelligence::anthropic_adapter::normalize_model_name as normalize_anthropic_model_name;

const DOT_TO_HYPHEN_PROVIDERS: &[&str] = &["anthropic"];

fn canonical_provider(provider: &str) -> String {
    match provider.trim().to_ascii_lowercase().as_str() {
        "codex" => "openai-codex".to_string(),
        "github-copilot" | "github-models" => "copilot".to_string(),
        "github-copilot-acp" | "copilot-acp-agent" => "copilot-acp".to_string(),
        "opencode" | "zen" => "opencode-zen".to_string(),
        "go" => "opencode-go".to_string(),
        "nous_api" | "nousapi" | "nous-portal-api" => "nous-api".to_string(),
        "moonshot" | "kimi-coding" | "kimi-coding-cn" => "kimi".to_string(),
        "glm" | "z-ai" | "z_ai" | "zhipu" => "zai".to_string(),
        "claude" | "claude-code" => "anthropic".to_string(),
        "google" | "google-gemini" | "google-ai-studio" => "gemini".to_string(),
        other => other.to_string(),
    }
}

fn split_vendor_prefix(model: &str) -> Option<(&str, &str)> {
    let (vendor, rest) = model.split_once('/')?;
    let vendor = vendor.trim();
    let rest = rest.trim();
    if vendor.is_empty() || rest.is_empty() {
        return None;
    }
    Some((vendor, rest))
}

fn strip_if_vendor_matches<'a>(model: &'a str, vendors: &[&str]) -> &'a str {
    let Some((vendor, rest)) = split_vendor_prefix(model) else {
        return model.trim();
    };
    if vendors
        .iter()
        .any(|candidate| vendor.eq_ignore_ascii_case(candidate))
    {
        rest
    } else {
        model.trim()
    }
}

fn strip_known_vendor_prefix(model: &str) -> &str {
    let Some((vendor, rest)) = split_vendor_prefix(model) else {
        return model.trim();
    };
    let known = [
        "anthropic",
        "openai",
        "google",
        "moonshot",
        "moonshotai",
        "kimi",
        "zai",
        "z-ai",
        "zai-org",
        "qwen",
        "deepseek",
        "minimax",
    ];
    if known
        .iter()
        .any(|candidate| vendor.eq_ignore_ascii_case(candidate))
    {
        rest
    } else {
        model.trim()
    }
}

pub fn detect_vendor(model: &str) -> Option<&'static str> {
    let bare = split_vendor_prefix(model)
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| model.trim());
    let lower = bare.to_ascii_lowercase();
    if lower.starts_with("claude-") || lower.contains("/claude-") {
        Some("anthropic")
    } else if lower.starts_with("gpt-") || lower.starts_with("o1") || lower.starts_with("o3") {
        Some("openai")
    } else if lower.starts_with("minimax") {
        Some("minimax")
    } else if lower.starts_with("glm-") {
        Some("z-ai")
    } else if lower.starts_with("kimi-") {
        Some("moonshotai")
    } else if lower.starts_with("qwen") {
        Some("qwen")
    } else if lower.starts_with("gemini-") {
        Some("google")
    } else if lower.starts_with("deepseek-") {
        Some("deepseek")
    } else {
        None
    }
}

fn normalize_for_aggregator(model: &str) -> String {
    let trimmed = model.trim();
    if trimmed.contains('/') {
        return trimmed.to_string();
    }
    match detect_vendor(trimmed) {
        Some(vendor) => format!("{vendor}/{trimmed}"),
        None => trimmed.to_string(),
    }
}

fn normalize_for_copilot(model: &str) -> String {
    claude_dash_version_to_dot(strip_known_vendor_prefix(model))
}

fn claude_dash_version_to_dot(model: &str) -> String {
    let lower = model.trim().to_ascii_lowercase();
    if !lower.starts_with("claude-") {
        return model.trim().to_string();
    }
    let parts: Vec<&str> = lower.split('-').collect();
    if parts.len() < 4 {
        return lower;
    }
    let last = parts[parts.len() - 1];
    let prev = parts[parts.len() - 2];
    if last.len() <= 2
        && prev.len() <= 2
        && last.chars().all(|c| c.is_ascii_digit())
        && prev.chars().all(|c| c.is_ascii_digit())
    {
        let mut normalized: Vec<String> = parts[..parts.len() - 2]
            .iter()
            .map(|part| (*part).to_string())
            .collect();
        normalized.push(format!("{prev}.{last}"));
        normalized.join("-")
    } else {
        lower
    }
}

pub fn normalize_for_deepseek(model: &str) -> String {
    let bare = strip_if_vendor_matches(model, &["deepseek"])
        .trim()
        .to_ascii_lowercase();
    if matches!(bare.as_str(), "deepseek-chat" | "deepseek-reasoner") {
        return bare;
    }
    if let Some(rest) = bare.strip_prefix("deepseek-v") {
        if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return bare;
        }
    }
    if bare.contains("r1")
        || bare.contains("reason")
        || bare.contains("think")
        || bare.contains("cot")
    {
        "deepseek-reasoner".to_string()
    } else {
        "deepseek-chat".to_string()
    }
}

fn normalize_for_opencode_zen(model: &str) -> String {
    let bare = strip_if_vendor_matches(model, &["opencode-zen", "zen"]);
    let lower = bare.to_ascii_lowercase();
    if lower.starts_with("claude-") || lower.starts_with("anthropic/claude-") {
        normalize_anthropic_model_name(bare, false)
    } else {
        bare.to_string()
    }
}

/// Normalize a model ID for the target provider API.
pub fn normalize_model_for_provider(model: &str, provider: &str) -> String {
    let provider = canonical_provider(provider);
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    match provider.as_str() {
        "anthropic" => normalize_anthropic_model_name(trimmed, false),
        "openrouter" | "nous" | "nous-api" => normalize_for_aggregator(trimmed),
        "opencode-go" => strip_if_vendor_matches(trimmed, &["opencode-go", "go"]).to_string(),
        "opencode-zen" => normalize_for_opencode_zen(trimmed),
        "copilot" | "copilot-acp" => normalize_for_copilot(trimmed),
        "openai" | "openai-codex" | "codex" => {
            strip_if_vendor_matches(trimmed, &["openai"]).to_string()
        }
        "deepseek" => normalize_for_deepseek(trimmed),
        "zai" => strip_if_vendor_matches(trimmed, &["zai", "z-ai", "z_ai", "zhipu"]).to_string(),
        "kimi" => strip_if_vendor_matches(trimmed, &["moonshot", "moonshotai", "kimi"]).to_string(),
        "qwen" => strip_if_vendor_matches(trimmed, &["qwen", "alibaba"]).to_string(),
        _ if DOT_TO_HYPHEN_PROVIDERS.contains(&provider.as_str()) => trimmed.replace('.', "-"),
        _ => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{detect_vendor, normalize_for_deepseek, normalize_model_for_provider};

    #[test]
    fn opencode_go_preserves_dot_versions() {
        for model in [
            "minimax-m2.7",
            "minimax-m2.5",
            "glm-4.5",
            "kimi-k2.5",
            "some-model-1.0.3",
        ] {
            assert_eq!(normalize_model_for_provider(model, "opencode-go"), model);
        }
    }

    #[test]
    fn anthropic_strips_vendor_and_converts_dots() {
        assert_eq!(
            normalize_model_for_provider("anthropic/claude-sonnet-4.6", "anthropic"),
            "claude-sonnet-4-6"
        );
        assert_eq!(
            normalize_model_for_provider("claude-opus-4.5", "anthropic"),
            "claude-opus-4-5"
        );
    }

    #[test]
    fn opencode_zen_preserves_non_claude_dots_but_hyphenates_claude() {
        assert_eq!(
            normalize_model_for_provider("opencode-zen/claude-opus-4.5", "opencode-zen"),
            "claude-opus-4-5"
        );
        assert_eq!(
            normalize_model_for_provider("opencode-zen/glm-5.1", "opencode-zen"),
            "glm-5.1"
        );
        assert_eq!(
            normalize_model_for_provider("minimax-m2.5-free", "opencode-zen"),
            "minimax-m2.5-free"
        );
    }

    #[test]
    fn copilot_uses_bare_dot_notation() {
        let cases = [
            ("anthropic/claude-opus-4.6", "claude-opus-4.6"),
            ("anthropic/claude-sonnet-4-6", "claude-sonnet-4.6"),
            ("claude-opus-4-6", "claude-opus-4.6"),
            ("openai/gpt-5.4", "gpt-5.4"),
            ("gpt-5-mini", "gpt-5-mini"),
        ];
        for (input, expected) in cases {
            assert_eq!(normalize_model_for_provider(input, "copilot"), expected);
            assert_eq!(normalize_model_for_provider(input, "copilot-acp"), expected);
        }
    }

    #[test]
    fn aggregators_prepend_detected_vendor_for_bare_models() {
        assert_eq!(
            normalize_model_for_provider("claude-sonnet-4.6", "openrouter"),
            "anthropic/claude-sonnet-4.6"
        );
        assert_eq!(
            normalize_model_for_provider("gpt-5.4", "nous"),
            "openai/gpt-5.4"
        );
        assert_eq!(
            normalize_model_for_provider("gpt-5.4", "nous-api"),
            "openai/gpt-5.4"
        );
        assert_eq!(
            normalize_model_for_provider("anthropic/claude-sonnet-4.6", "openrouter"),
            "anthropic/claude-sonnet-4.6"
        );
    }

    #[test]
    fn native_provider_prefixes_strip_only_on_matching_provider() {
        assert_eq!(
            normalize_model_for_provider("zai/glm-5.1", "zai"),
            "glm-5.1"
        );
        assert_eq!(
            normalize_model_for_provider("google/gemini-2.5-pro", "gemini"),
            "google/gemini-2.5-pro"
        );
        assert_eq!(
            normalize_model_for_provider("moonshot/kimi-k2.5", "kimi-coding"),
            "kimi-k2.5"
        );
        assert_eq!(
            normalize_model_for_provider("Qwen/Qwen3.5-397B-A17B", "huggingface"),
            "Qwen/Qwen3.5-397B-A17B"
        );
        assert_eq!(
            normalize_model_for_provider("modal/zai-org/GLM-5-FP8", "custom"),
            "modal/zai-org/GLM-5-FP8"
        );
    }

    #[test]
    fn detect_vendor_covers_catalog_bare_names() {
        assert_eq!(detect_vendor("claude-sonnet-4.6"), Some("anthropic"));
        assert_eq!(detect_vendor("gpt-5.4-mini"), Some("openai"));
        assert_eq!(detect_vendor("minimax-m2.7"), Some("minimax"));
        assert_eq!(detect_vendor("glm-4.5"), Some("z-ai"));
        assert_eq!(detect_vendor("kimi-k2.5"), Some("moonshotai"));
    }

    #[test]
    fn deepseek_v_series_passes_through_and_reasoners_fold() {
        for model in [
            "deepseek-v4-pro",
            "deepseek-v4-flash",
            "deepseek/deepseek-v4-pro",
            "DeepSeek-V4-Pro",
            "deepseek-v10-ultra",
        ] {
            assert_eq!(
                normalize_for_deepseek(model),
                model.split('/').next_back().unwrap().to_ascii_lowercase()
            );
        }
        assert_eq!(normalize_for_deepseek("deepseek-chat"), "deepseek-chat");
        assert_eq!(normalize_for_deepseek("DEEPSEEK-CHAT"), "deepseek-chat");
        for model in [
            "deepseek-r1",
            "deepseek-r1-0528",
            "deepseek-think-v3",
            "deepseek-reasoning-preview",
            "deepseek-cot-experimental",
        ] {
            assert_eq!(normalize_for_deepseek(model), "deepseek-reasoner");
        }
        assert_eq!(normalize_for_deepseek("unknown-model"), "deepseek-chat");
        assert_eq!(
            normalize_model_for_provider("deepseek-v4-pro", "deepseek"),
            "deepseek-v4-pro"
        );
    }
}
