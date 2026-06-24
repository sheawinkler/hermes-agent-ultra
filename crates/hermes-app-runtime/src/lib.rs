//! Non-UI application runtime policy and agent configuration.
//!
//! This crate stays below `hermes-cli`: tests for agent configuration and
//! query-mode policy should not compile terminal UI, platform adapters, cron
//! wiring, or slash-command rendering.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use hermes_agent::agent_loop::{
    CheapModelRouteConfig, RetryConfig, RuntimeProviderConfig, SmartModelRoutingConfig,
    ToolRegistry as AgentToolRegistry,
};
use hermes_agent::smart_model_routing::ApiMode;
use hermes_agent::{AgentCallbacks, AgentConfig, AgentLoop};
use hermes_config::{normalize_service_tier, GatewayConfig, LlmProviderConfig};
use hermes_core::{AgentError, AgentResult, LlmProvider, Message, MessageRole, ToolSchema};
use hermes_intelligence::future_grade_problem_solving_guidance;
use hermes_provider_runtime::{
    active_llm_provider_config, normalize_runtime_provider_name, resolve_provider_and_model,
};
use serde_json::Value;

pub const QUERY_ALLOW_TOOLS_ENV_KEY: &str = "HERMES_QUERY_ALLOW_TOOLS";
pub const QUERY_DISABLE_TOOLS_ENV_KEY: &str = "HERMES_QUERY_DISABLE_TOOLS";
pub const RUNTIME_REFORMULATION_PREFIX: &str = "[HERMES_RUNTIME_REFORMULATION] ";
pub const RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS_DEFAULT: usize = 1_600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryModelRemediation {
    pub next_model: String,
    pub close_matches: Vec<String>,
}

#[derive(Debug)]
pub struct NoninteractiveQueryOutcome {
    pub active_model: String,
    pub result: AgentResult,
    pub reply: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeReformulationObjective {
    pub id: String,
    pub behavior_mode: String,
    pub objective_text: String,
    pub behavior_directives: Vec<String>,
    pub success_criteria: Vec<String>,
}

fn build_retry_config(config: &GatewayConfig) -> RetryConfig {
    let mut retry_cfg = RetryConfig::default();
    if let Some(max_retries) = config.agent.api_max_retries {
        retry_cfg.max_retries = max_retries;
    }
    let mut seen = HashSet::new();

    let mut push_candidate = |candidate: &str, retry_cfg: &mut RetryConfig| {
        let trimmed = candidate.trim();
        if trimmed.is_empty() {
            return;
        }
        let identity = trimmed.to_ascii_lowercase();
        if seen.insert(identity) {
            retry_cfg.fallback_models.push(trimmed.to_string());
        }
    };

    for model in &config.fallback_models {
        push_candidate(model, &mut retry_cfg);
    }
    if let Some(model) = config.fallback_model.as_deref() {
        push_candidate(model, &mut retry_cfg);
    }

    if !retry_cfg.fallback_models.is_empty() {
        retry_cfg.fallback_model = retry_cfg.fallback_models.first().cloned();
    }

    if let Ok(raw) = std::env::var("HERMES_FALLBACK_MODELS") {
        let parsed: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string)
            .collect();
        if !parsed.is_empty() {
            retry_cfg.fallback_models = parsed;
            retry_cfg.fallback_model = retry_cfg.fallback_models.first().cloned();
            return retry_cfg;
        }
    }

    if let Ok(raw) = std::env::var("HERMES_FALLBACK_MODEL") {
        let value = raw.trim();
        if !value.is_empty() {
            retry_cfg.fallback_model = Some(value.to_string());
            retry_cfg.fallback_models = vec![value.to_string()];
        }
    }

    retry_cfg
}

fn parse_provider_api_mode(value: &str) -> Option<ApiMode> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "chat_completions" => Some(ApiMode::ChatCompletions),
        "anthropic_messages" => Some(ApiMode::AnthropicMessages),
        "codex_responses" => Some(ApiMode::CodexResponses),
        "bedrock_converse" => Some(ApiMode::BedrockConverse),
        _ => None,
    }
}

fn configured_agent_max_tokens(provider_config: Option<&LlmProviderConfig>) -> Option<u32> {
    if let Ok(raw) = std::env::var("HERMES_MAX_TOKENS") {
        if let Ok(value) = raw.trim().parse::<u32>() {
            if value > 0 {
                return Some(value);
            }
        }
    }
    provider_config.and_then(|cfg| cfg.max_tokens.filter(|value| *value > 0))
}

pub fn build_agent_config(config: &GatewayConfig, model: &str) -> AgentConfig {
    let (resolved_provider, _) = resolve_provider_and_model(config, model);
    let runtime_provider = normalize_runtime_provider_name(resolved_provider.as_str());
    let provider_config = active_llm_provider_config(
        config,
        resolved_provider.as_str(),
        runtime_provider.as_str(),
    );
    let provider_extra_body = provider_config.and_then(|cfg| cfg.extra_body.clone());
    let max_tokens = configured_agent_max_tokens(provider_config);
    let extra_body =
        merge_service_tier_extra_body(provider_extra_body, config.agent.normalized_service_tier());
    let skip_memory_env = std::env::var("HERMES_SKIP_MEMORY")
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    let skip_context_files_env = std::env::var("HERMES_SKIP_CONTEXT_FILES")
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    let hermes_home = config
        .home_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home);
    let skip_memory = skip_memory_env || hermes_home.join(".memory_disabled").exists();
    let skip_context_files = config.agent.skip_context_files || skip_context_files_env;

    let retry_cfg = build_retry_config(config);
    let max_delegate_depth = config
        .delegation
        .max_spawn_depth
        .map(|depth| depth.max(1))
        .unwrap_or_else(|| AgentConfig::default().max_delegate_depth);

    AgentConfig {
        max_turns: config.max_turns,
        budget: config.budget.clone(),
        model: model.to_string(),
        system_prompt: config.system_prompt.clone(),
        personality: config.personality.clone(),
        extra_body,
        hermes_home: config.home_dir.clone(),
        provider: Some(resolved_provider),
        stream: config.streaming.enabled,
        max_tokens,
        max_delegate_depth,
        delegation_model: config
            .delegation
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        delegation_provider: config
            .delegation
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        delegation_base_url: config
            .delegation
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        delegation_api_key: config
            .delegation
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        skip_memory,
        skip_context_files,
        coding_context: config.agent.coding_context.clone(),
        platform: Some("cli".to_string()),
        enabled_skills: config.skills.enabled.clone(),
        disabled_skills: config.skills.disabled.clone(),
        pass_session_id: true,
        runtime_providers: config
            .llm_providers
            .iter()
            .map(|(name, cfg)| {
                (
                    name.clone(),
                    RuntimeProviderConfig {
                        api_key: cfg.api_key.clone(),
                        api_key_env: cfg.api_key_env.clone(),
                        base_url: cfg.base_url.clone(),
                        request_timeout_seconds: cfg.request_timeout_seconds,
                        api_mode: cfg.api_mode.as_deref().and_then(parse_provider_api_mode),
                        command: cfg.command.clone(),
                        args: cfg.args.clone(),
                        oauth_token_url: cfg.oauth_token_url.clone(),
                        oauth_client_id: cfg.oauth_client_id.clone(),
                    },
                )
            })
            .collect(),
        prefill_messages: hermes_config::load_prefill_messages(config),
        retry: retry_cfg,
        smart_model_routing: SmartModelRoutingConfig {
            enabled: config.smart_model_routing.enabled,
            max_simple_chars: config.smart_model_routing.max_simple_chars,
            max_simple_words: config.smart_model_routing.max_simple_words,
            cheap_model: config.smart_model_routing.cheap_model.as_ref().map(|m| {
                CheapModelRouteConfig {
                    provider: m.provider.clone(),
                    model: m.model.clone(),
                    base_url: m.base_url.clone(),
                    api_key_env: m.api_key_env.clone(),
                }
            }),
        },
        memory_nudge_interval: config.agent.memory_nudge_interval,
        skill_creation_nudge_interval: config.agent.skill_creation_nudge_interval,
        background_review_enabled: config.agent.background_review_enabled,
        code_index_enabled: config.agent.code_index_enabled,
        code_index_max_files: config.agent.code_index_max_files,
        code_index_max_symbols: config.agent.code_index_max_symbols,
        lsp_context_enabled: config.agent.lsp_context_enabled,
        lsp_context_max_chars: config.agent.lsp_context_max_chars,
        ..AgentConfig::default()
    }
}

fn merge_service_tier_extra_body(
    extra_body: Option<Value>,
    service_tier: Option<String>,
) -> Option<Value> {
    let Some(service_tier) = service_tier.and_then(|tier| normalize_service_tier(Some(&tier)))
    else {
        return extra_body;
    };
    let mut map = match extra_body {
        Some(Value::Object(map)) => map,
        Some(other) => {
            let mut map = serde_json::Map::new();
            map.insert("extra_body".to_string(), other);
            map
        }
        None => serde_json::Map::new(),
    };
    map.insert("service_tier".to_string(), Value::String(service_tier));
    Some(Value::Object(map))
}

pub fn resolve_cli_chat_provider_model_with(
    config_model: Option<&str>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
    normalize_provider_model: impl Fn(&str) -> Result<String, AgentError>,
) -> Result<String, AgentError> {
    let provider_override = provider_override
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_ascii_lowercase());
    let model_override = model_override.map(str::trim).filter(|v| !v.is_empty());

    let mut current_model = config_model
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("gpt-4o")
        .to_string();

    if let Some(model) = model_override {
        current_model = model.to_string();
    } else if provider_override.is_none() {
        if let Ok(model_env) = std::env::var("HERMES_INFERENCE_MODEL") {
            let model_env = model_env.trim();
            if !model_env.is_empty() {
                current_model = model_env.to_string();
            }
        }
    }
    if let Some(provider) = provider_override.as_deref() {
        if let Some((_, model_name)) = current_model.split_once(':') {
            current_model = format!("{provider}:{}", model_name.trim());
        } else {
            current_model = format!("{provider}:{}", current_model.trim());
        }
    }
    if !current_model.contains(':') {
        current_model = normalize_provider_model(&current_model)?;
    }
    Ok(current_model)
}

pub fn apply_cli_chat_runtime_env(provider_model: &str) {
    let provider_model = provider_model.trim();
    if provider_model.is_empty() {
        return;
    }
    std::env::set_var("HERMES_MODEL", provider_model);
    std::env::set_var("HERMES_INFERENCE_MODEL", provider_model);
    if let Some((provider, _)) = provider_model.split_once(':') {
        let provider = provider.trim();
        if !provider.is_empty() {
            std::env::set_var("HERMES_INFERENCE_PROVIDER", provider);
            std::env::set_var("HERMES_TUI_PROVIDER", provider);
        }
    }
}

pub fn query_mode_tools_enabled(query_mode: bool, allow_tools_flag: bool) -> bool {
    if !query_mode {
        return true;
    }
    if allow_tools_flag {
        return true;
    }
    if hermes_config::env_var_enabled(QUERY_DISABLE_TOOLS_ENV_KEY) {
        return false;
    }
    // Backward compatible explicit-enable override (now redundant with default-on).
    if hermes_config::env_var_enabled(QUERY_ALLOW_TOOLS_ENV_KEY) {
        return true;
    }
    true
}

pub fn runtime_prompt_reformulation_enabled() -> bool {
    !matches!(
        std::env::var("HERMES_RUNTIME_PROMPT_REFORMULATION")
            .ok()
            .as_deref()
            .map(|v| v.trim().to_ascii_lowercase()),
        Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
    )
}

pub fn runtime_contradiction_self_check_enabled() -> bool {
    !matches!(
        std::env::var("HERMES_RUNTIME_CONTRADICTION_SELF_CHECK")
            .ok()
            .as_deref()
            .map(|v| v.trim().to_ascii_lowercase()),
        Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
    )
}

pub fn runtime_reformulation_prompt_preview_chars() -> usize {
    std::env::var("HERMES_RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS_DEFAULT)
}

pub fn runtime_tool_profile_mode() -> String {
    std::env::var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "balanced".to_string())
}

pub fn runtime_contextlattice_topic_path() -> String {
    std::env::var("CONTEXTLATTICE_TOPIC_PATH")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "runbooks/hermes".to_string())
}

pub fn preview_for_runtime_status(raw: &str, max_chars: usize) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        collapsed
    } else {
        let mut out: String = collapsed
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect();
        out.push('…');
        out
    }
}

fn bullet_lines(lines: &[String], limit: usize) -> String {
    let joined = lines
        .iter()
        .take(limit)
        .map(|line| format!("- {}", line.trim()))
        .filter(|line| line.trim() != "-")
        .collect::<Vec<_>>()
        .join("\n");
    if joined.is_empty() {
        "- (none)".to_string()
    } else {
        joined
    }
}

pub fn build_runtime_reformulation_message(
    latest_user_prompt: &str,
    objective: Option<&RuntimeReformulationObjective>,
) -> Option<String> {
    if !runtime_prompt_reformulation_enabled() {
        return None;
    }
    let prompt = latest_user_prompt.trim();
    if prompt.is_empty() {
        return None;
    }
    let tool_profile_mode = runtime_tool_profile_mode();
    let contradiction_check = runtime_contradiction_self_check_enabled();
    let context_topic = runtime_contextlattice_topic_path();

    let objective_line = objective
        .as_ref()
        .map(|contract| {
            format!(
                "objective(active): {} | behavior={} | text={}",
                contract.id,
                contract.behavior_mode,
                preview_for_runtime_status(&contract.objective_text, 220)
            )
        })
        .unwrap_or_else(|| "objective(active): none".to_string());
    let objective_directives = objective
        .map(|contract| bullet_lines(&contract.behavior_directives, 6))
        .unwrap_or_else(|| "- (none)".to_string());
    let objective_success = objective
        .map(|contract| bullet_lines(&contract.success_criteria, 5))
        .unwrap_or_else(|| "- (none)".to_string());

    let contradiction_line = if contradiction_check {
        "before final response: self-audit contradictions across tool outputs, runtime facts, and claims; unresolved items must be marked UNPROVEN/CONTRADICTORY."
    } else {
        "before final response: consistency self-audit optional (disabled by runtime toggle)."
    };

    let mut out = String::new();
    out.push_str(RUNTIME_REFORMULATION_PREFIX);
    out.push_str(
        "\nRuntime execution reformulation (internal):\n\
         1) apply anti-scheming evidence-first discipline\n\
         2) pull ContextLattice context first when relevant\n\
         3) route tool usage intentionally and avoid repetitive low-signal loops\n\
         4) match requested output shape exactly (count/format), with no template placeholders or duplicate list items\n\
         5) for open-ended missions, execute at least one concrete action before returning status text\n\
         6) maintain iterative objective momentum: gather evidence, test, refine, then continue with next high-value action\n",
    );
    out.push_str(&format!(
        "tool-profile(mode): {}\ncontextlattice(topic): {}\n{}\n",
        tool_profile_mode, context_topic, objective_line
    ));
    out.push_str("objective behavior directives:\n");
    out.push_str(&objective_directives);
    out.push('\n');
    out.push_str("objective success criteria:\n");
    out.push_str(&objective_success);
    out.push('\n');
    out.push_str(
        "objective loop protocol:\n\
         - baseline: state current objective KPI and latest known value\n\
         - execute: perform concrete highest-leverage action now\n\
         - verify: present measurable delta or explicit blocked evidence\n\
         - continue: state next action with no soft deferral\n",
    );
    out.push_str(contradiction_line);
    out.push('\n');
    out.push_str(future_grade_problem_solving_guidance());
    out.push_str("\nuser-request(routing-preview):\n");
    let preview_cap = runtime_reformulation_prompt_preview_chars();
    let prompt_preview = preview_for_runtime_status(prompt, preview_cap);
    out.push_str(&prompt_preview);
    if prompt.chars().count() > preview_cap {
        out.push_str(
            "\n[preview truncated; the full user request remains available as the next user message]",
        );
    } else {
        out.push_str("\n[full user request remains available as the next user message]");
    }
    Some(out)
}

pub fn split_provider_model(provider_model: &str) -> (&str, &str) {
    provider_model
        .split_once(':')
        .unwrap_or(("openai", provider_model))
}

pub fn resolve_catalog_model_candidate(
    requested_model: &str,
    catalog: &[String],
) -> Option<String> {
    if catalog.is_empty() {
        return None;
    }
    let requested_trimmed = requested_model.trim();
    if requested_trimmed.is_empty() {
        return catalog.first().cloned();
    }
    if let Some(hit) = catalog
        .iter()
        .find(|m| m.trim().eq_ignore_ascii_case(requested_trimmed))
    {
        return Some(hit.clone());
    }
    let requested_lc = requested_trimmed.to_ascii_lowercase();
    let slash_suffix = format!("/{requested_lc}");
    if let Some(hit) = catalog.iter().find(|m| {
        let lower = m.trim().to_ascii_lowercase();
        lower.ends_with(&slash_suffix) || lower == requested_lc
    }) {
        return Some(hit.clone());
    }
    rank_catalog_model_candidates(requested_trimmed, catalog, 1)
        .into_iter()
        .next()
}

pub fn rank_catalog_model_candidates(
    requested_model: &str,
    catalog: &[String],
    limit: usize,
) -> Vec<String> {
    if catalog.is_empty() || limit == 0 {
        return Vec::new();
    }
    let requested = requested_model.trim().to_ascii_lowercase();
    if requested.is_empty() {
        return catalog.iter().take(limit).cloned().collect();
    }
    let requested_tail = requested.rsplit('/').next().unwrap_or(requested.as_str());
    let requested_norm: String = requested
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();

    let mut scored: Vec<(usize, usize, String)> = catalog
        .iter()
        .enumerate()
        .filter_map(|(idx, candidate)| {
            let cand_trimmed = candidate.trim();
            if cand_trimmed.is_empty() {
                return None;
            }
            let cand = cand_trimmed.to_ascii_lowercase();
            let cand_tail = cand.rsplit('/').next().unwrap_or(cand.as_str());
            let cand_norm: String = cand.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            let mut score = 0usize;

            if cand == requested {
                score += 10_000;
            }
            if cand_tail == requested_tail {
                score += 8_000;
            }
            if cand.ends_with(&format!("/{}", requested_tail)) {
                score += 6_000;
            }
            if cand.contains(requested_tail) || requested_tail.contains(cand_tail) {
                score += 2_000;
            }

            let shared_prefix = requested_norm
                .chars()
                .zip(cand_norm.chars())
                .take_while(|(a, b)| a == b)
                .count();
            score += shared_prefix.saturating_mul(40);

            let shared_chars = requested_norm
                .chars()
                .filter(|ch| cand_norm.contains(*ch))
                .count();
            score += shared_chars.saturating_mul(12);

            let len_delta = requested_norm.len().abs_diff(cand_norm.len());
            score = score.saturating_sub(len_delta.saturating_mul(4));
            if score == 0 {
                return None;
            }
            Some((score, idx, cand_trimmed.to_string()))
        })
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, candidate)| candidate)
        .collect()
}

pub fn query_mode_remediation_target_from_catalog(
    provider_model: &str,
    catalog: &[String],
) -> Option<QueryModelRemediation> {
    let (provider, model_id) = split_provider_model(provider_model);
    let provider = provider.trim().to_ascii_lowercase();
    if provider.is_empty() || model_id.trim().is_empty() || catalog.is_empty() {
        return None;
    }
    let close_matches = rank_catalog_model_candidates(model_id.trim(), catalog, 5);
    let selected = resolve_catalog_model_candidate(model_id.trim(), catalog)
        .or_else(|| close_matches.first().cloned())
        .or_else(|| catalog.first().cloned())?;
    let next_model = format!("{}:{}", provider, selected.trim());
    if next_model.eq_ignore_ascii_case(provider_model) {
        return None;
    }
    Some(QueryModelRemediation {
        next_model,
        close_matches,
    })
}

pub fn query_mode_model_not_found(err: &AgentError) -> bool {
    let msg = err.to_string().to_ascii_lowercase();
    (msg.contains("model") && msg.contains("not found"))
        || msg.contains("requested model does not exist")
        || msg.contains("openrouter catalog")
}

pub fn assistant_reply_from_result(result: &AgentResult) -> String {
    result
        .messages
        .iter()
        .rev()
        .find_map(|m| {
            if m.role == MessageRole::Assistant {
                m.content.clone()
            } else {
                None
            }
        })
        .unwrap_or_else(|| "(no assistant reply)".to_string())
}

pub async fn run_noninteractive_query(
    config: &GatewayConfig,
    active_model: &str,
    query: &str,
    agent_tool_registry: Arc<AgentToolRegistry>,
    tool_schemas: Vec<ToolSchema>,
    callbacks: AgentCallbacks,
    provider_factory: impl Fn(&GatewayConfig, &str) -> Arc<dyn LlmProvider>,
) -> Result<NoninteractiveQueryOutcome, AgentError> {
    let active_model = active_model.trim().to_string();
    apply_cli_chat_runtime_env(&active_model);
    let agent_config = build_agent_config(config, &active_model);
    let provider = provider_factory(config, &active_model);
    let agent =
        AgentLoop::new(agent_config, agent_tool_registry, provider).with_callbacks(callbacks);
    let result = agent
        .run(vec![Message::user(query)], Some(tool_schemas))
        .await?;
    let reply = assistant_reply_from_result(&result);
    Ok(NoninteractiveQueryOutcome {
        active_model,
        result,
        reply,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::{self, BoxStream};
    use futures::StreamExt;
    use hermes_config::LlmProviderConfig;
    use hermes_core::{LlmResponse, StreamChunk};

    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("env test lock poisoned")
    }

    struct EnvSnapshot {
        vars: Vec<(&'static str, Option<String>)>,
    }

    impl EnvSnapshot {
        fn capture(keys: &[&'static str]) -> Self {
            Self {
                vars: keys
                    .iter()
                    .map(|key| (*key, std::env::var(key).ok()))
                    .collect(),
            }
        }
    }

    impl Drop for EnvSnapshot {
        fn drop(&mut self) {
            for (key, value) in &self.vars {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn test_normalize_provider_model(input: &str) -> Result<String, AgentError> {
        if input.contains(':') {
            Ok(input.to_string())
        } else {
            Ok(format!("openai:{input}"))
        }
    }

    #[test]
    fn test_build_agent_config_maps_runtime_provider_api_key_env() {
        let mut cfg = GatewayConfig::default();
        let mut providers = std::collections::HashMap::new();
        providers.insert(
            "custom".to_string(),
            LlmProviderConfig {
                api_key: None,
                api_key_env: Some("MY_FALLBACK_KEY".to_string()),
                ..LlmProviderConfig::default()
            },
        );
        cfg.llm_providers = providers;

        let agent_cfg = build_agent_config(&cfg, "custom:some-model");
        let runtime = agent_cfg
            .runtime_providers
            .get("custom")
            .expect("runtime provider should exist");
        assert_eq!(runtime.api_key_env.as_deref(), Some("MY_FALLBACK_KEY"));
    }

    #[test]
    fn test_build_agent_config_loads_prefill_messages_from_config() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_PREFILL_MESSAGES_FILE"]);
        std::env::remove_var("HERMES_PREFILL_MESSAGES_FILE");

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("prefill.json"),
            r#"[{"role":"system","content":"cli prefill"},{"role":"user","content":"cli example"}]"#,
        )
        .unwrap();
        let cfg = GatewayConfig {
            home_dir: Some(dir.path().to_string_lossy().to_string()),
            prefill_messages_file: Some("prefill.json".to_string()),
            ..GatewayConfig::default()
        };

        let agent_cfg = build_agent_config(&cfg, "openai:gpt-4o");
        assert_eq!(agent_cfg.prefill_messages.len(), 2);
        assert_eq!(
            agent_cfg.prefill_messages[0].content.as_deref(),
            Some("cli prefill")
        );
        assert_eq!(
            agent_cfg.prefill_messages[1].content.as_deref(),
            Some("cli example")
        );
    }

    #[test]
    fn test_build_agent_config_maps_runtime_provider_request_timeout_seconds() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "anthropic".to_string(),
            LlmProviderConfig {
                api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
                request_timeout_seconds: Some(45.5),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "anthropic:claude-sonnet-4.5");
        let runtime = agent_cfg
            .runtime_providers
            .get("anthropic")
            .expect("runtime provider should exist");

        assert_eq!(runtime.request_timeout_seconds, Some(45.5));
    }

    #[test]
    fn test_build_agent_config_maps_delegation_max_spawn_depth_without_legacy_ceiling() {
        let mut cfg = GatewayConfig::default();
        cfg.delegation.max_spawn_depth = Some(99);
        let agent_cfg = build_agent_config(&cfg, "openai:gpt-4o");
        assert_eq!(agent_cfg.max_delegate_depth, 99);

        cfg.delegation.max_spawn_depth = Some(0);
        let agent_cfg = build_agent_config(&cfg, "openai:gpt-4o");
        assert_eq!(agent_cfg.max_delegate_depth, 1);
    }

    #[test]
    fn test_build_agent_config_maps_delegation_provider_model_runtime_overrides() {
        let mut cfg = GatewayConfig::default();
        cfg.delegation.model = Some(" google/gemini-3-flash-preview ".to_string());
        cfg.delegation.provider = Some(" openrouter ".to_string());
        cfg.delegation.base_url = Some(" http://localhost:1234/v1 ".to_string());
        cfg.delegation.api_key = Some(" local-key ".to_string());

        let agent_cfg = build_agent_config(&cfg, "nous:hermes-3");

        assert_eq!(
            agent_cfg.delegation_model.as_deref(),
            Some("google/gemini-3-flash-preview")
        );
        assert_eq!(agent_cfg.delegation_provider.as_deref(), Some("openrouter"));
        assert_eq!(
            agent_cfg.delegation_base_url.as_deref(),
            Some("http://localhost:1234/v1")
        );
        assert_eq!(agent_cfg.delegation_api_key.as_deref(), Some("local-key"));
    }

    #[test]
    fn test_build_agent_config_preserves_same_host_provider_api_modes() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "codex".to_string(),
            LlmProviderConfig {
                api_key_env: Some("CODEX_KEY".to_string()),
                base_url: Some("https://gateway.example.com/v1".to_string()),
                api_mode: Some("codex_responses".to_string()),
                ..LlmProviderConfig::default()
            },
        );
        cfg.llm_providers.insert(
            "anthropic".to_string(),
            LlmProviderConfig {
                api_key_env: Some("ANTHROPIC_KEY".to_string()),
                base_url: Some("https://gateway.example.com/v1".to_string()),
                api_mode: Some("anthropic_messages".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "codex:gpt-5");
        let codex = agent_cfg
            .runtime_providers
            .get("codex")
            .expect("codex runtime provider should exist");
        let anthropic = agent_cfg
            .runtime_providers
            .get("anthropic")
            .expect("anthropic runtime provider should exist");

        assert_eq!(codex.api_key_env.as_deref(), Some("CODEX_KEY"));
        assert_eq!(
            codex.base_url.as_deref(),
            Some("https://gateway.example.com/v1")
        );
        assert_eq!(codex.api_mode, Some(ApiMode::CodexResponses));
        assert_eq!(anthropic.api_key_env.as_deref(), Some("ANTHROPIC_KEY"));
        assert_eq!(
            anthropic.base_url.as_deref(),
            Some("https://gateway.example.com/v1")
        );
        assert_eq!(anthropic.api_mode, Some(ApiMode::AnthropicMessages));
    }

    #[test]
    fn test_build_agent_config_maps_named_custom_runtime_provider() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "beans".to_string(),
            LlmProviderConfig {
                api_key: Some("sk-beans".to_string()),
                base_url: Some("http://beans.local/v1".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "beans:my-model");
        assert_eq!(agent_cfg.provider.as_deref(), Some("beans"));
        let runtime = agent_cfg
            .runtime_providers
            .get("beans")
            .expect("named custom runtime provider should exist");
        assert_eq!(runtime.api_key.as_deref(), Some("sk-beans"));
        assert_eq!(runtime.base_url.as_deref(), Some("http://beans.local/v1"));
    }

    #[test]
    fn test_build_agent_config_maps_active_provider_max_tokens() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_MAX_TOKENS"]);
        std::env::remove_var("HERMES_MAX_TOKENS");

        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "openrouter".to_string(),
            LlmProviderConfig {
                max_tokens: Some(4096),
                ..LlmProviderConfig::default()
            },
        );
        cfg.llm_providers.insert(
            "openai".to_string(),
            LlmProviderConfig {
                max_tokens: Some(2048),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "openrouter:anthropic/claude-sonnet-4.6");

        assert_eq!(agent_cfg.max_tokens, Some(4096));
    }

    #[test]
    fn test_build_agent_config_maps_normalized_provider_max_tokens_alias() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_MAX_TOKENS"]);
        std::env::remove_var("HERMES_MAX_TOKENS");

        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "openai-codex".to_string(),
            LlmProviderConfig {
                max_tokens: Some(1234),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "codex:gpt-5");

        assert_eq!(agent_cfg.max_tokens, Some(1234));
    }

    #[test]
    fn test_build_agent_config_env_max_tokens_overrides_provider_cap() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_MAX_TOKENS"]);

        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "openrouter".to_string(),
            LlmProviderConfig {
                max_tokens: Some(4096),
                ..LlmProviderConfig::default()
            },
        );

        std::env::set_var("HERMES_MAX_TOKENS", "8192");
        let agent_cfg = build_agent_config(&cfg, "openrouter:anthropic/claude-sonnet-4.6");
        assert_eq!(agent_cfg.max_tokens, Some(8192));

        std::env::set_var("HERMES_MAX_TOKENS", "not-a-number");
        let agent_cfg = build_agent_config(&cfg, "openrouter:anthropic/claude-sonnet-4.6");
        assert_eq!(agent_cfg.max_tokens, Some(4096));

        std::env::set_var("HERMES_MAX_TOKENS", "0");
        let agent_cfg = build_agent_config(&cfg, "openrouter:anthropic/claude-sonnet-4.6");
        assert_eq!(agent_cfg.max_tokens, Some(4096));
    }

    #[test]
    fn test_build_agent_config_forwards_provider_extra_body() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "nous".to_string(),
            LlmProviderConfig {
                extra_body: Some(serde_json::json!({
                    "reasoning_effort": "high",
                    "reasoning": { "effort": "high" }
                })),
                ..LlmProviderConfig::default()
            },
        );
        let agent_cfg = build_agent_config(&cfg, "nous:moonshotai/kimi-k2.6");
        assert_eq!(
            agent_cfg
                .extra_body
                .as_ref()
                .and_then(|body| body.get("reasoning_effort"))
                .and_then(|value| value.as_str()),
            Some("high")
        );
    }

    #[test]
    fn test_build_agent_config_merges_fast_service_tier_into_extra_body() {
        let mut cfg = GatewayConfig::default();
        cfg.agent.service_tier = Some("fast".to_string());
        cfg.llm_providers.insert(
            "nous".to_string(),
            LlmProviderConfig {
                extra_body: Some(serde_json::json!({
                    "reasoning_effort": "medium"
                })),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "nous:moonshotai/kimi-k2.6");
        let body = agent_cfg.extra_body.expect("extra body");
        assert_eq!(body["reasoning_effort"], "medium");
        assert_eq!(body["service_tier"], "priority");
    }

    #[test]
    fn test_build_agent_config_infers_provider_for_bare_model() {
        let mut cfg = GatewayConfig::default();
        cfg.model = Some("claude-opus-4-6".to_string());
        cfg.llm_providers.insert(
            "anthropic".to_string(),
            LlmProviderConfig {
                model: Some("claude-opus-4-6".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "claude-opus-4-6");
        assert_eq!(agent_cfg.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn test_build_agent_config_maps_failover_chain_from_env() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_FALLBACK_MODELS", "HERMES_FALLBACK_MODEL"]);
        std::env::set_var(
            "HERMES_FALLBACK_MODELS",
            "nous:moonshotai/kimi-k2.6,openai:gpt-4o-mini",
        );
        std::env::remove_var("HERMES_FALLBACK_MODEL");
        let cfg = GatewayConfig::default();
        let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");
        assert_eq!(
            agent_cfg.retry.fallback_model.as_deref(),
            Some("nous:moonshotai/kimi-k2.6")
        );
        assert_eq!(
            agent_cfg.retry.fallback_models,
            vec![
                "nous:moonshotai/kimi-k2.6".to_string(),
                "openai:gpt-4o-mini".to_string()
            ]
        );
    }

    #[test]
    fn test_build_agent_config_maps_single_failover_model_from_env() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_FALLBACK_MODELS", "HERMES_FALLBACK_MODEL"]);
        std::env::remove_var("HERMES_FALLBACK_MODELS");
        std::env::set_var("HERMES_FALLBACK_MODEL", "anthropic:claude-3-5-sonnet");
        let cfg = GatewayConfig::default();
        let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");
        assert_eq!(
            agent_cfg.retry.fallback_model.as_deref(),
            Some("anthropic:claude-3-5-sonnet")
        );
        assert_eq!(
            agent_cfg.retry.fallback_models,
            vec!["anthropic:claude-3-5-sonnet".to_string()]
        );
    }

    #[test]
    fn test_build_agent_config_maps_failover_chain_from_config() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_FALLBACK_MODELS", "HERMES_FALLBACK_MODEL"]);
        std::env::remove_var("HERMES_FALLBACK_MODELS");
        std::env::remove_var("HERMES_FALLBACK_MODEL");

        let mut cfg = GatewayConfig::default();
        cfg.fallback_models = vec![
            "openrouter:anthropic/claude-sonnet-4.6".to_string(),
            "nous:Hermes-4".to_string(),
        ];
        cfg.fallback_model = Some("OpenRouter:anthropic/claude-sonnet-4.6".to_string());

        let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");
        assert_eq!(
            agent_cfg.retry.fallback_model.as_deref(),
            Some("openrouter:anthropic/claude-sonnet-4.6")
        );
        assert_eq!(
            agent_cfg.retry.fallback_models,
            vec![
                "openrouter:anthropic/claude-sonnet-4.6".to_string(),
                "nous:Hermes-4".to_string()
            ]
        );
    }

    #[test]
    fn test_build_agent_config_env_failover_overrides_config() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_FALLBACK_MODELS", "HERMES_FALLBACK_MODEL"]);
        std::env::remove_var("HERMES_FALLBACK_MODELS");
        std::env::set_var("HERMES_FALLBACK_MODEL", "anthropic:claude-3-5-sonnet");

        let mut cfg = GatewayConfig::default();
        cfg.fallback_models = vec!["openrouter:backup".to_string()];

        let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");
        assert_eq!(
            agent_cfg.retry.fallback_models,
            vec!["anthropic:claude-3-5-sonnet".to_string()]
        );
    }

    #[test]
    fn test_build_agent_config_maps_agent_api_max_retries() {
        let mut cfg = GatewayConfig::default();
        cfg.agent.api_max_retries = Some(11);

        let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");

        assert_eq!(agent_cfg.retry.max_retries, 11);
    }

    #[test]
    fn resolve_cli_chat_provider_model_defaults_to_config_when_no_overrides() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_INFERENCE_MODEL"]);
        std::env::remove_var("HERMES_INFERENCE_MODEL");
        let resolved = resolve_cli_chat_provider_model_with(
            Some("nous:moonshotai/kimi-k2.6"),
            None,
            None,
            test_normalize_provider_model,
        )
        .expect("resolve");
        assert_eq!(resolved, "nous:moonshotai/kimi-k2.6");
    }

    #[test]
    fn resolve_cli_chat_provider_model_applies_provider_override() {
        let resolved = resolve_cli_chat_provider_model_with(
            Some("gpt-4o"),
            None,
            Some("anthropic"),
            test_normalize_provider_model,
        )
        .expect("resolve");
        assert_eq!(resolved, "anthropic:gpt-4o");
    }

    #[test]
    fn resolve_cli_chat_provider_model_prefers_model_override_with_provider_prefix() {
        let resolved = resolve_cli_chat_provider_model_with(
            Some("openai:gpt-4o"),
            Some("moonshotai/kimi-k2.6"),
            Some("nous"),
            test_normalize_provider_model,
        )
        .expect("resolve");
        assert_eq!(resolved, "nous:moonshotai/kimi-k2.6");
    }

    #[test]
    fn resolve_cli_chat_provider_model_uses_inference_model_env_when_no_flag_override() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_INFERENCE_MODEL"]);
        std::env::set_var("HERMES_INFERENCE_MODEL", "nous:moonshotai/kimi-k2.6");
        let resolved = resolve_cli_chat_provider_model_with(
            Some("openai:gpt-4o"),
            None,
            None,
            test_normalize_provider_model,
        )
        .expect("resolve");
        assert_eq!(resolved, "nous:moonshotai/kimi-k2.6");
    }

    #[test]
    fn resolve_cli_chat_provider_model_normalizes_bare_model() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_INFERENCE_MODEL"]);
        std::env::remove_var("HERMES_INFERENCE_MODEL");
        let resolved = resolve_cli_chat_provider_model_with(
            Some("gpt-4o"),
            None,
            None,
            test_normalize_provider_model,
        )
        .expect("resolve");
        assert_eq!(resolved, "openai:gpt-4o");
    }

    #[test]
    fn apply_cli_chat_runtime_env_sets_provider_model() {
        let _lock = env_test_lock();
        let keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        let _env = EnvSnapshot::capture(&keys);
        for key in keys {
            std::env::remove_var(key);
        }
        std::env::set_var("HERMES_TUI_PROVIDER", "openai");

        apply_cli_chat_runtime_env("nous:openai/gpt-5.5");

        assert_eq!(
            std::env::var("HERMES_MODEL").ok().as_deref(),
            Some("nous:openai/gpt-5.5")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_MODEL").ok().as_deref(),
            Some("nous:openai/gpt-5.5")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
            Some("nous")
        );
        assert_eq!(
            std::env::var("HERMES_TUI_PROVIDER").ok().as_deref(),
            Some("nous")
        );
    }

    #[test]
    fn apply_cli_chat_runtime_env_sets_tui_provider_when_absent() {
        let _lock = env_test_lock();
        let keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        let _env = EnvSnapshot::capture(&keys);
        for key in keys {
            std::env::remove_var(key);
        }

        apply_cli_chat_runtime_env("custom-xuanji:deepseek-v4-pro");

        assert_eq!(
            std::env::var("HERMES_TUI_PROVIDER").ok().as_deref(),
            Some("custom-xuanji")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
            Some("custom-xuanji")
        );
    }

    #[test]
    fn query_mode_tools_enabled_defaults_on_for_query_mode() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&[QUERY_DISABLE_TOOLS_ENV_KEY, QUERY_ALLOW_TOOLS_ENV_KEY]);
        std::env::remove_var(QUERY_DISABLE_TOOLS_ENV_KEY);
        std::env::remove_var(QUERY_ALLOW_TOOLS_ENV_KEY);
        assert!(query_mode_tools_enabled(true, false));
        assert!(query_mode_tools_enabled(false, false));
    }

    #[test]
    fn query_mode_tools_enabled_respects_disable_env_and_flag_override() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&[QUERY_DISABLE_TOOLS_ENV_KEY, QUERY_ALLOW_TOOLS_ENV_KEY]);
        std::env::remove_var(QUERY_ALLOW_TOOLS_ENV_KEY);
        std::env::set_var(QUERY_DISABLE_TOOLS_ENV_KEY, "1");
        assert!(!query_mode_tools_enabled(true, false));
        assert!(query_mode_tools_enabled(true, true));
    }

    #[test]
    fn query_mode_tools_enabled_respects_legacy_allow_env() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&[QUERY_DISABLE_TOOLS_ENV_KEY, QUERY_ALLOW_TOOLS_ENV_KEY]);
        std::env::remove_var(QUERY_DISABLE_TOOLS_ENV_KEY);
        std::env::remove_var(QUERY_ALLOW_TOOLS_ENV_KEY);
        assert!(query_mode_tools_enabled(true, false));
        std::env::set_var(QUERY_ALLOW_TOOLS_ENV_KEY, "1");
        assert!(query_mode_tools_enabled(true, false));
    }

    #[test]
    fn runtime_reformulation_message_includes_objective_and_kernel_guidance() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&[
            "HERMES_RUNTIME_PROMPT_REFORMULATION",
            "HERMES_RUNTIME_CONTRADICTION_SELF_CHECK",
            "HERMES_REPO_REVIEW_TOOL_PROFILE_MODE",
            "CONTEXTLATTICE_TOPIC_PATH",
            "HERMES_RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS",
        ]);
        std::env::set_var("HERMES_RUNTIME_PROMPT_REFORMULATION", "1");
        std::env::set_var("HERMES_RUNTIME_CONTRADICTION_SELF_CHECK", "1");
        std::env::set_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "focus");
        std::env::set_var(
            "CONTEXTLATTICE_TOPIC_PATH",
            "runbooks/objective/test-objective",
        );

        let objective = RuntimeReformulationObjective {
            id: "test-objective".to_string(),
            behavior_mode: "mission".to_string(),
            objective_text: "Grow SOL with controlled risk".to_string(),
            behavior_directives: vec!["Act with evidence".to_string()],
            success_criteria: vec!["Positive risk-adjusted delta".to_string()],
        };
        let injected = build_runtime_reformulation_message(
            "provide 3 more ideas with contextlattice being one",
            Some(&objective),
        )
        .expect("reformulation");

        assert!(injected.contains(RUNTIME_REFORMULATION_PREFIX));
        assert!(injected.contains("tool-profile(mode): focus"));
        assert!(injected.contains("contextlattice(topic): runbooks/objective/test-objective"));
        assert!(injected.contains("objective(active): test-objective | behavior=mission"));
        assert!(injected.contains("- Act with evidence"));
        assert!(injected.contains("- Positive risk-adjusted delta"));
        assert!(injected.contains("UNPROVEN/CONTRADICTORY"));
        assert!(injected.contains("execute at least one concrete action"));
        assert!(injected.contains("Hermes intelligence kernel:"));
        assert!(injected.contains("research synthesis engine:"));
        assert!(injected.contains("ContextLattice memory cycle:"));
        assert!(injected.contains("read back memory"));
        assert!(injected.contains("user-request(routing-preview):"));
        assert!(injected.contains("full user request remains available as the next user message"));
    }

    #[test]
    fn runtime_reformulation_message_respects_toggle_off() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_RUNTIME_PROMPT_REFORMULATION"]);
        std::env::set_var("HERMES_RUNTIME_PROMPT_REFORMULATION", "off");
        assert!(build_runtime_reformulation_message("plain request", None).is_none());
    }

    #[test]
    fn runtime_reformulation_message_truncates_preview_without_losing_user_message() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&[
            "HERMES_RUNTIME_PROMPT_REFORMULATION",
            "HERMES_RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS",
        ]);
        std::env::set_var("HERMES_RUNTIME_PROMPT_REFORMULATION", "1");
        std::env::set_var("HERMES_RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS", "48");

        let long_prompt =
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu".repeat(12);
        let injected =
            build_runtime_reformulation_message(&long_prompt, None).expect("reformulation");
        assert!(injected.contains("user-request(routing-preview):"));
        assert!(injected.contains("preview truncated"));
        assert!(!injected.contains(&long_prompt));
        assert!(
            injected.contains("the full user request remains available as the next user message")
        );
    }

    #[test]
    fn resolve_catalog_model_candidate_prefers_suffix_match() {
        let catalog = vec![
            "nousresearch/hermes-4-405b".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
        ];
        let chosen = resolve_catalog_model_candidate("kimi-k2.6", &catalog).expect("candidate");
        assert_eq!(chosen, "moonshotai/kimi-k2.6");
    }

    #[test]
    fn resolve_catalog_model_candidate_uses_relative_match_for_near_miss() {
        let catalog = vec![
            "qwen/qwen3.6-plus".to_string(),
            "qwen/qwen3.6-max-preview".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
        ];
        let chosen = resolve_catalog_model_candidate("qwen3.6-max", &catalog).expect("candidate");
        assert_eq!(chosen, "qwen/qwen3.6-max-preview");
    }

    #[test]
    fn rank_catalog_model_candidates_returns_best_first() {
        let catalog = vec![
            "qwen/qwen3.6-plus".to_string(),
            "qwen/qwen3.6-max-preview".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
        ];
        let ranked = rank_catalog_model_candidates("qwen3.6-max", &catalog, 2);
        assert_eq!(
            ranked,
            vec![
                "qwen/qwen3.6-max-preview".to_string(),
                "qwen/qwen3.6-plus".to_string()
            ]
        );
    }

    #[test]
    fn query_mode_remediation_target_from_catalog_selects_close_model() {
        let catalog = vec![
            "qwen/qwen3.6-plus".to_string(),
            "qwen/qwen3.6-max-preview".to_string(),
        ];
        let remediation =
            query_mode_remediation_target_from_catalog("openrouter:qwen3.6-max", &catalog)
                .expect("remediation");
        assert_eq!(
            remediation.next_model,
            "openrouter:qwen/qwen3.6-max-preview"
        );
        assert_eq!(
            remediation.close_matches.first().map(String::as_str),
            Some("qwen/qwen3.6-max-preview")
        );
    }

    #[test]
    fn query_mode_model_not_found_detects_provider_shapes() {
        assert!(query_mode_model_not_found(&AgentError::LlmApi(
            "requested model does not exist".to_string()
        )));
        assert!(query_mode_model_not_found(&AgentError::Config(
            "OpenRouter catalog did not include model".to_string()
        )));
        assert!(!query_mode_model_not_found(&AgentError::Config(
            "bad config".to_string()
        )));
    }

    #[test]
    fn assistant_reply_from_result_prefers_last_assistant_message() {
        let result = AgentResult {
            messages: vec![
                Message::assistant("first"),
                Message::user("next"),
                Message::assistant("last"),
            ],
            ..AgentResult::default()
        };
        assert_eq!(assistant_reply_from_result(&result), "last");
    }

    struct FixedProvider {
        reply: &'static str,
    }

    #[async_trait]
    impl LlmProvider for FixedProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            Ok(LlmResponse {
                message: Message::assistant(self.reply),
                usage: None,
                model: model.unwrap_or("test-model").to_string(),
                finish_reason: Some("stop".to_string()),
            })
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            stream::empty().boxed()
        }
    }

    #[tokio::test]
    async fn run_noninteractive_query_uses_injected_provider_factory_and_returns_reply() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&[
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ]);
        let calls = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let calls_for_factory = Arc::clone(&calls);
        let outcome = run_noninteractive_query(
            &GatewayConfig::default(),
            "openai:gpt-5.5",
            "hello",
            Arc::new(AgentToolRegistry::new()),
            Vec::new(),
            AgentCallbacks::default(),
            move |_config, model| {
                calls_for_factory.lock().unwrap().push(model.to_string());
                Arc::new(FixedProvider {
                    reply: "runtime-ok",
                })
            },
        )
        .await
        .expect("query run");

        assert_eq!(outcome.active_model, "openai:gpt-5.5");
        assert_eq!(outcome.reply, "runtime-ok");
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &["openai:gpt-5.5".to_string()]
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
            Some("openai")
        );
    }
}
