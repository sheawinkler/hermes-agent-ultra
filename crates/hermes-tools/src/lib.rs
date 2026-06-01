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
pub mod rtk_filter;
pub mod teams_pipeline;
pub mod terminal_requirements;
pub mod tool_policy;
pub mod tools;
pub mod toolset;
pub mod toolset_distributions;
pub mod tts_streaming;
pub mod v4a_patch;
pub mod website_policy;

// Re-export registry types
pub use registry::{ToolEntry, ToolEntryInfo, ToolRegistry};
pub use rtk_filter::RawModeState;
pub use tool_policy::{
    default_tool_policy_counters_path, load_tool_policy_counters, persist_tool_policy_counters,
    ToolPolicyCounters, ToolPolicyDecision, ToolPolicyEngine, ToolPolicyMode,
};

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
pub use tools::dashboard_control::DashboardControlHandler;
pub use tools::delegation::{DelegateTaskHandler, DelegationBackend};
pub use tools::disk_cleanup::{DiskCleanup, DiskCleanupAutoTracker, DiskCleanupHandler};
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
pub use tools::spotify::{
    SpotifyApiRequest, SpotifyBackend, SpotifyHandler, SpotifyHttpMethod, SpotifyTool,
};
pub use tools::terminal::{ProcessBackend, ProcessHandler, TerminalHandler};
pub use tools::todo::{TodoBackend, TodoHandler};
pub use tools::tool_result_storage::ToolResultStorageHandler;
pub use tools::transcription::TranscriptionHandler;
pub use tools::tts::{TextToSpeechHandler, TtsBackend};
pub use tools::tts_premium::TtsPremiumHandler;
pub use tools::url_safety::UrlSafetyHandler;
pub use tools::video::{
    VideoAnalyzeHandler, VideoBackend, VideoGenerateBackend, VideoGenerateHandler,
    VideoGenerateRequest,
};
pub use tools::vision::{VisionAnalyzeHandler, VisionBackend};
pub use tools::voice_mode::VoiceModeHandler;
pub use tools::web::{
    WebCrawlBackend, WebCrawlHandler, WebExtractBackend, WebExtractHandler, WebSearchBackend,
    WebSearchHandler,
};

// Re-export real backend implementations
pub use backends::browser::{
    browser_backend_from_env, BrowserUseBrowserBackend, BrowserUseConfig,
    BrowserbaseBrowserBackend, BrowserbaseConfig, CamoFoxBrowserBackend, CdpBrowserBackend,
};
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
pub use backends::spotify::{SpotifyRuntimeCredentials, SpotifyWebApiBackend};
pub use backends::todo::FileTodoBackend;
pub use backends::tts::MultiTtsBackend;
pub use backends::video::VisionFrameSamplingVideoBackend;
pub use backends::video_gen::FalVideoGenBackend;
pub use backends::vision::OpenAiVisionBackend;
pub use backends::web::{
    crawl_backend_from_env_or_fallback, extract_backend_from_env_or_fallback,
    search_backend_from_env_or_fallback, ExaSearchBackend, FallbackCrawlBackend,
    FallbackSearchBackend, FirecrawlExtractBackend, FirecrawlSearchBackend, SimpleExtractBackend,
    TavilyCrawlBackend, TavilyExtractBackend, TavilySearchBackend, XaiWebSearchBackend,
};

pub use teams_pipeline::{
    build_summary_prompt, collect_call_metrics, collect_participants,
    default_change_type_for_resource, expected_client_state, extract_meeting_id_from_resource,
    heuristic_summary, maintain_graph_subscriptions, parse_summary_json, render_summary_markdown,
    resolve_teams_pipeline_store_path, select_preferred_transcript, sync_graph_subscription_record,
    GraphSubscription, HeuristicTeamsSummarizer, MeetingArtifact, MicrosoftGraphClient,
    MicrosoftGraphTeamsBackend, MicrosoftGraphTokenProvider, SummaryParts, TeamsGraphBackend,
    TeamsMeetingPipeline, TeamsMeetingPipelineJob, TeamsMeetingRef, TeamsMeetingSummaryPayload,
    TeamsPipelineConfig, TeamsPipelineError, TeamsPipelineResult, TeamsPipelineStore,
    TeamsSinkKind, TeamsSinkWriter, TeamsSummarizer, TeamsTranscriber,
    TranscriptionToolTeamsTranscriber, DEFAULT_TEAMS_PIPELINE_STORE_FILENAME,
};

// Re-export builtin registration helper
pub use register_builtins::register_builtin_tools;

// Re-export core types needed by consumers
pub use hermes_core::{BudgetConfig, ToolCall, ToolError, ToolHandler, ToolResult, ToolSchema};
