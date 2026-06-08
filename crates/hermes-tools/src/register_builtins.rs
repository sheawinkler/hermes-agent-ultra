//! Registers all built-in tool handlers into a ToolRegistry.
//!
//! This is the Rust equivalent of Python's `_discover_tools()` in `model_tools.py`.
//! Each tool handler is instantiated with its default backend and registered
//! in the appropriate toolset.

use std::path::PathBuf;
use std::sync::Arc;

use hermes_config::voice::{SttConfig, TtsConfig};
use hermes_core::{SkillProvider, TerminalBackend, ToolHandler};

use crate::ToolRegistry;

/// Voice/media config passed into built-in TTS/STT tools.
#[derive(Debug, Clone, Default)]
pub struct VoiceMediaToolConfig {
    pub tts: Option<TtsConfig>,
    pub stt: Option<SttConfig>,
}

/// Register built-in tools without an injected vision backend.
pub fn register_builtin_tools(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
) {
    register_builtin_tools_impl(registry, terminal_backend, skill_provider, None, None);
}

/// Register built-in tools with optional voice (tts/stt) config from `GatewayConfig`.
pub fn register_builtin_tools_with_voice(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    voice: Option<VoiceMediaToolConfig>,
) {
    register_builtin_tools_impl(registry, terminal_backend, skill_provider, None, voice);
}

/// Register all built-in tool handlers into the given registry.
///
/// `terminal_backend` is shared by terminal, file read/write handlers.
/// `skill_provider` is shared by the three skills handlers.
/// `vision_backend` should be an [`AuxiliaryVisionAdapter`] when auxiliary LLM is configured.
pub fn register_builtin_tools_with_vision(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    vision_backend: Option<Arc<dyn crate::tools::vision::VisionBackend>>,
) {
    register_builtin_tools_impl(
        registry,
        terminal_backend,
        skill_provider,
        vision_backend,
        None,
    );
}

pub fn register_builtin_tools_with_vision_and_voice(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    vision_backend: Option<Arc<dyn crate::tools::vision::VisionBackend>>,
    voice: Option<VoiceMediaToolConfig>,
) {
    register_builtin_tools_impl(
        registry,
        terminal_backend,
        skill_provider,
        vision_backend,
        voice,
    );
}

fn register_builtin_tools_impl(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    vision_backend: Option<Arc<dyn crate::tools::vision::VisionBackend>>,
    voice: Option<VoiceMediaToolConfig>,
) {
    let tts_cfg = voice.as_ref().and_then(|v| v.tts.clone());
    let stt_cfg = voice.as_ref().and_then(|v| v.stt.clone());
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
        Arc::new(crate::tools::web::WebExtractHandler::new(Box::new(
            crate::backends::web::SimpleExtractBackend::new(),
        ))),
        "📄",
        vec![],
    );

    // -- Content framework ---------------------------------------------------
    reg(
        registry,
        "content",
        Arc::new(crate::tools::content_framework::ContentPlanHandler),
        "🧭",
        vec![],
    );
    reg(
        registry,
        "content",
        Arc::new(crate::tools::content_framework::ContentNormalizeHandler),
        "🧩",
        vec![],
    );
    reg(
        registry,
        "content",
        Arc::new(crate::tools::content_framework::ContentExecuteHandler),
        "▶️",
        vec![],
    );
    reg(
        registry,
        "capture",
        Arc::new(crate::tools::capture::CaptureHandler),
        "📥",
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

    // -- Vision (requires injected AuxiliaryVisionAdapter from hermes-agent) --
    if let Some(vision_backend) = vision_backend {
        reg(
            registry,
            "vision",
            Arc::new(crate::tools::vision::VisionAnalyzeHandler::new(
                vision_backend.clone(),
            )),
            "👁️",
            vec![],
        );
        reg(
            registry,
            "vision",
            Arc::new(crate::tools::video::VideoAnalyzeHandler::new(Arc::new(
                crate::backends::video::VisionFrameSamplingVideoBackend::new(vision_backend),
            ))),
            "🎬",
            vec![],
        );
    } else {
        tracing::debug!("Skipping vision_analyze/video_analyze — no VisionBackend injected");
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
        let db_path = hermes_config::state_db_path();
        if let Ok(backend) =
            crate::backends::session_search::SqliteSessionSearchBackend::new(
                &db_path.to_string_lossy(),
            )
            .or_else(|_| crate::backends::session_search::SqliteSessionSearchBackend::default_path()) {
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

    // -- Code execution (PTC sandbox needs registry for in-script tool RPC) --
    reg(
        registry,
        "code_execution",
        Arc::new(crate::tools::code_execution::ExecuteCodeHandler::new(
            Arc::new(
                crate::backends::code_execution::LocalCodeExecutionBackend::with_tool_registry(
                    Arc::new(registry.clone()),
                ),
            ),
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
    let tts_backend = Arc::new(crate::backends::tts::MultiTtsBackend::with_config(tts_cfg));
    reg(
        registry,
        "tts",
        Arc::new(crate::tools::tts::TextToSpeechHandler::new(
            tts_backend.clone(),
        )),
        "🔊",
        vec![],
    );

    // -- Browser (10 tools) --------------------------------------------------
    {
        let browser_backend: Arc<dyn crate::tools::browser::BrowserBackend> =
            crate::backends::agent_browser::create_browser_backend();
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

    // -- Computer use (macOS + cua-driver) ----------------------------------
    {
        let handler =
            Arc::new(crate::tools::computer_use::ComputerUseHandler::with_default_backend());
        let schema = handler.schema();
        let name = schema.name.clone();
        let desc = schema.description.clone();
        registry.register(
            name,
            "computer_use",
            schema,
            handler,
            Arc::new(crate::tools::computer_use::check_computer_use_requirements),
            vec![],
            true,
            desc,
            "🖱️",
            None,
        );
    }

    // -- Mixture of Agents ---------------------------------------------------
    reg(
        registry,
        "mixture_of_agents",
        Arc::new(
            crate::tools::mixture_of_agents::MixtureOfAgentsHandler::new(
                Arc::new(crate::tools::mixture_of_agents::StubMoaBackend),
                crate::tools::mixture_of_agents::MoaConfig::default(),
            ),
        ),
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
        Arc::new(crate::tools::transcription::TranscriptionHandler::with_config(stt_cfg)),
        "🎙️",
        vec![
            "VOICE_TOOLS_OPENAI_KEY".into(),
            "HERMES_OPENAI_API_KEY".into(),
            "OPENAI_API_KEY".into(),
            "STT_OPENAI_BASE_URL".into(),
        ],
    );

    // -- Voice mode ----------------------------------------------------------
    reg(
        registry,
        "voice",
        Arc::new(crate::tools::voice_mode::VoiceModeHandler::default()),
        "🎤",
        vec![],
    );

    // -- TTS Premium ---------------------------------------------------------
    reg(
        registry,
        "tts",
        Arc::new(crate::tools::tts_premium::TtsPremiumHandler::new(
            tts_backend,
        )),
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
        Arc::new(crate::tools::url_safety::UrlSafetyHandler::default()),
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

    // -- Feishu (3 tools) ----------------------------------------------------
    if let Some(feishu_client) = crate::tools::feishu::FeishuApiClient::from_env() {
        let feishu = Arc::new(feishu_client);
        let feishu_deps = vec!["FEISHU_APP_ID".into()];
        reg(
            registry,
            "feishu",
            Arc::new(crate::tools::feishu::FeishuCalendarHandler::new(
                feishu.clone(),
            )),
            "📅",
            feishu_deps.clone(),
        );
        reg(
            registry,
            "feishu",
            Arc::new(crate::tools::feishu::FeishuDocsHandler::new(
                feishu.clone(),
            )),
            "📄",
            feishu_deps.clone(),
        );
        reg(
            registry,
            "feishu",
            Arc::new(crate::tools::feishu::FeishuTaskHandler::new(
                feishu.clone(),
            )),
            "✅",
            feishu_deps.clone(),
        );
        reg(
            registry,
            "feishu",
            Arc::new(crate::tools::feishu::FeishuChatHistoryHandler::new(
                feishu,
            )),
            "💬",
            feishu_deps,
        );
    } else {
        tracing::debug!("Skipping feishu tools — FEISHU_APP_ID / FEISHU_APP_SECRET not set");
    }

    tracing::info!(
        tool_count = registry.list_tools().len(),
        "Registered built-in tools"
    );
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

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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

        async fn list_processes(&self) -> Result<serde_json::Value, AgentError> {
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
    }
}
