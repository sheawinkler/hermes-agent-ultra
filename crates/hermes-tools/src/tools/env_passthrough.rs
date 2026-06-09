use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::OnceLock;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{Value, json};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

static CONFIG_PASSTHROUGH: OnceLock<BTreeSet<String>> = OnceLock::new();

thread_local! {
    static ALLOWED_ENV_VARS: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
}

const HERMES_PROVIDER_ENV_BLOCKLIST: &[&str] = &[
    "OPENAI_BASE_URL",
    "OPENAI_API_KEY",
    "OPENAI_API_BASE",
    "OPENAI_ORG_ID",
    "OPENAI_ORGANIZATION",
    "OPENROUTER_API_KEY",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "LLM_MODEL",
    "GOOGLE_API_KEY",
    "DEEPSEEK_API_KEY",
    "MISTRAL_API_KEY",
    "GROQ_API_KEY",
    "TOGETHER_API_KEY",
    "PERPLEXITY_API_KEY",
    "COHERE_API_KEY",
    "FIREWORKS_API_KEY",
    "XAI_API_KEY",
    "HELICONE_API_KEY",
    "PARALLEL_API_KEY",
    "FIRECRAWL_API_KEY",
    "FIRECRAWL_API_URL",
    "HASS_TOKEN",
    "HASS_URL",
    "GH_TOKEN",
    "MODAL_TOKEN_ID",
    "MODAL_TOKEN_SECRET",
    "DAYTONA_API_KEY",
    "AWS_BEARER_TOKEN_BEDROCK",
];

pub struct EnvPassthroughHandler;

pub fn is_hermes_provider_credential(name: &str) -> bool {
    HERMES_PROVIDER_ENV_BLOCKLIST.contains(&name)
}

/// Register environment variable names as allowed through sandbox env scrubbing.
pub fn register_env_passthrough<I, S>(var_names: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    ALLOWED_ENV_VARS.with(|allowed| {
        let mut allowed = allowed.borrow_mut();
        for name in var_names {
            let name = name.as_ref().trim();
            if name.is_empty() {
                continue;
            }
            if is_hermes_provider_credential(name) {
                tracing::warn!(name, "env passthrough refused Hermes provider credential");
                continue;
            }
            allowed.insert(name.to_string());
        }
    });
}

fn load_config_passthrough() -> &'static BTreeSet<String> {
    CONFIG_PASSTHROUGH.get_or_init(|| {
        hermes_config::load_config(None)
            .map(|cfg| {
                cfg.terminal
                    .env_passthrough
                    .into_iter()
                    .map(|name| name.trim().to_string())
                    .filter(|name| !name.is_empty())
                    .filter(|name| {
                        if is_hermes_provider_credential(name) {
                            tracing::warn!(
                                name,
                                "env passthrough refused Hermes provider credential from config"
                            );
                            false
                        } else {
                            true
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Check whether an environment variable is allowed through sandbox scrubbing.
pub fn is_env_passthrough(var_name: &str) -> bool {
    ALLOWED_ENV_VARS.with(|allowed| allowed.borrow().contains(var_name))
        || load_config_passthrough().contains(var_name)
}

/// Return the union of registered and config-based passthrough variables.
pub fn get_all_passthrough() -> BTreeSet<String> {
    let mut all = load_config_passthrough().clone();
    ALLOWED_ENV_VARS.with(|allowed| {
        all.extend(allowed.borrow().iter().cloned());
    });
    all
}

/// Reset the session-scoped passthrough allowlist.
pub fn clear_env_passthrough() {
    ALLOWED_ENV_VARS.with(|allowed| allowed.borrow_mut().clear());
}

#[async_trait]
impl ToolHandler for EnvPassthroughHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let keys = params
            .get("keys")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'keys'".into()))?;

        let mut out = serde_json::Map::new();
        for key in keys.iter().filter_map(|v| v.as_str()) {
            if let Ok(value) = std::env::var(key) {
                out.insert(key.to_string(), json!(value));
            }
        }
        Ok(Value::Object(out).to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "keys".into(),
            json!({"type":"array","items":{"type":"string"}}),
        );
        tool_schema(
            "env_passthrough",
            "Expose selected env vars to tool workflows.",
            JsonSchema::object(props, vec!["keys".into()]),
        )
    }
}
