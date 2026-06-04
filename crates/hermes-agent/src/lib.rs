//! # hermes-agent
//!
//! Core agent loop engine — orchestrates LLM calls, tool execution, and
//! context management into a fully autonomous loop that runs until the
//! model finishes naturally or the turn budget is exhausted.

pub mod agent_loop;
pub mod api_bridge;
pub mod api_message_oracle;
pub mod api_messages;
pub mod auxiliary_builder;
pub mod budget;
pub mod code_index;
pub mod compression;
pub mod context;
pub mod context_files;
pub mod context_references;
pub mod conversation_loop;
pub mod copilot_acp;
pub mod credential_pool;
pub mod credential_pool_recovery;
pub mod fallback;
pub mod file_mutation_tracker;
pub mod honcho_provider;
pub mod inbound_prepare;
pub mod interrupt;
pub mod iteration_budget;
pub mod lsp_context;
pub mod memory_manager;
pub mod memory_plugins;
pub mod message_sanitization;
pub use message_sanitization as python_alignment;
pub mod oauth;
pub mod plugins;
mod prompt_builder;
pub mod prompt_caching;
pub mod provider;
mod provider_serialize_cache;
pub mod providers_extra;
pub mod rate_limit;
pub mod reasoning;
pub mod session_persistence;
pub mod shell_hooks;
pub mod skill_orchestrator;
pub mod smart_model_routing;
pub mod steer;
pub mod stream_scrubber;
pub mod sub_agent_orchestrator;
pub mod subdirectory_hints;
mod system_prompt;
pub mod tool_guardrails;
pub mod tools_wiring;
pub mod user_interest;
pub mod vision_adapter;
pub mod vision_message_prepare;

// Re-export primary agent types
pub use agent_loop::{
    AgentCallbacks, AgentConfig, AgentLoop, ApiMode, AsyncToolDispatch, CheapModelRouteConfig,
    ErrorClass, RetryConfig, SmartModelRoutingConfig, ToolRegistry, TurnMetrics,
};
pub use conversation_loop::{
    ConversationResult, RunConversationParams, extract_last_assistant_reply,
    split_messages_for_run_conversation,
};

// Re-export context management
pub use compression::summarize_messages_with_llm;
pub use context::{
    ContextManager, SystemPromptBuilder, builtin_personality_descriptions,
    builtin_personality_names, load_context_files, load_soul_md, load_soul_md_from,
    switch_personality,
};

// Re-export budget enforcement
pub use budget::{check_aggregate_budget, enforce_budget, truncate_result};

// Re-export LLM providers
pub use api_bridge::CodexProvider;
pub use api_message_oracle::{
    assert_dual_run_eq, assert_messages_oracle_eq, canonical_messages_json,
};
pub use auxiliary_builder::{
    AuxiliaryBuildParams, AuxiliaryWiringSummary, build_auxiliary_client,
    build_default_auxiliary_client,
};
pub use inbound_prepare::AgentInboundPreparer;
pub use prompt_caching::{
    anthropic_prompt_cache_policy, apply_anthropic_cache_control, build_cache_marker,
    record_prompt_cache_telemetry,
};
pub use provider::{AnthropicProvider, GenericProvider, OpenAiProvider, OpenRouterProvider};
pub use providers_extra::{
    CopilotProvider, KimiProvider, MiniMaxProvider, NousProvider, QwenProvider,
};
pub use tools_wiring::{
    register_builtin_tools as register_agent_builtin_tools,
    register_builtin_tools_with_voice as register_agent_builtin_tools_with_voice,
};
pub use vision_adapter::AuxiliaryVisionAdapter;

// Re-export rate limiting, credential pool, and fallback chain
pub use credential_pool::CredentialPool;
pub use fallback::FallbackChain;
pub use oauth::{OAuthManager, OAuthToken, TokenFetcher};
pub use rate_limit::RateLimitTracker;

// Re-export reasoning parser
pub use reasoning::parse_reasoning;

// Re-export interrupt controller
pub use interrupt::InterruptController;
pub use steer::{PendingSteer, STEER_GUIDANCE_MARKER};

// Re-export memory manager
pub use memory_manager::{
    MemoryManager, MemoryProviderPlugin, build_memory_context_block, sanitize_context,
};
pub use user_interest::{
    ExtractOptions, InterestMemoryPlugin, InterestStore, InterestTopic, TopicStatus,
    extract_signals_from_text, filter_persistable_signals, is_rejected_poi_topic,
    load_interest_snapshot,
};

// Re-export plugin system
pub use plugins::{Plugin, PluginManager, PluginMeta};
pub use shell_hooks::set_process_accept_hooks;

// Re-export skill orchestrator
pub use skill_orchestrator::SkillOrchestrator;

// Re-export session persistence
pub use session_persistence::{
    SessionFlushCursor, SessionPersistence, leading_system_prompt_for_persist,
};

// Re-export context files
pub use code_index::{CodeIndex, CodeIndexConfig, IndexStats, ReferenceHit, SymbolInfo};
pub use context_files::{load_hermes_context_files, load_workspace_context, scan_context_content};
pub use context_references::{
    ContextReference, ContextReferenceResult, parse_context_references,
    preprocess_context_references_async,
};
pub use lsp_context::{LspContextConfig, build_lsp_context_note};

// Re-export subdirectory hints
pub use subdirectory_hints::{SubdirectoryHintTracker, generate_project_hints};

// Python `run_agent.py` alignment helpers (budget strip/inject, surrogate sanitize)
pub use message_sanitization::{
    CODEX_CONTINUE_USER_MESSAGE, PARTIAL_STREAM_STUB_ID, budget_pressure_text,
    build_partial_stream_stub_response, continuation_prompt_for_response,
    format_partial_stream_tool_call_warning, get_continuation_prompt, has_natural_response_ending,
    inject_budget_pressure_into_last_tool_result, looks_like_codex_intermediate_ack,
    partial_stream_dropped_tool_names, sanitize_surrogates, should_treat_stop_as_truncated,
    strip_budget_warnings_from_messages, strip_system_messages_from_history,
    strip_think_blocks_for_ack,
};

// Re-export sub-agent orchestrator
pub use sub_agent_orchestrator::{
    SubAgentLineage, SubAgentOrchestrator, SubAgentOrchestratorConfig, SubAgentRequest,
    SubAgentStatus,
};

// Re-export honcho provider
pub use honcho_provider::HonchoProvider;

// Re-export core types that consumers need
pub use hermes_core::{
    AgentError, AgentResult, BudgetConfig, LlmProvider, Message, StreamChunk, ToolCall, ToolError,
    ToolResult, ToolSchema, UsageStats,
};

fn default_memory_home() -> String {
    std::env::var("HERMES_HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var("HERMES_AGENT_ULTRA_HOME")
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|home| format!("{home}/.hermes-agent-ultra"))
        })
        .unwrap_or_else(|| ".hermes-agent-ultra".to_string())
}

/// Attach a runtime [`PluginManager`] (config shell hooks + future native plugins).
pub fn attach_discovered_plugins(agent: AgentLoop) -> AgentLoop {
    let hermes_home = agent
        .config()
        .hermes_home
        .clone()
        .unwrap_or_else(default_memory_home);
    let Some(pm) = PluginManager::build_runtime_manager(std::path::Path::new(&hermes_home)) else {
        return agent;
    };
    agent.with_plugins(pm)
}

/// Memory + plugin runtime wiring for CLI, gateway, HTTP, and cron surfaces.
pub fn attach_agent_runtime(agent: AgentLoop) -> AgentLoop {
    attach_discovered_plugins(attach_discovered_memory(agent))
}

/// Attach discovered external memory providers to an `AgentLoop`.
///
/// This keeps ContextLattice-first and multi-provider memory behavior enabled
/// consistently across CLI, gateway, HTTP, and cron execution surfaces.
pub fn attach_discovered_memory(mut agent: AgentLoop) -> AgentLoop {
    let session_id = agent
        .config()
        .session_id
        .clone()
        .unwrap_or_else(|| "session-default".to_string());
    let hermes_home = agent
        .config()
        .hermes_home
        .clone()
        .unwrap_or_else(default_memory_home);
    let memory_nudge_interval = agent.config().memory_nudge_interval;
    let interest = agent.config().interest.clone();

    let interest_store = user_interest::open_interest_store(&hermes_home, &interest);
    if let Some(store) = interest_store.clone() {
        agent = agent.with_interest_store(store);
    }

    if agent.config().skip_memory {
        return agent;
    }

    if let Some(manager) = memory_plugins::build_initialized_memory_manager(
        &session_id,
        &hermes_home,
        memory_nudge_interval,
        &interest,
        interest_store,
    ) {
        agent = agent.with_memory(manager);
    }
    agent
}
