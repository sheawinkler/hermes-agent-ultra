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
pub mod registry;
pub mod toolset;
pub mod tools;
pub mod v4a_patch;

// Re-export registry types
pub use registry::{ToolEntry, ToolEntryInfo, ToolRegistry};

// Re-export toolset types
pub use toolset::{ToolsetError, Toolset, ToolsetManager};

// Re-export dispatch
pub use dispatch::{dispatch_tools, dispatch_single, DispatchedResult};

// Re-export approval types
pub use approval::{ApprovalDecision, ApprovalManager, check_approval};

// Re-export credential guard
pub mod credential_guard;
pub use credential_guard::CredentialGuard;

// Re-export all tool handler implementations and their backend traits
pub use tools::web::{WebExtractHandler, WebSearchHandler, WebExtractBackend, WebSearchBackend};
pub use tools::terminal::{ProcessBackend, ProcessHandler, TerminalHandler};
pub use tools::file::{ReadFileHandler, WriteFileHandler, PatchHandler, PatchBackend, SearchFilesHandler, SearchBackend};
pub use tools::browser::{
    BrowserBackend, BrowserBackHandler, BrowserClickHandler, BrowserConsoleHandler,
    BrowserGetImagesHandler, BrowserNavigateHandler, BrowserPressHandler, BrowserScrollHandler,
    BrowserSnapshotHandler, BrowserTypeHandler, BrowserVisionHandler,
};
pub use tools::vision::{VisionAnalyzeHandler, VisionBackend};
pub use tools::image_gen::{ImageGenerateHandler, ImageGenBackend};
pub use tools::skills::{SkillManageHandler, SkillsListHandler, SkillViewHandler};
pub use tools::memory::{MemoryHandler, MemoryBackend};
pub use tools::session_search::{SessionSearchHandler, SessionSearchBackend};
pub use tools::todo::{TodoHandler, TodoBackend};
pub use tools::clarify::{ClarifyHandler, ClarifyBackend};
pub use tools::code_execution::{ExecuteCodeHandler, CodeExecutionBackend};
pub use tools::delegation::{DelegateTaskHandler, DelegationBackend};
pub use tools::cronjob::{CronjobHandler, CronjobBackend};
pub use tools::messaging::{SendMessageHandler, MessagingBackend};
pub use tools::homeassistant::{
    HaCallServiceHandler, HaGetStateHandler, HaListEntitiesHandler, HaListServicesHandler,
    HomeAssistantBackend,
};
pub use tools::tts::{TextToSpeechHandler, TtsBackend};
pub use tools::voice_mode::VoiceModeHandler;
pub use tools::transcription::TranscriptionHandler;
pub use tools::tts_premium::TtsPremiumHandler;
pub use tools::mixture_of_agents::MixtureOfAgentsHandler;
pub use tools::rl_training::RlTrainingHandler;
pub use tools::osv_check::OsvCheckHandler;
pub use tools::url_safety::UrlSafetyHandler;
pub use tools::process_registry::ProcessRegistryHandler;
pub use tools::env_passthrough::EnvPassthroughHandler;
pub use tools::credential_files::CredentialFilesHandler;
pub use tools::managed_tool_gateway::ManagedToolGatewayHandler;
pub use tools::tool_result_storage::ToolResultStorageHandler;

// Re-export real backend implementations
pub use backends::web::{ExaSearchBackend, FallbackSearchBackend, FirecrawlExtractBackend, SimpleExtractBackend};
pub use backends::file::{LocalPatchBackend, LocalSearchBackend};
pub use backends::memory::FileMemoryBackend;
pub use backends::session_search::SqliteSessionSearchBackend;
pub use backends::vision::OpenAiVisionBackend;
pub use backends::image_gen::FalImageGenBackend;
pub use backends::todo::FileTodoBackend;
pub use backends::clarify::SignalClarifyBackend;
pub use backends::code_execution::LocalCodeExecutionBackend;
pub use backends::delegation::{RpcDelegationBackend, SignalDelegationBackend};
pub use backends::cronjob::SignalCronjobBackend;
pub use backends::messaging::SignalMessagingBackend;
pub use backends::homeassistant::HaRestBackend;
pub use backends::tts::MultiTtsBackend;
pub use backends::browser::{CamoFoxBrowserBackend, CdpBrowserBackend};

// Re-export core types needed by consumers
pub use hermes_core::{BudgetConfig, ToolCall, ToolError, ToolHandler, ToolResult, ToolSchema};