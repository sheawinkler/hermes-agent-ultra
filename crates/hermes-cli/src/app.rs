//! Application state management for the interactive CLI.
//!
//! The `App` struct owns the configuration, agent loop, tool registry,
//! and conversation message history. It coordinates input handling,
//! slash commands, and session management.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Instant, SystemTime};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use hermes_agent::agent_loop::ToolRegistry as AgentToolRegistry;
use hermes_agent::provider::{
    AnthropicProvider, GenericProvider, OpenAiProvider, OpenRouterProvider,
};
use hermes_agent::providers_extra::{
    CopilotProvider, KimiProvider, MiniMaxProvider, NousProvider, QwenProvider,
};
use hermes_agent::sub_agent_orchestrator::SubAgentOrchestrator;
use hermes_agent::{
    AgentCallbacks, AgentConfig, AgentLoop, InterruptController, SessionPersistence,
};
use hermes_config::{hermes_home as hermes_home_dir, load_config, state_dir, GatewayConfig};
use hermes_core::ToolSchema;
use hermes_core::{AgentError, LlmProvider};
use hermes_cron::cron_scheduler_for_data_dir;
use hermes_skills::{FileSkillStore, SkillManager};
use hermes_tools::ToolRegistry;

use crate::alpha_runtime::load_objective_contract;
use crate::auth::{
    resolve_gemini_oauth_runtime_credentials, resolve_nous_runtime_credentials,
    resolve_qwen_runtime_credentials, DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
    NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS, QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
};
use crate::cli::Cli;
use crate::commands::recover_queued_background_jobs;
use crate::model_switch::provider_model_ids;
use crate::runtime_tool_wiring::{wire_cron_scheduler_backend, wire_stdio_clarify_backend};
use crate::terminal_backend::build_terminal_backend;
use crate::tui::StreamHandle;

const SESSION_SNAPSHOT_MAX_FILES_DEFAULT: usize = 1500;
const SESSION_SNAPSHOT_MAX_TOTAL_BYTES_DEFAULT: u64 = 1536 * 1024 * 1024;
const SESSION_SNAPSHOT_MIN_FREE_BYTES_DEFAULT: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone)]
struct SessionSnapshotEntry {
    path: PathBuf,
    modified: SystemTime,
    size_bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PetDock {
    Left,
    #[default]
    Right,
}

impl PetDock {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PetSettings {
    pub enabled: bool,
    pub species: String,
    pub mood: String,
    pub dock: PetDock,
    pub tick_ms: u64,
}

impl Default for PetSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            species: "boba".to_string(),
            mood: "ready".to_string(),
            dock: PetDock::Right,
            tick_ms: 420,
        }
    }
}

impl PetSettings {
    const SPECIES: [&'static str; 6] = ["boba", "bytecat", "otter", "fox", "owl", "capy"];
    const MOODS: [&'static str; 5] = ["ready", "working", "sleepy", "hyped", "chill"];
    const MIN_TICK_MS: u64 = 120;
    const MAX_TICK_MS: u64 = 2000;

    pub fn normalized(mut self) -> Self {
        let species = self.species.trim().to_ascii_lowercase();
        if Self::SPECIES.iter().any(|candidate| *candidate == species) {
            self.species = species;
        } else {
            self.species = Self::default().species;
        }

        let mood = self.mood.trim().to_ascii_lowercase();
        if Self::MOODS.iter().any(|candidate| *candidate == mood) {
            self.mood = mood;
        } else {
            self.mood = Self::default().mood;
        }

        self.tick_ms = self.tick_ms.clamp(Self::MIN_TICK_MS, Self::MAX_TICK_MS);
        self
    }

    pub fn species_catalog() -> &'static [&'static str] {
        &Self::SPECIES
    }

    pub fn mood_catalog() -> &'static [&'static str] {
        &Self::MOODS
    }
}

fn read_env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn read_env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn snapshot_max_files() -> usize {
    read_env_usize(
        "HERMES_SESSION_SNAPSHOT_MAX_FILES",
        SESSION_SNAPSHOT_MAX_FILES_DEFAULT,
    )
}

fn snapshot_max_total_bytes() -> u64 {
    read_env_u64(
        "HERMES_SESSION_SNAPSHOT_MAX_TOTAL_BYTES",
        SESSION_SNAPSHOT_MAX_TOTAL_BYTES_DEFAULT,
    )
}

fn snapshot_min_free_bytes() -> u64 {
    read_env_u64(
        "HERMES_SESSION_SNAPSHOT_MIN_FREE_BYTES",
        SESSION_SNAPSHOT_MIN_FREE_BYTES_DEFAULT,
    )
}

fn list_session_snapshot_entries(sessions_dir: &Path) -> Vec<SessionSnapshotEntry> {
    let mut entries: Vec<SessionSnapshotEntry> = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(sessions_dir) else {
        return entries;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        entries.push(SessionSnapshotEntry {
            path,
            modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            size_bytes: meta.len(),
        });
    }
    entries.sort_by_key(|row| row.modified);
    entries
}

#[cfg(unix)]
fn available_disk_space_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c_path` is a valid NUL-terminated C string and `stats` points
    // to valid writable memory for the kernel call.
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), stats.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: `rc == 0` means `stats` was initialized by `statvfs`.
    let stats = unsafe { stats.assume_init() };
    Some((stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64))
}

#[cfg(not(unix))]
fn available_disk_space_bytes(_path: &Path) -> Option<u64> {
    None
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

/// Top-level application state for an interactive Hermes session.
pub struct App {
    /// Resolved Hermes state root (respects `-C/--config-dir`).
    pub state_root: PathBuf,

    /// Loaded gateway configuration.
    pub config: Arc<GatewayConfig>,

    /// The agent loop engine.
    pub agent: Arc<AgentLoop>,

    /// The tool registry (shared with the agent).
    pub tool_registry: Arc<ToolRegistry>,

    /// Active tool schemas exposed to the model for this runtime.
    pub tool_schemas: Vec<ToolSchema>,

    /// Conversation messages for the current session.
    pub messages: Vec<hermes_core::Message>,

    /// UI-only transcript messages (slash commands, local notices), anchored
    /// to a conversation index so they do not pollute model context.
    pub ui_messages: Vec<UiTranscriptMessage>,

    /// Unique identifier for the current session.
    pub session_id: String,

    /// Whether the application loop is still running.
    pub running: bool,

    /// Currently active model identifier (e.g. "openai:gpt-4o").
    pub current_model: String,

    /// Currently active personality name.
    pub current_personality: Option<String>,

    /// History of user inputs for recall.
    pub input_history: Vec<String>,

    /// Index into input_history for up/down arrow navigation.
    pub history_index: usize,

    /// Interrupt controller for stopping agent execution.
    pub interrupt_controller: InterruptController,

    /// Optional TUI streaming sink for incremental chunks.
    pub stream_handle: Option<StreamHandle>,
    /// Shared streaming sink used by agent callbacks for progress events.
    stream_handle_shared: Arc<StdMutex<Option<StreamHandle>>>,
    /// Whether TUI mouse events are enabled.
    pub mouse_enabled: bool,
    /// Pending skin/theme slug to apply in the TUI loop.
    pub pending_theme: Option<String>,
    /// Optional image path hint injected into the next user prompt.
    pub pending_image_hint: Option<String>,
    /// Optional durable objective for the current interactive session.
    pub session_objective: Option<String>,
    /// Animated companion pet settings.
    pub pet_settings: PetSettings,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("state_root", &self.state_root)
            .field("session_id", &self.session_id)
            .field("running", &self.running)
            .field("current_model", &self.current_model)
            .field("current_personality", &self.current_personality)
            .field("history_index", &self.history_index)
            .field("mouse_enabled", &self.mouse_enabled)
            .field("pending_theme", &self.pending_theme)
            .field("pending_image_hint", &self.pending_image_hint)
            .field("session_objective", &self.session_objective)
            .field("pet_settings", &self.pet_settings)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// SessionInfo (for serialization)
// ---------------------------------------------------------------------------

/// Serializable snapshot of a session (for save/restore).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub model: String,
    pub personality: Option<String>,
    pub message_count: usize,
    pub created_at: String,
}

/// A TUI-local transcript message anchored to a conversation position.
#[derive(Debug, Clone)]
pub struct UiTranscriptMessage {
    /// Conversation message count at insertion time.
    pub insert_at: usize,
    /// Rendered message payload.
    pub message: hermes_core::Message,
}

// ---------------------------------------------------------------------------
// App implementation
// ---------------------------------------------------------------------------

impl App {
    const SESSION_OBJECTIVE_PREFIX: &'static str = "[SESSION_OBJECTIVE] ";

    fn ensure_session_stub_snapshot(&self) {
        if let Err(err) = self.persist_session_snapshot(None) {
            tracing::warn!("session startup snapshot skipped: {}", err);
        }
    }

    fn push_stream_extra_event(
        shared: &Arc<StdMutex<Option<StreamHandle>>>,
        payload: serde_json::Value,
    ) {
        if let Ok(guard) = shared.lock() {
            if let Some(handle) = guard.clone() {
                handle.send_chunk(hermes_core::StreamChunk {
                    delta: Some(hermes_core::StreamDelta {
                        content: None,
                        tool_calls: None,
                        extra: Some(payload),
                    }),
                    finish_reason: None,
                    usage: None,
                });
            }
        }
    }

    fn preview_for_status(raw: &str, max_chars: usize) -> String {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.chars().count() <= max_chars {
            collapsed
        } else {
            let mut out: String = collapsed
                .chars()
                .take(max_chars.saturating_sub(1))
                .collect();
            out.push('…');
            out
        }
    }

    fn set_env_if_changed(key: &str, value: &str) -> bool {
        let next = value.trim();
        if next.is_empty() {
            return false;
        }
        let current = std::env::var(key).ok().unwrap_or_default();
        if current == next {
            return false;
        }
        std::env::set_var(key, next);
        true
    }

    async fn refresh_runtime_provider_credentials_if_needed(&mut self, force_refresh: bool) {
        let (provider_name, _) = resolve_provider_and_model(&self.config, &self.current_model);
        let provider = normalize_runtime_provider_name(provider_name.as_str());
        let mut rotated = false;
        let mut note: Option<String> = None;

        match provider.as_str() {
            "nous" => match resolve_nous_runtime_credentials(
                force_refresh,
                true,
                NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
            )
            .await
            {
                Ok(creds) => {
                    rotated |= Self::set_env_if_changed("NOUS_API_KEY", &creds.api_key);
                    if !creds.base_url.trim().is_empty() {
                        rotated |=
                            Self::set_env_if_changed("NOUS_INFERENCE_BASE_URL", &creds.base_url);
                    }
                    if rotated {
                        note = Some("refreshed Nous runtime credential".to_string());
                    }
                }
                Err(e) => {
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!("warning: Nous credential refresh skipped ({e})"),
                    );
                }
            },
            "qwen-oauth" => match resolve_qwen_runtime_credentials(
                force_refresh,
                true,
                QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
            )
            .await
            {
                Ok(creds) => {
                    rotated |=
                        Self::set_env_if_changed("HERMES_QWEN_OAUTH_API_KEY", &creds.api_key);
                    rotated |= Self::set_env_if_changed("DASHSCOPE_API_KEY", &creds.api_key);
                    if !creds.base_url.trim().is_empty() {
                        rotated |=
                            Self::set_env_if_changed("HERMES_QWEN_BASE_URL", &creds.base_url);
                    }
                    if rotated {
                        note = Some("refreshed Qwen OAuth runtime credential".to_string());
                    }
                }
                Err(e) => {
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!("warning: Qwen OAuth refresh skipped ({e})"),
                    );
                }
            },
            "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => {
                match resolve_gemini_oauth_runtime_credentials(force_refresh).await {
                    Ok(creds) => {
                        rotated |=
                            Self::set_env_if_changed("HERMES_GEMINI_OAUTH_API_KEY", &creds.api_key);
                        rotated |= Self::set_env_if_changed("GOOGLE_API_KEY", &creds.api_key);
                        rotated |= Self::set_env_if_changed("GEMINI_API_KEY", &creds.api_key);
                        if rotated {
                            note = Some("refreshed Gemini OAuth runtime credential".to_string());
                        }
                    }
                    Err(e) => {
                        Self::emit_lifecycle_event(
                            &self.stream_handle_shared,
                            format!("warning: Gemini OAuth refresh skipped ({e})"),
                        );
                    }
                }
            }
            _ => {}
        }

        if rotated {
            self.switch_model(&self.current_model.clone());
        }
        if let Some(msg) = note {
            Self::emit_lifecycle_event(&self.stream_handle_shared, msg);
        }
    }

    fn stream_callbacks(shared: Arc<StdMutex<Option<StreamHandle>>>) -> AgentCallbacks {
        let thinking_shared = shared.clone();
        let tool_start_shared = shared.clone();
        let tool_done_shared = shared.clone();
        let status_shared = shared;
        AgentCallbacks {
            on_thinking: Some(Box::new(move |thinking: &str| {
                let preview = App::preview_for_status(thinking, 220);
                if preview.is_empty() {
                    return;
                }
                App::push_stream_extra_event(
                    &thinking_shared,
                    serde_json::json!({
                        "ui_event": "thinking",
                        "text": preview,
                    }),
                );
            })),
            on_tool_start: Some(Box::new(move |tool: &str, args: &Value| {
                let arg_preview = App::preview_for_status(&args.to_string(), 140);
                App::push_stream_extra_event(
                    &tool_start_shared,
                    serde_json::json!({
                        "ui_event": "tool_start",
                        "tool": tool,
                        "args_preview": arg_preview,
                    }),
                );
            })),
            on_tool_complete: Some(Box::new(move |tool: &str, content: &str| {
                let preview = App::preview_for_status(content, 160);
                App::push_stream_extra_event(
                    &tool_done_shared,
                    serde_json::json!({
                        "ui_event": "tool_complete",
                        "tool": tool,
                        "result_preview": preview,
                    }),
                );
            })),
            status_callback: Some(Arc::new(move |event_type: &str, message: &str| {
                let preview = App::preview_for_status(message, 200);
                if preview.is_empty() {
                    return;
                }
                App::push_stream_extra_event(
                    &status_shared,
                    serde_json::json!({
                        "ui_event": "status",
                        "event_type": event_type,
                        "message": preview,
                    }),
                );
            })),
            ..AgentCallbacks::default()
        }
    }

    fn emit_lifecycle_event(
        shared: &Arc<StdMutex<Option<StreamHandle>>>,
        message: impl AsRef<str>,
    ) {
        let preview = App::preview_for_status(message.as_ref(), 220);
        if preview.is_empty() {
            return;
        }
        App::push_stream_extra_event(
            shared,
            serde_json::json!({
                "ui_event": "lifecycle",
                "message": preview,
            }),
        );
    }

    fn emit_phase_event(
        shared: &Arc<StdMutex<Option<StreamHandle>>>,
        phase: &str,
        label: &str,
        progress_pct: u8,
    ) {
        let phase = phase.trim();
        let label = App::preview_for_status(label, 220);
        if phase.is_empty() || label.is_empty() {
            return;
        }
        App::push_stream_extra_event(
            shared,
            serde_json::json!({
                "ui_event": "phase",
                "phase": phase,
                "label": label,
                "progress_pct": progress_pct.min(100),
            }),
        );
    }

    fn objective_context_autopin_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_OBJECTIVE_CONTEXT_AUTOPIN")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn sanitize_topic_path_segment(raw: &str) -> String {
        let mut out = String::with_capacity(raw.len());
        for ch in raw.chars() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '/') {
                out.push(ch);
            } else {
                out.push('-');
            }
        }
        out.trim_matches('-').to_string()
    }

    fn maybe_autopin_contextlattice_topic_from_objective(&self) {
        if !Self::objective_context_autopin_enabled() {
            return;
        }
        let Ok(Some(contract)) = load_objective_contract() else {
            return;
        };
        let objective_id = Self::sanitize_topic_path_segment(contract.id.trim());
        if objective_id.is_empty() {
            return;
        }
        let target_topic = format!("runbooks/objective/{}", objective_id);
        let current_topic = std::env::var("CONTEXTLATTICE_TOPIC_PATH")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let should_override = match current_topic.as_deref() {
            None => true,
            Some("runbooks/hermes") => true,
            Some(existing)
                if existing.eq_ignore_ascii_case(target_topic.as_str())
                    || !existing
                        .to_ascii_lowercase()
                        .starts_with("runbooks/objective/") =>
            {
                false
            }
            Some(_) => true,
        };
        if should_override {
            std::env::set_var("CONTEXTLATTICE_TOPIC_PATH", &target_topic);
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!(
                    "ContextLattice objective autopin set topic_path={} (objective_id={})",
                    target_topic, contract.id
                ),
            );
            Self::emit_phase_event(
                &self.stream_handle_shared,
                "context",
                "objective context autopin",
                8,
            );
        }
    }

    /// Create a new `App` from the parsed CLI arguments.
    ///
    /// This loads (or creates) the gateway configuration, builds a tool
    /// registry with the configured tools, constructs an LLM provider,
    /// and initializes the agent loop.
    pub async fn new(cli: Cli) -> Result<Self, AgentError> {
        let state_root = state_dir(cli.config_dir.as_deref().map(std::path::Path::new));
        let config = load_config(cli.config_dir.as_deref())
            .map_err(|e| AgentError::Config(e.to_string()))?;

        let mut config = config;
        apply_cli_runtime_overrides(&mut config, &cli);

        if config.sessions.auto_prune {
            let resolved_home = config
                .home_dir
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var("HERMES_HOME")
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                })
                .unwrap_or_else(hermes_home_dir);
            let sp = SessionPersistence::new(&resolved_home);
            let maintenance = sp.maybe_auto_prune_and_vacuum(
                config.sessions.retention_days,
                config.sessions.min_interval_hours,
                config.sessions.vacuum_after_prune,
            );
            if let Some(err) = maintenance.error {
                tracing::debug!("sessions db auto-maintenance skipped: {}", err);
            } else if !maintenance.skipped && maintenance.pruned > 0 {
                tracing::info!(
                    "sessions db auto-maintenance pruned {} session(s){}",
                    maintenance.pruned,
                    if maintenance.vacuumed {
                        " + vacuum"
                    } else {
                        ""
                    }
                );
            }
        }

        let configured_model = config.model.clone().unwrap_or_else(|| "gpt-4o".to_string());
        let current_model = resolve_startup_model(&config, &configured_model);
        let current_personality = config.personality.clone();

        sync_runtime_model_env(&config, &current_model);

        let tool_registry = Arc::new(ToolRegistry::new());
        if default_rtk_raw_mode() {
            tool_registry.set_raw_mode(true);
        }
        let stream_handle_shared: Arc<StdMutex<Option<StreamHandle>>> =
            Arc::new(StdMutex::new(None));
        let terminal_backend = build_terminal_backend(&config);
        let skill_store = Arc::new(FileSkillStore::new(FileSkillStore::default_dir()));
        let skill_provider: Arc<dyn hermes_core::SkillProvider> =
            Arc::new(SkillManager::new(skill_store));
        hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
        wire_stdio_clarify_backend(&tool_registry);
        let cron_data_dir = state_root.join("cron");
        std::fs::create_dir_all(&cron_data_dir)
            .map_err(|e| AgentError::Io(format!("cron dir {}: {}", cron_data_dir.display(), e)))?;
        let cron_scheduler = Arc::new(cron_scheduler_for_data_dir(cron_data_dir));
        cron_scheduler
            .load_persisted_jobs()
            .await
            .map_err(|e| AgentError::Config(format!("cron load: {e}")))?;
        cron_scheduler.start().await;
        wire_cron_scheduler_backend(&tool_registry, cron_scheduler);
        let agent_tool_registry = Arc::new(bridge_tool_registry(&tool_registry));
        let tool_schemas =
            crate::platform_toolsets::resolve_platform_tool_schemas(&config, "cli", &tool_registry);

        let agent_config = build_agent_config(&config, &current_model);
        let provider = build_provider(&config, &current_model);

        let agent_inner = AgentLoop::new(agent_config, agent_tool_registry, provider)
            .with_callbacks(Self::stream_callbacks(stream_handle_shared.clone()));
        let orchestrator = Arc::new(SubAgentOrchestrator::from_parent(
            &agent_inner,
            state_root.clone(),
        ));
        let agent = Arc::new(agent_inner.with_sub_agent_orchestrator(orchestrator));

        let recovered_background_jobs = recover_queued_background_jobs(8);
        if recovered_background_jobs > 0 {
            tracing::info!(
                "Recovered {} queued background job(s) from durable status queue",
                recovered_background_jobs
            );
        }

        let app = Self {
            state_root,
            config: Arc::new(config),
            agent,
            tool_registry,
            tool_schemas,
            messages: Vec::new(),
            ui_messages: Vec::new(),
            session_id: Uuid::new_v4().to_string(),
            running: true,
            current_model,
            current_personality,
            input_history: Vec::new(),
            history_index: 0,
            interrupt_controller: InterruptController::new(),
            stream_handle: None,
            stream_handle_shared,
            mouse_enabled: default_mouse_enabled(),
            pending_theme: None,
            pending_image_hint: None,
            session_objective: None,
            pet_settings: load_pet_settings(),
        };
        app.ensure_session_stub_snapshot();
        Ok(app)
    }

    /// Attach a streaming handle (used by TUI mode).
    pub fn set_stream_handle(&mut self, handle: Option<StreamHandle>) {
        if let Ok(mut guard) = self.stream_handle_shared.lock() {
            *guard = handle.clone();
        }
        self.stream_handle = handle;
    }

    /// Enable/disable TUI mouse handling.
    pub fn set_mouse_enabled(&mut self, enabled: bool) {
        self.mouse_enabled = enabled;
    }

    /// Current TUI mouse handling state.
    pub fn mouse_enabled(&self) -> bool {
        self.mouse_enabled
    }

    /// Queue a TUI skin/theme change request to be applied in the UI loop.
    pub fn request_theme_change(&mut self, skin: &str) {
        let value = skin.trim();
        if value.is_empty() {
            return;
        }
        self.pending_theme = Some(value.to_string());
    }

    /// Queue an image hint for the next user prompt.
    pub fn set_pending_image_hint(&mut self, path: String) {
        let trimmed = path.trim();
        self.pending_image_hint = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }

    /// Read queued image hint without consuming it.
    pub fn pending_image_hint(&self) -> Option<&str> {
        self.pending_image_hint.as_deref()
    }

    /// Clear queued image hint.
    pub fn clear_pending_image_hint(&mut self) {
        self.pending_image_hint = None;
    }

    /// Prepare outbound user text, consuming any queued image hint.
    pub fn prepare_user_message(&mut self, raw: &str) -> String {
        let base = raw.trim();
        if let Some(path) = self
            .pending_image_hint
            .take()
            .filter(|value| !value.trim().is_empty())
        {
            format!("[IMAGE_HINT] path={}\n{}", path, base)
        } else {
            base.to_string()
        }
    }

    /// Drain any queued skin/theme change request.
    pub fn take_pending_theme_change(&mut self) -> Option<String> {
        self.pending_theme.take()
    }

    /// Retrieve current companion pet settings.
    pub fn pet_settings(&self) -> &PetSettings {
        &self.pet_settings
    }

    /// Update and persist companion pet settings.
    pub fn set_pet_settings(&mut self, settings: PetSettings) -> Result<(), AgentError> {
        let normalized = settings.normalized();
        persist_pet_settings(&normalized)?;
        self.pet_settings = normalized;
        Ok(())
    }

    /// Run the interactive REPL loop.
    ///
    /// This is the main entry point for interactive mode. It delegates
    /// to the TUI subsystem for rendering and event handling.
    pub async fn run_interactive(&mut self) -> Result<(), AgentError> {
        // The actual TUI loop is in crate::tui::run()
        // This method exists so non-TUI callers can drive the loop manually.
        if self.running {
            loop {
                if !self.running {
                    break;
                }
                // In a real implementation, the TUI event loop would drive this.
                // Here we just mark that we're ready.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
        Ok(())
    }

    /// Handle a line of user input.
    ///
    /// If the input starts with `/` it is treated as a slash command.
    /// Otherwise it is sent as a user message to the agent.
    pub async fn handle_input(&mut self, input: &str) -> Result<(), AgentError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        // Store in input history
        self.input_history.push(trimmed.to_string());
        self.history_index = self.input_history.len();

        if trimmed.starts_with('/') {
            if self.stream_handle.is_some() {
                self.push_ui_user(trimmed);
            }
            // Parse the slash command and its arguments
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            let cmd = parts[0];
            let args: Vec<&str> = parts
                .get(1)
                .map(|s| s.split_whitespace().collect())
                .unwrap_or_default();

            let result = crate::commands::handle_slash_command(self, cmd, &args).await?;
            if result == crate::commands::CommandResult::Quit {
                self.running = false;
            }
        } else {
            // Regular user message
            let user_message = self.prepare_user_message(trimmed);
            self.messages.push(hermes_core::Message::user(user_message));
            self.run_agent().await?;
        }

        Ok(())
    }

    /// Handle a slash command string (without the leading `/`).
    pub async fn handle_command(&mut self, cmd: &str) -> Result<(), AgentError> {
        let trimmed = cmd.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
        let slash_cmd = if parts[0].starts_with('/') {
            parts[0]
        } else {
            // Prepend / if not present
            return self.handle_input(&format!("/{}", trimmed)).await;
        };

        if self.stream_handle.is_some() {
            self.push_ui_user(trimmed);
        }

        let args: Vec<&str> = parts
            .get(1)
            .map(|s| s.split_whitespace().collect())
            .unwrap_or_default();

        let result = crate::commands::handle_slash_command(self, slash_cmd, &args).await?;
        if result == crate::commands::CommandResult::Quit {
            self.running = false;
        }
        Ok(())
    }

    /// Create a new session, clearing all messages.
    pub fn new_session(&mut self) {
        self.session_id = Uuid::new_v4().to_string();
        self.messages.clear();
        self.ui_messages.clear();
        self.pending_image_hint = None;
        self.session_objective = None;
        self.input_history.clear();
        self.history_index = 0;
        self.ensure_session_stub_snapshot();
    }

    /// Reset the current session (clear messages but keep session ID).
    pub fn reset_session(&mut self) {
        self.messages.clear();
        self.ui_messages.clear();
        self.pending_image_hint = None;
        self.session_objective = None;
        self.input_history.clear();
        self.history_index = 0;
    }

    /// Set or clear a durable session objective.
    ///
    /// The objective is represented as a synthetic system message so it is
    /// applied consistently on every turn without requiring user re-entry.
    pub fn set_session_objective(&mut self, objective: Option<String>) {
        self.messages.retain(|m| {
            if m.role != hermes_core::MessageRole::System {
                return true;
            }
            !m.content
                .as_deref()
                .unwrap_or_default()
                .starts_with(Self::SESSION_OBJECTIVE_PREFIX)
        });

        self.session_objective = objective
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        if let Some(obj) = &self.session_objective {
            let system =
                hermes_core::Message::system(format!("{}{}", Self::SESSION_OBJECTIVE_PREFIX, obj));
            self.messages.insert(0, system);
        }
        self.prune_ui_after_current_messages();
    }

    /// Retry the last user message by re-sending it to the agent.
    ///
    /// Finds the last user message in history, removes all messages after it
    /// (including the assistant response), and re-runs the agent.
    pub async fn retry_last(&mut self) -> Result<(), AgentError> {
        // Find the last user message
        let last_user_idx = self
            .messages
            .iter()
            .rposition(|m| m.role == hermes_core::MessageRole::User);

        if let Some(idx) = last_user_idx {
            let last_user_msg = self.messages[idx].clone();
            // Truncate messages to just before the last user message
            self.messages.truncate(idx);
            // Re-add the user message
            self.messages.push(last_user_msg);
            // Re-run the agent
            self.run_agent().await?;
            self.prune_ui_after_current_messages();
        }

        Ok(())
    }

    /// Undo the last exchange (remove the last user message and its response).
    pub fn undo_last(&mut self) {
        // Find the last user message
        if let Some(idx) = self
            .messages
            .iter()
            .rposition(|m| m.role == hermes_core::MessageRole::User)
        {
            // Remove everything from the last user message onward
            self.messages.truncate(idx);
            self.prune_ui_after_current_messages();
        }
    }

    /// Switch the active model, rebuilding the provider and agent loop.
    pub fn switch_model(&mut self, provider_model: &str) {
        self.current_model = provider_model.to_string();
        sync_runtime_model_env(&self.config, &self.current_model);

        let provider = build_provider(&self.config, &self.current_model);
        let agent_config = build_agent_config(&self.config, &self.current_model);
        let agent_tool_registry = Arc::new(bridge_tool_registry(&self.tool_registry));

        let agent_inner = AgentLoop::new(agent_config, agent_tool_registry, provider)
            .with_callbacks(Self::stream_callbacks(self.stream_handle_shared.clone()));
        let orchestrator = Arc::new(SubAgentOrchestrator::from_parent(
            &agent_inner,
            self.state_root.clone(),
        ));
        self.agent = Arc::new(agent_inner.with_sub_agent_orchestrator(orchestrator));

        tracing::info!("Switched model to: {}", provider_model);
    }

    /// Switch the active personality.
    pub fn switch_personality(&mut self, name: &str) {
        self.current_personality = Some(name.to_string());
        tracing::info!("Switched personality to: {}", name);
    }

    /// Run the agent on the current message history.
    ///
    /// Sends all messages to the agent loop and appends the result.
    /// Checks the interrupt controller before running and clears it after.
    async fn run_agent(&mut self) -> Result<(), AgentError> {
        let run_started_at = Instant::now();
        self.maybe_autopin_contextlattice_topic_from_objective();
        Self::emit_phase_event(
            &self.stream_handle_shared,
            "preflight",
            "runtime preflight + credential hydration",
            5,
        );
        self.refresh_runtime_provider_credentials_if_needed(false)
            .await;
        Self::emit_phase_event(
            &self.stream_handle_shared,
            "dispatch",
            "dispatching model request",
            15,
        );
        self.interrupt_controller.clear_interrupt();
        let mut remediation_attempted = false;
        let mut auth_refresh_attempted = false;
        loop {
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!(
                    "dispatching request to {} (messages={})",
                    self.current_model,
                    self.messages.len()
                ),
            );
            Self::emit_phase_event(
                &self.stream_handle_shared,
                "inference",
                "model inference + tool execution",
                35,
            );
            let messages = self.messages.clone();
            let result = if self.config.streaming.enabled {
                let stream_handle = self.stream_handle.clone();
                let stream_cb: Option<Box<dyn Fn(hermes_core::StreamChunk) + Send + Sync>> =
                    stream_handle.map(|h| {
                        Box::new(move |chunk: hermes_core::StreamChunk| {
                            h.send_chunk(chunk);
                        })
                            as Box<dyn Fn(hermes_core::StreamChunk) + Send + Sync>
                    });
                self.agent
                    .run_stream(messages, Some(self.tool_schemas.clone()), stream_cb)
                    .await
            } else {
                self.agent
                    .run(messages, Some(self.tool_schemas.clone()))
                    .await
            };

            match result {
                Ok(result) => {
                    self.messages = result.messages;
                    self.prune_ui_after_current_messages();
                    if let Err(err) = self.persist_session_snapshot(None) {
                        tracing::warn!("session autosave skipped: {}", err);
                    }
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!(
                            "run finished in {:.2}s (total_turns={})",
                            run_started_at.elapsed().as_secs_f64(),
                            result.total_turns
                        ),
                    );
                    Self::emit_phase_event(
                        &self.stream_handle_shared,
                        "finalize",
                        "transcript finalization + persistence",
                        100,
                    );
                    if let Some(handle) = &self.stream_handle {
                        handle.send_done();
                    }
                    if result.interrupted {
                        tracing::info!("Agent loop returned interrupted=true (graceful stop)");
                        if self.stream_handle.is_some() {
                            self.push_ui_assistant("[Agent execution interrupted]");
                        } else {
                            println!("[Agent execution interrupted]");
                        }
                    } else if !result.finished_naturally {
                        tracing::warn!(
                            "Agent stopped after {} turns (did not finish naturally)",
                            result.total_turns
                        );
                    }
                    break;
                }
                Err(AgentError::Interrupted { message }) => {
                    self.interrupt_controller.clear_interrupt();
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!(
                            "run interrupted after {:.2}s",
                            run_started_at.elapsed().as_secs_f64()
                        ),
                    );
                    if let Some(handle) = &self.stream_handle {
                        handle.send_done();
                    }
                    if let Some(redirect) = message {
                        tracing::info!("Agent interrupted with redirect: {}", redirect);
                    } else {
                        tracing::info!("Agent interrupted by user");
                    }
                    if self.stream_handle.is_some() {
                        self.push_ui_assistant("[Agent execution interrupted]");
                    } else {
                        println!("[Agent execution interrupted]");
                    }
                    break;
                }
                Err(e) => {
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!(
                            "run error after {:.2}s: {}",
                            run_started_at.elapsed().as_secs_f64(),
                            e
                        ),
                    );
                    Self::emit_phase_event(
                        &self.stream_handle_shared,
                        "recovery",
                        "error handling + remediation",
                        60,
                    );
                    if let Some(handle) = &self.stream_handle {
                        handle.send_done();
                    }
                    if !auth_refresh_attempted
                        && Self::is_provider_auth_or_session_error(&e)
                        && self.force_auth_refresh_after_error().await
                    {
                        auth_refresh_attempted = true;
                        continue;
                    }
                    if !remediation_attempted {
                        if let Some((next_model, notice)) =
                            self.model_auto_remediation_target(&e).await
                        {
                            tracing::warn!(
                                "Model auto-remediation triggered: {} -> {}",
                                self.current_model,
                                next_model
                            );
                            if self.stream_handle.is_some() {
                                self.push_ui_assistant(notice.clone());
                            } else {
                                println!("{notice}");
                            }
                            Self::emit_lifecycle_event(
                                &self.stream_handle_shared,
                                format!("auto-remediation switching model to {}", next_model),
                            );
                            self.switch_model(&next_model);
                            remediation_attempted = true;
                            continue;
                        }
                    }
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    /// Append a UI-only message anchored to the current conversation size.
    pub fn push_ui_message(&mut self, message: hermes_core::Message) {
        self.ui_messages.push(UiTranscriptMessage {
            insert_at: self.messages.len(),
            message,
        });
    }

    /// Append a UI-only user transcript line.
    pub fn push_ui_user(&mut self, text: impl Into<String>) {
        self.push_ui_message(hermes_core::Message::user(text.into()));
    }

    /// Append a UI-only assistant transcript line.
    pub fn push_ui_assistant(&mut self, text: impl Into<String>) {
        self.push_ui_message(hermes_core::Message::assistant(text.into()));
    }

    /// Build the merged transcript for TUI rendering.
    ///
    /// This includes durable conversation history and UI-only events in
    /// chronological order, while preserving model-facing context purity.
    pub fn transcript_messages(&self) -> Vec<hermes_core::Message> {
        let mut merged = Vec::with_capacity(self.messages.len() + self.ui_messages.len());
        for idx in 0..=self.messages.len() {
            for ui in self.ui_messages.iter().filter(|m| m.insert_at == idx) {
                merged.push(ui.message.clone());
            }
            if idx < self.messages.len() {
                merged.push(self.messages[idx].clone());
            }
        }
        merged
    }

    fn prune_ui_after_current_messages(&mut self) {
        let cap = self.messages.len();
        self.ui_messages.retain(|m| m.insert_at <= cap);
    }

    /// Apply the finalized messages returned by an agent run.
    pub fn apply_agent_result(&mut self, result: hermes_core::AgentResult) {
        self.messages = result.messages;
        self.prune_ui_after_current_messages();
    }

    /// Count background jobs currently queued/running.
    pub fn running_background_job_count(&self) -> usize {
        let jobs_dir = hermes_config::hermes_home().join("background_jobs");
        let mut active = 0usize;
        let entries = match std::fs::read_dir(jobs_dir) {
            Ok(entries) => entries,
            Err(_) => return 0,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
                continue;
            };
            let status = value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if matches!(status, "queued" | "running") {
                active += 1;
            }
        }
        active
    }

    fn prune_session_snapshot_entry(
        entry: &SessionSnapshotEntry,
        total_bytes: &mut u64,
    ) -> Result<(), AgentError> {
        match std::fs::remove_file(&entry.path) {
            Ok(()) => {
                *total_bytes = total_bytes.saturating_sub(entry.size_bytes);
                Ok(())
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(AgentError::Io(format!(
                "Failed to prune session snapshot {}: {}",
                entry.path.display(),
                err
            ))),
        }
    }

    fn enforce_session_snapshot_guardrails(
        &self,
        sessions_dir: &Path,
        preserve_path: &Path,
    ) -> Result<(), AgentError> {
        let preserve = preserve_path.to_path_buf();
        let mut entries = list_session_snapshot_entries(sessions_dir);
        let mut total_bytes = entries.iter().map(|e| e.size_bytes).sum::<u64>();

        let max_files = snapshot_max_files();
        if max_files > 0 {
            while entries.len() > max_files {
                let Some(idx) = entries.iter().position(|entry| entry.path != preserve) else {
                    break;
                };
                let removed = entries.remove(idx);
                Self::prune_session_snapshot_entry(&removed, &mut total_bytes)?;
            }
        }

        let max_total_bytes = snapshot_max_total_bytes();
        if max_total_bytes > 0 {
            while total_bytes > max_total_bytes {
                let Some(idx) = entries.iter().position(|entry| entry.path != preserve) else {
                    break;
                };
                let removed = entries.remove(idx);
                Self::prune_session_snapshot_entry(&removed, &mut total_bytes)?;
            }
        }

        let min_free_bytes = snapshot_min_free_bytes();
        if min_free_bytes > 0 {
            if let Some(mut free_bytes) = available_disk_space_bytes(sessions_dir) {
                while free_bytes < min_free_bytes {
                    let Some(idx) = entries.iter().position(|entry| entry.path != preserve) else {
                        break;
                    };
                    let removed = entries.remove(idx);
                    Self::prune_session_snapshot_entry(&removed, &mut total_bytes)?;
                    free_bytes = available_disk_space_bytes(sessions_dir).unwrap_or(free_bytes);
                }
                if free_bytes < min_free_bytes {
                    return Err(AgentError::Io(format!(
                        "Session snapshot write blocked by disk guardrail: free={} bytes, required_min={} bytes (dir={})",
                        free_bytes,
                        min_free_bytes,
                        sessions_dir.display()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Get a serializable snapshot of the current session info.
    pub fn session_info(&self) -> SessionInfo {
        SessionInfo {
            session_id: self.session_id.clone(),
            model: self.current_model.clone(),
            personality: self.current_personality.clone(),
            message_count: self.messages.len(),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Persist a JSON session snapshot to `<state_root>/sessions`.
    ///
    /// When `name_override` is provided, that value is used as the file stem.
    /// Otherwise the active `session_id` is used.
    pub fn persist_session_snapshot(
        &self,
        name_override: Option<&str>,
    ) -> Result<PathBuf, AgentError> {
        let sessions_dir = self.state_root.join("sessions");
        std::fs::create_dir_all(&sessions_dir).map_err(|e| {
            AgentError::Io(format!(
                "Failed to create sessions dir {}: {}",
                sessions_dir.display(),
                e
            ))
        })?;
        let stem = name_override
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(self.session_id.as_str());
        let path = sessions_dir.join(format!("{stem}.json"));
        let payload = serde_json::json!({
            "session_info": self.session_info(),
            "messages": self.messages.iter().map(|m| {
                serde_json::json!({
                    "role": format!("{:?}", m.role),
                    "content": m.content.as_deref().unwrap_or(""),
                    "tool_call_id": m.tool_call_id,
                    "tool_calls": m.tool_calls,
                    "reasoning_content": m.reasoning_content,
                })
            }).collect::<Vec<_>>(),
        });
        let json = serde_json::to_string_pretty(&payload).map_err(|e| {
            AgentError::Config(format!("Failed to serialize session snapshot: {e}"))
        })?;
        std::fs::write(&path, json).map_err(|e| {
            AgentError::Io(format!(
                "Failed to write session snapshot {}: {}",
                path.display(),
                e
            ))
        })?;
        self.enforce_session_snapshot_guardrails(&sessions_dir, &path)?;
        Ok(path)
    }

    fn model_auto_remediation_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_MODEL_AUTO_REMEDIATE")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn is_model_not_found_error(err: &AgentError) -> bool {
        let message = match err {
            AgentError::LlmApi(msg)
            | AgentError::Config(msg)
            | AgentError::ToolExecution(msg)
            | AgentError::Gateway(msg) => msg.to_ascii_lowercase(),
            _ => return false,
        };
        let model_not_found = message.contains("model not found")
            || message.contains("requested model does not exist")
            || message.contains("404 not found")
            || message.contains("openrouter catalog");
        model_not_found && message.contains("model")
    }

    fn is_provider_auth_or_session_error(err: &AgentError) -> bool {
        let message = match err {
            AgentError::LlmApi(msg)
            | AgentError::Config(msg)
            | AgentError::ToolExecution(msg)
            | AgentError::Gateway(msg)
            | AgentError::AuthFailed(msg) => msg.to_ascii_lowercase(),
            _ => return false,
        };
        message.contains("401")
            || message.contains("403")
            || message.contains("unauthorized")
            || message.contains("invalid token")
            || message.contains("token expired")
            || message.contains("invalid_token")
            || message.contains("expired")
            || message.contains("authentication")
    }

    async fn force_auth_refresh_after_error(&mut self) -> bool {
        let (provider_name, _) = resolve_provider_and_model(&self.config, &self.current_model);
        let provider = normalize_runtime_provider_name(provider_name.as_str());
        let notice = match provider.as_str() {
            "nous" => match resolve_nous_runtime_credentials(
                true,
                true,
                NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
            )
            .await
            {
                Ok(creds) => {
                    let mut changed = false;
                    changed |= Self::set_env_if_changed("NOUS_API_KEY", &creds.api_key);
                    if !creds.base_url.trim().is_empty() {
                        changed |=
                            Self::set_env_if_changed("NOUS_INFERENCE_BASE_URL", &creds.base_url);
                    }
                    if changed {
                        self.switch_model(&self.current_model.clone());
                    }
                    Some("Nous auth auto-refresh succeeded; retrying request.".to_string())
                }
                Err(err) => Some(format!("Nous auth auto-refresh failed: {}", err)),
            },
            "qwen-oauth" => {
                match resolve_qwen_runtime_credentials(
                    true,
                    true,
                    QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                )
                .await
                {
                    Ok(creds) => {
                        let mut changed = false;
                        changed |=
                            Self::set_env_if_changed("HERMES_QWEN_OAUTH_API_KEY", &creds.api_key);
                        changed |= Self::set_env_if_changed("DASHSCOPE_API_KEY", &creds.api_key);
                        if !creds.base_url.trim().is_empty() {
                            changed |=
                                Self::set_env_if_changed("HERMES_QWEN_BASE_URL", &creds.base_url);
                        }
                        if changed {
                            self.switch_model(&self.current_model.clone());
                        }
                        Some("Qwen OAuth auto-refresh succeeded; retrying request.".to_string())
                    }
                    Err(err) => Some(format!("Qwen OAuth auto-refresh failed: {}", err)),
                }
            }
            "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => {
                match resolve_gemini_oauth_runtime_credentials(true).await {
                    Ok(creds) => {
                        let mut changed = false;
                        changed |=
                            Self::set_env_if_changed("HERMES_GEMINI_OAUTH_API_KEY", &creds.api_key);
                        changed |= Self::set_env_if_changed("GOOGLE_API_KEY", &creds.api_key);
                        changed |= Self::set_env_if_changed("GEMINI_API_KEY", &creds.api_key);
                        if changed {
                            self.switch_model(&self.current_model.clone());
                        }
                        Some("Gemini OAuth auto-refresh succeeded; retrying request.".to_string())
                    }
                    Err(err) => Some(format!("Gemini OAuth auto-refresh failed: {}", err)),
                }
            }
            _ => None,
        };

        if let Some(text) = notice {
            Self::emit_lifecycle_event(&self.stream_handle_shared, &text);
            if self.stream_handle.is_some() {
                self.push_ui_assistant(text);
            } else {
                println!("{}", text);
            }
            return true;
        }
        false
    }

    async fn model_auto_remediation_target(&self, err: &AgentError) -> Option<(String, String)> {
        if !Self::model_auto_remediation_enabled() || !Self::is_model_not_found_error(err) {
            return None;
        }

        let (provider, current_model_id) = self
            .current_model
            .split_once(':')
            .unwrap_or(("openai", self.current_model.as_str()));
        let provider = provider.trim().to_ascii_lowercase();
        if provider.is_empty() {
            return None;
        }

        let catalog = provider_model_ids(&provider).await;
        if catalog.is_empty() {
            return None;
        }

        let current_trimmed = current_model_id.trim().to_ascii_lowercase();
        let slash_suffix = format!("/{}", current_trimmed);
        let selected = catalog
            .iter()
            .find(|m| {
                let lower = m.trim().to_ascii_lowercase();
                lower == current_trimmed || lower.ends_with(&slash_suffix)
            })
            .cloned()
            .or_else(|| catalog.first().cloned())?;

        let next_model = format!("{}:{}", provider, selected.trim());
        if next_model.eq_ignore_ascii_case(&self.current_model) {
            return None;
        }
        let notice = format!(
            "Model catalog remediation: `{}` failed with not-found; switching to `{}` and retrying once.",
            self.current_model, next_model
        );
        Some((next_model, notice))
    }

    /// Navigate backward in input history.
    pub fn history_prev(&mut self) -> Option<&str> {
        if self.history_index > 0 {
            self.history_index -= 1;
            self.input_history
                .get(self.history_index)
                .map(|s| s.as_str())
        } else {
            None
        }
    }

    /// Navigate forward in input history.
    pub fn history_next(&mut self) -> Option<&str> {
        if self.history_index < self.input_history.len() {
            self.history_index += 1;
            if self.history_index < self.input_history.len() {
                self.input_history
                    .get(self.history_index)
                    .map(|s| s.as_str())
            } else {
                None
            }
        } else {
            None
        }
    }
}

fn apply_cli_runtime_overrides(config: &mut GatewayConfig, cli: &Cli) {
    if let Some(ref model) = cli.model {
        config.model = Some(model.clone());
    }
    if let Some(ref personality) = cli.personality {
        config.personality = Some(personality.clone());
    }
    if let Some(provider) = cli
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        let provider = normalize_runtime_provider_name(provider);
        let existing_model = config.model.as_deref().unwrap_or("gpt-4o").trim();
        let model_name = existing_model
            .split_once(':')
            .map(|(_, name)| name.trim())
            .unwrap_or(existing_model);
        config.model = Some(format!("{provider}:{model_name}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alpha_runtime::upsert_objective_contract;
    use crate::test_env_lock;
    use hermes_config::LlmProviderConfig;
    use std::collections::HashMap;

    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        test_env_lock::lock()
    }

    fn build_minimal_test_app() -> App {
        let config = Arc::new(GatewayConfig::default());
        let tool_registry = Arc::new(ToolRegistry::new());
        let agent_tool_registry = Arc::new(bridge_tool_registry(&tool_registry));
        let agent_config = build_agent_config(config.as_ref(), "openai:gpt-4o");
        let provider: Arc<dyn LlmProvider> = Arc::new(NoBackendProvider {
            model: "openai:gpt-4o".to_string(),
        });
        let agent_inner = AgentLoop::new(agent_config, agent_tool_registry, provider)
            .with_callbacks(App::stream_callbacks(Arc::new(StdMutex::new(None))));
        let orchestrator = Arc::new(SubAgentOrchestrator::from_parent(
            &agent_inner,
            hermes_home_dir(),
        ));
        let agent = Arc::new(agent_inner.with_sub_agent_orchestrator(orchestrator));

        App {
            state_root: hermes_home_dir(),
            config,
            agent,
            tool_registry,
            tool_schemas: Vec::new(),
            messages: Vec::new(),
            ui_messages: Vec::new(),
            session_id: "test-session".to_string(),
            running: true,
            current_model: "openai:gpt-4o".to_string(),
            current_personality: None,
            input_history: Vec::new(),
            history_index: 0,
            interrupt_controller: InterruptController::new(),
            stream_handle: None,
            stream_handle_shared: Arc::new(StdMutex::new(None)),
            mouse_enabled: true,
            pending_theme: None,
            pending_image_hint: None,
            session_objective: None,
            pet_settings: PetSettings::default(),
        }
    }

    #[test]
    fn test_session_info_serialization() {
        let info = SessionInfo {
            session_id: "test-123".to_string(),
            model: "gpt-4o".to_string(),
            personality: Some("helpful".to_string()),
            message_count: 5,
            created_at: "2025-01-01T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session_id, "test-123");
        assert_eq!(back.model, "gpt-4o");
    }

    #[test]
    fn test_persist_session_snapshot_writes_default_session_file() {
        let _guard = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());

        let mut app = build_minimal_test_app();
        app.session_id = "resume-test".to_string();
        app.messages = vec![
            hermes_core::Message::system("[SESSION_OBJECTIVE] Preserve context"),
            hermes_core::Message::user("hello"),
            hermes_core::Message::assistant("world"),
        ];

        let path = app
            .persist_session_snapshot(None)
            .expect("persist session snapshot");
        assert!(path.ends_with("resume-test.json"));
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).expect("read snapshot");
        let value: serde_json::Value = serde_json::from_str(&content).expect("parse snapshot");
        assert_eq!(
            value
                .get("session_info")
                .and_then(|v| v.get("session_id"))
                .and_then(|v| v.as_str()),
            Some("resume-test")
        );
        assert_eq!(
            value
                .get("messages")
                .and_then(|v| v.as_array())
                .map(|v| v.len()),
            Some(3)
        );

        match prev_home {
            Some(val) => std::env::set_var("HERMES_HOME", val),
            None => std::env::remove_var("HERMES_HOME"),
        }
    }

    #[test]
    fn test_persist_session_snapshot_respects_app_state_root() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut app = build_minimal_test_app();
        app.state_root = tmp.path().join("custom-state-root");
        app.session_id = "state-root-test".to_string();
        app.messages = vec![hermes_core::Message::user("ping")];

        let path = app
            .persist_session_snapshot(None)
            .expect("persist session snapshot");
        assert_eq!(
            path,
            app.state_root.join("sessions").join("state-root-test.json")
        );
        assert!(path.exists());
    }

    #[test]
    fn test_new_session_persists_startup_stub_snapshot() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut app = build_minimal_test_app();
        app.state_root = tmp.path().join("custom-state-root");
        std::fs::create_dir_all(app.state_root.join("sessions")).expect("create sessions dir");
        let old_session_id = app.session_id.clone();

        app.new_session();

        assert_ne!(app.session_id, old_session_id);
        let snapshot_path = app
            .state_root
            .join("sessions")
            .join(format!("{}.json", app.session_id));
        assert!(snapshot_path.exists());

        let content = std::fs::read_to_string(&snapshot_path).expect("read snapshot");
        let value: serde_json::Value = serde_json::from_str(&content).expect("parse snapshot");
        assert_eq!(
            value
                .get("messages")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len()),
            Some(0)
        );
    }

    #[test]
    fn test_persist_session_snapshot_prunes_old_files_by_count_limit() {
        let _guard = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let prev_max_files = std::env::var("HERMES_SESSION_SNAPSHOT_MAX_FILES").ok();
        let prev_max_total = std::env::var("HERMES_SESSION_SNAPSHOT_MAX_TOTAL_BYTES").ok();
        let prev_min_free = std::env::var("HERMES_SESSION_SNAPSHOT_MIN_FREE_BYTES").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::set_var("HERMES_SESSION_SNAPSHOT_MAX_FILES", "2");
        std::env::set_var("HERMES_SESSION_SNAPSHOT_MAX_TOTAL_BYTES", "999999999");
        std::env::set_var("HERMES_SESSION_SNAPSHOT_MIN_FREE_BYTES", "0");

        let mut app = build_minimal_test_app();
        app.session_id = "snap-prune".to_string();
        app.messages = vec![hermes_core::Message::user("snapshot payload")];

        let p1 = app
            .persist_session_snapshot(Some("older-1"))
            .expect("persist snapshot 1");
        let p2 = app
            .persist_session_snapshot(Some("older-2"))
            .expect("persist snapshot 2");
        let p3 = app
            .persist_session_snapshot(Some("newest"))
            .expect("persist snapshot 3");
        assert!(!p1.exists(), "oldest snapshot should be pruned");
        assert!(p2.exists(), "middle snapshot should remain");
        assert!(p3.exists(), "newest snapshot should remain");

        let sessions_dir = app.state_root.join("sessions");
        let remaining: Vec<_> = std::fs::read_dir(&sessions_dir)
            .expect("read sessions dir")
            .flatten()
            .filter(|entry| entry.path().extension().and_then(|v| v.to_str()) == Some("json"))
            .collect();
        assert_eq!(remaining.len(), 2, "snapshot file count should be capped");

        match prev_min_free {
            Some(v) => std::env::set_var("HERMES_SESSION_SNAPSHOT_MIN_FREE_BYTES", v),
            None => std::env::remove_var("HERMES_SESSION_SNAPSHOT_MIN_FREE_BYTES"),
        }
        match prev_max_total {
            Some(v) => std::env::set_var("HERMES_SESSION_SNAPSHOT_MAX_TOTAL_BYTES", v),
            None => std::env::remove_var("HERMES_SESSION_SNAPSHOT_MAX_TOTAL_BYTES"),
        }
        match prev_max_files {
            Some(v) => std::env::set_var("HERMES_SESSION_SNAPSHOT_MAX_FILES", v),
            None => std::env::remove_var("HERMES_SESSION_SNAPSHOT_MAX_FILES"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }
    }

    #[test]
    fn test_apply_cli_runtime_overrides_applies_provider_to_prefixed_model() {
        let mut cfg = GatewayConfig::default();
        cfg.model = Some("openai:gpt-4o".to_string());
        let cli = Cli {
            command: None,
            verbose: false,
            config_dir: None,
            model: None,
            provider: Some("nous".to_string()),
            oneshot: None,
            allow_tools: false,
            personality: None,
            ignore_user_config: false,
            ignore_rules: false,
        };

        apply_cli_runtime_overrides(&mut cfg, &cli);
        assert_eq!(cfg.model.as_deref(), Some("nous:gpt-4o"));
    }

    #[test]
    fn test_apply_cli_runtime_overrides_applies_provider_to_bare_model() {
        let mut cfg = GatewayConfig::default();
        cfg.model = Some("moonshotai/kimi-k2.6".to_string());
        let cli = Cli {
            command: None,
            verbose: false,
            config_dir: None,
            model: None,
            provider: Some("anthropic".to_string()),
            oneshot: None,
            allow_tools: false,
            personality: None,
            ignore_user_config: false,
            ignore_rules: false,
        };

        apply_cli_runtime_overrides(&mut cfg, &cli);
        assert_eq!(cfg.model.as_deref(), Some("anthropic:moonshotai/kimi-k2.6"));
    }

    #[test]
    fn test_build_agent_config_maps_runtime_provider_api_key_env() {
        let mut cfg = GatewayConfig::default();
        let mut providers = HashMap::new();
        providers.insert(
            "custom".to_string(),
            LlmProviderConfig {
                api_key: None,
                api_key_env: Some("MY_FALLBACK_KEY".to_string()),
                ..LlmProviderConfig::default()
            },
        );
        cfg.llm_providers = providers;

        let agent_cfg = build_agent_config(&cfg, "custom:some-model");
        let runtime = agent_cfg
            .runtime_providers
            .get("custom")
            .expect("runtime provider should exist");
        assert_eq!(runtime.api_key_env.as_deref(), Some("MY_FALLBACK_KEY"));
    }

    #[test]
    fn test_build_agent_config_forwards_provider_extra_body() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "nous".to_string(),
            LlmProviderConfig {
                extra_body: Some(serde_json::json!({
                    "reasoning_effort": "high",
                    "reasoning": { "effort": "high" }
                })),
                ..LlmProviderConfig::default()
            },
        );
        let agent_cfg = build_agent_config(&cfg, "nous:moonshotai/kimi-k2.6");
        assert_eq!(
            agent_cfg
                .extra_body
                .as_ref()
                .and_then(|body| body.get("reasoning_effort"))
                .and_then(|value| value.as_str()),
            Some("high")
        );
    }

    #[test]
    fn test_build_agent_config_infers_provider_for_bare_model() {
        let mut cfg = GatewayConfig::default();
        cfg.model = Some("claude-opus-4-6".to_string());
        cfg.llm_providers.insert(
            "anthropic".to_string(),
            LlmProviderConfig {
                model: Some("claude-opus-4-6".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "claude-opus-4-6");
        assert_eq!(agent_cfg.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn test_resolve_provider_and_model_uses_single_provider_fallback() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers
            .insert("stepfun".to_string(), LlmProviderConfig::default());
        let (provider, model) = resolve_provider_and_model(&cfg, "step-3.5-flash");
        assert_eq!(provider, "stepfun");
        assert_eq!(model, "step-3.5-flash");
    }

    #[test]
    fn test_resolve_startup_model_prefers_provider_runtime_model_for_provider_slug() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "nous".to_string(),
            LlmProviderConfig {
                model: Some("moonshotai/kimi-k2.6".to_string()),
                ..LlmProviderConfig::default()
            },
        );
        let startup = resolve_startup_model(&cfg, "nous");
        assert_eq!(startup, "nous:moonshotai/kimi-k2.6");
    }

    #[test]
    fn test_sync_runtime_model_env_sets_model_and_provider_values() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers
            .insert("anthropic".to_string(), LlmProviderConfig::default());

        let keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        for key in keys {
            std::env::remove_var(key);
        }
        std::env::set_var("HERMES_TUI_PROVIDER", "openai");

        sync_runtime_model_env(&cfg, "anthropic:claude-sonnet-4-6");

        assert_eq!(
            std::env::var("HERMES_MODEL").ok().as_deref(),
            Some("anthropic:claude-sonnet-4-6")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_MODEL").ok().as_deref(),
            Some("anthropic:claude-sonnet-4-6")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            std::env::var("HERMES_TUI_PROVIDER").ok().as_deref(),
            Some("anthropic")
        );

        for key in keys {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn test_provider_api_key_from_env_supports_stepfun() {
        let hermes_var = "HERMES_STEPFUN_API_KEY";
        let stepfun_var = "STEPFUN_API_KEY";
        std::env::remove_var(hermes_var);
        std::env::remove_var(stepfun_var);

        std::env::set_var(stepfun_var, "stepfun-direct");
        assert_eq!(
            provider_api_key_from_env("stepfun").as_deref(),
            Some("stepfun-direct")
        );

        std::env::set_var(hermes_var, "stepfun-hermes");
        assert_eq!(
            provider_api_key_from_env("stepfun").as_deref(),
            Some("stepfun-hermes")
        );

        std::env::remove_var(hermes_var);
        std::env::remove_var(stepfun_var);
    }

    #[test]
    fn test_provider_api_key_from_env_supports_openai_codex() {
        let var = "HERMES_OPENAI_CODEX_API_KEY";
        std::env::remove_var(var);
        std::env::set_var(var, "codex-oauth-token");
        assert_eq!(
            provider_api_key_from_env("openai-codex").as_deref(),
            Some("codex-oauth-token")
        );
        std::env::remove_var(var);
    }

    #[test]
    fn test_provider_api_key_from_env_supports_anthropic_aliases() {
        let primary = "ANTHROPIC_API_KEY";
        let secondary = "ANTHROPIC_TOKEN";
        let tertiary = "CLAUDE_CODE_OAUTH_TOKEN";
        std::env::remove_var(primary);
        std::env::remove_var(secondary);
        std::env::remove_var(tertiary);

        std::env::set_var(tertiary, "claude-oauth-token");
        assert_eq!(
            provider_api_key_from_env("anthropic").as_deref(),
            Some("claude-oauth-token")
        );

        std::env::set_var(secondary, "anthropic-token");
        assert_eq!(
            provider_api_key_from_env("anthropic").as_deref(),
            Some("anthropic-token")
        );

        std::env::set_var(primary, "anthropic-api-key");
        assert_eq!(
            provider_api_key_from_env("anthropic").as_deref(),
            Some("anthropic-api-key")
        );

        std::env::remove_var(primary);
        std::env::remove_var(secondary);
        std::env::remove_var(tertiary);
    }

    #[test]
    fn test_provider_api_key_from_env_supports_qwen_oauth() {
        let oauth_var = "HERMES_QWEN_OAUTH_API_KEY";
        let fallback_var = "DASHSCOPE_API_KEY";
        std::env::remove_var(oauth_var);
        std::env::remove_var(fallback_var);

        std::env::set_var(fallback_var, "dashscope-fallback");
        assert_eq!(
            provider_api_key_from_env("qwen-oauth").as_deref(),
            Some("dashscope-fallback")
        );

        std::env::set_var(oauth_var, "qwen-oauth-token");
        assert_eq!(
            provider_api_key_from_env("qwen-oauth").as_deref(),
            Some("qwen-oauth-token")
        );

        std::env::remove_var(oauth_var);
        std::env::remove_var(fallback_var);
    }

    #[test]
    fn test_provider_api_key_from_env_supports_google_gemini_cli() {
        let var = "HERMES_GEMINI_OAUTH_API_KEY";
        std::env::remove_var(var);
        std::env::set_var(var, "google-gemini-oauth-token");
        assert_eq!(
            provider_api_key_from_env("google-gemini-cli").as_deref(),
            Some("google-gemini-oauth-token")
        );
        std::env::remove_var(var);
    }

    #[test]
    fn test_provider_api_key_from_env_supports_extended_registry() {
        let checks = [
            ("AI_GATEWAY_API_KEY", "ai-gateway"),
            ("DEEPSEEK_API_KEY", "deepseek"),
            ("HF_TOKEN", "huggingface"),
            ("KILOCODE_API_KEY", "kilocode"),
            ("NVIDIA_API_KEY", "nvidia"),
            ("OLLAMA_LOCAL_API_KEY", "ollama-local"),
            ("LLAMA_CPP_API_KEY", "llama-cpp"),
            ("VLLM_API_KEY", "vllm"),
            ("MLX_API_KEY", "mlx"),
            ("APPLE_ANE_API_KEY", "apple-ane"),
            ("SGLANG_API_KEY", "sglang"),
            ("TGI_API_KEY", "tgi"),
            ("OPENCODE_GO_API_KEY", "opencode-go"),
            ("OPENCODE_ZEN_API_KEY", "opencode-zen"),
            ("XAI_API_KEY", "xai"),
            ("XIAOMI_API_KEY", "xiaomi"),
            ("GLM_API_KEY", "zai"),
        ];
        for (env_var, provider) in checks {
            std::env::remove_var(env_var);
            let expected = format!("token-for-{provider}");
            std::env::set_var(env_var, expected.clone());
            assert_eq!(
                provider_api_key_from_env(provider).as_deref(),
                Some(expected.as_str())
            );
            std::env::remove_var(env_var);
        }
    }

    #[test]
    fn test_normalize_runtime_provider_name_covers_aliases() {
        assert_eq!(
            normalize_runtime_provider_name("gemini-cli"),
            "google-gemini-cli"
        );
        assert_eq!(normalize_runtime_provider_name("moonshot"), "kimi");
        assert_eq!(
            normalize_runtime_provider_name("alibaba-coding-plan"),
            "qwen"
        );
        assert_eq!(normalize_runtime_provider_name("opencode"), "opencode-zen");
        assert_eq!(normalize_runtime_provider_name("ollama"), "ollama-local");
        assert_eq!(normalize_runtime_provider_name("llama.cpp"), "llama-cpp");
        assert_eq!(normalize_runtime_provider_name("ollvm"), "vllm");
        assert_eq!(normalize_runtime_provider_name("llvm"), "vllm");
        assert_eq!(normalize_runtime_provider_name("mlx-lm"), "mlx");
        assert_eq!(normalize_runtime_provider_name("ane"), "apple-ane");
    }

    #[test]
    fn test_allow_no_api_key_for_local_backends_and_private_base_urls() {
        assert!(allow_no_api_key("ollama-local", "ollama-local", None));
        assert!(allow_no_api_key(
            "openai",
            "openai",
            Some("http://127.0.0.1:11434/v1")
        ));
        assert!(allow_no_api_key(
            "custom",
            "custom",
            Some("http://192.168.1.20:8000/v1")
        ));
        assert!(allow_no_api_key(
            "custom",
            "custom",
            Some("http://[::1]:11434/v1")
        ));
        assert!(!allow_no_api_key(
            "openai",
            "openai",
            Some("https://api.openai.com/v1")
        ));
    }

    #[test]
    fn test_default_mouse_enabled_respects_env_override() {
        std::env::remove_var("HERMES_TUI_MOUSE");
        assert!(default_mouse_enabled());

        std::env::set_var("HERMES_TUI_MOUSE", "off");
        assert!(!default_mouse_enabled());

        std::env::set_var("HERMES_TUI_MOUSE", "1");
        assert!(default_mouse_enabled());

        std::env::remove_var("HERMES_TUI_MOUSE");
    }

    #[test]
    fn test_pet_settings_normalization_clamps_and_rewrites_invalid_values() {
        let input = PetSettings {
            enabled: true,
            species: "unknown".to_string(),
            mood: "invalid".to_string(),
            dock: PetDock::Left,
            tick_ms: 10,
        };
        let normalized = input.normalized();
        assert!(normalized.enabled);
        assert_eq!(normalized.species, "boba");
        assert_eq!(normalized.mood, "ready");
        assert_eq!(normalized.dock, PetDock::Left);
        assert_eq!(normalized.tick_ms, 120);
    }

    #[test]
    fn test_load_pet_settings_uses_persisted_file_if_present() {
        let _lock = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        std::fs::write(
            tmp.path().join("pet.json"),
            r#"{"enabled":true,"species":"fox","mood":"hyped","dock":"left","tick_ms":180}"#,
        )
        .expect("write pet settings");
        let loaded = load_pet_settings();
        assert!(loaded.enabled);
        assert_eq!(loaded.species, "fox");
        assert_eq!(loaded.mood, "hyped");
        assert_eq!(loaded.dock, PetDock::Left);
        assert_eq!(loaded.tick_ms, 180);
        std::env::remove_var("HERMES_HOME");
    }

    #[test]
    fn test_default_rtk_raw_mode_respects_env_override() {
        std::env::remove_var("HERMES_RTK_RAW");
        assert!(!default_rtk_raw_mode());

        std::env::set_var("HERMES_RTK_RAW", "on");
        assert!(default_rtk_raw_mode());

        std::env::set_var("HERMES_RTK_RAW", "0");
        assert!(!default_rtk_raw_mode());

        std::env::remove_var("HERMES_RTK_RAW");
    }

    #[test]
    fn test_is_model_not_found_error_detects_provider_404_shape() {
        let err = AgentError::LlmApi(
            "API error 404 Not Found: model foo/bar not found in OpenRouter catalog".to_string(),
        );
        assert!(App::is_model_not_found_error(&err));
    }

    #[test]
    fn test_is_model_not_found_error_ignores_non_catalog_errors() {
        let err = AgentError::LlmApi("Rate limit exceeded".to_string());
        assert!(!App::is_model_not_found_error(&err));
    }

    #[test]
    fn test_is_provider_auth_or_session_error_detects_auth_failures() {
        let err = AgentError::LlmApi("HTTP 401 Unauthorized: token expired".to_string());
        assert!(App::is_provider_auth_or_session_error(&err));
        let non_auth = AgentError::LlmApi("API error 404 Not Found: model missing".to_string());
        assert!(!App::is_provider_auth_or_session_error(&non_auth));
    }

    #[test]
    fn test_set_session_objective_injects_replaces_and_clears_system_message() {
        let mut app = build_minimal_test_app();
        app.messages
            .push(hermes_core::Message::user("hello before objective"));

        app.set_session_objective(Some(
            "Ship parity with upstream plus stronger UX".to_string(),
        ));
        assert_eq!(
            app.session_objective.as_deref(),
            Some("Ship parity with upstream plus stronger UX")
        );
        assert_eq!(app.messages.len(), 2);
        assert_eq!(app.messages[0].role, hermes_core::MessageRole::System);
        let system = app.messages[0].content.clone().unwrap_or_default();
        assert!(system.starts_with("[SESSION_OBJECTIVE] "));
        assert!(system.contains("Ship parity with upstream plus stronger UX"));

        app.set_session_objective(Some("Minimize latency regressions".to_string()));
        let system_count = app
            .messages
            .iter()
            .filter(|m| {
                m.role == hermes_core::MessageRole::System
                    && m.content
                        .as_deref()
                        .unwrap_or_default()
                        .starts_with("[SESSION_OBJECTIVE] ")
            })
            .count();
        assert_eq!(system_count, 1);
        assert_eq!(
            app.session_objective.as_deref(),
            Some("Minimize latency regressions")
        );

        app.set_session_objective(None);
        assert!(app.session_objective.is_none());
        assert!(app.messages.iter().all(|m| {
            !m.content
                .as_deref()
                .unwrap_or_default()
                .starts_with("[SESSION_OBJECTIVE] ")
        }));
    }

    #[test]
    fn test_objective_context_autopin_sets_topic_for_default_path() {
        let _guard = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let prev_topic = std::env::var("CONTEXTLATTICE_TOPIC_PATH").ok();
        let prev_toggle = std::env::var("HERMES_OBJECTIVE_CONTEXT_AUTOPIN").ok();

        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::set_var("CONTEXTLATTICE_TOPIC_PATH", "runbooks/hermes");
        std::env::set_var("HERMES_OBJECTIVE_CONTEXT_AUTOPIN", "1");

        let contract = upsert_objective_contract("grow wallet safely", true).expect("objective");
        let app = build_minimal_test_app();
        app.maybe_autopin_contextlattice_topic_from_objective();
        let expected = format!("runbooks/objective/{}", contract.id);
        assert_eq!(
            std::env::var("CONTEXTLATTICE_TOPIC_PATH").ok().as_deref(),
            Some(expected.as_str())
        );

        match prev_toggle {
            Some(v) => std::env::set_var("HERMES_OBJECTIVE_CONTEXT_AUTOPIN", v),
            None => std::env::remove_var("HERMES_OBJECTIVE_CONTEXT_AUTOPIN"),
        }
        match prev_topic {
            Some(v) => std::env::set_var("CONTEXTLATTICE_TOPIC_PATH", v),
            None => std::env::remove_var("CONTEXTLATTICE_TOPIC_PATH"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }
    }

    #[test]
    fn test_objective_context_autopin_respects_custom_topic_pin() {
        let _guard = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let prev_topic = std::env::var("CONTEXTLATTICE_TOPIC_PATH").ok();

        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::set_var("CONTEXTLATTICE_TOPIC_PATH", "runbooks/custom/keep-me");

        let _contract =
            upsert_objective_contract("objective override regression test", false).expect("obj");
        let app = build_minimal_test_app();
        app.maybe_autopin_contextlattice_topic_from_objective();
        assert_eq!(
            std::env::var("CONTEXTLATTICE_TOPIC_PATH").ok().as_deref(),
            Some("runbooks/custom/keep-me")
        );

        match prev_topic {
            Some(v) => std::env::set_var("CONTEXTLATTICE_TOPIC_PATH", v),
            None => std::env::remove_var("CONTEXTLATTICE_TOPIC_PATH"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: build AgentConfig from GatewayConfig
// ---------------------------------------------------------------------------

pub fn build_agent_config(config: &GatewayConfig, model: &str) -> AgentConfig {
    let (resolved_provider, _) = resolve_provider_and_model(config, model);
    let runtime_provider = normalize_runtime_provider_name(resolved_provider.as_str());
    let provider_extra_body = config
        .llm_providers
        .get(resolved_provider.as_str())
        .or_else(|| config.llm_providers.get(runtime_provider.as_str()))
        .or_else(|| {
            config.llm_providers.iter().find_map(|(name, cfg)| {
                if name.eq_ignore_ascii_case(resolved_provider.as_str())
                    || name.eq_ignore_ascii_case(runtime_provider.as_str())
                {
                    Some(cfg)
                } else {
                    None
                }
            })
        })
        .and_then(|cfg| cfg.extra_body.clone());
    let skip_memory_env = std::env::var("HERMES_SKIP_MEMORY")
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    let skip_context_files_env = std::env::var("HERMES_SKIP_CONTEXT_FILES")
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    let hermes_home = config
        .home_dir
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home);
    let skip_memory = skip_memory_env || hermes_home.join(".memory_disabled").exists();
    let skip_context_files = config.agent.skip_context_files || skip_context_files_env;

    AgentConfig {
        max_turns: config.max_turns,
        budget: config.budget.clone(),
        model: model.to_string(),
        system_prompt: config.system_prompt.clone(),
        personality: config.personality.clone(),
        extra_body: provider_extra_body,
        hermes_home: config.home_dir.clone(),
        provider: Some(resolved_provider),
        stream: config.streaming.enabled,
        skip_memory,
        skip_context_files,
        platform: Some("cli".to_string()),
        enabled_skills: config.skills.enabled.clone(),
        disabled_skills: config.skills.disabled.clone(),
        pass_session_id: true,
        runtime_providers: config
            .llm_providers
            .iter()
            .map(|(name, cfg)| {
                (
                    name.clone(),
                    hermes_agent::agent_loop::RuntimeProviderConfig {
                        api_key: cfg.api_key.clone(),
                        api_key_env: cfg.api_key_env.clone(),
                        base_url: cfg.base_url.clone(),
                        command: cfg.command.clone(),
                        args: cfg.args.clone(),
                        oauth_token_url: cfg.oauth_token_url.clone(),
                        oauth_client_id: cfg.oauth_client_id.clone(),
                    },
                )
            })
            .collect(),
        smart_model_routing: hermes_agent::agent_loop::SmartModelRoutingConfig {
            enabled: config.smart_model_routing.enabled,
            max_simple_chars: config.smart_model_routing.max_simple_chars,
            max_simple_words: config.smart_model_routing.max_simple_words,
            cheap_model: config.smart_model_routing.cheap_model.as_ref().map(|m| {
                hermes_agent::agent_loop::CheapModelRouteConfig {
                    provider: m.provider.clone(),
                    model: m.model.clone(),
                    base_url: m.base_url.clone(),
                    api_key_env: m.api_key_env.clone(),
                }
            }),
        },
        memory_nudge_interval: config.agent.memory_nudge_interval,
        skill_creation_nudge_interval: config.agent.skill_creation_nudge_interval,
        background_review_enabled: config.agent.background_review_enabled,
        code_index_enabled: config.agent.code_index_enabled,
        code_index_max_files: config.agent.code_index_max_files,
        code_index_max_symbols: config.agent.code_index_max_symbols,
        lsp_context_enabled: config.agent.lsp_context_enabled,
        lsp_context_max_chars: config.agent.lsp_context_max_chars,
        ..AgentConfig::default()
    }
}

// ---------------------------------------------------------------------------
// Helper: bridge hermes_tools::ToolRegistry → agent_loop::ToolRegistry
// ---------------------------------------------------------------------------

pub fn bridge_tool_registry(tools: &ToolRegistry) -> AgentToolRegistry {
    let mut agent_registry = AgentToolRegistry::new();
    for schema in tools.get_definitions() {
        let name = schema.name.clone();
        let tools_clone = tools.clone();
        agent_registry.register(
            name.clone(),
            schema,
            Arc::new(
                move |params: Value| -> Result<String, hermes_core::ToolError> {
                    Ok(tools_clone.dispatch(&name, params))
                },
            ),
        );
    }
    agent_registry
}

// ---------------------------------------------------------------------------
// Helper: build LLM provider from config + model string
// ---------------------------------------------------------------------------

const STEPFUN_BASE_URL: &str = "https://api.stepfun.ai/step_plan/v1";
const OPENAI_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const QWEN_BASE_URL: &str = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1";
const ALIBABA_CODING_PLAN_BASE_URL: &str = "https://coding-intl.dashscope.aliyuncs.com/v1";
const GOOGLE_GEMINI_CLI_BASE_URL: &str = "cloudcode-pa://google";
const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const AI_GATEWAY_BASE_URL: &str = "https://ai-gateway.vercel.sh/v1";
const KIMI_CODING_BASE_URL: &str = "https://api.moonshot.ai/v1";
const KIMI_CODING_CN_BASE_URL: &str = "https://api.moonshot.cn/v1";
const MINIMAX_CN_BASE_URL: &str = "https://api.minimaxi.com/anthropic";
const XAI_BASE_URL: &str = "https://api.x.ai/v1";
const NVIDIA_BASE_URL: &str = "https://integrate.api.nvidia.com/v1";
const OPENCODE_GO_BASE_URL: &str = "https://opencode.ai/zen/go/v1";
const OPENCODE_ZEN_BASE_URL: &str = "https://opencode.ai/zen/v1";
const KILOCODE_BASE_URL: &str = "https://api.kilo.ai/api/gateway";
const HUGGINGFACE_BASE_URL: &str = "https://router.huggingface.co/v1";
const XIAOMI_BASE_URL: &str = "https://api.xiaomimimo.com/v1";
const ZAI_BASE_URL: &str = "https://api.z.ai/api/paas/v4";
const ARCEE_BASE_URL: &str = "https://api.arcee.ai/api/v1";
const OLLAMA_CLOUD_BASE_URL: &str = "https://ollama.com/v1";
const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com/v1";
const OLLAMA_LOCAL_BASE_URL: &str = "http://127.0.0.1:11434/v1";
const LLAMA_CPP_BASE_URL: &str = "http://127.0.0.1:8080/v1";
const VLLM_BASE_URL: &str = "http://127.0.0.1:8000/v1";
const MLX_BASE_URL: &str = "http://127.0.0.1:8080/v1";
const APPLE_ANE_BASE_URL: &str = "http://127.0.0.1:8081/v1";
const SGLANG_BASE_URL: &str = "http://127.0.0.1:30000/v1";
const TGI_BASE_URL: &str = "http://127.0.0.1:8082/v1";

fn normalize_runtime_provider_name(provider: &str) -> String {
    let normalized = provider.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "codex" => "openai-codex".to_string(),
        "claude" | "claude-code" => "anthropic".to_string(),
        "qwen-cli" | "qwen-portal" => "qwen-oauth".to_string(),
        "gemini-cli" | "gemini-oauth" => "google-gemini-cli".to_string(),
        "step" | "step-plan" => "stepfun".to_string(),
        "moonshot" | "kimi-coding" | "kimi-coding-cn" => "kimi".to_string(),
        "alibaba" | "alibaba-coding-plan" => "qwen".to_string(),
        "minimax-cn" => "minimax".to_string(),
        "kilo" | "kilo-code" | "kilo-gateway" => "kilocode".to_string(),
        "opencode" | "opencode-zen" | "zen" => "opencode-zen".to_string(),
        "go" => "opencode-go".to_string(),
        "ollama" => "ollama-local".to_string(),
        "llama.cpp" | "llamacpp" => "llama-cpp".to_string(),
        "ollvm" | "llvm" => "vllm".to_string(),
        "mlx-lm" | "apple-mlx" => "mlx".to_string(),
        "ane" | "apple-neural-engine" | "neural-engine" => "apple-ane".to_string(),
        "text-generation-inference" => "tgi".to_string(),
        _ => normalized,
    }
}

fn provider_default_base_url(provider: &str) -> Option<&'static str> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai-codex" | "codex" => Some(OPENAI_CODEX_BASE_URL),
        "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => Some(GOOGLE_GEMINI_CLI_BASE_URL),
        "gemini" | "google" => Some(GEMINI_BASE_URL),
        "qwen" | "alibaba" => Some(QWEN_BASE_URL),
        "alibaba-coding-plan" => Some(ALIBABA_CODING_PLAN_BASE_URL),
        "stepfun" | "step" | "step-plan" => Some(STEPFUN_BASE_URL),
        "ai-gateway" => Some(AI_GATEWAY_BASE_URL),
        "kimi-coding" => Some(KIMI_CODING_BASE_URL),
        "kimi-coding-cn" | "moonshot" | "kimi" => Some(KIMI_CODING_CN_BASE_URL),
        "minimax-cn" => Some(MINIMAX_CN_BASE_URL),
        "xai" => Some(XAI_BASE_URL),
        "nvidia" => Some(NVIDIA_BASE_URL),
        "opencode-go" => Some(OPENCODE_GO_BASE_URL),
        "opencode-zen" | "opencode" => Some(OPENCODE_ZEN_BASE_URL),
        "kilocode" | "kilo" => Some(KILOCODE_BASE_URL),
        "huggingface" => Some(HUGGINGFACE_BASE_URL),
        "xiaomi" => Some(XIAOMI_BASE_URL),
        "zai" => Some(ZAI_BASE_URL),
        "arcee" => Some(ARCEE_BASE_URL),
        "ollama-cloud" => Some(OLLAMA_CLOUD_BASE_URL),
        "ollama-local" | "ollama" => Some(OLLAMA_LOCAL_BASE_URL),
        "llama-cpp" | "llama.cpp" | "llamacpp" => Some(LLAMA_CPP_BASE_URL),
        "vllm" | "ollvm" | "llvm" => Some(VLLM_BASE_URL),
        "mlx" | "mlx-lm" | "apple-mlx" => Some(MLX_BASE_URL),
        "apple-ane" | "ane" | "apple-neural-engine" => Some(APPLE_ANE_BASE_URL),
        "sglang" => Some(SGLANG_BASE_URL),
        "tgi" | "text-generation-inference" => Some(TGI_BASE_URL),
        "deepseek" => Some(DEEPSEEK_BASE_URL),
        _ => None,
    }
}

fn resolve_provider_and_model(config: &GatewayConfig, model: &str) -> (String, String) {
    let trimmed = model.trim();
    if let Some((provider, model_name)) = trimmed.split_once(':') {
        return (provider.trim().to_string(), model_name.trim().to_string());
    }

    if let Some((provider, _)) = config.llm_providers.iter().find(|(_, cfg)| {
        cfg.model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .is_some_and(|m| m == trimmed)
    }) {
        return (provider.to_string(), trimmed.to_string());
    }

    if config.llm_providers.len() == 1 {
        if let Some((provider, _)) = config.llm_providers.iter().next() {
            return (provider.to_string(), trimmed.to_string());
        }
    }

    ("openai".to_string(), trimmed.to_string())
}

fn resolve_startup_model(config: &GatewayConfig, configured_model: &str) -> String {
    let raw = configured_model.trim();
    if raw.is_empty() {
        return "gpt-4o".to_string();
    }
    if raw.contains(':') {
        return raw.to_string();
    }

    // If config.model is a provider slug (e.g. "nous"), prefer that provider's
    // configured runtime model instead of sending the bare slug as a model id.
    if let Some((provider, provider_cfg)) = config
        .llm_providers
        .iter()
        .find(|(provider, _)| provider.eq_ignore_ascii_case(raw))
    {
        if let Some(runtime_model) = provider_cfg
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .filter(|m| !m.eq_ignore_ascii_case(provider))
        {
            if runtime_model.contains(':') {
                return runtime_model.to_string();
            }
            return format!("{provider}:{runtime_model}");
        }
    }

    raw.to_string()
}

fn sync_runtime_model_env(config: &GatewayConfig, provider_model: &str) {
    let model = provider_model.trim();
    if model.is_empty() {
        return;
    }
    let (provider, _) = resolve_provider_and_model(config, model);
    std::env::set_var("HERMES_MODEL", model);
    std::env::set_var("HERMES_INFERENCE_MODEL", model);
    std::env::set_var("HERMES_INFERENCE_PROVIDER", provider.as_str());
    if std::env::var_os("HERMES_TUI_PROVIDER").is_some() {
        std::env::set_var("HERMES_TUI_PROVIDER", provider.as_str());
    }
}

fn resolve_api_key_literal_or_env_ref(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(env_ref) = trimmed.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        return std::env::var(env_ref).ok().filter(|v| !v.trim().is_empty());
    }
    Some(trimmed.to_string())
}

fn default_mouse_enabled() -> bool {
    match std::env::var("HERMES_TUI_MOUSE") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        Err(_) => true,
    }
}

fn pet_settings_path() -> PathBuf {
    hermes_home_dir().join("pet.json")
}

fn parse_bool_env(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn default_pet_settings() -> PetSettings {
    let mut settings = PetSettings::default();
    if let Ok(raw) = std::env::var("HERMES_PET") {
        if let Some(enabled) = parse_bool_env(&raw) {
            settings.enabled = enabled;
        }
    }
    if let Ok(raw) = std::env::var("HERMES_PET_SPECIES") {
        settings.species = raw;
    }
    if let Ok(raw) = std::env::var("HERMES_PET_MOOD") {
        settings.mood = raw;
    }
    if let Ok(raw) = std::env::var("HERMES_PET_DOCK") {
        settings.dock = if raw.trim().eq_ignore_ascii_case("left") {
            PetDock::Left
        } else {
            PetDock::Right
        };
    }
    if let Ok(raw) = std::env::var("HERMES_PET_TICK_MS") {
        if let Ok(value) = raw.trim().parse::<u64>() {
            settings.tick_ms = value;
        }
    }
    settings.normalized()
}

fn load_pet_settings() -> PetSettings {
    let path = pet_settings_path();
    let from_file = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<PetSettings>(&raw).ok())
        .map(PetSettings::normalized);
    from_file.unwrap_or_else(default_pet_settings)
}

fn persist_pet_settings(settings: &PetSettings) -> Result<(), AgentError> {
    let path = pet_settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            AgentError::Io(format!(
                "Failed to create pet settings directory '{}': {}",
                parent.display(),
                e
            ))
        })?;
    }
    let body = serde_json::to_string_pretty(settings)
        .map_err(|e| AgentError::Config(format!("pet settings serialization failed: {e}")))?;
    std::fs::write(&path, format!("{body}\n")).map_err(|e| {
        AgentError::Io(format!(
            "Failed to persist pet settings '{}': {}",
            path.display(),
            e
        ))
    })
}

fn default_rtk_raw_mode() -> bool {
    match std::env::var("HERMES_RTK_RAW") {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        ),
        Err(_) => false,
    }
}

fn provider_base_url_from_env(provider: &str) -> Option<String> {
    let env_var = match provider.trim().to_ascii_lowercase().as_str() {
        "ollama-local" | "ollama" => "OLLAMA_BASE_URL",
        "llama-cpp" | "llama.cpp" | "llamacpp" => "LLAMA_CPP_BASE_URL",
        "vllm" | "ollvm" | "llvm" => "VLLM_BASE_URL",
        "mlx" | "mlx-lm" | "apple-mlx" => "MLX_BASE_URL",
        "apple-ane" | "ane" | "apple-neural-engine" => "APPLE_ANE_BASE_URL",
        "sglang" => "SGLANG_BASE_URL",
        "tgi" | "text-generation-inference" => "TGI_BASE_URL",
        _ => return None,
    };
    std::env::var(env_var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn provider_is_local_backend(provider: &str) -> bool {
    matches!(
        provider.trim().to_ascii_lowercase().as_str(),
        "ollama-local" | "llama-cpp" | "vllm" | "mlx" | "apple-ane" | "sglang" | "tgi"
    )
}

fn allow_no_api_key(provider_name: &str, runtime_provider: &str, base_url: Option<&str>) -> bool {
    provider_is_local_backend(runtime_provider)
        || provider_is_local_backend(provider_name)
        || base_url.is_some_and(url_is_local_or_private)
}

fn url_is_local_or_private(base_url: &str) -> bool {
    let trimmed = base_url.trim();
    let no_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let authority = no_scheme.split('/').next().unwrap_or(no_scheme).trim();
    let host = if authority.starts_with('[') {
        authority
            .find(']')
            .map(|idx| authority[1..idx].to_string())
            .unwrap_or_else(|| authority.trim_matches(&['[', ']'][..]).to_string())
    } else {
        authority
            .split(':')
            .next()
            .unwrap_or(authority)
            .trim()
            .to_string()
    }
    .to_ascii_lowercase();

    if host == "localhost" {
        return true;
    }

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
        };
    }
    false
}

/// Resolve API key / token for a named LLM provider from well-known environment variables.
pub fn provider_api_key_from_env(provider: &str) -> Option<String> {
    let provider = normalize_runtime_provider_name(provider);
    match provider.as_str() {
        "openai" => std::env::var("HERMES_OPENAI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "openai-codex" | "codex" => std::env::var("HERMES_OPENAI_CODEX_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "anthropic" | "claude" | "claude-code" => std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("ANTHROPIC_TOKEN").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok())
            .filter(|s| !s.trim().is_empty()),
        "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => {
            std::env::var("HERMES_GEMINI_OAUTH_API_KEY")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| std::env::var("GOOGLE_API_KEY").ok())
                .filter(|s| !s.trim().is_empty())
                .or_else(|| std::env::var("GEMINI_API_KEY").ok())
                .filter(|s| !s.trim().is_empty())
        }
        "openrouter" => std::env::var("OPENROUTER_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "qwen" => std::env::var("DASHSCOPE_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "qwen-oauth" => std::env::var("HERMES_QWEN_OAUTH_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("DASHSCOPE_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "kimi" | "moonshot" => std::env::var("KIMI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("KIMI_CODING_API_KEY").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("MOONSHOT_API_KEY").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("KIMI_CN_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "minimax" => std::env::var("MINIMAX_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("MINIMAX_CN_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "stepfun" => std::env::var("HERMES_STEPFUN_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("STEPFUN_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "nous" => std::env::var("NOUS_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "copilot" => std::env::var("GITHUB_COPILOT_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "ai-gateway" => std::env::var("AI_GATEWAY_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "arcee" => std::env::var("ARCEEAI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("ARCEE_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "deepseek" => std::env::var("DEEPSEEK_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "huggingface" => std::env::var("HF_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "kilocode" => std::env::var("KILOCODE_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "nvidia" => std::env::var("NVIDIA_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "ollama-cloud" => std::env::var("OLLAMA_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "ollama-local" => std::env::var("OLLAMA_LOCAL_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("OLLAMA_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "llama-cpp" => std::env::var("LLAMA_CPP_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "vllm" => std::env::var("VLLM_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "mlx" => std::env::var("MLX_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "apple-ane" => std::env::var("APPLE_ANE_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "sglang" => std::env::var("SGLANG_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "tgi" => std::env::var("TGI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("HUGGINGFACE_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "opencode-go" => std::env::var("OPENCODE_GO_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "opencode-zen" => std::env::var("OPENCODE_ZEN_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "xai" => std::env::var("XAI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "xiaomi" => std::env::var("XIAOMI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "zai" => std::env::var("GLM_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("ZAI_API_KEY").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("Z_AI_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        _ => None,
    }
}

pub fn build_provider(config: &GatewayConfig, model: &str) -> Arc<dyn LlmProvider> {
    let (provider_name, model_name) = resolve_provider_and_model(config, model);
    let runtime_provider = normalize_runtime_provider_name(provider_name.as_str());

    let provider_config = config
        .llm_providers
        .get(provider_name.as_str())
        .or_else(|| config.llm_providers.get(runtime_provider.as_str()));
    let provider_config = provider_config.or_else(|| {
        config.llm_providers.iter().find_map(|(name, cfg)| {
            if name.eq_ignore_ascii_case(provider_name.as_str())
                || name.eq_ignore_ascii_case(runtime_provider.as_str())
            {
                Some(cfg)
            } else {
                None
            }
        })
    });

    let default_base_url = provider_default_base_url(provider_name.as_str())
        .or_else(|| provider_default_base_url(runtime_provider.as_str()));
    let base_url = provider_config
        .and_then(|c| c.base_url.clone())
        .or_else(|| provider_base_url_from_env(provider_name.as_str()))
        .or_else(|| provider_base_url_from_env(runtime_provider.as_str()))
        .or_else(|| default_base_url.map(ToString::to_string));

    let api_key = provider_config
        .and_then(|c| c.api_key.as_deref())
        .and_then(resolve_api_key_literal_or_env_ref)
        .or_else(|| {
            provider_config
                .and_then(|c| c.api_key_env.as_deref())
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .and_then(|name| std::env::var(name).ok())
                .filter(|v| !v.trim().is_empty())
        })
        .or_else(|| provider_api_key_from_env(provider_name.as_str()))
        .or_else(|| provider_api_key_from_env(runtime_provider.as_str()));

    let local_no_key_ok = allow_no_api_key(
        provider_name.as_str(),
        runtime_provider.as_str(),
        base_url.as_deref(),
    );

    let api_key = match api_key {
        Some(k) => k,
        None if local_no_key_ok => "local-no-key".to_string(),
        None => {
            tracing::warn!(
                "No API key for provider '{}'(runtime '{}'); using NoBackendProvider",
                provider_name,
                runtime_provider
            );
            return Arc::new(NoBackendProvider {
                model: model.to_string(),
            });
        }
    };

    match runtime_provider.as_str() {
        "openai" => {
            let mut p = OpenAiProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "openai-codex" | "codex" => {
            let mut p = OpenAiProvider::new(&api_key).with_model(model_name.as_str());
            p = p.with_base_url(base_url.unwrap_or_else(|| OPENAI_CODEX_BASE_URL.to_string()));
            Arc::new(p)
        }
        "anthropic" => {
            let mut p = AnthropicProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "openrouter" => {
            let p = OpenRouterProvider::new(&api_key).with_model(model_name.as_str());
            Arc::new(p)
        }
        "qwen" | "qwen-oauth" => {
            let mut p = QwenProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "kimi" | "moonshot" => {
            let mut p = KimiProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "minimax" => {
            let mut p = MiniMaxProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "stepfun" => {
            let url = base_url.unwrap_or_else(|| STEPFUN_BASE_URL.to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name.as_str()))
        }
        "nous" => {
            let mut p = NousProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "copilot" => {
            let p = CopilotProvider::new(
                base_url.unwrap_or_else(|| "https://api.github.com/copilot".to_string()),
                &api_key,
            )
            .with_model(model_name.as_str());
            Arc::new(p)
        }
        "ollama-local" | "llama-cpp" | "vllm" | "mlx" | "apple-ane" | "sglang" | "tgi" => {
            let url = base_url.unwrap_or_else(|| "http://127.0.0.1:11434/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name.as_str()))
        }
        _ => {
            let url = base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name.as_str()))
        }
    }
}

// ---------------------------------------------------------------------------
// NoBackendProvider — explicit fallback when no API key is configured
// ---------------------------------------------------------------------------

struct NoBackendProvider {
    model: String,
}

#[async_trait::async_trait]
impl LlmProvider for NoBackendProvider {
    async fn chat_completion(
        &self,
        _messages: &[hermes_core::Message],
        _tools: &[hermes_core::ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> Result<hermes_core::LlmResponse, AgentError> {
        Err(AgentError::LlmApi(format!(
            "NoBackendProvider: no LLM backend configured for model '{}'. \
             Configure an API key and provider in the config file.",
            self.model
        )))
    }

    fn chat_completion_stream(
        &self,
        _messages: &[hermes_core::Message],
        _tools: &[hermes_core::ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> futures::stream::BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>> {
        futures::stream::once(async move {
            Err(AgentError::LlmApi(
                "NoBackendProvider: no LLM backend configured for streaming.".to_string(),
            ))
        })
        .boxed()
    }
}
