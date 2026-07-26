//! Typed containment certificates and execution receipts for closed modes.
//!
//! A configuration label is not a certificate, and a certificate is not an
//! execution receipt.  This module keeps those boundaries explicit: only the
//! hardened Docker backend may issue a capability and bind it to a command
//! plan; each command then needs a single-use context-bound invocation token.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use hermes_core::{CommandOutput, ExecutionMode, ResolvedExecutionMode};
use parking_lot::Mutex;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Version of the least-privilege Docker launch policy encoded by a
/// [`VerifiedIsolatedBackendCapability`].
pub const CLOSED_EXECUTION_POLICY_VERSION: u16 = 1;

static NEXT_CONTEXT_NONCE: AtomicU64 = AtomicU64::new(1);

/// Backend family named by a verified containment certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifiedIsolatedBackendKind {
    Docker,
}

impl VerifiedIsolatedBackendKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Docker => "docker",
        }
    }
}

/// Authority covered by a verified containment certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifiedIsolatedBackendScope {
    /// The backend may run host-sensitive tool effects and authoritative closed
    /// runs, subject to the matching execution-mode policy.
    ClosedExecution,
}

impl VerifiedIsolatedBackendScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClosedExecution => "closed_execution",
        }
    }
}

/// Safe, serializable containment evidence.
///
/// This projection deliberately contains only immutable backend metadata and a
/// SHA-256 digest. Docker environment values are never certifiable, so no
/// secrets can enter this evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifiedIsolatedBackendEvidence {
    pub backend_kind: VerifiedIsolatedBackendKind,
    pub configuration_digest: String,
    pub scope: VerifiedIsolatedBackendScope,
    pub policy_version: u16,
}

/// Opaque proof of an exact hardened execution backend.
///
/// There is intentionally no public constructor and no deserialization path.
/// Only this crate's hardened Docker backend can issue it after canonical
/// least-privilege validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedIsolatedBackendCapability {
    backend_kind: VerifiedIsolatedBackendKind,
    configuration_digest: String,
    scope: VerifiedIsolatedBackendScope,
    policy_version: u16,
}

impl VerifiedIsolatedBackendCapability {
    pub const fn backend_kind(&self) -> VerifiedIsolatedBackendKind {
        self.backend_kind
    }

    /// SHA-256 of the canonical, non-secret hardened launch configuration.
    pub fn configuration_digest(&self) -> &str {
        &self.configuration_digest
    }

    pub const fn scope(&self) -> VerifiedIsolatedBackendScope {
        self.scope
    }

    pub const fn policy_version(&self) -> u16 {
        self.policy_version
    }

    pub fn evidence(&self) -> VerifiedIsolatedBackendEvidence {
        VerifiedIsolatedBackendEvidence {
            backend_kind: self.backend_kind,
            configuration_digest: self.configuration_digest.clone(),
            scope: self.scope,
            policy_version: self.policy_version,
        }
    }

    fn is_closed_execution_certificate(&self) -> bool {
        self.backend_kind == VerifiedIsolatedBackendKind::Docker
            && self.scope == VerifiedIsolatedBackendScope::ClosedExecution
            && self.policy_version == CLOSED_EXECUTION_POLICY_VERSION
            && self.configuration_digest.len() == 64
            && self
                .configuration_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
    }

    /// Crate-private authority used exclusively by the hardened Docker path.
    pub(crate) fn issue_hardened_docker(configuration_digest: String) -> Self {
        debug_assert_eq!(configuration_digest.len(), 64);
        debug_assert!(configuration_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()));
        Self {
            backend_kind: VerifiedIsolatedBackendKind::Docker,
            configuration_digest,
            scope: VerifiedIsolatedBackendScope::ClosedExecution,
            policy_version: CLOSED_EXECUTION_POLICY_VERSION,
        }
    }
}

/// Exact command forwarded to a hardened backend plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainmentCommandRequest {
    pub command: String,
    pub timeout: Option<u64>,
    pub workdir: Option<String>,
    pub background: bool,
    pub pty: bool,
    pub stdin_data: Option<String>,
}

/// Safe receipt proving that the hardened backend launch began.
///
/// No command text, environment value, container id, output, or error detail is
/// included. The digests bind this receipt to one opaque invocation and one
/// immutable configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContainmentLaunchReceipt {
    pub backend_kind: VerifiedIsolatedBackendKind,
    pub configuration_digest: String,
    pub scope: VerifiedIsolatedBackendScope,
    pub policy_version: u16,
    pub invocation_digest: String,
    pub operation: String,
    pub launch_digest: String,
}

/// Safe completion facts for a hardened command invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContainmentCompletionReceipt {
    pub exit_code: i32,
    pub output_digest: String,
}

/// Typed receipt returned only after a certified backend has both launched and
/// completed a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContainmentExecutionReceipt {
    pub launch: ContainmentLaunchReceipt,
    pub completion: ContainmentCompletionReceipt,
    pub receipt_digest: String,
}

/// Successful result of a hardened command plan.
#[derive(Debug, Clone)]
pub struct ContainmentCommandResult {
    pub output: CommandOutput,
    pub receipt: ContainmentExecutionReceipt,
}

/// Typed failure from the contained plan. Launch failure is distinct because no
/// command handler may retry it through a host or manual backend.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContainmentExecutionError {
    #[error("host-sensitive execution requires a certified closed-mode backend")]
    MissingCertifiedBackend,
    #[error("containment invocation does not belong to this execution context")]
    InvalidInvocation,
    #[error("containment invocation was already consumed")]
    InvocationAlreadyConsumed,
    #[error("hardened backend launch failed")]
    LaunchFailed {
        launch: ContainmentLaunchReceipt,
        detail: String,
    },
    #[error("hardened backend command execution failed")]
    ExecutionFailed {
        launch: ContainmentLaunchReceipt,
        detail: String,
    },
}

/// Opaque, single-use authorization for one host-sensitive dispatch.
///
/// Only [`ExecutionContainmentContext::prepare_host_sensitive_invocation`] can
/// construct this token. It is accepted by the same context only once.
#[derive(Debug)]
pub struct ContainmentInvocation {
    context_key: [u8; 32],
    invocation_key: u64,
    configuration_digest: String,
    operation: String,
    used: Arc<AtomicBool>,
}

impl ContainmentInvocation {
    pub fn operation(&self) -> &str {
        &self.operation
    }
}

#[async_trait]
pub(crate) trait HardenedCommandPlan: Send + Sync {
    async fn execute_hardened(
        &self,
        request: ContainmentCommandRequest,
    ) -> Result<CommandOutput, HardenedCommandPlanFailure>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HardenedCommandPlanFailure {
    Launch(String),
    Execution(String),
}

struct ExecutionContainmentInner {
    mode: ResolvedExecutionMode,
    certificate: Option<VerifiedIsolatedBackendCapability>,
    plan: Option<Arc<dyn HardenedCommandPlan>>,
    context_key: [u8; 32],
    next_invocation_key: AtomicU64,
    completed_receipts: Mutex<Vec<ContainmentExecutionReceipt>>,
}

/// Admission result shared by tool-policy dispatch and authoritative run
/// startup. A certified context owns a hardened backend plan, its immutable
/// certificate, and all execution receipts together.
#[derive(Clone)]
pub struct ExecutionContainmentContext {
    inner: Arc<ExecutionContainmentInner>,
}

impl fmt::Debug for ExecutionContainmentContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionContainmentContext")
            .field("mode", &self.inner.mode)
            .field("certificate", &self.inner.certificate)
            .finish_non_exhaustive()
    }
}

impl ExecutionContainmentContext {
    /// Construct the explicit host-local manual context.
    pub fn manual(mode: ResolvedExecutionMode) -> Result<Self, ExecutionContainmentAdmissionError> {
        if mode.mode.requires_containment_certificate() {
            return Err(ExecutionContainmentAdmissionError::MissingCertificate { mode: mode.mode });
        }
        Ok(Self {
            inner: Arc::new(ExecutionContainmentInner {
                mode,
                certificate: None,
                plan: None,
                context_key: [0; 32],
                next_invocation_key: AtomicU64::new(1),
                completed_receipts: Mutex::new(Vec::new()),
            }),
        })
    }

    /// The resolved mode that this context admitted.
    pub fn mode(&self) -> ResolvedExecutionMode {
        self.inner.mode
    }

    /// Certificate used by closed-mode dispatch and run provenance, if any.
    pub fn certificate(&self) -> Option<&VerifiedIsolatedBackendCapability> {
        self.inner.certificate.as_ref()
    }

    /// Canonical sandbox digest for configured-backend run provenance.
    pub fn sandbox_digest(&self) -> Option<&str> {
        self.inner
            .certificate
            .as_ref()
            .map(VerifiedIsolatedBackendCapability::configuration_digest)
    }

    pub fn is_manual_host(&self) -> bool {
        self.inner.certificate.is_none()
    }

    /// True only when a closed-mode certificate and its executable hardened
    /// backend plan were admitted together.
    pub fn is_certified_dispatch_context(&self) -> bool {
        self.inner.certificate.is_some() && self.inner.plan.is_some()
    }

    /// Start one host-sensitive tool invocation. The token is tied to this
    /// context and configuration digest and can be consumed once only.
    pub fn prepare_host_sensitive_invocation(
        &self,
        operation: impl AsRef<str>,
    ) -> Result<ContainmentInvocation, ContainmentExecutionError> {
        let certificate = self
            .inner
            .certificate
            .as_ref()
            .filter(|_| self.inner.plan.is_some())
            .ok_or(ContainmentExecutionError::MissingCertifiedBackend)?;
        let operation = operation.as_ref().trim();
        if operation.is_empty() {
            return Err(ContainmentExecutionError::InvalidInvocation);
        }
        let invocation_key = self
            .inner
            .next_invocation_key
            .fetch_add(1, Ordering::Relaxed);
        Ok(ContainmentInvocation {
            context_key: self.inner.context_key,
            invocation_key,
            configuration_digest: certificate.configuration_digest.clone(),
            operation: operation.to_string(),
            used: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Execute through the certified hardened plan only. There is deliberately
    /// no host/manual fallback on launch or execution failure.
    pub async fn execute_host_sensitive(
        &self,
        invocation: ContainmentInvocation,
        request: ContainmentCommandRequest,
    ) -> Result<ContainmentCommandResult, ContainmentExecutionError> {
        let certificate = self
            .inner
            .certificate
            .as_ref()
            .filter(|_| self.inner.plan.is_some())
            .ok_or(ContainmentExecutionError::MissingCertifiedBackend)?;
        if invocation.context_key != self.inner.context_key
            || invocation.configuration_digest != certificate.configuration_digest
        {
            return Err(ContainmentExecutionError::InvalidInvocation);
        }
        if invocation.used.swap(true, Ordering::AcqRel) {
            return Err(ContainmentExecutionError::InvocationAlreadyConsumed);
        }

        let invocation_digest = invocation_digest(
            &invocation.context_key,
            invocation.invocation_key,
            &certificate.configuration_digest,
            &invocation.operation,
        );
        let launch =
            containment_launch_receipt(certificate, invocation_digest, invocation.operation);
        let plan = self
            .inner
            .plan
            .as_ref()
            .expect("certificate and plan are admitted together")
            .clone();
        let output = match plan.execute_hardened(request).await {
            Ok(output) => output,
            Err(HardenedCommandPlanFailure::Launch(detail)) => {
                return Err(ContainmentExecutionError::LaunchFailed { launch, detail })
            }
            Err(HardenedCommandPlanFailure::Execution(detail)) => {
                return Err(ContainmentExecutionError::ExecutionFailed { launch, detail })
            }
        };
        let completion = ContainmentCompletionReceipt {
            exit_code: output.exit_code,
            output_digest: command_output_digest(&output),
        };
        let receipt = ContainmentExecutionReceipt {
            receipt_digest: execution_receipt_digest(&launch, &completion),
            launch,
            completion,
        };
        self.inner.completed_receipts.lock().push(receipt.clone());
        Ok(ContainmentCommandResult { output, receipt })
    }

    /// Safe completed receipts for authoritative effect evidence.
    pub fn completed_execution_receipts(&self) -> Vec<ContainmentExecutionReceipt> {
        self.inner.completed_receipts.lock().clone()
    }

    /// Internal admission path. The certificate is only supplied by the
    /// hardened Docker backend that also supplies the executable plan.
    pub(crate) fn certified(
        mode: ResolvedExecutionMode,
        certificate: VerifiedIsolatedBackendCapability,
        plan: Arc<dyn HardenedCommandPlan>,
    ) -> Result<Self, ExecutionContainmentAdmissionError> {
        if !mode.mode.requires_containment_certificate() {
            return Err(ExecutionContainmentAdmissionError::UnexpectedCertificateForManual);
        }
        if !certificate.is_closed_execution_certificate() {
            return Err(ExecutionContainmentAdmissionError::InvalidCertificate { mode: mode.mode });
        }
        Ok(Self {
            inner: Arc::new(ExecutionContainmentInner {
                mode,
                context_key: context_key(&certificate),
                certificate: Some(certificate),
                plan: Some(plan),
                next_invocation_key: AtomicU64::new(1),
                completed_receipts: Mutex::new(Vec::new()),
            }),
        })
    }
}

/// Typed reason a mode could not be admitted to an execution backend.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecutionContainmentAdmissionError {
    #[error("closed execution mode {mode} requires a verified isolated backend certificate")]
    MissingCertificate { mode: ExecutionMode },
    #[error("manual execution must use its explicit host-local containment context")]
    UnexpectedCertificateForManual,
    #[error("containment certificate is invalid for closed execution mode {mode}")]
    InvalidCertificate { mode: ExecutionMode },
}

fn context_key(certificate: &VerifiedIsolatedBackendCapability) -> [u8; 32] {
    let nonce = NEXT_CONTEXT_NONCE.fetch_add(1, Ordering::Relaxed);
    let mut hasher = Sha256::new();
    hasher.update(b"hermes.closed-execution-context/v1\0");
    hasher.update(certificate.configuration_digest.as_bytes());
    hasher.update(nonce.to_be_bytes());
    hasher.finalize().into()
}

fn invocation_digest(
    context_key: &[u8; 32],
    invocation_key: u64,
    configuration_digest: &str,
    operation: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"hermes.closed-execution-invocation/v1\0");
    hasher.update(context_key);
    hasher.update(invocation_key.to_be_bytes());
    hasher.update(configuration_digest.as_bytes());
    hasher.update(operation.as_bytes());
    hex::encode(hasher.finalize())
}

fn containment_launch_receipt(
    certificate: &VerifiedIsolatedBackendCapability,
    invocation_digest: String,
    operation: String,
) -> ContainmentLaunchReceipt {
    let mut hasher = Sha256::new();
    hasher.update(b"hermes.closed-execution-launch/v1\0");
    hasher.update(certificate.configuration_digest.as_bytes());
    hasher.update(invocation_digest.as_bytes());
    hasher.update(operation.as_bytes());
    ContainmentLaunchReceipt {
        backend_kind: certificate.backend_kind,
        configuration_digest: certificate.configuration_digest.clone(),
        scope: certificate.scope,
        policy_version: certificate.policy_version,
        invocation_digest,
        operation,
        launch_digest: hex::encode(hasher.finalize()),
    }
}

fn command_output_digest(output: &CommandOutput) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"hermes.closed-execution-output/v1\0");
    hasher.update(output.exit_code.to_be_bytes());
    hasher.update((output.stdout.len() as u64).to_be_bytes());
    hasher.update(output.stdout.as_bytes());
    hasher.update((output.stderr.len() as u64).to_be_bytes());
    hasher.update(output.stderr.as_bytes());
    hex::encode(hasher.finalize())
}

fn execution_receipt_digest(
    launch: &ContainmentLaunchReceipt,
    completion: &ContainmentCompletionReceipt,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"hermes.closed-execution-receipt/v1\0");
    hasher.update(launch.launch_digest.as_bytes());
    hasher.update(completion.exit_code.to_be_bytes());
    hasher.update(completion.output_digest.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use hermes_core::{ExecutionMode, ExecutionModeSource};

    use super::*;

    fn closed_mode() -> ResolvedExecutionMode {
        ResolvedExecutionMode {
            mode: ExecutionMode::Reviewed,
            source: ExecutionModeSource::Cli,
        }
    }

    #[test]
    fn closed_modes_cannot_be_admitted_by_a_mode_label() {
        assert_eq!(
            ExecutionContainmentContext::manual(closed_mode()).err(),
            Some(ExecutionContainmentAdmissionError::MissingCertificate {
                mode: ExecutionMode::Reviewed,
            })
        );
    }

    #[test]
    fn manual_host_context_is_explicit_and_has_no_sandbox_digest() {
        let context = ExecutionContainmentContext::manual(ResolvedExecutionMode {
            mode: ExecutionMode::Manual,
            source: ExecutionModeSource::BuiltInDefault,
        })
        .expect("manual context");
        assert!(context.is_manual_host());
        assert!(context.sandbox_digest().is_none());
        assert!(context
            .prepare_host_sensitive_invocation("terminal")
            .is_err());
    }
}
