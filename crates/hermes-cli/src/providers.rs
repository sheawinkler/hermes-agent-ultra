pub const KNOWN_PROVIDERS: &[&str] = &[
    "openai",
    "anthropic",
    "openrouter",
    "codex",
    "openai-codex",
    "custom",
    "qwen",
    "qwen-oauth",
    "google-gemini-cli",
    "gemini",
    "kimi",
    "kimi-coding",
    "kimi-coding-cn",
    "minimax",
    "minimax-cn",
    "novita",
    "stepfun",
    "nous",
    "copilot",
    "copilot-acp",
    "ai-gateway",
    "alibaba",
    "alibaba-coding-plan",
    "arcee",
    "azure-foundry",
    "bedrock",
    "deepseek",
    "huggingface",
    "kilocode",
    "gmi",
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
    "tencent-tokenhub",
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

pub fn canonical_provider_id(provider: &str) -> String {
    let normalized = provider.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "codex" => "openai-codex".to_string(),
        "claude" | "claude-code" => "anthropic".to_string(),
        "qwen-cli" | "qwen-portal" => "qwen-oauth".to_string(),
        "gemini-cli" | "gemini-oauth" => "google-gemini-cli".to_string(),
        "google" | "google-gemini" | "google-ai-studio" => "gemini".to_string(),
        "azure" | "azure-ai-foundry" | "azure_ai_foundry" => "azure-foundry".to_string(),
        "step" | "step-plan" => "stepfun".to_string(),
        "moonshot" | "kimi-coding" | "kimi-coding-cn" => "kimi".to_string(),
        "alibaba" | "alibaba-coding-plan" => "qwen".to_string(),
        "minimax_cn" => "minimax-cn".to_string(),
        "novita-ai" | "novitaai" => "novita".to_string(),
        "glm" | "z-ai" | "z_ai" | "zhipu" => "zai".to_string(),
        "aigateway" | "vercel" => "ai-gateway".to_string(),
        "github-copilot" | "github-models" => "copilot".to_string(),
        "github-copilot-acp" | "copilot-acp-agent" => "copilot-acp".to_string(),
        "hf" | "hugging-face" | "huggingface-hub" => "huggingface".to_string(),
        "gmi-cloud" | "gmicloud" => "gmi".to_string(),
        "arcee-ai" | "arceeai" => "arcee".to_string(),
        "mimo" | "xiaomi-mimo" => "xiaomi".to_string(),
        "tencent" | "tokenhub" | "tencent-cloud" | "tencentmaas" => "tencent-tokenhub".to_string(),
        "aws" | "aws-bedrock" | "amazon-bedrock" | "amazon" => "bedrock".to_string(),
        "kilo" | "kilo-code" | "kilo-gateway" => "kilocode".to_string(),
        "opencode" | "opencode-zen" | "zen" => "opencode-zen".to_string(),
        "go" => "opencode-go".to_string(),
        "ollama" => "ollama-local".to_string(),
        "llama.cpp" | "llamacpp" => "llama-cpp".to_string(),
        "ollvm" | "llvm" => "vllm".to_string(),
        "mlx-lm" | "apple-mlx" => "mlx".to_string(),
        "ane" | "apple-neural-engine" | "neural-engine" => "apple-ane".to_string(),
        "text-generation-inference" => "tgi".to_string(),
        _ => normalized,
    }
}

pub fn provider_capability_for(provider: &str) -> Option<ProviderCapability> {
    let normalized = canonical_provider_id(provider);
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
        canonical_provider_id, known_providers, oauth_capable_providers, provider_capability_for,
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

    #[test]
    fn canonical_provider_id_normalizes_common_aliases() {
        assert_eq!(canonical_provider_id("moonshot"), "kimi");
        assert_eq!(canonical_provider_id("gemini-cli"), "google-gemini-cli");
        assert_eq!(canonical_provider_id("ollama"), "ollama-local");
        assert_eq!(canonical_provider_id("llama.cpp"), "llama-cpp");
        assert_eq!(canonical_provider_id("llvm"), "vllm");
        assert_eq!(canonical_provider_id("glm"), "zai");
        assert_eq!(canonical_provider_id("z-ai"), "zai");
        assert_eq!(canonical_provider_id("zhipu"), "zai");
        assert_eq!(canonical_provider_id("github-copilot"), "copilot");
        assert_eq!(canonical_provider_id("github-models"), "copilot");
        assert_eq!(canonical_provider_id("github-copilot-acp"), "copilot-acp");
        assert_eq!(canonical_provider_id("copilot-acp-agent"), "copilot-acp");
        assert_eq!(canonical_provider_id("hf"), "huggingface");
        assert_eq!(canonical_provider_id("hugging-face"), "huggingface");
        assert_eq!(canonical_provider_id("huggingface-hub"), "huggingface");
        assert_eq!(canonical_provider_id("aigateway"), "ai-gateway");
        assert_eq!(canonical_provider_id("vercel"), "ai-gateway");
        assert_eq!(canonical_provider_id("gmi-cloud"), "gmi");
        assert_eq!(canonical_provider_id("gmicloud"), "gmi");
        assert_eq!(canonical_provider_id("google-ai-studio"), "gemini");
        assert_eq!(canonical_provider_id("arcee-ai"), "arcee");
        assert_eq!(canonical_provider_id("arceeai"), "arcee");
        assert_eq!(canonical_provider_id("azure"), "azure-foundry");
        assert_eq!(canonical_provider_id("azure-ai-foundry"), "azure-foundry");
        assert_eq!(canonical_provider_id("mimo"), "xiaomi");
        assert_eq!(canonical_provider_id("xiaomi-mimo"), "xiaomi");
        assert_eq!(canonical_provider_id("tencent-cloud"), "tencent-tokenhub");
        assert_eq!(canonical_provider_id("tokenhub"), "tencent-tokenhub");
        assert_eq!(canonical_provider_id("aws"), "bedrock");
        assert_eq!(canonical_provider_id("aws-bedrock"), "bedrock");
        assert_eq!(canonical_provider_id("amazon-bedrock"), "bedrock");
        assert_eq!(canonical_provider_id("amazon"), "bedrock");
        assert_eq!(canonical_provider_id("minimax_cn"), "minimax-cn");
    }

    #[test]
    fn bundled_model_provider_plugin_dirs_are_known() {
        let plugins_dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/model-providers");
        let mut plugin_dirs = Vec::new();
        for entry in std::fs::read_dir(&plugins_dir)
            .unwrap_or_else(|err| panic!("read {} failed: {}", plugins_dir.display(), err))
        {
            let path = entry.expect("dir entry").path();
            if !path.is_dir() {
                continue;
            }
            assert!(
                path.join("__init__.py").exists(),
                "{} missing __init__.py",
                path.display()
            );
            assert!(
                path.join("plugin.yaml").exists(),
                "{} missing plugin.yaml",
                path.display()
            );
            plugin_dirs.push(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .expect("utf8 provider dir")
                    .to_string(),
            );
        }
        assert!(
            plugin_dirs.len() >= 28,
            "expected at least 28 bundled provider plugins, got {}",
            plugin_dirs.len()
        );

        let known: BTreeSet<String> = known_providers()
            .into_iter()
            .map(|provider| provider.to_string())
            .collect();
        let missing: Vec<String> = plugin_dirs
            .into_iter()
            .filter(|provider| !known.contains(provider))
            .collect();
        assert!(
            missing.is_empty(),
            "bundled model-provider plugin dirs missing from Rust known_providers: {:?}",
            missing
        );
    }
}
