pub const KNOWN_PROVIDERS: &[&str] = &[
    "openai",
    "anthropic",
    "openrouter",
    "codex",
    "openai-codex",
    "qwen",
    "qwen-oauth",
    "google-gemini-cli",
    "gemini",
    "kimi",
    "kimi-coding",
    "kimi-coding-cn",
    "minimax",
    "minimax-cn",
    "stepfun",
    "nous",
    "copilot",
    "copilot-acp",
    "ai-gateway",
    "alibaba",
    "alibaba-coding-plan",
    "arcee",
    "bedrock",
    "deepseek",
    "huggingface",
    "kilocode",
    "nvidia",
    "ollama-cloud",
    "ollama-local",
    "llama-cpp",
    "vllm",
    "mlx",
    "apple-ane",
    "sglang",
    "tgi",
    "opencode-go",
    "opencode-zen",
    "xai",
    "xiaomi",
    "zai",
];

pub const OAUTH_CAPABLE_PROVIDERS: &[&str] = &[
    "openai",
    "anthropic",
    "nous",
    "openai-codex",
    "qwen-oauth",
    "google-gemini-cli",
];

pub const MODELS_DEV_MERGED_PROVIDERS: &[&str] = &[
    "opencode-go",
    "opencode-zen",
    "deepseek",
    "kilocode",
    "fireworks",
    "mistral",
    "togetherai",
    "cohere",
    "perplexity",
    "groq",
    "nvidia",
    "huggingface",
    "zai",
    "gemini",
    "google",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCapability {
    pub id: String,
    pub oauth_supported: bool,
    pub models_dev_merged: bool,
    pub managed_tools_supported: bool,
}

pub fn known_providers() -> Vec<&'static str> {
    KNOWN_PROVIDERS.to_vec()
}

pub fn oauth_capable_providers() -> Vec<&'static str> {
    OAUTH_CAPABLE_PROVIDERS.to_vec()
}

pub fn provider_capability_for(provider: &str) -> Option<ProviderCapability> {
    let normalized = provider.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    Some(ProviderCapability {
        id: normalized.clone(),
        oauth_supported: OAUTH_CAPABLE_PROVIDERS
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&normalized)),
        models_dev_merged: MODELS_DEV_MERGED_PROVIDERS
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&normalized)),
        managed_tools_supported: normalized == "nous",
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use serde::Deserialize;

    use super::{
        known_providers, oauth_capable_providers, provider_capability_for,
        MODELS_DEV_MERGED_PROVIDERS,
    };

    #[derive(Debug, Deserialize)]
    struct ProviderSnapshot {
        required_known_provider_ids: Vec<String>,
        required_oauth_provider_ids: Vec<String>,
    }

    fn snapshot_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/parity/upstream-provider-auth-snapshot.json")
    }

    fn load_snapshot() -> ProviderSnapshot {
        let path = snapshot_path();
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {} failed: {}", path.display(), err));
        serde_json::from_str(&raw)
            .unwrap_or_else(|err| panic!("parse {} failed: {}", path.display(), err))
    }

    #[test]
    fn known_provider_registry_covers_upstream_snapshot_required_ids() {
        let snapshot = load_snapshot();
        let known: BTreeSet<String> = known_providers()
            .into_iter()
            .map(|provider| provider.to_string())
            .collect();

        let missing: Vec<String> = snapshot
            .required_known_provider_ids
            .into_iter()
            .filter(|provider| !known.contains(provider))
            .collect();

        assert!(
            missing.is_empty(),
            "missing providers from known_providers: {:?}",
            missing
        );
    }

    #[test]
    fn oauth_capable_provider_registry_matches_upstream_snapshot() {
        let snapshot = load_snapshot();
        let expected: BTreeSet<String> = snapshot.required_oauth_provider_ids.into_iter().collect();
        let actual: BTreeSet<String> = oauth_capable_providers()
            .into_iter()
            .map(|provider| provider.to_string())
            .collect();
        let missing: Vec<String> = expected
            .iter()
            .filter(|provider| !actual.contains(*provider))
            .cloned()
            .collect();
        assert!(
            missing.is_empty(),
            "missing upstream oauth-capable providers: {:?}",
            missing
        );
        assert!(
            actual.contains("openai"),
            "OpenAI OAuth capability should be enabled for Hermes Ultra"
        );
    }

    #[test]
    fn known_provider_registry_has_no_duplicates() {
        let list = known_providers();
        let set: BTreeSet<&str> = list.iter().copied().collect();
        assert_eq!(
            list.len(),
            set.len(),
            "known provider registry contains duplicate ids"
        );
    }

    #[test]
    fn provider_capability_registry_marks_nous_as_oauth_and_managed_tools() {
        let cap = provider_capability_for("nous").expect("nous capability");
        assert!(cap.oauth_supported);
        assert!(cap.managed_tools_supported);
        assert!(!cap.models_dev_merged);
    }

    #[test]
    fn provider_capability_registry_marks_models_dev_merged_providers() {
        let mut missing = Vec::new();
        for provider in MODELS_DEV_MERGED_PROVIDERS {
            let cap = provider_capability_for(provider).expect("capability");
            if !cap.models_dev_merged {
                missing.push(provider.to_string());
            }
        }
        assert!(
            missing.is_empty(),
            "models.dev merged providers missing capability bit: {:?}",
            missing
        );
    }

    #[test]
    fn local_backends_are_known_and_not_oauth() {
        for provider in [
            "ollama-local",
            "llama-cpp",
            "vllm",
            "mlx",
            "apple-ane",
            "sglang",
            "tgi",
        ] {
            let cap = provider_capability_for(provider).expect("capability");
            assert!(
                !cap.oauth_supported,
                "{provider} should not advertise oauth"
            );
            assert!(
                !cap.models_dev_merged,
                "{provider} should not require models.dev merge"
            );
        }
    }
}
