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
use hermes_agent::provider::is_openai_dynamic_model_alias;
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
    if model_id.trim().eq_ignore_ascii_case("dynamic")
        || provider_model.trim().eq_ignore_ascii_case("dynamic")
    {
        return None;
    }
    let runtime_provider = normalize_runtime_provider_name(provider.as_str());
    if matches!(
        runtime_provider.as_str(),
        "openai" | "openai-codex" | "codex"
    ) && is_openai_dynamic_model_alias(model_id)
    {
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

include!("lib_tests.rs");
