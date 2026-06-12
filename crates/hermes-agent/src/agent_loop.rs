//! Core agent loop engine.
//!
//! The `AgentLoop` orchestrates the autonomous agent cycle:
//! 1. Send messages + tools to the LLM
//! 2. If the LLM responds with tool calls, execute them (in parallel)
//! 3. Append results to conversation history
//! 4. Repeat until the model finishes naturally or the turn budget is exceeded

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use futures::StreamExt;
use hermes_intelligence::get_model_context_length;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use hermes_core::{
    AgentError, AgentResult, LlmProvider, LlmResponse, Message, MessageRole, StreamChunk, ToolCall,
    ToolResult, ToolSchema, UsageStats, separate_text_and_calls,
};

use crate::agent_runtime_helpers;
use crate::api_bridge::CodexProvider;
use crate::auxiliary_builder::{AuxiliaryBuildParams, build_auxiliary_client};
use crate::bedrock::{BedrockProvider, resolve_bedrock_region};
use crate::code_index::CodeIndex;
use crate::compression::{
    CompressorConfig, ContextCompressor, estimate_messages_tokens,
    estimate_request_tokens_for_compression,
};
use crate::context::ContextManager;
use crate::context_files::{load_hermes_context_files, load_workspace_context};
use crate::context_references::preprocess_context_references_async;
use crate::credential_pool::CredentialPool;
use crate::interrupt::InterruptController;
use crate::lsp_context::{LspContextConfig, build_lsp_context_note};
use crate::memory_manager::MemoryManager;
use crate::message_sanitization::{
    build_partial_stream_stub_response, format_partial_stream_tool_call_warning,
    partial_stream_dropped_tool_names, partial_stream_tool_calls_in_flight, sanitize_surrogates,
    should_treat_stop_as_truncated, strip_budget_warnings_from_messages,
};
use crate::plugins::{HookResult, HookType, PluginManager};
use crate::provider::{AnthropicProvider, GenericProvider, OpenAiProvider, OpenRouterProvider};
use crate::providers_extra::{
    CopilotProvider, KimiProvider, MiniMaxProvider, NousProvider, QwenProvider,
};
use crate::replay::{
    RouteLearningState, RouteLearningStats, short_sha256_hex, truncate_hook_preview,
};
use crate::session_persistence::{SessionFlushCursor, SessionPersistence};
use crate::skill_orchestrator::SkillOrchestrator;
pub use crate::smart_model_routing::{ApiMode, CheapModelRouteConfig, SmartModelRoutingConfig};
use crate::smart_model_routing::{
    PrimaryRuntime, ResolveTurnOutcome, ResolvedCheapRuntime, TurnRouteSignature,
    detect_api_mode_for_url, resolve_turn_route,
};
use crate::steer::PendingSteer;
use crate::system_prompt::{
    BACKEND_PROBE_COMMAND, format_probe_output, platform_hint_for, probe_remote_backend_cached,
};
use crate::user_interest::{InterestStore, ingest_user_message, is_poi_synthetic_user_text};
use crate::work_session::{spawn_session_end_pipeline, touch_active_session};
use hermes_intelligence::auxiliary::AuxiliaryClient;

// ---------------------------------------------------------------------------
// ToolRegistry (re-exported from `tool_registry` module)
// ---------------------------------------------------------------------------

pub use crate::agent_config::{
    AgentConfig, ErrorClass, RetryConfig, RuntimeProviderConfig, TurnMetrics,
};
use crate::agent_config::{
    CompactionGovernanceMode, EvolutionCounters, FinalizationSignals, OAuthStoreCredential,
    compaction_governance_mode_runtime, contextlattice_orchestration_script_path,
    has_ssl_transient_phrase, is_copilot_acp_transport, is_stream_not_supported_error,
    is_transient_stream_error, rand_u64_range, should_inject_tool_enforcement_for_model,
};
pub(crate) use crate::agent_state::AgentSharedState;
pub use crate::tool_registry::{ToolEntry, ToolRegistry};

// ---------------------------------------------------------------------------
// Governor (extracted to `governor` module)
// ---------------------------------------------------------------------------

pub(crate) use crate::governor::{
    governor_for_turn, governor_runtime_state, governor_window_size,
    should_apply_turn_reliability_guard, should_trip_tool_loop_guard,
};

// ---------------------------------------------------------------------------
// Replay recorder (extracted to `replay` module)
// ---------------------------------------------------------------------------

pub(crate) use crate::replay::ReplayRecorder;

// ---------------------------------------------------------------------------
// AgentCallbacks (extracted to `agent_callbacks` module)
// ---------------------------------------------------------------------------

pub use crate::agent_callbacks::AgentCallbacks;

// ---------------------------------------------------------------------------
// LoopExit (extracted to `loop_exit` module)
// ---------------------------------------------------------------------------

pub(crate) use crate::loop_exit::LoopExit;

// ---------------------------------------------------------------------------
// Sliding window stats (extracted to `window_stats` module)
// ---------------------------------------------------------------------------

pub(crate) use crate::window_stats::{push_window_f64, push_window_u64};

pub(crate) const CONVERSATIONAL_SUPPORT_GUIDANCE: &str = "# Conversational support protocol\nWhen users share personal stress, emotions, or difficult decisions, start with a brief non-judgmental acknowledgment, ask one clarifying question if context is missing, then offer practical options with trade-offs. Keep factual or technical requests direct and do not force emotional language where it does not fit. Do not present yourself as a therapist or crisis service; when safety risk appears, urge the user to seek immediate professional or emergency help.";
pub(crate) const OAUTH_REFRESH_BACKOFF_SECS: u64 = 60;
const SESSION_OBJECTIVE_PREFIX: &str = "[SESSION_OBJECTIVE] ";
const OBJECTIVE_PATCH_TAG: &str = "PATCH_VERIFIED:";
const OBJECTIVE_ANALYTICS_TAG: &str = "ANALYTICS_VERIFIED:";
const OBJECTIVE_DEEP_AUDIT_TAG: &str = "DEEP_AUDIT_VERIFIED:";
pub(crate) const OBJECTIVE_GUARD_MAX_RETRIES: u32 = 2;
pub(crate) const OBJECTIVE_DEEP_AUDIT_MAX_RETRIES: u32 = 4;
const OBJECTIVE_DEEP_AUDIT_MIN_PATCH_ITEMS: usize = 2;
const OBJECTIVE_DEEP_AUDIT_MIN_UNIQUE_FILES: usize = 5;
const OBJECTIVE_DEEP_AUDIT_MIN_UNIQUE_COMMANDS: usize = 3;
const OBJECTIVE_DEEP_AUDIT_MIN_WORKSTREAMS: usize = 3;
const FINALIZER_EVIDENCE_MAX_RETRIES: u32 = 2;
const FINALIZER_OUTPUT_QUALITY_MAX_RETRIES: u32 = 2;
const FINALIZER_ACTION_EXECUTION_MAX_RETRIES: u32 = 2;

// Python `AIAgent._MEMORY_REVIEW_PROMPT` / `_SKILL_REVIEW_PROMPT` / `_COMBINED_REVIEW_PROMPT` (0.14.0)
const MEMORY_REVIEW_PROMPT: &str = "Review the conversation above and consider saving durable facts \
with the memory tool. Use the correct target — do not mix user identity with \
project/environment notes.\n\n\
**User profile (target='user')** — save ONLY if the user revealed:\n\
1. Persona, role, or stable identity details (not dated trips/events — those are memory)\n\
2. Communication preferences: language, verbosity, tone, formatting\n\
3. Cross-session expectations about how you should interact with them\n\n\
**Agent notes (target='memory')** — save ONLY if you learned stable facts about:\n\
4. Their environment, project layout, toolchain, or repo conventions\n\
5. Durable workspace corrections (not universal communication style)\n\
6. Upcoming events, travel, appointments, or explicit 'don't forget' facts they asked you to remember\n\n\
USER profile must stay about WHO the user is, not WHAT they are working on. \
If nothing is worth saving, just say 'Nothing to save.' and stop.";

const SKILL_REVIEW_PROMPT: &str = "Review the conversation above and update the skill library. Be \
ACTIVE — most sessions produce at least one skill update, even if \
small. A pass that does nothing is a missed learning opportunity, \
not a neutral outcome.\n\n\
Target shape of the library: CLASS-LEVEL skills, each with a rich \
SKILL.md and a `references/` directory for session-specific detail. \
Not a long flat list of narrow one-session-one-skill entries. This \
shapes HOW you update, not WHETHER you update.\n\n\
Signals to look for (any one of these warrants action):\n\
  • User corrected your style, tone, format, legibility, or \
verbosity. Frustration signals like 'stop doing X', 'this is too \
verbose', 'don't format like this', 'why are you explaining', \
'just give me the answer', 'you always do Y and I hate it', or an \
explicit 'remember this' are FIRST-CLASS skill signals, not just \
memory signals. Update the relevant skill(s) to embed the \
preference so the next session starts already knowing.\n\
  • User corrected your workflow, approach, or sequence of steps. \
Encode the correction as a pitfall or explicit step in the skill \
that governs that class of task.\n\
  • Non-trivial technique, fix, workaround, debugging path, or \
tool-usage pattern emerged that a future session would benefit \
from. Capture it.\n\
  • A skill that got loaded or consulted this session turned out \
to be wrong, missing a step, or outdated. Patch it NOW.\n\n\
Preference order — prefer the earliest action that fits, but do \
pick one when a signal above fired:\n\
  1. UPDATE A CURRENTLY-LOADED SKILL. Look back through the \
conversation for skills the user loaded via /skill-name or you \
read via skill_view. If any of them covers the territory of the \
new learning, PATCH that one first. It is the skill that was in \
play, so it's the right one to extend.\n\
  2. UPDATE AN EXISTING UMBRELLA (via skills_list + skill_view). \
If no loaded skill fits but an existing class-level skill does, \
patch it. Add a subsection, a pitfall, or broaden a trigger.\n\
  3. ADD A SUPPORT FILE under an existing umbrella. Skills can be \
packaged with three kinds of support files — use the right \
directory per kind:\n\
     • `references/<topic>.md` — session-specific detail (error \
transcripts, reproduction recipes, provider quirks) AND \
condensed knowledge banks: quoted research, API docs, external \
authoritative excerpts, or domain notes you found while working \
on the problem. Write it concise and for the value of the task, \
not as a full mirror of upstream docs.\n\
     • `templates/<name>.<ext>` — starter files meant to be \
copied and modified (boilerplate configs, scaffolding, a \
known-good example the agent can `reproduce with modifications`).\n\
     • `scripts/<name>.<ext>` — statically re-runnable actions \
the skill can invoke directly (verification scripts, fixture \
generators, deterministic probes, anything the agent should run \
rather than hand-type each time).\n\
     Add support files via skill_manage action=write_file with \
file_path starting 'references/', 'templates/', or 'scripts/'. \
The umbrella's SKILL.md should gain a one-line pointer to any \
new support file so future agents know it exists.\n\
  4. CREATE A NEW CLASS-LEVEL UMBRELLA SKILL when no existing \
skill covers the class. The name MUST be at the class level. \
The name MUST NOT be a specific PR number, error string, feature \
codename, library-alone name, or 'fix-X / debug-Y / audit-Z-today' \
session artifact. If the proposed name only makes sense for \
today's task, it's wrong — fall back to (1), (2), or (3).\n\n\
User-preference embedding (important): universal style/format preferences \
that apply across all tasks → memory target='user'. Task-specific workflow \
or approach corrections → the relevant SKILL.md body. Environment and \
project facts → memory target='memory'. Skills capture 'how to do this \
class of task'; USER profile captures 'who the user is'. When they \
complain about how you handled a specific task, the skill that governs \
that task needs to carry the lesson.\n\n\
If you notice two existing skills that overlap, note it in your \
reply — the background curator handles consolidation at scale.\n\n\
Protected skills (DO NOT edit these):\n\
  • Bundled skills (shipped with Hermes, e.g. 'hermes-agent').\n\
  • Hub-installed skills (installed via 'hermes skills install').\n\
  • Pinned skills (marked via 'hermes curator pin').\n\
If the only skills that need updating are protected, say\n\
'Nothing to save.' and stop.\n\n\
Do NOT capture (these become persistent self-imposed constraints \
that bite you later when the environment changes):\n\
  • Environment-dependent failures: missing binaries, fresh-install \
errors, post-migration path mismatches, 'command not found', \
unconfigured credentials, uninstalled packages. The user can fix \
these — they are not durable rules.\n\
  • Negative claims about tools or features ('browser tools do not \
work', 'X tool is broken', 'cannot use Y from execute_code'). These \
harden into refusals the agent cites against itself for months \
after the actual problem was fixed.\n\
  • Session-specific transient errors that resolved before the \
conversation ended. If retrying worked, the lesson is the retry \
pattern, not the original failure.\n\
  • One-off task narratives. A user asking 'summarize today's \
market' or 'analyze this PR' is not a class of work that warrants \
a skill.\n\n\
If a tool failed because of setup state, capture the FIX (install \
command, config step, env var to set) under an existing setup or \
troubleshooting skill — never 'this tool does not work' as a \
standalone constraint.\n\n\
'Nothing to save.' is a real option but should NOT be the \
default. If the session ran smoothly with no corrections and \
produced no new technique, just say 'Nothing to save.' and stop. \
Otherwise, act.";

const COMBINED_REVIEW_PROMPT: &str = "Review the conversation above and update two things:\n\n\
**Memory** — use the memory tool with the correct target:\n\
• target='user': persona, communication preferences, and cross-session \
expectations about how you should interact with them (WHO they are).\n\
• target='memory': stable environment, project, toolchain, or repo \
conventions you learned (WHAT they work on — not live system state).\n\n\
**Skills**: how to do this class of task. Be ACTIVE — most \
sessions produce at least one skill update. A pass that does \
nothing is a missed learning opportunity, not a neutral outcome.\n\n\
Target shape of the skill library: CLASS-LEVEL skills with a rich \
SKILL.md and a `references/` directory for session-specific detail. \
Not a long flat list of narrow one-session-one-skill entries.\n\n\
Signals that warrant a skill update (any one is enough):\n\
  • User corrected your style, tone, format, legibility, \
verbosity, or approach. Frustration is a FIRST-CLASS skill \
signal, not just a memory signal. 'stop doing X', 'don't format \
like this', 'I hate when you Y' — embed the lesson in the skill \
that governs that task so the next session starts fixed.\n\
  • Non-trivial technique, fix, workaround, or debugging path \
emerged.\n\
  • A skill that was loaded or consulted turned out wrong, \
missing, or outdated — patch it now.\n\n\
Preference order for skills — pick the earliest that fits:\n\
  1. UPDATE A CURRENTLY-LOADED SKILL. Check what skills were \
loaded via /skill-name or skill_view in the conversation. If one \
of them covers the learning, PATCH it first. It was in play; \
it's the right place.\n\
  2. UPDATE AN EXISTING UMBRELLA (skills_list + skill_view to \
find the right one). Patch it.\n\
  3. ADD A SUPPORT FILE under an existing umbrella via \
skill_manage action=write_file. Three kinds: \
`references/<topic>.md` for session-specific detail OR condensed \
knowledge banks (quoted research, API docs excerpts, domain \
notes) written concise and task-focused; `templates/<name>.<ext>` \
for starter files meant to be copied and modified; \
`scripts/<name>.<ext>` for statically re-runnable actions \
(verification, fixture generators, probes). Add a one-line \
pointer in SKILL.md so future agents find them.\n\
  4. CREATE A NEW CLASS-LEVEL UMBRELLA when nothing exists. \
Name at the class level — NOT a PR number, error string, \
codename, library-alone name, or 'fix-X / debug-Y' session \
artifact. If the name only fits today's task, fall back to (1), \
(2), or (3).\n\n\
User-preference embedding: universal style/format preferences → memory \
target='user'. Task-specific workflow corrections → the relevant skill. \
Environment/project facts → memory target='memory'. When the user complains \
about how you handled a specific task, update the skill that governs that \
task — USER profile alone is not enough for task-class lessons.\n\n\
If you notice overlapping existing skills, mention it — the \
background curator handles consolidation.\n\n\
Protected skills (DO NOT edit these):\n\
  • Bundled skills (shipped with Hermes, e.g. 'hermes-agent').\n\
  • Hub-installed skills (installed via 'hermes skills install').\n\
  • Pinned skills (marked via 'hermes curator pin').\n\
If the only skills that need updating are protected, say\n\
'Nothing to save.' and stop.\n\n\
Do NOT capture as skills (these become persistent self-imposed \
constraints that bite you later when the environment changes):\n\
  • Environment-dependent failures: missing binaries, fresh-install \
errors, post-migration path mismatches, 'command not found', \
unconfigured credentials, uninstalled packages. The user can fix \
these — they are not durable rules.\n\
  • Negative claims about tools or features ('browser tools do not \
work', 'X tool is broken', 'cannot use Y from execute_code'). These \
harden into refusals the agent cites against itself for months \
after the actual problem was fixed.\n\
  • Session-specific transient errors that resolved before the \
conversation ended. If retrying worked, the lesson is the retry \
pattern, not the original failure.\n\
  • One-off task narratives. A user asking 'summarize today's \
market' or 'analyze this PR' is not a class of work that warrants \
a skill.\n\n\
If a tool failed because of setup state, capture the FIX (install \
command, config step, env var to set) under an existing setup or \
troubleshooting skill — never 'this tool does not work' as a \
standalone constraint.\n\n\
Act on whichever of the two dimensions has real signal. If \
genuinely nothing stands out on either, say 'Nothing to save.' \
and stop — but don't reach for that conclusion as a default.";

pub(crate) const CONTEXTLATTICE_OPERATIONAL_GUIDANCE: &str = "# ContextLattice operational guidance\nWhen a user asks to confirm, connect, verify, or harden ContextLattice integration, do not answer from assumptions. First check local integration instructions when present (env `HERMES_CONTEXTLATTICE_INSTRUCTIONS_PATH`, or local `scripts/agent_orchestration.py` in the workspace, typically `/Users/sheawinkler/Documents/Projects/scripts/agent_orchestration.py`). Then attempt ContextLattice tool calls: use `contextlattice_search` for a direct probe and `contextlattice_context_pack` when broader grounding is needed. If a call fails, report the concrete error and provide the exact remediation steps. Never run shell command `contextlattice` for this workflow; use the ContextLattice tools directly. Do not claim lack of access before attempting at least one ContextLattice tool call in the current turn.";

fn unlimited_turns_enabled() -> bool {
    std::env::var("HERMES_MAX_TURNS_UNLIMITED")
        .ok()
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

pub(crate) fn effective_max_turns(config_max_turns: u32) -> Option<u32> {
    if unlimited_turns_enabled() || config_max_turns == 0 {
        None
    } else {
        Some(config_max_turns)
    }
}

fn normalize_delegate_depth(value: u32) -> u32 {
    value.max(1)
}

fn parse_delegate_depth(raw: &str) -> Option<u32> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse().ok().map(normalize_delegate_depth)
}

fn delegation_spawning_paused() -> bool {
    std::env::var("HERMES_DELEGATION_PAUSED")
        .ok()
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// AgentLoop
// ---------------------------------------------------------------------------

pub(crate) fn maybe_nous_401_diagnostic(
    provider_hint: &str,
    err: &str,
    hermes_home: Option<&str>,
) -> Option<String> {
    let provider = provider_hint.trim().to_ascii_lowercase();
    if !provider.starts_with("nous") {
        return None;
    }
    let lower = err.to_ascii_lowercase();
    let is_auth_401 =
        lower.contains("401") || lower.contains("unauthorized") || lower.contains("authentication");
    if !is_auth_401 {
        return None;
    }

    let response = err.replace('\n', " ");
    let response_snippet = if response.len() > 200 {
        format!("{}...", &response[..200])
    } else {
        response
    };
    let auth_json = hermes_home
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(hermes_config::PRIMARY_HOME_DIR)
        })
        .join("auth.json");

    Some(format!(
        "Nous 401 - Portal authentication failed.\n\
         Response: {response_snippet}\n\
         Most likely: Portal OAuth expired, account out of credits, or agent key revoked.\n\
         Troubleshooting:\n\
           - Re-authenticate: hermes auth login nous\n\
           - Check credits / billing: https://portal.nousresearch.com\n\
           - Verify stored credentials: {}\n\
           - Switch providers temporarily: /model <model> --provider openrouter",
        auth_json.display()
    ))
}

pub(crate) fn classify_error(err: &str) -> ErrorClass {
    let lower = err.to_lowercase();
    let model_not_found = lower.contains("model not found")
        || lower.contains("invalid model")
        || lower.contains("no such model")
        || lower.contains("unknown model");
    let openrouter_privacy_guardrail =
        lower.contains("privacy guardrail") || lower.contains("openrouter privacy");

    if lower.contains("rate limit") || lower.contains("429") || lower.contains("too many") {
        ErrorClass::RateLimit
    } else if lower.contains("404") || lower.contains("not found") {
        if model_not_found || openrouter_privacy_guardrail {
            ErrorClass::Fatal
        } else {
            ErrorClass::Retryable
        }
    } else if lower.contains("context length")
        || lower.contains("maximum context")
        || lower.contains("token limit")
        || lower.contains("context_length_exceeded")
    {
        ErrorClass::ContextOverflow
    } else if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("authentication")
    {
        ErrorClass::Auth
    } else if has_ssl_transient_phrase(&lower) {
        ErrorClass::Retryable
    } else if lower.contains("500")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("timeout")
        || lower.contains("connection")
        || lower.contains("disconnected")
        || lower.contains("overloaded")
    {
        ErrorClass::Retryable
    } else {
        ErrorClass::Fatal
    }
}

pub(crate) fn is_tool_payload_validation_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    (lower.contains("invalid input") && lower.contains("function"))
        || lower.contains("provider returned error")
            && (lower.contains("request is not valid") || lower.contains("check the model name"))
        || (lower.contains("no choices in response") || lower.contains("empty choices array"))
            && (lower.contains("request is not valid")
                || lower.contains("valid payload")
                || lower.contains("provider returned error")
                || lower.contains("tool"))
        || lower.contains("unprocessable entity") && lower.contains("valid payload")
        || lower.contains("tools") && lower.contains("invalid")
}

pub(crate) fn preferred_tool_payload_fallback_model(
    provider_hint: &str,
    model_name: &str,
) -> Option<String> {
    let provider = provider_hint.trim().to_ascii_lowercase();
    let model = model_name.trim().to_ascii_lowercase();
    let nous_openai_route = provider == "nous" && model.starts_with("openai/");
    if !nous_openai_route {
        return None;
    }
    if let Ok(override_model) = std::env::var("HERMES_TOOL_PAYLOAD_FALLBACK_MODEL") {
        let trimmed = override_model.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    Some("nousresearch/hermes-4-70b".to_string())
}

/// Compute jittered exponential backoff delay.
pub(crate) fn jittered_backoff(attempt: u32, base_ms: u64, max_ms: u64) -> Duration {
    let exp = base_ms.saturating_mul(1u64 << attempt.min(10));
    let capped = exp.min(max_ms);
    let jitter = capped / 4;
    let delay = capped.saturating_sub(jitter / 2) + (rand_u64_range(0, jitter.max(1)));
    Duration::from_millis(delay)
}

/// Result of collecting one streaming completion (may end with user interrupt).
pub(crate) enum StreamCollectOutcome {
    Complete(LlmResponse),
    Interrupted(LlmResponse),
}

/// Build a PrimaryRuntime from agent config (no runtime snapshot).
/// Extracted as a free function so it can be called from `runtime_provider`.
pub(crate) fn primary_runtime_from_config(config: &AgentConfig) -> PrimaryRuntime {
    let provider = config
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let base_url = provider
        .as_ref()
        .and_then(|p| config.runtime_providers.get(p))
        .and_then(|c| {
            c.base_url
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
    let mut command = config
        .acp_command
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let mut args: Vec<String> = config
        .acp_args
        .iter()
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .collect();
    if let Some(provider) = provider.as_deref() {
        if let Some(cfg) = config.runtime_providers.get(provider) {
            if let Some(cmd) = cfg
                .command
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                command = Some(cmd.to_string());
            }
            if !cfg.args.is_empty() {
                args = cfg
                    .args
                    .iter()
                    .map(|a| a.trim().to_string())
                    .filter(|a| !a.is_empty())
                    .collect();
            }
        }
    }
    let provider_key = provider.as_deref().unwrap_or("");
    let api_mode = crate::smart_model_routing::maybe_apply_codex_app_server_runtime(
        provider_key,
        config.api_mode.clone(),
        config.openai_runtime.as_deref(),
    );
    PrimaryRuntime {
        model: config.model.clone(),
        provider,
        base_url,
        api_mode,
        command,
        args,
        credential_pool: None,
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RepoReviewBudgetState {
    pub(crate) last_discovery_signature: Option<String>,
    pub(crate) repeat_streak: u32,
    pub(crate) low_signal_streak: u32,
    pub(crate) last_signal_score: f64,
}

fn web_tool_budget_max_calls() -> u32 {
    std::env::var("HERMES_WEB_TOOL_BUDGET_MAX_CALLS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(3)
}

fn web_search_budget_max_calls() -> u32 {
    std::env::var("HERMES_WEB_SEARCH_BUDGET_MAX_CALLS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(2)
}

pub(crate) fn web_tool_budget_max_consecutive_errors() -> u32 {
    std::env::var("HERMES_WEB_TOOL_BUDGET_MAX_CONSECUTIVE_ERRORS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(2)
}

pub(crate) fn is_budgeted_web_tool(name: &str) -> bool {
    matches!(name, "web_search" | "web_extract" | "browser_navigate")
}

pub(crate) fn web_tool_budget_user_notice(tool_name: &str, blocked_by_errors: bool) -> String {
    match tool_name {
        "web_search" => "网络检索次数已达上限，将基于已有信息直接回复。".to_string(),
        "web_extract" | "browser_navigate" if blocked_by_errors => {
            "网页读取多次失败，将基于已有信息直接回复。".to_string()
        }
        "web_extract" | "browser_navigate" => {
            "网页抓取次数已达上限，将基于已有信息直接回复。".to_string()
        }
        _ => format!("工具 {tool_name} 调用受限，将基于已有信息直接回复。"),
    }
}

fn tool_progress_enabled() -> bool {
    std::env::var("HERMES_TOOL_PROGRESS_ENABLED")
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off"
            )
        })
        .unwrap_or(true)
}

fn tool_progress_initial_delay_ms() -> u64 {
    std::env::var("HERMES_TOOL_PROGRESS_INITIAL_DELAY_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(12_000)
}

fn tool_progress_interval_ms() -> u64 {
    std::env::var("HERMES_TOOL_PROGRESS_INTERVAL_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(20_000)
}

fn format_tool_progress_message(turn: u32, tool_names: &[String], pulse: u32) -> String {
    let joined = tool_names.join(", ");
    let webish = tool_names.iter().any(|name| {
        matches!(
            name.as_str(),
            "web_search" | "web_extract" | "browser_navigate"
        )
    });
    let base = if webish {
        format!("正在检索网络数据（第 {turn} 步，工具 {joined}）")
    } else if tool_names.len() > 1 {
        format!(
            "正在执行工具（第 {turn} 步，{} 个调用：{joined}）",
            tool_names.len()
        )
    } else if let Some(name) = tool_names.first() {
        format!("正在执行 {name}（第 {turn} 步）")
    } else {
        format!("处理中，请稍候（第 {turn} 步）")
    };
    if pulse > 1 {
        format!("{base}…（仍在进行）")
    } else {
        format!("{base}…")
    }
}

pub(crate) fn summarize_tool_failure_for_user(tool_name: &str, error: &str) -> Option<String> {
    let err = error.to_ascii_lowercase();
    match tool_name {
        "web_extract"
            if err.contains("403") || err.contains("401") || err.contains("blocks automated") =>
        {
            Some(
                "该网页拒绝自动抓取，将优先基于已有搜索结果摘要回答，或换一条检索词继续搜索。"
                    .to_string(),
            )
        }
        "browser_navigate"
            if err.contains("cdp not reachable")
                || err.contains("auto-start")
                || err.contains("chrome executable not found") =>
        {
            Some("正在启动浏览器读取页面，请稍候…".to_string())
        }
        "browser_navigate" if err.contains("did not become ready") => {
            Some("浏览器启动较慢，仍在等待…".to_string())
        }
        "web_search" if err.contains("timed out") || err.contains("failed after trying") => {
            Some("网络搜索较慢，正在尝试其他搜索引擎…".to_string())
        }
        _ => None,
    }
}

pub(crate) struct ToolProgressWatchdog {
    handle: Option<tokio::task::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl ToolProgressWatchdog {
    pub(crate) fn start(
        status_callback: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
        turn: u32,
        tool_names: Vec<String>,
    ) -> Self {
        if !tool_progress_enabled() || status_callback.is_none() || tool_names.is_empty() {
            return Self {
                handle: None,
                stop: Arc::new(AtomicBool::new(true)),
            };
        }
        let cb = status_callback.expect("checked above");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_worker = stop.clone();
        let handle = tokio::spawn(async move {
            let initial = Duration::from_millis(tool_progress_initial_delay_ms());
            let interval = Duration::from_millis(tool_progress_interval_ms());
            tokio::time::sleep(initial).await;
            if stop_worker.load(Ordering::Acquire) {
                return;
            }
            let mut pulse = 0u32;
            loop {
                if stop_worker.load(Ordering::Acquire) {
                    break;
                }
                pulse = pulse.saturating_add(1);
                let msg = format_tool_progress_message(turn, &tool_names, pulse);
                cb("tool_progress", &msg);
                tracing::info!(
                    turn = turn,
                    pulse = pulse,
                    tools = %tool_names.join(","),
                    "agent tool progress notice"
                );
                tokio::time::sleep(interval).await;
            }
        });
        Self {
            handle: Some(handle),
            stop,
        }
    }
}

impl Drop for ToolProgressWatchdog {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

pub(crate) fn apply_web_tool_budget(
    tool_calls: &mut Vec<ToolCall>,
    web_tool_calls_used: u32,
    web_search_calls_used: u32,
    consecutive_web_error_turns: u32,
    turn: u32,
) -> Vec<(String, ToolResult)> {
    let mut blocked_results: Vec<(String, ToolResult)> = Vec::new();
    let max_calls = web_tool_budget_max_calls();
    let max_search_calls = web_search_budget_max_calls();
    let max_consecutive_errors = web_tool_budget_max_consecutive_errors();
    let mut remaining = max_calls.saturating_sub(web_tool_calls_used);
    let blocked_by_errors = consecutive_web_error_turns >= max_consecutive_errors;
    let mut kept: Vec<ToolCall> = Vec::with_capacity(tool_calls.len());
    for tc in tool_calls.drain(..) {
        if !is_budgeted_web_tool(&tc.function.name) {
            kept.push(tc);
            continue;
        }
        let search_cap_hit =
            tc.function.name == "web_search" && web_search_calls_used >= max_search_calls;
        let block = blocked_by_errors || remaining == 0 || search_cap_hit;
        if block {
            let reason = if search_cap_hit {
                format!(
                    "Web search budget exceeded on turn {}: blocked '{}' after {} web_search call(s).",
                    turn, tc.function.name, web_search_calls_used
                )
            } else if blocked_by_errors {
                format!(
                    "Web tool budget guard tripped on turn {}: blocked '{}' after {} consecutive web-tool error turn(s).",
                    turn, tc.function.name, consecutive_web_error_turns
                )
            } else {
                format!(
                    "Web tool budget exceeded on turn {}: blocked '{}' after {} web-tool call(s).",
                    turn, tc.function.name, web_tool_calls_used
                )
            };
            blocked_results.push((tc.function.name.clone(), ToolResult::err(tc.id, reason)));
            continue;
        }
        remaining = remaining.saturating_sub(1);
        kept.push(tc);
    }
    *tool_calls = kept;
    blocked_results
}

pub(crate) fn looks_like_tool_error_output(output: &str) -> bool {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return false;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(obj) = value.as_object() {
            if let Some(err) = obj.get("error") {
                if !err.is_null() {
                    return true;
                }
            }
            if let Some(success) = obj.get("success").and_then(|v| v.as_bool()) {
                if !success {
                    return true;
                }
            }
            if let Some(status) = obj.get("status").and_then(|v| v.as_str()) {
                if status.eq_ignore_ascii_case("error") || status.eq_ignore_ascii_case("failed") {
                    return true;
                }
            }
        }
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("error:")
        || lower.contains("invalid tool parameters")
        || lower.contains("missing '")
}

/// The main agent loop.
///
/// Owns the configuration, a tool registry, and an LLM provider.
/// Call `run()` or `run_stream()` to begin an autonomous loop.
pub struct AgentLoop {
    pub(crate) config_runtime: std::sync::RwLock<Arc<AgentConfig>>,
    pub tool_registry: Arc<ToolRegistry>,
    pub llm_provider: Arc<dyn LlmProvider>,
    pub interrupt: InterruptController,
    /// Optional memory manager for prefetch/sync/tool routing.
    pub memory_manager: Option<Arc<std::sync::Mutex<MemoryManager>>>,
    /// Optional session search backend for proactive recall (Recall Planner).
    recall_backend: Option<Arc<dyn hermes_tools::SessionSearchBackend>>,
    /// Local POI store (works even when `skip_memory` is true).
    interest_store: Option<Arc<Mutex<InterestStore>>>,
    /// Consolidated shared mutable state (replaces ~20 scattered Arc<Mutex<>> fields).
    pub state: Arc<Mutex<AgentSharedState>>,
    /// Optional plugin manager for lifecycle hooks.
    pub plugin_manager: Option<Arc<std::sync::Mutex<PluginManager>>>,
    /// Callbacks for progress reporting.
    pub callbacks: Arc<AgentCallbacks>,
    /// Sub-agent delegation depth (0 = root).
    pub delegate_depth: u32,
    /// Primary LLM credential pool (Python `primary["credential_pool"]` / runtime pool).
    pub primary_credential_pool: Option<Arc<CredentialPool>>,
    /// Optional in-process sub-agent orchestrator. When set, `delegate_task`
    /// tool calls are executed by the orchestrator (spawn/timeout/cancel/
    /// lineage) instead of simply returning a signal envelope.
    pub(crate) sub_agent_orchestrator:
        Option<Arc<crate::sub_agent_orchestrator::SubAgentOrchestrator>>,
    /// Always-on workspace code index + repo-map source.
    code_index: Option<Arc<Mutex<CodeIndex>>>,
    /// LSP-style context injection controls.
    lsp_context: LspContextConfig,
    /// Rolling per-route performance state for online smart-routing adaptation.
    pub(crate) route_learning: Arc<Mutex<HashMap<String, RouteLearningStats>>>,
    /// Frozen primary runtime at session start (Python `_primary_runtime`).
    pub(crate) stored_primary_runtime: PrimaryRuntime,
    /// When set, tool calls use this async path instead of sync `ToolRegistry` handlers
    /// (avoids `block_in_place` + `block_on` from inside `JoinSet` tasks on the gateway).
    pub(crate) async_tool_dispatch: Option<AsyncToolDispatch>,
    /// Mid-run `/steer` queue (Python `_pending_steer`).
    pub(crate) pending_steer: PendingSteer,
    /// Python `agent.context_compressor.ContextCompressor` (LLM summary + boundary alignment).
    pub(crate) context_compressor: Arc<tokio::sync::Mutex<ContextCompressor>>,
    /// Reused SQLite persistence handle for this agent (Python `SessionDB._conn` parity).
    shared_session_persistence: std::sync::OnceLock<Arc<SessionPersistence>>,
    /// Per-turn cache for OpenAI-compat provider message/tool JSON serialization.
    pub(crate) provider_serialize_cache:
        Arc<crate::provider_serialize_cache::ProviderSerializeCache>,
    /// Python `_disable_streaming` — set after "stream not supported" for the rest of the session.
    pub(crate) disable_streaming: Arc<AtomicBool>,
    /// Per-turn vision capability (Python `_vision_supported`; reset each `prepare_turn`).
    pub(crate) vision_supported: Arc<std::sync::atomic::AtomicBool>,
    /// Plan-then-execute phase (read-only planning → approval → write execution).
    plan_phase: Arc<Mutex<hermes_tools::PlanPhase>>,
    /// Pending structured plan awaiting user approval.
    pending_plan: Arc<Mutex<Option<String>>>,
    /// Live `hermes_tools` registry to sync plan phase into dispatch gates.
    synced_tools_registry: Option<Arc<hermes_tools::ToolRegistry>>,
}

/// Async tool execution hook (gateway: `hermes_tools::ToolRegistry::dispatch_async`).
pub type AsyncToolDispatch = Arc<
    dyn Fn(
            String,
            Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<String, hermes_core::ToolError>> + Send>,
        > + Send
        + Sync,
>;

#[derive(Debug, Clone)]
pub(crate) struct TurnRuntimeRoute {
    pub(crate) model: String,
    pub(crate) provider: Option<String>,
    pub(crate) base_url: Option<String>,
    pub(crate) api_key_env: Option<String>,
    pub(crate) api_mode: Option<ApiMode>,
    pub(crate) command: Option<String>,
    pub(crate) args: Vec<String>,
    pub(crate) credential_pool: Option<Arc<CredentialPool>>,
    /// When true (default), merge with [`AgentLoop::primary_credential_pool`] if route pool is unset.
    pub(crate) credential_pool_fallback: bool,
    pub(crate) route_label: Option<String>,
    pub(crate) routing_reason: Option<String>,
    pub(crate) signature: TurnRouteSignature,
}

fn runtime_llm_providers_map(
    config: &AgentConfig,
) -> HashMap<String, hermes_config::config::LlmProviderConfig> {
    config
        .runtime_providers
        .iter()
        .map(|(name, rp)| {
            (
                name.clone(),
                hermes_config::config::LlmProviderConfig {
                    api_key: rp.api_key.clone(),
                    api_key_env: rp.api_key_env.clone(),
                    base_url: rp.base_url.clone(),
                    command: rp.command.clone(),
                    args: rp.args.clone(),
                    ..Default::default()
                },
            )
        })
        .collect()
}

pub(crate) fn try_build_auxiliary_arc_for_config(
    config: &AgentConfig,
) -> Option<Arc<AuxiliaryClient>> {
    let gateway_cfg = hermes_config::load_config(None).unwrap_or_default();
    let aux_cfg = crate::auxiliary_builder::auxiliary_config_from_gateway(&gateway_cfg);
    let (primary_provider, primary_model) =
        crate::auxiliary_builder::auxiliary_primary_runtime_from_agent_config(config);
    let (auxiliary, summary) = build_auxiliary_client(AuxiliaryBuildParams {
        config: aux_cfg,
        primary_provider,
        primary_model,
        llm_providers: runtime_llm_providers_map(config),
    });
    if auxiliary.chain_len() == 0 {
        tracing::warn!(
            registered = ?summary.registered,
            skipped = ?summary.skipped,
            "interest: no auxiliary LLM provider available — set llm.<provider>.api_key in config.yaml, auxiliary.interest.* override, or OPENROUTER_API_KEY"
        );
        return None;
    }
    Some(Arc::new(auxiliary))
}

pub(crate) fn build_auxiliary_arc_for_config(config: &AgentConfig) -> Arc<AuxiliaryClient> {
    try_build_auxiliary_arc_for_config(config).unwrap_or_else(|| {
        let gateway_cfg = hermes_config::load_config(None).unwrap_or_default();
        let aux_cfg = crate::auxiliary_builder::auxiliary_config_from_gateway(&gateway_cfg);
        let (primary_provider, primary_model) =
            crate::auxiliary_builder::auxiliary_primary_runtime_from_agent_config(config);
        Arc::new(
            build_auxiliary_client(AuxiliaryBuildParams {
                config: aux_cfg,
                primary_provider,
                primary_model,
                llm_providers: runtime_llm_providers_map(config),
            })
            .0,
        )
    })
}

fn build_context_compressor_for_config(
    config: &AgentConfig,
) -> Arc<tokio::sync::Mutex<ContextCompressor>> {
    let compressor_config = CompressorConfig {
        context_length: get_model_context_length(&config.model),
        quiet_mode: config.quiet_mode,
        ..CompressorConfig::default()
    };
    Arc::new(tokio::sync::Mutex::new(ContextCompressor::new(
        compressor_config,
        build_auxiliary_arc_for_config(config),
    )))
}

impl AgentLoop {
    /// Populate [`AgentConfig::stored_system_prompt`] from SQLite (`sessions.system_prompt`) for session continuation.
    ///
    /// Call before [`AgentLoop::run`] when resuming a gateway/CLI session so Anthropic prefix cache matches Python.
    pub fn hydrate_stored_system_prompt_from_hermes_home(
        config: &mut AgentConfig,
        hermes_home: &std::path::Path,
    ) -> Result<(), AgentError> {
        let Some(ref sid) = config.session_id else {
            return Ok(());
        };
        if sid.trim().is_empty() {
            return Ok(());
        }
        let sp = crate::session_persistence::SessionPersistence::new(hermes_home);
        sp.ensure_db()?;
        if let Some(prompt) = sp.get_system_prompt(sid)? {
            config.stored_system_prompt = Some(prompt);
        }
        Ok(())
    }

    /// Build [`AgentResult`] messages for return / persistence (applies `persist_user_message` override).
    fn messages_for_persisted_result(
        &self,
        ctx: &ContextManager,
        persist_user_idx: Option<usize>,
        prefill_range: Option<Range<usize>>,
    ) -> Vec<Message> {
        let mut msgs = ctx.get_messages().to_vec();
        if let (Some(idx), Some(override_text)) = (
            persist_user_idx,
            self.config().persist_user_message.as_deref(),
        ) {
            if let Some(msg) = msgs.get_mut(idx) {
                if msg.role == MessageRole::User {
                    msg.content = Some(override_text.to_string());
                }
            }
        }
        if let Some(range) = prefill_range {
            if range.start <= range.end && range.end <= msgs.len() {
                msgs.drain(range);
            }
        }
        msgs
    }

    pub(crate) fn graceful_interrupt_result(
        &self,
        ctx: &ContextManager,
        total_turns: u32,
        tool_errors: Vec<hermes_core::ToolErrorRecord>,
        accumulated_usage: Option<UsageStats>,
        session_cost_usd: f64,
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
        prefill_range: Option<Range<usize>>,
        api_calls: u32,
    ) -> AgentResult {
        self.pending_steer.clear();
        crate::hooks::turn_end_plugin_hooks(
            self,
            ctx.get_messages(),
            false,
            true,
            total_turns,
            session_started_hooks_fired,
        );
        self.seal_loop_result(
            ctx,
            persist_user_idx,
            prefill_range,
            LoopExit::base("interrupted_by_user", api_calls, false, false, false, true),
            total_turns,
            tool_errors,
            accumulated_usage,
            session_cost_usd,
            session_started_hooks_fired,
        )
    }

    /// Attach Python `run_conversation` return-dict telemetry to [`AgentResult`].
    pub(crate) fn enrich_turn_telemetry(
        &self,
        mut result: AgentResult,
        guardrails: Option<&crate::tool_guardrails::ToolGuardrailController>,
    ) -> AgentResult {
        if let Some(reason) = guardrails.and_then(|g| g.halt_decision()) {
            result.guardrail = Some(serde_json::json!({ "reason": reason }));
        }
        if result.interrupted {
            result.interrupt_message = self.interrupt.peek_redirect_message();
        }
        let rt = crate::route_learning::primary_runtime_snapshot(self);
        result.model = Some(rt.model);
        result.provider = rt.provider;
        result.base_url = rt.base_url;
        result.session_id = self.config().session_id.clone();
        if let Some(usage) = &result.usage {
            result.input_tokens = Some(usage.prompt_tokens);
            result.output_tokens = Some(usage.completion_tokens);
            result.prompt_tokens = Some(usage.prompt_tokens);
            result.completion_tokens = Some(usage.completion_tokens);
            result.total_tokens = Some(usage.total_tokens);
        }
        if result.session_cost_usd.is_some() {
            result.cost_status = Some("tracked".into());
            result.cost_source = Some("session".into());
        }
        result
    }

    pub(crate) fn finalize_agent_result(&self, mut result: AgentResult) -> AgentResult {
        if result.pending_steer.is_none() {
            result.pending_steer = self.pending_steer.drain();
        }
        if result.turn_exit_reason.is_empty() || result.turn_exit_reason == "unknown" {
            result.turn_exit_reason = if result.interrupted {
                "interrupted_by_user".to_string()
            } else if result.partial {
                "invalid_tool_calls".to_string()
            } else if !result.finished_naturally {
                format!(
                    "max_iterations_reached({}/{})",
                    result.api_calls.max(result.total_turns),
                    self.config().max_turns
                )
            } else {
                "text_response".to_string()
            };
        }
        if result.api_calls == 0 && result.total_turns > 0 {
            result.api_calls = result.total_turns;
        }
        self.enrich_turn_telemetry(result, None)
    }

    /// Pack loop state into [`AgentResult`] (C–D segment return).
    pub(crate) fn seal_loop_result(
        &self,
        ctx: &ContextManager,
        persist_user_idx: Option<usize>,
        prefill_range: Option<Range<usize>>,
        exit: LoopExit<'_>,
        total_turns: u32,
        tool_errors: Vec<hermes_core::ToolErrorRecord>,
        accumulated_usage: Option<UsageStats>,
        session_cost_usd: f64,
        session_started_hooks_fired: bool,
    ) -> AgentResult {
        self.finalize_agent_result(AgentResult {
            messages: self.messages_for_persisted_result(ctx, persist_user_idx, prefill_range),
            finished_naturally: exit.finished_naturally,
            total_turns,
            tool_errors,
            usage: accumulated_usage,
            interrupted: exit.interrupted,
            session_cost_usd: Some(session_cost_usd),
            session_started_hooks_fired,
            pending_steer: None,
            api_calls: exit.api_calls,
            turn_exit_reason: exit.turn_exit_reason.to_string(),
            failed: exit.failed,
            partial: exit.partial,
            guardrail: None,
            interrupt_message: if exit.interrupted {
                self.interrupt.peek_redirect_message()
            } else {
                None
            },
            response_transformed: false,
            response_previewed: false,
            cost_status: None,
            cost_source: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            last_prompt_tokens: None,
            model: None,
            provider: None,
            base_url: None,
            session_id: None,
            plan_pending: exit.plan_pending,
            plan_phase: exit.plan_phase,
        })
    }

    /// Inject mid-run user guidance without interrupting (Python `AIAgent.steer`).
    pub fn steer(&self, text: &str) -> bool {
        self.pending_steer.steer(text)
    }

    fn primary_runtime_from_config(config: &AgentConfig) -> PrimaryRuntime {
        crate::agent_loop::primary_runtime_from_config(config)
    }

    /// Restore primary model/provider at the start of a new turn (Python `run_conversation` prelude).
    pub(crate) fn restore_primary_runtime_at_turn_start(&self) {
        let restored = {
            // Clone both fields out, operate on them, then write back.
            // This avoids double-mutable-borrow issues since both fields live
            // behind the same MutexGuard on AgentSharedState.
            let mut state = match self.state.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
            let mut fb = state.turn_fallback.clone();
            let restored =
                fb.restore_primary_runtime(&self.stored_primary_runtime, &mut state.active_runtime);
            state.turn_fallback = fb;
            restored
        };
        if restored {
            self.sync_config_from_active_runtime();
        }
    }

    fn sync_config_from_active_runtime(&self) {
        let Ok(state) = self.state.lock() else {
            return;
        };
        let active = &state.active_runtime;
        let mut guard = self
            .config_runtime
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let mut cfg = (*guard).as_ref().clone();
        cfg.model = active.model.clone();
        cfg.provider = active.provider.clone();
        cfg.api_mode = active.api_mode.clone();
        *guard = Arc::new(cfg);
    }

    fn runtime_provider_api_mode(&self, provider: &str) -> Option<ApiMode> {
        let provider = provider.trim();
        if provider.is_empty() {
            return None;
        }
        let config = self.config();
        let lookup = |key: &str| {
            config
                .runtime_providers
                .get(key)
                .and_then(|cfg| cfg.api_mode.clone())
        };
        if let Some(mode) = lookup(provider) {
            return Some(mode);
        }

        let lower = provider.to_ascii_lowercase();
        if let Some(mode) = lookup(lower.as_str()) {
            return Some(mode);
        }

        let canonical = hermes_core::providers::canonical_provider_id(provider);
        if let Some(mode) = lookup(canonical.as_str()) {
            return Some(mode);
        }

        if let Some(profile) = crate::provider_profiles::canonical_provider_profile_id(provider) {
            if let Some(mode) = lookup(profile) {
                return Some(mode);
            }
        }

        config
            .runtime_providers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(provider))
            .and_then(|(_, cfg)| cfg.api_mode.clone())
    }

    /// Try switching to a configured fallback model (Nous guard, billing, rate limit).
    pub(crate) fn try_activate_session_fallback(&self, active_model: &str) -> bool {
        if self
            .state
            .lock()
            .map(|state| state.turn_fallback.is_fallback_activated())
            .unwrap_or(false)
        {
            return false;
        }
        let chain = crate::route_learning::resolve_retry_failover_chain(self, active_model);
        let Some(next) = chain.first() else {
            return false;
        };
        let rt = crate::runtime_provider::primary_runtime_for_failover_model(self, next);
        self.activate_runtime_fallback(rt);
        true
    }

    /// Apply an in-session fallback runtime (Python `_try_activate_fallback` outcome).
    pub(crate) fn activate_runtime_fallback(&self, runtime: PrimaryRuntime) {
        if let Ok(mut state) = self.state.lock() {
            state.active_runtime = runtime;
            state.turn_fallback.mark_fallback_activated();
        }
        self.sync_config_from_active_runtime();
    }

    /// Snapshot of agent config (`model` / `provider` / `api_mode` follow active runtime after fallback).
    pub fn config(&self) -> Arc<AgentConfig> {
        self.config_runtime
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn config_snapshot(&self) -> Arc<AgentConfig> {
        self.config()
    }

    /// Create a new agent loop.
    pub fn new(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
    ) -> Self {
        let route_learning = Arc::new(Mutex::new(
            crate::route_learning::load_route_learning_state(&config),
        ));
        let code_index = Self::init_code_index(&config);
        let lsp_context = Self::build_lsp_context_config(&config);
        let stored_primary_runtime = Self::primary_runtime_from_config(&config);
        let context_compressor = build_context_compressor_for_config(&config);
        Self {
            config_runtime: std::sync::RwLock::new(Arc::new(config)),
            tool_registry,
            llm_provider,
            interrupt: InterruptController::new(),
            memory_manager: None,
            recall_backend: None,
            interest_store: None,
            state: Arc::new(Mutex::new(AgentSharedState::new(
                stored_primary_runtime.clone(),
                EvolutionCounters::default(),
            ))),
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
            primary_credential_pool: None,
            sub_agent_orchestrator: None,
            code_index,
            lsp_context,
            route_learning,
            stored_primary_runtime,
            async_tool_dispatch: None,
            pending_steer: PendingSteer::new(),
            context_compressor,
            shared_session_persistence: std::sync::OnceLock::new(),
            provider_serialize_cache: Arc::new(
                crate::provider_serialize_cache::ProviderSerializeCache::new(),
            ),
            disable_streaming: Arc::new(AtomicBool::new(false)),
            vision_supported: Arc::new(AtomicBool::new(true)),
            plan_phase: Arc::new(Mutex::new(hermes_tools::PlanPhase::Off)),
            pending_plan: Arc::new(Mutex::new(None)),
            synced_tools_registry: None,
        }
    }

    /// Wire the live tools registry so plan phase gates apply to `dispatch_async`.
    pub fn with_synced_tools_registry(mut self, registry: Arc<hermes_tools::ToolRegistry>) -> Self {
        registry.set_plan_phase(self.plan_phase());
        self.synced_tools_registry = Some(registry);
        self
    }

    /// Current plan mode phase.
    pub fn plan_phase(&self) -> hermes_tools::PlanPhase {
        self.plan_phase
            .lock()
            .map(|g| *g)
            .unwrap_or(hermes_tools::PlanPhase::Off)
    }

    /// Update plan mode phase and propagate to the synced tools registry.
    pub fn set_plan_phase(&self, phase: hermes_tools::PlanPhase) {
        if let Ok(mut guard) = self.plan_phase.lock() {
            *guard = phase;
        }
        if let Some(registry) = self.synced_tools_registry.as_ref() {
            registry.set_plan_phase(phase);
        }
    }

    /// Pending plan text awaiting approval.
    pub fn pending_plan(&self) -> Option<String> {
        self.pending_plan.lock().ok().and_then(|g| g.clone())
    }

    /// Store pending plan text.
    pub fn set_pending_plan(&self, plan: Option<String>) {
        if let Ok(mut guard) = self.pending_plan.lock() {
            *guard = plan;
        }
    }

    /// Reset incremental session DB flush cursor (e.g. after `/new` or compression rotation).
    pub fn reset_session_db_flush_cursor(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.session_db_flush.reset();
        }
    }

    /// Invalidate session-scoped system prompt cache (compression / `/new`).
    pub fn invalidate_cached_system_prompt(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.cached_system_prompt = None;
        }
    }

    pub(crate) fn set_turn_ext_prefetch_cache(&self, prefetch: String) {
        if let Ok(mut state) = self.state.lock() {
            state.turn_ext_prefetch_cache = prefetch;
        }
        self.invalidate_turn_api_messages_cache();
    }

    pub(crate) fn invalidate_turn_api_messages_cache(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.turn_api_messages_cache = None;
        }
        self.provider_serialize_cache.invalidate();
    }

    fn runtime_generic_provider(
        &self,
        provider: crate::provider::GenericProvider,
    ) -> crate::provider::GenericProvider {
        provider.with_serialize_cache(Arc::clone(&self.provider_serialize_cache))
    }

    fn api_messages_cache_key(
        &self,
        ctx: &ContextManager,
    ) -> crate::api_messages::ApiMessagesCacheKey {
        let prefetch = self
            .state
            .lock()
            .map(|state| state.turn_ext_prefetch_cache.clone())
            .unwrap_or_default();
        let cfg = self.config();
        crate::api_messages::ApiMessagesCacheKey {
            message_count: ctx.len(),
            total_chars: ctx.total_chars(),
            prefetch_len: prefetch.len(),
            prefetch_hash: crate::api_messages::hash_str(&prefetch),
            ephemeral_len: cfg
                .ephemeral_system_prompt
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::len)
                .unwrap_or(0),
            model_hash: crate::api_messages::hash_str(&crate::runtime_provider::active_model(self)),
            use_prompt_caching: cfg.use_prompt_caching,
            use_native_cache_layout: cfg.use_native_cache_layout,
            cache_ttl_hash: crate::api_messages::hash_str(&cfg.cache_ttl),
        }
    }

    fn prepare_ctx_for_api_call(&self, ctx: &mut ContextManager) {
        let cfg = self.config();
        let provider = cfg.provider.as_deref().unwrap_or("");
        let base_url = self
            .resolve_runtime_base_url(provider, None)
            .unwrap_or_default();
        let api_mode = crate::hooks::api_mode_as_hook_str(&cfg.api_mode);
        crate::runtime_provider::refresh_prompt_cache_policy(self, provider, &base_url, api_mode);
        let session_id = cfg.session_id.as_deref();
        let (tool_repairs, seq_repairs) =
            agent_runtime_helpers::prepare_live_history_for_api(ctx.get_messages_mut(), session_id);
        if tool_repairs > 0 || seq_repairs > 0 {
            tracing::debug!(
                tool_call_arg_repairs = tool_repairs,
                message_sequence_repairs = seq_repairs,
                "pre-API live history repairs"
            );
            self.invalidate_turn_api_messages_cache();
        }
        self.pending_steer
            .drain_pre_api_into_messages(ctx.get_messages_mut());
        self.interest_sync_user_messages(ctx.get_messages());
    }

    pub(crate) fn build_turn_api_messages(&self, ctx: &mut ContextManager) -> Arc<[Message]> {
        crate::llm_caller::build_turn_api_messages(self, ctx)
    }

    pub(crate) fn build_api_messages_legacy(&self, ctx: &mut ContextManager) -> Vec<Message> {
        crate::llm_caller::build_api_messages_legacy(self, ctx)
    }

    /// Golden harness entry for `messages_for_api_call` (zero-copy migration oracle).
    #[doc(hidden)]
    pub fn oracle_messages_for_api_call(&self, ctx: &mut ContextManager) -> Vec<Message> {
        self.build_api_messages_legacy(ctx)
    }

    #[doc(hidden)]
    pub fn oracle_candidate_messages_for_api_call(&self, ctx: &mut ContextManager) -> Vec<Message> {
        self.build_turn_api_messages(ctx).to_vec()
    }

    /// Set turn-scoped memory prefetch injected at API-call time (test harness only).
    #[doc(hidden)]
    pub fn oracle_set_turn_ext_prefetch_cache(&self, prefetch: impl Into<String>) {
        self.set_turn_ext_prefetch_cache(prefetch.into());
    }

    pub(crate) fn session_persistence(&self) -> Option<Arc<SessionPersistence>> {
        let home = self.config().hermes_home.clone()?;
        let home = home.trim().to_string();
        if home.is_empty() {
            return None;
        }
        Some(
            self.shared_session_persistence
                .get_or_init({
                    let home = home.clone();
                    move || Arc::new(SessionPersistence::new(Path::new(&home)))
                })
                .clone(),
        )
    }

    fn compression_lock_holder(&self) -> String {
        format!(
            "pid={}:tid={:?}:agent={:p}:nonce={}",
            std::process::id(),
            std::thread::current().id(),
            self,
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        )
    }

    fn new_compression_session_id() -> String {
        let now = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        format!("{}_{}", now, &suffix[..6.min(suffix.len())])
    }

    fn patch_leading_system_message(messages: &mut Vec<Message>, prompt: &str) {
        if messages
            .first()
            .map(|m| m.role == MessageRole::System)
            .unwrap_or(false)
        {
            messages[0].content = Some(prompt.to_string());
        } else {
            messages.insert(0, Message::system(prompt));
        }
    }

    /// Route tool execution through an async dispatcher (e.g. `hermes_tools::dispatch_async`).
    pub fn with_async_tool_dispatch(mut self, dispatch: AsyncToolDispatch) -> Self {
        self.async_tool_dispatch = Some(dispatch);
        self
    }

    pub(crate) fn maybe_with_async_tool_dispatch(
        self,
        dispatch: Option<AsyncToolDispatch>,
    ) -> Self {
        if let Some(dispatch) = dispatch {
            self.with_async_tool_dispatch(dispatch)
        } else {
            self
        }
    }

    /// Clone the async tool dispatch hook (for background review / sub-agents).
    pub(crate) fn async_tool_dispatch(&self) -> Option<AsyncToolDispatch> {
        self.async_tool_dispatch.clone()
    }

    /// Create a new agent loop with a shared interrupt controller.
    pub fn with_interrupt(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
        interrupt: InterruptController,
    ) -> Self {
        let route_learning = Arc::new(Mutex::new(
            crate::route_learning::load_route_learning_state(&config),
        ));
        // let code_index = Self::init_code_index(&config);
        let lsp_context = Self::build_lsp_context_config(&config);
        let stored_primary_runtime = Self::primary_runtime_from_config(&config);
        let context_compressor = build_context_compressor_for_config(&config);
        Self {
            config_runtime: std::sync::RwLock::new(Arc::new(config)),
            tool_registry,
            llm_provider,
            interrupt,
            memory_manager: None,
            recall_backend: None,
            interest_store: None,
            state: Arc::new(Mutex::new(AgentSharedState::new(
                stored_primary_runtime.clone(),
                EvolutionCounters::default(),
            ))),
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
            primary_credential_pool: None,
            sub_agent_orchestrator: None,
            code_index: None,
            lsp_context,
            route_learning,
            stored_primary_runtime,
            async_tool_dispatch: None,
            pending_steer: PendingSteer::new(),
            context_compressor,
            shared_session_persistence: std::sync::OnceLock::new(),
            provider_serialize_cache: Arc::new(
                crate::provider_serialize_cache::ProviderSerializeCache::new(),
            ),
            disable_streaming: Arc::new(AtomicBool::new(false)),
            vision_supported: Arc::new(AtomicBool::new(true)),
            plan_phase: Arc::new(Mutex::new(hermes_tools::PlanPhase::Off)),
            pending_plan: Arc::new(Mutex::new(None)),
            synced_tools_registry: None,
        }
    }

    fn init_code_index(config: &AgentConfig) -> Option<Arc<Mutex<CodeIndex>>> {
        if !config.code_index_enabled {
            return None;
        }
        let workspace_root = std::env::var("TERMINAL_CWD")
            .ok()
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        if !workspace_root.exists() {
            return None;
        }
        let index = CodeIndex::default_for_workspace(workspace_root);
        // let _ = index.ensure_fresh(); // high time comsumption, should be called in a separate thread
        Some(Arc::new(Mutex::new(index)))
    }

    fn build_lsp_context_config(config: &AgentConfig) -> LspContextConfig {
        let mut cfg = LspContextConfig::from_env();
        cfg.enabled = cfg.enabled && config.lsp_context_enabled;
        cfg.max_chars = config.lsp_context_max_chars.max(400);
        cfg
    }

    /// Attach an in-process sub-agent orchestrator. When set, `delegate_task`
    /// tool calls are actually executed by the orchestrator instead of just
    /// returning a signal envelope. See
    /// [`crate::sub_agent_orchestrator::SubAgentOrchestrator`].
    pub fn with_sub_agent_orchestrator(
        mut self,
        orchestrator: Arc<crate::sub_agent_orchestrator::SubAgentOrchestrator>,
    ) -> Self {
        self.sub_agent_orchestrator = Some(orchestrator);
        self
    }

    /// Attach the primary runtime credential pool (API key rotation).
    pub fn with_primary_credential_pool(mut self, pool: Arc<CredentialPool>) -> Self {
        self.primary_credential_pool = Some(pool);
        self
    }

    /// Set the memory manager.
    pub fn with_memory(mut self, mm: Arc<std::sync::Mutex<MemoryManager>>) -> Self {
        self.memory_manager = Some(mm);
        self
    }

    /// Attach session search backend for proactive recall at turn start.
    pub fn with_recall_backend(
        mut self,
        backend: Arc<dyn hermes_tools::SessionSearchBackend>,
    ) -> Self {
        self.recall_backend = Some(backend);
        self
    }

    /// Attach local POI store (independent of external memory / `skip_memory`).
    pub fn with_interest_store(mut self, store: Arc<Mutex<InterestStore>>) -> Self {
        self.interest_store = Some(store);
        self
    }

    /// Set the plugin manager.
    pub fn with_plugins(mut self, pm: Arc<std::sync::Mutex<PluginManager>>) -> Self {
        self.plugin_manager = Some(pm);
        self
    }

    /// Set the callbacks.
    pub fn with_callbacks(mut self, cb: AgentCallbacks) -> Self {
        self.callbacks = Arc::new(cb);
        self
    }

    /// Set the delegate depth.
    pub fn with_delegate_depth(mut self, depth: u32) -> Self {
        self.delegate_depth = depth;
        self
    }

    // -- Memory helpers ----------------------------------------------------

    fn interest_prefetch_block(&self, query: &str) -> String {
        if !self.config().interest.enabled {
            return String::new();
        }
        let Some(ref store) = self.interest_store else {
            return String::new();
        };
        let Ok(guard) = store.lock() else {
            return String::new();
        };
        guard.render_prefetch_block(query).unwrap_or_default()
    }

    fn reset_interest_sync_cursor(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.interest_synced_message_len = 0;
            state.interest_synced_user_hashes.clear();
            state.interest_session_buffer.clear();
        }
    }

    pub(crate) fn interest_sync_user_messages(&self, messages: &[Message]) {
        if !self.config().interest.enabled {
            return;
        }
        let interest_cfg = self.config().interest.clone();
        if !interest_cfg.per_turn_persist && !interest_cfg.per_turn_buffer {
            return;
        }
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let start = state.interest_synced_message_len;
        if start >= messages.len() {
            return;
        }
        for msg in messages.iter().skip(start) {
            if msg.role != MessageRole::User {
                continue;
            }
            let Some(text) = msg.content.as_deref() else {
                continue;
            };
            let trimmed = text.trim();
            if trimmed.is_empty() || is_poi_synthetic_user_text(trimmed) {
                continue;
            }
            let hash = {
                let mut hasher = Sha256::new();
                hasher.update(trimmed.as_bytes());
                let digest = hasher.finalize();
                u64::from_be_bytes(digest[..8].try_into().unwrap_or([0u8; 8]))
            };
            if !state.interest_synced_user_hashes.insert(hash) {
                continue;
            }
            if interest_cfg.per_turn_persist {
                let Some(ref store) = self.interest_store else {
                    continue;
                };
                if let Ok(guard) = store.lock() {
                    let _ = ingest_user_message(&guard, &interest_cfg, trimmed, 0.35);
                }
            } else if interest_cfg.per_turn_buffer {
                state
                    .interest_session_buffer
                    .absorb_turn(trimmed, &interest_cfg);
            }
        }
        state.interest_synced_message_len = messages.len();
    }

    fn interest_on_session_end(&self, messages: &[Message]) {
        let interest_enabled = self.config().interest.enabled;
        let insights_enabled = hermes_config::load_config(None)
            .unwrap_or_default()
            .insights
            .contribution
            .enabled;
        if !interest_enabled && !insights_enabled {
            return;
        }
        let buffered = if interest_enabled {
            self.state
                .lock()
                .map(|mut state| {
                    let buf = state.interest_session_buffer.drain();
                    state.interest_synced_user_hashes.clear();
                    state.interest_synced_message_len = 0;
                    buf
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let as_values: Vec<Value> = messages
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();
        let interest_cfg = self.config().interest.clone();
        let insights_cfg = hermes_config::load_config(None)
            .unwrap_or_default()
            .insights
            .contribution;
        let hermes_home = self
            .config()
            .hermes_home
            .as_ref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(hermes_config::hermes_home);
        let session_id = self
            .config()
            .session_id
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let auxiliary = if interest_cfg.session_end_llm_enabled() {
            tracing::warn!(
                "interest: session-end LLM extraction is enabled; user-only messages may be sent to the auxiliary LLM provider"
            );
            try_build_auxiliary_arc_for_config(&self.config())
        } else {
            None
        };
        spawn_session_end_pipeline(
            hermes_home,
            interest_cfg,
            insights_cfg,
            session_id,
            as_values,
            buffered,
            auxiliary,
        );
    }

    pub(crate) fn memory_prefetch(&self, query: &str, session_id: &str) -> String {
        let mut parts = Vec::new();
        let interest = self.interest_prefetch_block(query);
        if !interest.is_empty() {
            parts.push(interest);
        }
        if self.config().skip_memory {
            return parts.join("\n\n");
        }
        if let Some(ref mm) = self.memory_manager {
            if let Ok(mm) = mm.lock() {
                let block = mm.prefetch_all(query, session_id);
                if !block.is_empty() {
                    parts.push(block);
                }
            }
        }
        parts.join("\n\n")
    }

    fn recall_enabled_for_agent(&self) -> bool {
        if self.config().skip_memory {
            return false;
        }
        if !self.config().recall_enabled {
            return false;
        }
        std::env::var("HERMES_RECALL_ENABLED")
            .ok()
            .map(|v| {
                !matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "off" | "no"
                )
            })
            .unwrap_or(true)
    }

    /// Proactive session recall block for continuation-style user messages.
    pub(crate) async fn recall_prefetch(&self, query: &str, session_id: &str) -> String {
        if !self.recall_enabled_for_agent() {
            return String::new();
        }
        let Some(backend) = self.recall_backend.as_ref() else {
            return String::new();
        };
        let Some(rq) = crate::recall_planner::classify(query) else {
            return String::new();
        };
        let options = hermes_tools::SessionSearchOptions {
            summarize: false,
        };
        let json = match backend
            .search(
                Some(&rq.keywords),
                None,
                5,
                Some(session_id),
                options,
            )
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = %e, signal = ?rq.signal, "recall_prefetch search failed");
                return String::new();
            }
        };
        crate::recall_planner::format_recall_block(&json)
    }

    pub(crate) fn memory_sync(&self, user: &str, assistant: &str, session_id: &str) {
        if self.config().skip_memory {
            return;
        }
        if let Some(ref mm) = self.memory_manager {
            if let Ok(mm) = mm.lock() {
                mm.sync_all(user, assistant, session_id);
                if !user.trim().is_empty() {
                    mm.queue_prefetch_all(user, session_id);
                }
            }
        }
    }

    /// Python `_sync_external_memory_for_turn` — end-of-turn durable memory sync.
    pub(crate) fn sync_external_memory_for_turn(
        &self,
        original_user_message: &str,
        final_response: Option<&str>,
        interrupted: bool,
    ) {
        if interrupted || self.config().skip_memory {
            return;
        }
        let Some(response) = final_response.map(str::trim).filter(|s| !s.is_empty()) else {
            return;
        };
        if original_user_message.trim().is_empty() {
            return;
        }
        let session_id = self
            .config()
            .session_id
            .as_deref()
            .unwrap_or("")
            .to_string();
        self.memory_sync(original_user_message, response, &session_id);
    }

    pub(crate) fn reset_vision_supported_for_turn(&self) {
        self.vision_supported
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn disable_vision_supported_and_strip_context(&self, ctx: &mut ContextManager) {
        self.vision_supported
            .store(false, std::sync::atomic::Ordering::Release);
        crate::vision_message_prepare::strip_images_for_non_vision_model_in_place(
            ctx.get_messages_mut(),
        );
        self.invalidate_turn_api_messages_cache();
    }

    pub(crate) async fn cleanup_dead_connections_at_turn_start(&self) {
        let rt = crate::route_learning::primary_runtime_snapshot(self);
        let provider = rt.provider.as_deref().unwrap_or("").trim();
        let Some(mut base) = crate::runtime_provider::resolve_runtime_base_url(
            self,
            provider,
            rt.base_url.as_deref(),
        ) else {
            return;
        };
        if base.is_empty() {
            return;
        }
        if !base.ends_with('/') {
            base.push('/');
        }
        let probe_url = format!("{base}models");
        crate::runtime_provider::effective_llm_provider(self)
            .turn_start_connection_hygiene(&probe_url)
            .await;
    }

    async fn compute_compression_feasibility_warning(&self) -> Option<String> {
        const AUX_FLOOR: u64 = 64_000;
        let threshold = self.context_compressor.lock().await.threshold_tokens();
        let aux_model = std::env::var("HERMES_COMPRESSION_MODEL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "google/gemini-3-flash-preview".to_string());
        let aux_ctx = get_model_context_length(&aux_model);
        if aux_ctx >= AUX_FLOOR && aux_ctx < threshold {
            return Some(format!(
                "Compression model '{aux_model}' context ({aux_ctx} tokens) is below the \
                 session compression threshold ({threshold} tokens). Auto-lowered threshold \
                 for this session; set a larger compression model in config.yaml if needed."
            ));
        }
        None
    }

    /// Replay stored compression feasibility warning once (Python `_replay_compression_warning`).
    pub(crate) async fn replay_compression_warning_at_turn_start(&self) {
        let should_compile = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            if !state.compression_feasibility_checked {
                state.compression_feasibility_checked = true;
                true
            } else {
                false
            }
        };
        if should_compile {
            if let Some(msg) = self.compute_compression_feasibility_warning().await {
                if let Ok(mut state) = self.state.lock() {
                    state.compression_warning = Some(msg);
                }
            }
        }
        let msg = self
            .state
            .lock()
            .ok()
            .and_then(|mut state| state.compression_warning.take());
        if let Some(msg) = msg {
            crate::hooks::emit_status(self, "lifecycle", &msg);
        }
    }

    pub(crate) fn log_turn_exit_diagnostic(
        &self,
        loop_result: &hermes_core::AgentResult,
        messages: &[Message],
    ) {
        let last_role = messages
            .last()
            .map(|m| format!("{:?}", m.role))
            .unwrap_or_else(|| "none".into());
        let pending_tool_assistant = messages
            .iter()
            .filter(|m| {
                m.role == MessageRole::Assistant
                    && m.tool_calls.as_ref().is_some_and(|t| !t.is_empty())
            })
            .count();
        let max_turns = effective_max_turns(self.config().max_turns)
            .map(|m| m.to_string())
            .unwrap_or_else(|| "unlimited".into());
        let last_assistant_tail = messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Assistant)
            .and_then(|m| m.content.as_deref())
            .map(|text| {
                let trimmed = text.trim();
                let count = trimmed.chars().count();
                if count <= 120 {
                    trimmed.replace('\n', " ")
                } else {
                    let tail: String = trimmed.chars().skip(count.saturating_sub(120)).collect();
                    format!("…{}", tail.replace('\n', " "))
                }
            })
            .unwrap_or_default();
        tracing::info!(
            session_id = %crate::session_log::current_session_tag(),
            turn_exit_reason = %loop_result.turn_exit_reason,
            api_calls = loop_result.api_calls,
            total_turns = loop_result.total_turns,
            max_turns = %max_turns,
            interrupted = loop_result.interrupted,
            failed = loop_result.failed,
            partial = loop_result.partial,
            finished_naturally = loop_result.finished_naturally,
            last_msg_role = %last_role,
            pending_tool_assistant_msgs = pending_tool_assistant,
            last_assistant_tail = %last_assistant_tail,
            "conversation turn exit"
        );
    }

    fn memory_write_event_from_tool_call(tc: &ToolCall) -> Option<(String, String, String)> {
        if tc.function.name != "memory" {
            return None;
        }
        let args: Value = serde_json::from_str(&tc.function.arguments).ok()?;
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_lowercase();
        if action != "add" && action != "replace" && action != "remove" {
            return None;
        }
        let target = args
            .get("target")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("memory")
            .to_string();
        let content = if action == "remove" {
            args.get("old_text")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("")
                .to_string()
        } else {
            args.get("content")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("")
                .to_string()
        };
        Some((action, target, content))
    }

    pub(crate) fn notify_memory_writes(&self, tool_calls: &[ToolCall], results: &[ToolResult]) {
        crate::tool_executor::notify_memory_writes(self, tool_calls, results)
    }

    fn delegation_event_from_tool_result(
        tc: &ToolCall,
        result: &ToolResult,
    ) -> Option<(String, String)> {
        if tc.function.name != "delegate_task" || result.is_error {
            return None;
        }
        let args: Value = serde_json::from_str(&tc.function.arguments).ok()?;
        let task = args
            .get("task")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())?
            .to_string();

        let sub_agent_id = serde_json::from_str::<Value>(&result.content)
            .ok()
            .and_then(|v| {
                v.get("sub_agent_id")
                    .and_then(|id| id.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
            })
            .unwrap_or_default();

        Some((task, sub_agent_id))
    }

    pub(crate) fn notify_delegations(&self, tool_calls: &[ToolCall], results: &[ToolResult]) {
        crate::tool_executor::notify_delegations(self, tool_calls, results)
    }

    pub(crate) fn memory_on_turn_start(&self, turn: u32, message: &str) {
        crate::tool_executor::memory_on_turn_start(self, turn, message)
    }

    pub(crate) fn memory_system_prompt(&self) -> String {
        crate::tool_executor::memory_system_prompt(self)
    }

    fn memory_pre_compress_note(&self, messages: &[Message]) -> Option<String> {
        if self.config().skip_memory {
            return None;
        }
        let Some(ref mm) = self.memory_manager else {
            return None;
        };
        let Ok(mm) = mm.lock() else {
            return None;
        };
        let as_values: Vec<Value> = messages
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();
        let note = mm.on_pre_compress(&as_values);
        if note.trim().is_empty() {
            None
        } else {
            Some(note)
        }
    }

    /// Notify memory providers that `session_id` rotated (compression, `/new`, resume, branch).
    pub fn memory_on_session_switch(
        &self,
        new_session_id: &str,
        parent_session_id: &str,
        reset: bool,
        reason: &str,
    ) {
        if self.config().skip_memory {
            return;
        }
        let Some(ref mm) = self.memory_manager else {
            return;
        };
        let Ok(mm) = mm.lock() else {
            return;
        };
        mm.on_session_switch(new_session_id, parent_session_id, reset, reason);
    }

    /// Update the active runtime session id (CLI `/new`, `/resume`, manual `/compress`).
    pub fn set_runtime_session_id(&self, session_id: &str) {
        let sid = session_id.trim();
        if sid.is_empty() {
            return;
        }
        if let Ok(mut guard) = self.config_runtime.write() {
            let mut updated = (*guard).as_ref().clone();
            updated.session_id = Some(sid.to_string());
            *guard = Arc::new(updated);
        }
        let hermes_home = self
            .config()
            .hermes_home
            .as_ref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(hermes_config::hermes_home);
        touch_active_session(&hermes_home, sid);
    }

    /// Current runtime session id from agent config.
    pub fn runtime_session_id(&self) -> Option<String> {
        self.config().session_id.clone()
    }

    /// Run Python-parity compression on a standalone message list (CLI `/compress`).
    pub async fn compress_messages(
        &self,
        messages: Vec<Message>,
        session_id: &str,
        model: &str,
    ) -> (Vec<Message>, bool) {
        self.set_runtime_session_id(session_id);
        if let Ok(mut guard) = self.config_runtime.write() {
            let m = model.trim();
            if !m.is_empty() {
                let mut updated = (*guard).as_ref().clone();
                updated.model = m.to_string();
                *guard = Arc::new(updated);
            }
        }
        let mut ctx = ContextManager::for_model(model);
        ctx.replace_messages(messages);
        let compressed = self.compress_context(&mut ctx).await;
        (ctx.get_messages().to_vec(), compressed)
    }

    pub(crate) fn memory_on_session_end(&self, messages: &[Message]) {
        self.interest_on_session_end(messages);
        if self.config().skip_memory {
            return;
        }
        let Some(ref mm) = self.memory_manager else {
            return;
        };
        let Ok(mm) = mm.lock() else {
            return;
        };
        let as_values: Vec<Value> = messages
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();
        mm.on_session_end(&as_values);
    }

    /// Full session teardown: memory providers + plugin `on_session_end`.
    pub fn session_end_hooks(
        &self,
        messages: &[Message],
        completed: bool,
        interrupted: bool,
        total_turns: u32,
        session_started_hooks_fired: bool,
    ) {
        crate::hooks::session_end_hooks(
            self,
            messages,
            completed,
            interrupted,
            total_turns,
            session_started_hooks_fired,
        );
    }

    pub(crate) fn openrouter_provider_preferences(&self) -> Option<Value> {
        let cfg = self.config();
        let mut prefs = serde_json::Map::new();
        if !cfg.providers_allowed.is_empty() {
            prefs.insert(
                "only".into(),
                Value::Array(
                    cfg.providers_allowed
                        .iter()
                        .map(|s| Value::String(s.clone()))
                        .collect(),
                ),
            );
        }
        if !cfg.providers_ignored.is_empty() {
            prefs.insert(
                "ignore".into(),
                Value::Array(
                    cfg.providers_ignored
                        .iter()
                        .map(|s| Value::String(s.clone()))
                        .collect(),
                ),
            );
        }
        if !cfg.providers_order.is_empty() {
            prefs.insert(
                "order".into(),
                Value::Array(
                    cfg.providers_order
                        .iter()
                        .map(|s| Value::String(s.clone()))
                        .collect(),
                ),
            );
        }
        if let Some(sort) = cfg.provider_sort.as_deref().filter(|s| !s.is_empty()) {
            prefs.insert("sort".into(), Value::String(sort.to_string()));
        }
        if let Some(req) = cfg.provider_require_parameters {
            prefs.insert("require_parameters".into(), Value::Bool(req));
        }
        if let Some(dc) = cfg
            .provider_data_collection
            .as_deref()
            .filter(|s| !s.is_empty())
        {
            prefs.insert("data_collection".into(), Value::String(dc.to_string()));
        }
        if prefs.is_empty() && cfg.openrouter_min_coding_score.is_none() {
            return None;
        }
        let mut provider_obj = Value::Object(prefs);
        if let Some(score) = cfg.openrouter_min_coding_score {
            if let Some(obj) = provider_obj.as_object_mut() {
                obj.insert(
                    "plugins".into(),
                    json!([{ "id": "pareto-router", "min_coding_score": score }]),
                );
            }
        }
        Some(provider_obj)
    }

    pub(crate) fn invoke_pre_api_request_hook(
        &self,
        api_call_count: u32,
        api_messages: &[Message],
        tool_count: usize,
        model: &str,
        provider: &str,
        base_url: Option<&str>,
        api_mode: &ApiMode,
        max_tokens: Option<u32>,
    ) {
        let request_messages: Vec<Value> = api_messages
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();
        let message_count = api_messages.len();
        let request_char_count: usize = api_messages
            .iter()
            .map(|m| {
                m.content.as_deref().map(str::len).unwrap_or(0)
                    + m.reasoning_content.as_deref().map(str::len).unwrap_or(0)
            })
            .sum();
        let approx_input_tokens = (request_char_count / 4).max(1);
        let user_message = api_messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let hook_ctx = serde_json::json!({
            "session_id": self.config().session_id.as_deref().unwrap_or(""),
            "user_message": user_message,
            "platform": self.config().platform.as_deref().unwrap_or(""),
            "model": model,
            "provider": provider,
            "base_url": base_url.unwrap_or(""),
            "api_mode": crate::hooks::api_mode_as_hook_str(api_mode),
            "api_call_count": api_call_count,
            "attempt": api_call_count,
            "stream": false,
            "request_messages": request_messages,
            "message_count": message_count,
            "tool_count": tool_count,
            "approx_input_tokens": approx_input_tokens,
            "request_char_count": request_char_count,
            "max_tokens": max_tokens,
        });
        let _ = crate::hooks::invoke_hook(self, HookType::PreApiRequest, &hook_ctx);
    }

    pub(crate) fn code_index_repo_map_block(&self) -> Option<String> {
        let Some(ref idx) = self.code_index else {
            return None;
        };
        let Ok(mut idx) = idx.lock() else {
            return None;
        };
        let rendered = idx.render_repo_map(
            Some(self.config().code_index_max_files),
            Some(self.config().code_index_max_symbols),
        );
        if rendered.trim().is_empty() {
            None
        } else {
            Some(rendered)
        }
    }

    pub(crate) fn lsp_context_note(
        &self,
        tool_calls: &[ToolCall],
        results: &[ToolResult],
    ) -> Option<String> {
        if !self.lsp_context.enabled {
            return None;
        }
        let Some(ref idx) = self.code_index else {
            return None;
        };
        let Ok(mut idx) = idx.lock() else {
            return None;
        };
        build_lsp_context_note(tool_calls, results, &mut idx, &self.lsp_context)
    }

    pub(crate) fn should_inject_tool_enforcement(&self, model: &str) -> bool {
        should_inject_tool_enforcement_for_model(model)
    }

    pub(crate) fn platform_hint_text(&self) -> Option<&'static str> {
        platform_hint_for(self.config().platform.as_deref())
    }

    pub(crate) fn probe_remote_backend_text(&self, env_type: &str) -> Option<String> {
        let cwd_hint = std::env::var("TERMINAL_CWD").unwrap_or_default();
        probe_remote_backend_cached(env_type, &cwd_hint, || {
            let terminal = self.tool_registry.get("terminal")?;
            let output = (terminal.handler)(json!({ "command": BACKEND_PROBE_COMMAND })).ok()?;
            format_probe_output(output.trim())
        })
    }

    pub(crate) fn effective_provider_for_prompt(&self, model: &str) -> Option<String> {
        if let Some(ref p) = self.config().provider {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        model
            .split_once(':')
            .map(|(provider, _)| provider.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn runtime_skills_tier() -> &'static str {
        match std::env::var("HERMES_SKILLS_EXECUTION_TIER")
            .ok()
            .unwrap_or_else(|| "balanced".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "trusted" => "trusted",
            "open" | "permissive" => "open",
            _ => "balanced",
        }
    }

    fn runtime_skills_tier_bypass_enabled() -> bool {
        std::env::var("HERMES_SKILLS_TIER_BYPASS")
            .ok()
            .is_some_and(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
    }

    fn skill_trust_score(cmd: &str, name: &str, description: &str) -> i32 {
        let corpus = format!(
            "{} {} {}",
            cmd.to_ascii_lowercase(),
            name.to_ascii_lowercase(),
            description.to_ascii_lowercase()
        );
        let mut score = 70i32;
        let high_risk_terms = [
            "trade",
            "money",
            "wallet",
            "deploy",
            "delete",
            "shell",
            "execute",
            "terminal",
            "browser automation",
            "computer use",
            "send email",
            "gmail",
            "calendar",
        ];
        for term in high_risk_terms {
            if corpus.contains(term) {
                score -= 12;
            }
        }
        let medium_risk_terms = ["write", "modify", "edit", "publish", "install", "webhook"];
        for term in medium_risk_terms {
            if corpus.contains(term) {
                score -= 6;
            }
        }
        let trusted_terms = ["search", "read", "summarize", "analyze", "query", "list"];
        for term in trusted_terms {
            if corpus.contains(term) {
                score += 4;
            }
        }
        score.clamp(0, 100)
    }

    fn skill_allowed_for_tier(tier: &str, score: i32) -> bool {
        match tier {
            "trusted" => score >= 62,
            "balanced" => score >= 34,
            _ => true,
        }
    }

    pub(crate) fn skills_system_prompt(&self, tool_names: &HashSet<&str>) -> Option<String> {
        let has_skills_tools = ["skills_list", "skill_view", "skill_manage"]
            .iter()
            .any(|t| tool_names.contains(*t));
        if !has_skills_tools {
            return None;
        }
        let mut orch = SkillOrchestrator::default_dir();
        orch.set_enabled_disabled(
            &self.config().enabled_skills,
            &self.config().disabled_skills,
        );
        let commands = orch.scan_skill_commands();
        if commands.is_empty() {
            return Some(
                "## Skills (mandatory)\nSkills tools are enabled. Use `skills_list` to discover available skills and `skill_view` before applying one."
                    .to_string(),
            );
        }
        let tier = Self::runtime_skills_tier();
        let bypass = Self::runtime_skills_tier_bypass_enabled();
        let mut rows: Vec<_> = commands
            .iter()
            .filter(|(cmd, info)| {
                if bypass || tier == "open" {
                    return true;
                }
                let score = Self::skill_trust_score(cmd, &info.name, &info.description);
                Self::skill_allowed_for_tier(tier, score)
            })
            .collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        let filtered = commands.len().saturating_sub(rows.len());
        if rows.is_empty() {
            return Some(format!(
                "## Skills (mandatory)\nSkills tools are enabled but current skills tier '{}' filtered all candidates. Use `/ops skills-tier balanced` or `/ops skills-tier open` for broader access.",
                tier
            ));
        }
        let mut body = String::from(
            "## Skills (mandatory)\nBefore replying, check whether an existing skill applies. If yes, inspect it with `skill_view` and follow it.\n<available_skills>\n",
        );
        body.push_str(&format!(
            "<skills_tier mode=\"{}\" bypass=\"{}\" filtered=\"{}\" />\n",
            tier,
            if bypass { "on" } else { "off" },
            filtered
        ));
        for (cmd, info) in rows.into_iter().take(80) {
            body.push_str(&format!(
                "- {}: {} ({})\n",
                cmd,
                info.name,
                info.description.trim()
            ));
        }
        body.push_str("</available_skills>");
        Some(body)
    }

    pub(crate) fn context_files_prompt(&self) -> Option<String> {
        if self.config().skip_context_files {
            return None;
        }
        let cwd = std::env::var("TERMINAL_CWD")
            .ok()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            });

        let mut sections = Vec::new();
        if let Some(workspace) = load_workspace_context(&cwd) {
            sections.push(format!("## Workspace Context\n{}", workspace));
        }

        let hermes_home = self
            .config()
            .hermes_home
            .as_deref()
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var("HERMES_HOME")
                    .ok()
                    .map(std::path::PathBuf::from)
            })
            .or_else(|| dirs::home_dir().map(|h| h.join(".hermes")))
            .unwrap_or_else(|| std::path::PathBuf::from(".hermes"));

        let personal_ctx = load_hermes_context_files(&hermes_home);
        if !personal_ctx.trim().is_empty() {
            sections.push(format!("## Personal Context\n{}", personal_ctx));
        }

        if sections.is_empty() {
            None
        } else {
            Some(sections.join("\n\n"))
        }
    }

    pub(crate) fn extract_provider_and_model<'a>(&self, model: &'a str) -> (String, &'a str) {
        if let Some((p, m)) = model.split_once(':') {
            let p = p.trim();
            let m = m.trim();
            if !p.is_empty() && !m.is_empty() {
                return (p.to_string(), m);
            }
        }
        let fallback_provider = self
            .config()
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("openai")
            .to_string();
        (fallback_provider, model)
    }

    fn resolve_runtime_api_key(
        &self,
        provider: &str,
        api_key_env_override: Option<&str>,
        explicit_api_key: Option<&str>,
    ) -> Option<String> {
        if provider == "copilot-acp" {
            return Some("copilot-acp".to_string());
        }
        if let Some(token) = self.resolve_oauth_store_api_key(provider) {
            return Some(token);
        }
        if let Some(key) = explicit_api_key.map(str::trim).filter(|s| !s.is_empty()) {
            return Some(key.to_string());
        }
        if let Some(env_name) = api_key_env_override
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if let Ok(v) = std::env::var(env_name) {
                if !v.trim().is_empty() {
                    return Some(v);
                }
            }
        }
        if let Some(cfg) = self.config().runtime_providers.get(provider) {
            if let Some(ref key) = cfg.api_key {
                let trimmed = key.trim();
                if let Some(env_ref) = trimmed.strip_prefix("${").and_then(|s| s.strip_suffix('}'))
                {
                    if let Ok(v) = std::env::var(env_ref) {
                        if !v.trim().is_empty() {
                            return Some(v);
                        }
                    }
                } else if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
            if let Some(env_name) = cfg
                .api_key_env
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if let Ok(v) = std::env::var(env_name) {
                    if !v.trim().is_empty() {
                        return Some(v);
                    }
                }
            }
        }
        if matches!(provider, "openai" | "codex" | "openai-codex") {
            return std::env::var("HERMES_OPENAI_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("OPENAI_API_KEY").ok())
                .filter(|v| !v.trim().is_empty());
        }
        if provider == "stepfun" {
            return std::env::var("HERMES_STEPFUN_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("STEPFUN_API_KEY").ok())
                .filter(|v| !v.trim().is_empty());
        }
        match provider {
            "anthropic" | "claude" | "claude-code" => std::env::var("ANTHROPIC_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("ANTHROPIC_TOKEN").ok())
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok())
                .filter(|v| !v.trim().is_empty()),
            "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => {
                std::env::var("HERMES_GEMINI_OAUTH_API_KEY")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
            }
            "openrouter" => std::env::var("OPENROUTER_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            "qwen" | "qwen-oauth" => std::env::var("DASHSCOPE_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            "kimi" | "moonshot" => std::env::var("MOONSHOT_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            "minimax" => std::env::var("MINIMAX_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            "nous" => std::env::var("NOUS_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            "copilot" | "copilot-acp" => std::env::var("GITHUB_COPILOT_TOKEN")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            _ => None,
        }
    }

    pub(crate) fn resolve_runtime_base_url(
        &self,
        provider: &str,
        route_base_url: Option<&str>,
    ) -> Option<String> {
        crate::runtime_provider::resolve_runtime_base_url(self, provider, route_base_url)
    }

    fn resolve_oauth_store_api_key(&self, provider: &str) -> Option<String> {
        let provider_key = match provider {
            "openai" => "openai",
            "openai-codex" | "codex" => "openai-codex",
            "nous" => "nous",
            "qwen-oauth" => "qwen-oauth",
            "anthropic" | "claude" | "claude-code" => "anthropic",
            "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => "google-gemini-cli",
            _ => return None,
        };
        let path = self.auth_tokens_path();
        let raw = std::fs::read_to_string(path).ok()?;
        let entries: HashMap<String, OAuthStoreCredential> = serde_json::from_str(&raw).ok()?;
        let cred = entries.get(provider_key)?;
        if cred.access_token.trim().is_empty() {
            return None;
        }
        if cred
            .expires_at
            .map(|exp| exp <= Utc::now())
            .unwrap_or(false)
        {
            return None;
        }
        Some(cred.access_token.clone())
    }

    fn oauth_refresh_config(&self, provider_key: &str) -> Option<(String, String)> {
        // Preferred source: unified provider config centre (runtime_providers).
        let cfg_token_url = self
            .config()
            .runtime_providers
            .get(provider_key)
            .and_then(|c| c.oauth_token_url.as_deref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let cfg_client_id = self
            .config()
            .runtime_providers
            .get(provider_key)
            .and_then(|c| c.oauth_client_id.as_deref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Env fallback - keeps previous behavior working when config centre is empty.
        let (token_url_env, client_id_env) = match provider_key {
            "openai" => (
                "HERMES_OPENAI_OAUTH_TOKEN_URL",
                "HERMES_OPENAI_OAUTH_CLIENT_ID",
            ),
            "openai-codex" => (
                "HERMES_OPENAI_CODEX_OAUTH_TOKEN_URL",
                "HERMES_OPENAI_CODEX_OAUTH_CLIENT_ID",
            ),
            "nous" => ("HERMES_NOUS_OAUTH_TOKEN_URL", "HERMES_NOUS_OAUTH_CLIENT_ID"),
            "qwen-oauth" => ("HERMES_QWEN_OAUTH_TOKEN_URL", "HERMES_QWEN_OAUTH_CLIENT_ID"),
            "anthropic" => (
                "HERMES_ANTHROPIC_OAUTH_TOKEN_URL",
                "HERMES_ANTHROPIC_OAUTH_CLIENT_ID",
            ),
            _ => return cfg_token_url.zip(cfg_client_id),
        };
        let token_url = cfg_token_url
            .or_else(|| {
                std::env::var(token_url_env)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| match provider_key {
                "openai" => std::env::var("HERMES_OPENAI_CODEX_OAUTH_TOKEN_URL")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .or_else(|| Some("https://auth.openai.com/oauth/token".to_string())),
                "nous" => std::env::var("NOUS_PORTAL_BASE_URL")
                    .ok()
                    .map(|s| s.trim().trim_end_matches('/').to_string())
                    .filter(|s| !s.is_empty())
                    .map(|base| format!("{base}/api/oauth/token"))
                    .or_else(|| {
                        Some("https://portal.nousresearch.com/api/oauth/token".to_string())
                    }),
                "anthropic" => Some("https://console.anthropic.com/v1/oauth/token".to_string()),
                _ => None,
            })?;
        let client_id = cfg_client_id
            .or_else(|| {
                std::env::var(client_id_env)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| match provider_key {
                "openai" => std::env::var("HERMES_OPENAI_CODEX_OAUTH_CLIENT_ID")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .or_else(|| Some("app_EMoamEEZ73f0CkXaXp7hrann".to_string())),
                "nous" => std::env::var("NOUS_CLIENT_ID")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .or_else(|| Some("hermes-cli".to_string())),
                "anthropic" => Some("9d1c250a-e61b-44d9-88ed-5944d1962f5e".to_string()),
                _ => None,
            })?;
        Some((token_url, client_id))
    }

    fn auth_tokens_path(&self) -> PathBuf {
        let hermes_home = self
            .config()
            .hermes_home
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HERMES_HOME").ok().map(PathBuf::from))
            .or_else(|| dirs::home_dir().map(|h| h.join(".hermes")))
            .unwrap_or_else(|| PathBuf::from(".hermes"));
        hermes_home.join("auth").join("tokens.json")
    }

    fn objective_runtime_ledger_path(&self) -> PathBuf {
        let hermes_home = self
            .config()
            .hermes_home
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HERMES_HOME").ok().map(PathBuf::from))
            .or_else(|| dirs::home_dir().map(|h| h.join(".hermes")))
            .unwrap_or_else(|| PathBuf::from(".hermes"));
        hermes_home
            .join("alpha")
            .join("objective_runtime_ledger.jsonl")
    }

    fn objective_eval_trend_path(&self) -> PathBuf {
        let hermes_home = self
            .config()
            .hermes_home
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HERMES_HOME").ok().map(PathBuf::from))
            .or_else(|| dirs::home_dir().map(|h| h.join(".hermes")))
            .unwrap_or_else(|| PathBuf::from(".hermes"));
        hermes_home.join("alpha").join("objective_eval_trend.json")
    }

    fn append_objective_eval_sample(
        &self,
        objective_id: &str,
        objective_state: &str,
        note: &str,
    ) -> Result<(), AgentError> {
        let path = self.objective_eval_trend_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AgentError::Io(format!("create {} failed: {}", parent.display(), e))
            })?;
        }
        let mut root: serde_json::Value = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .unwrap_or_else(
                    || serde_json::json!({"updated_at": Utc::now().to_rfc3339(), "samples": []}),
                )
        } else {
            serde_json::json!({"updated_at": Utc::now().to_rfc3339(), "samples": []})
        };
        let Some(samples) = root.get_mut("samples").and_then(|v| v.as_array_mut()) else {
            root = serde_json::json!({"updated_at": Utc::now().to_rfc3339(), "samples": []});
            let samples = root
                .get_mut("samples")
                .and_then(|v| v.as_array_mut())
                .ok_or_else(|| {
                    AgentError::Config("objective_eval_trend samples field missing".to_string())
                })?;
            samples.push(serde_json::json!({
                "recorded_at": Utc::now().to_rfc3339(),
                "objective_id": objective_id,
                "objective_state": objective_state,
                "score": objective_eval_score(objective_state),
                "note": note,
            }));
            root["updated_at"] = serde_json::json!(Utc::now().to_rfc3339());
            let payload = serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".to_string());
            std::fs::write(&path, payload)
                .map_err(|e| AgentError::Io(format!("write {} failed: {}", path.display(), e)))?;
            return Ok(());
        };
        samples.push(serde_json::json!({
            "recorded_at": Utc::now().to_rfc3339(),
            "objective_id": objective_id,
            "objective_state": objective_state,
            "score": objective_eval_score(objective_state),
            "note": note,
        }));
        if samples.len() > 512 {
            let drain = samples.len().saturating_sub(512);
            samples.drain(0..drain);
        }
        root["updated_at"] = serde_json::json!(Utc::now().to_rfc3339());
        let payload = serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".to_string());
        std::fs::write(&path, payload)
            .map_err(|e| AgentError::Io(format!("write {} failed: {}", path.display(), e)))?;
        Ok(())
    }

    pub(crate) fn append_objective_runtime_ledger(
        &self,
        messages: &[Message],
        assistant_text: &str,
        total_turns: u32,
    ) -> Result<(), AgentError> {
        let Some(objective) = extract_session_objective(messages) else {
            return Ok(());
        };
        if objective.trim().is_empty() {
            return Ok(());
        }
        let objective_id = short_sha256_hex(&format!("objective:{}", objective))
            .chars()
            .take(12)
            .collect::<String>();
        let objective_state = extract_objective_state_marker(assistant_text);
        let evidence_files = extract_marker_values(assistant_text, "path=", 12);
        let evidence_commands = extract_marker_values(assistant_text, "cmd=", 12);
        let decision = if objective_state == "advancing" {
            "promote"
        } else if objective_state == "regressing" {
            "investigate"
        } else if objective_state == "unproven" {
            "collect-more-evidence"
        } else {
            "monitor"
        };
        let entry = serde_json::json!({
            "recorded_at": Utc::now().to_rfc3339(),
            "objective_id": format!("obj-{}", objective_id),
            "objective_state": objective_state,
            "decision": decision,
            "turns": total_turns,
            "evidence_files": evidence_files,
            "evidence_commands": evidence_commands,
        });
        let path = self.objective_runtime_ledger_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AgentError::Io(format!("create {} failed: {}", parent.display(), e))
            })?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| AgentError::Io(format!("open {} failed: {}", path.display(), e)))?;
        writeln!(file, "{}", entry)
            .map_err(|e| AgentError::Io(format!("append {} failed: {}", path.display(), e)))?;
        self.append_objective_eval_sample(
            &format!("obj-{}", objective_id),
            &objective_state,
            &format!("decision={decision} turns={total_turns}"),
        )?;
        Ok(())
    }

    fn resolve_runtime_command_args(
        &self,
        provider: Option<&str>,
    ) -> (Option<String>, Vec<String>) {
        let mut command = self
            .config()
            .acp_command
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let mut args: Vec<String> = self
            .config()
            .acp_args
            .iter()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect();

        if let Some(provider) = provider {
            if let Some(cfg) = self.config().runtime_providers.get(provider) {
                if let Some(cmd) = cfg
                    .command
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    command = Some(cmd.to_string());
                }
                if !cfg.args.is_empty() {
                    args = cfg
                        .args
                        .iter()
                        .map(|a| a.trim().to_string())
                        .filter(|a| !a.is_empty())
                        .collect();
                }
            }
            if provider == "copilot-acp" {
                if command.is_none() {
                    command = std::env::var("HERMES_COPILOT_ACP_COMMAND")
                        .ok()
                        .or_else(|| std::env::var("COPILOT_CLI_PATH").ok())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .or_else(|| Some("copilot".to_string()));
                }
                if args.is_empty() {
                    args = std::env::var("HERMES_COPILOT_ACP_ARGS")
                        .ok()
                        .and_then(|raw| shlex::split(raw.trim()))
                        .filter(|v| !v.is_empty())
                        .unwrap_or_else(|| vec!["--acp".to_string(), "--stdio".to_string()]);
                }
                if let Some(cmd) = command.as_deref() {
                    if let Ok(resolved) = which::which(cmd) {
                        command = Some(resolved.to_string_lossy().to_string());
                    }
                }
            }
        }
        (command, args)
    }

    fn resolve_runtime_request_timeout_seconds(&self, provider: &str) -> Option<f64> {
        self.config()
            .runtime_providers
            .get(provider)
            .and_then(|c| c.request_timeout_seconds)
            .or_else(|| {
                let alias = match provider {
                    "codex" => "openai-codex",
                    "openai-codex" => "codex",
                    "qwen" => "qwen-oauth",
                    "qwen-oauth" => "qwen",
                    "kimi" => "moonshot",
                    "moonshot" => "kimi",
                    _ => return None,
                };
                self.config()
                    .runtime_providers
                    .get(alias)
                    .and_then(|c| c.request_timeout_seconds)
            })
    }

    pub(crate) fn build_runtime_provider(
        &self,
        provider: &str,
        model_name: &str,
        route_base_url: Option<&str>,
        api_key_env_override: Option<&str>,
        explicit_api_key: Option<&str>,
        api_mode: Option<&ApiMode>,
        credential_pool: Option<&Arc<CredentialPool>>,
    ) -> Result<Arc<dyn LlmProvider>, AgentError> {
        crate::runtime_provider::build_runtime_provider(
            self,
            provider,
            model_name,
            route_base_url,
            api_key_env_override,
            explicit_api_key,
            api_mode,
            credential_pool,
        )
    }

    pub(crate) fn credential_pool_for_route<'a>(
        &'a self,
        rt: &'a TurnRuntimeRoute,
    ) -> Option<&'a Arc<CredentialPool>> {
        crate::runtime_provider::credentials_pool_for_route(self, rt)
    }

    /// Recompute prompt-cache policy from current route (Python `_anthropic_prompt_cache_policy`).
    pub fn refresh_prompt_cache_policy(&self, provider: &str, base_url: &str, api_mode: &str) {
        crate::runtime_provider::refresh_prompt_cache_policy(self, provider, base_url, api_mode)
    }

    async fn context_compression_should_run(&self, ctx: &ContextManager) -> bool {
        let total_chars = ctx.total_chars();
        let max_c = ctx.max_context_chars().max(1);
        let char_threshold = (max_c as f64 * 0.8) as usize;
        if total_chars > char_threshold {
            return true;
        }
        let system_prompt = ctx
            .get_messages()
            .first()
            .filter(|m| m.role == MessageRole::System)
            .and_then(|m| m.content.as_deref())
            .unwrap_or("");
        let tool_schemas = self.tool_registry.schemas();
        let estimated = estimate_request_tokens_for_compression(
            ctx.get_messages(),
            system_prompt,
            &tool_schemas,
        );
        self.context_compressor
            .lock()
            .await
            .should_compress(Some(estimated))
    }

    /// Run context compression on `ctx` (auxiliary LLM summary + tool-pair sanitiser).
    /// Returns `true` when messages were actually compressed and session rotation occurred.
    pub(crate) async fn compress_context(&self, ctx: &mut ContextManager) -> bool {
        let task_hint = ctx
            .get_messages()
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let tool_schemas = self.tool_registry.schemas();
        let old_session_id = self.config().session_id.clone().unwrap_or_default();
        let lock_holder = self.compression_lock_holder();
        let sp = self.session_persistence();
        let lock_acquired = if old_session_id.is_empty() {
            true
        } else if let Some(ref db) = sp {
            db.try_acquire_compression_lock(&old_session_id, &lock_holder, 300.0)
                .unwrap_or(false)
        } else {
            true
        };
        if !lock_acquired {
            if let (Some(db), true) = (&sp, !old_session_id.is_empty()) {
                if let Ok(existing) = db.get_compression_lock_holder(&old_session_id) {
                    tracing::warn!(
                        session_id = %old_session_id,
                        holder = ?existing,
                        "compression skipped: another path holds the compression lock"
                    );
                }
            }
            return false;
        }

        let pre_len = ctx.get_messages().len();
        let context_length = get_model_context_length(&crate::runtime_provider::active_model(self));
        let messages = ctx.get_messages().to_vec();
        let memory_hint = self
            .memory_pre_compress_note(&messages)
            .filter(|n| !n.trim().is_empty());
        let estimated_tokens = estimate_messages_tokens(&messages);
        let compressed = {
            let mut compressor = self.context_compressor.lock().await;
            compressor.set_context_length(context_length);
            compressor
                .compress(messages, Some(estimated_tokens), memory_hint.as_deref())
                .await
        };

        let release_lock = || {
            if let (Some(db), true) = (&sp, !old_session_id.is_empty()) {
                let _ = db.release_compression_lock(&old_session_id, &lock_holder);
            }
        };

        if compressed.len() >= pre_len {
            release_lock();
            return false;
        }

        self.invalidate_cached_system_prompt();
        let (new_system, _) = self.active_cached_system_prompt(&task_hint, &tool_schemas);
        let mut final_messages = compressed;
        Self::patch_leading_system_message(&mut final_messages, &new_system);
        ctx.replace_messages(final_messages.clone());
        self.reset_interest_sync_cursor();
        self.invalidate_turn_api_messages_cache();

        let new_session_id = Self::new_compression_session_id();
        if let Ok(mut guard) = self.config_runtime.write() {
            let mut updated = (*guard).as_ref().clone();
            updated.session_id = Some(new_session_id.clone());
            *guard = Arc::new(updated);
        }
        self.memory_on_session_switch(&new_session_id, &old_session_id, false, "compression");
        self.reset_session_db_flush_cursor();

        if let Some(ref db) = sp {
            let cfg = self.config();
            let platform = cfg.platform.as_deref();
            let model = crate::runtime_provider::active_model(self);
            let _ = db.create_compression_continuation_session(
                &new_session_id,
                &old_session_id,
                Some(model.as_str()),
                platform,
                &new_system,
            );
            let transcript: Vec<Message> = final_messages
                .iter()
                .filter(|m| m.role != MessageRole::System)
                .cloned()
                .collect();
            let mut cursor = SessionFlushCursor::new();
            let _ = db.replace_session_messages(&new_session_id, &transcript, &mut cursor);
            let _ = db.update_system_prompt(&new_session_id, &new_system);
        }

        release_lock();
        true
    }

    /// Compress when char budget or model token threshold is exceeded (Python auto-compaction).
    pub(crate) async fn auto_compress_if_over_threshold(&self, ctx: &mut ContextManager) {
        let total_chars = ctx.total_chars();
        let max_c = ctx.max_context_chars().max(1);
        if !self.context_compression_should_run(ctx).await {
            return;
        }
        let pct = (total_chars * 100) / max_c;
        tracing::info!("Context pressure at {}%, triggering compression", pct);
        self.compress_context(ctx).await;
        let after_chars = ctx.total_chars();
        self.emit_compaction_contextlattice_checkpoint(total_chars, after_chars, max_c);
    }

    fn emit_compaction_contextlattice_checkpoint(
        &self,
        before_chars: usize,
        after_chars: usize,
        max_context_chars: usize,
    ) {
        let mode = compaction_governance_mode_runtime();
        if matches!(mode, CompactionGovernanceMode::Off) {
            return;
        }

        let Some(script_path) = contextlattice_orchestration_script_path() else {
            if matches!(mode, CompactionGovernanceMode::Enforce) {
                crate::hooks::emit_status(
                    self,
                    "lifecycle",
                    "Compaction governance enforce-mode: ContextLattice script missing; checkpoint skipped.",
                );
            }
            return;
        };

        let session = self
            .config()
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "session".to_string());
        let session = session.as_str();
        let topic = format!("runbooks/alpha/compaction/{}", session);
        let pressure_before = ((before_chars as f64 / max_context_chars as f64) * 100.0).round();
        let pressure_after = ((after_chars as f64 / max_context_chars as f64) * 100.0).round();
        let content = format!(
            "compaction_event mode={} session={} before_chars={} after_chars={} max_chars={} pressure_before_pct={} pressure_after_pct={}",
            mode.as_str(),
            session,
            before_chars,
            after_chars,
            max_context_chars,
            pressure_before,
            pressure_after
        );

        let output = Command::new("python3")
            .arg(script_path)
            .arg("write")
            .arg("hermes-agent-ultra")
            .arg(topic)
            .arg(content)
            .env(
                "MEMMCP_ORCHESTRATOR_URL",
                std::env::var("MEMMCP_ORCHESTRATOR_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:8075".to_string()),
            )
            .env(
                "CONTEXTLATTICE_ORCHESTRATOR_URL",
                std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:8075".to_string()),
            )
            .env(
                "CONTEXTLATTICE_AGENT_ID",
                std::env::var("CONTEXTLATTICE_AGENT_ID")
                    .unwrap_or_else(|_| "codex_gpt5".to_string()),
            )
            .env(
                "MEMMCP_AGENT_ID",
                std::env::var("MEMMCP_AGENT_ID").unwrap_or_else(|_| "codex_gpt5".to_string()),
            )
            .output();

        match output {
            Ok(out) if out.status.success() => {
                crate::hooks::emit_status(
                    self,
                    "lifecycle",
                    &format!(
                        "ContextLattice compaction checkpoint written ({}% -> {}%).",
                        pressure_before, pressure_after
                    ),
                );
            }
            Ok(out) => {
                if matches!(mode, CompactionGovernanceMode::Enforce) {
                    crate::hooks::emit_status(
                        self,
                        "lifecycle",
                        &format!(
                            "Compaction governance enforce-mode: checkpoint failed (exit={}) {}",
                            out.status.code().unwrap_or(-1),
                            String::from_utf8_lossy(&out.stderr)
                        ),
                    );
                }
            }
            Err(err) => {
                if matches!(mode, CompactionGovernanceMode::Enforce) {
                    crate::hooks::emit_status(
                        self,
                        "lifecycle",
                        &format!(
                            "Compaction governance enforce-mode: checkpoint error: {}",
                            err
                        ),
                    );
                }
            }
        }
    }

    /// Drop oldest non-system messages until context is at or below `target_percent` of max.
    fn emergency_trim_context_to_percent(&self, ctx: &mut ContextManager, target_percent: usize) {
        let max_c = ctx.max_context_chars().max(1);
        let target_chars = (max_c * target_percent.min(100)) / 100;
        if ctx.total_chars() <= target_chars {
            return;
        }
        let before = ctx.total_chars();
        let budget = hermes_core::BudgetConfig {
            max_aggregate_chars: target_chars,
            max_result_size_chars: 100_000,
        };
        ctx.truncate_to_budget(&budget);
        let after = ctx.total_chars();
        tracing::warn!(
            "Emergency context trim: {} -> {} chars (target {}% of {} max)",
            before,
            after,
            target_percent,
            max_c
        );
    }

    /// Emit explicit preflight compression status before first LLM call.
    pub(crate) async fn preflight_context_compress_with_status(&self, ctx: &mut ContextManager) {
        let model = crate::runtime_provider::active_model(self);
        let model_tokens = get_model_context_length(model.as_str());
        let max_c = ctx.max_context_chars().max(1);
        let before = ctx.total_chars();
        let before_pct = (before * 100) / max_c;
        let gateway_msgs = ctx
            .get_messages()
            .iter()
            .filter(|m| m.role != MessageRole::System)
            .count();
        if !self.context_compression_should_run(ctx).await {
            tracing::debug!(
                model = %model,
                model_context_tokens = model_tokens,
                max_context_chars = max_c,
                transcript_chars = before,
                gateway_messages = gateway_msgs,
                context_usage_pct = before_pct,
                "Preflight compression check: no compression needed"
            );
            return;
        }
        tracing::info!(
            model = %model,
            model_context_tokens = model_tokens,
            max_context_chars = max_c,
            transcript_chars = before,
            gateway_messages = gateway_msgs,
            context_usage_pct = before_pct,
            "📦 Preflight compression: preparing session"
        );
        // Avoid auxiliary summarisation on multi-megabyte histories (very slow, often ineffective).
        if before_pct > 150 {
            let trim_target = if before_pct > 400 { 40 } else { 60 };
            self.emergency_trim_context_to_percent(ctx, trim_target);
            if !self.context_compression_should_run(ctx).await {
                let after_pct = (ctx.total_chars() * 100) / max_c;
                tracing::info!(
                    "Preflight: emergency trim sufficient ({}% -> {}%)",
                    before_pct,
                    after_pct
                );
                return;
            }
        }
        self.auto_compress_if_over_threshold(ctx).await;
        let mut after = ctx.total_chars();
        let mut after_pct = (after * 100) / max_c;
        let threshold_pct = {
            let compressor = self.context_compressor.lock().await;
            (compressor.threshold_percent() * 100.0) as usize
        };
        if after_pct >= threshold_pct {
            self.emergency_trim_context_to_percent(ctx, 50);
            after = ctx.total_chars();
            after_pct = (after * 100) / max_c;
        }
        tracing::info!(
            "Preflight compression complete: {}% -> {}% context usage",
            before_pct,
            after_pct
        );
        if after_pct >= threshold_pct {
            crate::hooks::emit_status(
                self,
                "lifecycle",
                &format!(
                    "会话上下文仍超过窗口容量（约 {}%）。请发送 /new 或 /reset 开始新会话后再问。",
                    after_pct
                ),
            );
            tracing::warn!(
                "Preflight compression did not reduce context enough ({}% -> {}%): \
                 LLM call may hit context limit",
                before_pct,
                after_pct
            );
        }
    }

    pub(crate) fn should_emit_context_pressure_warning(
        progress_ratio: f64,
        tier: f64,
        warned_tier: &mut f64,
        last_warn_at: &mut Option<Instant>,
        last_warn_percent: &mut f64,
    ) -> bool {
        if tier <= 0.0 {
            return false;
        }
        let progress_percent = progress_ratio * 100.0;
        let now = Instant::now();
        const WARN_COOLDOWN_SECS: u64 = 20;
        const WARN_PERCENT_STEP: f64 = 5.0;

        let tier_upgraded = tier > *warned_tier;
        let cooldown_elapsed = last_warn_at
            .map(|t| now.duration_since(t) >= Duration::from_secs(WARN_COOLDOWN_SECS))
            .unwrap_or(true);
        let percent_advanced = (progress_percent - *last_warn_percent) >= WARN_PERCENT_STEP;

        if tier_upgraded || (cooldown_elapsed && percent_advanced) {
            if tier_upgraded {
                *warned_tier = tier;
            }
            *last_warn_at = Some(now);
            *last_warn_percent = progress_percent;
            return true;
        }
        false
    }

    pub(crate) fn assistant_visible_text(m: &Message) -> bool {
        m.content
            .as_deref()
            .map(str::trim)
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    pub(crate) fn assistant_visible_text_after_think_blocks(m: &Message) -> bool {
        let Some(content) = m.content.as_deref() else {
            return false;
        };
        !agent_runtime_helpers::strip_think_blocks(content)
            .trim()
            .is_empty()
    }

    pub(crate) fn assistant_has_reasoning(m: &Message) -> bool {
        m.reasoning_content
            .as_deref()
            .map(str::trim)
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    fn finish_reason_requires_continuation(finish_reason: Option<&str>) -> bool {
        matches!(finish_reason, Some("length" | "pause_turn"))
    }

    fn assemble_stream_assistant_message(
        content: &str,
        reasoning_content: &str,
        tool_calls: &[ToolCall],
    ) -> Message {
        if tool_calls.is_empty() || tool_calls.iter().all(|tc| tc.function.name.is_empty()) {
            let mut m = Message::assistant(content.to_string());
            if !reasoning_content.is_empty() {
                m.reasoning_content = Some(reasoning_content.to_string());
            }
            m
        } else {
            let content_opt = if content.is_empty() {
                None
            } else {
                Some(content.to_string())
            };
            let mut m = Message::assistant_with_tool_calls(content_opt, tool_calls.to_vec());
            if !reasoning_content.is_empty() {
                m.reasoning_content = Some(reasoning_content.to_string());
            }
            m
        }
    }

    fn partial_stream_stub_outcome(
        recovered_text: &str,
        tool_calls: &[ToolCall],
        last_usage: Option<UsageStats>,
        model: &str,
        on_chunk: &(dyn Fn(StreamChunk) + Send + Sync),
        err: &AgentError,
    ) -> StreamCollectOutcome {
        let dropped = partial_stream_dropped_tool_names(tool_calls);
        let mut content = recovered_text.to_string();
        if !dropped.is_empty() {
            let warn = format_partial_stream_tool_call_warning(&dropped);
            on_chunk(StreamChunk {
                delta: Some(hermes_core::StreamDelta {
                    content: Some(warn.clone()),
                    tool_calls: None,
                    extra: None,
                }),
                finish_reason: None,
                usage: None,
            });
            content.push_str(&warn);
            tracing::warn!(
                dropped_tools = ?dropped,
                recovered_chars = recovered_text.chars().count(),
                error = %err,
                "Partial stream dropped tool call(s); returning length stub for continuation"
            );
        } else {
            tracing::warn!(
                recovered_chars = recovered_text.chars().count(),
                error = %err,
                "Partial stream delivered before error; returning length stub for continuation"
            );
        }
        let mut response = build_partial_stream_stub_response(
            model,
            content,
            if dropped.is_empty() {
                None
            } else {
                Some(dropped)
            },
        );
        response.usage = last_usage;
        StreamCollectOutcome::Complete(response)
    }

    /// Collect one streaming completion into [`LlmResponse`] (first attempt in `run_stream` D-step).

    /// Expand `@file:` / `@diff` / … tokens in user messages before the LLM sees them.
    ///
    /// Mirrors Python `agent.context_references.preprocess_context_references_async`
    /// (also invoked from gateway/CLI before `run_conversation` on some paths). Both
    /// `run` and `run_stream` call this so streaming callers get the same expansion.
    fn context_reference_workspace_root() -> PathBuf {
        std::env::var("TERMINAL_CWD")
            .ok()
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    pub(crate) async fn preprocess_user_message_context_references(
        &self,
        messages: &mut [Message],
    ) {
        let cwd = Self::context_reference_workspace_root();
        let context_length = get_model_context_length(&crate::runtime_provider::active_model(self));
        for msg in messages.iter_mut() {
            if msg.role != MessageRole::User {
                continue;
            }
            let Some(content) = msg.content.clone() else {
                continue;
            };
            let result =
                preprocess_context_references_async(&content, &cwd, context_length, Some(&cwd))
                    .await;
            if result.expanded && result.message != content {
                msg.content = Some(result.message);
            }
        }
    }

    /// Per-turn message prelude (sanitize, budget strip, @file expansion, restore primary).
    pub(crate) async fn apply_turn_message_prelude(&self, messages: &mut Vec<Message>) {
        for msg in messages.iter_mut() {
            if let Some(ref mut c) = msg.content {
                *c = sanitize_surrogates(c).into_owned();
            }
        }
        strip_budget_warnings_from_messages(messages);
        self.preprocess_user_message_context_references(messages)
            .await;
        self.restore_primary_runtime_at_turn_start();
    }

    /// Ask the LLM for a final summary when the turn budget is exhausted.
    pub(crate) async fn handle_max_iterations(
        &self,
        ctx: &mut ContextManager,
    ) -> Result<Option<Message>, AgentError> {
        if hermes_tools::kanban_task_from_env().is_some() {
            let block = hermes_tools::kanban_block_reason(Some("iteration_budget_exhausted"));
            ctx.add_message(Message::tool_result(
                "kanban_block",
                serde_json::to_string(&block).unwrap_or_else(|_| block.to_string()),
            ));
            return Ok(None);
        }
        ctx.add_message(Message::system(
            "[SYSTEM] Maximum conversation turns reached. Please provide a brief summary of \
             what was accomplished and any remaining tasks.",
        ));
        let runtime = crate::route_learning::primary_runtime_snapshot(self);
        let (_, model_name) =
            crate::route_learning::extract_provider_and_model(self, runtime.model.as_str());
        let response = self
            .llm_provider
            .chat_completion(
                ctx.get_messages(),
                &[],
                self.config().max_tokens,
                self.config().temperature,
                Some(model_name),
                crate::llm_caller::extra_body_for_api_mode(self, &runtime.api_mode).as_ref(),
            )
            .await
            .map_err(|e| AgentError::LlmApi(e.to_string()))?;
        Ok(Some(response.message))
    }

    pub(crate) async fn handle_tool_loop_guard_summary(
        &self,
        ctx: &mut ContextManager,
        consecutive_error_turns: u32,
        failed_calls: u32,
        total_calls: usize,
    ) -> Result<Option<Message>, AgentError> {
        ctx.add_message(Message::system(format!(
            "[SYSTEM] Tool-loop guard triggered after {} consecutive error turn(s). Latest turn failed {}/{} tool call(s). Stop calling tools and provide a concise final response with what succeeded, what failed, and precise next manual step(s).",
            consecutive_error_turns, failed_calls, total_calls
        )));
        let runtime = crate::route_learning::primary_runtime_snapshot(self);
        let (_, model_name) =
            crate::route_learning::extract_provider_and_model(self, runtime.model.as_str());
        let response = self
            .llm_provider
            .chat_completion(
                ctx.get_messages(),
                &[],
                self.config().max_tokens,
                self.config().temperature,
                Some(model_name),
                crate::llm_caller::extra_body_for_api_mode(self, &runtime.api_mode).as_ref(),
            )
            .await
            .map_err(|e| AgentError::LlmApi(e.to_string()))?;
        Ok(Some(response.message))
    }

    pub(crate) fn emit_background_review_metrics(&self, turn: u32, ctx: &ContextManager) {
        if !self.config().background_review_metrics_enabled {
            return;
        }
        let snapshot = ctx.get_messages().to_vec();
        tokio::spawn(async move {
            let tool_msg_count = snapshot
                .iter()
                .filter(|m| matches!(m.role, hermes_core::MessageRole::Tool))
                .count();
            tracing::debug!(
                turn,
                tool_messages = tool_msg_count,
                total_messages = snapshot.len(),
                "Background review snapshot captured"
            );
        });
    }

    /// Metrics (always) + optional Python-style memory/skill review LLM pass on session end.
    pub(crate) fn spawn_background_review(
        &self,
        turn: u32,
        ctx: &ContextManager,
        review_memory_at_end: bool,
        session_key: Option<&str>,
    ) {
        self.emit_background_review_metrics(turn, ctx);
        if !self.config().background_review_enabled {
            return;
        }
        let mut review_skills = false;
        if self.config().skill_creation_nudge_interval > 0
            && self
                .tool_registry
                .names()
                .iter()
                .any(|n| n == "skill_manage")
        {
            if let Ok(mut state) = self.state.lock() {
                if state.evolution_counters.iters_since_skill
                    >= self.config().skill_creation_nudge_interval
                {
                    review_skills = true;
                    state.evolution_counters.iters_since_skill = 0;
                }
            }
        }
        let review_memory = review_memory_at_end;
        if !review_memory && !review_skills {
            return;
        }
        let trigger = match crate::evolution_ledger::review_trigger(review_memory, review_skills) {
            Some(t) => t,
            None => return,
        };
        let prompt: &'static str = match (review_memory, review_skills) {
            (true, true) => COMBINED_REVIEW_PROMPT,
            (true, false) => MEMORY_REVIEW_PROMPT,
            (false, true) => SKILL_REVIEW_PROMPT,
            _ => return,
        };
        let ledger_enabled = crate::evolution_ledger::evolution_ledger_enabled(self.config().as_ref());
        let hermes_home = crate::evolution_ledger::resolve_hermes_home(self.config().as_ref());
        let ledger_max = self.config().evolution_ledger_max_entries;
        let review_id = crate::evolution_ledger::new_review_id();
        let session_key_owned = session_key.map(str::to_string);
        if ledger_enabled {
            let started = crate::evolution_ledger::started_event(
                review_id.clone(),
                session_key_owned.clone(),
                trigger,
            );
            if let Err(e) = crate::evolution_ledger::append_event(&hermes_home, &started, ledger_max)
            {
                tracing::debug!(error = %e, "evolution ledger append (started) failed");
            }
        }
        let mut hist = ctx.get_messages().to_vec();
        hist.push(Message::user(prompt));
        let mut cfg = (*self.config()).clone();
        cfg.background_review_enabled = false;
        cfg.background_review_metrics_enabled = false;
        cfg.memory_nudge_interval = 0;
        cfg.skill_creation_nudge_interval = 0;
        cfg.max_concurrent_delegates = 0;
        cfg.quiet_mode = true;
        cfg.skip_memory = true;
        cfg.use_prompt_caching = self.config().use_prompt_caching;
        cfg.use_native_cache_layout = self.config().use_native_cache_layout;
        cfg.cache_ttl = self.config().cache_ttl.clone();
        if let Some(sys) = ctx
            .get_messages()
            .iter()
            .find(|m| m.role == MessageRole::System)
            .and_then(|m| m.content.clone())
            .filter(|s| !s.trim().is_empty())
        {
            cfg.stored_system_prompt = Some(sys);
        } else if let Some(sys) = self.config().stored_system_prompt.clone() {
            cfg.stored_system_prompt = Some(sys);
        }
        cfg.max_turns = if cfg.max_turns == 0 {
            16
        } else {
            cfg.max_turns.min(16)
        };
        let tools = self.tool_registry.clone();
        let provider = self.llm_provider.clone();
        let async_tool_dispatch = self.async_tool_dispatch();
        let review_cb = self.callbacks.background_review_callback.clone();
        tokio::spawn(async move {
            let timer = crate::evolution_ledger::ReviewTimer::start();
            let agent = AgentLoop::new(cfg, tools, provider)
                .maybe_with_async_tool_dispatch(async_tool_dispatch);
            match agent.run(hist, None).await {
                Ok(result) => {
                    let tools = crate::evolution_ledger::extract_review_tools(&result.messages);
                    let summary = crate::evolution_ledger::summarize_review_for_chat(&result.messages);
                    if ledger_enabled {
                        let completed = crate::evolution_ledger::completed_event(
                            review_id.clone(),
                            session_key_owned.clone(),
                            trigger,
                            timer.elapsed_ms(),
                            tools,
                            summary.clone(),
                        );
                        if let Err(e) =
                            crate::evolution_ledger::append_event(&hermes_home, &completed, ledger_max)
                        {
                            tracing::debug!(error = %e, "evolution ledger append (completed) failed");
                        }
                    }
                    if let Some(cb) = review_cb.as_ref() {
                        if let Some(summary) = summary {
                            cb(&summary);
                        }
                    }
                }
                Err(e) => {
                    if ledger_enabled {
                        let failed = crate::evolution_ledger::failed_event(
                            review_id,
                            session_key_owned,
                            trigger,
                            timer.elapsed_ms(),
                            e.to_string(),
                        );
                        if let Err(err) =
                            crate::evolution_ledger::append_event(&hermes_home, &failed, ledger_max)
                        {
                            tracing::debug!(error = %err, "evolution ledger append (failed) failed");
                        }
                    }
                    tracing::debug!(error = %e, "background memory/skill review failed");
                }
            }
        });
    }

    /// Recover todo-state hints from historical messages at loop start.
    pub(crate) fn hydrate_todo_store(&self, ctx: &ContextManager) {
        let todo_markers = ctx
            .get_messages()
            .iter()
            .filter_map(|m| m.content.as_deref())
            .filter(|c| c.contains("TODO") || c.contains("[ ]") || c.contains("[x]"))
            .count();
        if todo_markers > 0 {
            tracing::debug!(todo_markers, "Hydrated todo markers from prior context");
        }
    }
}

/// Extract the last user and assistant content from a message slice for memory sync.
pub(crate) fn extract_last_user_assistant(messages: &[Message]) -> (String, String) {
    let user = messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::User))
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    let assistant = messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::Assistant))
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    (user, assistant)
}

pub(crate) fn latest_user_content(messages: &[Message]) -> Option<&str> {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::User))
        .and_then(|m| m.content.as_deref())
}

fn session_search_has_query(tc: &ToolCall) -> bool {
    serde_json::from_str::<Value>(&tc.function.arguments)
        .ok()
        .and_then(|v| {
            v.get("query")
                .and_then(|q| q.as_str())
                .map(str::trim)
                .map(str::to_string)
        })
        .is_some_and(|q| !q.is_empty())
}

pub(crate) fn inject_runtime_tool_params(
    tool_name: &str,
    params: &mut Value,
    task_id: Option<&str>,
    user_task: Option<&str>,
) {
    if !params.is_object() {
        *params = serde_json::json!({});
    }
    let Some(obj) = params.as_object_mut() else {
        return;
    };

    if let Some(task_id) = task_id.filter(|v| !v.trim().is_empty()) {
        obj.entry("task_id".to_string())
            .or_insert_with(|| Value::String(task_id.to_string()));
    }
    if tool_name.starts_with("browser_") {
        if let Some(user_task) = user_task.filter(|v| !v.trim().is_empty()) {
            obj.entry("user_task".to_string())
                .or_insert_with(|| Value::String(user_task.to_string()));
        }
    }
}

fn extract_session_objective(messages: &[Message]) -> Option<String> {
    messages
        .iter()
        .find(|m| matches!(m.role, hermes_core::MessageRole::System))
        .and_then(|m| m.content.as_deref())
        .and_then(|content| content.strip_prefix(SESSION_OBJECTIVE_PREFIX))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn detect_repo_review_intent(messages: &[Message]) -> bool {
    let user = latest_user_content(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let objective = extract_session_objective(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();

    let combined = format!("{} {}", user, objective);
    let review_terms = [
        "repo",
        "repository",
        "codebase",
        "review",
        "audit",
        "inspect",
        "diagnose",
        "debug",
        "patch",
        "implement",
        "fix",
    ];
    let has_review_signal = review_terms.iter().any(|needle| combined.contains(needle));
    let has_path_signal =
        combined.contains('/') || combined.contains(".rs") || combined.contains(".py");
    has_review_signal && has_path_signal
}

fn detect_communication_intent(messages: &[Message]) -> bool {
    let text = latest_user_content(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let comm_terms = [
        "telegram",
        "discord",
        "slack",
        "whatsapp",
        "signal",
        "notify",
        "notification",
        "send message",
        "message me",
        "dm",
    ];
    comm_terms.iter().any(|needle| text.contains(needle))
}

fn detect_tool_profile_escape_hatch(messages: &[Message]) -> bool {
    let text = latest_user_content(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let escape_terms = [
        "allow all tools",
        "disable narrowing",
        "open tool profile",
        "no tool filtering",
        "bypass tool profile",
    ];
    escape_terms.iter().any(|needle| text.contains(needle))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepoReviewToolProfileMode {
    Off,
    Balanced,
    Focus,
}

impl RepoReviewToolProfileMode {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "open" => Some(Self::Off),
            "balanced" | "default" => Some(Self::Balanced),
            "focus" | "strict" => Some(Self::Focus),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Balanced => "balanced",
            Self::Focus => "focus",
        }
    }
}

fn repo_review_tool_profile_mode() -> RepoReviewToolProfileMode {
    std::env::var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE")
        .ok()
        .as_deref()
        .and_then(RepoReviewToolProfileMode::parse)
        .unwrap_or(RepoReviewToolProfileMode::Balanced)
}

pub(crate) fn exploratory_problem_solving_system_hint(messages: &[Message]) -> Option<String> {
    if !detect_repo_review_intent(messages) {
        return None;
    }
    let user = latest_user_content(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let objective = extract_session_objective(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let combined = format!("{} {}", user, objective);
    let exploratory = [
        "explore",
        "investigate",
        "understand",
        "diagnose",
        "audit",
        "deep",
        "root cause",
        "why",
    ]
    .iter()
    .any(|needle| combined.contains(needle));
    if !exploratory {
        return None;
    }
    Some(
        "[SYSTEM] Exploratory problem-solving protocol active. \
1) Start by declaring workstreams (`workstream=<name>`) that cover the full problem surface. \
2) Run focused evidence collection per workstream (`file=...`, `cmd=...`) rather than repeated broad scans. \
3) After each evidence batch, update status per workstream (`complete|blocked|unproven`) and refine next probes. \
4) Do not finalize until high-leverage workstreams are either complete or explicitly blocked with concrete blockers and next actions."
            .to_string(),
    )
}

fn detect_deep_repo_audit_intent(messages: &[Message]) -> bool {
    if !detect_repo_review_intent(messages) {
        return false;
    }
    let user = latest_user_content(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let objective = extract_session_objective(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let combined = format!("{} {}", user, objective);
    [
        "deep",
        "deeply",
        "comprehensive",
        "full ",
        "full-scope",
        "end-to-end",
        "line-by-line",
        "thorough",
        "complete",
        "surgical",
        "parity",
    ]
    .iter()
    .any(|needle| combined.contains(needle))
}

pub(crate) fn objective_guard_policy(messages: &[Message]) -> (bool, bool, bool) {
    let user = latest_user_content(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let objective = extract_session_objective(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let combined = format!("{} {}", user, objective);

    let objective_active = !objective.is_empty()
        || user.contains("/objective")
        || user.contains("objective:")
        || user.contains("goal:");
    let repo_like = detect_repo_review_intent(messages)
        || combined.contains("plan")
        || combined.contains("analysis")
        || combined.contains("review");
    let trading_like = [
        "solana",
        "wallet",
        "trade",
        "trading",
        "pnl",
        "profit",
        "exponent",
        "objective",
    ]
    .iter()
    .any(|needle| combined.contains(needle));
    let guard_active = objective_active && repo_like;
    let deep_audit_required = guard_active && detect_deep_repo_audit_intent(messages);

    (guard_active, trading_like, deep_audit_required)
}

pub(crate) fn objective_mode_system_hint(messages: &[Message]) -> Option<String> {
    let (guard_active, requires_analytics, deep_audit_required) = objective_guard_policy(messages);
    if !guard_active {
        return None;
    }
    let analytics_line = if requires_analytics {
        "2) ANALYTICS_VERIFIED: include copied metric values (or `objective_state=unproven` with blocker)."
    } else {
        "2) ANALYTICS_VERIFIED: include objective-state evidence relevant to this task."
    };
    let deep_audit_line = if deep_audit_required {
        format!(
            "3) {OBJECTIVE_DEEP_AUDIT_TAG} include `scope_complete=true|false`, at least {OBJECTIVE_DEEP_AUDIT_MIN_WORKSTREAMS} `workstream=<name> status=<complete|blocked|unproven> evidence(file=...|cmd=...)` lines, plus breadth evidence (>= {OBJECTIVE_DEEP_AUDIT_MIN_UNIQUE_FILES} unique files and >= {OBJECTIVE_DEEP_AUDIT_MIN_UNIQUE_COMMANDS} unique commands), and explicit `unknowns=` + `blockers=` fields."
        )
    } else {
        String::new()
    };
    Some(format!(
        "[SYSTEM] Objective-mode guard active. Before finalizing, output sections exactly:\n\
         1) {OBJECTIVE_PATCH_TAG} each proposed change must include `path=...` and `exists_now=true|false`.\n\
         {analytics_line}\n\
         {deep_audit_line}\n\
         Use only evidence verified in this run; if missing evidence, state `unproven` explicitly."
    ))
}

fn section_after_tag<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let tag_lc = tag.to_ascii_lowercase();
    let start = text.find(&tag_lc)?;
    Some(&text[start + tag_lc.len()..])
}

fn unique_values_for_markers(section: &str, markers: &[&str]) -> HashSet<String> {
    let mut values = HashSet::new();
    for raw_line in section.lines() {
        let line = raw_line.trim();
        for marker in markers {
            if let Some(idx) = line.find(marker) {
                let candidate = line[idx + marker.len()..]
                    .trim()
                    .trim_matches('`')
                    .trim_matches('"')
                    .trim_matches('\'');
                if candidate.is_empty() {
                    continue;
                }
                let token = candidate
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(',')
                    .trim_end_matches(';');
                if !token.is_empty() {
                    values.insert(token.to_string());
                }
                break;
            }
        }
    }
    values
}

fn deep_audit_workstream_lines(section: &str) -> Vec<String> {
    section
        .lines()
        .map(str::trim)
        .filter(|line| {
            line.contains("workstream=")
                || line.contains("workstream:")
                || line.contains("stream=")
                || line.contains("stream:")
        })
        .map(str::to_string)
        .collect()
}

fn workstream_line_has_terminal_status(line: &str) -> bool {
    line.contains("status=complete")
        || line.contains("status: complete")
        || line.contains("status=done")
        || line.contains("status: done")
        || line.contains("status=blocked")
        || line.contains("status: blocked")
        || line.contains("status=unproven")
        || line.contains("status: unproven")
}

fn workstream_line_is_complete(line: &str) -> bool {
    line.contains("status=complete")
        || line.contains("status: complete")
        || line.contains("status=done")
        || line.contains("status: done")
}

fn workstream_line_has_evidence(line: &str) -> bool {
    line.contains("file=")
        || line.contains("file:")
        || line.contains("path=")
        || line.contains("path:")
        || line.contains("cmd=")
        || line.contains("cmd:")
        || line.contains("command=")
        || line.contains("command:")
}

fn deep_audit_verified_patch_items(lower: &str) -> usize {
    let path_hits = ["path=", "path:"]
        .iter()
        .map(|needle| lower.matches(needle).count())
        .sum::<usize>();
    let exists_hits = [
        "exists_now=true",
        "exists_now=false",
        "exists_now: true",
        "exists_now: false",
        "verified_exists=true",
        "verified_exists=false",
        "verified_exists: true",
        "verified_exists: false",
    ]
    .iter()
    .map(|needle| lower.matches(needle).count())
    .sum::<usize>();
    path_hits.min(exists_hits)
}

fn objective_deep_audit_satisfied(lower: &str) -> bool {
    if !lower.contains(&OBJECTIVE_DEEP_AUDIT_TAG.to_ascii_lowercase()) {
        return false;
    }
    if deep_audit_verified_patch_items(lower) < OBJECTIVE_DEEP_AUDIT_MIN_PATCH_ITEMS {
        return false;
    }
    let section = section_after_tag(lower, OBJECTIVE_DEEP_AUDIT_TAG).unwrap_or_default();

    let workstream_lines = deep_audit_workstream_lines(section);
    if workstream_lines.len() < OBJECTIVE_DEEP_AUDIT_MIN_WORKSTREAMS {
        return false;
    }
    if workstream_lines.iter().any(|line| {
        !workstream_line_has_terminal_status(line) || !workstream_line_has_evidence(line)
    }) {
        return false;
    }

    let scope_complete_true =
        lower.contains("scope_complete=true") || lower.contains("scope_complete: true");
    let scope_complete_false =
        lower.contains("scope_complete=false") || lower.contains("scope_complete: false");
    if !(scope_complete_true || scope_complete_false) {
        return false;
    }
    if scope_complete_true
        && workstream_lines
            .iter()
            .any(|line| !workstream_line_is_complete(line))
    {
        return false;
    }

    let unique_files = unique_values_for_markers(section, &["file=", "file:", "path=", "path:"]);
    if unique_files.len() < OBJECTIVE_DEEP_AUDIT_MIN_UNIQUE_FILES {
        return false;
    }
    let unique_commands =
        unique_values_for_markers(section, &["cmd=", "cmd:", "command=", "command:"]);
    if unique_commands.len() < OBJECTIVE_DEEP_AUDIT_MIN_UNIQUE_COMMANDS {
        return false;
    }
    let has_unknowns_field = lower.contains("unknowns=") || lower.contains("unknowns:");
    let has_blockers_field = lower.contains("blockers=") || lower.contains("blockers:");
    has_unknowns_field && has_blockers_field
}

pub(crate) fn objective_guard_satisfied(
    text: &str,
    requires_analytics: bool,
    deep_audit_required: bool,
) -> bool {
    let lower = text.to_ascii_lowercase();
    let has_patch_tag = lower.contains(&OBJECTIVE_PATCH_TAG.to_ascii_lowercase());
    let has_patch_evidence = lower.contains("exists_now=true")
        || lower.contains("exists_now: true")
        || lower.contains("verified_exists=true");
    if !(has_patch_tag && has_patch_evidence) {
        return false;
    }
    if !requires_analytics {
        return true;
    }
    let has_analytics_tag = lower.contains(&OBJECTIVE_ANALYTICS_TAG.to_ascii_lowercase());
    let has_objective_state = lower.contains("objective_state=")
        || lower.contains("objective_state:")
        || lower.contains("metric=");
    let analytics_ok = has_analytics_tag && has_objective_state;
    if !analytics_ok {
        return false;
    }
    if deep_audit_required {
        return objective_deep_audit_satisfied(&lower);
    }
    true
}

pub(crate) fn objective_guard_retry_prompt(
    requires_analytics: bool,
    deep_audit_required: bool,
) -> String {
    let analytics_line = if requires_analytics {
        "Also include copied analytics values and `objective_state=<advancing|flat|regressing|unproven>`."
    } else {
        "Include objective-state evidence even if the objective is currently unproven."
    };
    let deep_audit_line = if deep_audit_required {
        format!(
            "{OBJECTIVE_DEEP_AUDIT_TAG}\n\
             - scope_complete=true|false\n\
             - workstream=<name> status=<complete|blocked|unproven> evidence(file=<path>|cmd=<command>)\n\
             - add at least {OBJECTIVE_DEEP_AUDIT_MIN_WORKSTREAMS} workstream lines\n\
             - file=<verified_path_1>\n\
             - file=<verified_path_2> ... (at least {OBJECTIVE_DEEP_AUDIT_MIN_UNIQUE_FILES} unique file lines)\n\
             - cmd=<command_1>\n\
             - cmd=<command_2> ... (at least {OBJECTIVE_DEEP_AUDIT_MIN_UNIQUE_COMMANDS} unique command lines)\n\
             - unknowns=<count>\n\
             - blockers=<none|list>\n\
             - include at least {OBJECTIVE_DEEP_AUDIT_MIN_PATCH_ITEMS} verified patch items in {OBJECTIVE_PATCH_TAG}"
        )
    } else {
        String::new()
    };
    format!(
        "[SYSTEM] Objective guard check failed. Re-issue your final response with required sections:\n\
         {OBJECTIVE_PATCH_TAG}\n\
         - path=<verified path>\n\
         - exists_now=true|false\n\
         {OBJECTIVE_ANALYTICS_TAG}\n\
         - objective_state=<value>\n\
         {analytics_line}\n\
         {deep_audit_line}"
    )
}

fn is_housekeeping_tool_name(name: &str) -> bool {
    matches!(
        name,
        "memory" | "todo" | "skill_manage" | "session_search" | "skills"
    )
}

fn is_discovery_tool_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("search")
        || lower.contains("find")
        || lower.contains("grep")
        || lower.contains("list")
        || lower.contains("read")
        || lower.contains("view")
        || lower.contains("scan")
        || lower.contains("context_pack")
        || lower == "terminal"
        || lower == "execute_code"
}

fn is_execution_tool_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "terminal" | "execute_code" | "apply_patch" | "edit_file" | "run_command"
    )
}

fn is_non_repo_tool_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "weather", "sports", "tarot", "zillow", "shopping", "gmail", "calendar", "artwork", "deal",
        "coursera", "datacamp", "jobkorea",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn is_messaging_tool_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "telegram",
        "discord",
        "slack",
        "mattermost",
        "signal",
        "whatsapp",
        "wecom",
        "weixin",
        "qqbot",
        "dingtalk",
        "feishu",
        "gmail",
        "calendar",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn compact_tool_args_for_signature(raw: &str) -> String {
    raw.split_whitespace().collect::<String>()
}

fn discovery_signature(tool_calls: &[ToolCall]) -> Option<String> {
    let mut fingerprints: Vec<String> = tool_calls
        .iter()
        .filter(|tc| is_discovery_tool_name(&tc.function.name))
        .map(|tc| {
            format!(
                "{}:{}",
                tc.function.name,
                compact_tool_args_for_signature(&tc.function.arguments)
            )
        })
        .collect();
    if fingerprints.is_empty() {
        return None;
    }
    fingerprints.sort();
    let mut hasher = Sha256::new();
    for fp in fingerprints {
        hasher.update(fp.as_bytes());
        hasher.update(b"\n");
    }
    Some(
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}

pub(crate) fn apply_repo_review_tool_profile_narrowing(
    tool_calls: &mut Vec<ToolCall>,
    messages: &[Message],
) -> Option<String> {
    if !detect_repo_review_intent(messages) {
        return None;
    }
    if detect_tool_profile_escape_hatch(messages) {
        return Some(
            "[SYSTEM] Repo-review tool profile narrowing bypassed by explicit operator escape hatch."
                .to_string(),
        );
    }
    let mode = repo_review_tool_profile_mode();
    if mode == RepoReviewToolProfileMode::Off {
        return None;
    }
    let allow_messaging = detect_communication_intent(messages);
    let mut filtered_messaging = 0usize;
    let mut filtered_non_repo = 0usize;
    let mut filtered_focus = 0usize;
    tool_calls.retain(|tc| {
        let mut should_filter = false;
        if is_messaging_tool_name(&tc.function.name) && !allow_messaging {
            filtered_messaging += 1;
            should_filter = true;
        } else if is_non_repo_tool_name(&tc.function.name)
            && !is_discovery_tool_name(&tc.function.name)
            && !is_execution_tool_name(&tc.function.name)
        {
            filtered_non_repo += 1;
            should_filter = true;
        } else if mode == RepoReviewToolProfileMode::Focus
            && !is_discovery_tool_name(&tc.function.name)
            && !is_execution_tool_name(&tc.function.name)
            && !is_housekeeping_tool_name(&tc.function.name)
            && !tc
                .function
                .name
                .to_ascii_lowercase()
                .contains("contextlattice")
        {
            filtered_focus += 1;
            should_filter = true;
        }
        if should_filter {
            return false;
        }
        true
    });
    let filtered = filtered_messaging + filtered_non_repo + filtered_focus;
    if filtered == 0 {
        return None;
    }
    Some(format!(
        "[SYSTEM] Repo-review tool profile narrowed this turn (mode={}): skipped {} low-signal call(s) (messaging={}, non-repo={}, focus={}) to keep focus on code evidence. `todo` remains enabled for task organization. If notifications are required, request telegram/discord/slack explicitly.",
        mode.as_str(),
        filtered,
        filtered_messaging,
        filtered_non_repo,
        filtered_focus
    ))
}

fn repo_review_repeat_threshold() -> u32 {
    std::env::var("HERMES_REPO_REVIEW_REPEAT_STREAK_THRESHOLD")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(2)
        .clamp(1, 12)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RepoReviewDiscoveryBudgetMode {
    Off,
    Advisory,
    Enforce,
}

impl RepoReviewDiscoveryBudgetMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Advisory => "advisory",
            Self::Enforce => "enforce",
        }
    }
}

fn repo_review_discovery_budget_mode() -> RepoReviewDiscoveryBudgetMode {
    let raw = std::env::var("HERMES_REPO_REVIEW_DISCOVERY_BUDGET_MODE")
        .ok()
        .unwrap_or_else(|| "advisory".to_string());
    match raw.trim().to_ascii_lowercase().as_str() {
        "0" | "off" | "disable" | "disabled" => RepoReviewDiscoveryBudgetMode::Off,
        "trim" | "hard" | "enforce" | "strict" => RepoReviewDiscoveryBudgetMode::Enforce,
        _ => RepoReviewDiscoveryBudgetMode::Advisory,
    }
}

fn repo_review_low_signal_threshold() -> u32 {
    std::env::var("HERMES_REPO_REVIEW_LOW_SIGNAL_STREAK_THRESHOLD")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(2)
        .clamp(1, 12)
}

fn repo_review_keep_limit_repeat() -> usize {
    std::env::var("HERMES_REPO_REVIEW_KEEP_LIMIT_REPEAT")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(2)
        .clamp(1, 12)
}

fn repo_review_keep_limit_low_signal() -> usize {
    std::env::var("HERMES_REPO_REVIEW_KEEP_LIMIT_LOW_SIGNAL")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 12)
}

fn repo_review_min_signal_score() -> f64 {
    std::env::var("HERMES_REPO_REVIEW_MIN_SIGNAL_SCORE")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(0.22)
        .clamp(0.0, 1.0)
}

pub(crate) fn apply_repo_review_discovery_budget_policy(
    tool_calls: &mut Vec<ToolCall>,
    messages: &[Message],
    state: &mut RepoReviewBudgetState,
) -> Option<String> {
    let mode = repo_review_discovery_budget_mode();
    if matches!(mode, RepoReviewDiscoveryBudgetMode::Off) {
        return None;
    }
    if !detect_repo_review_intent(messages) {
        *state = RepoReviewBudgetState::default();
        return None;
    }
    let Some(signature) = discovery_signature(tool_calls) else {
        state.repeat_streak = 0;
        state.last_discovery_signature = None;
        return None;
    };
    let only_discovery_or_housekeeping = tool_calls.iter().all(|tc| {
        is_discovery_tool_name(&tc.function.name) || is_housekeeping_tool_name(&tc.function.name)
    });

    if only_discovery_or_housekeeping
        && state.last_discovery_signature.as_deref() == Some(signature.as_str())
    {
        state.repeat_streak = state.repeat_streak.saturating_add(1);
    } else {
        state.repeat_streak = 0;
    }
    state.last_discovery_signature = Some(signature);

    let repeat_threshold = repo_review_repeat_threshold();
    let low_signal_threshold = repo_review_low_signal_threshold();
    let keep_limit_repeat = repo_review_keep_limit_repeat();
    let keep_limit_low_signal = repo_review_keep_limit_low_signal();

    let repeat_threshold_hit = state.repeat_streak >= repeat_threshold;
    let low_signal_threshold_hit = state.low_signal_streak >= low_signal_threshold;
    if (!repeat_threshold_hit && !low_signal_threshold_hit) || !only_discovery_or_housekeeping {
        return None;
    }

    if matches!(mode, RepoReviewDiscoveryBudgetMode::Advisory) {
        return Some(format!(
            "[SYSTEM] Discovery budget advisory (mode={} repeat_streak={} threshold={} low_signal_streak={} threshold={} last_signal_score={:.2} min_signal={:.2}). Tool calls are not trimmed in advisory mode. Prefer narrower path/glob scope, context-pack pivots, and then move to concrete patch synthesis.",
            mode.as_str(),
            state.repeat_streak + 1,
            repeat_threshold,
            state.low_signal_streak,
            low_signal_threshold,
            state.last_signal_score,
            repo_review_min_signal_score(),
        ));
    }

    let mut kept_per_tool: HashMap<String, usize> = HashMap::new();
    let mut removed = 0usize;
    let keep_limit = if low_signal_threshold_hit {
        keep_limit_low_signal
    } else {
        keep_limit_repeat
    };
    tool_calls.retain(|tc| {
        if !is_discovery_tool_name(&tc.function.name) {
            return true;
        }
        let counter = kept_per_tool.entry(tc.function.name.clone()).or_insert(0);
        if *counter < keep_limit {
            *counter += 1;
            true
        } else {
            removed += 1;
            false
        }
    });

    Some(format!(
        "[SYSTEM] Discovery budget policy engaged (repeat_streak={} threshold={} low_signal_streak={} threshold={} last_signal_score={:.2} min_signal={:.2}). {} duplicate low-yield discovery call(s) were trimmed (per-tool keep limit {}). Refine search scope with targeted paths/globs or context-pack query expansion, then move to synthesis and concrete patch planning.",
        state.repeat_streak + 1,
        repeat_threshold,
        state.low_signal_streak,
        low_signal_threshold,
        state.last_signal_score,
        repo_review_min_signal_score(),
        removed,
        keep_limit
    ))
}

fn tool_result_signal_score(content: &str, is_error: bool) -> f64 {
    if is_error {
        return 0.0;
    }
    let lower = content.to_ascii_lowercase();
    let mut score: f64 = 0.0;
    if content.len() >= 160 {
        score += 0.25;
    } else if content.len() >= 80 {
        score += 0.15;
    }
    if lower.contains("file=")
        || lower.contains("path=")
        || lower.contains(".rs")
        || lower.contains(".py")
    {
        score += 0.35;
    }
    if lower.contains("cmd=")
        || lower.contains("rg ")
        || lower.contains("sed -n")
        || lower.contains("cargo ")
    {
        score += 0.25;
    }
    if lower.contains("not found")
        || lower.contains("no such file")
        || lower.contains("\"entries\":[]")
    {
        score -= 0.15;
    }
    score.clamp(0.0, 1.0)
}

pub(crate) fn update_repo_review_budget_state_from_results(
    state: &mut RepoReviewBudgetState,
    messages: &[Message],
    results: &[ToolResult],
) {
    if !detect_repo_review_intent(messages) {
        *state = RepoReviewBudgetState::default();
        return;
    }
    if results.is_empty() {
        state.last_signal_score = 0.0;
        state.low_signal_streak = state.low_signal_streak.saturating_add(1);
        return;
    }
    let avg_signal = results
        .iter()
        .map(|r| tool_result_signal_score(&r.content, r.is_error))
        .sum::<f64>()
        / results.len() as f64;
    state.last_signal_score = avg_signal;
    if avg_signal < repo_review_min_signal_score() {
        state.low_signal_streak = state.low_signal_streak.saturating_add(1);
    } else {
        state.low_signal_streak = 0;
    }
}

fn objective_eval_score(state: &str) -> f64 {
    match state.trim().to_ascii_lowercase().as_str() {
        "advancing" => 1.0,
        "flat" => 0.5,
        "regressing" => 0.0,
        "unproven" => 0.25,
        _ => 0.4,
    }
}

fn claim_verifier_enabled_runtime() -> bool {
    if let Ok(raw) = std::env::var("HERMES_CLAIM_VERIFIER_ENABLED") {
        return !matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        );
    }
    let hermes_home = std::env::var("HERMES_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".hermes")))
        .unwrap_or_else(|| PathBuf::from(".hermes"));
    let path = hermes_home.join("alpha").join("claim_verifier_policy.json");
    let raw = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return true,
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return true,
    };
    parsed
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

pub(crate) fn finalizer_claim_requires_evidence_retry(
    messages: &[Message],
    assistant_text: &str,
    retry_count: u32,
) -> bool {
    if !claim_verifier_enabled_runtime() {
        return false;
    }
    if retry_count >= FINALIZER_EVIDENCE_MAX_RETRIES || !detect_repo_review_intent(messages) {
        return false;
    }
    let lower = assistant_text.to_ascii_lowercase();
    let claims_completion = [
        "completed",
        "implemented",
        "fixed",
        "done",
        "resolved",
        "ready",
        "finished",
        "shipped",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if !claims_completion {
        return false;
    }
    let has_evidence = lower.contains("file=")
        || lower.contains("path=")
        || lower.contains("cmd=")
        || lower.contains("exists_now=")
        || lower.contains("`/users/")
        || lower.contains("cargo test");
    let has_confidence = lower.contains("confidence=high")
        || lower.contains("confidence=medium")
        || lower.contains("confidence=low")
        || lower.contains("confidence:");
    !(has_evidence && has_confidence)
}

fn strip_list_prefix(line: &str) -> &str {
    let trimmed = line.trim();
    let without_bullet = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
        .unwrap_or(trimmed);
    let mut chars = without_bullet.char_indices();
    let mut end_idx = 0usize;
    while let Some((idx, ch)) = chars.next() {
        if ch.is_ascii_digit() {
            end_idx = idx + ch.len_utf8();
            continue;
        }
        if (ch == '.' || ch == ')') && end_idx > 0 {
            let tail = &without_bullet[idx + ch.len_utf8()..];
            return tail.trim_start();
        }
        break;
    }
    without_bullet
}

pub(crate) fn finalizer_output_quality_requires_retry(
    assistant_text: &str,
    retry_count: u32,
) -> bool {
    if retry_count >= FINALIZER_OUTPUT_QUALITY_MAX_RETRIES {
        return false;
    }
    let lower = assistant_text.to_ascii_lowercase();
    let placeholder_markers = [
        "[url](url)",
        "(url)",
        "[paper details](url)",
        "pack of authors",
        "lorem ipsum",
        "<insert",
        "<todo",
    ];
    if placeholder_markers
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return true;
    }

    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut in_code_block = false;
    for raw_line in assistant_text.lines() {
        let line = raw_line.trim();
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }
        let normalized = strip_list_prefix(line).trim().to_ascii_lowercase();
        if normalized.len() < 24 {
            continue;
        }
        let entry = counts.entry(normalized).or_insert(0);
        *entry += 1;
        if *entry >= 3 {
            return true;
        }
    }
    false
}

fn assistant_response_has_execution_evidence(lower: &str) -> bool {
    [
        "file=",
        "path=",
        "cmd=",
        "exists_now=",
        "objective_state=",
        "error:",
        "blocked:",
        "blocker:",
        "run finished",
        "tested",
        "verified",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn detect_execution_required_intent(messages: &[Message]) -> bool {
    let user = latest_user_content(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if user.trim().is_empty() {
        return false;
    }
    let objective = extract_session_objective(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let combined = format!("{} {}", user, objective);
    let action_terms = [
        "proceed",
        "implement",
        "fix",
        "debug",
        "diagnose",
        "run",
        "test",
        "patch",
        "sync",
        "rebuild",
        "verify",
        "connect",
        "integrat",
        "investigate",
        "analyze",
        "review",
    ];
    let has_action = action_terms.iter().any(|needle| combined.contains(needle));
    let has_surface = combined.contains("repo")
        || combined.contains("repository")
        || combined.contains("codebase")
        || combined.contains("contextlattice")
        || combined.contains('/')
        || combined.contains(".rs")
        || combined.contains(".py")
        || combined.contains("session");
    has_action && has_surface
}

pub(crate) fn finalizer_action_execution_requires_retry(
    messages: &[Message],
    assistant_text: &str,
    retry_count: u32,
) -> bool {
    if retry_count >= FINALIZER_ACTION_EXECUTION_MAX_RETRIES {
        return false;
    }
    if !detect_execution_required_intent(messages) {
        return false;
    }
    let lower = assistant_text.to_ascii_lowercase();
    if assistant_response_has_execution_evidence(&lower) {
        return false;
    }
    let deferral_markers = [
        "i will",
        "i'll",
        "let me",
        "i can",
        "i'm going to",
        "proceeding",
        "next i",
        "i should",
        "i would",
    ];
    deferral_markers.iter().any(|needle| lower.contains(needle))
}

pub(crate) fn detect_contextlattice_connect_intent(messages: &[Message]) -> bool {
    let Some(last_user) = messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::User))
        .and_then(|m| m.content.as_deref())
    else {
        return false;
    };
    let lower = last_user.to_ascii_lowercase();
    if !lower.contains("contextlattice") {
        return false;
    }
    [
        "connect",
        "connection",
        "configure",
        "setup",
        "set up",
        "verify",
        "harden",
        "probe",
        "integrat",
        "health",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

pub(crate) fn contextlattice_connect_system_hint(messages: &[Message]) -> Option<String> {
    if !detect_contextlattice_connect_intent(messages) {
        return None;
    }
    Some(
        "[SYSTEM] ContextLattice integration intent detected. Execute this order: \
         (1) If available, inspect local instructions file from `HERMES_CONTEXTLATTICE_INSTRUCTIONS_PATH` \
         or workspace `scripts/agent_orchestration.py` (preferred path: `/Users/sheawinkler/Documents/Projects/scripts/agent_orchestration.py`); \
         (2) call `contextlattice_search` for a direct connectivity probe; \
         (3) if needed call `contextlattice_context_pack` for broader grounding; \
         (4) call `contextlattice_write` to checkpoint what was verified. \
         Never use terminal command `contextlattice` for this workflow."
            .to_string(),
    )
}

pub(crate) fn contextlattice_intelligence_system_hint(
    messages: &[Message],
    tool_schemas: &[ToolSchema],
) -> Option<String> {
    let has_context_tools = tool_schemas.iter().any(|t| {
        matches!(
            t.name.as_str(),
            "contextlattice_search"
                | "contextlattice_context_pack"
                | "contextlattice_write"
                | "memory"
        )
    });
    if !has_context_tools {
        return None;
    }

    let objective_active = objective_guard_policy(messages).0;
    let repo_intent = detect_repo_review_intent(messages);
    let connect_intent = detect_contextlattice_connect_intent(messages);
    if !(objective_active || repo_intent || connect_intent) {
        return None;
    }

    Some(
        "[SYSTEM] ContextLattice-first intelligence policy active.\n\
         1) Start with scoped retrieval (`contextlattice_search`) using project + topic path.\n\
         2) If scoped retrieval is empty/degraded, run one broader retrieval in the same project and compare.\n\
         3) For broad or multi-file tasks, run `contextlattice_context_pack` before deep tool loops.\n\
         4) During long execution, checkpoint durable progress with `contextlattice_write`.\n\
         5) Before final answer, run one scoped readback and report contradictions as `unproven` rather than guessing.\n\
         6) Copy numeric facts verbatim; do not normalize or round unless explicitly requested."
            .to_string(),
    )
}

pub(crate) fn is_contextlattice_shell_invocation(raw_args: &str) -> bool {
    let Ok(args) = serde_json::from_str::<Value>(raw_args) else {
        return false;
    };
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default();
    let lower = command.to_ascii_lowercase();
    lower == "contextlattice" || lower.starts_with("contextlattice ")
}

fn summarize_background_review_result(messages: &[Message]) -> Option<String> {
    crate::evolution_ledger::summarize_review_for_chat(messages)
}

fn default_model_cost_per_million(model: &str) -> Option<(f64, f64)> {
    let m = model.to_lowercase();
    if m.contains("gpt-4o-mini") || m.contains("4.1-mini") || m.contains("haiku") {
        return Some((0.15, 0.60));
    }
    if m.contains("gpt-4o") || m.contains("4.1") || m.contains("sonnet") {
        return Some((2.5, 10.0));
    }
    if m.contains("o3") {
        return Some((10.0, 40.0));
    }
    None
}

fn extract_objective_state_marker(text: &str) -> String {
    for line in text.lines() {
        let lowered = line.trim().to_ascii_lowercase();
        if let Some(rest) = lowered.split("objective_state=").nth(1) {
            let token = rest
                .trim_start()
                .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
                .next()
                .unwrap_or("")
                .trim_matches(|c: char| c == ')' || c == '.');
            if !token.is_empty() {
                return token.to_string();
            }
        }
        if let Some(rest) = lowered.split("objective_state:").nth(1) {
            let token = rest
                .trim_start()
                .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
                .next()
                .unwrap_or("")
                .trim_matches(|c: char| c == ')' || c == '.');
            if !token.is_empty() {
                return token.to_string();
            }
        }
    }
    "unspecified".to_string()
}

fn extract_marker_values(text: &str, marker: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(idx) = line.find(marker) else {
            continue;
        };
        let rest = &line[idx + marker.len()..];
        let value = rest
            .split(|c: char| c.is_whitespace() || c == ')' || c == ',' || c == ';' || c == '|')
            .next()
            .unwrap_or("")
            .trim();
        if value.is_empty() {
            continue;
        }
        let normalized = value.trim_matches(|c: char| c == '"' || c == '\'' || c == '`');
        if normalized.is_empty() || out.iter().any(|v| v == normalized) {
            continue;
        }
        out.push(normalized.to_string());
        if out.len() >= limit {
            break;
        }
    }
    out
}

pub(crate) fn estimate_usage_cost_usd(
    usage: &UsageStats,
    model: &str,
    config: &AgentConfig,
) -> Option<f64> {
    if let Some(v) = usage.estimated_cost {
        return Some(v.max(0.0));
    }
    let canonical = usage_stats_to_canonical(usage);
    let provider = config.provider.as_deref();
    let cost =
        hermes_intelligence::usage_pricing::calculate_cost(model, &canonical, provider, None);
    if let Some(amount) = cost.amount_usd {
        return Some(amount.max(0.0));
    }
    let (in_pm, out_pm) = match (
        config.prompt_cost_per_million_usd,
        config.completion_cost_per_million_usd,
    ) {
        (Some(i), Some(o)) => (i, o),
        _ => default_model_cost_per_million(model)?,
    };
    let prompt_cost = (usage.prompt_tokens as f64 / 1_000_000.0) * in_pm;
    let completion_cost = (usage.completion_tokens as f64 / 1_000_000.0) * out_pm;
    Some(prompt_cost + completion_cost)
}

fn usage_stats_to_canonical(
    usage: &UsageStats,
) -> hermes_intelligence::usage_pricing::CanonicalUsage {
    let input = if usage.input_tokens > 0 {
        usage.input_tokens
    } else {
        usage
            .prompt_tokens
            .saturating_sub(usage.cache_read_tokens + usage.cache_write_tokens)
    };
    let output = if usage.output_tokens > 0 {
        usage.output_tokens
    } else {
        usage.completion_tokens
    };
    hermes_intelligence::usage_pricing::CanonicalUsage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        request_count: 1,
    }
}

/// Merge two UsageStats, summing token counts and keeping the latest cost estimate.
pub(crate) fn merge_usage(existing: Option<UsageStats>, new: &UsageStats) -> UsageStats {
    match existing {
        Some(prev) => UsageStats {
            prompt_tokens: prev.prompt_tokens + new.prompt_tokens,
            completion_tokens: prev.completion_tokens + new.completion_tokens,
            total_tokens: prev.total_tokens + new.total_tokens,
            input_tokens: prev.input_tokens + new.input_tokens,
            output_tokens: prev.output_tokens + new.output_tokens,
            cache_read_tokens: prev.cache_read_tokens + new.cache_read_tokens,
            cache_write_tokens: prev.cache_write_tokens + new.cache_write_tokens,
            reasoning_tokens: prev.reasoning_tokens + new.reasoning_tokens,
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
// Forwarding methods — delegate to free functions in conversation_loop
// so callers can continue using `agent.run(...)`, `agent.current_task_id()`, etc.
// ---------------------------------------------------------------------------

impl AgentLoop {
    /// Run the autonomous loop with non-streaming LLM transport.
    pub async fn run(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolSchema>>,
    ) -> Result<AgentResult, AgentError> {
        crate::conversation_loop::run_agent_loop(self, messages, tools).await
    }

    /// Run one full user turn (Python `run_conversation`).
    pub async fn run_conversation(
        &self,
        params: crate::conversation_loop::RunConversationParams,
    ) -> Result<crate::conversation_loop::ConversationResult, AgentError> {
        crate::conversation_loop::run_conversation(self, params).await
    }

    /// Active task id for this turn (Python `agent._current_task_id`).
    pub fn current_task_id(&self) -> Option<String> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.current_task_id.clone())
    }

    /// Returns `(prompt, restored_from_storage)` using session-level cache when warm.
    pub(crate) fn active_cached_system_prompt(
        &self,
        task_hint: &str,
        tool_schemas: &[ToolSchema],
    ) -> (String, bool) {
        crate::conversation_loop::active_cached_system_prompt(self, task_hint, tool_schemas)
    }

    /// Returns `(prompt, restored_from_storage)` — restored prompts skip fresh `build_system_prompt`.
    pub(crate) fn resolve_initial_system_prompt(
        &self,
        task_hint: &str,
        tool_schemas: &[ToolSchema],
    ) -> (String, bool) {
        crate::conversation_loop::resolve_initial_system_prompt(self, task_hint, tool_schemas)
    }

    pub(crate) fn guard_session_search_without_query(
        &self,
        tool_calls: &mut Vec<ToolCall>,
    ) -> Vec<ToolResult> {
        let web_hint = if self.tool_registry.get("web_search").is_some() {
            " If current external facts are needed, use web_search with a specific query instead."
        } else {
            ""
        };
        let mut blocked = Vec::new();
        tool_calls.retain(|tc| {
            let should_block =
                tc.function.name == "session_search" && !session_search_has_query(tc);
            if should_block {
                blocked.push(ToolResult::err(
                    tc.id.clone(),
                    format!(
                        "Blocked: session_search was called without a concrete query. In normal agent turns, session_search must include a query describing the past conversation context to recall.{web_hint}"
                    ),
                ));
                false
            } else {
                true
            }
        });
        blocked
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governor::{GovernorRuntimeState, TurnGovernor};
    use crate::hooks::spill_hook_context_if_oversized;
    use crate::llm_caller::{
        collect_stream_llm_response, session_disable_streaming, use_streaming_llm_transport,
    };
    use crate::message_sanitization::budget_pressure_text;
    use crate::replay::{ReplayState, redact_json_value};
    use crate::route_learning::{
        now_unix_ms, route_learning_effective_stats, route_learning_state_path,
    };
    use crate::tool_executor::{
        coerce_textual_tool_calls, deduplicate_tool_calls, hydrate_session_search_args,
    };
    use futures::stream::BoxStream;
    use hermes_core::JsonSchema;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn env_test_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env test lock poisoned")
    }

    fn session_search_call(args: &str) -> ToolCall {
        ToolCall {
            id: "call_session".into(),
            function: hermes_core::FunctionCall {
                name: "session_search".into(),
                arguments: args.into(),
            },
            extra_content: None,
        }
    }

    #[test]
    fn session_search_query_guard_detects_missing_query() {
        assert!(!session_search_has_query(&session_search_call("{}")));
        assert!(!session_search_has_query(&session_search_call(
            r#"{"query":"   "}"#
        )));
    }

    #[test]
    fn session_search_query_guard_allows_concrete_query() {
        assert!(session_search_has_query(&session_search_call(
            r#"{"query":"上次的项目进展"}"#
        )));
    }

    #[test]
    fn restore_primary_runtime_at_turn_start_after_fallback() {
        use crate::test_support::ErrNoopProvider as NoopProvider;

        let config = AgentConfig::default();
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(NoopProvider),
        );
        agent.activate_runtime_fallback(PrimaryRuntime {
            model: "anthropic/claude-sonnet-4".to_string(),
            provider: Some("openrouter".to_string()),
            base_url: None,
            api_mode: ApiMode::ChatCompletions,
            command: None,
            args: Vec::new(),
            credential_pool: None,
        });
        assert_eq!(
            crate::runtime_provider::active_model(&agent),
            "anthropic/claude-sonnet-4"
        );
        assert_eq!(agent.config().model, "anthropic/claude-sonnet-4");
        agent.restore_primary_runtime_at_turn_start();
        assert_eq!(crate::runtime_provider::active_model(&agent), "gpt-4o");
        assert_eq!(agent.config().model, "gpt-4o");
        assert!(
            !agent
                .state
                .lock()
                .expect("lock")
                .turn_fallback
                .is_fallback_activated()
        );
    }

    #[tokio::test]
    async fn preprocess_user_message_context_references_expands_at_file() {
        let _guard = env_test_lock();
        let td = tempfile::tempdir().expect("tempdir");
        std::fs::write(td.path().join("note.txt"), "hello context\n").expect("write");
        let prev_cwd = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(td.path()).expect("chdir");

        use crate::test_support::ErrNoopProvider as NoopProvider;

        let loop_engine = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(NoopProvider),
        );
        let mut messages = vec![Message::user("summarize @file:note.txt")];
        loop_engine
            .preprocess_user_message_context_references(&mut messages)
            .await;

        std::env::set_current_dir(prev_cwd).expect("restore cwd");
        let content = messages[0].content.as_deref().expect("content");
        assert!(content.contains("Attached Context"));
        assert!(content.contains("hello context"));
    }

    #[test]
    fn test_agent_config_default() {
        let config = AgentConfig::default();
        assert_eq!(config.max_turns, 250);
        assert_eq!(config.model, "gpt-4o");
        assert!(!config.stream);
        assert_eq!(config.max_concurrent_delegates, 1);
        assert_eq!(config.memory_flush_interval, 5);
        assert_eq!(config.api_mode, ApiMode::ChatCompletions);
        assert_eq!(config.retry.max_retries, 3);
        assert!(config.session_id.is_none());
        assert!(!config.skip_memory);
        assert!(!config.skip_context_files);
        assert!(config.platform.is_none());
        assert!(!config.pass_session_id);
        assert!(config.max_cost_usd.is_none());
        assert_eq!(config.cost_guard_degrade_at_ratio, 0.8);
        assert!(config.cost_guard_degrade_model.is_none());
        assert_eq!(config.checkpoint_interval_turns, 3);
        assert_eq!(config.rollback_on_tool_error_threshold, 3);
        assert!(!config.smart_model_routing.enabled);
        assert!(config.background_review_metrics_enabled);
        assert_eq!(config.stream_read_max_retries, 2);
    }

    #[test]
    fn delegation_spawning_paused_honors_env_toggle() {
        hermes_core::test_env::remove_var("HERMES_DELEGATION_PAUSED");
        assert!(!delegation_spawning_paused());
        hermes_core::test_env::set_var("HERMES_DELEGATION_PAUSED", "1");
        assert!(delegation_spawning_paused());
        hermes_core::test_env::set_var("HERMES_DELEGATION_PAUSED", "true");
        assert!(delegation_spawning_paused());
        hermes_core::test_env::set_var("HERMES_DELEGATION_PAUSED", "0");
        assert!(!delegation_spawning_paused());
    }

    #[test]
    fn tool_enforcement_prompt_gate_matches_python_model_patterns() {
        assert!(should_inject_tool_enforcement_for_model("openai:gpt-5"));
        assert!(should_inject_tool_enforcement_for_model("xai:grok-4-fast"));
        assert!(should_inject_tool_enforcement_for_model("zhipu:glm-4.5"));
        assert!(!should_inject_tool_enforcement_for_model(
            "anthropic:claude-3-7-sonnet"
        ));
    }

    #[test]
    fn summarize_background_review_nothing_to_save() {
        let msgs = vec![Message::assistant("Nothing to save.")];
        let out = summarize_background_review_result(&msgs);
        assert!(out.is_none());
    }

    #[test]
    fn classify_error_404_generic_is_retryable() {
        assert_eq!(classify_error("HTTP 404 Not Found"), ErrorClass::Retryable);
        assert_eq!(
            classify_error("gateway returned not found"),
            ErrorClass::Retryable
        );
    }

    #[test]
    fn classify_error_404_model_not_found_is_fatal() {
        assert_eq!(
            classify_error("404 model not found: foo/bar"),
            ErrorClass::Fatal
        );
        assert_eq!(
            classify_error("invalid model: gpt-unknown"),
            ErrorClass::Fatal
        );
    }

    #[test]
    fn classify_error_openrouter_privacy_guardrail_is_fatal() {
        assert_eq!(
            classify_error("HTTP 404: OpenRouter privacy guardrail blocked this endpoint"),
            ErrorClass::Fatal
        );
    }

    #[test]
    fn classify_error_ssl_bad_record_mac_is_retryable() {
        assert_eq!(
            classify_error("[SSL: BAD_RECORD_MAC] sslv3 alert bad record mac (_ssl.c:2580)"),
            ErrorClass::Retryable
        );
    }

    #[test]
    fn classify_error_ssl_openssl_token_form_is_retryable() {
        assert_eq!(
            classify_error("ERR_SSL_SSL/TLS_ALERT_BAD_RECORD_MAC during streaming"),
            ErrorClass::Retryable
        );
    }

    #[test]
    fn classify_error_plain_disconnect_stays_retryable() {
        assert_eq!(
            classify_error("Server disconnected without sending a response"),
            ErrorClass::Retryable
        );
    }

    #[test]
    fn tool_payload_validation_error_detector_matches_known_provider_signatures() {
        let strict_shape = "API error 400 Bad Request: Invalid input: expected \"function\"";
        assert!(is_tool_payload_validation_error(strict_shape));
        let provider_generic = "API error 400 Bad Request: This request is not valid. Check the model name and other parameters. Additional info: Provider returned error";
        assert!(is_tool_payload_validation_error(provider_generic));
        let no_choices_provider_shape = "No choices in response (status=400; message=This request is not valid. Check the model name and other parameters. Additional info: Provider returned error)";
        assert!(is_tool_payload_validation_error(no_choices_provider_shape));
        let unprocessable_payload =
            "API error 422 Unprocessable Entity: Check that you're sending a valid payload";
        assert!(is_tool_payload_validation_error(unprocessable_payload));
        assert!(!is_tool_payload_validation_error(
            "API error 400 Bad Request: max_tokens must be positive"
        ));
    }

    #[test]
    fn preferred_tool_payload_fallback_model_defaults_and_override() {
        assert_eq!(
            preferred_tool_payload_fallback_model("nous", "openai/gpt-5.5"),
            Some("nousresearch/hermes-4-70b".to_string())
        );
        assert_eq!(
            preferred_tool_payload_fallback_model("openrouter", "openai/gpt-5.5"),
            None
        );
        hermes_core::test_env::set_var(
            "HERMES_TOOL_PAYLOAD_FALLBACK_MODEL",
            "nousresearch/hermes-4-405b",
        );
        assert_eq!(
            preferred_tool_payload_fallback_model("nous", "openai/gpt-5.5"),
            Some("nousresearch/hermes-4-405b".to_string())
        );
        hermes_core::test_env::remove_var("HERMES_TOOL_PAYLOAD_FALLBACK_MODEL");
    }

    #[test]
    fn maybe_nous_401_diagnostic_returns_hint_for_nous_auth_failures() {
        let diag = maybe_nous_401_diagnostic(
            "nous",
            "HTTP 401 Unauthorized: token expired",
            Some("/tmp/hermes-home"),
        )
        .expect("nous 401 should produce diagnostics");
        assert!(diag.contains("Nous 401 - Portal authentication failed."));
        assert!(diag.contains("hermes auth login nous"));
        assert!(diag.contains("portal.nousresearch.com"));
        assert!(diag.contains("/tmp/hermes-home/auth.json"));
    }

    #[test]
    fn maybe_nous_401_diagnostic_ignores_non_nous_provider() {
        let diag = maybe_nous_401_diagnostic(
            "openrouter",
            "HTTP 401 Unauthorized: token expired",
            Some("/tmp/hermes-home"),
        );
        assert!(diag.is_none());
    }

    #[test]
    fn maybe_nous_401_diagnostic_ignores_non_auth_errors() {
        let diag = maybe_nous_401_diagnostic("nous", "HTTP 500 upstream timeout", None);
        assert!(diag.is_none());
    }

    #[test]
    fn summarize_background_review_counts_tool_calls() {
        let msgs = vec![
            Message::tool_result(
                "tc_mem",
                "{\"success\":true,\"message\":\"Skill 'prospect-scanner' created.\"}",
            ),
            Message::tool_result(
                "tc_skill",
                "{\"success\":true,\"message\":\"Entry added\",\"target\":\"memory\"}",
            ),
            Message::tool_result("tc_skip", "{\"success\":false,\"message\":\"failed\"}"),
        ];
        let out = summarize_background_review_result(&msgs).expect("summary should exist");
        assert!(out.starts_with("\u{1F9E0} "));
        assert!(out.contains("Skill 'prospect-scanner' created."));
        assert!(out.contains("Memory updated"));
    }

    #[test]
    fn summarize_background_review_filters_status_and_secret_like_text() {
        let msgs = vec![
            Message::tool_result(
                "tc_safe",
                "{\"success\":true,\"message\":\"created docs/repo-review-notes.md\"}",
            ),
            Message::tool_result(
                "tc_status",
                "{\"success\":true,\"message\":\"status=ok token refreshed\"}",
            ),
            Message::tool_result(
                "tc_obj",
                "{\"success\":true,\"message\":\"{\\\"message\\\":\\\"updated config\\\"}\"}",
            ),
        ];
        let out = summarize_background_review_result(&msgs).expect("summary should exist");
        assert!(out.contains("created docs/repo-review-notes.md"));
        assert!(!out.contains("status=ok token refreshed"));
        assert!(!out.contains("{\"message\""));
    }

    #[test]
    fn exploratory_hint_enabled_for_repo_exploration_intent() {
        let msgs = vec![Message::user(
            "Deeply audit /Users/sheawinkler/Documents/Projects/hermes-agent-ultra/crates/hermes-agent/src/agent_loop.rs and diagnose root cause.",
        )];
        let hint = exploratory_problem_solving_system_hint(&msgs).expect("hint should exist");
        assert!(hint.contains("Exploratory problem-solving protocol active"));
        assert!(hint.contains("workstream=<name>"));
    }

    #[test]
    fn exploratory_hint_disabled_for_non_exploratory_repo_intent() {
        let msgs = vec![Message::user(
            "Implement this fix directly in /Users/sheawinkler/Documents/Projects/hermes-agent-ultra/src/main.rs.",
        )];
        assert!(exploratory_problem_solving_system_hint(&msgs).is_none());
    }

    #[test]
    fn hook_context_spill_writes_file_for_oversized_payload() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let prev = std::env::var("HERMES_HOOK_CONTEXT_SPILL_CHARS").ok();
        hermes_core::test_env::set_var("HERMES_HOOK_CONTEXT_SPILL_CHARS", "1024");
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = AgentConfig {
            hermes_home: Some(tmp.path().display().to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let large = "x".repeat(2_048);
        let spilled =
            spill_hook_context_if_oversized(&agent, &large).expect("spill should write file");
        assert!(spilled.exists(), "spill file should exist");
        let read_back = std::fs::read_to_string(&spilled).expect("read spill file");
        assert_eq!(read_back.len(), large.len());
        assert!(
            spill_hook_context_if_oversized(&agent, "small payload").is_none(),
            "small payload must not spill"
        );
        if let Some(v) = prev {
            hermes_core::test_env::set_var("HERMES_HOOK_CONTEXT_SPILL_CHARS", v);
        } else {
            hermes_core::test_env::remove_var("HERMES_HOOK_CONTEXT_SPILL_CHARS");
        }
    }

    #[test]
    fn post_llm_transform_hook_rewrites_assistant_content() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let mut content = Some("before".to_string());
        crate::hooks::apply_hook_output_transforms(
            &[HookResult::TransformLlmOutput("after".to_string())],
            &mut content,
        );
        assert_eq!(content.as_deref(), Some("after"));
    }

    #[test]
    fn set_runtime_session_id_updates_config() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        agent.set_runtime_session_id("session-abc");
        assert_eq!(agent.runtime_session_id().as_deref(), Some("session-abc"));
    }

    #[tokio::test]
    async fn compress_messages_short_transcript_is_noop() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let messages = vec![
            Message::system("sys"),
            Message::user("hi"),
            Message::assistant("hello"),
        ];
        let (out, compressed) = agent
            .compress_messages(messages.clone(), "sid-1", "gpt-4o")
            .await;
        assert!(!compressed);
        assert_eq!(out.len(), messages.len());
    }

    #[tokio::test]
    async fn preflight_compression_status_reports_skipped_when_under_threshold() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        )
        .with_callbacks(callbacks);
        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("small"));
        agent.preflight_context_compress_with_status(&mut ctx).await;

        let rows = captured.lock().expect("captured lock");
        assert!(
            rows.iter().any(|(kind, msg)| {
                kind == "lifecycle" && msg.contains("no compression needed")
            })
        );
    }

    #[tokio::test]
    async fn preflight_compression_status_reports_when_compressing() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        )
        .with_callbacks(callbacks);
        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(95)));
        agent.preflight_context_compress_with_status(&mut ctx).await;

        let rows = captured.lock().expect("captured lock");
        assert!(rows.iter().any(|(kind, msg)| {
            kind == "lifecycle" && msg.contains("compressing before first turn")
        }));
        assert!(
            rows.iter()
                .any(|(kind, msg)| kind == "lifecycle" && msg.contains("compression complete"))
        );
    }

    #[tokio::test]
    async fn status_callback_receives_context_pressure_messages() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        )
        .with_callbacks(callbacks);

        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(90)));
        agent.auto_compress_if_over_threshold(&mut ctx).await;

        let rows = captured.lock().expect("captured lock");
        assert!(
            rows.iter()
                .any(|(kind, msg)| kind == "lifecycle" && msg.contains("triggering compression"))
        );
    }

    #[tokio::test]
    async fn call_llm_with_retry_strips_provider_prefix_for_primary_and_fallback_models() {
        use futures::stream::BoxStream;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct RecordingProvider {
            seen_models: Arc<std::sync::Mutex<Vec<String>>>,
            call_count: AtomicUsize,
        }

        #[async_trait::async_trait]
        impl LlmProvider for RecordingProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                self.seen_models
                    .lock()
                    .expect("seen model lock")
                    .push(model.unwrap_or_default().to_string());
                let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
                if idx == 0 {
                    return Err(AgentError::LlmApi(
                        "API error 429: synthetic retry".to_string(),
                    ));
                }
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("ok"),
                    usage: None,
                    model: "ok".to_string(),
                    finish_reason: Some("stop".to_string()),
                    ..Default::default()
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let seen_models = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let mut cfg = AgentConfig::default();
        cfg.model = "nous:primary-model".to_string();
        cfg.retry.max_retries = 0;
        cfg.retry.fallback_model = Some("openrouter:backup-model".to_string());

        let provider = Arc::new(RecordingProvider {
            seen_models: seen_models.clone(),
            call_count: AtomicUsize::new(0),
        });
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider);
        let mut ctx = ContextManager::new(32);
        ctx.add_message(Message::user("hello"));

        let mut api_call_count = 0u32;
        let resp = agent
            .call_llm_with_retry_inner(&mut ctx, &[], None, None, &mut api_call_count)
            .await
            .expect("fallback should recover");
        assert_eq!(resp.message.content.as_deref(), Some("ok"));

        let seen = seen_models.lock().expect("seen model lock").clone();
        assert_eq!(
            seen,
            vec!["primary-model".to_string(), "backup-model".to_string()]
        );
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    struct ChaosHarnessStep {
        kind: String,
        message: Option<String>,
    }

    #[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
    struct ChaosHarnessExpectation {
        outcome: String,
        attempts: usize,
        fallback_calls: usize,
        error_contains: Option<String>,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    struct ChaosHarnessScenario {
        id: String,
        seed: u64,
        max_retries: u32,
        fallback_model: Option<String>,
        steps: Vec<ChaosHarnessStep>,
        expected: ChaosHarnessExpectation,
    }

    #[derive(Debug, serde::Deserialize)]
    struct ChaosHarnessFixture {
        schema_version: u32,
        scenarios: Vec<ChaosHarnessScenario>,
    }

    #[derive(Debug)]
    struct ChaosHarnessRun {
        outcome: &'static str,
        attempts: usize,
        fallback_calls: usize,
        error: Option<String>,
    }

    fn load_chaos_harness_fixture() -> ChaosHarnessFixture {
        serde_json::from_str(include_str!("testdata/adapter_chaos_profiles.json"))
            .expect("parse adapter chaos fixture")
    }

    struct ChaosHarnessProvider {
        scenario_id: String,
        steps: Vec<ChaosHarnessStep>,
        call_index: std::sync::atomic::AtomicUsize,
        seen_models: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl ChaosHarnessProvider {
        fn new(scenario: &ChaosHarnessScenario) -> Self {
            Self {
                scenario_id: scenario.id.clone(),
                steps: scenario.steps.clone(),
                call_index: std::sync::atomic::AtomicUsize::new(0),
                seen_models: Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn attempts(&self) -> usize {
            self.call_index.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn fallback_calls(&self, fallback_model: Option<&str>) -> usize {
            let Some(fallback) = fallback_model else {
                return 0;
            };
            let fallback_name = fallback
                .split_once(':')
                .map(|(_, model)| model)
                .unwrap_or(fallback);
            self.seen_models
                .lock()
                .expect("seen model lock")
                .iter()
                .filter(|m| m.as_str() == fallback_name)
                .count()
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for ChaosHarnessProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            let idx = self
                .call_index
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.seen_models
                .lock()
                .expect("seen model lock")
                .push(model.unwrap_or_default().to_string());

            let step = self
                .steps
                .get(idx)
                .cloned()
                .or_else(|| self.steps.last().cloned())
                .unwrap_or(ChaosHarnessStep {
                    kind: "success".to_string(),
                    message: Some("ok-default".to_string()),
                });
            match step.kind.as_str() {
                "success" => Ok(hermes_core::LlmResponse {
                    message: Message::assistant(
                        step.message
                            .unwrap_or_else(|| format!("ok-{}", self.scenario_id)),
                    ),
                    usage: None,
                    model: "chaos".to_string(),
                    finish_reason: Some("stop".to_string()),
                    ..Default::default()
                }),
                "timeout" => Err(AgentError::LlmApi(
                    step.message
                        .unwrap_or_else(|| "request timeout".to_string()),
                )),
                "http_5xx" => {
                    Err(AgentError::LlmApi(step.message.unwrap_or_else(|| {
                        "API error 500: synthetic upstream fault".to_string()
                    })))
                }
                "rate_limit" => {
                    Err(AgentError::LlmApi(step.message.unwrap_or_else(|| {
                        "API error 429: synthetic rate limit".to_string()
                    })))
                }
                other => Err(AgentError::LlmApi(format!(
                    "unsupported chaos step '{}' in scenario '{}'",
                    other, self.scenario_id
                ))),
            }
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            futures::stream::empty().boxed()
        }
    }

    async fn run_chaos_harness_scenario(scenario: &ChaosHarnessScenario) -> ChaosHarnessRun {
        let mut cfg = AgentConfig::default();
        cfg.model = "nous:primary-model".to_string();
        cfg.retry.max_retries = scenario.max_retries;
        cfg.retry.base_delay_ms = 0;
        cfg.retry.max_delay_ms = 0;
        cfg.retry.fallback_model = scenario.fallback_model.clone();

        let provider = Arc::new(ChaosHarnessProvider::new(scenario));
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
        let mut ctx = ContextManager::new(32);
        ctx.add_message(Message::user(format!(
            "chaos scenario {} seed {}",
            scenario.id, scenario.seed
        )));

        let mut api_call_count = 0u32;
        match agent
            .call_llm_with_retry_inner(&mut ctx, &[], None, None, &mut api_call_count)
            .await
        {
            Ok(_) => ChaosHarnessRun {
                outcome: "success",
                attempts: provider.attempts(),
                fallback_calls: provider.fallback_calls(scenario.fallback_model.as_deref()),
                error: None,
            },
            Err(err) => ChaosHarnessRun {
                outcome: "error",
                attempts: provider.attempts(),
                fallback_calls: provider.fallback_calls(scenario.fallback_model.as_deref()),
                error: Some(err.to_string()),
            },
        }
    }

    #[test]
    fn chaos_harness_fixture_is_seeded_and_unique() {
        let fixture = load_chaos_harness_fixture();
        assert_eq!(fixture.schema_version, 1, "unexpected fixture schema");
        assert!(
            !fixture.scenarios.is_empty(),
            "chaos fixture must not be empty"
        );
        let mut ids = std::collections::HashSet::new();
        let mut seeds = std::collections::HashSet::new();
        for scenario in fixture.scenarios {
            assert!(
                ids.insert(scenario.id.clone()),
                "duplicate chaos scenario id: {}",
                scenario.id
            );
            assert!(
                seeds.insert(scenario.seed),
                "duplicate chaos scenario seed: {}",
                scenario.seed
            );
        }
    }

    #[tokio::test]
    async fn chaos_harness_profiles_verify_retry_and_fallback() {
        let fixture = load_chaos_harness_fixture();
        let mut diagnostics = Vec::new();
        let mut runs = Vec::new();
        for scenario in fixture.scenarios {
            let run = run_chaos_harness_scenario(&scenario).await;
            runs.push(serde_json::json!({
                "scenario": scenario.id,
                "seed": scenario.seed,
                "actual": {
                    "outcome": run.outcome,
                    "attempts": run.attempts,
                    "fallback_calls": run.fallback_calls,
                    "error": run.error,
                }
            }));
            let mut mismatches = Vec::new();

            if run.outcome != scenario.expected.outcome {
                mismatches.push(format!(
                    "outcome mismatch expected={} actual={}",
                    scenario.expected.outcome, run.outcome
                ));
            }
            if run.attempts != scenario.expected.attempts {
                mismatches.push(format!(
                    "attempt mismatch expected={} actual={}",
                    scenario.expected.attempts, run.attempts
                ));
            }
            if run.fallback_calls != scenario.expected.fallback_calls {
                mismatches.push(format!(
                    "fallback_calls mismatch expected={} actual={}",
                    scenario.expected.fallback_calls, run.fallback_calls
                ));
            }
            if let Some(expect_fragment) = scenario.expected.error_contains.as_ref() {
                let got_error = run.error.as_deref().unwrap_or("");
                if !got_error.contains(expect_fragment) {
                    mismatches.push(format!(
                        "error fragment missing expected='{}' actual='{}'",
                        expect_fragment, got_error
                    ));
                }
            }

            if !mismatches.is_empty() {
                diagnostics.push(serde_json::json!({
                    "scenario": scenario.id,
                    "seed": scenario.seed,
                    "expected": scenario.expected,
                    "actual": {
                        "outcome": run.outcome,
                        "attempts": run.attempts,
                        "fallback_calls": run.fallback_calls,
                        "error": run.error,
                    },
                    "mismatches": mismatches,
                }));
            }
        }

        println!(
            "adapter chaos harness results: {}",
            serde_json::to_string(&runs).expect("serialize chaos runs")
        );

        assert!(
            diagnostics.is_empty(),
            "adapter chaos harness mismatches:\n{}",
            serde_json::to_string_pretty(&diagnostics).expect("serialize diagnostics")
        );
    }

    #[tokio::test]
    async fn handle_max_iterations_uses_provider_native_model_id() {
        use futures::stream::BoxStream;

        struct RecordingProvider {
            seen_model: Arc<std::sync::Mutex<Option<String>>>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for RecordingProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                *self.seen_model.lock().expect("seen model lock") =
                    Some(model.unwrap_or_default().to_string());
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("summary"),
                    usage: None,
                    model: "ok".to_string(),
                    finish_reason: Some("stop".to_string()),
                    ..Default::default()
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let seen_model = Arc::new(std::sync::Mutex::new(None::<String>));
        let mut cfg = AgentConfig::default();
        cfg.model = "nous:moonshotai/kimi-k2.6".to_string();

        let provider = Arc::new(RecordingProvider {
            seen_model: seen_model.clone(),
        });
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider);
        let mut ctx = ContextManager::new(32);
        ctx.add_message(Message::user("hit turn limit"));

        let _ = agent
            .handle_max_iterations(&mut ctx)
            .await
            .expect("max iterations summary should succeed");
        let seen = seen_model.lock().expect("seen model lock").clone();
        assert_eq!(seen.as_deref(), Some("moonshotai/kimi-k2.6"));
    }

    #[tokio::test]
    async fn status_callback_receives_empty_response_retry_notice() {
        use futures::stream::BoxStream;

        #[derive(Default)]
        struct RetryDummyProvider {
            calls: std::sync::Mutex<u32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for RetryDummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                let mut n = self.calls.lock().expect("calls lock");
                *n += 1;
                let msg = if *n == 1 {
                    Message::assistant("")
                } else {
                    Message::assistant("ok")
                };
                let finish_reason = if *n == 1 {
                    None
                } else {
                    Some("stop".to_string())
                };
                Ok(hermes_core::LlmResponse {
                    message: msg,
                    usage: None,
                    model: "dummy".into(),
                    finish_reason,
                    ..Default::default()
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let cfg = AgentConfig {
            max_turns: 1,
            empty_content_max_retries: 1,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(RetryDummyProvider::default()),
        )
        .with_callbacks(callbacks);

        let result = agent.run(vec![Message::user("hello")], None).await;
        assert!(result.is_ok());
        let rows = captured.lock().expect("captured lock");
        assert!(rows.iter().any(|(kind, msg)| {
            kind == "lifecycle" && msg.contains("Empty assistant response - retrying")
        }));
    }

    #[tokio::test]
    async fn empty_stop_response_is_accepted_without_retry() {
        use futures::stream::BoxStream;

        #[derive(Default)]
        struct EmptyStopProvider {
            calls: std::sync::Mutex<u32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for EmptyStopProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                let mut n = self.calls.lock().expect("calls lock");
                *n += 1;
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant(""),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                    ..Default::default()
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let provider = Arc::new(EmptyStopProvider::default());
        let cfg = AgentConfig {
            max_turns: 1,
            empty_content_max_retries: 3,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone())
            .with_callbacks(callbacks);

        let result = agent.run(vec![Message::user("hello")], None).await;
        assert!(result.is_ok());
        assert_eq!(*provider.calls.lock().expect("calls lock"), 1);
        let rows = captured.lock().expect("captured lock");
        assert!(!rows.iter().any(|(kind, msg)| {
            kind == "lifecycle" && msg.contains("Empty assistant response - retrying")
        }));
    }

    #[tokio::test]
    async fn run_truncated_tool_call_retries_before_tool_execution() {
        use futures::stream::BoxStream;
        use hermes_core::{FunctionCall, ToolCall};

        #[derive(Default)]
        struct TruncatedThenOkProvider {
            calls: std::sync::Mutex<u32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for TruncatedThenOkProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                let mut n = self.calls.lock().expect("calls lock");
                *n += 1;
                if *n == 1 {
                    Ok(hermes_core::LlmResponse {
                        message: Message::assistant_with_tool_calls(
                            None,
                            vec![ToolCall {
                                id: "call_trunc".to_string(),
                                function: FunctionCall {
                                    name: "echo".to_string(),
                                    arguments: "{\"path\":\"/tmp/x\",".to_string(),
                                },
                                extra_content: None,
                            }],
                        ),
                        usage: None,
                        model: "dummy".into(),
                        finish_reason: Some("stop".into()),
                        ..Default::default()
                    })
                } else {
                    Ok(hermes_core::LlmResponse {
                        message: Message::assistant("done"),
                        usage: None,
                        model: "dummy".into(),
                        finish_reason: Some("stop".into()),
                        ..Default::default()
                    })
                }
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let provider = Arc::new(TruncatedThenOkProvider::default());
        let mut registry = ToolRegistry::new();
        registry.register(
            "echo",
            hermes_core::tool_schema("echo", "Echo input", hermes_core::JsonSchema::new("object")),
            Arc::new(|_| Ok("{}".to_string())),
        );
        let cfg = AgentConfig {
            max_turns: 3,
            truncated_tool_call_max_retries: 1,
            ..AgentConfig::default()
        };
        let agent =
            AgentLoop::new(cfg, Arc::new(registry), provider.clone()).with_callbacks(callbacks);

        let result = agent.run(vec![Message::user("hello")], None).await;
        assert!(result.is_ok());
        assert_eq!(*provider.calls.lock().expect("calls lock"), 2);
        let rows = captured.lock().expect("captured lock");
        assert!(rows.iter().any(|(kind, msg)| {
            kind == "lifecycle" && msg.contains("Truncated tool arguments")
        }));
    }

    fn stream_chunk_content(text: &str) -> StreamChunk {
        StreamChunk {
            delta: Some(hermes_core::StreamDelta {
                content: Some(text.to_string()),
                tool_calls: None,
                extra: None,
            }),
            finish_reason: None,
            usage: None,
        }
    }

    fn stream_chunk_tool_name(index: u32, id: &str, name: &str) -> StreamChunk {
        StreamChunk {
            delta: Some(hermes_core::StreamDelta {
                content: None,
                tool_calls: Some(vec![hermes_core::ToolCallDelta {
                    index,
                    id: Some(id.to_string()),
                    function: Some(hermes_core::FunctionCallDelta {
                        name: Some(name.to_string()),
                        arguments: None,
                    }),
                }]),
                extra: None,
            }),
            finish_reason: None,
            usage: None,
        }
    }

    fn stream_chunk_tool_args(index: u32, args: &str) -> StreamChunk {
        StreamChunk {
            delta: Some(hermes_core::StreamDelta {
                content: None,
                tool_calls: Some(vec![hermes_core::ToolCallDelta {
                    index,
                    id: None,
                    function: Some(hermes_core::FunctionCallDelta {
                        name: None,
                        arguments: Some(args.to_string()),
                    }),
                }]),
                extra: None,
            }),
            finish_reason: None,
            usage: None,
        }
    }

    fn stream_chunk_finish(reason: &str) -> StreamChunk {
        StreamChunk {
            delta: None,
            finish_reason: Some(reason.to_string()),
            usage: None,
        }
    }

    #[derive(Clone, Copy)]
    enum StreamRetryScenario {
        RecoverOnSecondAttempt,
        AlwaysFailMidToolCall,
        TextOnlyDrop,
    }

    struct StreamRetryProvider {
        scenario: StreamRetryScenario,
        calls: std::sync::Mutex<u32>,
    }

    impl StreamRetryProvider {
        fn new(scenario: StreamRetryScenario) -> Self {
            Self {
                scenario,
                calls: std::sync::Mutex::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for StreamRetryProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            Err(AgentError::LlmApi("unused".to_string()))
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            let mut calls = self.calls.lock().expect("calls lock");
            *calls += 1;
            let attempt = *calls;

            let events: Vec<Result<StreamChunk, AgentError>> = match self.scenario {
                StreamRetryScenario::RecoverOnSecondAttempt => {
                    if attempt == 1 {
                        vec![
                            Ok(stream_chunk_content("Let me write the audit: ")),
                            Ok(stream_chunk_tool_name(0, "call_1", "write_file")),
                            Ok(stream_chunk_tool_args(0, "{\"path\":\"/tmp/x\",")),
                            Err(AgentError::LlmApi(
                                "Stream read error: peer closed connection".to_string(),
                            )),
                        ]
                    } else {
                        vec![
                            Ok(stream_chunk_content("Let me write the audit: ")),
                            Ok(stream_chunk_tool_name(0, "call_1", "write_file")),
                            Ok(stream_chunk_tool_args(
                                0,
                                "{\"path\":\"/tmp/x\",\"content\":\"hi\"}",
                            )),
                            Ok(stream_chunk_finish("tool_calls")),
                        ]
                    }
                }
                StreamRetryScenario::AlwaysFailMidToolCall => vec![
                    Ok(stream_chunk_content("Working...")),
                    Ok(stream_chunk_tool_name(0, "call_2", "write_file")),
                    Ok(stream_chunk_tool_args(0, "{\"path\":\"/tmp/y\",")),
                    Err(AgentError::LlmApi(
                        "Stream read error: connection reset by peer".to_string(),
                    )),
                ],
                StreamRetryScenario::TextOnlyDrop => vec![
                    Ok(stream_chunk_content("Partial text")),
                    Err(AgentError::LlmApi(
                        "Stream read error: connection lost".to_string(),
                    )),
                ],
            };

            futures::stream::iter(events).boxed()
        }
    }

    #[tokio::test]
    async fn stream_mid_tool_call_silent_retry_recovers_tool_call() {
        let provider = Arc::new(StreamRetryProvider::new(
            StreamRetryScenario::RecoverOnSecondAttempt,
        ));
        let cfg = AgentConfig {
            stream_read_max_retries: 2,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_ref = seen.clone();
        let mut api_call_count = 0u32;

        let out = collect_stream_llm_response(
            &agent,
            &mut ctx,
            &[],
            None,
            "dummy-model",
            None,
            &move |chunk| {
                if let Some(delta) = chunk.delta {
                    if let Some(text) = delta.content {
                        seen_ref.lock().expect("seen lock").push(text);
                    }
                }
            },
            &mut api_call_count,
            None,
        )
        .await;

        let StreamCollectOutcome::Complete(resp) = out.expect("stream should recover") else {
            panic!("expected complete response");
        };
        let tc = resp
            .message
            .tool_calls
            .as_ref()
            .and_then(|calls| calls.first())
            .expect("missing tool call");
        assert_eq!(tc.function.name, "write_file");
        assert_eq!(*provider.calls.lock().expect("calls lock"), 2);
        assert!(seen.lock().expect("seen lock").iter().any(|text| {
            text.to_lowercase()
                .contains("connection dropped mid tool-call; reconnecting")
        }));
    }

    #[tokio::test]
    async fn stream_mid_tool_call_exhausted_retries_returns_partial_stub() {
        let provider = Arc::new(StreamRetryProvider::new(
            StreamRetryScenario::AlwaysFailMidToolCall,
        ));
        let cfg = AgentConfig {
            stream_read_max_retries: 1,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_ref = seen.clone();
        let mut api_call_count = 0u32;

        let out = collect_stream_llm_response(
            &agent,
            &mut ctx,
            &[],
            None,
            "dummy-model",
            None,
            &move |chunk| {
                if let Some(delta) = chunk.delta {
                    if let Some(text) = delta.content {
                        seen_ref.lock().expect("seen lock").push(text);
                    }
                }
            },
            &mut api_call_count,
            None,
        )
        .await
        .expect("partial stub should recover instead of hard error");

        let StreamCollectOutcome::Complete(resp) = out else {
            panic!("expected complete partial-stream stub");
        };
        assert_eq!(
            resp.response_id.as_deref(),
            Some(hermes_core::PARTIAL_STREAM_STUB_ID)
        );
        assert_eq!(resp.finish_reason.as_deref(), Some("length"));
        assert!(
            resp.message
                .tool_calls
                .as_ref()
                .map_or(true, |calls| calls.is_empty())
        );
        assert_eq!(
            resp.dropped_tool_names.as_deref(),
            Some(["write_file".to_string()].as_slice())
        );
        let body = resp.message.content.as_deref().unwrap_or_default();
        assert!(body.contains("Working..."));
        assert!(body.contains("Stream stalled mid tool-call"));
        assert!(body.contains("write_file"));
        assert_eq!(*provider.calls.lock().expect("calls lock"), 2);
        assert!(seen.lock().expect("seen lock").iter().any(|text| {
            text.to_lowercase()
                .contains("connection dropped mid tool-call; reconnecting")
        }));
        assert!(
            seen.lock()
                .expect("seen lock")
                .iter()
                .any(|text| { text.contains("Stream stalled mid tool-call") })
        );
    }

    #[tokio::test]
    async fn stream_text_only_drop_returns_partial_stub_without_retry() {
        let provider = Arc::new(StreamRetryProvider::new(StreamRetryScenario::TextOnlyDrop));
        let cfg = AgentConfig {
            stream_read_max_retries: 2,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_ref = seen.clone();
        let mut api_call_count = 0u32;

        let out = collect_stream_llm_response(
            &agent,
            &mut ctx,
            &[],
            None,
            "dummy-model",
            None,
            &move |chunk| {
                if let Some(delta) = chunk.delta {
                    if let Some(text) = delta.content {
                        seen_ref.lock().expect("seen lock").push(text);
                    }
                }
            },
            &mut api_call_count,
            None,
        )
        .await
        .expect("text-only partial stream should return stub");

        let StreamCollectOutcome::Complete(resp) = out else {
            panic!("expected partial-stream stub");
        };
        assert_eq!(
            resp.response_id.as_deref(),
            Some(hermes_core::PARTIAL_STREAM_STUB_ID)
        );
        assert_eq!(resp.finish_reason.as_deref(), Some("length"));
        assert_eq!(resp.message.content.as_deref(), Some("Partial text"));
        assert_eq!(*provider.calls.lock().expect("calls lock"), 1);
        assert!(!seen.lock().expect("seen lock").iter().any(|text| {
            text.to_lowercase()
                .contains("connection dropped mid tool-call; reconnecting")
        }));
        assert_eq!(
            crate::message_sanitization::continuation_prompt_for_response(&resp),
            crate::message_sanitization::get_continuation_prompt(true, None)
        );
    }

    #[tokio::test]
    async fn quiet_mode_suppresses_status_callback() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let cfg = AgentConfig {
            quiet_mode: true,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        )
        .with_callbacks(callbacks);
        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(90)));
        agent.auto_compress_if_over_threshold(&mut ctx).await;

        assert!(captured.lock().expect("captured lock").is_empty());
    }

    #[test]
    fn test_builtin_personality_injected_into_system_prompt() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let config = AgentConfig {
            personality: Some("coder".to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let prompt = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(prompt.contains("## Active Personality (coder)"));
        assert!(prompt.contains("`coder` persona"));
    }

    #[test]
    fn test_unknown_personality_name_does_not_add_overlay_block() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let config = AgentConfig {
            personality: Some("unknown_persona".to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let prompt = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(!prompt.contains("## Active Personality (unknown_persona)"));
        assert!(prompt.contains("You are Hermes Agent"));
    }

    #[test]
    fn test_default_personality_name_does_not_add_overlay_block() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let config = AgentConfig {
            personality: Some("default".to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let prompt = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(!prompt.contains("## Active Personality (default)"));
        assert!(prompt.contains("You are Hermes Agent"));
    }

    #[test]
    fn test_task_completion_guidance_default_injects_when_tools_present() {
        use futures::stream::BoxStream;
        use hermes_core::JsonSchema;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let tools = vec![ToolSchema::new(
            "terminal",
            "Execute commands",
            JsonSchema::new("object"),
        )];
        let prompt = agent.build_system_prompt("", &tools, "anthropic/claude-opus-4.8");
        assert!(prompt.contains(crate::prompt_builder::TASK_COMPLETION_GUIDANCE));
    }

    #[test]
    fn test_task_completion_guidance_false_disables_injection() {
        use futures::stream::BoxStream;
        use hermes_core::JsonSchema;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let config = AgentConfig {
            task_completion_guidance: false,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let tools = vec![ToolSchema::new(
            "terminal",
            "Execute commands",
            JsonSchema::new("object"),
        )];
        let prompt = agent.build_system_prompt("", &tools, "anthropic/claude-opus-4.8");
        assert!(!prompt.contains(crate::prompt_builder::TASK_COMPLETION_GUIDANCE));
    }

    #[test]
    fn test_task_completion_guidance_not_injected_without_tools() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let prompt = agent.build_system_prompt("", &[], "anthropic/claude-opus-4.8");
        assert!(!prompt.contains(crate::prompt_builder::TASK_COMPLETION_GUIDANCE));
    }

    #[test]
    fn test_smart_model_routing_cheap_route_for_simple_turn() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                api_key_env: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
                ..Default::default()
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let messages = vec![Message::user("帮我总结一下今天要做什么")];
        let selected = crate::route_learning::resolve_smart_runtime_route(&agent, &messages);
        assert_eq!(
            selected.as_ref().map(|r| r.model.as_str()),
            Some("gpt-4o-mini")
        );
    }

    #[test]
    fn test_smart_model_routing_online_learning_prefers_primary_when_cheap_unstable() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                api_key_env: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
                ..Default::default()
            },
        );
        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        if let Ok(mut m) = agent.route_learning.lock() {
            m.insert(
                "openai:gpt-4o".to_string(),
                RouteLearningStats {
                    samples: 12,
                    success_rate: 0.98,
                    avg_latency_ms: 900.0,
                    consecutive_failures: 0,
                    updated_at_unix_ms: now_unix_ms(),
                },
            );
            m.insert(
                "openai:gpt-4o-mini".to_string(),
                RouteLearningStats {
                    samples: 12,
                    success_rate: 0.35,
                    avg_latency_ms: 3800.0,
                    consecutive_failures: 3,
                    updated_at_unix_ms: now_unix_ms(),
                },
            );
        }
        let selected = crate::route_learning::resolve_smart_runtime_route(
            &agent,
            &[Message::user("summarize today's work")],
        );
        assert!(
            selected.is_none(),
            "online learning should keep primary when cheap route is unstable"
        );
    }

    #[test]
    fn test_route_learning_updates_after_outcomes() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut cfg = AgentConfig::default();
        cfg.hermes_home = Some(tmp.path().to_string_lossy().to_string());
        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        crate::route_learning::update_route_learning(
            &agent,
            None,
            Some("openai:gpt-4o"),
            2100,
            false,
        );
        crate::route_learning::update_route_learning(
            &agent,
            None,
            Some("openai:gpt-4o"),
            900,
            true,
        );
        let snapshot =
            crate::route_learning::route_learning_snapshot(&agent, None, Some("openai:gpt-4o"));
        assert_eq!(snapshot["enabled"], true);
        assert_eq!(snapshot["key"], "openai:gpt-4o");
        assert_eq!(snapshot["stats"]["samples"], 2);
        assert!(snapshot["stats"]["success_rate"].as_f64().unwrap_or(0.0) > 0.0);
        assert!(snapshot["stats"]["avg_latency_ms"].as_f64().unwrap_or(0.0) > 0.0);
    }

    #[test]
    fn test_route_learning_persists_across_agent_restarts() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut cfg = AgentConfig::default();
        cfg.hermes_home = Some(tmp.path().to_string_lossy().to_string());

        let agent = AgentLoop::new(
            cfg.clone(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        crate::route_learning::update_route_learning(
            &agent,
            None,
            Some("openai:gpt-4o"),
            1200,
            true,
        );
        let persisted_path = route_learning_state_path(&cfg);
        assert!(
            persisted_path.exists(),
            "route-learning state file must exist"
        );

        let reloaded = AgentLoop::new(
            cfg.clone(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let snapshot =
            crate::route_learning::route_learning_snapshot(&reloaded, None, Some("openai:gpt-4o"));
        assert_eq!(snapshot["key"], "openai:gpt-4o");
        assert!(snapshot["stats"]["samples"].as_u64().unwrap_or(0) >= 1);
    }

    #[test]
    fn test_route_learning_malformed_file_is_safe_fallback() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut cfg = AgentConfig::default();
        cfg.hermes_home = Some(tmp.path().to_string_lossy().to_string());
        let state_path = route_learning_state_path(&cfg);
        std::fs::create_dir_all(
            state_path
                .parent()
                .expect("route-learning path should have a parent"),
        )
        .expect("create route-learning dir");
        std::fs::write(&state_path, "{ this-is-invalid-json")
            .expect("write malformed route-learning file");

        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let snapshot =
            crate::route_learning::route_learning_snapshot(&agent, None, Some("openai:gpt-4o"));
        assert!(
            snapshot["stats"].is_null(),
            "malformed state must fall back cleanly"
        );
    }

    #[test]
    fn test_route_learning_decay_and_ttl() {
        let now_ms = now_unix_ms();
        let stale = RouteLearningStats {
            samples: 10,
            success_rate: 0.05,
            avg_latency_ms: 9900.0,
            consecutive_failures: 9,
            updated_at_unix_ms: now_ms - (8 * 24 * 60 * 60 * 1000),
        };
        assert!(
            route_learning_effective_stats(&stale, now_ms).is_none(),
            "stale route entries must expire by ttl"
        );

        let recent = RouteLearningStats {
            samples: 10,
            success_rate: 0.20,
            avg_latency_ms: 4000.0,
            consecutive_failures: 4,
            updated_at_unix_ms: now_ms - (12 * 60 * 60 * 1000),
        };
        let adjusted = route_learning_effective_stats(&recent, now_ms)
            .expect("recent entry should not expire");
        assert!(adjusted.success_rate > recent.success_rate);
        assert!(adjusted.avg_latency_ms < recent.avg_latency_ms);
        assert!(adjusted.samples <= recent.samples);
    }

    #[test]
    fn test_runtime_provider_command_args_override_primary_acp_metadata() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                api_key_env: None,
                base_url: Some("https://api.openai.com/v1".to_string()),
                command: Some("copilot-language-server".to_string()),
                args: vec![
                    "--stdio".to_string(),
                    "--model".to_string(),
                    "gpt-4o-mini".to_string(),
                ],
                oauth_token_url: None,
                oauth_client_id: None,
                ..Default::default()
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            provider: Some("openai".to_string()),
            runtime_providers,
            acp_command: Some("global-acp".to_string()),
            acp_args: vec!["--global".to_string()],
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let primary = crate::route_learning::primary_runtime_snapshot(&agent);
        assert_eq!(primary.command.as_deref(), Some("copilot-language-server"));
        assert_eq!(
            primary.args,
            vec![
                "--stdio".to_string(),
                "--model".to_string(),
                "gpt-4o-mini".to_string()
            ]
        );
    }

    #[test]
    fn test_smart_model_routing_codex_provider_alias_builds_runtime() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "codex".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                api_key_env: None,
                base_url: Some("https://api.openai.com/v1".to_string()),
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
                ..Default::default()
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("codex".to_string()),
                    model: Some("gpt-5-mini".to_string()),
                    base_url: Some("https://api.openai.com/v1".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let messages = vec![Message::user("总结一下这个需求")];
        let selected = crate::route_learning::resolve_smart_runtime_route(&agent, &messages);
        assert_eq!(
            selected.as_ref().map(|r| r.model.as_str()),
            Some("gpt-5-mini")
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("codex")
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.api_mode.as_ref()),
            Some(&ApiMode::CodexResponses)
        );
    }

    #[test]
    fn test_smart_model_routing_qwen_oauth_alias_builds_runtime() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "qwen-oauth".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-qwen-oauth".to_string()),
                api_key_env: None,
                base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string()),
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
                ..Default::default()
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("qwen-oauth".to_string()),
                    model: Some("qwen3-coder-plus".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let selected = crate::route_learning::resolve_smart_runtime_route(
            &agent,
            &[Message::user("给我一段简短总结")],
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("qwen-oauth")
        );
    }

    #[test]
    fn test_runtime_provider_stepfun_build_supported() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "stepfun".to_string(),
            RuntimeProviderConfig {
                api_key: Some("stepfun-test-key".to_string()),
                api_key_env: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
                ..Default::default()
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );

        let built = crate::runtime_provider::build_runtime_provider(
            &agent,
            "stepfun",
            "step-3.5-flash",
            None,
            None,
            None,
            None,
            None,
        );
        assert!(built.is_ok(), "stepfun runtime provider should build");
    }

    #[test]
    fn test_smart_model_routing_openai_codex_reads_auth_store_token() {
        use futures::stream::BoxStream;
        use std::time::{SystemTime, UNIX_EPOCH};

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let home = std::env::temp_dir().join(format!("hermes-auth-fixture-{}", nonce));
        let auth_dir = home.join("auth");
        std::fs::create_dir_all(&auth_dir).expect("create auth dir");
        std::fs::write(
            auth_dir.join("tokens.json"),
            r#"{
  "openai-codex": {
    "provider": "openai-codex",
    "access_token": "codex-oauth-token",
    "token_type": "bearer",
    "refresh_token": null,
    "scope": null,
    "expires_at": "2099-01-01T00:00:00Z"
  }
}"#,
        )
        .expect("write token store");

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai-codex".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
                ..Default::default()
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            hermes_home: Some(home.to_string_lossy().to_string()),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai-codex".to_string()),
                    model: Some("gpt-5-codex".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let selected = crate::route_learning::resolve_smart_runtime_route(
            &agent,
            &[Message::user("帮我总结这段话")],
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("openai-codex")
        );
        // Best effort cleanup.
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn test_openai_runtime_reads_openai_oauth_token_store_entry() {
        use futures::stream::BoxStream;
        use std::time::{SystemTime, UNIX_EPOCH};

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let home = std::env::temp_dir().join(format!("hermes-openai-auth-fixture-{}", nonce));
        let auth_dir = home.join("auth");
        std::fs::create_dir_all(&auth_dir).expect("create auth dir");
        std::fs::write(
            auth_dir.join("tokens.json"),
            r#"{
  "openai": {
    "provider": "openai",
    "access_token": "openai-oauth-token",
    "token_type": "bearer",
    "refresh_token": null,
    "scope": null,
    "expires_at": "2099-01-01T00:00:00Z"
  }
}"#,
        )
        .expect("write token store");

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
                ..Default::default()
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            hermes_home: Some(home.to_string_lossy().to_string()),
            runtime_providers,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let resolved = agent.resolve_runtime_api_key("openai", None, None);
        assert_eq!(resolved.as_deref(), Some("openai-oauth-token"));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn test_self_evolution_skill_counter_ticks_each_iteration() {
        use futures::stream::BoxStream;
        use hermes_core::JsonSchema;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let mut registry = ToolRegistry::new();
        registry.register(
            "skill_manage",
            ToolSchema::new("skill_manage", "Manage skills", JsonSchema::new("object")),
            Arc::new(|_args| Ok("{\"success\":true}".to_string())),
        );

        let config = AgentConfig {
            skill_creation_nudge_interval: 10,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(registry),
            Arc::new(DummyProvider::default()),
        );
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("hello")], None))
            .expect("agent run should succeed");

        let counters = &agent.state.lock().expect("counter lock").evolution_counters;
        assert_eq!(counters.iters_since_skill, 1);
    }

    #[test]
    fn test_self_evolution_parity_fixtures_v2026_4_13_memory_nudge() {
        use futures::stream::BoxStream;
        use hermes_core::JsonSchema;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        // Fixture-style cases distilled from Python v2026.4.13:
        // - counter persists across runs
        // - resets to 0 when hitting interval threshold
        #[derive(Clone, Copy)]
        struct Case {
            runs: u32,
            expected_turns_since_memory: u32,
        }
        let cases = vec![
            Case {
                runs: 1,
                expected_turns_since_memory: 1,
            },
            Case {
                runs: 2,
                expected_turns_since_memory: 0,
            },
        ];

        for case in cases {
            let mut registry = ToolRegistry::new();
            registry.register(
                "memory",
                ToolSchema::new("memory", "Memory tool", JsonSchema::new("object")),
                Arc::new(|_args| Ok("{\"success\":true}".to_string())),
            );

            let config = AgentConfig {
                memory_nudge_interval: 2,
                ..AgentConfig::default()
            };
            let agent = AgentLoop::new(
                config,
                Arc::new(registry),
                Arc::new(DummyProvider::default()),
            );
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            for _ in 0..case.runs {
                let _ = rt
                    .block_on(agent.run(vec![Message::user("hello")], None))
                    .expect("agent run should succeed");
            }
            let counters = &agent.state.lock().expect("counter lock").evolution_counters;
            assert_eq!(
                counters.turns_since_memory, case.expected_turns_since_memory,
                "fixture runs={} mismatch",
                case.runs
            );
        }
    }

    #[test]
    fn test_iters_since_skill_resets_then_reincrements_on_followup_iteration() {
        use futures::stream::BoxStream;
        use std::sync::atomic::{AtomicU32, Ordering};

        struct TwoStepProvider {
            calls: Arc<AtomicU32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for TwoStepProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                let msg = if n == 0 {
                    Message::assistant_with_tool_calls(
                        None,
                        vec![hermes_core::ToolCall {
                            id: "tc_skill".to_string(),
                            function: hermes_core::FunctionCall {
                                name: "skill_manage".to_string(),
                                arguments: "{}".to_string(),
                            },
                            extra_content: None,
                        }],
                    )
                } else {
                    Message::assistant("done")
                };
                Ok(hermes_core::LlmResponse {
                    message: msg,
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                    ..Default::default()
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(
            "skill_manage",
            ToolSchema::new("skill_manage", "Manage skills", JsonSchema::new("object")),
            Arc::new(|_args| Ok("{\"success\":true}".to_string())),
        );

        let config = AgentConfig {
            skill_creation_nudge_interval: 10,
            ..AgentConfig::default()
        };
        let provider = TwoStepProvider {
            calls: Arc::new(AtomicU32::new(0)),
        };
        let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(provider));
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("hello")], None))
            .expect("agent run should succeed");

        let counters = &agent.state.lock().expect("counter lock").evolution_counters;
        // Iteration #1 increments then skill_manage resets to 0.
        // Iteration #2 (final assistant turn) increments again to 1.
        // Python follows the same cadence because `_iters_since_skill += 1`
        // happens at each loop iteration before the tool/reset branch.
        assert_eq!(counters.iters_since_skill, 1);
    }

    #[test]
    fn test_use_streaming_llm_transport_matches_python_gates() {
        use futures::stream::BoxStream;

        struct HealthCheckProvider;
        #[async_trait::async_trait]
        impl LlmProvider for HealthCheckProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("ok"),
                    usage: None,
                    model: "t".into(),
                    finish_reason: Some("stop".into()),
                    ..Default::default()
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }

            fn prefers_non_streaming_transport(&self) -> bool {
                true
            }
        }

        struct OpenProvider;
        #[async_trait::async_trait]
        impl LlmProvider for OpenProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("ok"),
                    usage: None,
                    model: "t".into(),
                    finish_reason: Some("stop".into()),
                    ..Default::default()
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let registry = Arc::new(ToolRegistry::new());
        let mock_like = AgentLoop::new(
            AgentConfig::default(),
            registry.clone(),
            Arc::new(HealthCheckProvider),
        );
        assert!(!use_streaming_llm_transport(&mock_like, false, 0, None));
        assert!(use_streaming_llm_transport(&mock_like, true, 0, None));

        let open = AgentLoop::new(
            AgentConfig::default(),
            registry.clone(),
            Arc::new(OpenProvider),
        );
        assert!(use_streaming_llm_transport(&open, false, 0, None));

        let acp_cfg = AgentConfig {
            provider: Some("copilot-acp".to_string()),
            ..AgentConfig::default()
        };
        let acp = AgentLoop::new(acp_cfg, registry.clone(), Arc::new(OpenProvider));
        assert!(!use_streaming_llm_transport(&acp, true, 0, None));

        let acp_url_cfg = AgentConfig {
            provider: Some("custom".to_string()),
            runtime_providers: [(
                "custom".to_string(),
                RuntimeProviderConfig {
                    base_url: Some("acp://copilot".to_string()),
                    ..RuntimeProviderConfig::default()
                },
            )]
            .into_iter()
            .collect(),
            ..AgentConfig::default()
        };
        let acp_url = AgentLoop::new(acp_url_cfg, registry, Arc::new(OpenProvider));
        assert!(!use_streaming_llm_transport(&acp_url, true, 0, None));

        session_disable_streaming(&open);
        assert!(!use_streaming_llm_transport(&open, true, 0, None));
        assert!(!use_streaming_llm_transport(&open, true, 1, None));
    }

    #[test]
    fn test_is_stream_not_supported_error_detects_provider_message() {
        let err = AgentError::LlmApi("Streaming is not supported for this model".into());
        assert!(is_stream_not_supported_error(&err));
        let transient = AgentError::LlmApi("connection reset".into());
        assert!(!is_stream_not_supported_error(&transient));
    }

    #[test]
    fn test_smart_model_routing_copilot_acp_missing_cli_falls_back() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "copilot-acp".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: Some("acp://copilot".to_string()),
                command: Some("definitely-not-installed-copilot-cli".to_string()),
                args: vec!["--acp".to_string(), "--stdio".to_string()],
                oauth_token_url: None,
                oauth_client_id: None,
                ..Default::default()
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("copilot-acp".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: Some("acp://copilot".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let selected = crate::route_learning::resolve_smart_runtime_route(
            &agent,
            &[Message::user("帮我总结这段话")],
        );
        assert!(
            selected.is_none(),
            "missing ACP CLI should fail cheap-route and fall back"
        );
    }

    #[test]
    fn test_smart_model_routing_copilot_acp_tcp_mode_skips_cli_check() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "copilot-acp".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: Some("acp+tcp://127.0.0.1:8765".to_string()),
                command: Some("definitely-not-installed-copilot-cli".to_string()),
                args: vec!["--acp".to_string(), "--stdio".to_string()],
                oauth_token_url: None,
                oauth_client_id: None,
                ..Default::default()
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("copilot-acp".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: Some("acp+tcp://127.0.0.1:8765".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let selected = crate::route_learning::resolve_smart_runtime_route(
            &agent,
            &[Message::user("帮我总结这段话")],
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("copilot-acp")
        );
    }

    #[test]
    fn test_smart_model_routing_skips_complex_turn() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let messages = vec![Message::user("请帮我 debug 这段 traceback 并修复错误")];
        let selected = crate::route_learning::resolve_smart_runtime_route(&agent, &messages);
        assert!(selected.is_none());
    }

    #[test]
    fn test_deduplicate_tool_calls() {
        let calls = vec![
            ToolCall {
                id: "1".into(),
                function: hermes_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "2".into(),
                function: hermes_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "3".into(),
                function: hermes_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"b.txt"}"#.into(),
                },
                extra_content: None,
            },
        ];
        let deduped = deduplicate_tool_calls(&calls);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].id, "1");
        assert_eq!(deduped[1].id, "3");
    }

    #[test]
    fn test_memory_write_event_from_tool_call_add() {
        let tc = ToolCall {
            id: "c1".into(),
            function: hermes_core::FunctionCall {
                name: "memory".into(),
                arguments:
                    r#"{"action":"add","target":"user","content":"Prefers concise answers"}"#.into(),
            },
            extra_content: None,
        };
        let event = AgentLoop::memory_write_event_from_tool_call(&tc).unwrap();
        assert_eq!(event.0, "add");
        assert_eq!(event.1, "user");
        assert_eq!(event.2, "Prefers concise answers");
    }

    #[test]
    fn test_memory_write_event_from_tool_call_remove_uses_old_text() {
        let tc = ToolCall {
            id: "c2".into(),
            function: hermes_core::FunctionCall {
                name: "memory".into(),
                arguments: r#"{"action":"remove","target":"memory","old_text":"obsolete fact"}"#
                    .into(),
            },
            extra_content: None,
        };
        let event = AgentLoop::memory_write_event_from_tool_call(&tc).unwrap();
        assert_eq!(event.0, "remove");
        assert_eq!(event.1, "memory");
        assert_eq!(event.2, "obsolete fact");
    }

    #[test]
    fn test_hydrate_session_search_args_injects_current_session_id() {
        use futures::stream::BoxStream;
        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let config = AgentConfig {
            session_id: Some("sess-auto-1".into()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let mut tc = ToolCall {
            id: "s1".into(),
            function: hermes_core::FunctionCall {
                name: "session_search".into(),
                arguments: r#"{"query":"previous issue","limit":3}"#.into(),
            },
            extra_content: None,
        };
        hydrate_session_search_args(&agent, &mut tc);
        let args: Value = serde_json::from_str(&tc.function.arguments).unwrap();
        assert_eq!(
            args.get("current_session_id").and_then(|v| v.as_str()),
            Some("sess-auto-1")
        );
    }

    #[test]
    fn test_hydrate_session_search_args_keeps_existing_current_session_id() {
        use futures::stream::BoxStream;
        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let config = AgentConfig {
            session_id: Some("sess-outer".into()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let mut tc = ToolCall {
            id: "s2".into(),
            function: hermes_core::FunctionCall {
                name: "session_search".into(),
                arguments: r#"{"query":"abc","current_session_id":"sess-explicit"}"#.into(),
            },
            extra_content: None,
        };
        hydrate_session_search_args(&agent, &mut tc);
        let args: Value = serde_json::from_str(&tc.function.arguments).unwrap();
        assert_eq!(
            args.get("current_session_id").and_then(|v| v.as_str()),
            Some("sess-explicit")
        );
    }

    #[test]
    fn test_budget_warning() {
        let config = AgentConfig {
            max_turns: 10,
            ..AgentConfig::default()
        };
        let registry = Arc::new(ToolRegistry::new());
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let agent = AgentLoop::new(config, registry, Arc::new(DummyProvider::default()));

        let max = agent.config().max_turns;
        assert!(budget_pressure_text(6, max, 0.7, 0.9, true).is_none());
        assert!(budget_pressure_text(7, max, 0.7, 0.9, true).is_some());
        let w = budget_pressure_text(9, max, 0.7, 0.9, true).unwrap();
        assert!(w.contains("BUDGET WARNING"), "{w}");
        let w10 = budget_pressure_text(10, max, 0.7, 0.9, true).unwrap();
        assert!(w10.contains("BUDGET WARNING"), "{w10}");
        let _ = agent;
    }

    #[test]
    fn test_tool_registry_new() {
        let registry = ToolRegistry::new();
        assert!(registry.names().is_empty());
    }

    #[test]
    fn test_merge_usage() {
        let a = UsageStats {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            cache_read_tokens: 10,
            estimated_cost: Some(0.01),
            ..Default::default()
        };
        let b = UsageStats {
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            cache_read_tokens: 20,
            estimated_cost: Some(0.02),
            ..Default::default()
        };
        let merged = merge_usage(Some(a), &b);
        assert_eq!(merged.prompt_tokens, 300);
        assert_eq!(merged.completion_tokens, 150);
        assert_eq!(merged.total_tokens, 450);
        assert_eq!(merged.cache_read_tokens, 30);
        assert_eq!(merged.estimated_cost, Some(0.03));
    }

    #[test]
    fn test_merge_usage_none() {
        let b = UsageStats {
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            ..Default::default()
        };
        let merged = merge_usage(None, &b);
        assert_eq!(merged.prompt_tokens, 200);
    }

    #[test]
    fn test_estimate_usage_cost_prefers_reported_estimate() {
        let cfg = AgentConfig::default();
        let u = UsageStats {
            prompt_tokens: 1000,
            completion_tokens: 1000,
            total_tokens: 2000,
            estimated_cost: Some(0.42),
            ..Default::default()
        };
        let cost = estimate_usage_cost_usd(&u, "openai:gpt-4o", &cfg).unwrap();
        assert!((cost - 0.42).abs() < 1e-9);
    }

    #[test]
    fn test_estimate_usage_cost_uses_model_fallback_table() {
        let cfg = AgentConfig::default();
        let u = UsageStats {
            prompt_tokens: 1_000_000,
            completion_tokens: 1_000_000,
            total_tokens: 2_000_000,
            ..Default::default()
        };
        let cost = estimate_usage_cost_usd(&u, "openai:gpt-4o-mini", &cfg).unwrap();
        assert!((cost - 0.75).abs() < 1e-9);
    }

    /// Smoke test: config-centre OAuth metadata wins over env fallback, and
    /// env is used when config is empty. Mirrors the Python behaviour of
    /// `resolve_runtime_provider_credentials` where provider config takes
    /// precedence over environment lookup.
    #[test]
    fn test_oauth_refresh_config_prefers_provider_config_over_env() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "qwen-oauth".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: Some("https://cfg.example.com/token".to_string()),
                oauth_client_id: Some("cfg-client".to_string()),
                ..Default::default()
            },
        );
        // An unknown provider reachable only via config (env fallback is gated
        // on known providers, so this exercises the cfg_token_url.zip path).
        runtime_providers.insert(
            "custom-oauth".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: Some("https://cfg.example.com/custom-token".to_string()),
                oauth_client_id: Some("custom-client".to_string()),
                ..Default::default()
            },
        );

        let config = AgentConfig {
            runtime_providers,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );

        // Set conflicting env values - config must win.
        hermes_core::test_env::set_var(
            "HERMES_QWEN_OAUTH_TOKEN_URL",
            "https://env.example.com/tok",
        );
        hermes_core::test_env::set_var("HERMES_QWEN_OAUTH_CLIENT_ID", "env-client");

        let (token_url, client_id) = agent.oauth_refresh_config("qwen-oauth").unwrap();
        assert_eq!(token_url, "https://cfg.example.com/token");
        assert_eq!(client_id, "cfg-client");

        // Unknown-provider path still resolves when config centre supplies both.
        let (token_url, client_id) = agent.oauth_refresh_config("custom-oauth").unwrap();
        assert_eq!(token_url, "https://cfg.example.com/custom-token");
        assert_eq!(client_id, "custom-client");

        hermes_core::test_env::remove_var("HERMES_QWEN_OAUTH_TOKEN_URL");
        hermes_core::test_env::remove_var("HERMES_QWEN_OAUTH_CLIENT_ID");
    }

    #[test]
    fn test_runtime_provider_api_key_env_is_resolved() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "custom".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: Some("MY_FALLBACK_KEY".to_string()),
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
                ..Default::default()
            },
        );

        let config = AgentConfig {
            runtime_providers,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );

        hermes_core::test_env::set_var("MY_FALLBACK_KEY", "env-secret");
        let resolved = agent.resolve_runtime_api_key("custom", None, None);
        assert_eq!(resolved.as_deref(), Some("env-secret"));
        hermes_core::test_env::remove_var("MY_FALLBACK_KEY");
    }

    #[test]
    fn test_runtime_provider_api_key_env_supports_anthropic_aliases_and_gemini_oauth() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );

        hermes_core::test_env::remove_var("ANTHROPIC_API_KEY");
        hermes_core::test_env::remove_var("ANTHROPIC_TOKEN");
        hermes_core::test_env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "claude-code-token");
        assert_eq!(
            agent
                .resolve_runtime_api_key("anthropic", None, None)
                .as_deref(),
            Some("claude-code-token")
        );
        hermes_core::test_env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");

        hermes_core::test_env::set_var("HERMES_GEMINI_OAUTH_API_KEY", "gemini-oauth-token");
        assert_eq!(
            agent
                .resolve_runtime_api_key("google-gemini-cli", None, None)
                .as_deref(),
            Some("gemini-oauth-token")
        );
        hermes_core::test_env::remove_var("HERMES_GEMINI_OAUTH_API_KEY");
    }

    #[test]
    fn test_oauth_refresh_config_anthropic_defaults_available() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        hermes_core::test_env::remove_var("HERMES_ANTHROPIC_OAUTH_TOKEN_URL");
        hermes_core::test_env::remove_var("HERMES_ANTHROPIC_OAUTH_CLIENT_ID");
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let (token_url, client_id) = agent.oauth_refresh_config("anthropic").unwrap();
        assert_eq!(token_url, "https://console.anthropic.com/v1/oauth/token");
        assert_eq!(client_id, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
    }

    #[test]
    fn test_oauth_refresh_config_openai_defaults_available() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        hermes_core::test_env::remove_var("HERMES_OPENAI_OAUTH_TOKEN_URL");
        hermes_core::test_env::remove_var("HERMES_OPENAI_OAUTH_CLIENT_ID");
        hermes_core::test_env::remove_var("HERMES_OPENAI_CODEX_OAUTH_TOKEN_URL");
        hermes_core::test_env::remove_var("HERMES_OPENAI_CODEX_OAUTH_CLIENT_ID");
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let (token_url, client_id) = agent.oauth_refresh_config("openai").unwrap();
        assert_eq!(token_url, "https://auth.openai.com/oauth/token");
        assert_eq!(client_id, "app_EMoamEEZ73f0CkXaXp7hrann");
    }

    #[test]
    fn test_oauth_refresh_config_nous_defaults_available() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        hermes_core::test_env::remove_var("HERMES_NOUS_OAUTH_TOKEN_URL");
        hermes_core::test_env::remove_var("HERMES_NOUS_OAUTH_CLIENT_ID");
        hermes_core::test_env::remove_var("NOUS_PORTAL_BASE_URL");
        hermes_core::test_env::remove_var("NOUS_CLIENT_ID");
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        let (token_url, client_id) = agent.oauth_refresh_config("nous").unwrap();
        assert_eq!(token_url, "https://portal.nousresearch.com/api/oauth/token");
        assert_eq!(client_id, "hermes-cli");
    }

    #[test]
    fn test_runtime_provider_stepfun_env_key_and_base_url_defaults() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let config = AgentConfig::default();
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );

        hermes_core::test_env::remove_var("HERMES_STEPFUN_API_KEY");
        hermes_core::test_env::set_var("STEPFUN_API_KEY", "stepfun-secret");
        let resolved = agent.resolve_runtime_api_key("stepfun", None, None);
        assert_eq!(resolved.as_deref(), Some("stepfun-secret"));
        hermes_core::test_env::remove_var("STEPFUN_API_KEY");

        let base = crate::runtime_provider::resolve_runtime_base_url(&agent, "stepfun", None);
        assert_eq!(base.as_deref(), Some("https://api.stepfun.ai/step_plan/v1"));
    }

    #[test]
    fn test_governor_reduces_budget_under_high_pressure() {
        let mut ctx = ContextManager::default_budget();
        let payload = "x".repeat(((ctx.max_context_chars() as f64) * 0.9) as usize);
        ctx.add_message(Message::user(payload));
        let config = AgentConfig {
            max_tokens: Some(1200),
            ..AgentConfig::default()
        };
        let gov = governor_for_turn(&config, &ctx, 12, None);
        assert!(gov.pressure >= 0.9);
        assert!(gov.max_tokens.unwrap_or(1200) < 1200);
        assert!(gov.tool_concurrency <= 4);
    }

    #[test]
    fn test_governor_reduces_budget_under_latency_degradation() {
        let ctx = ContextManager::default_budget();
        let config = AgentConfig {
            max_tokens: Some(1200),
            ..AgentConfig::default()
        };
        let runtime = GovernorRuntimeState {
            avg_llm_latency_ms: Some(7000.0),
            avg_tool_error_rate: 0.0,
            consecutive_error_turns: 0,
        };
        let gov = governor_for_turn(&config, &ctx, 6, Some(&runtime));
        assert!(gov.latency_degraded);
        assert!(gov.max_tokens.unwrap_or(1200) < 1200);
        assert!(gov.tool_concurrency <= 2);
    }

    #[test]
    fn test_governor_reduces_budget_under_error_degradation() {
        let ctx = ContextManager::default_budget();
        let config = AgentConfig {
            max_tokens: Some(1200),
            ..AgentConfig::default()
        };
        let runtime = GovernorRuntimeState {
            avg_llm_latency_ms: Some(1000.0),
            avg_tool_error_rate: 0.55,
            consecutive_error_turns: 3,
        };
        let gov = governor_for_turn(&config, &ctx, 10, Some(&runtime));
        assert!(gov.error_degraded);
        assert!(gov.max_tokens.unwrap_or(1200) < 1200);
        assert!(gov.tool_concurrency <= 2);
    }

    #[test]
    fn test_reliability_guard_requires_sustained_tool_errors_or_multi_sample_latency() {
        let ctx = ContextManager::default_budget();
        let config = AgentConfig {
            max_tokens: Some(1200),
            ..AgentConfig::default()
        };
        let one_error_turn = GovernorRuntimeState {
            avg_llm_latency_ms: Some(1000.0),
            avg_tool_error_rate: 0.0,
            consecutive_error_turns: 1,
        };
        let gov_one = governor_for_turn(&config, &ctx, 0, Some(&one_error_turn));
        assert!(!should_apply_turn_reliability_guard(
            &one_error_turn,
            &gov_one,
            1
        ));

        let slow_single_sample = GovernorRuntimeState {
            avg_llm_latency_ms: Some(7000.0),
            avg_tool_error_rate: 0.0,
            consecutive_error_turns: 0,
        };
        let gov_slow = governor_for_turn(&config, &ctx, 0, Some(&slow_single_sample));
        assert!(!should_apply_turn_reliability_guard(
            &slow_single_sample,
            &gov_slow,
            1
        ));
        assert!(should_apply_turn_reliability_guard(
            &slow_single_sample,
            &gov_slow,
            2
        ));

        let two_error_turns = GovernorRuntimeState {
            avg_llm_latency_ms: Some(1000.0),
            avg_tool_error_rate: 0.0,
            consecutive_error_turns: 2,
        };
        let gov_two = governor_for_turn(&config, &ctx, 0, Some(&two_error_turns));
        assert!(should_apply_turn_reliability_guard(
            &two_error_turns,
            &gov_two,
            0
        ));
    }

    #[test]
    fn test_resolve_reliability_degrade_model_does_not_hop_to_openai_by_default() {
        use futures::stream::BoxStream;

        type DummyProvider = crate::test_support::FixedAssistantProvider;

        let agent = AgentLoop::new(
            AgentConfig {
                provider: Some("anthropic".to_string()),
                model: "anthropic:claude-sonnet-4-20250514".to_string(),
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider::default()),
        );
        assert_eq!(
            crate::route_learning::resolve_reliability_degrade_model(
                &agent,
                "anthropic:claude-sonnet-4-20250514",
                None
            ),
            None
        );
    }

    #[test]
    fn test_tool_loop_guard_trips_on_consecutive_full_failure_turns() {
        hermes_core::test_env::set_var("HERMES_TOOL_LOOP_GUARD_ENABLED", "1");
        hermes_core::test_env::set_var("HERMES_TOOL_LOOP_GUARD_MAX_CONSEC_ERROR_TURNS", "3");
        hermes_core::test_env::set_var("HERMES_TOOL_LOOP_GUARD_MIN_FAILED_CALLS", "1");
        assert!(!should_trip_tool_loop_guard(2, 2, 2));
        assert!(should_trip_tool_loop_guard(3, 2, 2));
        hermes_core::test_env::remove_var("HERMES_TOOL_LOOP_GUARD_ENABLED");
        hermes_core::test_env::remove_var("HERMES_TOOL_LOOP_GUARD_MAX_CONSEC_ERROR_TURNS");
        hermes_core::test_env::remove_var("HERMES_TOOL_LOOP_GUARD_MIN_FAILED_CALLS");
    }

    #[test]
    fn test_tool_loop_guard_ignores_partial_success_turns() {
        hermes_core::test_env::set_var("HERMES_TOOL_LOOP_GUARD_ENABLED", "1");
        hermes_core::test_env::set_var("HERMES_TOOL_LOOP_GUARD_MAX_CONSEC_ERROR_TURNS", "2");
        hermes_core::test_env::set_var("HERMES_TOOL_LOOP_GUARD_MIN_FAILED_CALLS", "1");
        assert!(!should_trip_tool_loop_guard(4, 3, 2));
        hermes_core::test_env::remove_var("HERMES_TOOL_LOOP_GUARD_ENABLED");
        hermes_core::test_env::remove_var("HERMES_TOOL_LOOP_GUARD_MAX_CONSEC_ERROR_TURNS");
        hermes_core::test_env::remove_var("HERMES_TOOL_LOOP_GUARD_MIN_FAILED_CALLS");
    }

    #[test]
    fn test_looks_like_tool_error_output_detects_json_error_envelope() {
        assert!(looks_like_tool_error_output(
            r#"{"error":"Invalid tool parameters: Missing 'platform' parameter"}"#
        ));
        assert!(looks_like_tool_error_output(
            r#"{"success":false,"message":"failed"}"#
        ));
        assert!(!looks_like_tool_error_output(
            r#"{"success":true,"result":"ok"}"#
        ));
    }

    #[test]
    fn test_looks_like_tool_error_output_detects_text_error_signatures() {
        assert!(looks_like_tool_error_output("error: invalid request"));
        assert!(looks_like_tool_error_output(
            "Invalid tool parameters: Missing 'platform' parameter"
        ));
        assert!(!looks_like_tool_error_output("all good"));
    }

    #[test]
    fn test_redact_json_value_masks_sensitive_fields() {
        let mut payload = serde_json::json!({
            "api_key": "abc",
            "nested": { "token": "def", "safe": "ok" },
            "list": [{"password":"x"}, {"value":"y"}],
            "text": "Authorization: Bearer sk-secretvalue12345"
        });
        redact_json_value(&mut payload);
        assert_eq!(payload["api_key"], "[redacted]");
        assert_eq!(payload["nested"]["token"], "[redacted]");
        assert_eq!(payload["nested"]["safe"], "ok");
        assert_eq!(payload["list"][0]["password"], "[redacted]");
        assert_eq!(payload["text"], "Authorization: Bearer [redacted]");
    }

    #[test]
    fn test_replay_recorder_adds_hash_chain_metadata() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let replay_path = tmp.path().join("trace.jsonl");
        let recorder = ReplayRecorder {
            path: Some(replay_path.clone()),
            state: Some(Arc::new(Mutex::new(ReplayState {
                seq: 0,
                prev_hash: short_sha256_hex("seed"),
                trace_root: short_sha256_hex("trace-seed"),
            }))),
        };

        recorder.record("turn_start", serde_json::json!({"token":"abc"}));
        recorder.record("tool_call", serde_json::json!({"cmd":"echo ok"}));

        let body = std::fs::read_to_string(&replay_path).expect("replay file");
        let mut lines = body.lines();
        let first: serde_json::Value =
            serde_json::from_str(lines.next().expect("line1")).expect("json line1");
        let second: serde_json::Value =
            serde_json::from_str(lines.next().expect("line2")).expect("json line2");

        assert_eq!(first["seq"], 1);
        assert_eq!(second["seq"], 2);
        assert!(first.get("trace_id").is_some());
        assert!(second.get("trace_id").is_some());
        assert_eq!(first["payload"]["token"], "[redacted]");
        assert_eq!(second["prev_hash"], first["event_hash"]);
        assert_ne!(first["event_hash"], second["event_hash"]);
    }

    #[test]
    fn test_detect_contextlattice_connect_intent() {
        let msgs = vec![Message::user(
            "please confirm and connect to contextlattice, then harden it",
        )];
        assert!(detect_contextlattice_connect_intent(&msgs));

        let msgs = vec![Message::user("explain contextlattice architecture only")];
        assert!(!detect_contextlattice_connect_intent(&msgs));
    }

    #[test]
    fn test_contextlattice_connect_system_hint_emitted() {
        let msgs = vec![Message::user("connect to contextlattice and verify health")];
        let hint = contextlattice_connect_system_hint(&msgs).expect("expected hint");
        assert!(hint.contains("contextlattice_search"));
        assert!(hint.contains("scripts/agent_orchestration.py"));
        assert!(hint.contains("Never use terminal command `contextlattice`"));
    }

    #[test]
    fn test_contextlattice_intelligence_system_hint_requires_tools_and_intent() {
        let msgs = vec![Message::user(
            "perform deep repo audit and objective verification on /tmp/repo",
        )];
        let tools = vec![
            ToolSchema::new("contextlattice_search", "search", JsonSchema::new("object")),
            ToolSchema::new(
                "contextlattice_context_pack",
                "pack",
                JsonSchema::new("object"),
            ),
        ];
        let hint = contextlattice_intelligence_system_hint(&msgs, &tools).expect("expected hint");
        assert!(hint.contains("ContextLattice-first intelligence policy active"));
        assert!(hint.contains("scoped retrieval"));
        assert!(hint.contains("Copy numeric facts verbatim"));
    }

    #[test]
    fn test_contextlattice_intelligence_system_hint_skips_without_tools() {
        let msgs = vec![Message::user(
            "perform deep repo audit and objective verification on /tmp/repo",
        )];
        let tools = vec![ToolSchema::new(
            "terminal",
            "terminal",
            JsonSchema::new("object"),
        )];
        assert!(contextlattice_intelligence_system_hint(&msgs, &tools).is_none());
    }

    #[test]
    fn test_contextlattice_shell_invocation_detector() {
        assert!(is_contextlattice_shell_invocation(
            r#"{"command":"contextlattice"}"#
        ));
        assert!(is_contextlattice_shell_invocation(
            r#"{"command":"contextlattice status"}"#
        ));
        assert!(!is_contextlattice_shell_invocation(
            r#"{"command":"which contextlattice"}"#
        ));
        assert!(!is_contextlattice_shell_invocation(r#"{"command":"ls"}"#));
    }

    #[test]
    fn test_repo_review_tool_profile_keeps_todo_filters_messaging() {
        let msgs = vec![Message::user("review repo at /tmp/app and diagnose issue")];
        let mut calls = vec![
            ToolCall {
                id: "a".to_string(),
                function: hermes_core::FunctionCall {
                    name: "todo".to_string(),
                    arguments: "{}".to_string(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "b".to_string(),
                function: hermes_core::FunctionCall {
                    name: "telegram_send".to_string(),
                    arguments: r#"{"text":"status"}"#.to_string(),
                },
                extra_content: None,
            },
        ];
        let note = apply_repo_review_tool_profile_narrowing(&mut calls, &msgs);
        assert!(note.is_some());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "todo");
    }

    #[test]
    fn test_repo_review_tool_profile_escape_hatch_disables_filtering() {
        let _guard = env_test_lock();
        hermes_core::test_env::set_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "focus");
        let msgs = vec![Message::user(
            "review repo at /tmp/app and diagnose issue; allow all tools",
        )];
        let mut calls = vec![ToolCall {
            id: "b".to_string(),
            function: hermes_core::FunctionCall {
                name: "telegram_send".to_string(),
                arguments: r#"{"text":"status"}"#.to_string(),
            },
            extra_content: None,
        }];
        let note = apply_repo_review_tool_profile_narrowing(&mut calls, &msgs);
        assert!(note.is_some());
        assert_eq!(calls.len(), 1, "escape hatch should bypass filtering");
        hermes_core::test_env::remove_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE");
    }

    #[test]
    fn test_repo_review_tool_profile_off_mode_disables_filtering() {
        let _guard = env_test_lock();
        hermes_core::test_env::set_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "off");
        let msgs = vec![Message::user("review repo at /tmp/app and diagnose issue")];
        let mut calls = vec![ToolCall {
            id: "b".to_string(),
            function: hermes_core::FunctionCall {
                name: "telegram_send".to_string(),
                arguments: r#"{"text":"status"}"#.to_string(),
            },
            extra_content: None,
        }];
        let note = apply_repo_review_tool_profile_narrowing(&mut calls, &msgs);
        assert!(note.is_none());
        assert_eq!(calls.len(), 1, "off mode should keep all calls");
        hermes_core::test_env::remove_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE");
    }

    #[test]
    fn test_repo_review_discovery_policy_trims_repeated_loops() {
        let _guard = env_test_lock();
        hermes_core::test_env::set_var("HERMES_REPO_REVIEW_DISCOVERY_BUDGET_MODE", "enforce");
        let msgs = vec![Message::user(
            "inspect repo /tmp/app and review codebase deeply",
        )];
        let mut state = RepoReviewBudgetState::default();
        let make_calls = || {
            vec![
                ToolCall {
                    id: "1".to_string(),
                    function: hermes_core::FunctionCall {
                        name: "terminal".to_string(),
                        arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                    },
                    extra_content: None,
                },
                ToolCall {
                    id: "2".to_string(),
                    function: hermes_core::FunctionCall {
                        name: "terminal".to_string(),
                        arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                    },
                    extra_content: None,
                },
                ToolCall {
                    id: "3".to_string(),
                    function: hermes_core::FunctionCall {
                        name: "terminal".to_string(),
                        arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                    },
                    extra_content: None,
                },
            ]
        };
        let mut first = make_calls();
        assert!(apply_repo_review_discovery_budget_policy(&mut first, &msgs, &mut state).is_none());
        let mut second = make_calls();
        assert!(
            apply_repo_review_discovery_budget_policy(&mut second, &msgs, &mut state).is_none()
        );
        let mut third = make_calls();
        let note = apply_repo_review_discovery_budget_policy(&mut third, &msgs, &mut state);
        assert!(note.is_some());
        assert!(third.len() < 3);
        hermes_core::test_env::remove_var("HERMES_REPO_REVIEW_DISCOVERY_BUDGET_MODE");
    }

    #[test]
    fn test_repo_review_discovery_policy_advisory_keeps_calls() {
        let _guard = env_test_lock();
        hermes_core::test_env::set_var("HERMES_REPO_REVIEW_DISCOVERY_BUDGET_MODE", "advisory");
        let msgs = vec![Message::user(
            "inspect repo /tmp/app and review codebase deeply",
        )];
        let mut state = RepoReviewBudgetState::default();
        let mut first = vec![
            ToolCall {
                id: "1".to_string(),
                function: hermes_core::FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "2".to_string(),
                function: hermes_core::FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "3".to_string(),
                function: hermes_core::FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                },
                extra_content: None,
            },
        ];
        assert!(apply_repo_review_discovery_budget_policy(&mut first, &msgs, &mut state).is_none());
        let mut second = first.clone();
        let _ = apply_repo_review_discovery_budget_policy(&mut second, &msgs, &mut state);
        let mut third = first.clone();
        let note = apply_repo_review_discovery_budget_policy(&mut third, &msgs, &mut state);
        assert!(note.is_some());
        assert_eq!(third.len(), 3, "advisory mode must not trim tool calls");
        hermes_core::test_env::remove_var("HERMES_REPO_REVIEW_DISCOVERY_BUDGET_MODE");
    }

    #[test]
    fn test_finalizer_output_quality_retry_detects_placeholders() {
        let templated =
            "**Title:** Example\n**Authors:** pack of authors\n(Full text available at [URL](URL))";
        assert!(finalizer_output_quality_requires_retry(templated, 0));
    }

    #[test]
    fn test_finalizer_output_quality_retry_detects_duplicate_lines() {
        let duplicated = "- **Title:** Bayesian Learning for Dive State Prediction and Management\n\
            - **Title:** Bayesian Learning for Dive State Prediction and Management\n\
            - **Title:** Bayesian Learning for Dive State Prediction and Management\n\
            - **Title:** Bayesian Learning for Dive State Prediction and Management";
        assert!(finalizer_output_quality_requires_retry(duplicated, 0));
        assert!(!finalizer_output_quality_requires_retry(duplicated, 2));
    }

    #[test]
    fn test_finalizer_action_execution_retry_detects_intent_narration() {
        let msgs = vec![Message::user(
            "proceed with deep repo review for /tmp/app and implement patches",
        )];
        assert!(finalizer_action_execution_requires_retry(
            &msgs,
            "I will proceed now and report back shortly.",
            0
        ));
        assert!(!finalizer_action_execution_requires_retry(
            &msgs,
            "I will proceed now and report back shortly.",
            2
        ));
    }

    #[test]
    fn test_finalizer_action_execution_retry_skips_when_evidence_present() {
        let msgs = vec![Message::user(
            "proceed with deep repo review for /tmp/app and implement patches",
        )];
        assert!(!finalizer_action_execution_requires_retry(
            &msgs,
            "cmd=rg -n TODO src\nfile=/tmp/app/src/main.rs\nobjective_state=advancing",
            0
        ));
    }

    #[test]
    fn test_objective_guard_requires_sections_for_trading_objective() {
        let msgs = vec![
            Message::system("[SESSION_OBJECTIVE] Exponentiate Solana wallet via trading."),
            Message::user("review repo /tmp/algotraderv2_rust and produce patch plan"),
        ];
        let (active, needs_analytics, deep_audit_required) = objective_guard_policy(&msgs);
        assert!(active);
        assert!(needs_analytics);
        assert!(!deep_audit_required);
        assert!(!objective_guard_satisfied("plain response", true, false));
        assert!(objective_guard_satisfied(
            "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=advancing metric=+0.42 SOL",
            true,
            false
        ));
    }

    #[test]
    fn test_deep_objective_guard_requires_deep_audit_section() {
        let msgs = vec![
            Message::system("[SESSION_OBJECTIVE] Exponentiate Solana wallet via trading."),
            Message::user(
                "deep end-to-end review repo /tmp/algotraderv2_rust and produce complete patch plan",
            ),
        ];
        let (active, needs_analytics, deep_audit_required) = objective_guard_policy(&msgs);
        assert!(active);
        assert!(needs_analytics);
        assert!(deep_audit_required);

        let shallow = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=advancing metric=+0.42 SOL";
        assert!(!objective_guard_satisfied(shallow, true, true));

        let numeric_only = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/b.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=flat metric=+0.00 SOL\nDEEP_AUDIT_VERIFIED:\n- scope_complete=true\n- verified_files=8\n- commands_run=5\n- unknowns=1\n- blockers=none";
        assert!(!objective_guard_satisfied(numeric_only, true, true));

        let deep = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/b.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=flat metric=+0.00 SOL\nDEEP_AUDIT_VERIFIED:\n- scope_complete=true\n- workstream=ingestion status=complete evidence(file=/tmp/a.rs cmd=rg -n ingest src)\n- workstream=strategy status=complete evidence(file=/tmp/b.rs cmd=sed -n 1,220p src/strategy.rs)\n- workstream=execution status=complete evidence(file=/tmp/c.rs cmd=cargo test -p hermes-agent objective_guard)\n- file=/tmp/a.rs\n- file=/tmp/b.rs\n- file=/tmp/c.rs\n- file=/tmp/d.rs\n- file=/tmp/e.rs\n- cmd=rg -n objective src\n- cmd=sed -n 1,220p src/main.rs\n- cmd=cargo test -p hermes-agent objective_guard\n- unknowns=1\n- blockers=none";
        assert!(objective_guard_satisfied(deep, true, true));
    }

    #[test]
    fn test_deep_objective_retry_prompt_contains_audit_requirements() {
        let prompt = objective_guard_retry_prompt(true, true);
        assert!(prompt.contains(OBJECTIVE_DEEP_AUDIT_TAG));
        assert!(prompt.contains("file=<verified_path_1>"));
        assert!(prompt.contains("cmd=<command_1>"));
        assert!(prompt.contains("workstream=<name> status=<complete|blocked|unproven>"));
    }

    #[test]
    fn test_deep_objective_scope_complete_rejects_non_complete_streams() {
        let text = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/b.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=flat metric=+0.00 SOL\nDEEP_AUDIT_VERIFIED:\n- scope_complete=true\n- workstream=ingestion status=complete evidence(file=/tmp/a.rs cmd=rg -n ingest src)\n- workstream=strategy status=blocked evidence(file=/tmp/b.rs cmd=rg -n strategy src)\n- workstream=execution status=complete evidence(file=/tmp/c.rs cmd=cargo test)\n- file=/tmp/a.rs\n- file=/tmp/b.rs\n- file=/tmp/c.rs\n- file=/tmp/d.rs\n- file=/tmp/e.rs\n- cmd=rg -n objective src\n- cmd=sed -n 1,220p src/main.rs\n- cmd=cargo test -p hermes-agent objective_guard\n- unknowns=1\n- blockers=rpc unavailable";
        assert!(!objective_guard_satisfied(text, true, true));
    }

    #[test]
    fn test_coerce_textual_tool_calls_extracts_and_cleans_message() {
        let msg = Message::assistant(
            "Proceeding with discovery now.\n<tool_call name=\"skill_view\">\n<argument name=\"skill\">contextlattice-master-router</argument>\n</tool_call>",
        );
        let (coerced, calls, parsed_textual) = coerce_textual_tool_calls(msg);
        assert!(parsed_textual);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "skill_view");
        assert_eq!(
            coerced.content.as_deref(),
            Some("Proceeding with discovery now.")
        );
    }

    #[test]
    fn test_coerce_textual_tool_calls_keeps_declared_calls() {
        let msg = Message::assistant_with_tool_calls(
            Some("Running tool.".to_string()),
            vec![ToolCall {
                id: "id1".to_string(),
                function: hermes_core::FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"pwd"}"#.to_string(),
                },
                extra_content: None,
            }],
        );
        let (coerced, calls, parsed_textual) = coerce_textual_tool_calls(msg);
        assert!(!parsed_textual);
        assert_eq!(calls.len(), 1);
        assert_eq!(coerced.tool_calls.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(coerced.content.as_deref(), Some("Running tool."));
    }

    #[test]
    fn test_extract_objective_state_marker_prefers_explicit_marker() {
        let text = "ANALYTICS_VERIFIED:\n- objective_state=advancing metric=+0.12 SOL";
        assert_eq!(extract_objective_state_marker(text), "advancing");
        let colon_text = "ANALYTICS_VERIFIED:\n- objective_state: regressing metric=-0.30 SOL";
        assert_eq!(extract_objective_state_marker(colon_text), "regressing");
    }

    #[test]
    fn test_extract_marker_values_collects_unique_paths_and_cmds() {
        let text = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/b.rs exists_now=true\nDEEP_AUDIT_VERIFIED:\n- cmd=rg -n objective src\n- cmd=cargo test -p hermes-agent objective_guard";
        let files = extract_marker_values(text, "path=", 8);
        let cmds = extract_marker_values(text, "cmd=", 8);
        assert_eq!(
            files,
            vec!["/tmp/a.rs".to_string(), "/tmp/b.rs".to_string()]
        );
        assert_eq!(cmds, vec!["rg".to_string(), "cargo".to_string()]);
    }

    #[test]
    fn test_format_tool_progress_message_web_and_repeat() {
        let web = vec!["web_search".to_string()];
        assert!(format_tool_progress_message(3, &web, 1).contains("检索网络数据"));
        assert!(format_tool_progress_message(3, &web, 2).contains("仍在进行"));

        let local = vec!["todo".to_string()];
        assert!(format_tool_progress_message(1, &local, 1).contains("todo"));
    }

    #[test]
    fn test_summarize_tool_failure_for_user_web_extract_403() {
        let msg = summarize_tool_failure_for_user(
            "web_extract",
            "HTTP 403 Forbidden when fetching 'https://zhuanlan.zhihu.com/p/1'. This site blocks automated access.",
        )
        .expect("expected user notice");
        assert!(msg.contains("拒绝自动抓取"));
    }

    #[test]
    fn test_summarize_tool_failure_for_user_browser_cdp() {
        let msg = summarize_tool_failure_for_user(
            "browser_navigate",
            "Chrome CDP not reachable. Start Chrome with --remote-debugging-port=9222 or set HERMES_BROWSER_AUTO_START=1",
        )
        .expect("expected user notice");
        assert!(msg.contains("浏览器"));
    }

    #[test]
    fn test_apply_web_tool_budget_caps_web_search_calls() {
        let _guard = env_test_lock();
        hermes_core::test_env::set_var("HERMES_WEB_SEARCH_BUDGET_MAX_CALLS", "2");
        let mut calls = vec![ToolCall {
            id: "s1".to_string(),
            function: hermes_core::FunctionCall {
                name: "web_search".to_string(),
                arguments: r#"{"query":"test"}"#.to_string(),
            },
            extra_content: None,
        }];
        let blocked = apply_web_tool_budget(&mut calls, 0, 2, 0, 1);
        assert_eq!(blocked.len(), 1);
        assert!(calls.is_empty());
        hermes_core::test_env::remove_var("HERMES_WEB_SEARCH_BUDGET_MAX_CALLS");
    }

    #[test]
    fn test_apply_web_tool_budget_includes_browser_navigate() {
        let mut calls = vec![ToolCall {
            id: "b1".to_string(),
            function: hermes_core::FunctionCall {
                name: "browser_navigate".to_string(),
                arguments: r#"{"url":"https://example.com"}"#.to_string(),
            },
            extra_content: None,
        }];
        let blocked = apply_web_tool_budget(&mut calls, 3, 0, 0, 1);
        assert_eq!(blocked.len(), 1);
        assert!(calls.is_empty());
    }

    /// Documents the turn-level API message cache contract used by
    /// `conversation_loop` (`invalidate_turn_api_messages_cache` each inner iteration).
    #[test]
    fn turn_api_messages_cache_contract() {
        use crate::test_support::ErrNoopProvider as NoopProvider;

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(NoopProvider),
        );

        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::user("aaa"));
        let first = crate::llm_caller::build_turn_api_messages(&agent, &mut ctx);
        let second = crate::llm_caller::build_turn_api_messages(&agent, &mut ctx);
        assert!(
            Arc::ptr_eq(&first, &second),
            "same ctx should return cached Arc"
        );

        ctx.add_message(Message::assistant("draft"));
        let after_assistant = crate::llm_caller::build_turn_api_messages(&agent, &mut ctx);
        assert!(
            !Arc::ptr_eq(&first, &after_assistant),
            "different ctx should return new Arc"
        );

        let _ = ctx.get_messages_mut().pop();
        let after_pop = crate::llm_caller::build_turn_api_messages(&agent, &mut ctx);
        assert!(
            !after_pop.iter().any(|m| m.role == MessageRole::Assistant),
            "ctx invalidation should recompute after pop"
        );
        let mut ctx_inplace = ContextManager::default_budget();
        ctx_inplace.add_message(Message::user("aaa"));
        let before_edit = crate::llm_caller::build_turn_api_messages(&agent, &mut ctx_inplace);
        ctx_inplace.get_messages_mut()[0].content = Some("xyz".to_string());
        let stale_hit = crate::llm_caller::build_turn_api_messages(&agent, &mut ctx_inplace);
        assert!(
            Arc::ptr_eq(&before_edit, &stale_hit),
            "in-place mutation without invalidation must return stale cache hit"
        );
        agent.invalidate_turn_api_messages_cache();
        let after_invalidate = crate::llm_caller::build_turn_api_messages(&agent, &mut ctx_inplace);
        assert!(!Arc::ptr_eq(&before_edit, &after_invalidate));
        let user_text = after_invalidate
            .iter()
            .find(|m| m.role == MessageRole::User)
            .and_then(|m| m.content.as_deref());
        assert_eq!(user_text, Some("xyz"));
    }
}
