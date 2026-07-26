//! # hermes-agent
//!
//! Core agent loop engine — orchestrates LLM calls, tool execution, and
//! context management into a fully autonomous loop that runs until the
//! model finishes naturally or the turn budget is exhausted.

pub mod agent_loop;
mod alpha_runtime_v1;
pub mod api_bridge;
pub mod artifact_lifecycle;
pub mod auxiliary_builder;
pub mod bedrock;
pub(crate) mod branch_replay_lifecycle;
pub mod budget;
pub mod code_index;
pub mod coding_context;
pub mod compression;
pub mod context;
pub mod context_files;
pub mod context_references;
pub mod credential_pool;
pub mod deterministic_replay;
pub mod exact_replay_lifecycle;
pub mod fallback;
pub mod honcho_provider;
pub mod interrupt;
pub mod local_backends;
pub mod lsp_context;
pub mod memory_manager;
pub mod memory_plugins;
pub mod model_normalize;
pub mod oauth;
pub mod oneshot;
pub mod plugins;
pub mod provider;
pub mod provider_profiles;
pub mod providers_extra;
pub mod python_alignment;
pub mod rate_limit;
pub mod reasoning;
pub mod run_export;
pub mod run_ledger;
mod run_proof;
pub mod run_runtime;
mod runtime_cwd;
pub mod sealed_artifact;
pub mod session_persistence;
pub mod skill_orchestrator;
pub mod smart_model_routing;
pub mod steer;
pub mod sub_agent_orchestrator;
pub mod subdirectory_hints;
pub mod tool_call_args;

// Re-export primary agent types
pub use agent_loop::{
    AgentCallbacks, AgentConfig, AgentLoop, AgentObserverEvent, AgentObserverReceiver,
    AgentObserverSink, AgentRenderDelta, ApiMode, CheapModelRouteConfig, ErrorClass, RetryConfig,
    SmartModelRoutingConfig, TurnMetrics,
};

// Re-export context management
pub use compression::summarize_messages_with_llm;
pub use context::{
    builtin_personality_descriptions, builtin_personality_names, ensure_default_soul_md,
    is_legacy_template_soul, load_context_files, load_soul_md, load_soul_md_from,
    load_soul_md_from_home, switch_personality, ContextManager, SoulSeedOutcome,
    SystemPromptBuilder,
};

// Re-export budget enforcement
pub use budget::{check_aggregate_budget, enforce_budget, truncate_result};

// Re-export LLM providers
pub use api_bridge::CodexProvider;
pub use auxiliary_builder::{
    build_auxiliary_client_with_main_runtime, build_default_auxiliary_client, AuxiliaryMainRuntime,
    AuxiliaryWiringSummary,
};
pub use bedrock::BedrockProvider;
pub use hermes_intelligence::auxiliary::AuxiliaryConfig;
pub use oneshot::{
    render_template as render_oneshot_template, run_oneshot_with_client, strip_wrapping_code_fence,
    truncate_template_text, OneShotError, OneShotRequest,
};
pub use provider::{AnthropicProvider, GenericProvider, OpenAiProvider, OpenRouterProvider};
pub use providers_extra::{
    CopilotProvider, KimiProvider, MiniMaxProvider, NousProvider, QwenProvider,
};

// Re-export rate limiting, credential pool, and fallback chain
pub use credential_pool::CredentialPool;
pub use fallback::FallbackChain;
pub use oauth::{OAuthManager, OAuthToken, TokenFetcher};
pub use rate_limit::RateLimitTracker;

// Re-export reasoning parser
pub use reasoning::parse_reasoning;

// Re-export interrupt controller
pub use interrupt::InterruptController;
pub use steer::{
    format_steer_marker, is_formatted_steer_marker, STEER_CHANNEL_NOTE, STEER_MARKER_CLOSE,
    STEER_MARKER_OPEN,
};

// Re-export subject-bound memory manager APIs.
pub use memory_manager::{
    build_memory_context_block, sanitize_context, MemoryExportProviderReceipt, MemoryExportReport,
    MemoryForgetProviderReceipt, MemoryForgetReport, MemoryManager, MemoryManagerError,
    MemoryProviderPlugin, MemoryRetentionProviderReceipt, MemoryRetentionReport,
    StreamingContextScrubber,
};

// Re-export plugin diagnostic discovery.
pub use plugins::PluginManager;

// Re-export skill orchestrator
pub use skill_orchestrator::SkillOrchestrator;

// Re-export session persistence
pub use alpha_runtime_v1::{
    AlphaRuntimeV1BackgroundJob, AlphaRuntimeV1DispatchSummary, AlphaRuntimeV1EnqueueReceipt,
    AlphaRuntimeV1Envelope, AlphaRuntimeV1Error, AlphaRuntimeV1EventType, AlphaRuntimeV1LoopId,
    AlphaRuntimeV1Request, AlphaRuntimeV1Variant, ALPHA_RUNTIME_V1_ENVELOPE_SCHEMA,
};
pub use run_runtime::{
    CronScheduleSource, EphemeralRunRuntimeFactory, RunInspection, RunInvocationContext,
    RunRuntime, SessionRunSelection,
};
pub use session_persistence::{leading_system_prompt_for_persist, SessionPersistence};

// Re-export deterministic, provider/tool-free replay evidence.
pub use deterministic_replay::{
    reconstruct_exact_verified_run, reconstruct_verified_run, BoundExactArtifactReader,
    DeterministicReplayError, DeterministicReplayReport, ExactArtifactReadError,
    ExactArtifactReader, ReplayArtifactAvailability, ReplayArtifactReference, ReplayEvidenceCount,
    ReplayEvidenceCounters, ReplayFidelity, VerifiedRunReplayPlan,
};

// Re-export scoped detached-signature verification for replay plans.
pub use run_proof::{
    sign_verified_run_proof, verify_exact_run_evidence, verify_trust_policy_run_proof,
    ExactVerifiedRunEvidence, ExactVerifiedRunEvidenceError, ExactVerifiedRunEvidenceResult,
    RunProofEnvelope, RunProofError, RunProofResult, RunProofTrustContext, RunProofTrustDecision,
    RunProofTrustPolicy, TrustPolicyVerifiedRunProof, RUN_PROOF_SCHEMA, RUN_PROOF_SCHEMA_VERSION,
    RUN_PROOF_SIGNATURE_ALGORITHM,
};

// Re-export invocation-local workspace scoping for scheduled runners.
pub use runtime_cwd::with_workspace_cwd;

// Re-export context files
pub use code_index::{CodeIndex, CodeIndexConfig, IndexStats, ReferenceHit, SymbolInfo};
pub use context_files::{load_hermes_context_files, load_workspace_context, scan_context_content};
pub use context_references::{
    parse_context_references, preprocess_context_references_async, ContextReference,
    ContextReferenceResult,
};
pub use lsp_context::{build_lsp_context_note, LspContextConfig};

// Re-export subdirectory hints
pub use subdirectory_hints::{generate_project_hints, SubdirectoryHintTracker};

// Python `run_agent.py` alignment helpers (budget strip/inject, surrogate sanitize)
pub use python_alignment::{
    budget_pressure_text, inject_budget_pressure_into_last_tool_result,
    looks_like_codex_intermediate_ack, sanitize_surrogates, strip_budget_warnings_from_messages,
    strip_think_blocks_for_ack, CODEX_CONTINUE_USER_MESSAGE,
};

// Re-export sub-agent orchestrator
pub use sub_agent_orchestrator::{
    SubAgentLineage, SubAgentOrchestrator, SubAgentOrchestratorConfig, SubAgentRequest,
    SubAgentStatus,
};

// Re-export honcho provider
pub use honcho_provider::HonchoProvider;

// Re-export core types that consumers need.
pub use hermes_core::{
    AgentError, AgentResult, BudgetConfig, LlmProvider, MemoryAuditActor, MemoryExportArtifact,
    MemoryExportReceipt, MemoryForgetReceipt, MemoryGovernanceOperation, MemoryPrincipalId,
    MemoryProviderGovernanceError, MemoryReadbackReceipt, MemoryRetentionPolicy,
    MemoryRetentionReceipt, MemorySessionId, MemorySubject, MemorySubjectDerivationKey,
    MemorySubjectError, MemorySubjectStorageKey, MemorySurface, MemoryTeamId, MemoryTenantId,
    MemoryVisibility, Message, StreamChunk, ToolCall, ToolError, ToolResult, ToolSchema,
    UsageStats,
};

fn default_memory_home() -> String {
    std::env::var("HERMES_HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var("HERMES_AGENT_ULTRA_HOME")
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|home| format!("{home}/.hermes-agent-ultra"))
        })
        .unwrap_or_else(|| ".hermes-agent-ultra".to_string())
}

/// Attach discovered providers to an `AgentLoop` under an explicit immutable
/// memory subject. The subject must come from authenticated ingress or an
/// explicit local-owner constructor; AgentConfig is never used as identity.
pub fn attach_discovered_memory(
    mut agent: AgentLoop,
    subject: MemorySubject,
) -> Result<AgentLoop, MemoryManagerError> {
    if agent.config.skip_memory {
        return Ok(agent);
    }
    let hermes_home = agent
        .config
        .hermes_home
        .clone()
        .unwrap_or_else(default_memory_home);
    if let Some(manager) = memory_plugins::build_initialized_memory_manager(
        subject,
        &hermes_home,
        agent.config.memory_nudge_interval,
    )? {
        agent = agent.with_memory(manager);
    }
    Ok(agent)
}
