/// The main agent loop.
///
/// Owns the configuration, a tool registry, and an LLM provider.
/// Call `run()` or `run_stream()` to begin an autonomous loop.
pub struct AgentLoop {
    pub config: AgentConfig,
    pub tool_registry: Arc<ToolRegistry>,
    pub llm_provider: Arc<dyn LlmProvider>,
    pub interrupt: InterruptController,
    /// Optional memory manager for prefetch/sync/tool routing.
    pub memory_manager: Option<Arc<std::sync::Mutex<MemoryManager>>>,
    /// Immutable subject cloned from the manager at attachment time. This is
    /// intentionally separate from AgentConfig/session metadata.
    memory_subject: Option<MemorySubject>,
    /// Opaque storage/filter key paired with `memory_subject`.
    memory_subject_filter_key: Option<MemorySubjectStorageKey>,
    /// Optional plugin manager for lifecycle hooks.
    pub plugin_manager: Option<Arc<std::sync::Mutex<PluginManager>>>,
    /// Callbacks for progress reporting.
    pub callbacks: Arc<AgentCallbacks>,
    /// Sub-agent delegation depth (0 = root).
    pub delegate_depth: u32,
    /// Primary LLM credential pool (Python `primary["credential_pool"]` / runtime pool).
    pub primary_credential_pool: Option<Arc<CredentialPool>>,
    /// Memory/skill nudge counters (persist for the lifetime of this `AgentLoop`).
    pub evolution_counters: Arc<Mutex<EvolutionCounters>>,
    /// Backoff window for oauth refresh failures (avoid hammering token endpoints every turn).
    oauth_refresh_backoff: Arc<Mutex<HashMap<String, Instant>>>,
    /// Optional in-process sub-agent orchestrator. When set, `delegate_task`
    /// tool calls are executed by the orchestrator (spawn/timeout/cancel/
    /// lineage) instead of simply returning a signal envelope.
    sub_agent_orchestrator: Option<Arc<crate::sub_agent_orchestrator::SubAgentOrchestrator>>,
    /// Reusable production run-record services. Each invocation creates its own
    /// fenced `ActiveRun`; no owner lease is stored on the shared loop.
    pub(crate) run_runtime: Option<Arc<RunRuntime>>,
    /// Exact containment context admitted for this loop. Manual loops receive
    /// an explicit host-local context; closed loops require one from a
    /// certified executable backend.
    execution_containment: Option<Arc<ExecutionContainmentContext>>,
    /// Immutable ingress trigger metadata for the next invocation.
    run_context: RunInvocationContext,
    /// Parent lineage for a delegated child invocation.
    parent_run_id: Option<RunId>,
    /// Always-on workspace code index + repo-map source.
    code_index: Option<Arc<Mutex<CodeIndex>>>,
    /// LSP-style context injection controls.
    lsp_context: LspContextConfig,
    /// Process-local per-route performance state for smart-routing adaptation.
    route_learning: Arc<Mutex<HashMap<String, RouteLearningStats>>>,
}

#[derive(Debug, Clone)]
struct TurnRuntimeRoute {
    model: String,
    provider: Option<String>,
    base_url: Option<String>,
    api_key_env: Option<String>,
    api_mode: Option<ApiMode>,
    command: Option<String>,
    args: Vec<String>,
    credential_pool: Option<Arc<CredentialPool>>,
    /// When true (default), merge with [`AgentLoop::primary_credential_pool`] if route pool is unset.
    credential_pool_fallback: bool,
    route_label: Option<String>,
    routing_reason: Option<String>,
    signature: TurnRouteSignature,
}

