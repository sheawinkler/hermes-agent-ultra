//! Registers all built-in tool handlers into a ToolRegistry.
//!
//! This is the Rust equivalent of Python's `_discover_tools()` in `model_tools.py`.
//! Each tool handler is instantiated with its default backend and registered
//! in the appropriate toolset.

use std::path::PathBuf;
use std::sync::Arc;

use hermes_core::{SkillProvider, TerminalBackend, ToolHandler};
use hermes_environments::ExecutionContainmentContext;


use crate::{ToolRegistry, VettedToolEffect};

/// Register all built-in tool handlers into the given registry.
///
/// `terminal_backend` is shared by terminal, file read/write handlers.
/// `skill_provider` is shared by the three skills handlers.
///
/// Backends that depend on environment variables (e.g. `OPENAI_API_KEY`,
/// `FAL_KEY`, `HASS_URL`) are constructed eagerly; if the vars are absent
/// the tools will return a clear error at *runtime*, not at registration.
/// Register all built-in tool handlers into the given registry.
///
/// This compatibility helper resolves the local memory backend through its
/// legacy default constructor. Production ingress must instead call
/// [`register_builtin_tools_with_home`] or
/// [`register_builtin_tools_with_execution_context_and_home`].
pub fn register_builtin_tools(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
) {
    register_builtin_tools_with_data_dir_and_memory_home(
        registry,
        terminal_backend,
        skill_provider,
        hermes_data_dir(),
        None,
    );
}

/// Register built-in tools using an explicit Hermes home for local memory.
///
/// This binds the memory tool to the same home selected by the ingress, rather
/// than allowing `FileMemoryBackend` to read a process environment fallback.
pub fn register_builtin_tools_with_home(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    hermes_home: PathBuf,
) {
    register_builtin_tools_with_data_dir_and_memory_home(
        registry,
        terminal_backend,
        skill_provider,
        hermes_data_dir(),
        Some(hermes_home),
    );
}

/// Register built-ins with the exact context that policy evaluation and the
/// containment-sensitive terminal handler must share.
///
/// This compatibility helper retains legacy memory-home resolution. Production
/// ingress must use [`register_builtin_tools_with_execution_context_and_home`].
pub fn register_builtin_tools_with_execution_context(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    execution_context: Arc<ExecutionContainmentContext>,
) {
    registry.set_execution_containment_context(execution_context);
    register_builtin_tools(registry, terminal_backend, skill_provider);
}

/// Register built-ins with shared execution containment and an explicit
/// Hermes home for the subject-bound local-memory backend.
pub fn register_builtin_tools_with_execution_context_and_home(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    execution_context: Arc<ExecutionContainmentContext>,
    hermes_home: PathBuf,
) {
    registry.set_execution_containment_context(execution_context);
    register_builtin_tools_with_home(registry, terminal_backend, skill_provider, hermes_home);
}

fn register_builtin_tools_with_data_dir_and_memory_home(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    data_dir: PathBuf,
    memory_home: Option<PathBuf>,
) {
    fn reg(
        registry: &ToolRegistry,
        toolset: &str,
        handler: Arc<dyn ToolHandler>,
        emoji: &str,
        env_deps: Vec<String>,
    ) {
        let schema = handler.schema();
        let name = schema.name.clone();
        let desc = schema.description.clone();
        registry.register_vetted(
            name,
            toolset,
            schema,
            handler,
            Arc::new(|| true),
            env_deps,
            true,
            desc,
            emoji,
            None,
            VettedToolEffect::LocalOneShot,
        );
    }
    fn reg_with_check(
        registry: &ToolRegistry,
        toolset: &str,
        handler: Arc<dyn ToolHandler>,
        emoji: &str,
        env_deps: Vec<String>,
        check_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    ) {
        let schema = handler.schema();
        let name = schema.name.clone();
        let desc = schema.description.clone();
        registry.register_vetted(
            name,
            toolset,
            schema,
            handler,
            check_fn,
            env_deps,
            true,
            desc,
            emoji,
            None,
            VettedToolEffect::LocalOneShot,
        );
    }

    let terminal_requirements_check: Arc<dyn Fn() -> bool + Send + Sync> =
        Arc::new(crate::terminal_requirements::check_terminal_requirements);

    // -- Web tools -----------------------------------------------------------
    reg(
        registry,
        "web",
        Arc::new(crate::tools::web::WebSearchHandler::new(
            crate::backends::web::search_backend_from_env_or_fallback(),
        )),
        "🔍",
        vec![],
    );
    reg(
        registry,
        "web",
        Arc::new(crate::tools::web::WebExtractHandler::new(
            crate::backends::web::extract_backend_from_env_or_fallback(),
        )),
        "📄",
        vec![],
    );
    reg(
        registry,
        "web",
        Arc::new(crate::tools::web::WebCrawlHandler::new(
            crate::backends::web::crawl_backend_from_env_or_fallback(),
        )),
        "🕸️",
        vec![],
    );

    // -- Terminal ------------------------------------------------------------
    let terminal_handler = Arc::new(crate::tools::terminal::TerminalHandler::with_execution_context(
        terminal_backend.clone(),
        registry.execution_containment_context(),
    ));
    let terminal_schema = terminal_handler.schema();
    let terminal_name = terminal_schema.name.clone();
    let terminal_description = terminal_schema.description.clone();
    registry.register_vetted_containment(
        terminal_name,
        "terminal",
        terminal_schema,
        terminal_handler.clone(),
        terminal_handler,
        terminal_requirements_check.clone(),
        vec![],
        true,
        terminal_description,
        "💻",
        None,
        VettedToolEffect::LocalOneShot,
    );
    reg_with_check(
        registry,
        "terminal",
        Arc::new(crate::tools::terminal::ProcessHandler::new(Arc::new(
            crate::tools::terminal::TerminalProcessBackendAdapter::new(terminal_backend.clone()),
        ))),
        "🧵",
        vec![],
        terminal_requirements_check.clone(),
    );

    // -- File tools ----------------------------------------------------------
    reg_with_check(
        registry,
        "file",
        Arc::new(crate::tools::file::ReadFileHandler::new(
            terminal_backend.clone(),
        )),
        "📖",
        vec![],
        terminal_requirements_check.clone(),
    );
    reg_with_check(
        registry,
        "file",
        Arc::new(crate::tools::file::WriteFileHandler::new(
            terminal_backend.clone(),
        )),
        "✏️",
        vec![],
        terminal_requirements_check.clone(),
    );
    reg(
        registry,
        "file",
        Arc::new(crate::tools::file::PatchHandler::new(Arc::new(
            crate::backends::file::LocalPatchBackend::new(),
        ))),
        "🩹",
        vec![],
    );
    reg(
        registry,
        "file",
        Arc::new(crate::tools::file::SearchFilesHandler::new(Arc::new(
            crate::backends::file::LocalSearchBackend::new(),
        ))),
        "🔎",
        vec![],
    );

    // -- Project/workspace tools ----------------------------------------------
    reg(
        registry,
        "project",
        Arc::new(crate::tools::project_workspace::ProjectFactsHandler),
        "🗂️",
        vec![],
    );
    reg(
        registry,
        "project",
        Arc::new(crate::tools::project_workspace::ProjectTreeHandler),
        "🌳",
        vec![],
    );
    reg(
        registry,
        "project",
        Arc::new(crate::tools::harness_cockpit::HarnessCockpitHandler::new()),
        "🧭",
        vec![],
    );
    reg(
        registry,
        "ultra_autonomy",
        Arc::new(crate::tools::ultra_autonomy::UltraAutonomyHandler::new(
            data_dir.clone(),
        )),
        "🧠",
        vec![],
    );

    // -- Ultra feature coding-agent surfaces -----------------------------------------
    for (handler, toolset, emoji) in
        crate::tools::ultra_features::builtin_ultra_feature_handlers(data_dir.clone())
    {
        reg(registry, toolset, handler, emoji, vec![]);
    }

    // -- Vision --------------------------------------------------------------
    {
        let backend =
            crate::backends::vision::OpenAiVisionBackend::from_env().unwrap_or_else(|_| {
                crate::backends::vision::OpenAiVisionBackend::new(
                    String::new(),
                    "https://api.openai.com/v1".into(),
                    "gpt-4o".into(),
                )
            });
        let vision_backend: Arc<dyn crate::tools::vision::VisionBackend> = Arc::new(backend);
        reg(
            registry,
            "vision",
            Arc::new(crate::tools::vision::VisionAnalyzeHandler::new(
                vision_backend.clone(),
            )),
            "👁️",
            vec![
                "AUXILIARY_VISION_API_KEY".into(),
                "AUXILIARY_VISION_PROVIDER".into(),
                "AUXILIARY_VISION_MODEL".into(),
                "AUXILIARY_VISION_BASE_URL".into(),
                "HERMES_OPENAI_API_KEY".into(),
                "OPENAI_API_KEY".into(),
                "OPENROUTER_API_KEY".into(),
                "NOUS_API_KEY".into(),
                "GEMINI_API_KEY".into(),
                "XAI_API_KEY".into(),
            ],
        );
        reg(
            registry,
            "vision",
            Arc::new(crate::tools::video::VideoAnalyzeHandler::new(Arc::new(
                crate::backends::video::VisionFrameSamplingVideoBackend::new(vision_backend),
            ))),
            "🎬",
            vec!["HERMES_OPENAI_API_KEY".into(), "OPENAI_API_KEY".into()],
        );
    }

    // -- Image generation ----------------------------------------------------
    {
        let backend = crate::backends::image_gen::ImageGenRuntimeBackend::from_env_or_managed();
        let env_deps = backend.required_env_vars();
        reg(
            registry,
            "image_gen",
            Arc::new(crate::tools::image_gen::ImageGenerateHandler::new(
                Arc::new(backend),
            )),
            "🎨",
            env_deps,
        );
    }

    // -- Video generation ----------------------------------------------------
    {
        let backend = crate::backends::video_gen::VideoGenBackend::from_env_or_managed();
        let env_deps = backend.required_env_vars();
        reg(
            registry,
            "video_gen",
            Arc::new(crate::tools::video::VideoGenerateHandler::new(Arc::new(
                backend,
            ))),
            "🎞️",
            env_deps,
        );
    }

    // -- Spotify -------------------------------------------------------------
    {
        let backend: Arc<dyn crate::tools::spotify::SpotifyBackend> =
            match crate::backends::spotify::SpotifyWebApiBackend::from_env_or_auth_store() {
                Ok(backend) => Arc::new(backend),
                Err(_) => Arc::new(crate::backends::spotify::SpotifyWebApiBackend::unconfigured()),
            };
        let deps = vec![
            "HERMES_SPOTIFY_ACCESS_TOKEN".into(),
            "SPOTIFY_ACCESS_TOKEN".into(),
            "HERMES_AUTH_FILE".into(),
        ];
        for (tool, emoji) in [
            (crate::tools::spotify::SpotifyTool::Playback, "🎵"),
            (crate::tools::spotify::SpotifyTool::Devices, "🔈"),
            (crate::tools::spotify::SpotifyTool::Queue, "📻"),
            (crate::tools::spotify::SpotifyTool::Search, "🔎"),
            (crate::tools::spotify::SpotifyTool::Playlists, "📚"),
            (crate::tools::spotify::SpotifyTool::Albums, "💿"),
            (crate::tools::spotify::SpotifyTool::Library, "❤️"),
        ] {
            reg(
                registry,
                "spotify",
                Arc::new(crate::tools::spotify::SpotifyHandler::new(
                    tool,
                    backend.clone(),
                )),
                emoji,
                deps.clone(),
            );
        }
    }

    // -- Skills (3 tools) ----------------------------------------------------
    reg(
        registry,
        "skills",
        Arc::new(crate::tools::skills::SkillsListHandler::new(
            skill_provider.clone(),
        )),
        "📚",
        vec![],
    );
    reg(
        registry,
        "skills",
        Arc::new(crate::tools::skills::SkillViewHandler::new(
            skill_provider.clone(),
        )),
        "📖",
        vec![],
    );
    reg(
        registry,
        "skills",
        Arc::new(crate::tools::skills::SkillManageHandler::new(
            skill_provider,
        )),
        "⚙️",
        vec![],
    );

    // -- Memory --------------------------------------------------------------
    let memory_backend = match memory_home {
        Some(home) => crate::backends::memory::FileMemoryBackend::with_home(home),
        None => crate::backends::memory::FileMemoryBackend::new(),
    };
    registry.register_vetted_memory(Arc::new(memory_backend));

    // -- Session search ------------------------------------------------------
    {
        let db_path = data_dir.join("sessions.db");
        if let Ok(backend) = crate::backends::session_search::SqliteSessionSearchBackend::new(
            &db_path.to_string_lossy(),
        ) {
            reg(
                registry,
                "session_search",
                Arc::new(crate::tools::session_search::SessionSearchHandler::new(
                    Arc::new(backend),
                )),
                "🔍",
                vec![],
            );
        } else {
            tracing::warn!("Failed to initialise session search DB; skipping session_search tool");
        }
    }

    // -- Todo ----------------------------------------------------------------
    {
        let todo_path = data_dir.join("todos.json");
        reg(
            registry,
            "todo",
            Arc::new(crate::tools::todo::TodoHandler::new(Arc::new(
                crate::backends::todo::FileTodoBackend::new(todo_path),
            ))),
            "📋",
            vec![],
        );
    }

    // -- Clarify -------------------------------------------------------------
    reg(
        registry,
        "clarify",
        Arc::new(crate::tools::clarify::ClarifyHandler::new(Arc::new(
            crate::backends::clarify::SignalClarifyBackend::new(),
        ))),
        "❓",
        vec![],
    );

    // -- Code execution ------------------------------------------------------
    // Host code execution uses the same availability gate and policy dispatch
    // boundary as terminal commands; it is not registered as a sandboxed
    // bypass.
    reg_with_check(
        registry,
        "code_execution",
        Arc::new(crate::tools::code_execution::ExecuteCodeHandler::new(
            Arc::new(crate::backends::code_execution::LocalCodeExecutionBackend::default()),
        )),
        "🖥️",
        vec![],
        terminal_requirements_check.clone(),
    );

    // -- Delegation ----------------------------------------------------------
    reg(
        registry,
        "delegation",
        Arc::new(crate::tools::delegation::DelegateTaskHandler::new(
            Arc::new(crate::backends::delegation::SignalDelegationBackend::new()),
        )),
        "🤝",
        vec![],
    );

    // -- Cronjob -------------------------------------------------------------
    reg(
        registry,
        "cronjob",
        Arc::new(crate::tools::cronjob::CronjobHandler::new(Arc::new(
            crate::backends::cronjob::SignalCronjobBackend::new(),
        ))),
        "⏰",
        vec![],
    );

    // -- Messaging -----------------------------------------------------------
    reg(
        registry,
        "messaging",
        Arc::new(crate::tools::messaging::SendMessageHandler::new(Arc::new(
            crate::backends::messaging::SignalMessagingBackend::new(),
        ))),
        "💬",
        vec![],
    );

    // -- Dashboard control ---------------------------------------------------
    reg(
        registry,
        "dashboard",
        Arc::new(crate::tools::dashboard_control::DashboardControlHandler),
        "🖥️",
        vec![],
    );

    // -- Home Assistant (4 tools) --------------------------------------------
    {
        let ha_backend: Arc<dyn crate::tools::homeassistant::HomeAssistantBackend> =
            match crate::backends::homeassistant::HaRestBackend::from_env() {
                Ok(b) => Arc::new(b),
                Err(_) => Arc::new(crate::backends::homeassistant::HaRestBackend::new(
                    String::new(),
                    String::new(),
                )),
            };
        let deps = vec!["HASS_URL".into(), "HASS_TOKEN".into()];
        reg(
            registry,
            "homeassistant",
            Arc::new(crate::tools::homeassistant::HaListEntitiesHandler::new(
                ha_backend.clone(),
            )),
            "🏠",
            deps.clone(),
        );
        reg(
            registry,
            "homeassistant",
            Arc::new(crate::tools::homeassistant::HaGetStateHandler::new(
                ha_backend.clone(),
            )),
            "🏠",
            deps.clone(),
        );
        reg(
            registry,
            "homeassistant",
            Arc::new(crate::tools::homeassistant::HaListServicesHandler::new(
                ha_backend.clone(),
            )),
            "🏠",
            deps.clone(),
        );
        reg(
            registry,
            "homeassistant",
            Arc::new(crate::tools::homeassistant::HaCallServiceHandler::new(
                ha_backend,
            )),
            "🏠",
            deps,
        );
    }

    // -- TTS -----------------------------------------------------------------
    reg(
        registry,
        "tts",
        Arc::new(crate::tools::tts::TextToSpeechHandler::new(Arc::new(
            crate::backends::tts::MultiTtsBackend::new(),
        ))),
        "🔊",
        vec![],
    );

    // -- Browser (10 tools) --------------------------------------------------
    {
        let browser_backend: Arc<dyn crate::tools::browser::BrowserBackend> =
            crate::backends::browser::browser_backend_from_env();
        reg(
            registry,
            "browser",
            Arc::new(crate::tools::browser::BrowserNavigateHandler::new(
                browser_backend.clone(),
            )),
            "🌐",
            vec![],
        );
        reg(
            registry,
            "browser",
            Arc::new(crate::tools::browser::BrowserSnapshotHandler::new(
                browser_backend.clone(),
            )),
            "📸",
            vec![],
        );
        reg(
            registry,
            "browser",
            Arc::new(crate::tools::browser::BrowserClickHandler::new(
                browser_backend.clone(),
            )),
            "🖱️",
            vec![],
        );
        reg(
            registry,
            "browser",
            Arc::new(crate::tools::browser::BrowserTypeHandler::new(
                browser_backend.clone(),
            )),
            "⌨️",
            vec![],
        );
        reg(
            registry,
            "browser",
            Arc::new(crate::tools::browser::BrowserScrollHandler::new(
                browser_backend.clone(),
            )),
            "📜",
            vec![],
        );
        reg(
            registry,
            "browser",
            Arc::new(crate::tools::browser::BrowserBackHandler::new(
                browser_backend.clone(),
            )),
            "⬅️",
            vec![],
        );
        reg(
            registry,
            "browser",
            Arc::new(crate::tools::browser::BrowserPressHandler::new(
                browser_backend.clone(),
            )),
            "🔘",
            vec![],
        );
        reg(
            registry,
            "browser",
            Arc::new(crate::tools::browser::BrowserGetImagesHandler::new(
                browser_backend.clone(),
            )),
            "🖼️",
            vec![],
        );
        reg(
            registry,
            "browser",
            Arc::new(crate::tools::browser::BrowserVisionHandler::new(
                browser_backend.clone(),
            )),
            "👁️",
            vec![],
        );
        reg(
            registry,
            "browser",
            Arc::new(crate::tools::browser::BrowserConsoleHandler::new(
                browser_backend,
            )),
            "🔧",
            vec![],
        );
    }

    // -- Mixture of Agents ---------------------------------------------------
    reg(
        registry,
        "mixture_of_agents",
        Arc::new(crate::tools::mixture_of_agents::MixtureOfAgentsHandler),
        "🤖",
        vec![],
    );

    // -- Process registry ----------------------------------------------------
    reg_with_check(
        registry,
        "terminal",
        Arc::new(crate::tools::process_registry::ProcessRegistryHandler::default()),
        "📊",
        vec![],
        terminal_requirements_check,
    );

    // -- Transcription -------------------------------------------------------
    reg(
        registry,
        "voice",
        Arc::new(crate::tools::transcription::TranscriptionHandler),
        "🎙️",
        vec![
            "VOICE_TOOLS_OPENAI_KEY".into(),
            "HERMES_OPENAI_API_KEY".into(),
            "OPENAI_API_KEY".into(),
            "GROQ_API_KEY".into(),
            "MISTRAL_API_KEY".into(),
            "TOOL_GATEWAY_USER_TOKEN".into(),
        ],
    );

    // -- TTS Premium ---------------------------------------------------------
    reg(
        registry,
        "tts",
        Arc::new(crate::tools::tts_premium::TtsPremiumHandler::default()),
        "🎵",
        vec!["ELEVENLABS_API_KEY".into()],
    );

    // -- OSV Check -----------------------------------------------------------
    reg(
        registry,
        "security",
        Arc::new(crate::tools::osv_check::OsvCheckHandler),
        "🛡️",
        vec![],
    );

    // -- URL Safety ----------------------------------------------------------
    reg(
        registry,
        "security",
        Arc::new(crate::tools::url_safety::UrlSafetyHandler),
        "🔒",
        vec![],
    );

    // -- RL training ---------------------------------------------------------
    {
        let state = crate::tools::rl_training::RlState::new(data_dir.join("rl_training"));
        reg(
            registry,
            "rl_training",
            Arc::new(crate::tools::rl_training::RlListEnvironmentsHandler),
            "🏋️",
            vec![],
        );
        reg(
            registry,
            "rl_training",
            Arc::new(crate::tools::rl_training::RlSelectEnvironmentHandler {
                state: state.clone(),
            }),
            "🏋️",
            vec![],
        );
        reg(
            registry,
            "rl_training",
            Arc::new(crate::tools::rl_training::RlGetCurrentConfigHandler {
                state: state.clone(),
            }),
            "🏋️",
            vec![],
        );
        reg(
            registry,
            "rl_training",
            Arc::new(crate::tools::rl_training::RlEditConfigHandler {
                state: state.clone(),
            }),
            "🏋️",
            vec![],
        );
        reg(
            registry,
            "rl_training",
            Arc::new(crate::tools::rl_training::RlStartTrainingHandler {
                state: state.clone(),
            }),
            "🏋️",
            vec![],
        );
        reg(
            registry,
            "rl_training",
            Arc::new(crate::tools::rl_training::RlCheckStatusHandler {
                state: state.clone(),
            }),
            "🏋️",
            vec![],
        );
        reg(
            registry,
            "rl_training",
            Arc::new(crate::tools::rl_training::RlStopTrainingHandler {
                state: state.clone(),
            }),
            "🏋️",
            vec![],
        );
        reg(
            registry,
            "rl_training",
            Arc::new(crate::tools::rl_training::RlGetResultsHandler {
                state: state.clone(),
            }),
            "🏋️",
            vec![],
        );
        reg(
            registry,
            "rl_training",
            Arc::new(crate::tools::rl_training::RlListRunsHandler {
                state: state.clone(),
            }),
            "🏋️",
            vec![],
        );
        reg(
            registry,
            "rl_training",
            Arc::new(crate::tools::rl_training::RlTestInferenceHandler { state }),
            "🏋️",
            vec![],
        );
    }

    // -- Env passthrough -----------------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::env_passthrough::EnvPassthroughHandler),
        "🔧",
        vec![],
    );

    // -- Computer Use --------------------------------------------------------
    reg_with_check(
        registry,
        "computer_use",
        Arc::new(crate::tools::computer_use::ComputerUseHandler::new(
            Arc::new(crate::backends::computer_use::CuaDriverBackend::from_env()),
        )),
        "🖱️",
        vec!["HERMES_CUA_DRIVER_CMD".into()],
        Arc::new(crate::backends::computer_use::CuaDriverBackend::command_available_from_env),
    );

    // -- Credential files ----------------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::credential_files::CredentialFilesHandler),
        "🔑",
        vec![],
    );

    // -- Auth/provider snapshot ---------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::auth_snapshot::AuthSnapshotHandler),
        "🔐",
        vec![],
    );

    // -- Integration control-plane snapshot ---------------------------------
    reg(
        registry,
        "system",
        Arc::new(
            crate::tools::integrations_snapshot::IntegrationsSnapshotHandler::new(Arc::new(
                registry.clone(),
            )),
        ),
        "🧩",
        vec![],
    );

    // -- Alpha objective/mission snapshots ----------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::alpha_snapshot::ObjectiveSnapshotHandler::new()),
        "🎯",
        vec![],
    );
    reg(
        registry,
        "system",
        Arc::new(crate::tools::alpha_snapshot::MissionSnapshotHandler::new()),
        "🛰️",
        vec![],
    );

    // -- Tool policy simulation ---------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(
            crate::tools::tool_policy_simulate::ToolPolicySimulateHandler::new(Arc::new(
                registry.clone(),
            )),
        ),
        "🧭",
        vec![],
    );

    // -- RTK raw trace control ----------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(
            crate::tools::raw_trace_control::RawTraceControlHandler::new(Arc::new(
                registry.clone(),
            )),
        ),
        "🧾",
        vec![],
    );

    // -- Runtime recovery runbooks ------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::runbook::RunbookControlHandler::new()),
        "📘",
        vec![],
    );

    // -- Runtime telemetry snapshot -----------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::telemetry_snapshot::TelemetrySnapshotHandler::new()),
        "📡",
        vec![],
    );

    // -- Read-only ops snapshot ---------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::ops_snapshot::OpsSnapshotHandler::new(
            Arc::new(registry.clone()),
        )),
        "🛰️",
        vec![],
    );

    // -- Tool result storage -------------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::tool_result_storage::ToolResultStorageHandler::default()),
        "💾",
        vec![],
    );

    // -- Disk cleanup --------------------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::disk_cleanup::DiskCleanupHandler::default()),
        "🧹",
        vec![],
    );

    tracing::info!("Registered {} built-in tools", registry.list_tools().len());
}

/// Return the Hermes data directory, creating it if needed.
fn hermes_data_dir() -> PathBuf {
    if let Some(dir) = hermes_home_dir() {
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    let home = dirs_home().unwrap_or_else(|| PathBuf::from("."));
    let dir = home.join(".hermes");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn hermes_home_dir() -> Option<PathBuf> {
    std::env::var_os("HERMES_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::approval::TEST_ENV_LOCK;
    use async_trait::async_trait;
    use hermes_core::{AgentError, CommandOutput, Skill, SkillMeta};
    use serde_json::Value;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn lock_env() -> parking_lot::MutexGuard<'static, ()> {
        TEST_ENV_LOCK.lock()
    }

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }

        fn remove(key: &'static str) -> Self {
            let original = std::env::var(key).ok();
            unsafe { std::env::remove_var(key) };
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(value) = &self.original {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    struct MockTerminalBackend;

    #[async_trait]
    impl TerminalBackend for MockTerminalBackend {
        async fn execute_command(
            &self,
            _command: &str,
            _timeout: Option<u64>,
            _workdir: Option<&str>,
            _background: bool,
            _pty: bool,
        ) -> Result<CommandOutput, AgentError> {
            Ok(CommandOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        async fn read_file(
            &self,
            _path: &str,
            _offset: Option<u64>,
            _limit: Option<u64>,
        ) -> Result<String, AgentError> {
            Ok(String::new())
        }

        async fn write_file(&self, _path: &str, _content: &str) -> Result<(), AgentError> {
            Ok(())
        }

        async fn file_exists(&self, _path: &str) -> Result<bool, AgentError> {
            Ok(false)
        }

        async fn list_processes(&self) -> Result<Value, AgentError> {
            Ok(serde_json::json!([]))
        }
    }

    struct MockSkillProvider;

    #[async_trait]
    impl SkillProvider for MockSkillProvider {
        async fn create_skill(
            &self,
            name: &str,
            content: &str,
            category: Option<&str>,
        ) -> Result<Skill, AgentError> {
            Ok(Skill {
                name: name.into(),
                content: content.into(),
                category: category.map(String::from),
                description: None,
            })
        }

        async fn get_skill(&self, _name: &str) -> Result<Option<Skill>, AgentError> {
            Ok(None)
        }

        async fn list_skills(&self) -> Result<Vec<SkillMeta>, AgentError> {
            Ok(Vec::new())
        }

        async fn update_skill(&self, name: &str, content: &str) -> Result<Skill, AgentError> {
            Ok(Skill {
                name: name.into(),
                content: content.into(),
                category: None,
                description: None,
            })
        }

        async fn delete_skill(&self, _name: &str) -> Result<(), AgentError> {
            Ok(())
        }
    }

    fn registered_names() -> Vec<String> {
        let registry = ToolRegistry::new();
        register_builtin_tools(
            &registry,
            Arc::new(MockTerminalBackend),
            Arc::new(MockSkillProvider),
        );
        let mut names: Vec<String> = registry
            .get_definitions()
            .into_iter()
            .map(|s| s.name)
            .collect();
        names.sort();
        names
    }

    fn registered_all_names_with_data_dir(data_dir: PathBuf) -> Vec<String> {
        let registry = ToolRegistry::new();
        register_builtin_tools_with_home(
            &registry,
            Arc::new(MockTerminalBackend),
            Arc::new(MockSkillProvider),
            data_dir,
        );
        let mut names: Vec<String> = registry
            .list_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        names.sort();
        names
    }

    #[test]
    fn local_backend_exposes_terminal_and_terminal_backed_file_tools() {
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let _terminal_env = EnvGuard::set("TERMINAL_ENV", "local");
        let names = registered_names();

        for expected in [
            "terminal",
            "process",
            "process_registry",
            "read_file",
            "write_file",
            "patch",
            "search_files",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "local backend should expose {expected}"
            );
        }
    }

    #[test]
    fn invalid_backend_hides_terminal_backed_tools_but_keeps_local_file_tools() {
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let _terminal_env = EnvGuard::set("TERMINAL_ENV", "unknown-backend");
        let _ssh_host = EnvGuard::remove("TERMINAL_SSH_HOST");
        let _ssh_user = EnvGuard::remove("TERMINAL_SSH_USER");
        let names = registered_names();

        for hidden in [
            "terminal",
            "process",
            "process_registry",
            "read_file",
            "write_file",
        ] {
            assert!(
                !names.contains(&hidden.to_string()),
                "invalid backend should hide {hidden}"
            );
        }
        for independent in ["patch", "search_files", "execute_code"] {
            assert!(
                names.contains(&independent.to_string()),
                "invalid backend should keep independent tool {independent}"
            );
        }
        assert!(
            names.contains(&"ultra-feature-1".to_string()),
            "invalid backend should keep Rust-native ultra-feature tools"
        );
        assert!(
            names.contains(&"tool_policy_simulate".to_string()),
            "invalid backend should keep policy simulator"
        );
        assert!(
            names.contains(&"auth_snapshot".to_string()),
            "invalid backend should keep read-only auth snapshots"
        );
        assert!(
            names.contains(&"integrations_snapshot".to_string()),
            "invalid backend should keep read-only integration snapshots"
        );
        assert!(
            names.contains(&"objective_snapshot".to_string()),
            "invalid backend should keep read-only objective snapshots"
        );
        assert!(
            names.contains(&"mission_snapshot".to_string()),
            "invalid backend should keep read-only mission snapshots"
        );
        assert!(
            names.contains(&"raw_trace_control".to_string()),
            "invalid backend should keep raw trace control"
        );
        assert!(
            !names.contains(&"replay_trace_control".to_string()),
            "legacy replay_trace_control must not be registered"
        );
        assert!(
            names.contains(&"runbook_control".to_string()),
            "invalid backend should keep runtime runbooks"
        );
        assert!(
            names.contains(&"telemetry_snapshot".to_string()),
            "invalid backend should keep telemetry snapshots"
        );
        assert!(
            names.contains(&"harness_cockpit".to_string()),
            "invalid backend should keep harness cockpit"
        );
        assert!(
            names.contains(&"ultra_autonomy".to_string()),
            "invalid backend should keep autonomy cockpit"
        );
        assert!(
            names.contains(&"ops_snapshot".to_string()),
            "invalid backend should keep read-only ops snapshots"
        );
    }

    #[test]
    fn builtin_registry_exposes_snapshot_surfaces_to_cli_list() {
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let _terminal_env = EnvGuard::set("TERMINAL_ENV", "local");
        let registry = ToolRegistry::new();
        register_builtin_tools(
            &registry,
            Arc::new(MockTerminalBackend),
            Arc::new(MockSkillProvider),
        );

        let names: Vec<String> = registry
            .list_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert!(
            names.contains(&"integrations_snapshot".to_string()),
            "`hermes tools list` should expose integrations_snapshot"
        );
        assert!(
            names.contains(&"objective_snapshot".to_string()),
            "`hermes tools list` should expose objective_snapshot"
        );
        assert!(
            names.contains(&"mission_snapshot".to_string()),
            "`hermes tools list` should expose mission_snapshot"
        );
    }

    #[test]
    fn builtin_registry_registers_terminal_tools_for_managed_modal_surface() {
        let _global_env = hermes_config::managed_gateway::test_lock::lock();
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let hermes_home = tempfile::tempdir().expect("temp hermes home");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let _hermes_home =
            EnvGuard::set("HERMES_HOME", hermes_home.path().to_string_lossy().as_ref());
        let names = registered_all_names_with_data_dir(hermes_home.path().to_path_buf());

        assert!(names.contains(&"terminal".to_string()));
        assert!(names.contains(&"process".to_string()));
        assert!(names.contains(&"execute_code".to_string()));
        assert!(
            hermes_home.path().join("sessions.db").is_file(),
            "session search should initialize under HERMES_HOME"
        );
        assert!(
            !home.path().join(".hermes").join("sessions.db").exists(),
            "HERMES_HOME must not be nested below HOME/.hermes"
        );
    }

    #[test]
    fn builtin_registry_registers_core_tool_surfaces_for_parity() {
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let _terminal_env = EnvGuard::set("TERMINAL_ENV", "local");
        let names = registered_names();

        for expected in [
            "terminal",
            "process",
            "process_registry",
            "project_facts",
            "project_tree",
            "ultra_autonomy",
            "todo",
            "text_to_speech",
            "tts_premium",
            "browser_navigate",
            "browser_snapshot",
            "browser_click",
            "browser_type",
            "browser_scroll",
            "browser_back",
            "browser_press",
            "browser_get_images",
            "browser_vision",
            "browser_console",
            "rl_list_environments",
            "rl_select_environment",
            "rl_get_current_config",
            "rl_edit_config",
            "rl_start_training",
            "rl_check_status",
            "rl_stop_training",
            "rl_get_results",
            "rl_list_runs",
            "rl_test_inference",
            "ultra-feature-1",
            "ultra-feature-2",
            "ultra-feature-3",
            "ultra-feature-4",
            "ultra-feature-5",
            "ultra-feature-6",
            "ultra-feature-7",
            "ultra-feature-8",
            "ultra-feature-9",
            "ultra-feature-10",
            "ultra-feature-11",
            "ultra-feature-12",
            "ultra-feature-13",
            "ultra-feature-14",
            "ultra-feature-15",
            "ultra-feature-16",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "builtin registry should expose {expected}"
            );
        }
        assert!(
            !names.contains(&"replay_trace_control".to_string()),
            "legacy replay_trace_control must not be registered"
        );
    }

    #[test]
    fn browser_tool_surface_exposes_current_commands_without_legacy_close() {
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let names = registered_names();

        for expected in [
            "browser_navigate",
            "browser_snapshot",
            "browser_click",
            "browser_type",
            "browser_scroll",
            "browser_back",
            "browser_press",
            "browser_get_images",
            "browser_vision",
            "browser_console",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "browser surface should expose {expected}"
            );
        }
        assert!(
            !names.contains(&"browser_close".to_string()),
            "Rust browser backend intentionally has no Python supervisor close tool"
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn builtin_registry_dispatches_configured_tts_through_shipped_path() {
        let _global_env = hermes_config::managed_gateway::test_lock::lock();
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let data_dir = tempfile::tempdir().expect("temp data dir");
        let output = data_dir.path().join("registry-smoke.wav");
        let fixture = data_dir.path().join("fixture.wav");
        let consumed_input = data_dir.path().join("consumed-input.txt");
        use std::os::unix::fs::PermissionsExt;

        let wrapper = data_dir.path().join("registry-tts-wrapper");
        std::fs::write(&wrapper, "#!/bin/sh\ncp \"$1\" \"$2\"\ncp \"$3\" \"$4\"\n").unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&38_u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&8_000_u32.to_le_bytes());
        wav.extend_from_slice(&16_000_u32.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&2_u32.to_le_bytes());
        wav.extend_from_slice(&0_i16.to_le_bytes());
        std::fs::write(&fixture, &wav).unwrap();
        let config = serde_json::json!({
            "provider": "registry-smoke",
            "providers": {
                "registry-smoke": {
                    "type": "command",
                    "command": format!(
                        "{} {{input_path}} {{voice}} {{model}} {{output_path}}",
                        wrapper.display()
                    ),
                    "model": fixture,
                    "output_format": "wav"
                }
            }
        })
        .to_string();
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let _tts_config = EnvGuard::set("HERMES_TTS_CONFIG_JSON", &config);
        let registry = ToolRegistry::new();
        register_builtin_tools_with_home(
            &registry,
            Arc::new(MockTerminalBackend),
            Arc::new(MockSkillProvider),
            home.path().to_path_buf(),
        );

        let raw = registry
            .dispatch_async(
                "text_to_speech",
                serde_json::json!({
                    "text": "registry shipped-path smoke",
                    "voice": consumed_input,
                    "output_path": output
                }),
            )
            .await;
        let result: Value = serde_json::from_str(&raw).expect("structured tool result");
        assert!(result.get("error").is_none(), "{result}");
        assert_eq!(result["provider"], "registry-smoke");
        assert_eq!(result["file_path"], output.to_string_lossy().to_string());
        assert_eq!(result["media_tag"], format!("MEDIA:{}", output.display()));
        let delivered = std::fs::read(&output).unwrap();
        assert_eq!(&delivered[..4], b"RIFF");
        assert_eq!(&delivered[8..12], b"WAVE");
        assert_eq!(delivered, wav);
        assert_eq!(
            std::fs::read_to_string(consumed_input).unwrap(),
            "registry shipped-path smoke"
        );
    }

    #[tokio::test]
    async fn builtin_registry_dispatches_transcription_through_shipped_path() {
        let _global_env = hermes_config::managed_gateway::test_lock::lock();
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let data_dir = tempfile::tempdir().expect("temp data dir");
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/audio/transcriptions"))
            .and(header("authorization", "Bearer registry-key"))
            .respond_with(ResponseTemplate::new(200).set_body_string("registry transcript"))
            .expect(1)
            .mount(&server)
            .await;
        let audio = data_dir.path().join("inbound.wav");
        std::fs::write(&audio, b"registry audio fixture").unwrap();
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let _key = EnvGuard::set("OPENAI_API_KEY", "registry-key");
        let _base = EnvGuard::set("STT_OPENAI_BASE_URL", &server.uri());
        let _allowed = EnvGuard::set(
            "HERMES_MEDIA_ALLOW_DIRS",
            data_dir.path().to_string_lossy().as_ref(),
        );
        let registry = ToolRegistry::new();
        register_builtin_tools_with_home(
            &registry,
            Arc::new(MockTerminalBackend),
            Arc::new(MockSkillProvider),
            home.path().to_path_buf(),
        );

        let raw = registry
            .dispatch_async(
                "transcription",
                serde_json::json!({
                    "audio_path": audio,
                    "provider": "openai"
                }),
            )
            .await;
        let result: Value = serde_json::from_str(&raw).expect("structured tool result");
        assert!(result.get("error").is_none(), "{result}");
        assert_eq!(result["status"], "transcribed");
        assert_eq!(result["provider"], "openai");
        assert_eq!(result["text"], "registry transcript");
    }
}
