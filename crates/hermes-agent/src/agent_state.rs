//! Consolidated shared mutable state for AgentLoop.
//!
//! Replaces ~20 scattered `Arc<Mutex<>>` fields with one struct behind
//! a single `Arc<Mutex<AgentSharedState>>`. This is the first step toward
//! actorizing the agent loop — once all shared state lives here, switching
//! to an `mpsc`-driven actor is a single abstraction boundary.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use hermes_core::Message;

use crate::agent_config::EvolutionCounters;
use crate::api_messages::ApiMessagesCacheKey;
use crate::cache_diagnostics::PrefixShape;
use crate::fallback::TurnFallbackState;
use crate::session_persistence::SessionFlushCursor;
use crate::session_state::SessionUsageMetrics;
use crate::smart_model_routing::PrimaryRuntime;
use crate::transports::codex_app_server_session::CodexAppServerSession;
use crate::user_interest::SessionPoiBuffer;

/// All shared mutable state for one `AgentLoop` instance.
///
/// Access pattern: `self.state.lock().unwrap().field_name`
pub struct AgentSharedState {
    // === Session lifecycle ===
    /// Python `AIAgent._last_flushed_db_idx` — incremental SQLite session writes.
    pub(crate) session_db_flush: SessionFlushCursor,
    /// Python `_cached_system_prompt` — built once per session, invalidated on compression.
    pub(crate) cached_system_prompt: Option<String>,
    /// Session-scoped token/cost counters (Python `session_*` fields).
    pub session_usage: SessionUsageMetrics,
    /// Compression feasibility warning replayed at turn start (Python `_compression_warning`).
    pub(crate) compression_warning: Option<String>,
    pub(crate) compression_feasibility_checked: bool,

    // === Runtime ===
    /// Effective model/provider for the current turn.
    pub(crate) active_runtime: PrimaryRuntime,
    /// Turn-scoped fallback activation.
    pub(crate) turn_fallback: TurnFallbackState,
    /// Active turn task id (Python `_current_task_id`).
    pub(crate) current_task_id: Option<String>,
    /// Python `_ext_prefetch_cache` — fetched once per turn, injected at API-call time only.
    pub(crate) turn_ext_prefetch_cache: String,
    /// Per-turn cache of assembled API messages (LLM retry fast path).
    pub(crate) turn_api_messages_cache: Option<(ApiMessagesCacheKey, Arc<[Message]>)>,

    // === Interest / POI ===
    pub(crate) interest_synced_user_hashes: HashSet<u64>,
    pub(crate) interest_synced_message_len: usize,
    pub(crate) interest_session_buffer: SessionPoiBuffer,

    // === Plugin / evolution ===
    pub evolution_counters: EvolutionCounters,
    pub(crate) oauth_refresh_backoff: HashMap<String, Instant>,

    // === Provider transport ===
    pub(crate) codex_app_server_session: Option<CodexAppServerSession>,
    /// Last-known Nous `x-ratelimit-*` headers.
    pub(crate) last_nous_rate_limit_headers: Option<HashMap<String, String>>,

    // === Prompt cache diagnostics ===
    /// Prefix shape captured at the start of the previous turn.
    /// Compared against the current turn's shape to explain cache misses.
    pub(crate) last_prefix_shape: Option<PrefixShape>,
    /// Cumulative session-level cache-hit tokens (from provider usage reports).
    pub(crate) session_cache_hit: u64,
    /// Cumulative session-level cache-miss tokens (from provider usage reports).
    pub(crate) session_cache_miss: u64,
}

impl AgentSharedState {
    pub(crate) fn new(
        active_runtime: PrimaryRuntime,
        evolution_counters: EvolutionCounters,
    ) -> Self {
        Self {
            session_db_flush: SessionFlushCursor::new(),
            cached_system_prompt: None,
            session_usage: SessionUsageMetrics::default(),
            compression_warning: None,
            compression_feasibility_checked: false,

            active_runtime,
            turn_fallback: TurnFallbackState::new(),
            current_task_id: None,
            turn_ext_prefetch_cache: String::new(),
            turn_api_messages_cache: None,

            interest_synced_user_hashes: HashSet::new(),
            interest_synced_message_len: 0,
            interest_session_buffer: SessionPoiBuffer::default(),

            evolution_counters,
            oauth_refresh_backoff: HashMap::new(),

            codex_app_server_session: None,
            last_nous_rate_limit_headers: None,

            last_prefix_shape: None,
            session_cache_hit: 0,
            session_cache_miss: 0,
        }
    }
}
