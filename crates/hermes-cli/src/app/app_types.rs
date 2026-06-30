
const SESSION_SNAPSHOT_MAX_FILES_DEFAULT: usize = 1500;
const SESSION_SNAPSHOT_MAX_TOTAL_BYTES_DEFAULT: u64 = 1536 * 1024 * 1024;
const SESSION_SNAPSHOT_MIN_FREE_BYTES_DEFAULT: u64 = 128 * 1024 * 1024;
const QUORUM_HINT_PREFIX: &str = "[QUORUM_MODE] ";
const QUORUM_MAX_VOTER_OUTPUT_CHARS: usize = 120_000;
const QUORUM_DEFAULT_VOTER_PASSES: usize = 6;
const QUORUM_AGENT_CONTRACT_DEFAULT_PATH: &str =
    "/Users/sheawinkler/Documents/Projects/hermes-agent-ultra/docs/QUORUM_AGENTS.md";
const MOA_DEFAULT_REFERENCE_MODELS: &[&str] = &[
    "openai-codex:gpt-5.5",
    "openrouter:deepseek/deepseek-v4-pro",
];
const MOA_DEFAULT_AGGREGATOR_MODEL: &str = "openrouter:anthropic/claude-opus-4.7";
const COMPOSER_DRAFTS_FILE: &str = "composer-drafts.json";
const MAX_COMPOSER_DRAFTS: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MoaRuntimePreset {
    name: &'static str,
    reference_models: &'static [&'static str],
    aggregator_model: &'static str,
    voters: usize,
    mode: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ComposerDraftStore {
    #[serde(default)]
    version: u8,
    #[serde(default)]
    drafts: Vec<ComposerDraftRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ComposerDraftRecord {
    session_id: String,
    text: String,
    updated_at: String,
}

#[derive(Debug, Clone)]
struct SessionSnapshotEntry {
    path: PathBuf,
    modified: SystemTime,
    size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
struct QuorumVoterOutcome {
    model: String,
    status: String,
    duration_ms: u64,
    total_turns: u32,
    tool_errors: usize,
    output: String,
    error: Option<String>,
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

    /// Runtime cron scheduler used by slash commands and the cronjob tool.
    pub cron_scheduler: Arc<CronScheduler>,

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

    /// Currently active model identifier (for example, "dynamic" or "provider:model").
    pub current_model: String,

    /// Actual provider-reported usage for the most recently applied agent run.
    pub last_usage: Option<UsageStats>,

    /// Aggregated provider-reported usage for the current interactive session.
    pub session_usage: Option<UsageStats>,

    /// Aggregated provider-reported or estimated cost for the current session.
    pub session_cost_usd: f64,

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
    /// User text staged back into the composer by commands such as `/undo`.
    pending_input_prefill: Option<String>,
    /// One-shot user prompt queued by slash commands such as `/prompt`.
    pending_agent_seed: Option<String>,
    /// System notes injected once before the next submitted user message.
    pending_system_notes: Vec<String>,
    /// One-shot quorum arm state set by `/quorum run`.
    pub quorum_armed_once: bool,
    /// Animated companion pet settings.
    pub pet_settings: PetSettings,

    /// Test-only hook for proving model-switch rollback on rebuild failure.
    #[cfg(test)]
    fail_model_rebuild_for: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentToolSnapshotRefresh {
    pub before_count: usize,
    pub after_count: usize,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

impl AgentToolSnapshotRefresh {
    pub fn changed(&self) -> bool {
        !self.added.is_empty() || !self.removed.is_empty()
    }
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("state_root", &self.state_root)
            .field("session_id", &self.session_id)
            .field("running", &self.running)
            .field("current_model", &self.current_model)
            .field("last_usage", &self.last_usage)
            .field("session_usage", &self.session_usage)
            .field("session_cost_usd", &self.session_cost_usd)
            .field("current_personality", &self.current_personality)
            .field("history_index", &self.history_index)
            .field("mouse_enabled", &self.mouse_enabled)
            .field("pending_theme", &self.pending_theme)
            .field("pending_image_hint", &self.pending_image_hint)
            .field("session_objective", &self.session_objective)
            .field("pending_input_prefill", &self.pending_input_prefill)
            .field("pending_agent_seed", &self.pending_agent_seed)
            .field("pending_system_notes", &self.pending_system_notes)
            .field("quorum_armed_once", &self.quorum_armed_once)
            .field("pet_settings", &self.pet_settings)
            .finish_non_exhaustive()
    }
}

impl Clone for App {
    fn clone(&self) -> Self {
        Self {
            state_root: self.state_root.clone(),
            config: self.config.clone(),
            agent: self.agent.clone(),
            tool_registry: self.tool_registry.clone(),
            cron_scheduler: self.cron_scheduler.clone(),
            tool_schemas: self.tool_schemas.clone(),
            messages: self.messages.clone(),
            ui_messages: self.ui_messages.clone(),
            session_id: self.session_id.clone(),
            running: self.running,
            current_model: self.current_model.clone(),
            last_usage: self.last_usage.clone(),
            session_usage: self.session_usage.clone(),
            session_cost_usd: self.session_cost_usd,
            current_personality: self.current_personality.clone(),
            input_history: self.input_history.clone(),
            history_index: self.history_index,
            interrupt_controller: self.interrupt_controller.clone(),
            stream_handle: self.stream_handle.clone(),
            stream_handle_shared: self.stream_handle_shared.clone(),
            mouse_enabled: self.mouse_enabled,
            pending_theme: self.pending_theme.clone(),
            pending_image_hint: self.pending_image_hint.clone(),
            session_objective: self.session_objective.clone(),
            pending_input_prefill: self.pending_input_prefill.clone(),
            pending_agent_seed: self.pending_agent_seed.clone(),
            pending_system_notes: self.pending_system_notes.clone(),
            quorum_armed_once: self.quorum_armed_once,
            pet_settings: self.pet_settings.clone(),
            #[cfg(test)]
            fail_model_rebuild_for: self.fail_model_rebuild_for.clone(),
        }
    }
}

fn merge_usage_stats(existing: Option<UsageStats>, new: &UsageStats) -> UsageStats {
    match existing {
        Some(prev) => UsageStats {
            prompt_tokens: prev.prompt_tokens + new.prompt_tokens,
            completion_tokens: prev.completion_tokens + new.completion_tokens,
            total_tokens: prev.total_tokens + new.total_tokens,
            estimated_cost: match (prev.estimated_cost, new.estimated_cost) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            },
        },
        None => new.clone(),
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
