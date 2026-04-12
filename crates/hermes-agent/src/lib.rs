//! # hermes-agent
//!
//! Core agent loop engine — orchestrates LLM calls, tool execution, and
//! context management into a fully autonomous loop that runs until the
//! model finishes naturally or the turn budget is exhausted.

pub mod agent_loop;
pub mod api_bridge;
pub mod budget;
pub mod context;
pub mod compression;
pub mod context_files;
pub mod credential_pool;
pub mod fallback;
pub mod honcho_provider;
pub mod interrupt;
pub mod memory_manager;
pub mod provider;
pub mod providers_extra;
pub mod oauth;
pub mod rate_limit;
pub mod reasoning;
pub mod session_persistence;
pub mod plugins;
pub mod skill_orchestrator;
pub mod subdirectory_hints;

// Re-export primary agent types
pub use agent_loop::{AgentConfig, AgentLoop, TurnMetrics};

// Re-export context management
pub use context::{ContextManager, SystemPromptBuilder, load_soul_md, load_soul_md_from, switch_personality, load_context_files};
pub use compression::summarize_messages_with_llm;

// Re-export budget enforcement
pub use budget::{check_aggregate_budget, enforce_budget, truncate_result};

// Re-export LLM providers
pub use provider::{
    AnthropicProvider, GenericProvider, OpenAiProvider, OpenRouterProvider,
};
pub use providers_extra::{
    QwenProvider, KimiProvider, MiniMaxProvider, NousProvider, CopilotProvider,
};
pub use api_bridge::CodexProvider;

// Re-export rate limiting, credential pool, and fallback chain
pub use rate_limit::RateLimitTracker;
pub use credential_pool::CredentialPool;
pub use fallback::FallbackChain;
pub use oauth::{OAuthManager, OAuthToken, TokenFetcher};

// Re-export reasoning parser
pub use reasoning::parse_reasoning;

// Re-export interrupt controller
pub use interrupt::InterruptController;

// Re-export memory manager
pub use memory_manager::{MemoryManager, MemoryProviderPlugin, sanitize_context, build_memory_context_block};

// Re-export plugin system
pub use plugins::{Plugin, PluginManager, PluginMeta};

// Re-export skill orchestrator
pub use skill_orchestrator::SkillOrchestrator;

// Re-export session persistence
pub use session_persistence::SessionPersistence;

// Re-export context files
pub use context_files::{load_hermes_context_files, load_workspace_context, scan_context_content};

// Re-export subdirectory hints
pub use subdirectory_hints::{SubdirectoryHintTracker, generate_project_hints};

// Re-export honcho provider
pub use honcho_provider::HonchoProvider;

// Re-export core types that consumers need
pub use hermes_core::{
    AgentError, AgentResult, BudgetConfig, LlmProvider, Message, StreamChunk, ToolCall, ToolError,
    ToolResult, ToolSchema, UsageStats,
};