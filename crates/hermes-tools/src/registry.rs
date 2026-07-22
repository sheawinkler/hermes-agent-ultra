//! Tool Registry (Requirement 4)
//!
//! Central registry for all tool definitions and handlers. Supports:
//! - Dynamic registration/deregistration with availability checks
//! - Name-conflict detection with warning on overwrite
//! - Per-tool result size limits and global default
//! - Dispatch with error catching that always returns JSON

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use hermes_core::{MemorySubjectStorageKey, ToolError, ToolHandler, ToolSchema};
use hermes_environments::{ContainmentInvocation, ExecutionContainmentContext};

use serde_json::{json, Value};
use tracing::warn;

use crate::credential_guard::SkillTrustPathGuard;
use crate::rtk_filter::{RawModeState, RtkFilterEngine};
use crate::tool_policy::{
    default_tool_policy_counters_path, persist_tool_policy_counters, ToolPolicyCounters,
    ToolPolicyDecision, ToolPolicyEngine, ToolPolicyMode,
};
use crate::tools::disk_cleanup::{CleanupSummary, DiskCleanupAutoTracker};


fn explicit_manual_execution_containment() -> Arc<ExecutionContainmentContext> {
    Arc::new(
        ExecutionContainmentContext::manual(hermes_core::ResolvedExecutionMode::default())
            .expect("the default execution mode is manual"),
    )
}
static NEXT_REGISTRATION_ID: AtomicU64 = AtomicU64::new(1);

fn requires_containment(effect: Option<VettedToolEffect>) -> bool {
    !matches!(effect, Some(VettedToolEffect::Observation))
}

// ---------------------------------------------------------------------------
// ToolEntry
// ---------------------------------------------------------------------------

/// Effect authority attached only by Hermes' built-in registration path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VettedToolEffect {
    /// The handler may observe external state but must not mutate it.
    Observation,
    /// The handler may produce an externally visible, non-replayable effect.
    LocalOneShot,
}

/// A handler able to consume the registry-issued invocation token and execute
/// only through the same certified containment context. Regular handlers never
/// receive this token and therefore cannot impersonate a contained dispatch.
#[async_trait::async_trait]
pub(crate) trait ContainmentSensitiveToolHandler: Send + Sync {
    async fn execute_with_containment(
        &self,
        params: Value,
        invocation: ContainmentInvocation,
    ) -> Result<String, ToolError>;
}

/// Typed outcome preserving whether a handler invocation began.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeDispatchOutcome {
    RejectedBeforeInvoke(String),
    Returned(String),
    FailedAfterInvoke(String),
    UnknownAfterInvoke(String),
}

impl RuntimeDispatchOutcome {
    fn map_wire(self, map: impl FnOnce(String) -> String) -> Self {
        match self {
            Self::RejectedBeforeInvoke(output) => Self::RejectedBeforeInvoke(map(output)),
            Self::Returned(output) => Self::Returned(map(output)),
            Self::FailedAfterInvoke(output) => Self::FailedAfterInvoke(map(output)),
            Self::UnknownAfterInvoke(output) => Self::UnknownAfterInvoke(map(output)),
        }
    }

    pub fn into_wire(self) -> String {
        match self {
            Self::RejectedBeforeInvoke(output)
            | Self::Returned(output)
            | Self::FailedAfterInvoke(output)
            | Self::UnknownAfterInvoke(output) => output,
        }
    }
}

/// Machine-readable policy preview returned when dispatch mode is `simulate`.
///
/// This outcome is synthesized before tool lookup or handler invocation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ToolPolicySyntheticPreview {
    pub status: &'static str,
    pub preview_type: &'static str,
    pub tool: String,
    pub policy: ToolPolicyDispatchMetadata,
    pub will_execute: bool,
    pub counter_effect: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ToolPolicyDispatchMetadata {
    pub mode: &'static str,
    pub decision: &'static str,
    pub allow: bool,
    pub would_block: bool,
    pub code: Option<String>,
    pub reason: Option<String>,
    pub audit_recorded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PolicyDispatchAction {
    Execute,
    Reject,
    Preview,
}

#[derive(Debug, Clone)]
struct PolicyDispatchPlan {
    mode: ToolPolicyMode,
    decision: ToolPolicyDecision,
    action: PolicyDispatchAction,
    containment_context: Option<Arc<ExecutionContainmentContext>>,

}


#[derive(Clone)]
pub struct RuntimeToolDefinition {
    schema: ToolSchema,
    registration_id: u64,
    vetted_effect: Option<VettedToolEffect>,
}

impl std::fmt::Debug for RuntimeToolDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeToolDefinition")
            .field("schema", &self.schema)
            .field("registration_id", &self.registration_id)
            .field("vetted_effect", &self.vetted_effect)
            .finish_non_exhaustive()
    }
}

impl RuntimeToolDefinition {
    pub fn schema(&self) -> &ToolSchema {
        &self.schema
    }

    /// Revalidate capability provenance against the exact live registry row.
    pub fn vetted_effect(&self, registry: &ToolRegistry) -> Option<VettedToolEffect> {
        let inner = registry.inner.lock();
        let entry = inner.tools.get(&self.schema.name)?;
        (entry.registration_id == self.registration_id)
            .then_some(entry.vetted_effect)
            .flatten()
    }
}
/// A per-agent subject binding for the exact canonical built-in `memory`
/// backend registered by Hermes Tools.
///
/// The handler is always constructed here from that private backend plus an
/// opaque authenticated subject key. Callers cannot substitute arbitrary code
/// while inheriting built-in provenance. Dispatch revalidates the live
/// registration and uses its policy, counters, filters, containment state, and
/// output boundary.
#[derive(Clone)]
pub struct BoundRuntimeToolDefinition {
    registry: ToolRegistry,
    definition: RuntimeToolDefinition,
    handler: Arc<crate::tools::memory::MemoryHandler>,
}

impl std::fmt::Debug for BoundRuntimeToolDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BoundRuntimeToolDefinition")
            .field("schema", self.definition.schema())
            .finish_non_exhaustive()
    }
}

impl BoundRuntimeToolDefinition {
    pub fn schema(&self) -> &ToolSchema {
        self.definition.schema()
    }

    pub fn vetted_effect(&self) -> Option<VettedToolEffect> {
        self.definition.vetted_effect(&self.registry)
    }

    pub fn dispatch(&self, params: Value) -> RuntimeDispatchOutcome {
        let handler: Arc<dyn ToolHandler> = self.handler.clone();
        self.registry.dispatch_expected(
            &self.definition.schema.name,
            Some(self.definition.registration_id),
            Some(handler),
            params,
        )
    }
}

#[derive(Clone)]
struct CanonicalMemoryBackendBinding {
    hermes_home: PathBuf,
    backend: Arc<dyn crate::tools::memory::MemoryBackend>,
}

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
    /// Present only for handlers installed by the crate-private vetted path.
    /// Changes on every registration so stale definitions cannot dispatch an
    /// overwritten handler.
    pub(crate) registration_id: u64,
    pub(crate) vetted_effect: Option<VettedToolEffect>,
    containment_handler: Option<Arc<dyn ContainmentSensitiveToolHandler>>,
    /// The exact home and backend captured only by canonical built-in memory
    /// registration. Arbitrary handlers cannot populate this binding.
    canonical_memory_backend: Option<CanonicalMemoryBackendBinding>,

}

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

/// Thread-safe registry of all available tools.
pub struct ToolRegistryInner {
    /// Registered tools keyed by name.
    tools: HashMap<String, ToolEntry>,
    /// Explicit aliases for dynamic toolset names, for example an MCP server
    /// name (`dynserver`) pointing at its canonical toolset (`mcp-dynserver`).
    toolset_aliases: HashMap<String, String>,
    /// Global default max result size in characters.
    pub global_max_result_size_chars: usize,
    /// Centralized policy engine for tool-call governance.
    pub policy: ToolPolicyEngine,
    /// Runtime counters for policy outcomes (allow/deny/audit/simulate).
    pub policy_counters: ToolPolicyCounters,
    /// RTK-style filter engine for rewrite/filter/log handling.
    pub rtk_filter: RtkFilterEngine,
    /// Session-wide raw pass-through mode for tool output.
    pub rtk_raw_mode: bool,
    /// One-shot raw pass-through for the next tool call only.
    pub rtk_raw_once: bool,
    /// Context shared with containment-sensitive handler dispatch. It starts
    /// as the explicit manual-host context, never an inferred backend label.
    execution_containment: Arc<ExecutionContainmentContext>,
    /// Verifier-only skill-store state, enforced before policy/raw dispatch.
    skill_trust_paths: SkillTrustPathGuard,
}

/// Thread-safe wrapper around `ToolRegistryInner`.
#[derive(Clone)]
pub struct ToolRegistry {
    inner: Arc<Mutex<ToolRegistryInner>>,
    disk_cleanup_auto: DiskCleanupAutoTracker,
}

impl ToolRegistry {
    /// Create a new empty registry with default global max result size.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ToolRegistryInner {
                tools: HashMap::new(),
                toolset_aliases: HashMap::new(),
                global_max_result_size_chars: 50_000,
                policy: ToolPolicyEngine::from_env(),
                policy_counters: ToolPolicyCounters::default(),
                rtk_filter: RtkFilterEngine::from_env(),
                rtk_raw_mode: false,
                rtk_raw_once: false,
                execution_containment: explicit_manual_execution_containment(),
                skill_trust_paths: SkillTrustPathGuard::default(),
            })),
            disk_cleanup_auto: DiskCleanupAutoTracker::default(),
        }
    }

    /// Create a new registry with a custom global max result size.
    pub fn with_max_result_size(max: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ToolRegistryInner {
                tools: HashMap::new(),
                toolset_aliases: HashMap::new(),
                global_max_result_size_chars: max,
                policy: ToolPolicyEngine::from_env(),
                policy_counters: ToolPolicyCounters::default(),
                rtk_filter: RtkFilterEngine::from_env(),
                rtk_raw_mode: false,
                rtk_raw_once: false,
                execution_containment: explicit_manual_execution_containment(),
                skill_trust_paths: SkillTrustPathGuard::default(),
            })),
            disk_cleanup_auto: DiskCleanupAutoTracker::default(),
        }
    }

    /// Run Rust-native disk cleanup session-end behavior if this session
    /// auto-tracked any test files through write/terminal/patch tools.
    pub fn disk_cleanup_session_end(&self) -> Option<CleanupSummary> {
        self.disk_cleanup_auto.on_session_end()
    }

    /// Override active tool policy engine (used by tests/runtime tuning).
    pub fn set_policy(&self, policy: ToolPolicyEngine) {
        let mut inner = self.inner.lock();
        inner.policy = policy;
    }

    /// Bind the exact execution context that containment-sensitive handlers
    /// must consume. Replacing it affects only future dispatches.
    pub fn set_execution_containment_context(
        &self,
        context: Arc<ExecutionContainmentContext>,
    ) {
        let mut inner = self.inner.lock();
        inner.execution_containment = context;
    }

    /// Protect the exact host skills root plus explicit remote trust-root,
    /// root public-key, epoch, and audit authority paths from all runtime tool
    /// effects. This is independent of policy mode and cannot be bypassed by
    /// raw dispatch.
    pub fn set_skill_trust_protected_paths(
        &self,
        skills_root: impl Into<PathBuf>,
        external_authority_paths: Vec<PathBuf>,
    ) {
        let mut inner = self.inner.lock();
        inner.skill_trust_paths =
            SkillTrustPathGuard::new(skills_root, external_authority_paths);
    }

    /// Bind the registry to a fully validated, explicitly provisioned skill
    /// trust authority. Production ingress must call this before registering
    /// tools; it deliberately has no environment or default-path fallback.
    pub fn bind_skill_trust_authority_paths(
        &self,
        authority: &hermes_skills::SkillTrustAuthorityPaths,
    ) {
        self.set_skill_trust_protected_paths(
            authority.skills_root().to_path_buf(),
            authority.external_authority_paths(),
        );
    }

    /// Snapshot the context used by the policy and handler bridge.
    pub fn execution_containment_context(&self) -> Arc<ExecutionContainmentContext> {
        let inner = self.inner.lock();
        Arc::clone(&inner.execution_containment)
    }

    /// Snapshot current policy counters.
    pub fn policy_counters(&self) -> ToolPolicyCounters {
        let inner = self.inner.lock();
        inner.policy_counters.clone()
    }

    /// Preview the current policy decision without executing a handler.
    pub fn evaluate_policy_preview(&self, name: &str, params: &Value) -> ToolPolicyDecision {
        let inner = self.inner.lock();
        let entry = inner.tools.get(name);
        inner.policy.evaluate_with_containment(
            name,
            params,
            entry.is_some_and(|entry| requires_containment(entry.vetted_effect)),
            entry.is_some_and(|entry| entry.containment_handler.is_some()),
            &inner.execution_containment,
        )
    }

    /// Current dispatch mode, separate from the mode-independent evaluation.
    pub fn policy_mode(&self) -> ToolPolicyMode {
        let inner = self.inner.lock();
        inner.policy.mode()
    }

    /// Enable or disable session-wide raw pass-through mode.
    pub fn set_raw_mode(&self, enabled: bool) {
        let mut inner = self.inner.lock();
        inner.rtk_raw_mode = enabled;
        inner.rtk_raw_once = false;
    }

    /// Enable one-shot raw pass-through for the next tool dispatch.
    pub fn set_raw_mode_once(&self) {
        let mut inner = self.inner.lock();
        inner.rtk_raw_once = true;
    }

    /// Current raw-mode state.
    pub fn raw_mode_state(&self) -> RawModeState {
        let inner = self.inner.lock();
        RawModeState {
            enabled: inner.rtk_raw_mode,
            once_pending: inner.rtk_raw_once,
        }
    }

    /// RTK dual-log directory.
    pub fn rtk_log_dir(&self) -> PathBuf {
        let inner = self.inner.lock();
        inner.rtk_filter.log_dir().to_path_buf()
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
        self.register_inner(
            name,
            toolset,
            schema,
            handler,
            check_fn,
            env_deps,
            is_async,
            description,
            emoji,
            max_result_size_chars,
            None,
            None,
            None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn register_vetted(
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
        effect: VettedToolEffect,
    ) {
        self.register_inner(
            name,
            toolset,
            schema,
            handler,
            check_fn,
            env_deps,
            is_async,
            description,
            emoji,
            max_result_size_chars,
            Some(effect),
            None,
            None,
        );
    }
    /// Register the one canonical built-in memory handler and retain its exact
    /// backend as an opaque subject-binding factory. No caller can attach
    /// arbitrary code to this vetted registration.
    pub(crate) fn register_vetted_memory(
        &self,
        backend: Arc<crate::backends::memory::FileMemoryBackend>,
    ) {
        let hermes_home = backend.hermes_home().to_path_buf();
        let memory_backend: Arc<dyn crate::tools::memory::MemoryBackend> = backend;
        let handler: Arc<dyn ToolHandler> = Arc::new(
            crate::tools::memory::MemoryHandler::new(Arc::clone(&memory_backend)),
        );
        let schema = handler.schema();
        let description = schema.description.clone();
        self.register_inner(
            "memory",
            "memory",
            schema,
            handler,
            Arc::new(|| true),
            Vec::new(),
            true,
            description,
            "🧠",
            None,
            Some(VettedToolEffect::LocalOneShot),
            None,
            Some(CanonicalMemoryBackendBinding {
                hermes_home,
                backend: memory_backend,
            }),
        );
    }


    /// Register a built-in host-sensitive handler that can consume the exact
    /// registry-issued containment invocation token.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn register_vetted_containment(
        &self,
        name: impl Into<String>,
        toolset: impl Into<String>,
        schema: ToolSchema,
        handler: Arc<dyn ToolHandler>,
        containment_handler: Arc<dyn ContainmentSensitiveToolHandler>,
        check_fn: Arc<dyn Fn() -> bool + Send + Sync>,
        env_deps: Vec<String>,
        is_async: bool,
        description: impl Into<String>,
        emoji: impl Into<String>,
        max_result_size_chars: Option<usize>,
        effect: VettedToolEffect,
    ) {
        self.register_inner(
            name,
            toolset,
            schema,
            handler,
            check_fn,
            env_deps,
            is_async,
            description,
            emoji,
            max_result_size_chars,
            Some(effect),
            Some(containment_handler),
            None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn register_inner(
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
        vetted_effect: Option<VettedToolEffect>,
        containment_handler: Option<Arc<dyn ContainmentSensitiveToolHandler>>,
        canonical_memory_backend: Option<CanonicalMemoryBackendBinding>,
    ) {
        let name = name.into();
        let mut inner = self.inner.lock();
        if inner.tools.contains_key(&name) {
            warn!("Tool '{}' already registered; overwriting", name);
        }
        inner.tools.insert(
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
                registration_id: NEXT_REGISTRATION_ID.fetch_add(1, Ordering::Relaxed),
                vetted_effect,
                containment_handler,
                canonical_memory_backend,
            },
        );
    }

    /// Deregister a tool by name.
    ///
    /// Returns `true` if the tool was present and removed.
    pub fn deregister(&self, name: &str) -> bool {
        let mut inner = self.inner.lock();
        let Some(removed) = inner.tools.remove(name) else {
            return false;
        };
        let target_toolset = removed.toolset;
        if !inner
            .tools
            .values()
            .any(|entry| entry.toolset == target_toolset)
        {
            inner
                .toolset_aliases
                .retain(|_, target| target != &target_toolset);
        }
        true
    }

    /// Get tool definitions for all tools whose `check_fn` returns true.
    ///
    /// This is used to build the tool list sent to the LLM.
    pub fn get_definitions(&self) -> Vec<ToolSchema> {
        let inner = self.inner.lock();
        inner
            .tools
            .values()
            .filter(|entry| (entry.check_fn)())
            .map(|entry| entry.schema.clone())
            .collect()
    }

    /// Runtime definitions retain the unforgeable capability provenance used
    /// by the agent-loop bridge. Dynamic registrations always return `None`.
    pub fn runtime_definitions(&self) -> Vec<RuntimeToolDefinition> {
        let inner = self.inner.lock();
        inner
            .tools
            .values()
            .filter(|entry| (entry.check_fn)())
            .map(|entry| RuntimeToolDefinition {
                schema: entry.schema.clone(),
                registration_id: entry.registration_id,
                vetted_effect: entry.vetted_effect,
            })
            .collect()
    }
    /// Bind the exact canonical built-in memory backend to one opaque,
    /// authenticated subject key without exposing a handler substitution API.
    pub fn bind_canonical_memory_subject(
        &self,
        definition: &RuntimeToolDefinition,
        expected_hermes_home: &Path,
        subject_filter: MemorySubjectStorageKey,
    ) -> Result<BoundRuntimeToolDefinition, ToolError> {
        let backend = {
            let inner = self.inner.lock();
            let entry = inner.tools.get(&definition.schema.name).ok_or_else(|| {
                ToolError::ExecutionFailed(
                    "Canonical memory registration is unavailable".to_string(),
                )
            })?;
            if definition.schema.name != "memory"
                || entry.toolset != "memory"
                || entry.registration_id != definition.registration_id
                || entry.schema != definition.schema
                || definition.vetted_effect != Some(VettedToolEffect::LocalOneShot)
                || entry.vetted_effect != Some(VettedToolEffect::LocalOneShot)
                || !(entry.check_fn)()
            {
                return Err(ToolError::ExecutionFailed(
                    "Runtime definition is not the live canonical memory registration".to_string(),
                ));
            }
            let binding = entry.canonical_memory_backend.as_ref().ok_or_else(|| {
                ToolError::ExecutionFailed(
                    "Canonical memory backend binding is unavailable".to_string(),
                )
            })?;
            if binding.hermes_home != expected_hermes_home {
                return Err(ToolError::ExecutionFailed(
                    "Canonical memory backend home does not match the bound memory manager"
                        .to_string(),
                ));
            }
            Arc::clone(&binding.backend)
        };
        let handler = Arc::new(crate::tools::memory::MemoryHandler::with_subject_filter(
            backend,
            subject_filter,
        ));
        if handler.schema() != definition.schema {
            return Err(ToolError::ExecutionFailed(
                "Canonical memory schema changed before subject binding".to_string(),
            ));
        }
        Ok(BoundRuntimeToolDefinition {
            registry: self.clone(),
            definition: definition.clone(),
            handler,
        })
    }


    /// Dispatch a tool call by name, catching all errors.
    ///
    /// On success, returns the tool result string.
    pub fn dispatch(&self, name: &str, params: Value) -> String {
        self.dispatch_expected(name, None, None, params).into_wire()
    }

    /// Dispatch exactly the registration represented by an opaque runtime
    /// definition while preserving the pre/post invocation boundary.
    pub fn dispatch_runtime(
        &self,
        definition: &RuntimeToolDefinition,
        params: Value,
    ) -> RuntimeDispatchOutcome {
        self.dispatch_expected(
            &definition.schema.name,
            Some(definition.registration_id),
            None,
            params,
        )
    }

    fn params_reference_skill_trust_state(&self, params: &Value) -> bool {
        let inner = self.inner.lock();
        value_references_skill_trust_state(params, &inner.skill_trust_paths)
    }

    fn skill_trust_dispatch_error(name: &str) -> String {
        Self::tool_error(&format!(
            "Tool '{name}' denied: verifier-only skill trust state may only be accessed by typed core/CLI owner APIs"
        ))
    }

    fn dispatch_expected(
        &self,
        name: &str,
        expected_registration_id: Option<u64>,
        handler_override: Option<Arc<dyn ToolHandler>>,
        params: Value,
    ) -> RuntimeDispatchOutcome {
        if self.params_reference_skill_trust_state(&params) {
            warn!(
                tool = name,
                "blocked tool dispatch referencing verifier-only skill trust state"
            );
            return RuntimeDispatchOutcome::RejectedBeforeInvoke(
                Self::skill_trust_dispatch_error(name),
            );
        }
        let plan = {
            let inner = self.inner.lock();
            let entry = inner.tools.get(name);
            policy_dispatch_plan(
                &inner.policy,
                name,
                &params,
                entry.is_some_and(|entry| requires_containment(entry.vetted_effect)),
                entry.is_some_and(|entry| entry.containment_handler.is_some()),
                Arc::clone(&inner.execution_containment),
            )
        };
        if !matches!(plan.action, PolicyDispatchAction::Execute) {
            self.record_policy_decision(&plan);
            maybe_log_policy(name, &plan);
            return match plan.action {
                PolicyDispatchAction::Reject => {
                    RuntimeDispatchOutcome::RejectedBeforeInvoke(Self::tool_policy_error(name, &plan))
                }
                PolicyDispatchAction::Preview => RuntimeDispatchOutcome::RejectedBeforeInvoke(
                    policy_synthetic_preview(name, &plan),
                ),
                PolicyDispatchAction::Execute => unreachable!("handled above"),
            };
        }

        let (
            handler,
            containment_handler,
            max_chars,
            effective_params,
            raw_bypassed,
            rewrite_applied,
            rtk_filter,
        ) = {
            let mut inner = self.inner.lock();
            let raw_bypassed = inner.rtk_raw_mode || inner.rtk_raw_once;
            if inner.rtk_raw_once {
                inner.rtk_raw_once = false;
            }
            let (effective_params, rewrite_applied) =
                inner.rtk_filter.rewrite_params(name, &params, raw_bypassed);
            let entry = match inner.tools.get(name) {
                Some(entry) => entry,
                None => {
                    return RuntimeDispatchOutcome::RejectedBeforeInvoke(Self::tool_error(&format!(
                        "Tool not found: {name}"
                    )))
                }
            };
            if expected_registration_id.is_some_and(|expected| expected != entry.registration_id) {
                return RuntimeDispatchOutcome::RejectedBeforeInvoke(Self::tool_error(&format!(
                    "Tool registration changed before dispatch: {name}"
                )));
            }
            (
                handler_override.unwrap_or_else(|| Arc::clone(&entry.handler)),
                plan.containment_context
                    .as_ref()
                    .and_then(|_| entry.containment_handler.as_ref().map(Arc::clone)),
                entry
                    .max_result_size_chars
                    .unwrap_or(inner.global_max_result_size_chars),
                effective_params,
                raw_bypassed,
                rewrite_applied,
                inner.rtk_filter.clone(),
            )
        };
        if self.params_reference_skill_trust_state(&effective_params) {
            warn!(
                tool = name,
                "blocked rewritten tool dispatch referencing verifier-only skill trust state"
            );
            return RuntimeDispatchOutcome::RejectedBeforeInvoke(
                Self::skill_trust_dispatch_error(name),
            );
        }
        let containment_invocation = match (&plan.containment_context, &containment_handler) {
            (Some(context), Some(_)) => match context.prepare_host_sensitive_invocation(name) {
                Ok(invocation) => Some(invocation),
                Err(error) => {
                    self.record_policy_decision(&plan);
                    maybe_log_policy(name, &plan);
                    return RuntimeDispatchOutcome::RejectedBeforeInvoke(Self::tool_error(&format!(
                        "Certified containment dispatch could not start: {error}"
                    )));
                }
            },
            (Some(_), None) => {
                self.record_policy_decision(&plan);
                maybe_log_policy(name, &plan);
                return RuntimeDispatchOutcome::RejectedBeforeInvoke(Self::tool_error(
                    "Certified containment handler changed before dispatch",
                ));
            }
            (None, None) => None,
            (None, Some(_)) => unreachable!("manual dispatch does not select containment handler"),
        };
        self.record_policy_decision(&plan);
        maybe_log_policy(name, &plan);

        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                match (containment_handler, containment_invocation) {
                    (Some(handler), Some(invocation)) => {
                        handler
                            .execute_with_containment(effective_params.clone(), invocation)
                            .await
                    }
                    (None, None) => handler.execute(effective_params.clone()).await,
                    _ => unreachable!("containment selection was validated before invocation"),
                }
            })
        });

        let output = match result {
            Ok(output) => {
                self.disk_cleanup_auto
                    .post_tool_call_success(name, &effective_params, &output);
                let filtered = rtk_filter.filter_and_log(
                    name,
                    &effective_params,
                    &output,
                    raw_bypassed,
                    rewrite_applied,
                );
                RuntimeDispatchOutcome::Returned(Self::tool_result(&truncate_to_chars(
                    &filtered, max_chars,
                )))
            }
            Err(error) => {
                let is_unknown = matches!(
                    &error,
                    ToolError::ExecutionFailed(_)
                        | ToolError::Timeout(_)
                        | ToolError::OutcomeUnknown(_)
                );
                let err_text = error.to_string();
                let filtered = rtk_filter.filter_and_log(
                    name,
                    &effective_params,
                    &err_text,
                    raw_bypassed,
                    rewrite_applied,
                );
                let output = Self::tool_error(&truncate_to_chars(&filtered, max_chars));
                if is_unknown {
                    RuntimeDispatchOutcome::UnknownAfterInvoke(output)
                } else {
                    RuntimeDispatchOutcome::FailedAfterInvoke(output)
                }
            }
        };
        output.map_wire(|wire| maybe_annotate_audit(wire, &plan))
    }

    /// Dispatch a tool call asynchronously.
    pub async fn dispatch_async(&self, name: &str, params: Value) -> String {
        if self.params_reference_skill_trust_state(&params) {
            warn!(
                tool = name,
                "blocked async tool dispatch referencing verifier-only skill trust state"
            );
            return Self::skill_trust_dispatch_error(name);
        }
        let plan = {
            let inner = self.inner.lock();
            let entry = inner.tools.get(name);
            policy_dispatch_plan(
                &inner.policy,
                name,
                &params,
                entry.is_some_and(|entry| requires_containment(entry.vetted_effect)),
                entry.is_some_and(|entry| entry.containment_handler.is_some()),
                Arc::clone(&inner.execution_containment),
            )
        };
        if !matches!(plan.action, PolicyDispatchAction::Execute) {
            self.record_policy_decision(&plan);
            maybe_log_policy(name, &plan);
            return match plan.action {
                PolicyDispatchAction::Reject => Self::tool_policy_error(name, &plan),
                PolicyDispatchAction::Preview => policy_synthetic_preview(name, &plan),
                PolicyDispatchAction::Execute => unreachable!("handled above"),
            };
        }

        let (
            handler,
            containment_handler,
            max_chars,
            effective_params,
            raw_bypassed,
            rewrite_applied,
            rtk_filter,
        ) = {
            let mut inner = self.inner.lock();
            let raw_bypassed = inner.rtk_raw_mode || inner.rtk_raw_once;
            if inner.rtk_raw_once {
                inner.rtk_raw_once = false;
            }
            let (effective_params, rewrite_applied) =
                inner.rtk_filter.rewrite_params(name, &params, raw_bypassed);
            match inner.tools.get(name) {
                Some(entry) => (
                    Arc::clone(&entry.handler),
                    plan.containment_context
                        .as_ref()
                        .and_then(|_| entry.containment_handler.as_ref().map(Arc::clone)),
                    entry
                        .max_result_size_chars
                        .unwrap_or(inner.global_max_result_size_chars),
                    effective_params,
                    raw_bypassed,
                    rewrite_applied,
                    inner.rtk_filter.clone(),
                ),
                None => return Self::tool_error(&format!("Tool not found: {name}")),
            }
        };
        if self.params_reference_skill_trust_state(&effective_params) {
            warn!(
                tool = name,
                "blocked rewritten async tool dispatch referencing verifier-only skill trust state"
            );
            return Self::skill_trust_dispatch_error(name);
        }
        let containment_invocation = match (&plan.containment_context, &containment_handler) {
            (Some(context), Some(_)) => match context.prepare_host_sensitive_invocation(name) {
                Ok(invocation) => Some(invocation),
                Err(error) => {
                    self.record_policy_decision(&plan);
                    maybe_log_policy(name, &plan);
                    return Self::tool_error(&format!(
                        "Certified containment dispatch could not start: {error}"
                    ));
                }
            },
            (Some(_), None) => {
                self.record_policy_decision(&plan);
                maybe_log_policy(name, &plan);
                return Self::tool_error("Certified containment handler changed before dispatch");
            }
            (None, None) => None,
            (None, Some(_)) => unreachable!("manual dispatch does not select containment handler"),
        };
        self.record_policy_decision(&plan);
        maybe_log_policy(name, &plan);

        let result = match (containment_handler, containment_invocation) {
            (Some(handler), Some(invocation)) => {
                handler
                    .execute_with_containment(effective_params.clone(), invocation)
                    .await
            }
            (None, None) => handler.execute(effective_params.clone()).await,
            _ => unreachable!("containment selection was validated before invocation"),
        };
        let output = match result {
            Ok(output) => {
                self.disk_cleanup_auto
                    .post_tool_call_success(name, &effective_params, &output);
                let filtered = rtk_filter.filter_and_log(
                    name,
                    &effective_params,
                    &output,
                    raw_bypassed,
                    rewrite_applied,
                );
                Self::tool_result(&truncate_to_chars(&filtered, max_chars))
            }
            Err(error) => {
                let err_text = error.to_string();
                let filtered = rtk_filter.filter_and_log(
                    name,
                    &effective_params,
                    &err_text,
                    raw_bypassed,
                    rewrite_applied,
                );
                Self::tool_error(&truncate_to_chars(&filtered, max_chars))
            }
        };
        maybe_annotate_audit(output, &plan)
    }

    /// Get a reference to a tool entry by name.
    ///
    /// Note: This returns a cloned `Arc<ToolEntry>` because we cannot return
    /// a reference to data behind a Mutex lock. Returns `None` if not found.
    pub fn get_tool(&self, name: &str) -> Option<ToolEntryInfo> {
        let inner = self.inner.lock();
        inner.tools.get(name).map(|e| ToolEntryInfo {
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
        let inner = self.inner.lock();
        inner
            .tools
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
        let inner = self.inner.lock();
        let mut sets: Vec<String> = inner.tools.values().map(|e| e.toolset.clone()).collect();
        sets.extend(inner.toolset_aliases.keys().cloned());
        sets.sort();
        sets.dedup();
        sets
    }

    /// Register an explicit alias from a user-facing toolset token to its
    /// canonical live-registry toolset.
    pub fn register_toolset_alias(&self, alias: impl Into<String>, target: impl Into<String>) {
        let alias = alias.into().trim().to_string();
        let target = target.into().trim().to_string();
        if alias.is_empty() || target.is_empty() {
            return;
        }
        let mut inner = self.inner.lock();
        inner.toolset_aliases.insert(alias, target);
    }

    /// Return the canonical target for a registered toolset alias.
    pub fn get_toolset_alias_target(&self, alias: &str) -> Option<String> {
        let inner = self.inner.lock();
        inner.toolset_aliases.get(alias).cloned()
    }

    /// Check whether the registry owns this live toolset or alias.
    pub fn has_toolset(&self, toolset: &str) -> bool {
        let inner = self.inner.lock();
        let resolved = resolve_toolset_alias_locked(&inner, toolset);
        inner.tools.values().any(|entry| entry.toolset == resolved)
            || inner.toolset_aliases.contains_key(toolset)
    }

    /// Return tool names belonging to a live registry-owned toolset or alias.
    pub fn tool_names_for_toolset(&self, toolset: &str, available_only: bool) -> Vec<String> {
        let inner = self.inner.lock();
        let resolved = resolve_toolset_alias_locked(&inner, toolset);
        let mut names: Vec<String> = inner
            .tools
            .values()
            .filter(|entry| entry.toolset == resolved)
            .filter(|entry| !available_only || (entry.check_fn)())
            .map(|entry| entry.name.clone())
            .collect();
        names.sort();
        names
    }

    /// Check whether a tool is available (exists and check_fn passes).
    pub fn is_available(&self, name: &str) -> bool {
        let inner = self.inner.lock();
        match inner.tools.get(name) {
            Some(e) => (e.check_fn)(),
            None => false,
        }
    }

    /// Format an error as JSON: `{"error": "msg"}`.
    pub fn tool_error(msg: &str) -> String {
        serde_json::json!({ "error": msg }).to_string()
    }

    fn tool_policy_error(tool_name: &str, plan: &PolicyDispatchPlan) -> String {
        let metadata = policy_dispatch_metadata(plan);
        json!({
            "error": format!(
                "Blocked by tool policy: {}",
                plan.decision.reason.as_deref().unwrap_or("policy deny")
            ),
            "policy": {
                "tool": tool_name,
                "mode": metadata.mode,
                "decision": metadata.decision,
                "allow": metadata.allow,
                "would_block": metadata.would_block,
                "would_execute": false,
                "code": metadata.code,
                "reason": metadata.reason,
                "audit_recorded": metadata.audit_recorded,
            }
        })
        .to_string()
    }

    /// Format a tool result. If the result looks like JSON, pass it through;
    /// otherwise wrap it as `{"result": "..."}`.
    pub fn tool_result(data: &str) -> String {
        if looks_like_json(data) {
            // Assume already JSON-ish, pass through
            data.to_string()
        } else {
            json!({ "result": data }).to_string()
        }
    }

    fn record_policy_decision(&self, plan: &PolicyDispatchPlan) {
        let counters_snapshot = {
            let mut inner = self.inner.lock();
            if plan.decision.allow {
                inner.policy_counters.allow = inner.policy_counters.allow.saturating_add(1);
            } else {
                inner.policy_counters.deny = inner.policy_counters.deny.saturating_add(1);
            }
            if matches!(plan.mode, ToolPolicyMode::Audit) {
                inner.policy_counters.audit_only =
                    inner.policy_counters.audit_only.saturating_add(1);
            }
            if matches!(plan.mode, ToolPolicyMode::Simulate) {
                inner.policy_counters.simulate = inner.policy_counters.simulate.saturating_add(1);
            }
            if !plan.decision.allow {
                inner.policy_counters.would_block =
                    inner.policy_counters.would_block.saturating_add(1);
            }
            inner.policy_counters.clone()
        };
        let _ =
            persist_tool_policy_counters(&default_tool_policy_counters_path(), &counters_snapshot);
    }
}

fn value_references_skill_trust_state(value: &Value, guard: &SkillTrustPathGuard) -> bool {
    match value {
        Value::String(text) => guard.denies_text(text),
        Value::Array(values) => values
            .iter()
            .any(|value| value_references_skill_trust_state(value, guard)),
        Value::Object(values) => values
            .values()
            .any(|value| value_references_skill_trust_state(value, guard)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn policy_dispatch_plan(
    policy: &ToolPolicyEngine,
    name: &str,
    params: &Value,
    containment_required: bool,
    containment_dispatch_available: bool,
    containment_context: Arc<ExecutionContainmentContext>,
) -> PolicyDispatchPlan {
    let decision = evaluate_dispatch_policy(
        policy,
        name,
        params,
        containment_required,
        containment_dispatch_available,
        &containment_context,
    );
    let mode = policy.mode();
    let action = match mode {
        ToolPolicyMode::Simulate => PolicyDispatchAction::Preview,
        ToolPolicyMode::Audit | ToolPolicyMode::Enforce if !decision.allow => {
            PolicyDispatchAction::Reject
        }
        ToolPolicyMode::Off | ToolPolicyMode::Audit | ToolPolicyMode::Enforce => {
            PolicyDispatchAction::Execute
        }
    };
    PolicyDispatchPlan {
        mode,
        decision,
        action,
        containment_context: (containment_required
            && containment_context
                .mode()
                .mode
                .requires_containment_certificate())
        .then_some(containment_context),
    }
}

fn evaluate_dispatch_policy(
    policy: &ToolPolicyEngine,
    name: &str,
    params: &Value,
    containment_required: bool,
    containment_dispatch_available: bool,
    containment_context: &ExecutionContainmentContext,
) -> ToolPolicyDecision {
    if name == crate::tools::tool_policy_simulate::TOOL_POLICY_SIMULATE_TOOL_NAME {
        if matches!(policy.mode(), ToolPolicyMode::Simulate) {
            if let Some(target_tool) =
                crate::tools::tool_policy_simulate::dispatch_policy_target(params)
            {
                let empty_args = Value::Object(serde_json::Map::new());
                let target_args = params.get("args").unwrap_or(&empty_args);
                return policy.evaluate_with_containment(
                    target_tool,
                    target_args,
                    containment_required,
                    containment_dispatch_available,
                    containment_context,
                );
            }
        }
        let wrapper_params =
            crate::tools::tool_policy_simulate::dispatch_policy_wrapper_params(params);
        return policy.evaluate_with_containment(
            name,
            &wrapper_params,
            containment_required,
            containment_dispatch_available,
            containment_context,
        );
    }
    policy.evaluate_with_containment(
        name,
        params,
        containment_required,
        containment_dispatch_available,
        containment_context,
    )
}

fn policy_dispatch_metadata(plan: &PolicyDispatchPlan) -> ToolPolicyDispatchMetadata {
    ToolPolicyDispatchMetadata {
        mode: plan.mode.as_str(),
        decision: if plan.decision.allow { "allow" } else { "deny" },
        allow: plan.decision.allow,
        would_block: !plan.decision.allow,
        code: plan.decision.code.clone(),
        reason: plan.decision.reason.clone(),
        audit_recorded: matches!(plan.mode, ToolPolicyMode::Audit),
    }
}

fn policy_synthetic_preview(tool_name: &str, plan: &PolicyDispatchPlan) -> String {
    let preview = ToolPolicySyntheticPreview {
        status: "preview",
        preview_type: "tool_policy_dispatch/v1",
        tool: tool_name.to_string(),
        policy: policy_dispatch_metadata(plan),
        will_execute: false,
        counter_effect: "none",
    };
    serde_json::to_string(&preview).expect("synthetic policy preview must serialize")
}

fn maybe_log_policy(tool_name: &str, plan: &PolicyDispatchPlan) {
    match plan.mode {
        ToolPolicyMode::Audit => warn!(
            tool = tool_name,
            decision = if plan.decision.allow { "allow" } else { "deny" },
            reason = plan.decision.reason.as_deref().unwrap_or("none"),
            "tool policy audit recorded"
        ),
        ToolPolicyMode::Simulate => warn!(
            tool = tool_name,
            decision = if plan.decision.allow { "allow" } else { "deny" },
            reason = plan.decision.reason.as_deref().unwrap_or("none"),
            "tool policy simulation returned synthetic preview"
        ),
        ToolPolicyMode::Off | ToolPolicyMode::Enforce => {}
    }
}

fn maybe_annotate_audit(output: String, plan: &PolicyDispatchPlan) -> String {
    if !matches!(plan.mode, ToolPolicyMode::Audit) {
        return output;
    }
    let audit = json!(policy_dispatch_metadata(plan));
    if let Ok(mut value) = serde_json::from_str::<Value>(&output) {
        if let Some(object) = value.as_object_mut() {
            object.insert("_tool_policy_audit".to_string(), audit);
            return value.to_string();
        }
    }
    json!({
        "result": output,
        "_tool_policy_audit": audit,
    })
    .to_string()
}

fn resolve_toolset_alias_locked(inner: &ToolRegistryInner, name: &str) -> String {
    let mut current = name.to_string();
    let mut seen = HashSet::new();
    while seen.insert(current.clone()) {
        let Some(next) = inner.toolset_aliases.get(&current) else {
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
    use hermes_core::{
        tool_schema, AgentError, CommandOutput, ExecutionMode, ExecutionModeSource, JsonSchema,
        MemorySubject, MemorySubjectDerivationKey, MemorySurface, ResolvedExecutionMode, ToolError,
    };
    use serde_json::json;
    use std::time::Instant;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    struct CountingHandler {
        name: &'static str,
        calls: Arc<AtomicUsize>,
        marker: PathBuf,
    }

    #[async_trait]
    impl ToolHandler for CountingHandler {
        async fn execute(&self, _params: Value) -> Result<String, ToolError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::fs::write(&self.marker, "handler invoked")
                .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
            Ok(json!({"executed": true}).to_string())
        }

        fn schema(&self) -> ToolSchema {
            tool_schema(self.name, "Counting test handler", JsonSchema::new("object"))
        }
    }

    struct HostFallbackProbe {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl hermes_core::TerminalBackend for HostFallbackProbe {
        async fn execute_command(
            &self,
            _command: &str,
            _timeout: Option<u64>,
            _workdir: Option<&str>,
            _background: bool,
            _pty: bool,
        ) -> Result<CommandOutput, AgentError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(CommandOutput {
                exit_code: 0,
                stdout: "host-fallback".into(),
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
    fn test_dispatch_async_audit_denial_emits_metadata_without_execution() {
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
        assert!(parsed["error"].as_str().unwrap_or("").contains("allowlist"));
        assert_eq!(parsed["policy"]["mode"], "audit");
        assert_eq!(parsed["policy"]["decision"], "deny");
        assert_eq!(parsed["policy"]["audit_recorded"], true);
        assert_eq!(parsed["policy"]["would_execute"], false);
        assert!(parsed.get("k").is_none());
    }

    #[tokio::test]
    async fn dispatch_simulation_mode_returns_typed_preview_without_execution() {
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
        assert_eq!(parsed["status"], "preview");
        assert_eq!(parsed["preview_type"], "tool_policy_dispatch/v1");
        assert_eq!(parsed["tool"], "echo");
        assert_eq!(parsed["policy"]["mode"], "simulate");
        assert_eq!(parsed["policy"]["decision"], "deny");
        assert_eq!(parsed["policy"]["would_block"], true);
        assert_eq!(parsed["policy"]["code"], "tool_denylisted");
        assert_eq!(parsed["will_execute"], false);
        assert!(parsed.get("msg").is_none());
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
        assert_eq!(counters.allow, 0);
        assert_eq!(counters.deny, 1);
        assert_eq!(counters.would_block, 1);

        registry
            .set_policy(ToolPolicyEngine::new(ToolPolicyMode::Simulate).with_denylist(&["echo"]));
        let _ = rt.block_on(async { registry.dispatch_async("echo", json!({"k":"v"})).await });
        let counters = registry.policy_counters();
        assert_eq!(counters.allow, 0);
        assert_eq!(counters.deny, 2);
        assert_eq!(counters.simulate, 1);
        assert_eq!(counters.audit_only, 0);
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

    #[tokio::test]
    async fn denied_terminal_and_write_paths_never_invoke_handlers_in_audit_or_simulate() {
        for mode in [ToolPolicyMode::Audit, ToolPolicyMode::Simulate] {
            let temp = tempfile::tempdir().expect("tempdir");
            let terminal_target = temp.path().join("terminal-target");
            let write_target = temp.path().join("write-target");
            let terminal_marker = temp.path().join("terminal-handler-invoked");
            let write_marker = temp.path().join("write-handler-invoked");
            let terminal_calls = Arc::new(AtomicUsize::new(0));
            let write_calls = Arc::new(AtomicUsize::new(0));
            let registry = ToolRegistry::new();

            let terminal_handler = Arc::new(CountingHandler {
                name: "terminal",
                calls: terminal_calls.clone(),
                marker: terminal_marker.clone(),
            });
            registry.register(
                "terminal",
                "test",
                terminal_handler.schema(),
                terminal_handler,
                Arc::new(|| true),
                vec![],
                false,
                "Counting terminal",
                "",
                None,
            );
            let write_handler = Arc::new(CountingHandler {
                name: "write_file",
                calls: write_calls.clone(),
                marker: write_marker.clone(),
            });
            registry.register(
                "write_file",
                "test",
                write_handler.schema(),
                write_handler,
                Arc::new(|| true),
                vec![],
                false,
                "Counting write",
                "",
                None,
            );
            registry.set_policy(
                ToolPolicyEngine::new(mode).with_denylist(&["terminal", "write_file"]),
            );

            let terminal = registry
                .dispatch_async(
                    "terminal",
                    json!({"command": format!("rm -rf {}", terminal_target.display())}),
                )
                .await;
            let write = registry
                .dispatch_async(
                    "write_file",
                    json!({"path": write_target.display().to_string(), "content": "must not be written"}),
                )
                .await;

            for output in [terminal, write] {
                let parsed: Value = serde_json::from_str(&output).expect("policy JSON");
                assert_eq!(parsed["policy"]["decision"], "deny");
                if matches!(mode, ToolPolicyMode::Simulate) {
                    assert_eq!(parsed["status"], "preview");
                    assert_eq!(parsed["will_execute"], false);
                } else {
                    assert_eq!(parsed["policy"]["would_execute"], false);
                    assert!(parsed["error"]
                        .as_str()
                        .unwrap_or("")
                        .contains("Blocked by tool policy"));
                    assert_eq!(parsed["policy"]["audit_recorded"], true);
                }
            }
            assert_eq!(terminal_calls.load(Ordering::SeqCst), 0);
            assert_eq!(write_calls.load(Ordering::SeqCst), 0);
            assert!(!terminal_marker.exists());
            assert!(!write_marker.exists());
            assert!(!terminal_target.exists());
            assert!(!write_target.exists());
        }
    }

    #[tokio::test]
    async fn certified_terminal_simulation_uses_shared_context_and_skips_host_handler() {
        use hermes_environments::docker::DockerBackend;

        let backend = Arc::new(DockerBackend::new(
            None,
            Some(format!("registry.example/hermes@sha256:{}", "b".repeat(64))),
            false,
            false,
            None,
            None,
            None,
            false,
            None,
            Vec::new(),
            Vec::new(),
            30,
            1024,
        ));
        let context = Arc::new(
            backend
                .closed_execution_context(ResolvedExecutionMode {
                    mode: ExecutionMode::Reviewed,
                    source: ExecutionModeSource::Cli,
                })
                .expect("exact hardened Docker context"),
        );
        let registry = ToolRegistry::new();
        registry.set_execution_containment_context(context.clone());
        let host_calls = Arc::new(AtomicUsize::new(0));
        let terminal = Arc::new(crate::tools::terminal::TerminalHandler::with_execution_context(
            Arc::new(HostFallbackProbe {
                calls: host_calls.clone(),
            }),
            context.clone(),
        ));
        assert!(Arc::ptr_eq(
            &context,
            &registry.execution_containment_context()
        ));
        assert!(Arc::ptr_eq(
            &context,
            &terminal.execution_containment_context()
        ));
        let schema = terminal.schema();
        let name = schema.name.clone();
        let description = schema.description.clone();
        registry.register_vetted_containment(
            name,
            "terminal",
            schema,
            terminal.clone(),
            terminal,
            Arc::new(|| true),
            Vec::new(),
            true,
            description,
            "test",
            None,
            VettedToolEffect::LocalOneShot,
        );
        let params = json!({"command": "printf contained"});
        let enforced_decision = registry.evaluate_policy_preview("terminal", &params);
        assert!(enforced_decision.allow);

        registry.set_policy(ToolPolicyEngine::new(ToolPolicyMode::Simulate));
        let simulated: Value = serde_json::from_str(
            &registry.dispatch_async("terminal", params).await,
        )
        .expect("synthetic simulation result");
        assert_eq!(simulated["status"], "preview");
        assert_eq!(simulated["will_execute"], false);
        assert_eq!(simulated["policy"]["allow"], enforced_decision.allow);
        assert_eq!(host_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn mismatched_registry_and_terminal_contexts_reject_before_host_fallback() {
        use hermes_environments::docker::DockerBackend;

        let build_context = || {
            Arc::new(
                Arc::new(DockerBackend::new(
                    None,
                    Some(format!("registry.example/hermes@sha256:{}", "d".repeat(64))),
                    false,
                    false,
                    None,
                    None,
                    None,
                    false,
                    None,
                    Vec::new(),
                    Vec::new(),
                    30,
                    1024,
                ))
                .closed_execution_context(ResolvedExecutionMode {
                    mode: ExecutionMode::Reviewed,
                    source: ExecutionModeSource::Cli,
                })
                .expect("certified Docker context"),
            )
        };
        let registry_context = build_context();
        let handler_context = build_context();
        assert!(!Arc::ptr_eq(&registry_context, &handler_context));

        let registry = ToolRegistry::new();
        registry.set_execution_containment_context(registry_context);
        registry.set_policy(ToolPolicyEngine::new(ToolPolicyMode::Enforce));
        let host_calls = Arc::new(AtomicUsize::new(0));
        let terminal = Arc::new(crate::tools::terminal::TerminalHandler::with_execution_context(
            Arc::new(HostFallbackProbe {
                calls: host_calls.clone(),
            }),
            handler_context,
        ));
        let schema = terminal.schema();
        let name = schema.name.clone();
        let description = schema.description.clone();
        registry.register_vetted_containment(
            name,
            "terminal",
            schema,
            terminal.clone(),
            terminal,
            Arc::new(|| true),
            Vec::new(),
            true,
            description,
            "test",
            None,
            VettedToolEffect::LocalOneShot,
        );
        let output = registry
            .dispatch_async("terminal", json!({"command": "printf contained"}))
            .await;
        assert!(output.contains("containment invocation does not belong"));
        assert_eq!(host_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn closed_raw_off_dispatch_blocks_entire_skill_root_and_authority_paths_before_effects() {
        use hermes_environments::docker::DockerBackend;

        let temp = tempfile::tempdir().expect("tempdir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))
                .expect("restrict trust fixture parent");
        }
        let skills_root = temp.path().join("skills");
        let hub = skills_root.join(".hub");
        let package_payload = skills_root
            .join(".packages")
            .join("local")
            .join("demo")
            .join("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .join("SKILL.md");
        let ordinary_skill = skills_root.join("candidate").join("SKILL.md");
        std::fs::create_dir_all(&hub).expect("hub");
        std::fs::create_dir_all(package_payload.parent().expect("payload parent"))
            .expect("package payload parent");
        std::fs::create_dir_all(ordinary_skill.parent().expect("ordinary parent"))
            .expect("ordinary parent");
        std::fs::write(hub.join("lock.json"), "{}").expect("lock");
        std::fs::write(&package_payload, "verified body").expect("package payload");
        std::fs::write(&ordinary_skill, "candidate body").expect("ordinary skill");
        let remote_root = temp.path().join("remote-trust-root.json");
        let remote_audit = temp.path().join("remote-keyring-audit.json");
        let local_owner_keyring = temp.path().join("local-owner-keyring.json");
        for authority_path in [&remote_root, &remote_audit, &local_owner_keyring] {
            std::fs::write(authority_path, "owner-only authority state")
                .expect("authority state");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for authority_path in [&remote_root, &remote_audit, &local_owner_keyring] {
                std::fs::set_permissions(authority_path, std::fs::Permissions::from_mode(0o600))
                    .expect("restrict authority file");
            }
        }
        let authority = hermes_skills::SkillTrustAuthorityPaths::new(
            skills_root.clone(),
            remote_root.clone(),
            remote_audit.clone(),
            local_owner_keyring.clone(),
        )
        .expect("validated typed skill trust authority");

        #[cfg(unix)]
        let terminal_target = {
            use std::os::unix::fs::symlink;
            let link = temp.path().join("skills-link");
            symlink(&skills_root, &link).expect("skills symlink");
            link.join("candidate").join("SKILL.md")
        };
        #[cfg(not(unix))]
        let terminal_target = skills_root.join("scratch").join("..").join("candidate").join("SKILL.md");
        let file_target = skills_root
            .join("scratch")
            .join("..")
            .join("candidate")
            .join("SKILL.md");

        let backend = Arc::new(DockerBackend::new(
            None,
            Some(format!("registry.example/hermes@sha256:{}", "c".repeat(64))),
            false,
            false,
            None,
            None,
            None,
            false,
            None,
            Vec::new(),
            Vec::new(),
            30,
            1024,
        ));
        let closed_context = Arc::new(
            backend
                .closed_execution_context(ResolvedExecutionMode {
                    mode: ExecutionMode::Reviewed,
                    source: ExecutionModeSource::Cli,
                })
                .expect("closed execution context"),
        );

        let registry = ToolRegistry::new();
        registry.set_execution_containment_context(closed_context);
        registry.bind_skill_trust_authority_paths(&authority);
        registry.set_policy(ToolPolicyEngine::new(ToolPolicyMode::Off));
        registry.set_raw_mode(true);

        let terminal_calls = Arc::new(AtomicUsize::new(0));
        let code_calls = Arc::new(AtomicUsize::new(0));
        let plugin_calls = Arc::new(AtomicUsize::new(0));
        let file_calls = Arc::new(AtomicUsize::new(0));
        for (name, calls) in [
            ("terminal", terminal_calls.clone()),
            ("execute_code", code_calls.clone()),
            ("plugin-write", plugin_calls.clone()),
            ("write_file", file_calls.clone()),
        ] {
            let handler = Arc::new(CountingHandler {
                name,
                calls,
                marker: temp.path().join(format!("{name}-invoked")),
            });
            registry.register(
                name,
                "test",
                handler.schema(),
                handler,
                Arc::new(|| true),
                vec![],
                false,
                "Counting effect",
                "",
                None,
            );
        }

        let terminal = registry
            .dispatch_async(
                "terminal",
                json!({"command": format!("printf x > {}", terminal_target.display())}),
            )
            .await;
        let code = registry
            .dispatch_async(
                "execute_code",
                json!({"code": format!("fs.writeFileSync('{}', 'x')", remote_root.display())}),
            )
            .await;
        let plugin = registry
            .dispatch_async(
                "plugin-write",
                json!({"path": package_payload.display().to_string()}),
            )
            .await;
        let file = registry
            .dispatch_async(
                "write_file",
                json!({"path": file_target.display().to_string(), "content": "mutate"}),
            )
            .await;
        let audit = registry
            .dispatch_async(
                "plugin-write",
                json!({"path": remote_audit.display().to_string()}),
            )
            .await;
        let local_owner = registry
            .dispatch_async(
                "plugin-write",
                json!({"path": local_owner_keyring.display().to_string()}),
            )
            .await;

        for output in [terminal, code, plugin, file, audit, local_owner] {
            assert!(
                output.contains("verifier-only skill trust state"),
                "unexpected output: {output}"
            );
        }
        assert_eq!(terminal_calls.load(Ordering::SeqCst), 0);
        assert_eq!(code_calls.load(Ordering::SeqCst), 0);
        assert_eq!(plugin_calls.load(Ordering::SeqCst), 0);
        assert_eq!(file_calls.load(Ordering::SeqCst), 0);
    }

    fn memory_filter(label: &str, key_byte: u8) -> MemorySubjectStorageKey {
        MemorySubject::local_owner(MemorySurface::Local, label)
            .expect("memory subject")
            .storage_key(&MemorySubjectDerivationKey::from_bytes([key_byte; 32]))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn canonical_memory_binding_uses_exact_subject_and_stales_on_overwrite() {
        let home = tempfile::tempdir().expect("memory home");
        let registry = ToolRegistry::new();
        registry.set_policy(ToolPolicyEngine::new(ToolPolicyMode::Off));
        registry.register_vetted_memory(Arc::new(
            crate::backends::memory::FileMemoryBackend::with_home(home.path().to_path_buf()),
        ));
        let definition = registry
            .runtime_definitions()
            .into_iter()
            .find(|definition| definition.schema().name == "memory")
            .expect("canonical memory definition");
        let alice = memory_filter("alice", 61);
        let bob = memory_filter("bob", 62);
        let bound = registry
            .bind_canonical_memory_subject(&definition, home.path(), alice.clone())
            .expect("subject-bound canonical memory");

        let outcome = bound.dispatch(json!({
            "action": "add",
            "target": "memory",
            "content": "exact alice partition"
        }));
        assert!(matches!(outcome, RuntimeDispatchOutcome::Returned(_)));
        let alice_file = home
            .path()
            .join("memories/subjects")
            .join(alice.as_str())
            .join("MEMORY.md");
        let bob_file = home
            .path()
            .join("memories/subjects")
            .join(bob.as_str())
            .join("MEMORY.md");
        assert!(std::fs::read_to_string(alice_file)
            .expect("alice memory")
            .contains("exact alice partition"));
        assert!(!bob_file.exists());

        let replacement_calls = Arc::new(AtomicUsize::new(0));
        let replacement_marker = home.path().join("replacement-invoked");
        let replacement = Arc::new(CountingHandler {
            name: "memory",
            calls: Arc::clone(&replacement_calls),
            marker: replacement_marker.clone(),
        });
        registry.register(
            "memory",
            "dynamic",
            replacement.schema(),
            replacement,
            Arc::new(|| true),
            vec![],
            true,
            "untrusted replacement",
            "",
            None,
        );
        assert!(matches!(
            bound.dispatch(json!({
                "action": "add",
                "target": "memory",
                "content": "must not execute"
            })),
            RuntimeDispatchOutcome::RejectedBeforeInvoke(_)
        ));
        assert_eq!(replacement_calls.load(Ordering::SeqCst), 0);
        assert!(!replacement_marker.exists());
    }

    #[test]
    fn dynamic_memory_registration_cannot_mint_a_canonical_subject_binding() {
        let home = tempfile::tempdir().expect("memory home");
        let registry = ToolRegistry::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingHandler {
            name: "memory",
            calls,
            marker: home.path().join("dynamic-memory-invoked"),
        });
        registry.register(
            "memory",
            "memory",
            handler.schema(),
            handler,
            Arc::new(|| true),
            vec![],
            true,
            "dynamic memory",
            "",
            None,
        );
        let definition = registry
            .runtime_definitions()
            .into_iter()
            .find(|definition| definition.schema().name == "memory")
            .expect("dynamic runtime definition");
        let error = registry
            .bind_canonical_memory_subject(
                &definition,
                home.path(),
                memory_filter("dynamic", 63),
            )
            .expect_err("dynamic registration must not bind as canonical memory");
        assert!(error
            .to_string()
            .contains("not the live canonical memory registration"));
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
