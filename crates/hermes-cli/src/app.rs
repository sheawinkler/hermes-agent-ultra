//! Application state management for the interactive CLI.
//!
//! The `App` struct owns the configuration, agent loop, tool registry,
//! and conversation message history. It coordinates input handling,
//! slash commands, and session management.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use hermes_agent::agent_loop::ToolRegistry as AgentToolRegistry;
use hermes_agent::bedrock::{
    bedrock_runtime_base_url, resolve_bedrock_region, BedrockProvider, BEDROCK_AUTH_MARKER,
};
use hermes_agent::plugins::HookType;
use hermes_agent::provider::{
    openai_codex_provider_with_timeout, AnthropicProvider, GenericProvider, OpenAiProvider,
    OpenRouterProvider, OPENAI_CODEX_BASE_URL,
};
use hermes_agent::provider_profiles;
use hermes_agent::providers_extra::{
    CopilotProvider, KimiProvider, MiniMaxProvider, NousProvider, QwenProvider,
};
use hermes_agent::smart_model_routing::ApiMode;
use hermes_agent::sub_agent_orchestrator::SubAgentOrchestrator;
use hermes_agent::{
    AgentCallbacks, AgentConfig, AgentLoop, InterruptController, SessionPersistence,
};
use hermes_config::{
    hermes_home as hermes_home_dir, load_config, normalize_service_tier, state_dir, GatewayConfig,
};
use hermes_core::ToolSchema;
use hermes_core::{AgentError, LlmProvider, UsageStats};
use hermes_cron::{CronRunner, CronScheduler, FileJobPersistence};
use hermes_skills::{FileSkillStore, SkillManager};
use hermes_tools::ToolRegistry;

use crate::alpha_runtime::{
    canonical_objective_behavior_mode, load_objective_contract, load_quorum_policy,
    objective_lifecycle_is_active, ObjectiveContract, QuorumPolicy,
};
use crate::auth::{
    login_nous_device_code, resolve_gemini_oauth_runtime_credentials,
    resolve_nous_runtime_credentials, resolve_qwen_runtime_credentials, save_nous_auth_state,
    NousDeviceCodeOptions, NousRuntimeCredentials, DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
    DEFAULT_NOUS_INFERENCE_URL, NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
    QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
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
const QUORUM_HINT_PREFIX: &str = "[QUORUM_MODE] ";
const QUORUM_MAX_VOTER_OUTPUT_CHARS: usize = 120_000;
const QUORUM_DEFAULT_VOTER_PASSES: usize = 6;
const RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS: usize = 1_600;
const QUORUM_AGENT_CONTRACT_DEFAULT_PATH: &str =
    "/Users/sheawinkler/Documents/Projects/hermes-agent-ultra/docs/QUORUM_AGENTS.md";

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
    /// One-shot quorum arm state set by `/quorum run`.
    pub quorum_armed_once: bool,
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
            quorum_armed_once: self.quorum_armed_once,
            pet_settings: self.pet_settings.clone(),
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

impl App {
    const SESSION_OBJECTIVE_PREFIX: &'static str = "[SESSION_OBJECTIVE] ";
    const RUNTIME_REFORMULATION_PREFIX: &'static str = "[HERMES_RUNTIME_REFORMULATION] ";

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

    fn bool_env(key: &str) -> Option<bool> {
        let raw = std::env::var(key).ok()?;
        let normalized = raw.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        }
    }

    fn is_unbounded_token(raw: &str) -> bool {
        matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "off" | "unlimited" | "infinite" | "max"
        )
    }

    fn auth_refresh_retry_limit() -> usize {
        std::env::var("HERMES_AUTH_REFRESH_MAX_RETRIES")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(3)
    }

    fn quorum_voter_retry_limit() -> usize {
        if let Ok(raw) = std::env::var("HERMES_QUORUM_VOTER_MAX_RETRIES") {
            if Self::is_unbounded_token(&raw) {
                return 16;
            }
            if let Some(parsed) = raw.trim().parse::<usize>().ok().filter(|v| *v > 0) {
                return parsed.max(2);
            }
        }
        Self::auth_refresh_retry_limit().max(6)
    }

    fn transient_retry_limit() -> usize {
        std::env::var("HERMES_TRANSIENT_MAX_RETRIES")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(2)
    }

    fn is_transient_retryable_error(err: &AgentError) -> bool {
        let message = match err {
            AgentError::LlmApi(msg)
            | AgentError::Config(msg)
            | AgentError::ToolExecution(msg)
            | AgentError::Gateway(msg)
            | AgentError::AuthFailed(msg)
            | AgentError::Io(msg) => msg.to_ascii_lowercase(),
            _ => return false,
        };
        message.contains("timed out")
            || message.contains("timeout")
            || message.contains("connection reset")
            || message.contains("connection refused")
            || message.contains("temporarily unavailable")
            || message.contains("try again")
            || message.contains("rate limit")
            || message.contains("429")
            || message.contains("502")
            || message.contains("503")
            || message.contains("504")
            || message.contains("provider rejected")
    }

    fn objective_execution_enforcer_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_OBJECTIVE_EXECUTION_ENFORCER")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn objective_continuation_retry_limit() -> usize {
        std::env::var("HERMES_OBJECTIVE_CONTINUATION_MAX_RETRIES")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(1)
    }

    fn load_active_objective_contract() -> Option<ObjectiveContract> {
        load_objective_contract()
            .ok()
            .flatten()
            .filter(|contract| objective_lifecycle_is_active(&contract.lifecycle_status))
    }

    fn looks_like_status_only_output(text: &str) -> bool {
        let lowered = text.trim().to_ascii_lowercase();
        if lowered.is_empty() {
            return true;
        }

        let has_future_language = [
            "i will",
            "i'll",
            "next i",
            "going to",
            "plan:",
            "i can",
            "we should",
            "i would",
            "i'll proceed",
            "i will proceed",
            "proceeding with",
        ]
        .iter()
        .any(|needle| lowered.contains(needle));
        let has_execution_evidence = [
            "path=",
            "file=",
            "exit code",
            "result:",
            "tested",
            "verified",
            "implemented",
            "changed",
            "patched",
            "command:",
            "run_id",
            "metric",
        ]
        .iter()
        .any(|needle| lowered.contains(needle));

        let has_weakness_markers = [
            "let me know",
            "if you'd like",
            "i can do that next",
            "awaiting",
            "need your confirmation",
        ]
        .iter()
        .any(|needle| lowered.contains(needle));

        (has_future_language && !has_execution_evidence) || has_weakness_markers
    }

    fn should_force_objective_continuation(
        &self,
        result: &hermes_core::AgentResult,
        baseline_len: usize,
    ) -> Option<String> {
        if !Self::objective_execution_enforcer_enabled() {
            return None;
        }
        let contract = Self::load_active_objective_contract()?;
        let behavior_mode = canonical_objective_behavior_mode(&contract.behavior_mode);
        if !matches!(behavior_mode.as_str(), "autonomous" | "mission") {
            return None;
        }

        let new_messages = if result.messages.len() > baseline_len {
            &result.messages[baseline_len..]
        } else {
            &result.messages[..]
        };

        let had_tool_activity = new_messages.iter().any(|message| {
            message.role == hermes_core::MessageRole::Tool
                || (message.role == hermes_core::MessageRole::Assistant
                    && message
                        .tool_calls
                        .as_ref()
                        .map(|calls| !calls.is_empty())
                        .unwrap_or(false))
        });
        if had_tool_activity {
            return None;
        }

        let output = Self::extract_last_assistant_output(new_messages);
        if output.trim().is_empty() {
            return Some(
                "assistant returned empty output while objective remained active".to_string(),
            );
        }
        if Self::looks_like_status_only_output(&output) {
            return Some(
                "assistant output was status/plan-heavy without concrete executed action"
                    .to_string(),
            );
        }
        None
    }

    fn objective_continuation_system_prompt(reason: &str) -> String {
        format!(
            "[OBJECTIVE_CONTINUATION_ENFORCER]\n\
             reason={}\n\
             Continue objective execution immediately.\n\
             Requirements for this pass:\n\
             1) execute at least one concrete action (tool or code operation),\n\
             2) include verifiable evidence from that action,\n\
             3) report objective delta in measurable terms,\n\
             4) end with the next highest-value action and continue momentum.\n\
             Do not return a plan-only or defer-only response.",
            reason
        )
    }

    fn should_force_preflight_auth_refresh(provider: &str) -> bool {
        if let Some(explicit) = Self::bool_env("HERMES_FORCE_RUNTIME_AUTH_REFRESH") {
            return explicit;
        }
        matches!(
            provider,
            "nous" | "qwen-oauth" | "google-gemini-cli" | "gemini-cli" | "gemini-oauth"
        )
    }

    fn quorum_force_refresh_each_voter() -> bool {
        Self::bool_env("HERMES_QUORUM_FORCE_REFRESH_EACH_VOTER").unwrap_or(false)
    }

    fn quorum_toolless_provider_fallback_enabled() -> bool {
        !matches!(
            Self::bool_env("HERMES_QUORUM_TOOLLESS_PROVIDER_FALLBACK"),
            Some(false)
        )
    }

    fn quorum_voter_tools_enabled() -> bool {
        !matches!(Self::bool_env("HERMES_QUORUM_VOTER_TOOLS"), Some(false))
    }

    fn quorum_synthesis_tools_enabled() -> bool {
        !matches!(Self::bool_env("HERMES_QUORUM_SYNTHESIS_TOOLS"), Some(false))
    }

    fn nous_refresh_contention_error(err: &AgentError) -> bool {
        let text = err.to_string().to_ascii_lowercase();
        text.contains("slow_down")
            || text.contains("too many requests")
            || text.contains("refresh already in progress")
            || text.contains("429")
    }

    fn apply_nous_runtime_credentials(creds: &NousRuntimeCredentials) -> bool {
        let mut changed = false;
        changed |= Self::set_env_if_changed("NOUS_API_KEY", &creds.api_key);
        if !creds.base_url.trim().is_empty() {
            changed |= Self::set_env_if_changed("NOUS_INFERENCE_BASE_URL", &creds.base_url);
        }
        changed
    }

    fn contextlattice_ui_status_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_CONTEXTLATTICE_UI_STATUS")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn contextlattice_orchestrator_url() -> String {
        std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
            .ok()
            .or_else(|| std::env::var("MEMMCP_ORCHESTRATOR_URL").ok())
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:8075".to_string())
    }

    fn contextlattice_ping_timeout_secs() -> u64 {
        std::env::var("HERMES_CONTEXTLATTICE_PING_TIMEOUT_SECONDS")
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(12)
            .clamp(1, 120)
    }

    async fn emit_contextlattice_connectivity_status(&self) {
        if !Self::contextlattice_ui_status_enabled() {
            return;
        }
        let base = Self::contextlattice_orchestrator_url();
        let url = format!("{}/status", base);
        let topic = std::env::var("CONTEXTLATTICE_TOPIC_PATH")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "runbooks/hermes".to_string());
        Self::emit_lifecycle_event(
            &self.stream_handle_shared,
            format!("contextlattice preflight ping {} (topic={})", base, topic),
        );
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(Self::contextlattice_ping_timeout_secs()))
            .build()
        {
            Ok(c) => c,
            Err(err) => {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    format!("contextlattice client init failed: {}", err),
                );
                return;
            }
        };
        match client.get(&url).send().await {
            Ok(resp) => {
                let status_code = resp.status();
                if status_code.is_success() {
                    let parsed = resp.json::<serde_json::Value>().await.ok();
                    let service = parsed
                        .as_ref()
                        .and_then(|v| v.get("service").and_then(|s| s.as_str()))
                        .unwrap_or("unknown");
                    let ok_flag = parsed
                        .as_ref()
                        .and_then(|v| v.get("ok").and_then(|s| s.as_bool()))
                        .unwrap_or(true);
                    let detail = if ok_flag { "connected" } else { "degraded" };
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!(
                            "contextlattice {} (service={} status={} endpoint={})",
                            detail, service, status_code, base
                        ),
                    );
                    Self::emit_phase_event(
                        &self.stream_handle_shared,
                        "context",
                        if ok_flag {
                            "contextlattice connected"
                        } else {
                            "contextlattice degraded"
                        },
                        12,
                    );
                } else {
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!(
                            "contextlattice status endpoint returned {} ({})",
                            status_code, url
                        ),
                    );
                }
            }
            Err(err) => {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    format!("contextlattice preflight failed: {} ({})", err, url),
                );
            }
        }
    }

    fn auto_nous_reauth_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_AUTO_NOUS_REAUTH")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn auth_error_requires_nous_login(err: &AgentError) -> bool {
        let text = err.to_string().to_ascii_lowercase();
        text.contains("not logged into nous portal")
            || text.contains("run `hermes portal`")
            || text.contains("re-run `hermes auth nous`")
            || text.contains("stored nous auth state is invalid")
            || text.contains("missing refresh token")
            || text.contains("invalid nous refresh response")
    }

    async fn attempt_interactive_nous_login(&mut self, reason: &str) -> bool {
        if !Self::auto_nous_reauth_enabled() {
            return false;
        }
        Self::emit_lifecycle_event(
            &self.stream_handle_shared,
            format!("Nous OAuth re-auth required ({reason}); launching portal login flow"),
        );
        match login_nous_device_code(NousDeviceCodeOptions::default()).await {
            Ok(state) => match save_nous_auth_state(&state) {
                Ok(path) => {
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!("Nous OAuth state refreshed: {}", path.display()),
                    );
                    true
                }
                Err(err) => {
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!("Nous OAuth state save failed: {}", err),
                    );
                    false
                }
            },
            Err(err) => {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    format!("Nous OAuth interactive login failed: {}", err),
                );
                false
            }
        }
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
                    rotated |= Self::apply_nous_runtime_credentials(&creds);
                    if rotated {
                        note = Some("refreshed Nous runtime credential".to_string());
                    }
                }
                Err(e) => {
                    if force_refresh && Self::nous_refresh_contention_error(&e) {
                        match resolve_nous_runtime_credentials(
                            false,
                            true,
                            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                            DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
                        )
                        .await
                        {
                            Ok(creds) => {
                                rotated |= Self::apply_nous_runtime_credentials(&creds);
                                note = Some(
                                    "Nous refresh busy; reused cached runtime credential"
                                        .to_string(),
                                );
                            }
                            Err(cache_err) => {
                                Self::emit_lifecycle_event(
                                    &self.stream_handle_shared,
                                    format!(
                                        "warning: Nous cached credential hydration failed after refresh contention ({cache_err})"
                                    ),
                                );
                            }
                        }
                    }
                    if Self::auth_error_requires_nous_login(&e)
                        && self
                            .attempt_interactive_nous_login("credential missing or invalid")
                            .await
                    {
                        match resolve_nous_runtime_credentials(
                            true,
                            true,
                            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                            DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
                        )
                        .await
                        {
                            Ok(creds) => {
                                rotated |= Self::apply_nous_runtime_credentials(&creds);
                                if rotated {
                                    note = Some("refreshed Nous runtime credential".to_string());
                                }
                            }
                            Err(err) => {
                                Self::emit_lifecycle_event(
                                    &self.stream_handle_shared,
                                    format!("warning: Nous credential refresh skipped ({err})"),
                                );
                            }
                        }
                    } else {
                        if !rotated && note.is_none() {
                            Self::emit_lifecycle_event(
                                &self.stream_handle_shared,
                                format!("warning: Nous credential refresh skipped ({e})"),
                            );
                        }
                    }
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
        if App::oneshot_lifecycle_stdout_enabled(shared) {
            println!("[lifecycle] {}", preview);
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
        if App::oneshot_lifecycle_stdout_enabled(shared) {
            println!("[phase {:>3}%] {}: {}", progress_pct.min(100), phase, label);
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

    fn oneshot_lifecycle_stdout_enabled(shared: &Arc<StdMutex<Option<StreamHandle>>>) -> bool {
        let stream_attached = shared
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|_| ()))
            .is_some();
        if stream_attached {
            return false;
        }
        matches!(
            std::env::var("HERMES_ONESHOT_LIFECYCLE_STDOUT")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "1" | "true" | "yes" | "on")
        )
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

    fn runtime_prompt_reformulation_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_RUNTIME_PROMPT_REFORMULATION")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn runtime_contradiction_self_check_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_RUNTIME_CONTRADICTION_SELF_CHECK")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn runtime_reformulation_prompt_preview_chars() -> usize {
        std::env::var("HERMES_RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS)
    }

    fn current_tool_profile_mode() -> String {
        std::env::var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "balanced".to_string())
    }

    fn build_runtime_reformulation_message(&self, latest_user_prompt: &str) -> Option<String> {
        if !Self::runtime_prompt_reformulation_enabled() {
            return None;
        }
        let prompt = latest_user_prompt.trim();
        if prompt.is_empty() {
            return None;
        }
        let tool_profile_mode = Self::current_tool_profile_mode();
        let contradiction_check = Self::runtime_contradiction_self_check_enabled();
        let context_topic = std::env::var("CONTEXTLATTICE_TOPIC_PATH")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "runbooks/hermes".to_string());

        let objective_contract = Self::load_active_objective_contract();
        let objective_line = objective_contract
            .as_ref()
            .map(|contract| {
                format!(
                    "objective(active): {} | behavior={} | text={}",
                    contract.id,
                    canonical_objective_behavior_mode(&contract.behavior_mode),
                    Self::preview_for_status(&contract.objective_text, 220)
                )
            })
            .unwrap_or_else(|| "objective(active): none".to_string());
        let objective_directives = objective_contract
            .as_ref()
            .map(|contract| {
                contract
                    .behavior_directives
                    .iter()
                    .take(6)
                    .map(|line| format!("- {}", line.trim()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "- (none)".to_string());
        let objective_success = objective_contract
            .as_ref()
            .map(|contract| {
                contract
                    .success_criteria
                    .iter()
                    .take(5)
                    .map(|line| format!("- {}", line.trim()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "- (none)".to_string());

        let contradiction_line = if contradiction_check {
            "before final response: self-audit contradictions across tool outputs, runtime facts, and claims; unresolved items must be marked UNPROVEN/CONTRADICTORY."
        } else {
            "before final response: consistency self-audit optional (disabled by runtime toggle)."
        };

        let mut out = String::new();
        out.push_str(Self::RUNTIME_REFORMULATION_PREFIX);
        out.push_str(
            "\nRuntime execution reformulation (internal):\n\
             1) apply anti-scheming evidence-first discipline\n\
             2) pull ContextLattice context first when relevant\n\
             3) route tool usage intentionally and avoid repetitive low-signal loops\n\
             4) match requested output shape exactly (count/format), with no template placeholders or duplicate list items\n\
             5) for open-ended missions, execute at least one concrete action before returning status text\n\
             6) maintain iterative objective momentum: gather evidence, test, refine, then continue with next high-value action\n",
        );
        out.push_str(&format!(
            "tool-profile(mode): {}\ncontextlattice(topic): {}\n{}\n",
            tool_profile_mode, context_topic, objective_line
        ));
        out.push_str("objective behavior directives:\n");
        out.push_str(&objective_directives);
        out.push('\n');
        out.push_str("objective success criteria:\n");
        out.push_str(&objective_success);
        out.push('\n');
        out.push_str(
            "objective loop protocol:\n\
             - baseline: state current objective KPI and latest known value\n\
             - execute: perform concrete highest-leverage action now\n\
             - verify: present measurable delta or explicit blocked evidence\n\
             - continue: state next action with no soft deferral\n",
        );
        out.push_str(contradiction_line);
        out.push_str("\nuser-request(routing-preview):\n");
        let preview_cap = Self::runtime_reformulation_prompt_preview_chars();
        let prompt_preview = Self::preview_for_status(prompt, preview_cap);
        out.push_str(&prompt_preview);
        if prompt.chars().count() > preview_cap {
            out.push_str(
                "\n[preview truncated; the full user request remains available as the next user message]",
            );
        } else {
            out.push_str("\n[full user request remains available as the next user message]");
        }
        Some(out)
    }

    fn build_inference_messages(&self) -> (Vec<hermes_core::Message>, bool) {
        let mut messages = self.messages.clone();
        let Some(last_user_idx) = messages
            .iter()
            .rposition(|m| m.role == hermes_core::MessageRole::User)
        else {
            return (messages, false);
        };
        let user_prompt = messages[last_user_idx]
            .content
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_string();
        let Some(reformulation) = self.build_runtime_reformulation_message(&user_prompt) else {
            return (messages, false);
        };
        messages.insert(last_user_idx, hermes_core::Message::system(reformulation));
        (messages, true)
    }

    fn compose_quorum_messages(
        control_sections: Vec<String>,
        base_messages: Vec<hermes_core::Message>,
        trailing_user_context: Option<String>,
    ) -> Vec<hermes_core::Message> {
        let control_context = control_sections
            .into_iter()
            .map(|section| section.trim().to_string())
            .filter(|section| !section.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut merged_system_sections: Vec<String> = Vec::new();
        let mut non_system_messages: Vec<hermes_core::Message> = Vec::new();

        for message in base_messages {
            if message.role == hermes_core::MessageRole::System {
                if let Some(content) = message.content.as_deref().map(str::trim) {
                    if !content.is_empty() {
                        merged_system_sections.push(content.to_string());
                    }
                }
            } else {
                non_system_messages.push(message);
            }
        }

        let mut messages = Vec::new();
        if !merged_system_sections.is_empty() {
            messages.push(hermes_core::Message::system(
                merged_system_sections.join("\n\n"),
            ));
        }
        if !control_context.is_empty() {
            messages.push(hermes_core::Message::user(format!(
                "[QUORUM_CONTROL]\n{}",
                control_context
            )));
        }
        messages.extend(non_system_messages);
        if let Some(context) = trailing_user_context
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            messages.push(hermes_core::Message::user(context));
        }
        messages
    }

    fn quorum_mode_armed_for_turn(&self) -> Option<QuorumPolicy> {
        let policy = match load_quorum_policy() {
            Ok(policy) => policy,
            Err(err) => {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    format!("quorum policy load failed: {}", err),
                );
                return None;
            }
        };
        if !policy.enabled {
            if self.quorum_armed_once {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    "quorum run requested but policy is disabled; run `/quorum on` first",
                );
            }
            return None;
        }
        let has_hint = self.messages.iter().any(|message| {
            message.role == hermes_core::MessageRole::System
                && message
                    .content
                    .as_deref()
                    .unwrap_or_default()
                    .starts_with(QUORUM_HINT_PREFIX)
        });
        let has_user_turn = self
            .messages
            .iter()
            .any(|m| m.role == hermes_core::MessageRole::User);
        if !has_user_turn {
            if self.quorum_armed_once || has_hint {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    "quorum armed but no user turn present yet; waiting for next user prompt",
                );
            }
            return None;
        }
        if !(self.quorum_armed_once || has_hint) {
            let auto_arm = std::env::var("HERMES_QUORUM_AUTO_ARM")
                .ok()
                .map(|raw| {
                    matches!(
                        raw.trim().to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on" | "auto"
                    )
                })
                .unwrap_or(false);
            if auto_arm {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    "quorum auto-arm enabled via HERMES_QUORUM_AUTO_ARM=1",
                );
                return Some(policy);
            }
            return None;
        }
        Some(policy)
    }

    fn clear_quorum_system_hints_inplace(&mut self) {
        self.messages.retain(|message| {
            if message.role != hermes_core::MessageRole::System {
                return true;
            }
            !message
                .content
                .as_deref()
                .unwrap_or_default()
                .starts_with(QUORUM_HINT_PREFIX)
        });
    }

    fn collect_quorum_models(policy: &QuorumPolicy, current_model: &str) -> Vec<String> {
        let mut models: Vec<String> = Vec::new();
        let push_unique = |target: &mut Vec<String>, raw: &str| {
            let candidate = raw.trim();
            if candidate.is_empty() {
                return;
            }
            if target.iter().any(|existing| existing == candidate) {
                return;
            }
            target.push(candidate.to_string());
        };
        for model in &policy.models {
            push_unique(&mut models, model);
        }
        if models.is_empty() {
            push_unique(&mut models, current_model);
        }
        let max_voters = policy.voters.clamp(2, 8);
        if models.len() < max_voters {
            push_unique(&mut models, current_model);
        }
        if models.len() > max_voters {
            models.truncate(max_voters);
        }
        models
    }

    fn quorum_voter_passes() -> usize {
        if let Ok(raw) = std::env::var("HERMES_QUORUM_VOTER_PASSES") {
            if Self::is_unbounded_token(&raw) {
                return 16;
            }
            if let Some(parsed) = raw.trim().parse::<usize>().ok().filter(|v| *v > 0) {
                return parsed.clamp(1, 16);
            }
        }
        QUORUM_DEFAULT_VOTER_PASSES
    }

    fn normalize_quorum_model_target(current_model: &str, raw: &str) -> String {
        let candidate = raw.trim();
        if candidate.is_empty() {
            return current_model.trim().to_string();
        }
        if let Some((provider, model)) = candidate.split_once(':') {
            return format!("{}:{}", provider.trim().to_ascii_lowercase(), model.trim());
        }
        let (provider, _) = resolve_provider_and_model(&GatewayConfig::default(), current_model);
        format!("{}:{}", provider.trim().to_ascii_lowercase(), candidate)
    }

    fn split_provider_model(provider_model: &str) -> (&str, &str) {
        if let Some((provider, model)) = provider_model.split_once(':') {
            (provider, model)
        } else {
            ("", provider_model)
        }
    }

    fn looks_like_version_pinned_model(model_id: &str) -> bool {
        let tail = model_id
            .trim()
            .rsplit('/')
            .next()
            .unwrap_or(model_id)
            .to_ascii_lowercase();
        tail.as_bytes()
            .windows(8)
            .any(|window| window.iter().all(|byte| byte.is_ascii_digit()))
    }

    fn resolve_quorum_catalog_candidate(
        requested_model: &str,
        catalog: &[String],
    ) -> Option<String> {
        if catalog.is_empty() {
            return None;
        }
        let requested_trimmed = requested_model.trim();
        if requested_trimmed.is_empty() {
            return catalog.first().cloned();
        }
        if let Some(hit) = catalog
            .iter()
            .find(|m| m.trim().eq_ignore_ascii_case(requested_trimmed))
        {
            return Some(hit.clone());
        }
        let requested_lc = requested_trimmed.to_ascii_lowercase();
        let slash_suffix = format!("/{}", requested_lc);
        if let Some(hit) = catalog.iter().find(|m| {
            let lower = m.trim().to_ascii_lowercase();
            lower.ends_with(&slash_suffix) || lower == requested_lc
        }) {
            return Some(hit.clone());
        }
        if Self::looks_like_version_pinned_model(requested_trimmed) {
            return None;
        }
        Self::rank_catalog_candidates(requested_trimmed, catalog, 1)
            .into_iter()
            .next()
    }

    fn rank_catalog_candidates(
        requested_model: &str,
        catalog: &[String],
        limit: usize,
    ) -> Vec<String> {
        if catalog.is_empty() || limit == 0 {
            return Vec::new();
        }
        let requested = requested_model.trim().to_ascii_lowercase();
        if requested.is_empty() {
            return catalog.iter().take(limit).cloned().collect();
        }
        let requested_tail = requested.rsplit('/').next().unwrap_or(requested.as_str());
        let requested_norm: String = requested
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();

        let mut scored: Vec<(usize, usize, String)> = catalog
            .iter()
            .enumerate()
            .filter_map(|(idx, candidate)| {
                let cand_trimmed = candidate.trim();
                if cand_trimmed.is_empty() {
                    return None;
                }
                let cand = cand_trimmed.to_ascii_lowercase();
                let cand_tail = cand.rsplit('/').next().unwrap_or(cand.as_str());
                let cand_norm: String =
                    cand.chars().filter(|c| c.is_ascii_alphanumeric()).collect();

                let mut score = 0usize;
                if cand == requested {
                    score += 10_000;
                }
                if cand_tail == requested_tail {
                    score += 8_000;
                }
                if cand.ends_with(&format!("/{}", requested_tail)) {
                    score += 6_000;
                }
                if cand.contains(requested_tail) || requested_tail.contains(cand_tail) {
                    score += 2_000;
                }

                let shared_prefix = requested_norm
                    .chars()
                    .zip(cand_norm.chars())
                    .take_while(|(a, b)| a == b)
                    .count();
                score += shared_prefix.saturating_mul(40);

                let shared_chars = requested_norm
                    .chars()
                    .filter(|ch| cand_norm.contains(*ch))
                    .count();
                score += shared_chars.saturating_mul(12);

                let len_delta = requested_norm.len().abs_diff(cand_norm.len());
                score = score.saturating_sub(len_delta.saturating_mul(4));
                if score == 0 {
                    return None;
                }
                Some((score, idx, cand_trimmed.to_string()))
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        scored
            .into_iter()
            .take(limit)
            .map(|(_, _, candidate)| candidate)
            .collect()
    }

    async fn resolve_quorum_models(&self, policy: &QuorumPolicy) -> (Vec<String>, Vec<String>) {
        let raw = Self::collect_quorum_models(policy, &self.current_model);
        if raw.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let mut notes = Vec::new();
        let mut resolved = Vec::new();
        for raw_target in raw {
            let normalized = Self::normalize_quorum_model_target(&self.current_model, &raw_target);
            let (provider, model_id) = Self::split_provider_model(&normalized);
            let provider = provider.trim().to_ascii_lowercase();
            let model_id = model_id.trim();
            if provider.is_empty() || model_id.is_empty() {
                continue;
            }
            let mut final_target = normalized.clone();
            let catalog = provider_model_ids(&provider).await;
            if !catalog.is_empty() {
                if let Some(candidate) = Self::resolve_quorum_catalog_candidate(model_id, &catalog)
                {
                    final_target = format!("{}:{}", provider, candidate.trim());
                    if !final_target.eq_ignore_ascii_case(&normalized) {
                        notes.push(format!(
                            "quorum model remapped via catalog: {} -> {}",
                            normalized, final_target
                        ));
                    }
                } else if Self::looks_like_version_pinned_model(model_id) {
                    notes.push(format!(
                        "quorum model preserved despite catalog miss: {}",
                        normalized
                    ));
                } else if let Some(fallback) = catalog.first() {
                    let ranked = Self::rank_catalog_candidates(model_id, &catalog, 3);
                    final_target = format!("{}:{}", provider, fallback.trim());
                    notes.push(format!(
                        "quorum model not in provider catalog: {} ; fallback -> {} ; close matches: {}",
                        normalized,
                        final_target,
                        if ranked.is_empty() {
                            "(none)".to_string()
                        } else {
                            ranked.join(", ")
                        }
                    ));
                }
            }
            if !resolved
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&final_target))
            {
                resolved.push(final_target);
            }
        }
        (resolved, notes)
    }

    fn quorum_output_char_cap() -> Option<usize> {
        if let Ok(raw) = std::env::var("HERMES_QUORUM_MAX_VOTER_OUTPUT_CHARS") {
            if Self::is_unbounded_token(&raw) {
                return None;
            }
            if let Some(parsed) = raw.trim().parse::<usize>().ok().filter(|v| *v > 0) {
                return Some(parsed);
            }
        }
        Some(QUORUM_MAX_VOTER_OUTPUT_CHARS)
    }

    fn load_quorum_agent_contract_text(&self) -> Option<(PathBuf, String)> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(raw) = std::env::var("HERMES_QUORUM_AGENT_CONTRACT_PATH") {
            let path = PathBuf::from(raw.trim());
            if !path.as_os_str().is_empty() {
                candidates.push(path);
            }
        }
        candidates.push(self.state_root.join("quorum").join("AGENTS.md"));
        candidates.push(PathBuf::from(QUORUM_AGENT_CONTRACT_DEFAULT_PATH));
        for path in candidates {
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let trimmed = content.trim();
            if trimmed.is_empty() {
                continue;
            }
            return Some((path, trimmed.to_string()));
        }
        None
    }

    fn build_quorum_voter_prompt(pass_index: usize, total_passes: usize, model: &str) -> String {
        if pass_index == 0 {
            return format!(
                "[QUORUM_VOTER] model={}\n\
                 You are in deep-voter mode. Act like quality is existential.\n\
                 Hard requirements:\n\
                 1) exhaustive exploration before conclusion,\n\
                 2) contradiction/null-hypothesis attack,\n\
                 3) final synthesis with explicit confidence and risk caveats,\n\
                 4) no placeholder names, no fake files, no invented metrics.\n\
                 Verification requirements:\n\
                 - every file/module claim must include an absolute path and exists_now=true/false\n\
                 - if you cannot verify a claim, mark it UNPROVEN (never guess)\n\
                 - include evidence bullets from tools/data/reasoning traces\n\
                 - include at least one counter-argument before final answer.\n\
                 Language requirement: answer in English unless the user explicitly requests another language.\n\
                 This is pass {}/{}.",
                model,
                pass_index + 1,
                total_passes
            );
        }
        format!(
            "[QUORUM_VOTER_REVIEW] pass {}/{}\n\
             Critique and strengthen your prior answer.\n\
             - Assume the previous draft is partially wrong.\n\
             - Remove any unverified file names/modules/metrics.\n\
             - Fix weak claims, tighten evidence, and improve actionability.\n\
             - Keep the answer in English unless the user explicitly requested another language.\n\
             - Keep objective truth over optimism.",
            pass_index + 1,
            total_passes
        )
    }

    fn extract_last_assistant_output(messages: &[hermes_core::Message]) -> String {
        for message in messages.iter().rev() {
            if message.role != hermes_core::MessageRole::Assistant {
                continue;
            }
            if let Some(content) = message.content.as_deref() {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
            if let Some(reasoning) = message.reasoning_content.as_deref() {
                let trimmed = reasoning.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
        String::new()
    }

    fn truncate_for_quorum(text: &str, max_chars: Option<usize>) -> String {
        let Some(max_chars) = max_chars else {
            return text.to_string();
        };
        if max_chars == 0 || text.chars().count() <= max_chars {
            return text.to_string();
        }
        let keep = max_chars.saturating_sub(1);
        let mut out = String::with_capacity(max_chars + 24);
        for ch in text.chars().take(keep) {
            out.push(ch);
        }
        out.push('…');
        out
    }

    fn build_quorum_synthesis_prompt(
        policy: &QuorumPolicy,
        voter_outcomes: &[QuorumVoterOutcome],
    ) -> String {
        let required_success = Self::required_quorum_success(voter_outcomes.len());
        let mut prompt = String::new();
        prompt.push_str(
            "[QUORUM_SYNTHESIS] You must synthesize across independent model voters.\n\
             Rules:\n\
             1) Use only the voter outputs below as evidence.\n\
             2) Call out disagreements explicitly.\n\
             3) If a voter failed, mark it failed and continue.\n\
             4) Return: (a) strongest case, (b) strongest counter-case, (c) final synthesis with confidence.\n\
             5) Do not claim quorum executed unless voter outputs are present.\n\
             6) Reject placeholder names/fake files/fake metrics; keep only verified claims.\n\
             7) Any file claim in final synthesis must include absolute path + exists_now status or be marked UNPROVEN.\n",
        );
        prompt.push_str(
            "             8) Do not invent commands, tool calls, benchmark results, repository paths, execution evidence, or research citations.\n\
             9) Only cite a command/file/result if it appears verbatim in the voter output or the original user prompt; otherwise mark it UNPROVEN.\n\
             10) If voter evidence is thin or failed, say that directly instead of filling the gap.\n",
        );
        prompt.push_str(&format!(
            "Configured voters: {} | mode={} | enabled={} | required_success={}\n\n",
            policy.voters, policy.mode, policy.enabled, required_success
        ));
        for (idx, voter) in voter_outcomes.iter().enumerate() {
            prompt.push_str(&format!(
                "=== VOTER {} ===\nmodel: {}\nstatus: {}\nduration_ms: {}\nturns: {}\ntool_errors: {}\n",
                idx + 1,
                voter.model,
                voter.status,
                voter.duration_ms,
                voter.total_turns,
                voter.tool_errors
            ));
            if let Some(err) = &voter.error {
                prompt.push_str("error:\n");
                prompt.push_str(err);
                prompt.push('\n');
            }
            prompt.push_str("output:\n");
            prompt.push_str(&voter.output);
            prompt.push_str("\n\n");
        }
        prompt
    }

    fn persist_quorum_artifact(
        &self,
        policy: &QuorumPolicy,
        voter_outcomes: &[QuorumVoterOutcome],
    ) -> Result<PathBuf, AgentError> {
        let dir = self.state_root.join("quorum");
        std::fs::create_dir_all(&dir).map_err(|e| {
            AgentError::Io(format!(
                "Failed to create quorum artifact dir {}: {}",
                dir.display(),
                e
            ))
        })?;
        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
        let file_name = format!("{}-{}.json", self.session_id, timestamp);
        let path = dir.join(file_name);
        let payload = serde_json::json!({
            "session_id": self.session_id,
            "saved_at": chrono::Utc::now().to_rfc3339(),
            "policy": policy,
            "model_at_start": self.current_model,
            "voters": voter_outcomes,
        });
        let raw = serde_json::to_string_pretty(&payload)
            .map_err(|e| AgentError::Config(format!("Failed to serialize quorum artifact: {e}")))?;
        std::fs::write(&path, raw).map_err(|e| {
            AgentError::Io(format!(
                "Failed to write quorum artifact {}: {}",
                path.display(),
                e
            ))
        })?;
        Ok(path)
    }

    fn update_quorum_artifact_with_synthesis(
        path: &Path,
        synthesis: &str,
    ) -> Result<(), AgentError> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            AgentError::Io(format!(
                "Failed to read quorum artifact {}: {}",
                path.display(),
                e
            ))
        })?;
        let mut payload: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
            AgentError::Config(format!(
                "Failed to parse quorum artifact {}: {}",
                path.display(),
                e
            ))
        })?;
        payload["synthesis"] = serde_json::Value::String(synthesis.trim().to_string());
        payload["synthesis_saved_at"] = serde_json::Value::String(chrono::Utc::now().to_rfc3339());
        let updated = serde_json::to_string_pretty(&payload).map_err(|e| {
            AgentError::Config(format!(
                "Failed to serialize quorum synthesis artifact {}: {}",
                path.display(),
                e
            ))
        })?;
        std::fs::write(path, updated).map_err(|e| {
            AgentError::Io(format!(
                "Failed to write quorum synthesis artifact {}: {}",
                path.display(),
                e
            ))
        })
    }

    fn apply_explore_first_runtime_defaults(config: &GatewayConfig) {
        if std::env::var("HERMES_SKILL_GUARD_MODE")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            std::env::set_var("HERMES_SKILL_GUARD_MODE", "off");
        }
        if std::env::var("HERMES_GUARD_MODE")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            std::env::set_var("HERMES_GUARD_MODE", "off");
        }
        if std::env::var("HERMES_TOOL_POLICY_PRESET")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            std::env::set_var("HERMES_TOOL_POLICY_PRESET", "dev");
        }
        if std::env::var("HERMES_TOOL_POLICY_MODE")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            std::env::set_var("HERMES_TOOL_POLICY_MODE", "audit");
        }
        if std::env::var("HERMES_REPO_REVIEW_BUDGET_PROFILE")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            std::env::set_var("HERMES_REPO_REVIEW_BUDGET_PROFILE", "off");
        }
        if std::env::var("HERMES_MAX_ITERATIONS")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            std::env::set_var("HERMES_MAX_ITERATIONS", "250");
        }
        if std::env::var("HERMES_TOOL_CALL_MAX_CONCURRENCY")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            std::env::set_var("HERMES_TOOL_CALL_MAX_CONCURRENCY", "12");
        }
        if config.delegation.max_spawn_depth.is_none()
            && std::env::var("HERMES_MAX_DELEGATE_DEPTH")
                .ok()
                .map(|v| v.trim().is_empty())
                .unwrap_or(true)
        {
            std::env::set_var("HERMES_MAX_DELEGATE_DEPTH", "4");
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
        Self::apply_explore_first_runtime_defaults(&config);

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
        let cron_scheduler = Arc::new(build_runtime_cron_scheduler(
            &config,
            &current_model,
            cron_data_dir,
            &tool_registry,
        ));
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

        let agent_inner = hermes_agent::attach_discovered_memory(AgentLoop::new(
            agent_config,
            agent_tool_registry,
            provider,
        ))
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
            last_usage: None,
            session_usage: None,
            session_cost_usd: 0.0,
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
            pending_input_prefill: None,
            quorum_armed_once: false,
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

    pub fn take_pending_input_prefill(&mut self) -> Option<String> {
        self.pending_input_prefill.take()
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
        let old_session_id = self.session_id.clone();
        self.invoke_session_lifecycle_hook(HookType::OnSessionFinalize, &old_session_id);
        self.session_id = Uuid::new_v4().to_string();
        self.messages.clear();
        self.ui_messages.clear();
        self.last_usage = None;
        self.session_usage = None;
        self.session_cost_usd = 0.0;
        self.pending_image_hint = None;
        self.session_objective = None;
        self.input_history.clear();
        self.history_index = 0;
        self.ensure_session_stub_snapshot();
        self.invoke_session_lifecycle_hook(HookType::OnSessionReset, &self.session_id);
    }

    /// Reset the current session (clear messages but keep session ID).
    pub fn reset_session(&mut self) {
        let session_id = self.session_id.clone();
        self.invoke_session_lifecycle_hook(HookType::OnSessionFinalize, &session_id);
        self.messages.clear();
        self.ui_messages.clear();
        self.last_usage = None;
        self.session_usage = None;
        self.session_cost_usd = 0.0;
        self.pending_image_hint = None;
        self.session_objective = None;
        self.input_history.clear();
        self.history_index = 0;
        self.invoke_session_lifecycle_hook(HookType::OnSessionReset, &session_id);
    }

    fn invoke_session_lifecycle_hook(&self, hook: HookType, session_id: &str) {
        let Some(plugin_manager) = self.agent.plugin_manager.as_ref() else {
            return;
        };
        let Ok(plugin_manager) = plugin_manager.lock() else {
            tracing::warn!(hook = hook.as_str(), "Plugin manager lock poisoned");
            return;
        };
        let context = serde_json::json!({
            "session_id": session_id,
            "platform": "cli",
        });
        let _ = plugin_manager.invoke_hook(hook, &context);
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

    /// Undo one or more user turns, returning the text staged for editing.
    pub fn undo_last(&mut self) -> Option<String> {
        self.undo_last_n(1)
    }

    pub fn undo_last_n(&mut self, user_turns: usize) -> Option<String> {
        let user_indices: Vec<usize> = self
            .messages
            .iter()
            .enumerate()
            .filter_map(|(idx, msg)| (msg.role == hermes_core::MessageRole::User).then_some(idx))
            .collect();
        if user_indices.is_empty() {
            return None;
        }
        let count = user_turns.max(1);
        let target_pos = user_indices.len().saturating_sub(count);
        let target_idx = user_indices[target_pos];
        let prefill = self.messages[target_idx]
            .content
            .as_deref()
            .unwrap_or_default()
            .to_string();

        match SessionPersistence::new(&self.state_root)
            .rewind_active_user_turns(&self.session_id, count)
        {
            Ok(Some(outcome)) => tracing::debug!(
                "Soft-rewound session {} at message {} (inactive={}, active={})",
                self.session_id,
                outcome.target_message_id,
                outcome.inactive_count,
                outcome.active_message_count
            ),
            Ok(None) => tracing::debug!(
                "No persisted session row available for undo in session {}",
                self.session_id
            ),
            Err(err) => tracing::debug!("Failed to soft-rewind persisted session: {}", err),
        }

        self.messages.truncate(target_idx);
        self.prune_ui_after_current_messages();
        if prefill.trim().is_empty() {
            self.pending_input_prefill = None;
        } else {
            self.pending_input_prefill = Some(prefill.clone());
        }
        Some(prefill)
    }

    /// Switch the active model, rebuilding the provider and agent loop.
    pub fn switch_model(&mut self, provider_model: &str) {
        self.current_model = provider_model.to_string();
        sync_runtime_model_env(&self.config, &self.current_model);

        let provider = build_provider(&self.config, &self.current_model);
        let agent_config = build_agent_config(&self.config, &self.current_model);
        let agent_tool_registry = Arc::new(bridge_tool_registry(&self.tool_registry));

        let agent_inner = hermes_agent::attach_discovered_memory(AgentLoop::new(
            agent_config,
            agent_tool_registry,
            provider,
        ))
        .with_callbacks(Self::stream_callbacks(self.stream_handle_shared.clone()));
        let orchestrator = Arc::new(SubAgentOrchestrator::from_parent(
            &agent_inner,
            self.state_root.clone(),
        ));
        self.agent = Arc::new(agent_inner.with_sub_agent_orchestrator(orchestrator));

        match SessionPersistence::new(&self.state_root)
            .update_session_model(&self.session_id, &self.current_model)
        {
            Ok(true) => tracing::debug!(
                "Persisted model switch for session {} to {}",
                self.session_id,
                self.current_model
            ),
            Ok(false) => {}
            Err(err) => tracing::debug!("Failed to persist model switch to session DB: {}", err),
        }

        tracing::info!("Switched model to: {}", provider_model);
    }

    /// Switch the active personality.
    pub fn switch_personality(&mut self, name: &str) {
        self.current_personality = Some(name.to_string());
        tracing::info!("Switched personality to: {}", name);
    }

    /// Return the normalized runtime provider for the active model.
    pub fn current_runtime_provider(&self) -> String {
        let (provider_name, _) = resolve_provider_and_model(&self.config, &self.current_model);
        normalize_runtime_provider_name(provider_name.as_str())
    }

    /// Refresh and verify runtime credentials for the active provider.
    ///
    /// This is the command-surface lifecycle helper used by `/auth`.
    pub async fn verify_runtime_auth(&mut self, force_refresh: bool) -> Result<String, AgentError> {
        let provider = self.current_runtime_provider();
        let before_present = provider_api_key_from_env(&provider).is_some();
        self.refresh_runtime_provider_credentials_if_needed(force_refresh)
            .await;
        let after = provider_api_key_from_env(&provider);
        let after_present = after.is_some();
        let status = if let Some(key) = after {
            format!(
                "present (masked={} chars)",
                key.chars().count().max(1).saturating_sub(8).max(1)
            )
        } else {
            "missing".to_string()
        };
        let refresh_mode = if force_refresh { "forced" } else { "passive" };
        let changed = if before_present == after_present {
            "unchanged"
        } else {
            "updated"
        };
        Ok(format!(
            "Auth verify\nprovider: {}\nmode: {}\ncredential: {}\nstate: {}\nmodel: {}",
            provider, refresh_mode, status, changed, self.current_model
        ))
    }

    async fn run_messages_with_current_agent(
        &self,
        messages: Vec<hermes_core::Message>,
        stream_enabled: bool,
    ) -> Result<hermes_core::AgentResult, AgentError> {
        self.run_messages_with_current_agent_tools(messages, stream_enabled, true)
            .await
    }

    async fn run_messages_with_current_agent_tools(
        &self,
        messages: Vec<hermes_core::Message>,
        stream_enabled: bool,
        include_tools: bool,
    ) -> Result<hermes_core::AgentResult, AgentError> {
        let tool_schemas = include_tools.then(|| self.tool_schemas.clone());
        if stream_enabled && self.config.streaming.enabled {
            let stream_handle = self.stream_handle.clone();
            let stream_cb: Option<Box<dyn Fn(hermes_core::StreamChunk) + Send + Sync>> =
                stream_handle.map(|h| {
                    Box::new(move |chunk: hermes_core::StreamChunk| {
                        h.send_chunk(chunk);
                    }) as Box<dyn Fn(hermes_core::StreamChunk) + Send + Sync>
                });
            self.agent
                .run_stream(messages, tool_schemas, stream_cb)
                .await
        } else {
            self.agent.run(messages, tool_schemas).await
        }
    }

    async fn run_quorum_fanout_turn(
        &mut self,
        run_started_at: Instant,
        policy: QuorumPolicy,
    ) -> Result<bool, AgentError> {
        let quorum_contract = self.load_quorum_agent_contract_text();
        let (voter_models, model_resolution_notes) = self.resolve_quorum_models(&policy).await;
        for note in model_resolution_notes {
            Self::emit_lifecycle_event(&self.stream_handle_shared, note);
        }
        if voter_models.len() < 2 {
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!(
                    "quorum armed but only {} distinct model configured; falling back to normal run",
                    voter_models.len()
                ),
            );
            return Ok(false);
        }

        let (base_messages, reformulated) = self.build_inference_messages();
        if reformulated {
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                "runtime prompt reformulation injected (anti-scheming + context + tool routing + contradiction self-check)",
            );
        }
        let original_model = self.current_model.clone();
        let mut outcomes: Vec<QuorumVoterOutcome> = Vec::new();
        let mut succeeded = 0usize;
        let output_char_cap = Self::quorum_output_char_cap();

        Self::emit_phase_event(
            &self.stream_handle_shared,
            "quorum",
            "multi-voter fan-out dispatch",
            30,
        );

        for (idx, model) in voter_models.iter().enumerate() {
            let display_index = idx + 1;
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!(
                    "quorum voter {}/{} dispatch -> {}",
                    display_index,
                    voter_models.len(),
                    model
                ),
            );
            if self.current_model != *model {
                self.switch_model(model);
            }
            let force_refresh = display_index == 1 || Self::quorum_force_refresh_each_voter();
            self.refresh_runtime_provider_credentials_if_needed(force_refresh)
                .await;

            let started = Instant::now();
            let max_attempts = Self::quorum_voter_retry_limit();
            let voter_passes = Self::quorum_voter_passes();
            let mut pass_errors: Vec<String> = Vec::new();
            let mut combined_output = String::new();
            let mut combined_turns: u32 = 0;
            let mut combined_tool_errors: usize = 0;
            let mut last_err: Option<AgentError> = None;
            let mut toolless_fallback_used = false;
            let voter_tools_enabled = Self::quorum_voter_tools_enabled();

            for pass_idx in 0..voter_passes {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    format!(
                        "quorum voter {}/{} pass {}/{}",
                        display_index,
                        voter_models.len(),
                        pass_idx + 1,
                        voter_passes
                    ),
                );

                let mut system_sections = Vec::new();
                if let Some((contract_path, contract_text)) = quorum_contract.as_ref() {
                    system_sections.push(format!(
                        "[QUORUM_AGENT_CONTRACT]\npath={}\nApply this contract strictly for this voter pass:\n{}",
                        contract_path.display(),
                        contract_text
                    ));
                }
                system_sections.push(Self::build_quorum_voter_prompt(
                    pass_idx,
                    voter_passes,
                    model,
                ));
                let trailing_user_context = if pass_idx > 0 && !combined_output.trim().is_empty() {
                    Some(format!(
                        "[PRIOR_VOTER_DRAFT]\n{}\n\nCritique and strengthen this prior draft for pass {}/{}.",
                        combined_output,
                        pass_idx + 1,
                        voter_passes
                    ))
                } else {
                    None
                };
                let pass_messages = Self::compose_quorum_messages(
                    system_sections,
                    base_messages.clone(),
                    trailing_user_context,
                );

                let mut attempts = 0usize;
                let mut maybe_result: Option<hermes_core::AgentResult> = None;
                while attempts < max_attempts {
                    attempts += 1;
                    match self
                        .run_messages_with_current_agent_tools(
                            pass_messages.clone(),
                            false,
                            voter_tools_enabled,
                        )
                        .await
                    {
                        Ok(result) => {
                            maybe_result = Some(result);
                            break;
                        }
                        Err(err) => {
                            if Self::is_provider_tool_payload_error(&err)
                                && Self::quorum_toolless_provider_fallback_enabled()
                                && voter_tools_enabled
                                && !toolless_fallback_used
                            {
                                toolless_fallback_used = true;
                                pass_errors.push(format!(
                                    "pass {}: provider rejected tool schema on requested model; retried this voter pass without tool schemas",
                                    pass_idx + 1
                                ));
                                Self::emit_lifecycle_event(
                                    &self.stream_handle_shared,
                                    format!(
                                        "quorum voter {}/{} provider rejected tool schema; retrying this voter pass without tool schemas",
                                        display_index,
                                        voter_models.len()
                                    ),
                                );
                                match self
                                    .run_messages_with_current_agent_tools(
                                        pass_messages.clone(),
                                        false,
                                        false,
                                    )
                                    .await
                                {
                                    Ok(result) => {
                                        maybe_result = Some(result);
                                        break;
                                    }
                                    Err(fallback_err) => {
                                        last_err = Some(fallback_err);
                                        break;
                                    }
                                }
                            }
                            if Self::is_provider_auth_or_session_error(&err)
                                && attempts < max_attempts
                            {
                                let refreshed = self.force_auth_refresh_after_error().await;
                                if refreshed {
                                    continue;
                                }
                            }
                            if Self::is_transient_retryable_error(&err) && attempts < max_attempts {
                                let backoff_ms = (attempts as u64).saturating_mul(750).max(500);
                                Self::emit_lifecycle_event(
                                    &self.stream_handle_shared,
                                    format!(
                                        "quorum voter {}/{} transient error (attempt {}/{}): {} — retrying after {}ms",
                                        display_index,
                                        voter_models.len(),
                                        attempts,
                                        max_attempts,
                                        err,
                                        backoff_ms
                                    ),
                                );
                                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms))
                                    .await;
                                continue;
                            }
                            last_err = Some(err);
                            break;
                        }
                    }
                }

                let Some(result) = maybe_result else {
                    if let Some(err) = &last_err {
                        pass_errors.push(format!("pass {}: {}", pass_idx + 1, err));
                    } else {
                        pass_errors.push(format!("pass {}: unknown error", pass_idx + 1));
                    }
                    break;
                };

                combined_turns = combined_turns.saturating_add(result.total_turns);
                combined_tool_errors =
                    combined_tool_errors.saturating_add(result.tool_errors.len());
                let latest = Self::extract_last_assistant_output(&result.messages);
                if !latest.trim().is_empty() {
                    combined_output = latest;
                } else {
                    pass_errors.push(format!("pass {}: empty assistant output", pass_idx + 1));
                    break;
                }
            }

            if !combined_output.trim().is_empty() {
                let output = Self::truncate_for_quorum(&combined_output, output_char_cap);
                let degraded = Self::quorum_output_is_degraded_non_answer(&output);
                let status = if output.trim().is_empty() {
                    "empty"
                } else if degraded {
                    pass_errors.push("voter returned degraded non-answer".to_string());
                    "degraded"
                } else {
                    succeeded += 1;
                    "ok"
                };
                let error = if !pass_errors.is_empty() {
                    Some(pass_errors.join(" | "))
                } else if output.trim().is_empty() {
                    Some("voter returned empty assistant output".to_string())
                } else {
                    None
                };
                outcomes.push(QuorumVoterOutcome {
                    model: model.clone(),
                    status: status.to_string(),
                    duration_ms: started.elapsed().as_millis() as u64,
                    total_turns: combined_turns,
                    tool_errors: combined_tool_errors,
                    output,
                    error,
                });
            } else {
                let err_text = last_err
                    .as_ref()
                    .map(ToString::to_string)
                    .or_else(|| (!pass_errors.is_empty()).then(|| pass_errors.join(" | ")))
                    .unwrap_or_else(|| "unknown voter error".to_string());
                outcomes.push(QuorumVoterOutcome {
                    model: model.clone(),
                    status: "error".to_string(),
                    duration_ms: started.elapsed().as_millis() as u64,
                    total_turns: combined_turns,
                    tool_errors: combined_tool_errors,
                    output: String::new(),
                    error: Some(err_text),
                });
            }
        }

        if self.current_model != original_model {
            self.switch_model(&original_model);
        }
        let artifact_path = self.persist_quorum_artifact(&policy, &outcomes)?;
        Self::emit_lifecycle_event(
            &self.stream_handle_shared,
            format!("quorum voter artifact saved: {}", artifact_path.display()),
        );

        let required_success = Self::required_quorum_success(voter_models.len());
        if succeeded < required_success {
            let error_summary = outcomes
                .iter()
                .map(|o| {
                    format!(
                        "{} => {}",
                        o.model,
                        match (o.status.as_str(), o.error.as_deref()) {
                            ("ok", _) => "ok".to_string(),
                            ("empty", Some(e)) => format!("empty ({})", e),
                            (_, Some(e)) => e.to_string(),
                            _ => "unknown error".to_string(),
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(" | ");
            return Err(AgentError::LlmApi(format!(
                "Quorum fan-out did not meet success threshold (required={}, got={}): {}",
                required_success, succeeded, error_summary
            )));
        }

        let synthesis_system = Self::build_quorum_synthesis_prompt(&policy, &outcomes);
        let mut synthesis_system_sections = Vec::new();
        if let Some((contract_path, contract_text)) = quorum_contract.as_ref() {
            synthesis_system_sections.push(format!(
                "[QUORUM_AGENT_CONTRACT]\npath={}\nApply this contract strictly for synthesis:\n{}",
                contract_path.display(),
                contract_text
            ));
        }
        synthesis_system_sections.push(synthesis_system);
        let synthesis_messages =
            Self::compose_quorum_messages(synthesis_system_sections, base_messages, None);

        Self::emit_phase_event(
            &self.stream_handle_shared,
            "synthesis",
            "quorum synthesis from voter outputs",
            75,
        );
        let result = self
            .run_messages_with_current_agent_tools(
                synthesis_messages,
                true,
                Self::quorum_synthesis_tools_enabled(),
            )
            .await?;
        let total_turns = result.total_turns;
        let synthesis_text = Self::extract_last_assistant_output(&result.messages);
        if let Err(err) =
            Self::update_quorum_artifact_with_synthesis(&artifact_path, &synthesis_text)
        {
            tracing::warn!("quorum synthesis artifact update skipped: {}", err);
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!("warning: quorum synthesis artifact update skipped: {}", err),
            );
        }
        if let Err(err) = self.apply_agent_result_and_persist(result) {
            tracing::warn!("session autosave skipped: {}", err);
        }
        Self::emit_lifecycle_event(
            &self.stream_handle_shared,
            format!(
                "quorum run finished in {:.2}s (voters={} succeeded={} total_turns={})",
                run_started_at.elapsed().as_secs_f64(),
                voter_models.len(),
                succeeded,
                total_turns
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
        Ok(true)
    }

    fn required_quorum_success(voter_count: usize) -> usize {
        let n = voter_count.max(1);
        (n / 2) + 1
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
        self.emit_contextlattice_connectivity_status().await;
        let provider = self.current_runtime_provider();
        let force_refresh = Self::should_force_preflight_auth_refresh(provider.as_str());
        self.refresh_runtime_provider_credentials_if_needed(force_refresh)
            .await;
        if force_refresh {
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!("preflight auth refresh forced for provider {}", provider),
            );
        }
        if let Some(policy) = self.quorum_mode_armed_for_turn() {
            self.quorum_armed_once = false;
            self.clear_quorum_system_hints_inplace();
            self.interrupt_controller.clear_interrupt();
            match self.run_quorum_fanout_turn(run_started_at, policy).await {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(err) => return Err(err),
            }
        }
        Self::emit_phase_event(
            &self.stream_handle_shared,
            "dispatch",
            "dispatching model request",
            15,
        );
        self.interrupt_controller.clear_interrupt();
        let mut remediation_attempted = false;
        let mut auth_refresh_attempts = 0usize;
        let auth_refresh_retry_limit = Self::auth_refresh_retry_limit();
        let mut transient_retry_attempts = 0usize;
        let transient_retry_limit = Self::transient_retry_limit();
        let mut objective_continuation_attempts = 0usize;
        let objective_continuation_limit = Self::objective_continuation_retry_limit();
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
            let baseline_len = self.messages.len();
            let (messages, reformulated) = self.build_inference_messages();
            if reformulated {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    "runtime prompt reformulation injected (anti-scheming + context + tool routing + contradiction self-check)",
                );
            }
            let result = self.run_messages_with_current_agent(messages, true).await;

            match result {
                Ok(result) => {
                    let total_turns = result.total_turns;
                    let interrupted = result.interrupted;
                    let finished_naturally = result.finished_naturally;
                    if objective_continuation_attempts < objective_continuation_limit {
                        if let Some(reason) =
                            self.should_force_objective_continuation(&result, baseline_len)
                        {
                            self.messages = result.messages;
                            self.messages.push(hermes_core::Message::system(
                                Self::objective_continuation_system_prompt(&reason),
                            ));
                            self.prune_ui_after_current_messages();
                            objective_continuation_attempts += 1;
                            Self::emit_lifecycle_event(
                                &self.stream_handle_shared,
                                format!(
                                    "objective continuation enforcer triggered ({}/{}): {}",
                                    objective_continuation_attempts,
                                    objective_continuation_limit,
                                    reason
                                ),
                            );
                            Self::emit_phase_event(
                                &self.stream_handle_shared,
                                "objective",
                                "auto-continuing objective loop for concrete execution",
                                50,
                            );
                            continue;
                        }
                    }
                    if let Err(err) = self.apply_agent_result_and_persist(result) {
                        tracing::warn!("session autosave skipped: {}", err);
                    }
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!(
                            "run finished in {:.2}s (total_turns={})",
                            run_started_at.elapsed().as_secs_f64(),
                            total_turns
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
                    if interrupted {
                        tracing::info!("Agent loop returned interrupted=true (graceful stop)");
                        if self.stream_handle.is_some() {
                            self.push_ui_assistant("[Agent execution interrupted]");
                        } else {
                            println!("[Agent execution interrupted]");
                        }
                    } else if !finished_naturally {
                        tracing::warn!(
                            "Agent stopped after {} turns (did not finish naturally)",
                            total_turns
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
                    if Self::is_provider_auth_or_session_error(&e) {
                        if auth_refresh_attempts < auth_refresh_retry_limit {
                            if self.force_auth_refresh_after_error().await {
                                auth_refresh_attempts += 1;
                                Self::emit_lifecycle_event(
                                    &self.stream_handle_shared,
                                    format!(
                                        "auth refresh retry {}/{}",
                                        auth_refresh_attempts, auth_refresh_retry_limit
                                    ),
                                );
                                continue;
                            }
                        } else {
                            Self::emit_lifecycle_event(
                                &self.stream_handle_shared,
                                format!(
                                    "auth refresh retries exhausted ({})",
                                    auth_refresh_retry_limit
                                ),
                            );
                        }
                    }
                    if Self::is_transient_retryable_error(&e)
                        && transient_retry_attempts < transient_retry_limit
                    {
                        transient_retry_attempts += 1;
                        let backoff_ms = (transient_retry_attempts as u64)
                            .saturating_mul(1_000)
                            .max(800);
                        Self::emit_lifecycle_event(
                            &self.stream_handle_shared,
                            format!(
                                "transient runtime error retry {}/{} after {}ms: {}",
                                transient_retry_attempts, transient_retry_limit, backoff_ms, e
                            ),
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
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
        let usage = result.usage.clone();
        let run_cost = result
            .session_cost_usd
            .or_else(|| usage.as_ref().and_then(|usage| usage.estimated_cost))
            .filter(|cost| cost.is_finite() && *cost >= 0.0);

        self.last_usage = usage.clone();
        if let Some(usage) = usage {
            self.session_usage = Some(merge_usage_stats(self.session_usage.take(), &usage));
        }
        if let Some(run_cost) = run_cost {
            self.session_cost_usd += run_cost;
        }
        self.messages = result.messages;
        self.prune_ui_after_current_messages();
    }

    /// Apply finalized messages and persist the session snapshot.
    pub fn apply_agent_result_and_persist(
        &mut self,
        result: hermes_core::AgentResult,
    ) -> Result<(), AgentError> {
        self.apply_agent_result(result);
        self.persist_session_snapshot(None).map(|_| ())
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
            || message.contains("token_expired")
            || message.contains("expired_token")
            || message.contains("token expired")
            || message.contains("invalid_token")
            || message.contains("expired")
            || message.contains("authentication")
            || message.contains("session expired")
    }

    fn is_provider_tool_payload_error(err: &AgentError) -> bool {
        let message = match err {
            AgentError::LlmApi(msg)
            | AgentError::Config(msg)
            | AgentError::ToolExecution(msg)
            | AgentError::Gateway(msg)
            | AgentError::AuthFailed(msg) => msg.to_ascii_lowercase(),
            _ => return false,
        };
        let mentions_tool_payload =
            message.contains("tool") || message.contains("function") || message.contains("schema");
        let provider_payload_rejected = message.contains("provider returned error")
            && mentions_tool_payload
            && (message.contains("request is not valid")
                || message.contains("valid payload")
                || message.contains("check the model name")
                || message.contains("invalid"));
        let openai_shape_rejected = (message.contains("no choices in response")
            || message.contains("empty choices array"))
            && mentions_tool_payload
            && (message.contains("request is not valid")
                || message.contains("valid payload")
                || message.contains("provider returned error")
                || message.contains("invalid"));
        let explicit_tool_schema_rejected =
            message.contains("tool") && (message.contains("invalid") || message.contains("schema"));
        let strict_function_shape =
            message.contains("invalid input") && message.contains("function");
        provider_payload_rejected
            || openai_shape_rejected
            || explicit_tool_schema_rejected
            || strict_function_shape
            || (message.contains("422") && message.contains("valid payload"))
    }

    fn quorum_output_is_degraded_non_answer(output: &str) -> bool {
        let lower = output.to_ascii_lowercase();
        lower.contains("objective delivery compromised")
            || lower.contains("reverting to hermes")
            || lower.contains("safe-mode response")
            || lower.contains("safe mode response")
            || (lower.contains("i do not have") && lower.contains("tools"))
            || (lower.contains("cannot access") && lower.contains("tools"))
    }

    async fn force_auth_refresh_after_error(&mut self) -> bool {
        let (provider_name, _) = resolve_provider_and_model(&self.config, &self.current_model);
        let provider = normalize_runtime_provider_name(provider_name.as_str());
        let (notice, refreshed) = match provider.as_str() {
            "nous" => match resolve_nous_runtime_credentials(
                true,
                true,
                NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
            )
            .await
            {
                Ok(creds) => {
                    let changed = Self::apply_nous_runtime_credentials(&creds);
                    if changed {
                        self.switch_model(&self.current_model.clone());
                    }
                    (
                        Some("Nous auth auto-refresh succeeded; retrying request.".to_string()),
                        true,
                    )
                }
                Err(err) => {
                    if Self::nous_refresh_contention_error(&err) {
                        match resolve_nous_runtime_credentials(
                            false,
                            true,
                            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                            DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
                        )
                        .await
                        {
                            Ok(creds) => {
                                let changed = Self::apply_nous_runtime_credentials(&creds);
                                if changed {
                                    self.switch_model(&self.current_model.clone());
                                }
                                (
                                    Some(
                                        "Nous refresh busy; reused cached runtime credential and retrying request."
                                            .to_string(),
                                    ),
                                    true,
                                )
                            }
                            Err(cache_err) => (
                                Some(format!(
                                    "Nous cached credential hydration failed after refresh contention: {}",
                                    cache_err
                                )),
                                false,
                            ),
                        }
                    } else if Self::auth_error_requires_nous_login(&err)
                        && self
                            .attempt_interactive_nous_login("runtime auth refresh failed")
                            .await
                    {
                        match resolve_nous_runtime_credentials(
                            true,
                            true,
                            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                            DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
                        )
                        .await
                        {
                            Ok(creds) => {
                                let changed = Self::apply_nous_runtime_credentials(&creds);
                                if changed {
                                    self.switch_model(&self.current_model.clone());
                                }
                                (
                                    Some(
                                        "Nous auth re-login succeeded; retrying request."
                                            .to_string(),
                                    ),
                                    true,
                                )
                            }
                            Err(retry_err) => (
                                Some(format!("Nous auth auto-refresh failed: {}", retry_err)),
                                false,
                            ),
                        }
                    } else {
                        (
                            Some(format!("Nous auth auto-refresh failed: {}", err)),
                            false,
                        )
                    }
                }
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
                        (
                            Some(
                                "Qwen OAuth auto-refresh succeeded; retrying request.".to_string(),
                            ),
                            true,
                        )
                    }
                    Err(err) => (
                        Some(format!("Qwen OAuth auto-refresh failed: {}", err)),
                        false,
                    ),
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
                        (
                            Some(
                                "Gemini OAuth auto-refresh succeeded; retrying request."
                                    .to_string(),
                            ),
                            true,
                        )
                    }
                    Err(err) => (
                        Some(format!("Gemini OAuth auto-refresh failed: {}", err)),
                        false,
                    ),
                }
            }
            _ => (None, false),
        };

        if let Some(text) = notice {
            Self::emit_lifecycle_event(&self.stream_handle_shared, &text);
            if self.stream_handle.is_some() {
                self.push_ui_assistant(text);
            } else {
                println!("{}", text);
            }
        }
        refreshed
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

        let selected = Self::resolve_quorum_catalog_candidate(current_model_id, &catalog)
            .or_else(|| catalog.first().cloned())?;

        let next_model = format!("{}:{}", provider, selected.trim());
        if next_model.eq_ignore_ascii_case(&self.current_model) {
            return None;
        }
        let close = Self::rank_catalog_candidates(current_model_id, &catalog, 3);
        let notice = format!(
            "Model catalog remediation: `{}` failed with not-found; switching to `{}` and retrying once. close matches: {}",
            self.current_model,
            next_model,
            if close.is_empty() {
                "(none)".to_string()
            } else {
                close.join(", ")
            }
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
    use crate::alpha_runtime::{
        load_quorum_policy, set_objective_contract_behavior_mode, set_quorum_policy,
        upsert_objective_contract,
    };
    use crate::test_env_lock;
    use hermes_agent::plugins::{HookResult, Plugin, PluginContext, PluginManager};
    use hermes_config::LlmProviderConfig;
    use std::collections::HashMap;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        test_env_lock::lock()
    }

    struct EnvSnapshot {
        vars: Vec<(&'static str, Option<String>)>,
    }

    impl EnvSnapshot {
        fn capture(keys: &[&'static str]) -> Self {
            Self {
                vars: keys
                    .iter()
                    .map(|key| (*key, std::env::var(key).ok()))
                    .collect(),
            }
        }
    }

    impl Drop for EnvSnapshot {
        fn drop(&mut self) {
            for (key, value) in &self.vars {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    struct TestToolHandler {
        name: &'static str,
    }

    #[async_trait::async_trait]
    impl hermes_core::ToolHandler for TestToolHandler {
        async fn execute(&self, _params: Value) -> Result<String, hermes_core::ToolError> {
            Ok(format!("{} ok", self.name))
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema::new(
                self.name,
                "test tool",
                hermes_core::JsonSchema::new("object"),
            )
        }
    }

    fn register_test_tool(tools: &ToolRegistry, name: &'static str) {
        let handler: Arc<dyn hermes_core::ToolHandler> = Arc::new(TestToolHandler { name });
        tools.register(
            name,
            "test",
            handler.schema(),
            handler,
            Arc::new(|| true),
            vec![],
            true,
            "test tool",
            "T",
            None,
        );
    }

    fn build_minimal_test_app() -> App {
        build_minimal_test_app_with_state_root(hermes_home_dir())
    }

    fn build_minimal_test_app_with_state_root(state_root: PathBuf) -> App {
        let config = Arc::new(GatewayConfig::default());
        let tool_registry = Arc::new(ToolRegistry::new());
        let agent_tool_registry = Arc::new(bridge_tool_registry(&tool_registry));
        let agent_config = build_agent_config(config.as_ref(), "openai:gpt-4o");
        let provider: Arc<dyn LlmProvider> = Arc::new(NoBackendProvider {
            model: "openai:gpt-4o".to_string(),
        });
        let agent_inner = hermes_agent::attach_discovered_memory(AgentLoop::new(
            agent_config,
            agent_tool_registry,
            provider,
        ))
        .with_callbacks(App::stream_callbacks(Arc::new(StdMutex::new(None))));
        let orchestrator = Arc::new(SubAgentOrchestrator::from_parent(
            &agent_inner,
            state_root.clone(),
        ));
        let agent = Arc::new(agent_inner.with_sub_agent_orchestrator(orchestrator));

        App {
            state_root,
            config,
            agent,
            tool_registry,
            tool_schemas: Vec::new(),
            messages: Vec::new(),
            ui_messages: Vec::new(),
            session_id: "test-session".to_string(),
            running: true,
            current_model: "openai:gpt-4o".to_string(),
            last_usage: None,
            session_usage: None,
            session_cost_usd: 0.0,
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
            pending_input_prefill: None,
            quorum_armed_once: false,
            pet_settings: PetSettings::default(),
        }
    }

    #[test]
    fn test_switch_model_updates_existing_session_db_row() {
        let _guard = env_test_lock();
        let env_keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        let saved_env: Vec<(&str, Option<String>)> = env_keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();

        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
        let persistence = SessionPersistence::new(tmp.path());
        persistence
            .persist_session(
                &app.session_id,
                &[hermes_core::Message::user("hello")],
                Some(&app.current_model),
                Some("cli"),
                None,
                None,
            )
            .unwrap();

        app.switch_model("anthropic:claude-sonnet-4-6");

        assert_eq!(
            persistence.get_session_model(&app.session_id).unwrap(),
            Some("anthropic:claude-sonnet-4-6".to_string())
        );

        for (key, value) in saved_env {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn test_undo_last_n_soft_rewinds_and_sets_prefill() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
        app.messages = vec![
            hermes_core::Message::system("sys"),
            hermes_core::Message::user("question 1"),
            hermes_core::Message::assistant("answer 1"),
            hermes_core::Message::user("question 2"),
            hermes_core::Message::assistant("answer 2"),
            hermes_core::Message::user("question 3"),
            hermes_core::Message::assistant("answer 3"),
        ];
        let persistence = SessionPersistence::new(tmp.path());
        persistence
            .persist_session(
                &app.session_id,
                &app.messages,
                None,
                Some("cli"),
                None,
                None,
            )
            .unwrap();

        let prefill = app.undo_last_n(2).expect("undo");

        assert_eq!(prefill, "question 2");
        assert_eq!(
            app.take_pending_input_prefill().as_deref(),
            Some("question 2")
        );
        assert_eq!(
            app.messages
                .iter()
                .filter_map(|m| m.content.as_deref())
                .collect::<Vec<_>>(),
            vec!["sys", "question 1", "answer 1"]
        );
        assert_eq!(persistence.load_session(&app.session_id).unwrap().len(), 3);
        let recent = persistence
            .list_recent_user_messages(&app.session_id, 5)
            .unwrap();
        assert_eq!(
            recent
                .iter()
                .filter_map(|row| row.content.as_deref())
                .collect::<Vec<_>>(),
            vec!["question 1"]
        );
    }

    struct LifecycleHookPlugin {
        seen: Arc<StdMutex<Vec<(String, String)>>>,
    }

    #[async_trait::async_trait]
    impl Plugin for LifecycleHookPlugin {
        fn meta(&self) -> hermes_agent::plugins::PluginMeta {
            hermes_agent::plugins::PluginMeta {
                name: "lifecycle-recorder".to_string(),
                version: "0.1.0".to_string(),
                description: "Lifecycle recorder".to_string(),
                author: None,
            }
        }

        async fn initialize(&self) -> Result<(), AgentError> {
            Ok(())
        }

        async fn shutdown(&self) -> Result<(), AgentError> {
            Ok(())
        }

        fn register(&self, ctx: &mut PluginContext) {
            let finalize_seen = self.seen.clone();
            ctx.on(
                HookType::OnSessionFinalize,
                Arc::new(move |value| {
                    let session_id = value
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    finalize_seen
                        .lock()
                        .unwrap()
                        .push(("on_session_finalize".to_string(), session_id));
                    HookResult::Ok
                }),
            );
            let reset_seen = self.seen.clone();
            ctx.on(
                HookType::OnSessionReset,
                Arc::new(move |value| {
                    let session_id = value
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    reset_seen
                        .lock()
                        .unwrap()
                        .push(("on_session_reset".to_string(), session_id));
                    HookResult::Ok
                }),
            );
        }
    }

    fn attach_lifecycle_recorder(app: &mut App, seen: Arc<StdMutex<Vec<(String, String)>>>) {
        let mut plugin_manager = PluginManager::new();
        plugin_manager.register(Arc::new(LifecycleHookPlugin { seen }));
        let agent = AgentLoop::new(
            app.agent.config.clone(),
            app.agent.tool_registry.clone(),
            app.agent.llm_provider.clone(),
        )
        .with_plugins(Arc::new(StdMutex::new(plugin_manager)));
        app.agent = Arc::new(agent);
    }

    #[test]
    fn app_new_session_invokes_session_lifecycle_hooks() {
        let mut app = build_minimal_test_app();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        attach_lifecycle_recorder(&mut app, seen.clone());
        let old_session_id = app.session_id.clone();

        app.new_session();

        let events = seen.lock().unwrap().clone();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            ("on_session_finalize".to_string(), old_session_id)
        );
        assert_eq!(events[1].0, "on_session_reset");
        assert_eq!(events[1].1, app.session_id);
        assert_ne!(events[0].1, events[1].1);
    }

    #[test]
    fn app_reset_session_invokes_session_lifecycle_hooks() {
        let mut app = build_minimal_test_app();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        attach_lifecycle_recorder(&mut app, seen.clone());
        let session_id = app.session_id.clone();

        app.reset_session();

        let events = seen.lock().unwrap().clone();
        assert_eq!(
            events,
            vec![
                ("on_session_finalize".to_string(), session_id.clone()),
                ("on_session_reset".to_string(), session_id)
            ]
        );
        assert_eq!(app.session_id, "test-session");
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
    fn test_collect_quorum_models_dedup_and_limit() {
        let policy = QuorumPolicy {
            enabled: true,
            voters: 3,
            models: vec![
                "nous:openai/gpt-5.5-pro".to_string(),
                "nous:openai/gpt-5.5-pro".to_string(),
                "nous:anthropic/claude-opus-4.7".to_string(),
                "nous:deepseek/deepseek-v4-pro".to_string(),
            ],
            mode: "balanced".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let models = App::collect_quorum_models(&policy, "nous:openai/gpt-5.5-pro");
        assert_eq!(
            models,
            vec![
                "nous:openai/gpt-5.5-pro".to_string(),
                "nous:anthropic/claude-opus-4.7".to_string(),
                "nous:deepseek/deepseek-v4-pro".to_string()
            ]
        );
    }

    #[test]
    fn test_extract_last_assistant_output_prefers_non_empty_assistant_text() {
        let messages = vec![
            hermes_core::Message::user("hello"),
            hermes_core::Message::assistant(""),
            hermes_core::Message::assistant("final answer"),
        ];
        let output = App::extract_last_assistant_output(&messages);
        assert_eq!(output, "final answer");
    }

    #[test]
    fn test_required_quorum_success_majority() {
        assert_eq!(App::required_quorum_success(1), 1);
        assert_eq!(App::required_quorum_success(2), 2);
        assert_eq!(App::required_quorum_success(3), 2);
        assert_eq!(App::required_quorum_success(4), 3);
        assert_eq!(App::required_quorum_success(5), 3);
    }

    #[test]
    fn test_quorum_mode_armed_once_triggers_without_system_hint() {
        let _guard = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());

        let _ = set_quorum_policy(
            true,
            Some(3),
            Some(vec![
                "nous:openai/gpt-5.5-pro".to_string(),
                "nous:anthropic/claude-opus-4.7".to_string(),
            ]),
        )
        .expect("set quorum policy");
        let policy = load_quorum_policy().expect("load quorum policy");
        assert!(
            policy.enabled,
            "quorum policy should be enabled in test home"
        );

        let mut app = build_minimal_test_app();
        app.messages = vec![hermes_core::Message::user("run quorum now")];
        app.quorum_armed_once = true;
        let has_hint = app.messages.iter().any(|message| {
            message.role == hermes_core::MessageRole::System
                && message
                    .content
                    .as_deref()
                    .unwrap_or_default()
                    .starts_with(QUORUM_HINT_PREFIX)
        });
        let has_user_turn = app
            .messages
            .iter()
            .any(|m| m.role == hermes_core::MessageRole::User);

        assert!(
            app.quorum_mode_armed_for_turn().is_some(),
            "one-shot quorum arm should trigger fan-out without relying on stale system hints (enabled={}, armed_once={}, has_hint={}, has_user_turn={})",
            policy.enabled,
            app.quorum_armed_once,
            has_hint,
            has_user_turn
        );

        match prev_home {
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }
    }

    #[test]
    fn test_clear_quorum_system_hints_inplace_preserves_other_system_messages() {
        let mut app = build_minimal_test_app();
        app.messages = vec![
            hermes_core::Message::system("[QUORUM_MODE] quorum armed"),
            hermes_core::Message::system("normal system context"),
            hermes_core::Message::user("hello"),
        ];

        app.clear_quorum_system_hints_inplace();

        assert_eq!(app.messages.len(), 2);
        assert!(app.messages.iter().all(|message| !message
            .content
            .as_deref()
            .unwrap_or_default()
            .starts_with("[QUORUM_MODE] ")));
        assert!(app
            .messages
            .iter()
            .any(|message| message.content.as_deref() == Some("normal system context")));
    }

    #[test]
    fn test_run_agent_quorum_arm_persists_artifact_even_on_voter_failures() {
        let _guard = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());

        let _ = set_quorum_policy(
            true,
            Some(3),
            Some(vec![
                "openai:gpt-4o".to_string(),
                "anthropic:claude-3-5-sonnet".to_string(),
                "nous:openai/gpt-5.5-pro".to_string(),
            ]),
        )
        .expect("set quorum policy");

        let mut app = build_minimal_test_app();
        app.session_id = "quorum-test-session".to_string();
        app.messages = vec![hermes_core::Message::user(
            "no tools, just verify quorum fan-out branch",
        )];
        app.quorum_armed_once = true;

        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let result = runtime.block_on(app.run_agent());
        assert!(
            result.is_err(),
            "NoBackendProvider should fail voter inference, but quorum artifact must still persist"
        );

        let quorum_dir = app.state_root.join("quorum");
        let artifacts: Vec<_> = std::fs::read_dir(&quorum_dir)
            .expect("read quorum artifact dir")
            .flatten()
            .filter(|entry| entry.path().extension().and_then(|v| v.to_str()) == Some("json"))
            .collect();
        assert!(
            !artifacts.is_empty(),
            "quorum run should write at least one artifact file"
        );
        let latest = artifacts
            .iter()
            .max_by_key(|entry| entry.metadata().and_then(|m| m.modified()).ok())
            .expect("latest quorum artifact");
        let raw = std::fs::read_to_string(latest.path()).expect("read quorum artifact");
        let doc: serde_json::Value = serde_json::from_str(&raw).expect("parse quorum artifact");
        assert_eq!(
            doc.get("session_id").and_then(|v| v.as_str()),
            Some("quorum-test-session")
        );
        assert!(
            doc.get("voters")
                .and_then(|v| v.as_array())
                .is_some_and(|arr| !arr.is_empty()),
            "artifact should contain per-voter outcomes"
        );

        match prev_home {
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }
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
    fn app_tracks_actual_usage_from_agent_results_and_resets() {
        let mut app = build_minimal_test_app();
        let first = hermes_core::UsageStats {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            estimated_cost: Some(0.0015),
        };
        let second = hermes_core::UsageStats {
            prompt_tokens: 7,
            completion_tokens: 3,
            total_tokens: 10,
            estimated_cost: None,
        };

        app.apply_agent_result(hermes_core::AgentResult {
            messages: vec![hermes_core::Message::assistant("first")],
            finished_naturally: true,
            total_turns: 1,
            tool_errors: Vec::new(),
            usage: Some(first.clone()),
            interrupted: false,
            session_cost_usd: Some(0.002),
            session_started_hooks_fired: false,
        });
        app.apply_agent_result(hermes_core::AgentResult {
            messages: vec![hermes_core::Message::assistant("second")],
            finished_naturally: true,
            total_turns: 1,
            tool_errors: Vec::new(),
            usage: Some(second.clone()),
            interrupted: false,
            session_cost_usd: None,
            session_started_hooks_fired: false,
        });

        assert_eq!(app.last_usage, Some(second));
        let session = app.session_usage.as_ref().expect("session usage");
        assert_eq!(session.prompt_tokens, 17);
        assert_eq!(session.completion_tokens, 8);
        assert_eq!(session.total_tokens, 25);
        assert_eq!(session.estimated_cost, Some(0.0015));
        assert!((app.session_cost_usd - 0.002).abs() < f64::EPSILON);

        app.reset_session();
        assert!(app.last_usage.is_none());
        assert!(app.session_usage.is_none());
        assert_eq!(app.session_cost_usd, 0.0);
    }

    #[test]
    fn test_apply_agent_result_and_persist_writes_updated_messages() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut app = build_minimal_test_app();
        app.state_root = tmp.path().join("custom-state-root");
        app.session_id = "persist-after-run".to_string();

        let result = hermes_core::AgentResult {
            messages: vec![
                hermes_core::Message::user("hello"),
                hermes_core::Message::assistant("world"),
            ],
            finished_naturally: true,
            interrupted: false,
            total_turns: 1,
            ..Default::default()
        };

        app.apply_agent_result_and_persist(result)
            .expect("persist updated messages");

        let path = app
            .state_root
            .join("sessions")
            .join("persist-after-run.json");
        assert!(path.exists());
        let raw = std::fs::read_to_string(path).expect("read snapshot");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("parse snapshot");
        assert_eq!(
            value
                .get("messages")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len()),
            Some(2)
        );
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
    fn test_build_agent_config_loads_prefill_messages_from_config() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_PREFILL_MESSAGES_FILE"]);
        std::env::remove_var("HERMES_PREFILL_MESSAGES_FILE");

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("prefill.json"),
            r#"[{"role":"system","content":"cli prefill"},{"role":"user","content":"cli example"}]"#,
        )
        .unwrap();
        let cfg = GatewayConfig {
            home_dir: Some(dir.path().to_string_lossy().to_string()),
            prefill_messages_file: Some("prefill.json".to_string()),
            ..GatewayConfig::default()
        };

        let agent_cfg = build_agent_config(&cfg, "openai:gpt-4o");
        assert_eq!(agent_cfg.prefill_messages.len(), 2);
        assert_eq!(
            agent_cfg.prefill_messages[0].content.as_deref(),
            Some("cli prefill")
        );
        assert_eq!(
            agent_cfg.prefill_messages[1].content.as_deref(),
            Some("cli example")
        );
    }

    #[test]
    fn test_build_agent_config_maps_runtime_provider_request_timeout_seconds() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "anthropic".to_string(),
            LlmProviderConfig {
                api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
                request_timeout_seconds: Some(45.5),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "anthropic:claude-sonnet-4.5");
        let runtime = agent_cfg
            .runtime_providers
            .get("anthropic")
            .expect("runtime provider should exist");

        assert_eq!(runtime.request_timeout_seconds, Some(45.5));
    }

    #[test]
    fn test_build_agent_config_maps_delegation_max_spawn_depth_without_legacy_ceiling() {
        let mut cfg = GatewayConfig::default();
        cfg.delegation.max_spawn_depth = Some(99);
        let agent_cfg = build_agent_config(&cfg, "openai:gpt-4o");
        assert_eq!(agent_cfg.max_delegate_depth, 99);

        cfg.delegation.max_spawn_depth = Some(0);
        let agent_cfg = build_agent_config(&cfg, "openai:gpt-4o");
        assert_eq!(agent_cfg.max_delegate_depth, 1);
    }

    #[test]
    fn test_build_agent_config_maps_delegation_provider_model_runtime_overrides() {
        let mut cfg = GatewayConfig::default();
        cfg.delegation.model = Some(" google/gemini-3-flash-preview ".to_string());
        cfg.delegation.provider = Some(" openrouter ".to_string());
        cfg.delegation.base_url = Some(" http://localhost:1234/v1 ".to_string());
        cfg.delegation.api_key = Some(" local-key ".to_string());

        let agent_cfg = build_agent_config(&cfg, "nous:hermes-3");

        assert_eq!(
            agent_cfg.delegation_model.as_deref(),
            Some("google/gemini-3-flash-preview")
        );
        assert_eq!(agent_cfg.delegation_provider.as_deref(), Some("openrouter"));
        assert_eq!(
            agent_cfg.delegation_base_url.as_deref(),
            Some("http://localhost:1234/v1")
        );
        assert_eq!(agent_cfg.delegation_api_key.as_deref(), Some("local-key"));
    }

    #[test]
    fn explore_first_defaults_do_not_shadow_configured_delegation_depth() {
        let _guard = env_test_lock();
        let keys = [
            "HERMES_SKILL_GUARD_MODE",
            "HERMES_GUARD_MODE",
            "HERMES_TOOL_POLICY_PRESET",
            "HERMES_TOOL_POLICY_MODE",
            "HERMES_REPO_REVIEW_BUDGET_PROFILE",
            "HERMES_MAX_ITERATIONS",
            "HERMES_TOOL_CALL_MAX_CONCURRENCY",
            "HERMES_MAX_DELEGATE_DEPTH",
        ];
        let _snapshot = EnvSnapshot::capture(&keys);
        for key in keys {
            std::env::remove_var(key);
        }

        let mut cfg = GatewayConfig::default();
        cfg.delegation.max_spawn_depth = Some(99);
        App::apply_explore_first_runtime_defaults(&cfg);

        assert!(std::env::var("HERMES_MAX_DELEGATE_DEPTH").is_err());

        cfg.delegation.max_spawn_depth = None;
        App::apply_explore_first_runtime_defaults(&cfg);
        assert_eq!(
            std::env::var("HERMES_MAX_DELEGATE_DEPTH").ok().as_deref(),
            Some("4")
        );
    }

    #[tokio::test]
    async fn runtime_cron_scheduler_uses_configured_provider_not_minimal_fallback() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "chatcmpl-live-cron-test",
                "object": "chat.completion",
                "created": 0,
                "model": "gpt-live-cron",
                "choices": [
                    {
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": "live-cron-provider-ok"
                        },
                        "finish_reason": "stop"
                    }
                ],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2
                }
            })))
            .mount(&server)
            .await;

        let mut config = GatewayConfig::default();
        config.model = Some("openai:gpt-live-cron".to_string());
        config.llm_providers.insert(
            "openai".to_string(),
            LlmProviderConfig {
                api_key: Some("test-key".to_string()),
                base_url: Some(server.uri()),
                model: Some("gpt-live-cron".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let temp = tempfile::tempdir().expect("cron tempdir");
        let tools = ToolRegistry::new();
        let scheduler = build_runtime_cron_scheduler(
            &config,
            "openai:gpt-live-cron",
            temp.path().to_path_buf(),
            &tools,
        );
        let job_id = scheduler
            .create_job(hermes_cron::CronJob::new(
                "0 * * * *",
                "prove live cron provider wiring",
            ))
            .await
            .expect("create cron job");
        let result = scheduler.run_job(&job_id).await.expect("run cron job");
        let final_text = result
            .messages
            .iter()
            .rev()
            .find_map(|message| message.content.as_deref())
            .unwrap_or_default();

        assert!(final_text.contains("live-cron-provider-ok"));
        assert!(!final_text.contains("fallback LLM path"));
        server.verify().await;
    }

    #[test]
    fn runtime_cron_scheduler_bridge_excludes_recursive_cronjob_tool() {
        let tools = ToolRegistry::new();
        register_test_tool(&tools, "cronjob");
        register_test_tool(&tools, "terminal");

        let agent_registry = bridge_tool_registry_excluding(&tools, &["cronjob"]);
        let names = agent_registry.names();

        assert!(!names.contains(&"cronjob".to_string()));
        assert!(names.contains(&"terminal".to_string()));
    }

    #[test]
    fn test_build_agent_config_preserves_same_host_provider_api_modes() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "codex".to_string(),
            LlmProviderConfig {
                api_key_env: Some("CODEX_KEY".to_string()),
                base_url: Some("https://gateway.example.com/v1".to_string()),
                api_mode: Some("codex_responses".to_string()),
                ..LlmProviderConfig::default()
            },
        );
        cfg.llm_providers.insert(
            "anthropic".to_string(),
            LlmProviderConfig {
                api_key_env: Some("ANTHROPIC_KEY".to_string()),
                base_url: Some("https://gateway.example.com/v1".to_string()),
                api_mode: Some("anthropic_messages".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "codex:gpt-5");
        let codex = agent_cfg
            .runtime_providers
            .get("codex")
            .expect("codex runtime provider should exist");
        let anthropic = agent_cfg
            .runtime_providers
            .get("anthropic")
            .expect("anthropic runtime provider should exist");

        assert_eq!(codex.api_key_env.as_deref(), Some("CODEX_KEY"));
        assert_eq!(
            codex.base_url.as_deref(),
            Some("https://gateway.example.com/v1")
        );
        assert_eq!(codex.api_mode, Some(ApiMode::CodexResponses));
        assert_eq!(anthropic.api_key_env.as_deref(), Some("ANTHROPIC_KEY"));
        assert_eq!(
            anthropic.base_url.as_deref(),
            Some("https://gateway.example.com/v1")
        );
        assert_eq!(anthropic.api_mode, Some(ApiMode::AnthropicMessages));
    }

    #[test]
    fn test_build_agent_config_maps_named_custom_runtime_provider() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "beans".to_string(),
            LlmProviderConfig {
                api_key: Some("sk-beans".to_string()),
                base_url: Some("http://beans.local/v1".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "beans:my-model");
        assert_eq!(agent_cfg.provider.as_deref(), Some("beans"));
        let runtime = agent_cfg
            .runtime_providers
            .get("beans")
            .expect("named custom runtime provider should exist");
        assert_eq!(runtime.api_key.as_deref(), Some("sk-beans"));
        assert_eq!(runtime.base_url.as_deref(), Some("http://beans.local/v1"));
    }

    #[test]
    fn test_build_agent_config_maps_active_provider_max_tokens() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_MAX_TOKENS"]);
        std::env::remove_var("HERMES_MAX_TOKENS");

        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "openrouter".to_string(),
            LlmProviderConfig {
                max_tokens: Some(4096),
                ..LlmProviderConfig::default()
            },
        );
        cfg.llm_providers.insert(
            "openai".to_string(),
            LlmProviderConfig {
                max_tokens: Some(2048),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "openrouter:anthropic/claude-sonnet-4.6");

        assert_eq!(agent_cfg.max_tokens, Some(4096));
    }

    #[test]
    fn test_build_agent_config_maps_normalized_provider_max_tokens_alias() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_MAX_TOKENS"]);
        std::env::remove_var("HERMES_MAX_TOKENS");

        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "openai-codex".to_string(),
            LlmProviderConfig {
                max_tokens: Some(1234),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "codex:gpt-5");

        assert_eq!(agent_cfg.max_tokens, Some(1234));
    }

    #[test]
    fn test_build_agent_config_env_max_tokens_overrides_provider_cap() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_MAX_TOKENS"]);

        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "openrouter".to_string(),
            LlmProviderConfig {
                max_tokens: Some(4096),
                ..LlmProviderConfig::default()
            },
        );

        std::env::set_var("HERMES_MAX_TOKENS", "8192");
        let agent_cfg = build_agent_config(&cfg, "openrouter:anthropic/claude-sonnet-4.6");
        assert_eq!(agent_cfg.max_tokens, Some(8192));

        std::env::set_var("HERMES_MAX_TOKENS", "not-a-number");
        let agent_cfg = build_agent_config(&cfg, "openrouter:anthropic/claude-sonnet-4.6");
        assert_eq!(agent_cfg.max_tokens, Some(4096));

        std::env::set_var("HERMES_MAX_TOKENS", "0");
        let agent_cfg = build_agent_config(&cfg, "openrouter:anthropic/claude-sonnet-4.6");
        assert_eq!(agent_cfg.max_tokens, Some(4096));
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
    fn test_build_agent_config_merges_fast_service_tier_into_extra_body() {
        let mut cfg = GatewayConfig::default();
        cfg.agent.service_tier = Some("fast".to_string());
        cfg.llm_providers.insert(
            "nous".to_string(),
            LlmProviderConfig {
                extra_body: Some(serde_json::json!({
                    "reasoning_effort": "medium"
                })),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "nous:moonshotai/kimi-k2.6");
        let body = agent_cfg.extra_body.expect("extra body");
        assert_eq!(body["reasoning_effort"], "medium");
        assert_eq!(body["service_tier"], "priority");
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
    fn test_build_agent_config_maps_failover_chain_from_env() {
        let _guard = env_test_lock();
        std::env::set_var(
            "HERMES_FALLBACK_MODELS",
            "nous:moonshotai/kimi-k2.6,openai:gpt-4o-mini",
        );
        std::env::remove_var("HERMES_FALLBACK_MODEL");
        let cfg = GatewayConfig::default();
        let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");
        assert_eq!(
            agent_cfg.retry.fallback_model.as_deref(),
            Some("nous:moonshotai/kimi-k2.6")
        );
        assert_eq!(
            agent_cfg.retry.fallback_models,
            vec![
                "nous:moonshotai/kimi-k2.6".to_string(),
                "openai:gpt-4o-mini".to_string()
            ]
        );
        std::env::remove_var("HERMES_FALLBACK_MODELS");
    }

    #[test]
    fn test_build_agent_config_maps_single_failover_model_from_env() {
        let _guard = env_test_lock();
        std::env::remove_var("HERMES_FALLBACK_MODELS");
        std::env::set_var("HERMES_FALLBACK_MODEL", "anthropic:claude-3-5-sonnet");
        let cfg = GatewayConfig::default();
        let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");
        assert_eq!(
            agent_cfg.retry.fallback_model.as_deref(),
            Some("anthropic:claude-3-5-sonnet")
        );
        assert_eq!(
            agent_cfg.retry.fallback_models,
            vec!["anthropic:claude-3-5-sonnet".to_string()]
        );
        std::env::remove_var("HERMES_FALLBACK_MODEL");
    }

    #[test]
    fn test_build_agent_config_maps_failover_chain_from_config() {
        let _guard = env_test_lock();
        std::env::remove_var("HERMES_FALLBACK_MODELS");
        std::env::remove_var("HERMES_FALLBACK_MODEL");

        let mut cfg = GatewayConfig::default();
        cfg.fallback_models = vec![
            "openrouter:anthropic/claude-sonnet-4.6".to_string(),
            "nous:Hermes-4".to_string(),
        ];
        cfg.fallback_model = Some("OpenRouter:anthropic/claude-sonnet-4.6".to_string());

        let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");
        assert_eq!(
            agent_cfg.retry.fallback_model.as_deref(),
            Some("openrouter:anthropic/claude-sonnet-4.6")
        );
        assert_eq!(
            agent_cfg.retry.fallback_models,
            vec![
                "openrouter:anthropic/claude-sonnet-4.6".to_string(),
                "nous:Hermes-4".to_string()
            ]
        );
    }

    #[test]
    fn test_build_agent_config_env_failover_overrides_config() {
        let _guard = env_test_lock();
        std::env::remove_var("HERMES_FALLBACK_MODELS");
        std::env::set_var("HERMES_FALLBACK_MODEL", "anthropic:claude-3-5-sonnet");

        let mut cfg = GatewayConfig::default();
        cfg.fallback_models = vec!["openrouter:backup".to_string()];

        let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");
        assert_eq!(
            agent_cfg.retry.fallback_models,
            vec!["anthropic:claude-3-5-sonnet".to_string()]
        );
        std::env::remove_var("HERMES_FALLBACK_MODEL");
    }

    #[test]
    fn test_build_agent_config_maps_agent_api_max_retries() {
        let mut cfg = GatewayConfig::default();
        cfg.agent.api_max_retries = Some(11);

        let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");

        assert_eq!(agent_cfg.retry.max_retries, 11);
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
    fn test_resolve_provider_and_model_uses_named_custom_provider_model() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "beans".to_string(),
            LlmProviderConfig {
                model: Some("my-model".to_string()),
                base_url: Some("http://beans.local/v1".to_string()),
                ..LlmProviderConfig::default()
            },
        );
        let (provider, model) = resolve_provider_and_model(&cfg, "my-model");
        assert_eq!(provider, "beans");
        assert_eq!(model, "my-model");
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
        let _guard = env_test_lock();
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers
            .insert("anthropic".to_string(), LlmProviderConfig::default());

        let keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        let previous: Vec<(&str, Option<String>)> = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();
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

        for (key, value) in previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn test_startup_model_env_sync_uses_config_provider_not_stale_env() {
        let _guard = env_test_lock();
        let keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
        ];
        let previous: Vec<(&str, Option<String>)> = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();
        for key in keys {
            std::env::remove_var(key);
        }
        std::env::set_var("HERMES_INFERENCE_PROVIDER", "openrouter");

        let mut cfg = GatewayConfig::default();
        cfg.model = Some("anthropic:claude-sonnet-4-6".to_string());
        cfg.llm_providers
            .insert("anthropic".to_string(), LlmProviderConfig::default());

        let configured_model = cfg.model.as_deref().expect("model should be set");
        let startup = resolve_startup_model(&cfg, configured_model);
        sync_runtime_model_env(&cfg, &startup);

        assert_eq!(startup, "anthropic:claude-sonnet-4-6");
        assert_eq!(
            std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_MODEL").ok().as_deref(),
            Some("anthropic:claude-sonnet-4-6")
        );

        for (key, value) in previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
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
    fn test_provider_api_key_from_env_prefers_kimi_coding_key_for_code_provider() {
        let _guard = env_test_lock();
        for key in [
            "KIMI_CODING_API_KEY",
            "KIMI_API_KEY",
            "MOONSHOT_API_KEY",
            "KIMI_CN_API_KEY",
        ] {
            std::env::remove_var(key);
        }

        std::env::set_var("KIMI_API_KEY", "sk-legacy");
        std::env::set_var("KIMI_CODING_API_KEY", "sk-kimi-code");
        assert_eq!(
            provider_api_key_from_env("kimi-coding").as_deref(),
            Some("sk-kimi-code")
        );
        assert_eq!(
            provider_api_key_from_env("kimi").as_deref(),
            Some("sk-legacy")
        );
        std::env::set_var("KIMI_CN_API_KEY", "sk-cn");
        assert_eq!(
            provider_api_key_from_env("kimi-coding-cn").as_deref(),
            Some("sk-cn")
        );

        for key in [
            "KIMI_CODING_API_KEY",
            "KIMI_API_KEY",
            "MOONSHOT_API_KEY",
            "KIMI_CN_API_KEY",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn test_provider_api_key_from_env_supports_extended_registry() {
        let _guard = env_test_lock();
        let env_vars = [
            "AI_GATEWAY_API_KEY",
            "DEEPSEEK_API_KEY",
            "HF_TOKEN",
            "KILOCODE_API_KEY",
            "NVIDIA_API_KEY",
            "OLLAMA_LOCAL_API_KEY",
            "LLAMA_CPP_API_KEY",
            "VLLM_API_KEY",
            "MLX_API_KEY",
            "APPLE_ANE_API_KEY",
            "SGLANG_API_KEY",
            "TGI_API_KEY",
            "NOVITA_API_KEY",
            "OPENCODE_GO_API_KEY",
            "OPENCODE_ZEN_API_KEY",
            "XAI_API_KEY",
            "XIAOMI_API_KEY",
            "ARCEEAI_API_KEY",
            "ARCEE_API_KEY",
            "GLM_API_KEY",
            "ZAI_API_KEY",
            "Z_AI_API_KEY",
            "GMI_API_KEY",
            "MINIMAX_CN_API_KEY",
            "NOUS_API_KEY",
            "COPILOT_GITHUB_TOKEN",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GITHUB_COPILOT_TOKEN",
            "TOKENHUB_API_KEY",
        ];
        for env_var in env_vars {
            std::env::remove_var(env_var);
        }
        let checks = [
            ("AI_GATEWAY_API_KEY", "ai-gateway"),
            ("AI_GATEWAY_API_KEY", "vercel"),
            ("DEEPSEEK_API_KEY", "deepseek"),
            ("HF_TOKEN", "huggingface"),
            ("HF_TOKEN", "hf"),
            ("HF_TOKEN", "hugging-face"),
            ("HF_TOKEN", "huggingface-hub"),
            ("KILOCODE_API_KEY", "kilocode"),
            ("NVIDIA_API_KEY", "nvidia"),
            ("OLLAMA_LOCAL_API_KEY", "ollama-local"),
            ("LLAMA_CPP_API_KEY", "llama-cpp"),
            ("VLLM_API_KEY", "vllm"),
            ("MLX_API_KEY", "mlx"),
            ("APPLE_ANE_API_KEY", "apple-ane"),
            ("SGLANG_API_KEY", "sglang"),
            ("TGI_API_KEY", "tgi"),
            ("NOVITA_API_KEY", "novita"),
            ("OPENCODE_GO_API_KEY", "opencode-go"),
            ("OPENCODE_ZEN_API_KEY", "opencode-zen"),
            ("XAI_API_KEY", "xai"),
            ("XIAOMI_API_KEY", "xiaomi"),
            ("GLM_API_KEY", "zai"),
            ("GLM_API_KEY", "glm"),
            ("ZAI_API_KEY", "z-ai"),
            ("Z_AI_API_KEY", "zhipu"),
            ("GMI_API_KEY", "gmi-cloud"),
            ("GMI_API_KEY", "gmicloud"),
            ("ARCEEAI_API_KEY", "arcee-ai"),
            ("ARCEEAI_API_KEY", "arceeai"),
            ("XIAOMI_API_KEY", "mimo"),
            ("XIAOMI_API_KEY", "xiaomi-mimo"),
            ("TOKENHUB_API_KEY", "tencent-tokenhub"),
            ("TOKENHUB_API_KEY", "tencent"),
            ("TOKENHUB_API_KEY", "tokenhub"),
            ("MINIMAX_CN_API_KEY", "minimax_cn"),
            ("NOUS_API_KEY", "nous-api"),
            ("NOUS_API_KEY", "nous-portal-api"),
            ("COPILOT_GITHUB_TOKEN", "github-copilot"),
            ("GH_TOKEN", "github-models"),
            ("GITHUB_TOKEN", "copilot"),
            ("GITHUB_COPILOT_TOKEN", "copilot"),
        ];
        for (env_var, provider) in checks {
            for env_var in env_vars {
                std::env::remove_var(env_var);
            }
            let expected = format!("token-for-{provider}");
            std::env::set_var(env_var, expected.clone());
            assert_eq!(
                provider_api_key_from_env(provider).as_deref(),
                Some(expected.as_str())
            );
        }
        for env_var in env_vars {
            std::env::remove_var(env_var);
        }
    }

    #[test]
    fn test_normalize_runtime_provider_name_covers_aliases() {
        assert_eq!(
            normalize_runtime_provider_name("gemini-cli"),
            "google-gemini-cli"
        );
        assert_eq!(normalize_runtime_provider_name("nous_api"), "nous-api");
        assert_eq!(normalize_runtime_provider_name("nousapi"), "nous-api");
        assert_eq!(
            normalize_runtime_provider_name("nous-portal-api"),
            "nous-api"
        );
        assert_eq!(normalize_runtime_provider_name("moonshot"), "kimi");
        assert_eq!(normalize_runtime_provider_name("novita-ai"), "novita");
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
        assert_eq!(normalize_runtime_provider_name("glm"), "zai");
        assert_eq!(normalize_runtime_provider_name("z-ai"), "zai");
        assert_eq!(normalize_runtime_provider_name("zhipu"), "zai");
        assert_eq!(normalize_runtime_provider_name("github-copilot"), "copilot");
        assert_eq!(normalize_runtime_provider_name("github-models"), "copilot");
        assert_eq!(
            normalize_runtime_provider_name("github-copilot-acp"),
            "copilot-acp"
        );
        assert_eq!(
            normalize_runtime_provider_name("copilot-acp-agent"),
            "copilot-acp"
        );
        assert_eq!(normalize_runtime_provider_name("hf"), "huggingface");
        assert_eq!(
            normalize_runtime_provider_name("hugging-face"),
            "huggingface"
        );
        assert_eq!(
            normalize_runtime_provider_name("huggingface-hub"),
            "huggingface"
        );
        assert_eq!(normalize_runtime_provider_name("aigateway"), "ai-gateway");
        assert_eq!(normalize_runtime_provider_name("vercel"), "ai-gateway");
        assert_eq!(normalize_runtime_provider_name("gmi-cloud"), "gmi");
        assert_eq!(normalize_runtime_provider_name("gmicloud"), "gmi");
        assert_eq!(
            normalize_runtime_provider_name("google-ai-studio"),
            "gemini"
        );
        assert_eq!(normalize_runtime_provider_name("arcee-ai"), "arcee");
        assert_eq!(normalize_runtime_provider_name("arceeai"), "arcee");
        assert_eq!(normalize_runtime_provider_name("azure"), "azure-foundry");
        assert_eq!(
            normalize_runtime_provider_name("azure-ai-foundry"),
            "azure-foundry"
        );
        assert_eq!(normalize_runtime_provider_name("mimo"), "xiaomi");
        assert_eq!(normalize_runtime_provider_name("xiaomi-mimo"), "xiaomi");
        assert_eq!(
            normalize_runtime_provider_name("tencent-cloud"),
            "tencent-tokenhub"
        );
        assert_eq!(
            normalize_runtime_provider_name("tokenhub"),
            "tencent-tokenhub"
        );
        assert_eq!(normalize_runtime_provider_name("aws"), "bedrock");
        assert_eq!(normalize_runtime_provider_name("aws-bedrock"), "bedrock");
        assert_eq!(normalize_runtime_provider_name("amazon"), "bedrock");
    }

    #[test]
    fn test_provider_base_url_from_env_supports_api_provider_aliases() {
        let _guard = env_test_lock();
        let env_vars = [
            "COPILOT_API_BASE_URL",
            "GLM_BASE_URL",
            "KIMI_BASE_URL",
            "MINIMAX_CN_BASE_URL",
            "GMI_BASE_URL",
            "HF_BASE_URL",
            "AI_GATEWAY_BASE_URL",
            "TOKENHUB_BASE_URL",
            "ARCEE_BASE_URL",
            "XIAOMI_BASE_URL",
            "BEDROCK_BASE_URL",
        ];
        for env_var in env_vars {
            std::env::remove_var(env_var);
        }

        std::env::set_var("COPILOT_API_BASE_URL", "https://copilot.example/v1");
        assert_eq!(
            provider_base_url_from_env("github-copilot").as_deref(),
            Some("https://copilot.example/v1")
        );
        std::env::set_var("GLM_BASE_URL", "https://glm.example/v4");
        assert_eq!(
            provider_base_url_from_env("z-ai").as_deref(),
            Some("https://glm.example/v4")
        );
        std::env::set_var("KIMI_BASE_URL", "https://kimi.example/v1");
        assert_eq!(
            provider_base_url_from_env("moonshot").as_deref(),
            Some("https://kimi.example/v1")
        );
        assert_eq!(
            provider_base_url_from_env("kimi-coding").as_deref(),
            Some("https://kimi.example/v1")
        );
        std::env::set_var("MINIMAX_CN_BASE_URL", "https://minimax-cn.example/v1");
        assert_eq!(
            provider_base_url_from_env("minimax_cn").as_deref(),
            Some("https://minimax-cn.example/v1")
        );
        std::env::set_var("GMI_BASE_URL", "https://gmi.example/v1");
        assert_eq!(
            provider_base_url_from_env("gmi-cloud").as_deref(),
            Some("https://gmi.example/v1")
        );
        assert_eq!(
            provider_base_url_from_env("gmicloud").as_deref(),
            Some("https://gmi.example/v1")
        );
        std::env::set_var("HF_BASE_URL", "https://hf.example/v1");
        assert_eq!(
            provider_base_url_from_env("huggingface-hub").as_deref(),
            Some("https://hf.example/v1")
        );
        std::env::set_var("AI_GATEWAY_BASE_URL", "https://gateway.example/v1");
        assert_eq!(
            provider_base_url_from_env("vercel").as_deref(),
            Some("https://gateway.example/v1")
        );
        std::env::set_var("TOKENHUB_BASE_URL", "https://tokenhub.example/v1");
        assert_eq!(
            provider_base_url_from_env("tencent").as_deref(),
            Some("https://tokenhub.example/v1")
        );
        std::env::set_var("ARCEE_BASE_URL", "https://arcee.example/v1");
        assert_eq!(
            provider_base_url_from_env("arcee-ai").as_deref(),
            Some("https://arcee.example/v1")
        );
        std::env::set_var("XIAOMI_BASE_URL", "https://mimo.example/v1");
        assert_eq!(
            provider_base_url_from_env("mimo").as_deref(),
            Some("https://mimo.example/v1")
        );
        std::env::set_var("BEDROCK_BASE_URL", "https://bedrock-runtime.example");
        assert_eq!(
            provider_base_url_from_env("aws").as_deref(),
            Some("https://bedrock-runtime.example")
        );

        for env_var in env_vars {
            std::env::remove_var(env_var);
        }
    }

    #[test]
    fn test_provider_default_base_url_supports_upstream_aliases() {
        assert_eq!(
            provider_default_base_url("github-copilot"),
            Some(COPILOT_BASE_URL)
        );
        assert_eq!(provider_default_base_url("glm"), Some(ZAI_BASE_URL));
        assert_eq!(
            provider_default_base_url("minimax_cn"),
            Some(MINIMAX_CN_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("huggingface-hub"),
            Some(HUGGINGFACE_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("vercel"),
            Some(AI_GATEWAY_BASE_URL)
        );
        assert_eq!(provider_default_base_url("gmi-cloud"), Some(GMI_BASE_URL));
        assert_eq!(provider_default_base_url("gmicloud"), Some(GMI_BASE_URL));
        assert_eq!(provider_default_base_url("arcee-ai"), Some(ARCEE_BASE_URL));
        assert_eq!(provider_default_base_url("mimo"), Some(XIAOMI_BASE_URL));
        assert_eq!(
            provider_default_base_url("tencent"),
            Some(TENCENT_TOKENHUB_BASE_URL)
        );
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
        assert!(!default_mouse_enabled());

        std::env::set_var("HERMES_TUI_MOUSE", "off");
        assert!(!default_mouse_enabled());

        std::env::set_var("HERMES_TUI_MOUSE", "1");
        assert!(default_mouse_enabled());

        std::env::remove_var("HERMES_TUI_MOUSE");
    }

    #[test]
    fn test_contextlattice_orchestrator_url_prefers_contextlattice_env_then_memmcp() {
        let _lock = env_test_lock();
        std::env::remove_var("CONTEXTLATTICE_ORCHESTRATOR_URL");
        std::env::remove_var("MEMMCP_ORCHESTRATOR_URL");
        assert_eq!(
            App::contextlattice_orchestrator_url(),
            "http://127.0.0.1:8075"
        );

        std::env::set_var("MEMMCP_ORCHESTRATOR_URL", "http://127.0.0.1:9999/");
        assert_eq!(
            App::contextlattice_orchestrator_url(),
            "http://127.0.0.1:9999"
        );

        std::env::set_var("CONTEXTLATTICE_ORCHESTRATOR_URL", "http://127.0.0.1:7777/");
        assert_eq!(
            App::contextlattice_orchestrator_url(),
            "http://127.0.0.1:7777"
        );

        std::env::remove_var("CONTEXTLATTICE_ORCHESTRATOR_URL");
        std::env::remove_var("MEMMCP_ORCHESTRATOR_URL");
    }

    #[test]
    fn test_build_inference_messages_injects_runtime_reformulation() {
        let _lock = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::set_var("HERMES_RUNTIME_PROMPT_REFORMULATION", "1");
        std::env::set_var("HERMES_RUNTIME_CONTRADICTION_SELF_CHECK", "1");
        std::env::set_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "focus");
        std::env::set_var(
            "CONTEXTLATTICE_TOPIC_PATH",
            "runbooks/objective/test-objective",
        );
        let contract =
            upsert_objective_contract("Grow SOL with controlled risk", true).expect("obj");

        let mut app = build_minimal_test_app();
        app.messages.push(hermes_core::Message::user(
            "provide 3 more ideas with contextlattice being one",
        ));
        let (messages, injected) = app.build_inference_messages();
        assert!(injected);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, hermes_core::MessageRole::System);
        let injected_text = messages[0].content.as_deref().unwrap_or_default();
        assert!(injected_text.contains(App::RUNTIME_REFORMULATION_PREFIX));
        assert!(injected_text.contains("tool-profile(mode): focus"));
        assert!(injected_text.contains("contextlattice(topic): runbooks/objective/test-objective"));
        assert!(injected_text.contains(contract.id.as_str()));
        assert!(injected_text.contains("UNPROVEN/CONTRADICTORY"));
        assert!(injected_text.contains("execute at least one concrete action"));
        assert!(injected_text.contains("iterative objective momentum"));
        assert!(injected_text.contains("objective behavior directives:"));
        assert!(injected_text.contains("objective success criteria:"));
        assert!(injected_text.contains("objective loop protocol:"));
        assert!(injected_text.contains("user-request(routing-preview):"));
        assert!(
            injected_text.contains("full user request remains available as the next user message")
        );
        assert_eq!(messages[1].role, hermes_core::MessageRole::User);

        match prev_home {
            Some(val) => std::env::set_var("HERMES_HOME", val),
            None => std::env::remove_var("HERMES_HOME"),
        }
        std::env::remove_var("HERMES_RUNTIME_PROMPT_REFORMULATION");
        std::env::remove_var("HERMES_RUNTIME_CONTRADICTION_SELF_CHECK");
        std::env::remove_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE");
        std::env::remove_var("CONTEXTLATTICE_TOPIC_PATH");
    }

    #[test]
    fn test_runtime_reformulation_caps_long_prompt_preview_without_losing_user_message() {
        let _lock = env_test_lock();
        std::env::set_var("HERMES_RUNTIME_PROMPT_REFORMULATION", "1");
        std::env::set_var("HERMES_RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS", "48");

        let long_prompt =
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu".repeat(12);
        let mut app = build_minimal_test_app();
        app.messages
            .push(hermes_core::Message::user(long_prompt.clone()));

        let (messages, injected) = app.build_inference_messages();
        assert!(injected);
        assert_eq!(messages.len(), 2);
        let injected_text = messages[0].content.as_deref().unwrap_or_default();
        assert!(injected_text.contains("user-request(routing-preview):"));
        assert!(injected_text.contains("preview truncated"));
        assert!(!injected_text.contains(&long_prompt));
        assert_eq!(
            messages[1].content.as_deref().unwrap_or_default(),
            long_prompt
        );

        std::env::remove_var("HERMES_RUNTIME_PROMPT_REFORMULATION");
        std::env::remove_var("HERMES_RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS");
    }

    #[test]
    fn test_compose_quorum_messages_coalesces_systems_before_user_messages() {
        let messages = App::compose_quorum_messages(
            vec!["contract rules".to_string(), "voter prompt".to_string()],
            vec![
                hermes_core::Message::system("runtime reformulation"),
                hermes_core::Message::user("mission prompt"),
            ],
            Some("prior draft".to_string()),
        );

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, hermes_core::MessageRole::System);
        assert_eq!(messages[1].role, hermes_core::MessageRole::User);
        assert_eq!(messages[2].role, hermes_core::MessageRole::User);
        assert_eq!(messages[3].role, hermes_core::MessageRole::User);
        let system = messages[0].content.as_deref().unwrap_or_default();
        assert!(system.contains("runtime reformulation"));
        assert!(!system.contains("contract rules"));
        let control = messages[1].content.as_deref().unwrap_or_default();
        assert!(control.contains("[QUORUM_CONTROL]"));
        assert!(control.contains("contract rules"));
        assert!(control.contains("voter prompt"));
        assert_eq!(messages[2].content.as_deref(), Some("mission prompt"));
        assert_eq!(messages[3].content.as_deref(), Some("prior draft"));
    }

    #[test]
    fn test_build_inference_messages_respects_reformulation_toggle_off() {
        let _lock = env_test_lock();
        std::env::set_var("HERMES_RUNTIME_PROMPT_REFORMULATION", "off");
        let mut app = build_minimal_test_app();
        app.messages
            .push(hermes_core::Message::user("plain request"));
        let (messages, injected) = app.build_inference_messages();
        assert!(!injected);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, hermes_core::MessageRole::User);
        std::env::remove_var("HERMES_RUNTIME_PROMPT_REFORMULATION");
    }

    #[test]
    fn test_looks_like_status_only_output_detects_defer_only_language() {
        assert!(App::looks_like_status_only_output(
            "I will proceed with investigation next. Let me know if you'd like me to continue."
        ));
        assert!(!App::looks_like_status_only_output(
            "Implemented patch in path=crates/hermes-cli/src/app.rs and verified with cargo test result: pass."
        ));
    }

    #[test]
    fn test_should_force_objective_continuation_for_mission_status_only_turn() {
        let _lock = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::set_var("HERMES_OBJECTIVE_EXECUTION_ENFORCER", "1");

        let mut app = build_minimal_test_app();
        app.messages.push(hermes_core::Message::user(
            "Proceed with objective and improve outcomes continuously.",
        ));
        upsert_objective_contract(
            "Run this assignment in perpetuity and continuously improve output quality",
            false,
        )
        .expect("set objective");
        set_objective_contract_behavior_mode("mission").expect("set mission mode");

        let baseline_len = app.messages.len();
        let mut result_messages = app.messages.clone();
        result_messages.push(hermes_core::Message::assistant(
            "I will proceed with the next steps and share updates shortly.",
        ));
        let result = hermes_core::AgentResult {
            messages: result_messages,
            finished_naturally: true,
            total_turns: 1,
            ..Default::default()
        };

        let reason = app.should_force_objective_continuation(&result, baseline_len);
        assert!(reason.is_some());

        match prev_home {
            Some(val) => std::env::set_var("HERMES_HOME", val),
            None => std::env::remove_var("HERMES_HOME"),
        }
        std::env::remove_var("HERMES_OBJECTIVE_EXECUTION_ENFORCER");
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
        let provider_payload = AgentError::LlmApi(
            "API error 400 Bad Request: This request is not valid. Additional info: Provider returned error"
                .to_string(),
        );
        assert!(!App::is_provider_auth_or_session_error(&provider_payload));
    }

    #[test]
    fn test_is_provider_tool_payload_error_detects_schema_rejections() {
        let generic_provider = AgentError::LlmApi(
            "API error 400 Bad Request: This request is not valid. Check the model name and other parameters. Additional info: Provider returned error"
                .to_string(),
        );
        assert!(!App::is_provider_tool_payload_error(&generic_provider));
        let no_choices = AgentError::LlmApi(
            "No choices in response (status=400; message=This request is not valid. Additional info: Provider returned error)"
                .to_string(),
        );
        assert!(!App::is_provider_tool_payload_error(&no_choices));
        let provider_tool = AgentError::LlmApi(
            "API error 400 Bad Request: tools request is not valid. Additional info: Provider returned error"
                .to_string(),
        );
        assert!(App::is_provider_tool_payload_error(&provider_tool));
        let invalid_tool = AgentError::LlmApi("tools schema is invalid".to_string());
        assert!(App::is_provider_tool_payload_error(&invalid_tool));
        let rate_limit = AgentError::LlmApi("HTTP 429 Too Many Requests".to_string());
        assert!(!App::is_provider_tool_payload_error(&rate_limit));
    }

    #[test]
    fn test_quorum_zero_env_no_longer_means_unbounded() {
        let _lock = env_test_lock();
        std::env::remove_var("HERMES_QUORUM_VOTER_PASSES");
        assert_eq!(App::quorum_voter_passes(), QUORUM_DEFAULT_VOTER_PASSES);
        std::env::set_var("HERMES_QUORUM_VOTER_PASSES", "0");
        assert_eq!(App::quorum_voter_passes(), QUORUM_DEFAULT_VOTER_PASSES);
        std::env::set_var("HERMES_QUORUM_VOTER_PASSES", "max");
        assert_eq!(App::quorum_voter_passes(), 16);
        std::env::remove_var("HERMES_QUORUM_VOTER_PASSES");
    }

    #[test]
    fn test_quorum_output_is_degraded_non_answer() {
        assert!(App::quorum_output_is_degraded_non_answer(
            "Objective delivery compromised; reverting to Hermes base model"
        ));
        assert!(App::quorum_output_is_degraded_non_answer(
            "I do not have access to tools in this environment"
        ));
        assert!(!App::quorum_output_is_degraded_non_answer(
            "Strategy table: edge hypothesis, required data, implementation delta"
        ));
    }

    #[test]
    fn test_is_transient_retryable_error_detects_timeout_and_rate_limit() {
        let timeout =
            AgentError::LlmApi("request timed out while waiting for provider".to_string());
        let rate_limit = AgentError::LlmApi("HTTP 429 Too Many Requests".to_string());
        let model_missing =
            AgentError::LlmApi("API error 404 Not Found: model missing".to_string());
        assert!(App::is_transient_retryable_error(&timeout));
        assert!(App::is_transient_retryable_error(&rate_limit));
        assert!(!App::is_transient_retryable_error(&model_missing));
    }

    #[test]
    fn test_auth_error_requires_nous_login_detects_missing_login_shape() {
        let err = AgentError::AuthFailed(
            "Hermes is not logged into Nous Portal. Run `hermes portal`.".to_string(),
        );
        assert!(App::auth_error_requires_nous_login(&err));
        let legacy = AgentError::AuthFailed(
            "Stored Nous auth state is invalid; re-run `hermes auth nous`.".to_string(),
        );
        assert!(App::auth_error_requires_nous_login(&legacy));
        let unrelated = AgentError::AuthFailed("rate limited".to_string());
        assert!(!App::auth_error_requires_nous_login(&unrelated));
    }

    #[test]
    fn test_auto_nous_reauth_toggle_defaults_on() {
        let _guard = env_test_lock();
        std::env::remove_var("HERMES_AUTO_NOUS_REAUTH");
        assert!(App::auto_nous_reauth_enabled());
        std::env::set_var("HERMES_AUTO_NOUS_REAUTH", "0");
        assert!(!App::auto_nous_reauth_enabled());
        std::env::remove_var("HERMES_AUTO_NOUS_REAUTH");
    }

    #[test]
    fn test_rank_catalog_candidates_prefers_syntactic_nearest() {
        let catalog = vec![
            "qwen/qwen3.6-plus".to_string(),
            "qwen/qwen3.6-max-preview".to_string(),
            "deepseek/deepseek-r1".to_string(),
        ];
        let ranked = App::rank_catalog_candidates("qwen3.6-max", &catalog, 2);
        assert!(!ranked.is_empty());
        assert_eq!(ranked[0], "qwen/qwen3.6-max-preview");
    }

    #[test]
    fn test_resolve_quorum_catalog_candidate_uses_relative_match_when_exact_missing() {
        let catalog = vec![
            "moonshotai/kimi-k2.6".to_string(),
            "qwen/qwen3.6-max-preview".to_string(),
        ];
        let resolved = App::resolve_quorum_catalog_candidate("qwen3.6-max", &catalog);
        assert_eq!(resolved.as_deref(), Some("qwen/qwen3.6-max-preview"));
    }

    #[test]
    fn test_resolve_quorum_catalog_candidate_preserves_version_pinned_miss() {
        let catalog = vec![
            "openai/gpt-5.5-pro".to_string(),
            "anthropic/claude-opus-4.7".to_string(),
            "qwen/qwen3.6-max-preview".to_string(),
        ];

        let gpt = App::resolve_quorum_catalog_candidate("openai/gpt-5.5-pro-20260423", &catalog);
        let claude = App::resolve_quorum_catalog_candidate(
            "anthropic/claude-4.7-opus-fast-20260512",
            &catalog,
        );
        let qwen =
            App::resolve_quorum_catalog_candidate("qwen/qwen3.6-max-preview-20260420", &catalog);

        assert!(
            gpt.is_none(),
            "version-pinned GPT ID should not fuzzy-remap"
        );
        assert!(
            claude.is_none(),
            "version-pinned Claude ID should not fuzzy-remap"
        );
        assert!(
            qwen.is_none(),
            "version-pinned Qwen ID should not fuzzy-remap"
        );
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

fn build_retry_config(config: &GatewayConfig) -> hermes_agent::agent_loop::RetryConfig {
    let mut retry_cfg = hermes_agent::agent_loop::RetryConfig::default();
    if let Some(max_retries) = config.agent.api_max_retries {
        retry_cfg.max_retries = max_retries;
    }
    let mut seen = std::collections::HashSet::new();

    let mut push_candidate =
        |candidate: &str, retry_cfg: &mut hermes_agent::agent_loop::RetryConfig| {
            let trimmed = candidate.trim();
            if trimmed.is_empty() {
                return;
            }
            let identity = trimmed.to_ascii_lowercase();
            if seen.insert(identity) {
                retry_cfg.fallback_models.push(trimmed.to_string());
            }
        };

    for model in &config.fallback_models {
        push_candidate(model, &mut retry_cfg);
    }
    if let Some(model) = config.fallback_model.as_deref() {
        push_candidate(model, &mut retry_cfg);
    }

    if !retry_cfg.fallback_models.is_empty() {
        retry_cfg.fallback_model = retry_cfg.fallback_models.first().cloned();
    }

    if let Ok(raw) = std::env::var("HERMES_FALLBACK_MODELS") {
        let parsed: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string)
            .collect();
        if !parsed.is_empty() {
            retry_cfg.fallback_models = parsed;
            retry_cfg.fallback_model = retry_cfg.fallback_models.first().cloned();
            return retry_cfg;
        }
    }

    if let Ok(raw) = std::env::var("HERMES_FALLBACK_MODEL") {
        let value = raw.trim();
        if !value.is_empty() {
            retry_cfg.fallback_model = Some(value.to_string());
            retry_cfg.fallback_models = vec![value.to_string()];
        }
    }

    retry_cfg
}

fn parse_provider_api_mode(value: &str) -> Option<ApiMode> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "chat_completions" => Some(ApiMode::ChatCompletions),
        "anthropic_messages" => Some(ApiMode::AnthropicMessages),
        "codex_responses" => Some(ApiMode::CodexResponses),
        "bedrock_converse" => Some(ApiMode::BedrockConverse),
        _ => None,
    }
}

fn active_llm_provider_config<'a>(
    config: &'a GatewayConfig,
    provider_name: &str,
    runtime_provider: &str,
) -> Option<&'a hermes_config::LlmProviderConfig> {
    config
        .llm_providers
        .get(provider_name)
        .or_else(|| config.llm_providers.get(runtime_provider))
        .or_else(|| {
            config.llm_providers.iter().find_map(|(name, cfg)| {
                if name.eq_ignore_ascii_case(provider_name)
                    || name.eq_ignore_ascii_case(runtime_provider)
                {
                    Some(cfg)
                } else {
                    None
                }
            })
        })
}

fn configured_agent_max_tokens(
    provider_config: Option<&hermes_config::LlmProviderConfig>,
) -> Option<u32> {
    if let Ok(raw) = std::env::var("HERMES_MAX_TOKENS") {
        if let Ok(value) = raw.trim().parse::<u32>() {
            if value > 0 {
                return Some(value);
            }
        }
    }
    provider_config.and_then(|cfg| cfg.max_tokens.filter(|value| *value > 0))
}

pub fn build_agent_config(config: &GatewayConfig, model: &str) -> AgentConfig {
    let (resolved_provider, _) = resolve_provider_and_model(config, model);
    let runtime_provider = normalize_runtime_provider_name(resolved_provider.as_str());
    let provider_config = active_llm_provider_config(
        config,
        resolved_provider.as_str(),
        runtime_provider.as_str(),
    );
    let provider_extra_body = provider_config.and_then(|cfg| cfg.extra_body.clone());
    let max_tokens = configured_agent_max_tokens(provider_config);
    let extra_body =
        merge_service_tier_extra_body(provider_extra_body, config.agent.normalized_service_tier());
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

    let retry_cfg = build_retry_config(config);
    let max_delegate_depth = config
        .delegation
        .max_spawn_depth
        .map(|depth| depth.max(1))
        .unwrap_or_else(|| AgentConfig::default().max_delegate_depth);

    AgentConfig {
        max_turns: config.max_turns,
        budget: config.budget.clone(),
        model: model.to_string(),
        system_prompt: config.system_prompt.clone(),
        personality: config.personality.clone(),
        extra_body,
        hermes_home: config.home_dir.clone(),
        provider: Some(resolved_provider),
        stream: config.streaming.enabled,
        max_tokens,
        max_delegate_depth,
        delegation_model: config
            .delegation
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        delegation_provider: config
            .delegation
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        delegation_base_url: config
            .delegation
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        delegation_api_key: config
            .delegation
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
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
                        request_timeout_seconds: cfg.request_timeout_seconds,
                        api_mode: cfg.api_mode.as_deref().and_then(parse_provider_api_mode),
                        command: cfg.command.clone(),
                        args: cfg.args.clone(),
                        oauth_token_url: cfg.oauth_token_url.clone(),
                        oauth_client_id: cfg.oauth_client_id.clone(),
                    },
                )
            })
            .collect(),
        prefill_messages: hermes_config::load_prefill_messages(config),
        retry: retry_cfg,
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

fn merge_service_tier_extra_body(
    extra_body: Option<Value>,
    service_tier: Option<String>,
) -> Option<Value> {
    let Some(service_tier) = service_tier.and_then(|tier| normalize_service_tier(Some(&tier)))
    else {
        return extra_body;
    };
    let mut map = match extra_body {
        Some(Value::Object(map)) => map,
        Some(other) => {
            let mut map = serde_json::Map::new();
            map.insert("extra_body".to_string(), other);
            map
        }
        None => serde_json::Map::new(),
    };
    map.insert("service_tier".to_string(), Value::String(service_tier));
    Some(Value::Object(map))
}

// ---------------------------------------------------------------------------
// Helper: bridge hermes_tools::ToolRegistry → agent_loop::ToolRegistry
// ---------------------------------------------------------------------------

pub fn bridge_tool_registry(tools: &ToolRegistry) -> AgentToolRegistry {
    bridge_tool_registry_excluding(tools, &[])
}

fn bridge_tool_registry_excluding(tools: &ToolRegistry, excluded: &[&str]) -> AgentToolRegistry {
    let mut agent_registry = AgentToolRegistry::new();
    for schema in tools.get_definitions() {
        if excluded.iter().any(|name| schema.name == *name) {
            continue;
        }
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

/// Build a scheduler for long-running CLI runtimes from the active provider and
/// registered tools. This keeps scheduled jobs equivalent to explicit
/// `hermes cron run` instead of completing through the minimal CRUD scheduler.
pub fn build_runtime_cron_scheduler(
    config: &GatewayConfig,
    model: &str,
    data_dir: PathBuf,
    tools: &ToolRegistry,
) -> CronScheduler {
    let persistence = Arc::new(FileJobPersistence::with_dir(data_dir));
    let provider = build_provider(config, model);
    let runner = Arc::new(CronRunner::new(
        provider,
        Arc::new(bridge_tool_registry_excluding(tools, &["cronjob"])),
    ));
    CronScheduler::new(persistence, runner)
}

// ---------------------------------------------------------------------------
// Helper: build LLM provider from config + model string
// ---------------------------------------------------------------------------

const STEPFUN_BASE_URL: &str = "https://api.stepfun.ai/step_plan/v1";
const QWEN_BASE_URL: &str = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1";
const ALIBABA_CODING_PLAN_BASE_URL: &str = "https://coding-intl.dashscope.aliyuncs.com/v1";
const GOOGLE_GEMINI_CLI_BASE_URL: &str = "cloudcode-pa://google";
const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const AI_GATEWAY_BASE_URL: &str = "https://ai-gateway.vercel.sh/v1";
const KIMI_CODING_BASE_URL: &str = provider_profiles::KIMI_CODE_BASE_URL;
const KIMI_LEGACY_BASE_URL: &str = provider_profiles::KIMI_LEGACY_BASE_URL;
const KIMI_CODING_CN_BASE_URL: &str = provider_profiles::KIMI_CN_BASE_URL;
const MINIMAX_CN_BASE_URL: &str = "https://api.minimaxi.com/anthropic";
const NOVITA_BASE_URL: &str = "https://api.novita.ai/openai/v1";
const XAI_BASE_URL: &str = "https://api.x.ai/v1";
const NVIDIA_BASE_URL: &str = "https://integrate.api.nvidia.com/v1";
const COPILOT_BASE_URL: &str = "https://api.githubcopilot.com";
const OPENCODE_GO_BASE_URL: &str = "https://opencode.ai/zen/go/v1";
const OPENCODE_ZEN_BASE_URL: &str = "https://opencode.ai/zen/v1";
const KILOCODE_BASE_URL: &str = "https://api.kilo.ai/api/gateway";
const HUGGINGFACE_BASE_URL: &str = "https://router.huggingface.co/v1";
const GMI_BASE_URL: &str = "https://api.gmi-serving.com/v1";
const XIAOMI_BASE_URL: &str = "https://api.xiaomimimo.com/v1";
const ZAI_BASE_URL: &str = "https://api.z.ai/api/paas/v4";
const ARCEE_BASE_URL: &str = "https://api.arcee.ai/api/v1";
const TENCENT_TOKENHUB_BASE_URL: &str = "https://tokenhub.tencentmaas.com/v1";
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
        "nous_api" | "nousapi" | "nous-portal-api" => "nous-api".to_string(),
        "qwen-cli" | "qwen-portal" => "qwen-oauth".to_string(),
        "gemini-cli" | "gemini-oauth" => "google-gemini-cli".to_string(),
        "google" | "google-gemini" | "google-ai-studio" => "gemini".to_string(),
        "azure" | "azure-ai-foundry" | "azure_ai_foundry" => "azure-foundry".to_string(),
        "step" | "step-plan" => "stepfun".to_string(),
        "moonshot" | "kimi-coding" | "kimi-coding-cn" => "kimi".to_string(),
        "alibaba" | "alibaba-coding-plan" => "qwen".to_string(),
        "minimax-cn" => "minimax".to_string(),
        "novita-ai" | "novitaai" => "novita".to_string(),
        "glm" | "z-ai" | "z_ai" | "zhipu" => "zai".to_string(),
        "aigateway" | "vercel" => "ai-gateway".to_string(),
        "github-copilot" | "github-models" => "copilot".to_string(),
        "github-copilot-acp" | "copilot-acp-agent" => "copilot-acp".to_string(),
        "hf" | "hugging-face" | "huggingface-hub" => "huggingface".to_string(),
        "gmi-cloud" | "gmicloud" => "gmi".to_string(),
        "arcee-ai" | "arceeai" => "arcee".to_string(),
        "mimo" | "xiaomi-mimo" => "xiaomi".to_string(),
        "tencent" | "tokenhub" | "tencent-cloud" | "tencentmaas" => "tencent-tokenhub".to_string(),
        "aws" | "aws-bedrock" | "amazon-bedrock" | "amazon" => "bedrock".to_string(),
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
        "nous-api" | "nous_api" | "nousapi" | "nous-portal-api" => Some(DEFAULT_NOUS_INFERENCE_URL),
        "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => Some(GOOGLE_GEMINI_CLI_BASE_URL),
        "gemini" | "google" | "google-gemini" | "google-ai-studio" => Some(GEMINI_BASE_URL),
        "qwen" | "alibaba" => Some(QWEN_BASE_URL),
        "alibaba-coding-plan" => Some(ALIBABA_CODING_PLAN_BASE_URL),
        "stepfun" | "step" | "step-plan" => Some(STEPFUN_BASE_URL),
        "ai-gateway" | "aigateway" | "vercel" => Some(AI_GATEWAY_BASE_URL),
        "kimi-coding" => Some(KIMI_CODING_BASE_URL),
        "kimi" | "moonshot" => Some(KIMI_LEGACY_BASE_URL),
        "kimi-coding-cn" => Some(KIMI_CODING_CN_BASE_URL),
        "minimax-cn" | "minimax_cn" => Some(MINIMAX_CN_BASE_URL),
        "novita" | "novita-ai" | "novitaai" => Some(NOVITA_BASE_URL),
        "xai" => Some(XAI_BASE_URL),
        "nvidia" => Some(NVIDIA_BASE_URL),
        "copilot" | "github-copilot" | "github-models" => Some(COPILOT_BASE_URL),
        "opencode-go" => Some(OPENCODE_GO_BASE_URL),
        "opencode-zen" | "opencode" => Some(OPENCODE_ZEN_BASE_URL),
        "kilocode" | "kilo" => Some(KILOCODE_BASE_URL),
        "huggingface" | "hf" | "hugging-face" | "huggingface-hub" => Some(HUGGINGFACE_BASE_URL),
        "gmi" | "gmi-cloud" | "gmicloud" => Some(GMI_BASE_URL),
        "xiaomi" | "mimo" | "xiaomi-mimo" => Some(XIAOMI_BASE_URL),
        "zai" | "glm" | "z-ai" | "z_ai" | "zhipu" => Some(ZAI_BASE_URL),
        "arcee" | "arcee-ai" | "arceeai" => Some(ARCEE_BASE_URL),
        "tencent-tokenhub" | "tencent" | "tokenhub" | "tencent-cloud" | "tencentmaas" => {
            Some(TENCENT_TOKENHUB_BASE_URL)
        }
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
        Err(_) => false,
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
    let raw_provider = provider.trim().to_ascii_lowercase();
    let normalized_provider = normalize_runtime_provider_name(raw_provider.as_str());
    let env_var = match raw_provider.as_str() {
        "minimax-cn" | "minimax_cn" => "MINIMAX_CN_BASE_URL",
        _ => match normalized_provider.as_str() {
            "openai" => "OPENAI_BASE_URL",
            "openai-codex" | "codex" => "HERMES_OPENAI_CODEX_BASE_URL",
            "nous-api" => "NOUS_BASE_URL",
            "anthropic" => "ANTHROPIC_BASE_URL",
            "bedrock" => "BEDROCK_BASE_URL",
            "google-gemini-cli" => "HERMES_GEMINI_BASE_URL",
            "gemini" | "google" => "GEMINI_BASE_URL",
            "qwen" => "DASHSCOPE_BASE_URL",
            "qwen-oauth" => "HERMES_QWEN_BASE_URL",
            "stepfun" => "STEPFUN_BASE_URL",
            "ai-gateway" => "AI_GATEWAY_BASE_URL",
            "kimi" => "KIMI_BASE_URL",
            "minimax" => "MINIMAX_BASE_URL",
            "novita" => "NOVITA_BASE_URL",
            "xai" => "XAI_BASE_URL",
            "nvidia" => "NVIDIA_BASE_URL",
            "copilot" => "COPILOT_API_BASE_URL",
            "opencode-go" => "OPENCODE_GO_BASE_URL",
            "opencode-zen" => "OPENCODE_ZEN_BASE_URL",
            "kilocode" => "KILOCODE_BASE_URL",
            "huggingface" => "HF_BASE_URL",
            "gmi" => "GMI_BASE_URL",
            "xiaomi" => "XIAOMI_BASE_URL",
            "zai" => "GLM_BASE_URL",
            "arcee" => "ARCEE_BASE_URL",
            "tencent-tokenhub" => "TOKENHUB_BASE_URL",
            "deepseek" => "DEEPSEEK_BASE_URL",
            "ollama-local" | "ollama" => "OLLAMA_BASE_URL",
            "llama-cpp" | "llama.cpp" | "llamacpp" => "LLAMA_CPP_BASE_URL",
            "vllm" | "ollvm" | "llvm" => "VLLM_BASE_URL",
            "mlx" | "mlx-lm" | "apple-mlx" => "MLX_BASE_URL",
            "apple-ane" | "ane" | "apple-neural-engine" => "APPLE_ANE_BASE_URL",
            "sglang" => "SGLANG_BASE_URL",
            "tgi" | "text-generation-inference" => "TGI_BASE_URL",
            _ => return None,
        },
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
        || runtime_provider == "bedrock"
        || provider_name == "bedrock"
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
    let raw_provider = provider.trim().to_ascii_lowercase();
    if raw_provider == "kimi-coding-cn" {
        return ["KIMI_CN_API_KEY", "KIMI_API_KEY", "MOONSHOT_API_KEY"]
            .iter()
            .find_map(|env_var| {
                std::env::var(env_var)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            });
    }
    if matches!(raw_provider.as_str(), "minimax-cn" | "minimax_cn") {
        return std::env::var("MINIMAX_CN_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("MINIMAX_API_KEY").ok())
            .filter(|s| !s.trim().is_empty());
    }
    let provider = normalize_runtime_provider_name(raw_provider.as_str());
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
        "bedrock" => Some(BEDROCK_AUTH_MARKER.to_string()),
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
        "kimi" | "moonshot" => {
            let env_vars: &[&str] = if raw_provider == "kimi-coding" {
                &["KIMI_CODING_API_KEY", "KIMI_API_KEY", "MOONSHOT_API_KEY"]
            } else {
                &[
                    "KIMI_API_KEY",
                    "KIMI_CODING_API_KEY",
                    "MOONSHOT_API_KEY",
                    "KIMI_CN_API_KEY",
                ]
            };
            env_vars.iter().find_map(|env_var| {
                std::env::var(env_var)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
        }
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
        "novita" => std::env::var("NOVITA_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "nous" | "nous-api" => std::env::var("NOUS_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "copilot" => std::env::var("COPILOT_GITHUB_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("GH_TOKEN").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("GITHUB_TOKEN").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("GITHUB_COPILOT_TOKEN").ok())
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
        "gmi" => std::env::var("GMI_API_KEY")
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
        "tencent-tokenhub" => std::env::var("TOKENHUB_API_KEY")
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
    let model_name = hermes_agent::model_normalize::normalize_model_for_provider(
        model_name.as_str(),
        runtime_provider.as_str(),
    );

    let provider_config =
        active_llm_provider_config(config, provider_name.as_str(), runtime_provider.as_str());
    let request_timeout_seconds = provider_config.and_then(|c| c.request_timeout_seconds);

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
            let mut p = OpenAiProvider::new(&api_key)
                .with_model(model_name.as_str())
                .with_optional_request_timeout_seconds(request_timeout_seconds);
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "openai-codex" | "codex" => Arc::new(openai_codex_provider_with_timeout(
            &api_key,
            model_name.as_str(),
            base_url.as_deref(),
            request_timeout_seconds,
        )),
        "anthropic" => {
            let mut p = AnthropicProvider::new(&api_key)
                .with_model(model_name.as_str())
                .with_optional_request_timeout_seconds(request_timeout_seconds);
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "bedrock" => {
            let mut p = BedrockProvider::new()
                .with_region(resolve_bedrock_region())
                .with_model(model_name.as_str());
            if let Some(url) =
                base_url.or_else(|| Some(bedrock_runtime_base_url(&resolve_bedrock_region())))
            {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "openrouter" => {
            let p = OpenRouterProvider::new(&api_key)
                .with_model(model_name.as_str())
                .with_optional_request_timeout_seconds(request_timeout_seconds);
            Arc::new(p)
        }
        "qwen" | "qwen-oauth" => {
            let mut p = QwenProvider::new(&api_key)
                .with_model(model_name.as_str())
                .with_optional_request_timeout_seconds(request_timeout_seconds);
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "kimi" | "moonshot" => {
            let mut p = KimiProvider::new(&api_key)
                .with_model(model_name.as_str())
                .with_optional_request_timeout_seconds(request_timeout_seconds);
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "minimax" => {
            let mut p = MiniMaxProvider::new(&api_key)
                .with_model(model_name.as_str())
                .with_optional_request_timeout_seconds(request_timeout_seconds);
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "stepfun" => {
            let url = base_url.unwrap_or_else(|| STEPFUN_BASE_URL.to_string());
            Arc::new(
                GenericProvider::new(url, &api_key, model_name.as_str())
                    .with_optional_request_timeout_seconds(request_timeout_seconds)
                    .with_provider_profile(runtime_provider.as_str()),
            )
        }
        "nous" | "nous-api" => {
            let mut p = NousProvider::new(&api_key)
                .with_model(model_name.as_str())
                .with_optional_request_timeout_seconds(request_timeout_seconds);
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "copilot" => {
            let p = CopilotProvider::new(
                base_url.unwrap_or_else(|| COPILOT_BASE_URL.to_string()),
                &api_key,
            )
            .with_model(model_name.as_str())
            .with_optional_request_timeout_seconds(request_timeout_seconds);
            Arc::new(p)
        }
        "ollama-local" | "llama-cpp" | "vllm" | "mlx" | "apple-ane" | "sglang" | "tgi" => {
            let url = base_url.unwrap_or_else(|| "http://127.0.0.1:11434/v1".to_string());
            Arc::new(
                GenericProvider::new(url, &api_key, model_name.as_str())
                    .with_optional_request_timeout_seconds(request_timeout_seconds)
                    .with_provider_profile(runtime_provider.as_str()),
            )
        }
        _ => {
            let url = base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            Arc::new(
                GenericProvider::new(url, &api_key, model_name.as_str())
                    .with_optional_request_timeout_seconds(request_timeout_seconds)
                    .with_provider_profile(runtime_provider.as_str()),
            )
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
