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
pub mod checkpoint_manager;
pub mod code_execution_env;
pub mod code_execution_ptc;
pub mod code_execution_stubs;
pub mod concurrency;
pub mod dispatch;
pub mod equity_research_seed;
pub mod kanban;
pub mod kanban_failure;
pub mod media_extract;
pub mod plan_mode;
pub mod register;
pub mod register_builtins;
pub mod registry;
pub mod rtk_filter;
pub mod state_db;
pub mod task_cleanup;
pub mod teams_pipeline;
pub mod terminal_requirements;
pub mod tool_dispatch_helpers;
pub mod tool_policy;
pub mod tools;
pub mod toolset;
pub mod toolset_distributions;
pub mod tts_streaming;
pub mod v4a_patch;
pub mod voice_providers;

pub use media_extract::extract_media;

// Re-export registry types
pub use registry::{ToolEntry, ToolEntryInfo, ToolRegistry};
pub use rtk_filter::RawModeState;
pub use tool_policy::{
    ToolPolicyCounters, ToolPolicyDecision, ToolPolicyEngine, ToolPolicyMode,
    default_tool_policy_counters_path, load_tool_policy_counters, persist_tool_policy_counters,
};

// Re-export toolset types
pub use toolset::{Toolset, ToolsetError, ToolsetManager};

// Re-export dispatch
pub use checkpoint_manager::{CheckpointManager, checkpoint_shadow_dir_id};
pub use dispatch::{DispatchedResult, dispatch_single, dispatch_tools};
pub use equity_research_seed::try_resolve_a_share_from_user_message;
pub use kanban::{KANBAN_TASK_ENV, kanban_block_reason, kanban_task_from_env};
pub use kanban_failure::{
    KanbanFailureOptions, KanbanFailureOutcome, record_iteration_budget_exhausted,
    record_task_failure,
};
pub use plan_mode::{PlanPhase, ToolRwClass, classify_tool, plan_allows_tool, plan_block_payload};
pub use task_cleanup::cleanup_task_resources;
pub use tool_dispatch_helpers::{
    NEVER_PARALLEL_TOOLS, ParallelMode, extract_parallel_scope_path, infer_parallel_mode,
    is_browser_tool, is_destructive_command, paths_overlap, should_parallelize_tool_batch,
};

// Re-export approval types
pub use approval::{ApprovalDecision, ApprovalManager, check_approval};
pub use code_execution_env::{SANDBOX_ALLOWED_TOOLS, prepare_child_env, scrub_child_env};
pub use code_execution_stubs::{RpcTransport, generate_hermes_tools_module};

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
pub use tools::computer_use::{
    ComputerUseHandler, check_computer_use_requirements, ensure_cua_driver_daemon_running,
};
pub use tools::content_framework::{ContentNormalizeHandler, ContentPlanHandler};
pub use tools::credential_files::CredentialFilesHandler;
pub use tools::cronjob::{CronjobBackend, CronjobHandler};
pub use tools::dashboard_control::DashboardControlHandler;
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
pub use tools::session_search::{SessionSearchBackend, SessionSearchHandler, SessionSearchOptions};
pub use tools::skill_commands;
pub use tools::skill_utils;
pub use tools::skills::{SkillManageHandler, SkillViewHandler, SkillsListHandler};
pub use tools::spotify::{
    SpotifyApiRequest, SpotifyBackend, SpotifyHandler, SpotifyHttpMethod, SpotifyTool,
};
pub use tools::terminal::{ProcessBackend, ProcessHandler, TerminalHandler};
pub use tools::todo::{TodoBackend, TodoHandler, TodoItem};
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
pub use backends::spotify::{SpotifyRuntimeCredentials, SpotifyWebApiBackend};
pub use backends::todo::FileTodoBackend;
pub use backends::tts::MultiTtsBackend;
pub use backends::video::VisionFrameSamplingVideoBackend;
pub use backends::video_gen::{
    FalVideoGenBackend, VideoGenBackend, XaiVideoCredentials, XaiVideoGenBackend,
};
#[cfg(feature = "web")]
pub use backends::web::{
    DdgsSearchBackend, ExaSearchBackend, FallbackSearchBackend, FirecrawlExtractBackend,
    SimpleExtractBackend, TavilySearchBackend, search_backend_from_env_or_fallback,
};
pub use state_db::{
    SearchMessageMatch, StateDb, StateDbError, TokenCountUpdate, decode_content_preview,
    sanitize_fts5_query,
};

pub use teams_pipeline::{
    DEFAULT_TEAMS_PIPELINE_STORE_FILENAME, GraphSubscription, HeuristicTeamsSummarizer,
    MeetingArtifact, MicrosoftGraphClient, MicrosoftGraphTeamsBackend, MicrosoftGraphTokenProvider,
    SummaryParts, TeamsGraphBackend, TeamsMeetingPipeline, TeamsMeetingPipelineJob,
    TeamsMeetingRef, TeamsMeetingSummaryPayload, TeamsPipelineConfig, TeamsPipelineError,
    TeamsPipelineResult, TeamsPipelineStore, TeamsSinkKind, TeamsSinkWriter, TeamsSummarizer,
    TeamsTranscriber, TranscriptionToolTeamsTranscriber, build_summary_prompt,
    collect_call_metrics, collect_participants, default_change_type_for_resource,
    expected_client_state, extract_meeting_id_from_resource, heuristic_summary,
    maintain_graph_subscriptions, parse_summary_json, render_summary_markdown,
    resolve_teams_pipeline_store_path, select_preferred_transcript, sync_graph_subscription_record,
};

// Re-export builtin registration helper
pub use register_builtins::{
    VoiceMediaToolConfig, register_builtin_tools, register_builtin_tools_with_vision,
    register_builtin_tools_with_vision_and_voice, register_builtin_tools_with_voice,
};
pub use tools::transcription::transcribe_audio_file;

// Re-export core types needed by consumers
pub use hermes_core::{BudgetConfig, ToolCall, ToolError, ToolHandler, ToolResult, ToolSchema};
