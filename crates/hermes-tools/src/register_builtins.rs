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
        registry.register(
            name, toolset, schema, handler, check_fn, env_deps, true, desc, emoji, None,
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
    reg_with_check(
        registry,
        "terminal",
        Arc::new(crate::tools::terminal::TerminalHandler::new(
            terminal_backend.clone(),
        )),
        "💻",
        vec![],
        terminal_requirements_check.clone(),
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
            vec!["HERMES_OPENAI_API_KEY".into(), "OPENAI_API_KEY".into()],
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

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use hermes_core::{AgentError, CommandOutput, Skill, SkillMeta};
    use serde_json::Value;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    #[test]
    fn local_backend_exposes_terminal_and_terminal_backed_file_tools() {
        let _lock = ENV_LOCK.lock().unwrap();
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
        let _lock = ENV_LOCK.lock().unwrap();
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
    }

    #[test]
    fn managed_modal_backend_exposes_terminal_tools_without_direct_modal_credentials() {
        let _lock = ENV_LOCK.lock().unwrap();
        let home = tempfile::tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let _userprofile = EnvGuard::set("USERPROFILE", home.path().to_string_lossy().as_ref());
        let _terminal_env = EnvGuard::set("TERMINAL_ENV", "modal");
        let _modal_mode = EnvGuard::set("TERMINAL_MODAL_MODE", "managed");
        let _modal_id = EnvGuard::remove("MODAL_TOKEN_ID");
        let _modal_secret = EnvGuard::remove("MODAL_TOKEN_SECRET");
        let _enabled = EnvGuard::set("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        let _token = EnvGuard::set("TOOL_GATEWAY_USER_TOKEN", "nous-token");
        let _domain = EnvGuard::set("TOOL_GATEWAY_DOMAIN", "tools.example.invalid");
        let names = registered_names();

        assert!(names.contains(&"terminal".to_string()));
        assert!(names.contains(&"process".to_string()));
        assert!(names.contains(&"execute_code".to_string()));
    }

    #[test]
    fn browser_tool_surface_exposes_current_commands_without_legacy_close() {
        let _lock = ENV_LOCK.lock().unwrap();
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
}
