//! # hermes-tools
//!
//! Tool registry, toolset system, and tool implementations for Hermes Agent.
//!
//! This crate provides:
//! - **ToolRegistry**: Central registry for all available tools with availability checks
//! - **ToolsetManager**: Manages named groups of tools (toolsets) with recursive resolution
//! - **Tool dispatch**: Parallel execution of multiple tool calls with budget enforcement
//! - **Tool implementations**: Concrete handlers for web, terminal, file, browser, and more
//! - **Approval system**: Dangerous command pattern detection for terminal safety

pub mod approval;
pub mod backends;
pub mod dispatch;
pub mod register_builtins;
pub mod registry;
pub mod tool_policy;
pub mod tools;
pub mod toolset;
pub mod toolset_distributions;
pub mod tts_streaming;
pub mod v4a_patch;

// Re-export registry types
pub use registry::{ToolEntry, ToolEntryInfo, ToolRegistry};
pub use tool_policy::{ToolPolicyDecision, ToolPolicyEngine, ToolPolicyMode};

// Re-export toolset types
pub use toolset::{Toolset, ToolsetError, ToolsetManager};

// Re-export dispatch
pub use dispatch::{dispatch_single, dispatch_tools, DispatchedResult};

// Re-export approval types
pub use approval::{check_approval, ApprovalDecision, ApprovalManager};

// Re-export credential guard
pub mod credential_guard;
pub use credential_guard::CredentialGuard;

// Re-export all tool handler implementations and their backend traits
pub use tools::browser::{
    BrowserBackHandler, BrowserBackend, BrowserClickHandler, BrowserConsoleHandler,
    BrowserGetImagesHandler, BrowserNavigateHandler, BrowserPressHandler, BrowserScrollHandler,
    BrowserSnapshotHandler, BrowserTypeHandler, BrowserVisionHandler,
};
pub use tools::clarify::{ClarifyBackend, ClarifyHandler};
pub use tools::code_execution::{CodeExecutionBackend, ExecuteCodeHandler};
pub use tools::credential_files::CredentialFilesHandler;
pub use tools::cronjob::{CronjobBackend, CronjobHandler};
pub use tools::delegation::{DelegateTaskHandler, DelegationBackend};
pub use tools::env_passthrough::EnvPassthroughHandler;
pub use tools::file::{
    PatchBackend, PatchHandler, ReadFileHandler, SearchBackend, SearchFilesHandler,
    WriteFileHandler,
};
pub use tools::homeassistant::{
    HaCallServiceHandler, HaGetStateHandler, HaListEntitiesHandler, HaListServicesHandler,
    HomeAssistantBackend,
};
pub use tools::image_gen::{ImageGenBackend, ImageGenerateHandler};
pub use tools::managed_tool_gateway::ManagedToolGatewayHandler;
pub use tools::memory::{MemoryBackend, MemoryHandler};
pub use tools::messaging::{MessagingBackend, SendMessageHandler};
pub use tools::mixture_of_agents::MixtureOfAgentsHandler;
pub use tools::osv_check::OsvCheckHandler;
pub use tools::process_registry::ProcessRegistryHandler;
pub use tools::session_search::{SessionSearchBackend, SessionSearchHandler};
pub use tools::skill_commands;
pub use tools::skill_utils;
pub use tools::skills::{SkillManageHandler, SkillViewHandler, SkillsListHandler};
pub use tools::terminal::{ProcessBackend, ProcessHandler, TerminalHandler};
pub use tools::todo::{TodoBackend, TodoHandler};
pub use tools::tool_result_storage::ToolResultStorageHandler;
pub use tools::transcription::TranscriptionHandler;
pub use tools::tts::{TextToSpeechHandler, TtsBackend};
pub use tools::tts_premium::TtsPremiumHandler;
pub use tools::url_safety::UrlSafetyHandler;
pub use tools::vision::{VisionAnalyzeHandler, VisionBackend};
pub use tools::voice_mode::VoiceModeHandler;
pub use tools::web::{WebExtractBackend, WebExtractHandler, WebSearchBackend, WebSearchHandler};

// Re-export real backend implementations
pub use backends::browser::{CamoFoxBrowserBackend, CdpBrowserBackend};
pub use backends::clarify::SignalClarifyBackend;
pub use backends::code_execution::LocalCodeExecutionBackend;
pub use backends::cronjob::SignalCronjobBackend;
pub use backends::delegation::{RpcDelegationBackend, SignalDelegationBackend};
pub use backends::file::{LocalPatchBackend, LocalSearchBackend};
pub use backends::homeassistant::HaRestBackend;
pub use backends::image_gen::FalImageGenBackend;
pub use backends::memory::FileMemoryBackend;
pub use backends::messaging::SignalMessagingBackend;
pub use backends::session_search::SqliteSessionSearchBackend;
pub use backends::todo::FileTodoBackend;
pub use backends::tts::MultiTtsBackend;
pub use backends::vision::OpenAiVisionBackend;
pub use backends::web::{
    search_backend_from_env_or_fallback, ExaSearchBackend, FallbackSearchBackend,
    FirecrawlExtractBackend, SimpleExtractBackend, TavilySearchBackend,
};

// Re-export builtin registration helper
pub use register_builtins::register_builtin_tools;

// Re-export core types needed by consumers
pub use hermes_core::{BudgetConfig, ToolCall, ToolError, ToolHandler, ToolResult, ToolSchema};
