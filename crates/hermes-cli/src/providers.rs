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
    "opencode-go",
    "opencode-zen",
    "xai",
    "xiaomi",
    "zai",
];

pub const OAUTH_CAPABLE_PROVIDERS: &[&str] = &[
    "anthropic",
    "nous",
    "openai-codex",
    "qwen-oauth",
    "google-gemini-cli",
];

pub fn known_providers() -> Vec<&'static str> {
    KNOWN_PROVIDERS.to_vec()
}

pub fn oauth_capable_providers() -> Vec<&'static str> {
    OAUTH_CAPABLE_PROVIDERS.to_vec()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use serde::Deserialize;

    use super::{known_providers, oauth_capable_providers};

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
        assert_eq!(actual, expected, "oauth-capable provider mismatch");
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
}
