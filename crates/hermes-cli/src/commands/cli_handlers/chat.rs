//! CLI chat subcommand handler.

use std::sync::Arc;

use hermes_agent::RunConversationParams;
use hermes_core::AgentError;
use hermes_tools::tools::messaging::MessagingSessionContext;

use super::super::model::{
    rank_catalog_model_candidates, resolve_catalog_model_candidate, split_provider_model,
};
use crate::model_switch::{normalize_provider_model, provider_model_ids};
pub(crate) fn resolve_cli_chat_provider_model(
    config_model: Option<&str>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
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

pub(crate) fn apply_cli_chat_runtime_env(provider_model: &str) {
    let provider_model = provider_model.trim();
    if provider_model.is_empty() {
        return;
    }
    crate::env_vars::set_var("HERMES_MODEL", provider_model);
    crate::env_vars::set_var("HERMES_INFERENCE_MODEL", provider_model);
    if let Some((provider, _)) = provider_model.split_once(':') {
        let provider = provider.trim();
        if !provider.is_empty() {
            crate::env_vars::set_var("HERMES_INFERENCE_PROVIDER", provider);
            if std::env::var_os("HERMES_TUI_PROVIDER").is_some() {
                crate::env_vars::set_var("HERMES_TUI_PROVIDER", provider);
            }
        }
    }
}

const QUERY_ALLOW_TOOLS_ENV_KEY: &str = "HERMES_QUERY_ALLOW_TOOLS";
const QUERY_DISABLE_TOOLS_ENV_KEY: &str = "HERMES_QUERY_DISABLE_TOOLS";

pub(crate) fn query_mode_tools_enabled(query_mode: bool, allow_tools_flag: bool) -> bool {
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

fn query_mode_model_not_found(err: &hermes_core::AgentError) -> bool {
    let msg = err.to_string().to_ascii_lowercase();
    (msg.contains("model") && msg.contains("not found"))
        || msg.contains("requested model does not exist")
        || msg.contains("openrouter catalog")
}

async fn query_mode_remediation_target(provider_model: &str) -> Option<(String, Vec<String>)> {
    let (provider, model_id) = split_provider_model(provider_model);
    let provider = provider.trim().to_ascii_lowercase();
    if provider.is_empty() || model_id.trim().is_empty() {
        return None;
    }
    let catalog = provider_model_ids(&provider).await;
    if catalog.is_empty() {
        return None;
    }
    let close = rank_catalog_model_candidates(model_id.trim(), &catalog, 5);
    let selected = resolve_catalog_model_candidate(model_id.trim(), &catalog)
        .or_else(|| close.first().cloned())
        .or_else(|| catalog.first().cloned())?;
    let next = format!("{}:{}", provider, selected.trim());
    if next.eq_ignore_ascii_case(provider_model) {
        return None;
    }
    Some((next, close))
}

/// Handle `hermes chat [--query ...] [--preload-skill ...] [--yolo] [--plan]`.
pub async fn handle_cli_chat(
    query: Option<String>,
    preload_skill: Option<String>,
    yolo: bool,
    plan: bool,
    model_override: Option<String>,
    provider_override: Option<String>,
    allow_tools_flag: bool,
) -> Result<(), hermes_core::AgentError> {
    use crate::runtime_tool_wiring::{wire_cron_scheduler_backend, wire_stdio_clarify_backend};
    use crate::terminal_backend::build_terminal_backend;
    use crate::tool_preview::{build_tool_preview_from_value, tool_emoji};
    use hermes_config::load_config;
    use hermes_core::MessageRole;
    use hermes_cron::cron_scheduler_for_data_dir;
    use hermes_skills::{FileSkillStore, SkillManager};
    use hermes_tools::ToolRegistry;

    if let Some(skill) = &preload_skill {
        println!("[Preloading skill: {}]", skill);
    }
    if yolo {
        println!("[YOLO mode: tool confirmations disabled]");
    }
    if plan {
        println!("[Plan mode: read-only planning until plan is approved]");
    }

    let mut config =
        load_config(None).map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;

    if yolo {
        config.approval.require_approval = false;
    }

    let query_mode = query.is_some();
    let tools_enabled = query_mode_tools_enabled(query_mode, allow_tools_flag);
    if query_mode && !tools_enabled {
        println!(
            "[Query mode tools are disabled by {}=1. Unset it or pass --allow-tools to re-enable.]",
            QUERY_DISABLE_TOOLS_ENV_KEY
        );
    }

    let current_model = resolve_cli_chat_provider_model(
        config.model.as_deref(),
        model_override.as_deref(),
        provider_override.as_deref(),
    )?;
    apply_cli_chat_runtime_env(&current_model);

    let tool_registry = Arc::new(ToolRegistry::new());
    let tool_schemas = if tools_enabled {
        let terminal_backend = build_terminal_backend(&config);
        let skill_store = Arc::new(FileSkillStore::new(FileSkillStore::default_dir()));
        let skill_provider: Arc<dyn hermes_core::SkillProvider> =
            Arc::new(SkillManager::new(skill_store));
        hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
        let live_count =
            crate::live_messaging::enable_live_messaging_tool(&config, &tool_registry).await;
        if live_count > 0 {
            println!(
                "[send_message live delivery enabled via {} configured adapter(s)]",
                live_count
            );
        }
        wire_stdio_clarify_backend(&tool_registry);
        let cron_data_dir = hermes_config::cron_dir();
        std::fs::create_dir_all(&cron_data_dir)
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
        let cron_scheduler = Arc::new(cron_scheduler_for_data_dir(cron_data_dir));
        cron_scheduler
            .load_persisted_jobs()
            .await
            .map_err(|e| hermes_core::AgentError::Config(format!("cron load: {e}")))?;
        cron_scheduler.start().await;
        wire_cron_scheduler_backend(
            &tool_registry,
            cron_scheduler,
            MessagingSessionContext::new(),
        );
        crate::platform_toolsets::resolve_platform_tool_schemas(&config, "cli", &tool_registry)
    } else {
        Vec::new()
    };
    let agent_tool_registry = Arc::new(crate::app::bridge_tool_registry(&tool_registry));

    let build_query_agent = |provider_model: &str| {
        let on_tool_start: Box<dyn Fn(&str, &serde_json::Value) + Send + Sync> =
            Box::new(move |name: &str, args: &serde_json::Value| {
                let emoji = tool_emoji(name);
                let preview = build_tool_preview_from_value(name, args, 56).unwrap_or_default();
                if preview.is_empty() {
                    println!("┊ {emoji} {name}");
                } else {
                    println!("┊ {emoji} {name:<16} {preview}");
                }
            });
        let on_tool_complete: Box<dyn Fn(&str, &str) + Send + Sync> =
            Box::new(move |name: &str, result: &str| {
                let mut snippet: String = result.trim().chars().take(96).collect();
                if result.trim().chars().count() > 96 {
                    snippet.push_str("...");
                }
                let emoji = tool_emoji(name);
                if snippet.is_empty() {
                    println!("┊ {emoji} {name:<16} done");
                } else {
                    println!("┊ {emoji} {name:<16} done: {snippet}");
                }
            });
        let callbacks = hermes_agent::AgentCallbacks {
            on_tool_start: Some(on_tool_start),
            on_tool_complete: Some(on_tool_complete),
            ..Default::default()
        };
        let agent_config = crate::app::build_agent_config(&config, provider_model);
        let provider = crate::app::build_provider(&config, provider_model);
        let base =
            hermes_agent::AgentLoop::new(agent_config, Arc::clone(&agent_tool_registry), provider)
                .with_async_tool_dispatch(crate::app::async_tool_dispatch_for(
                    tool_registry.clone(),
                ))
                .with_callbacks(callbacks);
        if query_mode {
            hermes_agent::attach_discovered_plugins(base)
        } else {
            hermes_agent::attach_agent_runtime(base)
        }
    };

    match query {
        Some(q) => {
            let mut active_model = current_model.clone();
            if let Some((next_model, close)) = query_mode_remediation_target(&active_model).await {
                println!(
                    "[Model remediation: {} -> {}. Close matches: {}]",
                    active_model,
                    next_model,
                    if close.is_empty() {
                        "(none)".to_string()
                    } else {
                        close.join(", ")
                    }
                );
                active_model = next_model;
            }
            apply_cli_chat_runtime_env(&active_model);
            let agent = build_query_agent(&active_model);
            if plan {
                agent.set_plan_phase(hermes_tools::PlanPhase::Planning);
            }
            let result = match agent
                .run_conversation(RunConversationParams {
                    user_message: q.clone(),
                    conversation_history: vec![],
                    task_id: None,
                    stream_callback: None,
                    persist_user_message: None,
                    tools: Some(tool_schemas.clone()),
                    persist_session: false,
                })
                .await
            {
                Ok(conv) => conv.into_loop_result(),
                Err(err) => {
                    if query_mode_model_not_found(&err) {
                        if let Some((next_model, close)) =
                            query_mode_remediation_target(&active_model).await
                        {
                            return Err(hermes_core::AgentError::Config(format!(
                                "{}\nModel remediation suggestion: {} -> {} (close matches: {})",
                                err,
                                active_model,
                                next_model,
                                if close.is_empty() {
                                    "(none)".to_string()
                                } else {
                                    close.join(", ")
                                }
                            )));
                        }
                    }
                    return Err(err);
                }
            };

            if result.turn_exit_reason == "plan_awaiting_approval" {
                if let Some(plan_text) = result.plan_pending.as_deref() {
                    println!("--- Plan (awaiting approval) ---\n{plan_text}\n---");
                    println!(
                        "Plan mode paused. In interactive mode use /plan-mode approve. \
                         One-shot: re-run with approval context."
                    );
                }
            } else {
                let reply = result
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
                    .unwrap_or_else(|| "(no assistant reply)".to_string());
                println!("{}", reply);
            }
        }
        None => {
            println!("Starting interactive chat session...");
            println!("(Use `hermes` for the default interactive TUI)");
        }
    }
    Ok(())
}
