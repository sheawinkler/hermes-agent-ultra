//! # hermes-agent
//!
//! Core agent loop engine — orchestrates LLM calls, tool execution, and
//! context management into a fully autonomous loop that runs until the
//! model finishes naturally or the turn budget is exhausted.

pub mod agent_loop;
pub mod api_bridge;
pub mod auxiliary_builder;
pub mod bedrock;
pub mod budget;
pub mod code_index;
pub mod compression;
pub mod context;
pub mod context_files;
pub mod context_references;
pub mod credential_pool;
pub mod fallback;
pub mod honcho_provider;
pub mod interrupt;
pub mod lsp_context;
pub mod memory_manager;
pub mod memory_plugins;
pub mod model_normalize;
pub mod oauth;
pub mod plugins;
pub mod provider;
pub mod providers_extra;
pub mod python_alignment;
pub mod rate_limit;
pub mod reasoning;
pub mod session_persistence;
pub mod skill_orchestrator;
pub mod smart_model_routing;
pub mod sub_agent_orchestrator;
pub mod subdirectory_hints;
pub mod tool_call_args;

// Re-export primary agent types
pub use agent_loop::{
    AgentCallbacks, AgentConfig, AgentLoop, ApiMode, CheapModelRouteConfig, ErrorClass,
    RetryConfig, SmartModelRoutingConfig, TurnMetrics,
};

// Re-export context management
pub use compression::summarize_messages_with_llm;
pub use context::{
    builtin_personality_descriptions, builtin_personality_names, load_context_files, load_soul_md,
    load_soul_md_from, switch_personality, ContextManager, SystemPromptBuilder,
};

// Re-export budget enforcement
pub use budget::{check_aggregate_budget, enforce_budget, truncate_result};

// Re-export LLM providers
pub use api_bridge::CodexProvider;
pub use auxiliary_builder::{
    build_auxiliary_client_with_main_runtime, build_default_auxiliary_client, AuxiliaryMainRuntime,
    AuxiliaryWiringSummary,
};
pub use bedrock::BedrockProvider;
pub use provider::{AnthropicProvider, GenericProvider, OpenAiProvider, OpenRouterProvider};
pub use providers_extra::{
    CopilotProvider, KimiProvider, MiniMaxProvider, NousProvider, QwenProvider,
};

// Re-export rate limiting, credential pool, and fallback chain
pub use credential_pool::CredentialPool;
pub use fallback::FallbackChain;
pub use oauth::{OAuthManager, OAuthToken, TokenFetcher};
pub use rate_limit::RateLimitTracker;

// Re-export reasoning parser
pub use reasoning::parse_reasoning;

// Re-export interrupt controller
pub use interrupt::InterruptController;

// Re-export memory manager
pub use memory_manager::{
    build_memory_context_block, sanitize_context, MemoryManager, MemoryProviderPlugin,
    StreamingContextScrubber,
};

// Re-export plugin system
pub use plugins::{Plugin, PluginManager, PluginMeta};

// Re-export skill orchestrator
pub use skill_orchestrator::SkillOrchestrator;

// Re-export session persistence
pub use session_persistence::{leading_system_prompt_for_persist, SessionPersistence};

// Re-export context files
pub use code_index::{CodeIndex, CodeIndexConfig, IndexStats, ReferenceHit, SymbolInfo};
pub use context_files::{load_hermes_context_files, load_workspace_context, scan_context_content};
pub use context_references::{
    parse_context_references, preprocess_context_references_async, ContextReference,
    ContextReferenceResult,
};
pub use lsp_context::{build_lsp_context_note, LspContextConfig};

// Re-export subdirectory hints
pub use subdirectory_hints::{generate_project_hints, SubdirectoryHintTracker};

// Python `run_agent.py` alignment helpers (budget strip/inject, surrogate sanitize)
pub use python_alignment::{
    budget_pressure_text, inject_budget_pressure_into_last_tool_result,
    looks_like_codex_intermediate_ack, sanitize_surrogates, strip_budget_warnings_from_messages,
    strip_think_blocks_for_ack, CODEX_CONTINUE_USER_MESSAGE,
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

/// Attach discovered external memory providers to an `AgentLoop`.
///
/// This keeps ContextLattice-first and multi-provider memory behavior enabled
/// consistently across CLI, gateway, HTTP, and cron execution surfaces.
pub fn attach_discovered_memory(mut agent: AgentLoop) -> AgentLoop {
    if agent.config.skip_memory {
        return agent;
    }
    let session_id = agent
        .config
        .session_id
        .clone()
        .unwrap_or_else(|| "session-default".to_string());
    let hermes_home = agent
        .config
        .hermes_home
        .clone()
        .unwrap_or_else(default_memory_home);
    if let Some(manager) = memory_plugins::build_initialized_memory_manager(
        &session_id,
        &hermes_home,
        agent.config.memory_nudge_interval,
    ) {
        agent = agent.with_memory(manager);
    }
    agent
}
