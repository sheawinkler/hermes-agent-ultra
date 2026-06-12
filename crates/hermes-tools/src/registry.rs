//! Tool Registry
//!
//! Central registry for all tool definitions and handlers. Supports:
//! - Dynamic registration/deregistration with availability checks
//! - Name-conflict detection with warning on overwrite
//! - Per-tool result size limits and global default
//! - Dispatch with error catching that always returns JSON

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use hermes_core::{ToolHandler, ToolSchema};
use serde_json::Value;
use tracing::warn;

use crate::plan_mode::{plan_allows_tool, plan_block_payload, PlanPhase};
use crate::rtk_filter::{RawModeState, RtkFilterEngine};
use crate::tool_dispatch_helpers::{ParallelMode, infer_parallel_mode};
use crate::tool_policy::{
    ToolPolicyCounters, ToolPolicyDecision, ToolPolicyEngine, annotate_policy_audit,
    annotate_policy_simulation, default_tool_policy_counters_path, persist_tool_policy_counters,
};
use crate::tools::schema_sanitizer::sanitize_tool_schema_list;

// ---------------------------------------------------------------------------
// ToolEntry
// ---------------------------------------------------------------------------

/// A registered tool with its handler, metadata, and availability check.
pub struct ToolEntry {
    /// Unique tool name (e.g. "web_search", "terminal").
    pub name: String,
    /// Which toolset this tool belongs to (e.g. "web", "terminal").
    pub toolset: String,
    /// OpenAI-format tool schema.
    pub schema: ToolSchema,
    /// Handler that executes the tool logic.
    pub handler: Arc<dyn ToolHandler>,
    /// Availability check — returns true if the tool can be used right now.
    pub check_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Environment dependencies required for this tool (e.g. "EXA_API_KEY").
    pub env_deps: Vec<String>,
    /// Whether this tool performs async I/O.
    pub is_async: bool,
    /// Human-readable description.
    pub description: String,
    /// Emoji icon for display.
    pub emoji: String,
    /// Per-tool max result size override (characters).
    pub max_result_size_chars: Option<usize>,
    /// Parallelism classification, assigned once at registration.
    pub parallel_mode: ParallelMode,
}

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

/// Thread-safe registry of all available tools.
///
/// # Lock ordering invariants
///
/// To prevent deadlocks, locks must always be acquired in this order:
///   1. `tools` (RwLock)
///   2. `aliases` (RwLock)
///   3. `raw_state` (Mutex) — held only briefly; never across `await`
///   4. `counters` (Mutex) — held only briefly; never across `await`
///
/// `policy` is a separate RwLock for mutable `set_policy` calls; it is
/// never held simultaneously with `tools` or `aliases`.
///
/// The hot dispatch path (`dispatch_async`) never holds `tools` across
/// the `handler.execute().await` call — the read guard is dropped before
/// any async work begins.
#[derive(Clone)]
pub struct ToolRegistry {
    /// Registered tools keyed by name. Read-heavy, infrequently written.
    tools: Arc<RwLock<HashMap<String, ToolEntry>>>,
    /// Toolset alias map. Rarely mutated.
    aliases: Arc<RwLock<HashMap<String, String>>>,
    /// Policy engine. Immutable after initial construction; mutable only via `set_policy`.
    policy: Arc<RwLock<ToolPolicyEngine>>,
    /// RTK filter engine. Never mutated after construction; `Arc` for cheap dispatch clones.
    rtk: Arc<RtkFilterEngine>,
    /// Session-wide and one-shot raw pass-through flags.
    raw_state: Arc<Mutex<RawModeState>>,
    /// Running counters for policy outcomes.
    counters: Arc<Mutex<ToolPolicyCounters>>,
    /// Global default max result size (characters). Immutable after construction.
    global_max_result_size_chars: usize,
    /// Plan-then-execute phase gate.
    plan_phase: Arc<Mutex<PlanPhase>>,
}

impl ToolRegistry {
    /// Create a new empty registry with default global max result size.
    pub fn new() -> Self {
        Self {
            tools: Arc::new(RwLock::new(HashMap::new())),
            aliases: Arc::new(RwLock::new(HashMap::new())),
            global_max_result_size_chars: 50_000,
            policy: Arc::new(RwLock::new(ToolPolicyEngine::from_env())),
            rtk: Arc::new(RtkFilterEngine::from_env()),
            raw_state: Arc::new(Mutex::new(RawModeState {
                enabled: false,
                once_pending: false,
            })),
            counters: Arc::new(Mutex::new(ToolPolicyCounters::default())),
            plan_phase: Arc::new(Mutex::new(PlanPhase::Off)),
        }
    }

    /// Create a new registry with a custom global max result size.
    pub fn with_max_result_size(max: usize) -> Self {
        Self {
            tools: Arc::new(RwLock::new(HashMap::new())),
            aliases: Arc::new(RwLock::new(HashMap::new())),
            global_max_result_size_chars: max,
            policy: Arc::new(RwLock::new(ToolPolicyEngine::from_env())),
            rtk: Arc::new(RtkFilterEngine::from_env()),
            raw_state: Arc::new(Mutex::new(RawModeState {
                enabled: false,
                once_pending: false,
            })),
            counters: Arc::new(Mutex::new(ToolPolicyCounters::default())),
            plan_phase: Arc::new(Mutex::new(PlanPhase::Off)),
        }
    }

    /// Set plan-then-execute phase for tool dispatch gating.
    pub fn set_plan_phase(&self, phase: PlanPhase) {
        *self.plan_phase.lock().unwrap() = phase;
    }

    pub fn plan_phase(&self) -> PlanPhase {
        *self.plan_phase.lock().unwrap()
    }

    /// Override active tool policy engine (used by tests/runtime tuning).
    pub fn set_policy(&self, policy: ToolPolicyEngine) {
        *self.policy.write().unwrap() = policy;
    }

    /// Snapshot current policy counters.
    pub fn policy_counters(&self) -> ToolPolicyCounters {
        self.counters.lock().unwrap().clone()
    }

    /// Preview the current policy decision for a tool call without executing it.
    pub fn evaluate_policy_preview(&self, name: &str, params: &Value) -> ToolPolicyDecision {
        self.policy.read().unwrap().evaluate(name, params)
    }

    /// Enable or disable session-wide raw pass-through mode.
    pub fn set_raw_mode(&self, enabled: bool) {
        let mut state = self.raw_state.lock().unwrap();
        state.enabled = enabled;
        if enabled {
            state.once_pending = false;
        }
    }

    /// Enable one-shot raw pass-through for the next tool dispatch.
    pub fn set_raw_mode_once(&self) {
        self.raw_state.lock().unwrap().once_pending = true;
    }

    /// Current raw-mode state.
    pub fn raw_mode_state(&self) -> RawModeState {
        *self.raw_state.lock().unwrap()
    }

    /// RTK dual-log directory.
    pub fn rtk_log_dir(&self) -> PathBuf {
        self.rtk.log_dir().to_path_buf()
    }

    /// Register a new tool.
    ///
    /// If a tool with the same name already exists, logs a warning and overwrites.
    pub fn register(
        &self,
        name: impl Into<String>,
        toolset: impl Into<String>,
        schema: ToolSchema,
        handler: Arc<dyn ToolHandler>,
        check_fn: Arc<dyn Fn() -> bool + Send + Sync>,
        env_deps: Vec<String>,
        is_async: bool,
        description: impl Into<String>,
        emoji: impl Into<String>,
        max_result_size_chars: Option<usize>,
    ) {
        let name = name.into();
        let mut tools = self.tools.write().unwrap();
        if tools.contains_key(&name) {
            warn!("Tool '{}' already registered; overwriting", name);
        }
        let parallel_mode = infer_parallel_mode(&name);
        tools.insert(
            name.clone(),
            ToolEntry {
                name: name.clone(),
                toolset: toolset.into(),
                schema,
                handler,
                check_fn,
                env_deps,
                is_async,
                description: description.into(),
                emoji: emoji.into(),
                max_result_size_chars,
                parallel_mode,
            },
        );
    }

    /// Return the `ParallelMode` assigned at registration for the named tool.
    ///
    /// Returns `ParallelMode::Unknown` for tools not in the registry.
    pub fn parallel_mode_of(&self, name: &str) -> ParallelMode {
        self.tools
            .read()
            .unwrap()
            .get(name)
            .map(|e| e.parallel_mode)
            .unwrap_or(ParallelMode::Unknown)
    }

    /// Deregister a tool by name.
    ///
    /// Returns `true` if the tool was present and removed.
    pub fn deregister(&self, name: &str) -> bool {
        let (removed_toolset, has_remaining) = {
            let mut tools = self.tools.write().unwrap();
            let Some(removed) = tools.remove(name) else {
                return false;
            };
            let target = removed.toolset.clone();
            let remaining = tools.values().any(|e| e.toolset == target);
            (target, remaining)
        };
        if !has_remaining {
            self.aliases
                .write()
                .unwrap()
                .retain(|_, target| target != &removed_toolset);
        }
        true
    }

    /// Get tool definitions for all tools whose `check_fn` returns true.
    ///
    /// The read lock on `tools` is released before any `check_fn` is called
    /// so that availability checks cannot deadlock against concurrent registrations.
    pub fn get_definitions(&self) -> Vec<ToolSchema> {
        let entries: Vec<(ToolSchema, Arc<dyn Fn() -> bool + Send + Sync>)> = {
            let tools = self.tools.read().unwrap();
            tools
                .values()
                .map(|e| (e.schema.clone(), Arc::clone(&e.check_fn)))
                .collect()
        };
        let definitions: Vec<ToolSchema> = entries
            .into_iter()
            .filter(|(_, check)| (check)())
            .map(|(schema, _)| schema)
            .collect();
        sanitize_tool_schema_list(definitions)
    }

    /// Dispatch a tool call by name, catching all errors.
    ///
    /// On success, returns the tool result string.
    /// On failure, returns a JSON error string: `{"error": "..."}`.
    pub fn dispatch(&self, name: &str, params: Value) -> String {
        // 1. Short raw_state lock — read and clear once-flag.
        let raw_bypassed = {
            let mut state = self.raw_state.lock().unwrap();
            let bypassed = state.enabled || state.once_pending;
            if state.once_pending {
                state.once_pending = false;
            }
            bypassed
        };
        // 2. Evaluate policy and rewrite params — no tools lock needed.
        let (effective_params, rewrite_applied) =
            self.rtk.rewrite_params(name, &params, raw_bypassed);
        let policy_decision = self.policy.read().unwrap().evaluate(name, &params);
        let plan_phase = *self.plan_phase.lock().unwrap();
        // 3. Short read lock on tools — clone handler and cap, then release.
        let (handler, max_chars) = {
            let tools = self.tools.read().unwrap();
            match tools.get(name) {
                Some(e) => (
                    Arc::clone(&e.handler),
                    e.max_result_size_chars
                        .unwrap_or(self.global_max_result_size_chars),
                ),
                None => return Self::tool_error(&format!("Tool not found: {}", name)),
            }
        };
        self.record_policy_decision(&policy_decision);
        if !policy_decision.allow {
            return Self::tool_policy_error(name, &policy_decision);
        }
        if !plan_allows_tool(plan_phase, name, &effective_params) {
            return plan_block_payload(name);
        }
        maybe_log_audit(name, &policy_decision);

        // Sync entry point: only safe outside a tokio worker. Gateway agents use
        // `dispatch_async` via `AgentLoop::with_async_tool_dispatch` instead.
        let result = if tokio::runtime::Handle::try_current().is_ok() {
            return Self::tool_error(
                "ToolRegistry::dispatch called from async context; use dispatch_async",
            );
        } else {
            tokio::runtime::Runtime::new()
                .map(|rt| rt.block_on(async { handler.execute(effective_params.clone()).await }))
                .unwrap_or_else(|e| Err(hermes_core::ToolError::ExecutionFailed(e.to_string())))
        };

        // 4. Filter output — fully lock-free.
        let output = match result {
            Ok(output) => {
                let filtered = self.rtk.filter_and_log(
                    name,
                    &effective_params,
                    &output,
                    raw_bypassed,
                    rewrite_applied,
                );
                Self::tool_result(&truncate_to_chars(&filtered, max_chars))
            }
            Err(e) => {
                let err_text = e.to_string();
                let filtered = self.rtk.filter_and_log(
                    name,
                    &effective_params,
                    &err_text,
                    raw_bypassed,
                    rewrite_applied,
                );
                Self::tool_error(&truncate_to_chars(&filtered, max_chars))
            }
        };
        maybe_annotate_audit(output, &policy_decision)
    }

    /// Dispatch a tool call asynchronously.
    ///
    /// # Lock invariants
    ///
    /// No lock is held across the `handler.execute().await` call:
    ///   - `raw_state` Mutex: acquired and released before any async work.
    ///   - `tools` RwLock: acquired to clone the handler, released immediately.
    ///   - `policy` RwLock: acquired to evaluate the decision, released immediately.
    pub async fn dispatch_async(&self, name: &str, params: Value) -> String {
        // 1. Short raw_state lock — read and clear once-flag.
        let raw_bypassed = {
            let mut state = self.raw_state.lock().unwrap();
            let bypassed = state.enabled || state.once_pending;
            if state.once_pending {
                state.once_pending = false;
            }
            bypassed
        };
        // 2. Evaluate policy and rewrite params — no tools lock needed.
        let (effective_params, rewrite_applied) =
            self.rtk.rewrite_params(name, &params, raw_bypassed);
        let policy_decision = self.policy.read().unwrap().evaluate(name, &params);
        let plan_phase = *self.plan_phase.lock().unwrap();
        // 3. Short read lock on tools — clone handler, cap, and required fields, then release.
        let (handler, max_chars, required_fields) = {
            let tools = self.tools.read().unwrap();
            match tools.get(name) {
                Some(e) => {
                    let required = e.schema.parameters.required.clone().unwrap_or_default();
                    (
                        Arc::clone(&e.handler),
                        e.max_result_size_chars
                            .unwrap_or(self.global_max_result_size_chars),
                        required,
                    )
                }
                None => return Self::tool_error(&format!("Tool not found: {}", name)),
            }
        };
        // 4. Handle policy outcome — counters Mutex held briefly.
        self.record_policy_decision(&policy_decision);
        if !policy_decision.allow {
            return Self::tool_policy_error(name, &policy_decision);
        }
        if !plan_allows_tool(plan_phase, name, &effective_params) {
            return plan_block_payload(name);
        }
        maybe_log_audit(name, &policy_decision);

        // Validate required parameters before invoking the handler.
        for field in &required_fields {
            if effective_params.get(field).map_or(true, |v| v.is_null()) {
                return Self::tool_error(&format!(
                    "Missing required parameter '{}' for tool '{}'.",
                    field, name
                ));
            }
        }

        // 5. Execute handler — completely lock-free.
        let output = match handler.execute(effective_params.clone()).await {
            Ok(output) => {
                let filtered = self.rtk.filter_and_log(
                    name,
                    &effective_params,
                    &output,
                    raw_bypassed,
                    rewrite_applied,
                );
                Self::tool_result(&truncate_to_chars(&filtered, max_chars))
            }
            Err(e) => {
                let err_text = e.to_string();
                let filtered = self.rtk.filter_and_log(
                    name,
                    &effective_params,
                    &err_text,
                    raw_bypassed,
                    rewrite_applied,
                );
                Self::tool_error(&truncate_to_chars(&filtered, max_chars))
            }
        };
        maybe_annotate_audit(output, &policy_decision)
    }

    /// Get a reference to a tool entry by name.
    pub fn get_tool(&self, name: &str) -> Option<ToolEntryInfo> {
        let tools = self.tools.read().unwrap();
        tools.get(name).map(|e| ToolEntryInfo {
            name: e.name.clone(),
            toolset: e.toolset.clone(),
            description: e.description.clone(),
            emoji: e.emoji.clone(),
            is_async: e.is_async,
            env_deps: e.env_deps.clone(),
            max_result_size_chars: e.max_result_size_chars,
        })
    }

    /// List all registered tool entries.
    pub fn list_tools(&self) -> Vec<ToolEntryInfo> {
        let tools = self.tools.read().unwrap();
        tools
            .values()
            .map(|e| ToolEntryInfo {
                name: e.name.clone(),
                toolset: e.toolset.clone(),
                description: e.description.clone(),
                emoji: e.emoji.clone(),
                is_async: e.is_async,
                env_deps: e.env_deps.clone(),
                max_result_size_chars: e.max_result_size_chars,
            })
            .collect()
    }

    /// List distinct toolset names.
    pub fn list_toolsets(&self) -> Vec<String> {
        let mut sets: Vec<String> = {
            let tools = self.tools.read().unwrap();
            tools.values().map(|e| e.toolset.clone()).collect()
        };
        sets.extend(self.aliases.read().unwrap().keys().cloned());
        sets.sort();
        sets.dedup();
        sets
    }

    /// Return the underlying aliases Arc so it can be shared with a `ToolsetManager`.
    ///
    /// Both objects then see consistent alias state through the shared `RwLock`
    /// without creating a circular ownership cycle.
    pub fn aliases_arc(&self) -> Arc<RwLock<HashMap<String, String>>> {
        Arc::clone(&self.aliases)
    }

    /// Register an explicit alias from a user-facing toolset token to its
    /// canonical live-registry toolset.
    pub fn register_toolset_alias(&self, alias: impl Into<String>, target: impl Into<String>) {
        let alias = alias.into().trim().to_string();
        let target = target.into().trim().to_string();
        if alias.is_empty() || target.is_empty() {
            return;
        }
        self.aliases.write().unwrap().insert(alias, target);
    }

    /// Return the canonical target for a registered toolset alias.
    pub fn get_toolset_alias_target(&self, alias: &str) -> Option<String> {
        self.aliases.read().unwrap().get(alias).cloned()
    }

    /// Check whether the registry owns this live toolset or alias.
    pub fn has_toolset(&self, toolset: &str) -> bool {
        let aliases = self.aliases.read().unwrap();
        let resolved = resolve_toolset_alias(&aliases, toolset);
        if aliases.contains_key(toolset) {
            return true;
        }
        let tools = self.tools.read().unwrap();
        tools.values().any(|entry| entry.toolset == resolved)
    }

    /// Return tool names belonging to a live registry-owned toolset or alias.
    pub fn tool_names_for_toolset(&self, toolset: &str, available_only: bool) -> Vec<String> {
        let resolved = {
            let aliases = self.aliases.read().unwrap();
            resolve_toolset_alias(&aliases, toolset)
        };
        let entries: Vec<(String, Arc<dyn Fn() -> bool + Send + Sync>)> = {
            let tools = self.tools.read().unwrap();
            tools
                .values()
                .filter(|entry| entry.toolset == resolved)
                .map(|entry| (entry.name.clone(), Arc::clone(&entry.check_fn)))
                .collect()
        };
        let mut names: Vec<String> = entries
            .into_iter()
            .filter(|(_, check)| !available_only || (check)())
            .map(|(name, _)| name)
            .collect();
        names.sort();
        names
    }

    /// Check whether a tool is available (exists and check_fn passes).
    pub fn is_available(&self, name: &str) -> bool {
        let check: Option<Arc<dyn Fn() -> bool + Send + Sync>> = {
            let tools = self.tools.read().unwrap();
            tools.get(name).map(|e| Arc::clone(&e.check_fn))
        };
        check.map_or(false, |f| f())
    }

    /// Format an error as JSON: `{"error": "msg"}`.
    pub fn tool_error(msg: &str) -> String {
        serde_json::json!({ "error": msg }).to_string()
    }

    fn tool_policy_error(tool_name: &str, decision: &ToolPolicyDecision) -> String {
        serde_json::json!({
            "error": format!(
                "Blocked by tool policy: {}",
                decision.reason.as_deref().unwrap_or("policy deny")
            ),
            "policy": {
                "tool": tool_name,
                "mode": decision.mode.as_str(),
                "decision": "deny",
                "code": decision.code.as_deref().unwrap_or("policy_deny"),
                "reason": decision.reason.as_deref().unwrap_or("policy deny"),
            }
        })
        .to_string()
    }

    /// Format a tool result. If the result looks like JSON, pass it through;
    /// otherwise wrap it as `{"result": "..."}`.
    pub fn tool_result(data: &str) -> String {
        if looks_like_json(data) {
            data.to_string()
        } else {
            serde_json::json!({ "result": data }).to_string()
        }
    }

    fn record_policy_decision(&self, decision: &ToolPolicyDecision) {
        let counters_snapshot = {
            let mut counters = self.counters.lock().unwrap();
            if decision.allow {
                counters.allow = counters.allow.saturating_add(1);
            } else {
                counters.deny = counters.deny.saturating_add(1);
            }
            if decision.audited_only {
                counters.audit_only = counters.audit_only.saturating_add(1);
            }
            if decision.simulated {
                counters.simulate = counters.simulate.saturating_add(1);
            }
            if decision.would_block {
                counters.would_block = counters.would_block.saturating_add(1);
            }
            counters.clone()
        };
        let _ =
            persist_tool_policy_counters(&default_tool_policy_counters_path(), &counters_snapshot);
    }
}

fn maybe_log_audit(tool_name: &str, decision: &ToolPolicyDecision) {
    if decision.audited_only {
        warn!(
            "Tool policy audit-only violation for '{}': {}",
            tool_name,
            decision.reason.as_deref().unwrap_or("no reason supplied")
        );
    } else if decision.simulated {
        warn!(
            "Tool policy simulation for '{}': {}",
            tool_name,
            decision.reason.as_deref().unwrap_or("simulation"),
        );
    }
}

fn maybe_annotate_audit(output: String, decision: &ToolPolicyDecision) -> String {
    if decision.simulated {
        annotate_policy_simulation(
            &output,
            decision
                .reason
                .as_deref()
                .unwrap_or(if decision.would_block {
                    "simulation: would block"
                } else {
                    "simulation: would allow"
                }),
            decision.would_block,
            decision.code.as_deref(),
        )
    } else if decision.audited_only {
        annotate_policy_audit(
            &output,
            decision
                .reason
                .as_deref()
                .unwrap_or("tool policy audit warning"),
        )
    } else {
        output
    }
}

fn resolve_toolset_alias(aliases: &HashMap<String, String>, name: &str) -> String {
    let mut current = name.to_string();
    let mut seen = HashSet::new();
    while seen.insert(current.clone()) {
        let Some(next) = aliases.get(&current) else {
            return current;
        };
        current = next.clone();
    }
    // Alias cycles are invalid configuration; fall back to the last distinct
    // name so resolution stays deterministic instead of looping forever.
    current
}

fn looks_like_json(data: &str) -> bool {
    let bytes = data.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() {
        return false;
    }
    matches!(bytes[i], b'{' | b'[')
}

fn truncate_to_chars(output: &str, max_chars: usize) -> String {
    if output.chars().count() <= max_chars {
        return output.to_string();
    }
    let cutoff = output
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(output.len());
    format!(
        "{}\n\n[... truncated {} characters ...]",
        &output[..cutoff],
        output.chars().count() - max_chars
    )
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Simplified info about a tool entry (no handler, for inspection).
#[derive(Debug, Clone)]
pub struct ToolEntryInfo {
    pub name: String,
    pub toolset: String,
    pub description: String,
    pub emoji: String,
    pub is_async: bool,
    pub env_deps: Vec<String>,
    pub max_result_size_chars: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use hermes_core::{JsonSchema, ToolError, tool_schema};
    use serde_json::json;
    use std::time::Instant;

    use crate::tool_policy::{ToolPolicyEngine, ToolPolicyMode};

    struct EchoHandler;

    #[async_trait]
    impl ToolHandler for EchoHandler {
        async fn execute(&self, params: Value) -> Result<String, ToolError> {
            Ok(params.to_string())
        }
        fn schema(&self) -> ToolSchema {
            tool_schema("echo", "Echo back input", JsonSchema::new("object"))
        }
    }

    #[test]
    fn test_register_and_dispatch() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "echo",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Echo tool",
            "🔊",
            None,
        );

        let defs = registry.get_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "echo");
    }

    #[test]
    fn test_dispatch_not_found() {
        let registry = ToolRegistry::new();
        let result = registry.dispatch("nonexistent", Value::Null);
        assert!(result.contains("error"));
    }

    #[test]
    fn test_tool_error_format() {
        let msg = ToolRegistry::tool_error("something went wrong");
        let parsed: Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(parsed["error"], "something went wrong");
    }

    #[test]
    fn test_tool_result_format() {
        // Plain string wrapped as JSON
        let result = ToolRegistry::tool_result("hello");
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["result"], "hello");

        // Already JSON passes through
        let json_result = ToolRegistry::tool_result(r#"{"key": "val"}"#);
        let parsed: Value = serde_json::from_str(&json_result).unwrap();
        assert_eq!(parsed["key"], "val");

        // JSON with whitespace prefix still passes through.
        let spaced = ToolRegistry::tool_result("  [1,2,3]");
        let parsed: Value = serde_json::from_str(&spaced).unwrap();
        assert_eq!(parsed.as_array().map(|a| a.len()), Some(3));
    }

    #[test]
    fn test_deregister() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "echo",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Echo tool",
            "🔊",
            None,
        );
        assert!(registry.deregister("echo"));
        assert!(!registry.deregister("echo"));
        assert!(registry.get_definitions().is_empty());
    }

    #[test]
    fn toolset_alias_resolves_live_registry_tools() {
        let registry = ToolRegistry::new();
        let schema = tool_schema("mcp_dynserver_ping", "MCP ping", JsonSchema::new("object"));
        registry.register(
            "mcp_dynserver_ping",
            "mcp-dynserver",
            schema,
            Arc::new(EchoHandler),
            Arc::new(|| true),
            vec![],
            false,
            "MCP ping",
            "x",
            None,
        );
        registry.register_toolset_alias("dynserver", "mcp-dynserver");

        assert_eq!(
            registry.get_toolset_alias_target("dynserver").as_deref(),
            Some("mcp-dynserver")
        );
        assert!(registry.has_toolset("dynserver"));
        assert_eq!(
            registry.tool_names_for_toolset("dynserver", true),
            vec!["mcp_dynserver_ping".to_string()]
        );
        let toolsets = registry.list_toolsets();
        assert!(toolsets.contains(&"dynserver".to_string()));
        assert!(toolsets.contains(&"mcp-dynserver".to_string()));
    }

    #[test]
    fn toolset_alias_cleanup_waits_for_last_target_tool() {
        let registry = ToolRegistry::new();
        for tool_name in ["mcp_dynserver_ping", "mcp_dynserver_status"] {
            let schema = tool_schema(tool_name, "MCP tool", JsonSchema::new("object"));
            registry.register(
                tool_name,
                "mcp-dynserver",
                schema,
                Arc::new(EchoHandler),
                Arc::new(|| true),
                vec![],
                false,
                "MCP tool",
                "x",
                None,
            );
        }
        registry.register_toolset_alias("dynserver", "mcp-dynserver");

        assert!(registry.deregister("mcp_dynserver_ping"));
        assert_eq!(
            registry.get_toolset_alias_target("dynserver").as_deref(),
            Some("mcp-dynserver")
        );
        assert_eq!(
            registry.tool_names_for_toolset("dynserver", true),
            vec!["mcp_dynserver_status".to_string()]
        );

        assert!(registry.deregister("mcp_dynserver_status"));
        assert_eq!(registry.get_toolset_alias_target("dynserver"), None);
        assert!(!registry.has_toolset("dynserver"));
    }

    #[test]
    fn test_check_fn_filtering() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "available",
            "test",
            schema.clone(),
            Arc::new(EchoHandler),
            Arc::new(|| true),
            vec![],
            false,
            "Available",
            "✅",
            None,
        );
        registry.register(
            "unavailable",
            "test",
            schema,
            Arc::new(EchoHandler),
            Arc::new(|| false),
            vec![],
            false,
            "Unavailable",
            "❌",
            None,
        );
        let defs = registry.get_definitions();
        assert_eq!(defs.len(), 1);
        // ToolSchema name comes from the handler's schema(), which is "echo" for EchoHandler.
        // The registry key ("available") and the schema name may differ.
        assert_eq!(defs[0].name, "echo");
    }

    struct BareObjectSchemaHandler;

    #[async_trait]
    impl ToolHandler for BareObjectSchemaHandler {
        async fn execute(&self, _params: Value) -> Result<String, ToolError> {
            Ok("ok".to_string())
        }

        fn schema(&self) -> ToolSchema {
            tool_schema(
                "bare_object",
                "Bare object schema",
                JsonSchema::new("object"),
            )
        }
    }

    #[test]
    fn get_definitions_sanitizes_tool_schemas() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(BareObjectSchemaHandler);
        let schema = handler.schema();
        registry.register(
            "bare_object",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Bare object schema",
            "T",
            None,
        );

        let defs = registry.get_definitions();
        assert_eq!(defs.len(), 1);
        assert!(defs[0].parameters.properties.is_some());
    }

    #[test]
    fn test_dispatch_async_read_allowed_in_plan_mode() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "read_file",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Read file",
            "📄",
            None,
        );
        registry.set_plan_phase(PlanPhase::Planning);
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let result = rt.block_on(async {
            registry
                .dispatch_async("read_file", json!({"path": "a.rs"}))
                .await
        });
        let parsed: Value = serde_json::from_str(&result).expect("json output");
        assert!(!parsed.get("plan").is_some(), "read should not plan_block: {result}");
        assert!(result.contains("a.rs") || result.contains("path"));
    }

    #[test]
    fn test_dispatch_async_blocked_by_plan_mode() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "echo",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Echo",
            "🔊",
            None,
        );
        registry.set_plan_phase(PlanPhase::Planning);
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let result = rt.block_on(async { registry.dispatch_async("echo", json!({"k":"v"})).await });
        let parsed: Value = serde_json::from_str(&result).expect("json output");
        assert_eq!(parsed["plan"]["decision"], "plan_block");
        assert_eq!(parsed["plan"]["code"], "plan_write_denied");
    }

    #[test]
    fn test_dispatch_async_blocked_by_policy() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "echo",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Echo",
            "🔊",
            None,
        );
        registry
            .set_policy(ToolPolicyEngine::new(ToolPolicyMode::Enforce).with_denylist(&["echo"]));
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let result = rt.block_on(async { registry.dispatch_async("echo", json!({"k":"v"})).await });
        assert!(result.contains("Blocked by tool policy"));
        let parsed: Value = serde_json::from_str(&result).expect("json output");
        assert_eq!(parsed["policy"]["tool"], "echo");
        assert_eq!(parsed["policy"]["decision"], "deny");
        assert_eq!(parsed["policy"]["mode"], "enforce");
    }

    #[test]
    fn test_dispatch_async_audit_annotation() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "echo",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Echo",
            "🔊",
            None,
        );
        registry
            .set_policy(ToolPolicyEngine::new(ToolPolicyMode::Audit).with_allowlist(&["other"]));
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let result = rt.block_on(async { registry.dispatch_async("echo", json!({"k":"v"})).await });
        let parsed: Value = serde_json::from_str(&result).expect("json output");
        assert!(
            parsed["_tool_policy_warning"]
                .as_str()
                .unwrap_or("")
                .contains("allowlist")
        );
        assert_eq!(parsed["k"], "v");
    }

    #[tokio::test]
    async fn dispatch_simulation_mode_attaches_policy_metadata() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "echo",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Echo tool",
            "🔊",
            None,
        );
        registry
            .set_policy(ToolPolicyEngine::new(ToolPolicyMode::Simulate).with_denylist(&["echo"]));
        let out = registry
            .dispatch_async("echo", json!({"msg":"hello"}))
            .await;
        let parsed: Value = serde_json::from_str(&out).expect("json");
        assert_eq!(parsed["msg"], "hello");
        assert_eq!(parsed["_tool_policy_simulation"]["mode"], "simulate");
        assert_eq!(parsed["_tool_policy_simulation"]["would_block"], true);
        assert_eq!(parsed["_tool_policy_simulation"]["code"], "tool_denylisted");
    }

    #[test]
    fn policy_counters_track_dispatch_outcomes() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "echo",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Echo",
            "🔊",
            None,
        );
        let rt = tokio::runtime::Runtime::new().expect("runtime");

        registry
            .set_policy(ToolPolicyEngine::new(ToolPolicyMode::Enforce).with_denylist(&["echo"]));
        let _ = rt.block_on(async { registry.dispatch_async("echo", json!({"k":"v"})).await });
        let counters = registry.policy_counters();
        assert_eq!(counters.deny, 1);
        assert_eq!(counters.would_block, 1);

        registry
            .set_policy(ToolPolicyEngine::new(ToolPolicyMode::Simulate).with_denylist(&["echo"]));
        let _ = rt.block_on(async { registry.dispatch_async("echo", json!({"k":"v"})).await });
        let counters = registry.policy_counters();
        assert_eq!(counters.allow, 1);
        assert_eq!(counters.simulate, 1);
        assert_eq!(counters.audit_only, 1);
        assert_eq!(counters.would_block, 2);
    }

    #[test]
    fn evaluate_policy_preview_reports_decision_without_dispatch() {
        let registry = ToolRegistry::new();
        registry
            .set_policy(ToolPolicyEngine::new(ToolPolicyMode::Enforce).with_denylist(&["echo"]));
        let decision = registry.evaluate_policy_preview("echo", &json!({"msg":"preview"}));
        assert!(!decision.allow);
        assert_eq!(decision.mode.as_str(), "enforce");
        assert_eq!(decision.code.as_deref(), Some("tool_denylisted"));
    }

    #[test]
    fn tool_result_fast_path_benchmark_report() {
        let payload = r#"{"alpha":1,"beta":2,"nested":{"k":"v","arr":[1,2,3]}}"#;
        let warmup = 1_000usize;
        for _ in 0..warmup {
            let _ = ToolRegistry::tool_result(payload);
            let _ = ToolRegistry::tool_result("plain result text");
        }

        let iters = 20_000usize;
        let start = Instant::now();
        for _ in 0..iters {
            let _ = ToolRegistry::tool_result(payload);
            let _ = ToolRegistry::tool_result("plain result text");
        }
        let elapsed = start.elapsed();
        let ns_per_op = elapsed.as_nanos() / (iters as u128 * 2);
        println!("registry_tool_result_ns_per_op={}", ns_per_op);
        assert!(
            ns_per_op < 300_000,
            "registry tool_result fast-path regressed: {} ns/op",
            ns_per_op
        );
    }
}
