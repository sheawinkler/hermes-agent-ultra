//! Registers all built-in tool handlers into a ToolRegistry.
//!
//! This is the Rust equivalent of Python's `_discover_tools()` in `model_tools.py`.
//! Each tool handler is instantiated with its default backend and registered
//! in the appropriate toolset.

use std::path::PathBuf;
use std::sync::Arc;

use hermes_core::{SkillProvider, TerminalBackend, ToolHandler};

use crate::ToolRegistry;

/// Register all built-in tool handlers into the given registry.
///
/// `terminal_backend` is shared by terminal, file read/write handlers.
/// `skill_provider` is shared by the three skills handlers.
///
/// Backends that depend on environment variables (e.g. `OPENAI_API_KEY`,
/// `FAL_KEY`, `HASS_URL`) are constructed eagerly; if the vars are absent
/// the tools will return a clear error at *runtime*, not at registration.
pub fn register_builtin_tools(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
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
        registry.register(
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
        );
    }

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
        Arc::new(crate::tools::web::WebExtractHandler::new(Box::new(
            crate::backends::web::SimpleExtractBackend::new(),
        ))),
        "📄",
        vec![],
    );

    // -- Terminal ------------------------------------------------------------
    reg(
        registry,
        "terminal",
        Arc::new(crate::tools::terminal::TerminalHandler::new(
            terminal_backend.clone(),
        )),
        "💻",
        vec![],
    );
    reg(
        registry,
        "terminal",
        Arc::new(crate::tools::terminal::ProcessHandler::new(Arc::new(
            crate::tools::terminal::TerminalProcessBackendAdapter::new(terminal_backend.clone()),
        ))),
        "🧵",
        vec![],
    );

    // -- File tools ----------------------------------------------------------
    reg(
        registry,
        "file",
        Arc::new(crate::tools::file::ReadFileHandler::new(
            terminal_backend.clone(),
        )),
        "📖",
        vec![],
    );
    reg(
        registry,
        "file",
        Arc::new(crate::tools::file::WriteFileHandler::new(
            terminal_backend.clone(),
        )),
        "✏️",
        vec![],
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
        reg(
            registry,
            "vision",
            Arc::new(crate::tools::vision::VisionAnalyzeHandler::new(Arc::new(
                backend,
            ))),
            "👁️",
            vec!["HERMES_OPENAI_API_KEY".into(), "OPENAI_API_KEY".into()],
        );
    }

    // -- Image generation ----------------------------------------------------
    {
        let backend = crate::backends::image_gen::FalImageGenBackend::from_env()
            .unwrap_or_else(|_| crate::backends::image_gen::FalImageGenBackend::new(String::new()));
        reg(
            registry,
            "image_gen",
            Arc::new(crate::tools::image_gen::ImageGenerateHandler::new(
                Arc::new(backend),
            )),
            "🎨",
            vec!["FAL_KEY".into()],
        );
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
    reg(
        registry,
        "memory",
        Arc::new(crate::tools::memory::MemoryHandler::new(Arc::new(
            crate::backends::memory::FileMemoryBackend::new(),
        ))),
        "🧠",
        vec![],
    );

    // -- Session search ------------------------------------------------------
    {
        let db_path = hermes_data_dir().join("sessions.db");
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
        let todo_path = hermes_data_dir().join("todos.json");
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
    reg(
        registry,
        "code_execution",
        Arc::new(crate::tools::code_execution::ExecuteCodeHandler::new(
            Arc::new(crate::backends::code_execution::LocalCodeExecutionBackend::default()),
        )),
        "🖥️",
        vec![],
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
            Arc::new(crate::backends::browser::CdpBrowserBackend::from_env());
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
    reg(
        registry,
        "terminal",
        Arc::new(crate::tools::process_registry::ProcessRegistryHandler::default()),
        "📊",
        vec![],
    );

    // -- Transcription -------------------------------------------------------
    reg(
        registry,
        "voice",
        Arc::new(crate::tools::transcription::TranscriptionHandler),
        "🎙️",
        vec!["HERMES_OPENAI_API_KEY".into(), "OPENAI_API_KEY".into()],
    );

    // -- Voice mode ----------------------------------------------------------
    reg(
        registry,
        "voice",
        Arc::new(crate::tools::voice_mode::VoiceModeHandler),
        "🎤",
        vec![],
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

    // -- Env passthrough -----------------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::env_passthrough::EnvPassthroughHandler),
        "🔧",
        vec![],
    );

    // -- Credential files ----------------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::credential_files::CredentialFilesHandler),
        "🔑",
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

    tracing::info!("Registered {} built-in tools", registry.list_tools().len());
}

/// Return the Hermes data directory (`~/.hermes/`), creating it if needed.
fn hermes_data_dir() -> PathBuf {
    let home = dirs_home().unwrap_or_else(|| PathBuf::from("."));
    let dir = home.join(".hermes");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}
