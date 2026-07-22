//! # hermes-eval
//!
//! Agent evaluation harness for Hermes Agent. Provides a benchmark-agnostic
//! framework for running agent rollouts against task datasets, verifying
//! outputs, and aggregating results.
//!
//! ## Architecture
//!
//! The eval harness separates three concerns:
//!
//! 1. **BenchmarkAdapter**: Per-benchmark logic (dataset loading, task setup,
//!    verification). Implementations for Terminal-Bench 2.0, SWE-bench, etc.
//!    live behind this trait.
//! 2. **Runner**: Executes an adapter's tasks in parallel with
//!    concurrency control, collects per-task results, and produces a
//!    reproducible run record.
//! 3. **Reporter**: Serializes results to JSON (see [`reporter::JsonReporter`]);
//!    Parquet / baseline diff can layer on top.
//!
//! ## Wiring a real agent
//!
//! - **Smoke / CI**: [`tblite::SmokeRollout`](crate::tblite::SmokeRollout).
//! - **Real agent** (feature `agent-loop`): [`AgentLoopRollout`](crate::AgentLoopRollout) wraps
//!   [`hermes_agent::AgentLoop`]; build the loop like `hermes-cli`, then `Arc` + [`Runner::run`](runner::Runner::run).
//!
//! ## Adapters
//!
//! - [`tblite`] — OpenThoughts TBLite **smoke** subset (no HF / Docker); CI wiring only.
//!
//! ## Planned (larger scope)
//!
//! - `terminal_bench_2` — NousResearch/terminal-bench-2, Docker-per-task
//! - `swe_bench_verified` — SWE-bench Verified (v1.1)
//! - `web_research` — Web research benchmark (v1.1)
//!
//! All adapters share the same `Runner` + `Reporter` infrastructure.

pub mod adapter;
#[cfg(feature = "agent-loop")]
pub mod agent_rollout;
pub mod configured;
pub mod error;
pub mod eval_ledger;
pub mod promotion_gate;
pub mod reporter;
pub mod release_candidate;
pub mod release_operations;
pub mod result;
pub mod runner;
pub mod tblite;
pub mod verifier;

pub use adapter::{BenchmarkAdapter, BenchmarkMetadata, TaskSpec};
#[cfg(feature = "agent-loop")]
pub use agent_rollout::AgentLoopRollout;
pub use configured::{ConfiguredBenchmarkAdapter, ConfiguredTask, ConfiguredVerifier};
pub use error::{EvalError, EvalResult};
pub use eval_ledger::{
    BaselineEvidence, EvaluationEvidence, EvaluationLedger, EvaluationReceipt,
    EvaluationReceiptError, EvaluationReceiptInputs, EvaluationReceiptResult, EvaluatorVersion,
    EvidenceCount, EvidenceDigest, FixedHoldoutEvidence, LegacyEvaluationDiagnostic,
    MetricEvidence, PromotionAuthority, ReplayFidelity, ReplayOperationCounts,
    ReproductionArtifactEvidence, RunExportAuthenticationStatus, RunExportIntegrityStatus,
    SignatureVerificationEvidence, TerminalValidity, EVALUATION_RECEIPT_DIGEST_ALGORITHM,
    EVALUATION_RECEIPT_SCHEMA, EVALUATION_RECEIPT_SCHEMA_VERSION,
};
#[cfg(feature = "agent-loop")]
pub use eval_ledger::ExactEvaluationReceiptEvidence;
pub use promotion_gate::{
    evaluate_promotion, ExactReplayEvidenceItem, PromotionDecision, PromotionGate,
    PromotionGateFailure, PromotionGateOutcome, PromotionGateResult, PromotionMetric,
    PromotionPolicyRequirement, PromotionSupplementaryEvidence, PromotionVerdict, ReleaseBinding,
    MAXIMUM_ROLLBACK_DRILL_MILLIS_EXCLUSIVE, MINIMUM_SOAK_MILLIS,
};
pub use release_candidate::{
    release_candidate_ledger_schema_fingerprint, sign_release_candidate_receipt,
    AuthenticatedSubject, ExactReplayEvidence, ReleaseCandidateReceipt,
    ReleaseCandidateReceiptAppendResult, ReleaseCandidateReceiptEnvelope,
    ReleaseCandidateReceiptError, ReleaseCandidateReceiptInputs, ReleaseCandidateReceiptLedger,
    ReleaseCandidateReceiptResult, ReleaseCandidateReceiptTrustPolicy, ReleaseDigest,
    ReleaseHoldoutEvidence, ReleaseMetrics, ReleaseOperationLineage, ReleasePolicyId,
    ReleasePromotionPolicy, ReleaseReceiptId, ReleaseReceiptScope, ReleaseReproductionEvidence,
    ReleaseRevisionAlgorithm, ReleaseSecurityEvidence, ReleaseSigningKeyId, ReleaseSource,
    ReleaseTenantId, ReleaseTrustDecision, VerifiedReleaseCandidateReceipt,
    RELEASE_CANDIDATE_DIGEST_ALGORITHM, RELEASE_CANDIDATE_LEDGER_SCHEMA_VERSION,
    RELEASE_CANDIDATE_RECEIPT_ENVELOPE_SCHEMA, RELEASE_CANDIDATE_RECEIPT_ENVELOPE_SCHEMA_VERSION,
    RELEASE_CANDIDATE_RECEIPT_SCHEMA, RELEASE_CANDIDATE_RECEIPT_SCHEMA_VERSION,
    RELEASE_CANDIDATE_SIGNATURE_DOMAIN,
};
pub use release_operations::{VerifiedRollbackDrillReceipt, VerifiedSoakReceipt};
#[cfg(feature = "agent-loop")]
pub use release_candidate::{
    exact_replay_evidence_from_verified_p3, verify_release_candidate_receipt,
};
pub use reporter::{JsonReporter, Reporter};
pub use result::{AggregateMetrics, RunRecord, TaskResult, TaskStatus};
pub use runner::{Runner, RunnerConfig, TaskRollout};
pub use tblite::{
    tblite_smoke_tasks, NoopRollout, SmokePassVerifier, SmokeRollout, TbliteSmokeAdapter,
};
pub use verifier::{VerificationOutcome, Verifier};
