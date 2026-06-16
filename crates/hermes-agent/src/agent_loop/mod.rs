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
use hermes_intelligence::get_model_context_length;
use serde_json::Value;

use hermes_core::{
    AgentError, AgentResult, LlmProvider, LlmResponse, Message, MessageRole, StreamChunk, ToolCall,
    ToolResult, ToolSchema, UsageStats,
};

use crate::agent_runtime_helpers;
use crate::auxiliary_builder::{AuxiliaryBuildParams, build_auxiliary_client};
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
    partial_stream_dropped_tool_names, sanitize_surrogates, strip_budget_warnings_from_messages,
};
use crate::plugins::{HookType, PluginManager, PluginManagerHandle};
use crate::replay::short_sha256_hex;
use crate::session_persistence::{SessionFlushCursor, SessionPersistence};
use crate::skill_orchestrator::SkillOrchestrator;
pub use crate::smart_model_routing::{ApiMode, CheapModelRouteConfig, SmartModelRoutingConfig};
use crate::smart_model_routing::{PrimaryRuntime, TurnRouteSignature};
use crate::steer::PendingSteer;
use crate::system_prompt::{format_probe_output, platform_hint_for, probe_remote_backend_cached};
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
    CompactionGovernanceMode, EvolutionCounters, OAuthStoreCredential,
    compaction_governance_mode_runtime, contextlattice_orchestration_script_path,
    has_ssl_transient_phrase, rand_u64_range, should_inject_tool_enforcement_for_model,
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
pub(crate) use crate::objective_guard::{
    OBJECTIVE_DEEP_AUDIT_MAX_RETRIES, OBJECTIVE_DEEP_AUDIT_TAG, OBJECTIVE_GUARD_MAX_RETRIES,
    detect_repo_review_intent, exploratory_problem_solving_system_hint, extract_session_objective,
    objective_guard_policy, objective_guard_retry_prompt, objective_guard_satisfied,
    objective_mode_system_hint,
};
pub(crate) use crate::tool_profile::{
    RepoReviewBudgetState, apply_repo_review_discovery_budget_policy,
    apply_repo_review_tool_profile_narrowing, update_repo_review_budget_state_from_results,
};
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
        auth_json.display().to_string().replace('\\', "/")
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
            if let Some(exit_code) = obj.get("exit_code").and_then(|v| v.as_i64()) {
                if exit_code != 0 {
                    return true;
                }
            }
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
    if let Some(code) = parse_terminal_exit_code_suffix(trimmed) {
        if code != 0 {
            return true;
        }
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("error:")
        || lower.contains("invalid tool parameters")
        || lower.contains("missing '")
}

fn parse_terminal_exit_code_suffix(output: &str) -> Option<i32> {
    const PREFIX: &str = "[exit code: ";
    let start = output.rfind(PREFIX)? + PREFIX.len();
    let rest = &output[start..];
    let end = rest.find(']')?;
    rest[..end].trim().parse().ok()
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
    pub plugin_manager: Option<PluginManagerHandle>,
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
    /// SmartRouter facade: wraps per-route learning stats + frozen primary runtime.
    pub(crate) router: crate::smart_router::SmartRouter,
    /// When set, tool calls use this async path instead of sync `ToolRegistry` handlers
    /// (avoids `block_in_place` + `block_on` from inside `JoinSet` tasks on the gateway).
    pub(crate) async_tool_dispatch: Option<AsyncToolDispatch>,
    /// Mid-run `/steer` queue (Python `_pending_steer`).
    pub(crate) pending_steer: PendingSteer,
    /// Compression orchestrator (wraps ContextCompressor; orchestration methods live on AgentLoop).
    pub(crate) context_compressor: crate::compression_orchestrator::ContextCompressionOrchestrator,
    /// Reused SQLite persistence handle for this agent (Python `SessionDB._conn` parity).
    shared_session_persistence: std::sync::OnceLock<Arc<SessionPersistence>>,
    /// Per-turn cache for OpenAI-compat provider message/tool JSON serialization.
    pub(crate) provider_serialize_cache:
        Arc<crate::provider_serialize_cache::ProviderSerializeCache>,
    /// Python `_disable_streaming` — set after "stream not supported" for the rest of the session.
    pub(crate) disable_streaming: Arc<AtomicBool>,
    /// Per-turn vision capability (Python `_vision_supported`; reset each `prepare_turn`).
    pub(crate) vision_supported: Arc<std::sync::atomic::AtomicBool>,
    /// Runtime prompt-caching policy (updated each LLM call, avoids config deep-clone).
    pub(crate) use_prompt_caching: Arc<AtomicBool>,
    pub(crate) use_native_cache_layout: Arc<AtomicBool>,
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
            api_calls: exit.api_calls,
            turn_exit_reason: exit.turn_exit_reason.to_string(),
            failed: exit.failed,
            partial: exit.partial,
            interrupt_message: if exit.interrupted {
                self.interrupt.peek_redirect_message()
            } else {
                None
            },
            plan_pending: exit.plan_pending,
            plan_phase: exit.plan_phase,
            ..Default::default()
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
            let restored = fb.restore_primary_runtime(
                &self.router.stored_primary_runtime,
                &mut state.active_runtime,
            );
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
        let context_compressor =
            crate::compression_orchestrator::ContextCompressionOrchestrator::new(
                build_context_compressor_for_config(&config),
            );
        let init_prompt_caching = config.use_prompt_caching;
        let init_native_cache_layout = config.use_native_cache_layout;
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
            router: crate::smart_router::SmartRouter::new(route_learning, stored_primary_runtime),
            async_tool_dispatch: None,
            pending_steer: PendingSteer::new(),
            context_compressor,
            shared_session_persistence: std::sync::OnceLock::new(),
            provider_serialize_cache: Arc::new(
                crate::provider_serialize_cache::ProviderSerializeCache::new(),
            ),
            disable_streaming: Arc::new(AtomicBool::new(false)),
            vision_supported: Arc::new(AtomicBool::new(true)),
            use_prompt_caching: Arc::new(AtomicBool::new(init_prompt_caching)),
            use_native_cache_layout: Arc::new(AtomicBool::new(init_native_cache_layout)),
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
            use_prompt_caching: self
                .use_prompt_caching
                .load(std::sync::atomic::Ordering::Relaxed),
            use_native_cache_layout: self
                .use_native_cache_layout
                .load(std::sync::atomic::Ordering::Relaxed),
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
        let context_compressor =
            crate::compression_orchestrator::ContextCompressionOrchestrator::new(
                build_context_compressor_for_config(&config),
            );
        let init_prompt_caching = config.use_prompt_caching;
        let init_native_cache_layout = config.use_native_cache_layout;
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
            router: crate::smart_router::SmartRouter::new(route_learning, stored_primary_runtime),
            async_tool_dispatch: None,
            pending_steer: PendingSteer::new(),
            context_compressor,
            shared_session_persistence: std::sync::OnceLock::new(),
            provider_serialize_cache: Arc::new(
                crate::provider_serialize_cache::ProviderSerializeCache::new(),
            ),
            disable_streaming: Arc::new(AtomicBool::new(false)),
            vision_supported: Arc::new(AtomicBool::new(true)),
            use_prompt_caching: Arc::new(AtomicBool::new(init_prompt_caching)),
            use_native_cache_layout: Arc::new(AtomicBool::new(init_native_cache_layout)),
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

    /// Attach local POI store (independent of external memory / `skip_memory`).
    pub fn with_interest_store(mut self, store: Arc<Mutex<InterestStore>>) -> Self {
        self.interest_store = Some(store);
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

    /// Set the plugin manager.
    pub fn with_plugins(mut self, pm: Arc<std::sync::Mutex<PluginManager>>) -> Self {
        self.plugin_manager = Some(PluginManagerHandle::new(pm));
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
}

mod compression_impl;
mod context_preprocess;
mod cost_fns;
mod finalizer_fns;
mod memory_ops;
mod oauth_impl;
mod objective_ledger;
mod prompt_builders;
mod runtime_build;
mod session_compress;

pub(crate) use finalizer_fns::{
    contextlattice_connect_system_hint, contextlattice_intelligence_system_hint,
    detect_contextlattice_connect_intent, extract_last_user_assistant,
    finalizer_action_execution_requires_retry, finalizer_claim_requires_evidence_retry,
    finalizer_output_quality_requires_retry, inject_runtime_tool_params,
    is_contextlattice_shell_invocation, latest_user_content, objective_eval_score,
    session_search_has_query, summarize_background_review_result,
};

pub(crate) use cost_fns::{
    estimate_usage_cost_usd, extract_marker_values, extract_objective_state_marker, merge_usage,
};

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

    /// Returns `(full_prompt, restored_from_storage)` — restored prompts skip fresh build.
    pub(crate) fn resolve_initial_system_prompt(
        &self,
        task_hint: &str,
        tool_schemas: &[ToolSchema],
    ) -> (String, bool) {
        let r =
            crate::conversation_loop::resolve_initial_system_prompt(self, task_hint, tool_schemas);
        (r.full_prompt, r.restored)
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
mod tests;
