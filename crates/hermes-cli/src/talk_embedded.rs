//! Embedded full Hermes runtime for `hermes talk` in-process channel transport.

use std::collections::HashMap;
use std::sync::Arc;

use hermes_agent::{
    RunConversationParams, extract_last_assistant_reply, split_messages_for_run_conversation,
};
use hermes_config::{GatewayConfig, hermes_home, load_config};
use hermes_core::{AgentError, Message};
use hermes_cron::CronCompletionEvent;
use hermes_gateway::GatewayRuntimeContext;
use hermes_talk::{HermesMessage, HermesWorkItem, TalkPushBridge};
use hermes_tools::ToolRegistry;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::app::bridge_tool_registry;
use crate::gateway_main::{GatewayAgentCache, get_or_build_gateway_cached_agent};
use crate::gateway_runtime::{EmbeddedGatewayParts, spawn_embedded_gateway_core};
use crate::moa_wiring::wire_mixture_of_agents_backend;
use crate::platform_toolsets::resolve_platform_tool_schemas;
use crate::runtime_tool_wiring::wire_gateway_clarify_backend;
use crate::state_paths::hermes_state_root_from_home;
use crate::terminal_backend::build_terminal_backend;
use hermes_gateway::tool_backends::ClarifyDispatcher;
use hermes_tools::tools::messaging::MessagingSessionContext;

/// Keeps embedded Hermes + gateway sidecars alive for the voice session lifetime.
pub struct TalkEmbeddedRuntime {
    pub work_tx: mpsc::Sender<HermesWorkItem>,
    _guard: TalkEmbeddedGuard,
}

struct TalkEmbeddedGuard {
    handles: Vec<JoinHandle<()>>,
}

/// Bootstrap tools, cron, gateway, and the in-process Hermes bridge worker.
pub async fn bootstrap_talk_embedded(
    talk_session_key: &str,
    push_bridge: TalkPushBridge,
) -> Result<TalkEmbeddedRuntime, AgentError> {
    let config =
        load_config(None).map_err(|e| AgentError::Config(format!("load config.yaml: {e}")))?;
    let config_arc = Arc::new(config.clone());
    let state_root = hermes_state_root_from_home();
    let (work_tx, work_rx) = mpsc::channel::<HermesWorkItem>(64);

    let tool_registry = Arc::new(ToolRegistry::new());
    let terminal_backend = build_terminal_backend(&config);
    let skills_runtime = crate::skills_runtime::build_skill_provider(true)
        .map_err(|e| AgentError::Config(e.to_string()))?;
    let skill_provider = skills_runtime.provider.clone();
    hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
    wire_mixture_of_agents_backend(&tool_registry, config_arc.clone());

    let _messaging_session = MessagingSessionContext::new();
    let clarify_dispatcher = ClarifyDispatcher::new();
    wire_gateway_clarify_backend(&tool_registry, clarify_dispatcher);

    let agent_tools = Arc::new(bridge_tool_registry(&tool_registry));
    let tool_schemas = resolve_platform_tool_schemas(&config, "talk", &tool_registry);
    let gateway_agent_cache: GatewayAgentCache = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let runtime_tools = tool_registry.clone();

    let EmbeddedGatewayParts {
        mut sidecar_tasks,
        cron_rx,
        ..
    } = spawn_embedded_gateway_core(&config, &state_root, tool_registry.clone()).await?;

    let cron_push = push_bridge.clone();
    sidecar_tasks.push(tokio::spawn(async move {
        run_cron_talk_push_loop(cron_rx, cron_push).await;
    }));

    let bridge_handle = spawn_hermes_bridge_worker(
        work_rx,
        config_arc,
        gateway_agent_cache,
        agent_tools,
        runtime_tools,
        talk_session_key.to_string(),
        tool_schemas,
    );
    sidecar_tasks.push(bridge_handle);

    info!(
        session_key = talk_session_key,
        hermes_home = %hermes_home().display(),
        "talk embedded Hermes runtime ready (channel transport)"
    );

    Ok(TalkEmbeddedRuntime {
        work_tx,
        _guard: TalkEmbeddedGuard {
            handles: sidecar_tasks,
        },
    })
}

async fn run_cron_talk_push_loop(
    mut cron_rx: broadcast::Receiver<CronCompletionEvent>,
    push: TalkPushBridge,
) {
    use tokio::sync::broadcast::error::RecvError;

    loop {
        let ev = match cron_rx.recv().await {
            Ok(ev) => ev,
            Err(RecvError::Lagged(n)) => {
                warn!(n, "talk cron push receiver lagged");
                continue;
            }
            Err(RecvError::Closed) => break,
        };

        let text = format_cron_push_message(&ev);
        push.push(HermesMessage {
            request_id: uuid::Uuid::new_v4().to_string(),
            text,
            status: "final".to_string(),
        })
        .await;
    }
}

fn format_cron_push_message(ev: &CronCompletionEvent) -> String {
    let name = ev
        .job_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&ev.job_id);
    if ev.ok {
        if let Some(snippet) = ev.assistant_snippet.as_deref().filter(|s| !s.is_empty()) {
            format!("定时任务 {name} 完成了，结果是，{snippet}")
        } else {
            format!("定时任务 {name} 已经完成了")
        }
    } else {
        let err = ev.error.as_deref().unwrap_or("未知错误");
        format!("定时任务 {name} 失败了，{err}")
    }
}

fn spawn_hermes_bridge_worker(
    mut work_rx: mpsc::Receiver<HermesWorkItem>,
    config: Arc<GatewayConfig>,
    gateway_agent_cache: GatewayAgentCache,
    agent_tools: Arc<hermes_agent::agent_loop::ToolRegistry>,
    runtime_tools: Arc<ToolRegistry>,
    session_key: String,
    tool_schemas: Vec<hermes_core::ToolSchema>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(item) = work_rx.recv().await {
            let result = execute_talk_hermes_request(
                &config,
                &gateway_agent_cache,
                agent_tools.clone(),
                runtime_tools.clone(),
                &session_key,
                &tool_schemas,
                &item,
            )
            .await;
            let _ = item.respond.send(result);
        }
        info!("talk hermes bridge worker exiting");
    })
}

async fn execute_talk_hermes_request(
    config: &GatewayConfig,
    gateway_agent_cache: &GatewayAgentCache,
    agent_tools: Arc<hermes_agent::agent_loop::ToolRegistry>,
    runtime_tools: Arc<ToolRegistry>,
    session_key: &str,
    tool_schemas: &[hermes_core::ToolSchema],
    item: &HermesWorkItem,
) -> Result<String, hermes_talk::DemoError> {
    use hermes_talk::DemoError;

    let ctx = GatewayRuntimeContext {
        session_key: session_key.to_string(),
        session_id: session_key.to_string(),
        platform: "talk".to_string(),
        chat_id: session_key.to_string(),
        user_id: "talk".to_string(),
        model: item.model.clone().or_else(|| config.model.clone()),
        provider: item.provider.clone(),
        profile: None,
        branch: None,
        personality: None,
        home: Some(hermes_home().to_string_lossy().to_string()),
        service_tier: config.agent.normalized_service_tier(),
        tool_progress: None,
        verbose: false,
        yolo: false,
        reasoning: false,
        mcp_reload_generation: 0,
        deferred_post_delivery_messages: None,
        deferred_post_delivery_released: None,
    };

    let agent = get_or_build_gateway_cached_agent(
        gateway_agent_cache,
        config,
        &ctx,
        agent_tools,
        runtime_tools,
    )
    .await;

    let messages = vec![Message::user(item.text.clone())];

    let (conversation_history, user_message) = split_messages_for_run_conversation(&messages)
        .ok_or_else(|| {
            DemoError::Tool("call_hermes: empty user message for Hermes agent".to_string())
        })?;

    let conv = {
        let agent = agent.lock().await;
        agent
            .run_conversation(RunConversationParams {
                user_message,
                conversation_history,
                task_id: Some(item.request_id.clone()),
                stream_callback: None,
                persist_user_message: None,
                tools: Some(tool_schemas.to_vec()),
                persist_session: true,
            })
            .await
            .map_err(|e| DemoError::Tool(format!("hermes agent error: {e}")))?
    };

    let text = conv
        .final_response
        .clone()
        .or_else(|| extract_last_assistant_reply(conv.messages()))
        .unwrap_or_else(|| "hermes 完成了任务".to_string());
    Ok(text)
}
