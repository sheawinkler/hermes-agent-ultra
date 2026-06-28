//! Command approval system
//!
//! Checks whether a terminal command requires explicit user approval
//! before execution, based on dangerous command patterns.

use regex::Regex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::Duration;

// ---------------------------------------------------------------------------
// ApprovalDecision
// ---------------------------------------------------------------------------

/// Decision from the approval check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Command is safe to execute without confirmation.
    Approved,
    /// Command is denied outright.
    Denied,
    /// Command requires user confirmation before execution.
    RequiresConfirmation,
}

/// User choice from an interactive approval prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalChoice {
    Deny,
    Once,
    Session,
    Always,
}

impl ApprovalChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Once => "once",
            Self::Session => "session",
            Self::Always => "always",
        }
    }
}

/// Human-facing prompt data for a combined command guard warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalPrompt {
    pub command: String,
    pub description: String,
    pub pattern_key: String,
    pub pattern_keys: Vec<String>,
    pub allow_permanent: bool,
}

/// Approval lifecycle hook emitted around user-visible approval requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalHookKind {
    PreApprovalRequest,
    PostApprovalResponse,
}

impl ApprovalHookKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreApprovalRequest => "pre_approval_request",
            Self::PostApprovalResponse => "post_approval_response",
        }
    }
}

/// Surface responsible for resolving an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalSurface {
    Cli,
    Gateway,
}

impl ApprovalSurface {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Gateway => "gateway",
        }
    }
}

/// Observer event for approval plugins/integrations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalHookEvent {
    pub kind: ApprovalHookKind,
    pub surface: ApprovalSurface,
    pub session_key: String,
    pub command: String,
    pub description: String,
    pub pattern_key: String,
    pub pattern_keys: Vec<String>,
    pub allow_permanent: bool,
    pub choice: Option<ApprovalChoice>,
}

/// Final result returned by combined command guards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandGuardResult {
    pub approved: bool,
    pub message: Option<String>,
    pub pattern_key: Option<String>,
    pub description: Option<String>,
    pub user_approved: bool,
    pub outcome: Option<String>,
    pub status: Option<String>,
    pub approval_pending: bool,
}

impl CommandGuardResult {
    fn approved() -> Self {
        Self {
            approved: true,
            message: None,
            pattern_key: None,
            description: None,
            user_approved: false,
            outcome: None,
            status: None,
            approval_pending: false,
        }
    }

    fn blocked(message: String, pattern_key: Option<String>, description: Option<String>) -> Self {
        Self {
            approved: false,
            message: Some(message),
            pattern_key,
            description,
            user_approved: false,
            outcome: Some("denied".to_string()),
            status: None,
            approval_pending: false,
        }
    }

    fn pending_approval(
        message: String,
        pattern_key: Option<String>,
        description: Option<String>,
    ) -> Self {
        Self {
            approved: false,
            message: Some(message),
            pattern_key,
            description,
            user_approved: false,
            outcome: Some("pending_approval".to_string()),
            status: Some("pending_approval".to_string()),
            approval_pending: true,
        }
    }
}

/// Errors from injected security scanners. Import/unavailable scanners are
/// modeled as `Ok(None)` so only wrapper bugs propagate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandGuardError {
    SecurityScanner(String),
}

impl std::fmt::Display for CommandGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SecurityScanner(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for CommandGuardError {}

/// Tirith scanner action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TirithAction {
    Allow,
    Warn,
    Block,
}

/// A single Tirith finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TirithFinding {
    pub rule_id: Option<String>,
    pub severity: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
}

impl TirithFinding {
    pub fn new(rule_id: impl Into<String>) -> Self {
        Self {
            rule_id: Some(rule_id.into()),
            severity: None,
            title: None,
            description: None,
        }
    }
}

/// Result from a Tirith command scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TirithResult {
    pub action: TirithAction,
    pub findings: Vec<TirithFinding>,
    pub summary: String,
}

impl TirithResult {
    pub fn allow() -> Self {
        Self {
            action: TirithAction::Allow,
            findings: Vec::new(),
            summary: String::new(),
        }
    }

    pub fn warn(rule_id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            action: TirithAction::Warn,
            findings: vec![TirithFinding::new(rule_id)],
            summary: summary.into(),
        }
    }

    pub fn block(summary: impl Into<String>) -> Self {
        Self {
            action: TirithAction::Block,
            findings: Vec::new(),
            summary: summary.into(),
        }
    }
}

/// Deterministic policy inputs for `check_all_command_guards_with_context`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandGuardContext {
    pub interactive: bool,
    pub gateway: bool,
    pub ask: bool,
    pub yolo_mode: bool,
    pub approval_mode_off: bool,
    pub sudo_password_configured: bool,
    pub cron_session: bool,
    pub cron_approval_deny: bool,
    pub session_key: Option<String>,
    pub tirith_result: Result<Option<TirithResult>, CommandGuardError>,
    pub gateway_approval_timeout: Duration,
}

impl CommandGuardContext {
    pub fn from_env() -> Self {
        let cron_session = env_var_enabled("HERMES_CRON_SESSION");
        Self {
            interactive: env_var_enabled("HERMES_INTERACTIVE"),
            gateway: env_var_enabled("HERMES_GATEWAY_SESSION"),
            ask: env_var_enabled("HERMES_EXEC_ASK"),
            yolo_mode: yolo_mode_from_env() || current_session_yolo_from_env(),
            approval_mode_off: false,
            sudo_password_configured: has_sudo_password_env(),
            cron_session,
            cron_approval_deny: cron_session && !cron_approval_mode_approves_from_env(),
            session_key: current_session_key_from_env().or_else(|| Some("default".to_string())),
            tirith_result: Ok(None),
            gateway_approval_timeout: Duration::from_secs(300),
        }
    }

    pub fn interactive_with_tirith(tirith_result: TirithResult) -> Self {
        Self {
            interactive: true,
            tirith_result: Ok(Some(tirith_result)),
            ..Self::default()
        }
    }

    fn is_interactive_surface(&self) -> bool {
        self.interactive || self.gateway || self.ask
    }

    fn session_key(&self) -> String {
        self.session_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("default")
            .to_string()
    }
}

impl Default for CommandGuardContext {
    fn default() -> Self {
        Self {
            interactive: false,
            gateway: false,
            ask: false,
            yolo_mode: false,
            approval_mode_off: false,
            sudo_password_configured: false,
            cron_session: false,
            cron_approval_deny: false,
            session_key: Some("default".to_string()),
            tirith_result: Ok(None),
            gateway_approval_timeout: Duration::from_secs(300),
        }
    }
}

/// Approval payload queued for gateway-visible command confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayApprovalRequest {
    pub session_key: String,
    pub command: String,
    pub description: String,
    pub pattern_key: String,
    pub pattern_keys: Vec<String>,
    pub allow_permanent: bool,
}

impl GatewayApprovalRequest {
    /// Return a user-facing copy safe to emit over gateway/TUI/chat transports.
    ///
    /// The pending approval queue keeps the raw command for local resolution
    /// state, but client-facing approval prompts are a hard secret-egress
    /// boundary and must not echo credential-shaped command text.
    pub fn redacted_for_display(&self) -> Self {
        let mut request = self.clone();
        request.command = redact_approval_command(&request.command);
        request
    }
}

#[derive(Debug)]
pub struct GatewayApprovalEntry {
    request: GatewayApprovalRequest,
    result: Mutex<Option<ApprovalChoice>>,
    resolved: Condvar,
}

impl GatewayApprovalEntry {
    pub fn new(request: GatewayApprovalRequest) -> Self {
        Self {
            request,
            result: Mutex::new(None),
            resolved: Condvar::new(),
        }
    }

    pub fn request(&self) -> &GatewayApprovalRequest {
        &self.request
    }

    pub fn result(&self) -> Option<ApprovalChoice> {
        *self.result.lock().expect("gateway approval lock poisoned")
    }

    pub fn is_resolved(&self) -> bool {
        self.result().is_some()
    }

    fn resolve(&self, choice: ApprovalChoice) {
        let mut result = self.result.lock().expect("gateway approval lock poisoned");
        if result.is_none() {
            *result = Some(choice);
            self.resolved.notify_all();
        }
    }

    pub fn wait(&self, timeout: Duration) -> Option<ApprovalChoice> {
        let result = self.result.lock().expect("gateway approval lock poisoned");
        let (result, _) = self
            .resolved
            .wait_timeout_while(result, timeout, |choice| choice.is_none())
            .expect("gateway approval condvar poisoned");
        *result
    }
}

/// Recoverable dangerous-command detection result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DangerousCommandFinding {
    pub pattern_key: String,
    pub description: String,
}

// ---------------------------------------------------------------------------
// Dangerous patterns
// ---------------------------------------------------------------------------

// Patterns that are always denied.
include!("approval/patterns.rs");

#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn collapse_command(command: &str) -> String {
    command
        .replace("\\\n", " ")
        .replace(['\n', '\r', '\t'], " ")
}

fn has_sudo_password_env() -> bool {
    std::env::var("SUDO_PASSWORD")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

fn yolo_mode_from_env() -> bool {
    std::env::var("HERMES_YOLO_MODE")
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

fn cron_approval_mode_value_approves(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "approve" | "allow" | "yes" | "on" | "true" | "1" | "off"
    )
}

fn cron_approval_mode_approves_from_env() -> bool {
    for key in ["HERMES_CRON_APPROVAL_MODE", "HERMES_APPROVALS_CRON_MODE"] {
        if let Ok(value) = std::env::var(key) {
            return cron_approval_mode_value_approves(&value);
        }
    }
    false
}

fn current_session_key_from_env() -> Option<String> {
    std::env::var("HERMES_SESSION_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn current_session_yolo_from_env() -> bool {
    current_session_key_from_env()
        .map(|session_key| is_session_yolo_enabled(&session_key))
        .unwrap_or(false)
}

fn env_var_enabled(key: &str) -> bool {
    std::env::var(key)
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

/// Approve a warning pattern for this session only.
pub fn approve_session(session_key: &str, pattern_key: &str) {
    let session_key = session_key.trim();
    let pattern_key = pattern_key.trim();
    if session_key.is_empty() || pattern_key.is_empty() {
        return;
    }
    SESSION_APPROVED
        .lock()
        .expect("session approval lock poisoned")
        .entry(session_key.to_string())
        .or_default()
        .insert(pattern_key.to_string());
}

/// Approve a warning pattern for this process.
pub fn approve_permanent(pattern_key: &str) {
    let pattern_key = pattern_key.trim();
    if pattern_key.is_empty() {
        return;
    }
    PERMANENT_APPROVED
        .lock()
        .expect("permanent approval lock poisoned")
        .insert(pattern_key.to_string());
}

/// Return whether a warning pattern is approved in this session or process.
pub fn is_approved(session_key: &str, pattern_key: &str) -> bool {
    let session_key = session_key.trim();
    let pattern_key = pattern_key.trim();
    if pattern_key.is_empty() {
        return false;
    }
    if PERMANENT_APPROVED
        .lock()
        .expect("permanent approval lock poisoned")
        .contains(pattern_key)
    {
        return true;
    }
    if session_key.is_empty() {
        return false;
    }
    SESSION_APPROVED
        .lock()
        .expect("session approval lock poisoned")
        .get(session_key)
        .map(|patterns| patterns.contains(pattern_key))
        .unwrap_or(false)
}

/// Register a gateway callback that receives newly blocked command approvals.
pub fn register_gateway_notify<F>(session_key: &str, callback: F)
where
    F: Fn(GatewayApprovalRequest) + Send + Sync + 'static,
{
    let session_key = session_key.trim();
    if session_key.is_empty() {
        return;
    }
    GATEWAY_NOTIFY_CBS
        .lock()
        .expect("gateway notify lock poisoned")
        .insert(session_key.to_string(), Arc::new(callback));
}

/// Remove a gateway callback and deny any blocked approval waiters for that session.
pub fn unregister_gateway_notify(session_key: &str) {
    let session_key = session_key.trim();
    if session_key.is_empty() {
        return;
    }
    GATEWAY_NOTIFY_CBS
        .lock()
        .expect("gateway notify lock poisoned")
        .remove(session_key);
    cancel_gateway_approvals(session_key, ApprovalChoice::Deny);
}

/// Return whether a session currently has blocked gateway approvals.
pub fn has_blocking_approval(session_key: &str) -> bool {
    let session_key = session_key.trim();
    if session_key.is_empty() {
        return false;
    }
    GATEWAY_QUEUES
        .lock()
        .expect("gateway queue lock poisoned")
        .get(session_key)
        .map(|entries| !entries.is_empty())
        .unwrap_or(false)
}

/// Number of pending gateway approval entries for a session.
pub fn pending_gateway_approval_count(session_key: &str) -> usize {
    let session_key = session_key.trim();
    if session_key.is_empty() {
        return 0;
    }
    GATEWAY_QUEUES
        .lock()
        .expect("gateway queue lock poisoned")
        .get(session_key)
        .map(VecDeque::len)
        .unwrap_or(0)
}

/// Resolve pending gateway approvals. Without `resolve_all`, this resolves the oldest entry.
pub fn resolve_gateway_approval(
    session_key: &str,
    choice: ApprovalChoice,
    resolve_all: bool,
) -> usize {
    let session_key = session_key.trim();
    if session_key.is_empty() {
        return 0;
    }

    let resolved = {
        let mut queues = GATEWAY_QUEUES.lock().expect("gateway queue lock poisoned");
        let Some(queue) = queues.get_mut(session_key) else {
            return 0;
        };
        let entries = if resolve_all {
            queue.drain(..).collect::<Vec<_>>()
        } else {
            queue.pop_front().into_iter().collect::<Vec<_>>()
        };
        let remove_queue = queue.is_empty();
        if remove_queue {
            queues.remove(session_key);
        }
        entries
    };

    for entry in &resolved {
        entry.resolve(choice);
    }
    resolved.len()
}

/// Register an observer for approval lifecycle events.
///
/// Observers are intentionally best-effort: a panic in one observer is
/// contained and cannot alter approval safety decisions.
pub fn register_approval_observer<F>(callback: F) -> u64
where
    F: Fn(ApprovalHookEvent) + Send + Sync + 'static,
{
    let id = NEXT_APPROVAL_OBSERVER_ID.fetch_add(1, Ordering::SeqCst);
    APPROVAL_OBSERVERS
        .lock()
        .expect("approval observer lock poisoned")
        .insert(id, Arc::new(callback));
    id
}

/// Remove a previously registered approval observer.
pub fn unregister_approval_observer(id: u64) -> bool {
    APPROVAL_OBSERVERS
        .lock()
        .expect("approval observer lock poisoned")
        .remove(&id)
        .is_some()
}

fn approval_surface(context: &CommandGuardContext) -> ApprovalSurface {
    if context.gateway || context.ask {
        ApprovalSurface::Gateway
    } else {
        ApprovalSurface::Cli
    }
}

fn emit_approval_hook(
    kind: ApprovalHookKind,
    surface: ApprovalSurface,
    session_key: &str,
    command: &str,
    prompt: &ApprovalPrompt,
    choice: Option<ApprovalChoice>,
) {
    let event = ApprovalHookEvent {
        kind,
        surface,
        session_key: session_key.to_string(),
        command: command.to_string(),
        description: prompt.description.clone(),
        pattern_key: prompt.pattern_key.clone(),
        pattern_keys: prompt.pattern_keys.clone(),
        allow_permanent: prompt.allow_permanent,
        choice,
    };
    let observers = APPROVAL_OBSERVERS
        .lock()
        .expect("approval observer lock poisoned")
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for observer in observers {
        let event = event.clone();
        let _ = catch_unwind(AssertUnwindSafe(|| observer(event)));
    }
}

fn cancel_gateway_approvals(session_key: &str, choice: ApprovalChoice) -> usize {
    resolve_gateway_approval(session_key, choice, true)
}

fn gateway_notify_callback(session_key: &str) -> Option<GatewayNotifyCallback> {
    GATEWAY_NOTIFY_CBS
        .lock()
        .expect("gateway notify lock poisoned")
        .get(session_key)
        .cloned()
}

enum GatewayApprovalWaitOutcome {
    NoListener,
    Resolved(ApprovalChoice),
    TimedOut,
}

fn submit_gateway_approval_and_wait(
    request: GatewayApprovalRequest,
    timeout: Duration,
) -> GatewayApprovalWaitOutcome {
    let Some(callback) = gateway_notify_callback(&request.session_key) else {
        return GatewayApprovalWaitOutcome::NoListener;
    };

    let session_key = request.session_key.clone();
    let entry = Arc::new(GatewayApprovalEntry::new(request.clone()));
    {
        let mut queues = GATEWAY_QUEUES.lock().expect("gateway queue lock poisoned");
        queues
            .entry(session_key.clone())
            .or_default()
            .push_back(entry.clone());
    }

    callback(request.redacted_for_display());

    if let Some(choice) = entry.wait(timeout) {
        GatewayApprovalWaitOutcome::Resolved(choice)
    } else {
        let mut queues = GATEWAY_QUEUES.lock().expect("gateway queue lock poisoned");
        if let Some(queue) = queues.get_mut(&session_key) {
            queue.retain(|candidate| !Arc::ptr_eq(candidate, &entry));
            let remove_queue = queue.is_empty();
            if remove_queue {
                queues.remove(&session_key);
            }
        }
        GatewayApprovalWaitOutcome::TimedOut
    }
}

/// Redact credentials from a command before it is shown in approval prompts.
///
/// Tirith/security scanners may redact their findings, but approval prompts
/// are built from the raw command string. This seam is intentionally
/// unconditional so gateway/TUI/chat approval transports cannot leak secrets
/// even if a broader user-facing redaction preference is disabled elsewhere.
pub fn redact_approval_command(command: impl ToString) -> String {
    hermes_intelligence::redact_sensitive_text(command)
}

/// Enable yolo approval bypass for a single session key.
pub fn enable_session_yolo(session_key: &str) {
    let session_key = session_key.trim();
    if session_key.is_empty() {
        return;
    }
    SESSION_YOLO
        .lock()
        .expect("session yolo lock poisoned")
        .insert(session_key.to_string());
}

/// Disable yolo approval bypass for a single session key.
pub fn disable_session_yolo(session_key: &str) {
    let session_key = session_key.trim();
    if session_key.is_empty() {
        return;
    }
    SESSION_YOLO
        .lock()
        .expect("session yolo lock poisoned")
        .remove(session_key);
}

/// Remove approval state associated with a session boundary.
pub fn clear_session(session_key: &str) {
    disable_session_yolo(session_key);
    let session_key = session_key.trim();
    if session_key.is_empty() {
        return;
    }
    SESSION_APPROVED
        .lock()
        .expect("session approval lock poisoned")
        .remove(session_key);
    cancel_gateway_approvals(session_key, ApprovalChoice::Deny);
}

/// Return whether yolo approval bypass is enabled for this session key.
pub fn is_session_yolo_enabled(session_key: &str) -> bool {
    let session_key = session_key.trim();
    if session_key.is_empty() {
        return false;
    }
    SESSION_YOLO
        .lock()
        .expect("session yolo lock poisoned")
        .contains(session_key)
}

fn environment_bypasses_host_guards(environment: &str) -> bool {
    CONTAINER_BACKENDS
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(environment))
}

fn delete_without_where(command: &str) -> bool {
    DELETE_FROM.is_match(command) && !command.to_ascii_lowercase().contains(" where ")
}

fn is_fork_bomb(command: &str) -> bool {
    let compact: String = command.chars().filter(|ch| !ch.is_whitespace()).collect();
    compact.contains(":(){:|:&};:")
}

fn hardline_reason(command: &str, sudo_password_configured: bool) -> Option<&'static str> {
    let normalized = collapse_command(command);
    if HARDLINE_RM_PROTECTED_PATH.is_match(&normalized) {
        return Some("unrecoverable recursive delete of a protected path");
    }
    if HARDLINE_MKFS_BLOCK_DEVICE.is_match(&normalized) {
        return Some("filesystem creation on a block device");
    }
    if HARDLINE_DD_BLOCK_DEVICE.is_match(&normalized) {
        return Some("raw overwrite of a block device");
    }
    if HARDLINE_REDIRECT_BLOCK_DEVICE.is_match(&normalized) {
        return Some("shell redirection to a block device");
    }
    if is_fork_bomb(&normalized) {
        return Some("fork bomb");
    }
    if HARDLINE_KILL_ALL.is_match(&normalized) {
        return Some("system-wide kill");
    }
    if HARDLINE_STOP_SYSTEM.is_match(&normalized) {
        return Some("host shutdown/reboot/halt");
    }
    if !sudo_password_configured && SUDO_STDIN_GUARD.is_match(&normalized) {
        return Some("sudo stdin/askpass requires an explicit configured password");
    }
    None
}

fn detect_dangerous_command_detail(command: &str) -> Option<DangerousCommandFinding> {
    let normalized = collapse_command(command);
    if delete_without_where(&normalized) {
        return Some(DangerousCommandFinding {
            pattern_key: "SQL DELETE without WHERE".to_string(),
            description: "SQL DELETE without WHERE".to_string(),
        });
    }
    for rule in DANGEROUS_COMMAND_RULES.iter() {
        if rule.regex.is_match(&normalized) {
            return Some(DangerousCommandFinding {
                pattern_key: rule.key.to_string(),
                description: rule.description.to_string(),
            });
        }
    }
    None
}

/// Detect recoverable dangerous commands that require approval.
pub fn detect_dangerous_command(command: &str) -> Option<DangerousCommandFinding> {
    detect_dangerous_command_detail(command)
}

fn tirith_pattern_key(result: &TirithResult) -> String {
    result
        .findings
        .first()
        .and_then(|finding| finding.rule_id.as_deref())
        .map(str::trim)
        .filter(|rule_id| !rule_id.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

fn format_tirith_description(result: &TirithResult) -> String {
    let mut parts = Vec::new();
    for finding in &result.findings {
        let severity = finding.severity.as_deref().unwrap_or("").trim();
        let title = finding.title.as_deref().unwrap_or("").trim();
        let description = finding.description.as_deref().unwrap_or("").trim();
        if title.is_empty() && description.is_empty() {
            continue;
        }
        let text = if !title.is_empty() && !description.is_empty() {
            format!("{title}: {description}")
        } else if !title.is_empty() {
            title.to_string()
        } else {
            description.to_string()
        };
        if severity.is_empty() {
            parts.push(text);
        } else {
            parts.push(format!("[{severity}] {text}"));
        }
    }
    if !parts.is_empty() {
        return format!("Security scan: {}", parts.join("; "));
    }
    let summary = result.summary.trim();
    if summary.is_empty() {
        "Security scan: security issue detected".to_string()
    } else {
        format!("Security scan: {summary}")
    }
}

struct GuardWarning {
    pattern_key: String,
    description: String,
    is_tirith: bool,
}

fn persist_approval_choice(session_key: &str, warnings: &[GuardWarning], choice: ApprovalChoice) {
    for warning in warnings {
        match choice {
            ApprovalChoice::Session => approve_session(session_key, &warning.pattern_key),
            ApprovalChoice::Always if warning.is_tirith => {
                approve_session(session_key, &warning.pattern_key)
            }
            ApprovalChoice::Always => {
                approve_session(session_key, &warning.pattern_key);
                approve_permanent(&warning.pattern_key);
            }
            ApprovalChoice::Once | ApprovalChoice::Deny => {}
        }
    }
}

fn user_denied_result(pattern_key: String, description: String) -> CommandGuardResult {
    CommandGuardResult::blocked(
        "BLOCKED: User denied this command. The user has NOT consented to this action. Do NOT retry this command, do NOT rephrase it, and do NOT attempt the same outcome via a different command. Stop the current workflow and wait for the user to respond before taking any further destructive or irreversible action.".to_string(),
        Some(pattern_key),
        Some(description),
    )
}

/// Run Tirith and dangerous-command checks as one approval surface.
pub fn check_all_command_guards(
    command: &str,
    environment: &str,
) -> Result<CommandGuardResult, CommandGuardError> {
    check_all_command_guards_with_context(
        command,
        environment,
        CommandGuardContext::from_env(),
        None,
    )
}

/// Run combined command guards with explicit policy inputs and optional prompt callback.
pub fn check_all_command_guards_with_context(
    command: &str,
    environment: &str,
    context: CommandGuardContext,
    mut approval_callback: Option<&mut dyn FnMut(ApprovalPrompt) -> ApprovalChoice>,
) -> Result<CommandGuardResult, CommandGuardError> {
    if environment_bypasses_host_guards(environment) {
        return Ok(CommandGuardResult::approved());
    }

    if let Some(reason) = hardline_reason(command, context.sudo_password_configured) {
        return Ok(CommandGuardResult::blocked(
            format!("BLOCKED: Command denied by hardline security policy: {reason}."),
            None,
            Some(reason.to_string()),
        ));
    }

    if context.yolo_mode || context.approval_mode_off {
        return Ok(CommandGuardResult::approved());
    }

    if context.cron_session {
        if context.cron_approval_deny {
            if let Some(finding) = detect_dangerous_command_detail(command) {
                return Ok(CommandGuardResult::blocked(
                    format!(
                        "BLOCKED: Command flagged as dangerous ({}) but cron jobs run without a user present to approve it.",
                        finding.description
                    ),
                    Some(finding.pattern_key),
                    Some(finding.description),
                ));
            }
        }
        return Ok(CommandGuardResult::approved());
    }

    if !context.is_interactive_surface() {
        return Ok(CommandGuardResult::approved());
    }

    let tirith_result = context.tirith_result.clone()?;
    let session_key = context.session_key();
    let mut warnings = Vec::new();

    if let Some(result) = tirith_result {
        if matches!(result.action, TirithAction::Warn | TirithAction::Block) {
            let rule_id = tirith_pattern_key(&result);
            let pattern_key = format!("tirith:{rule_id}");
            if !is_approved(&session_key, &pattern_key) {
                warnings.push(GuardWarning {
                    pattern_key,
                    description: format_tirith_description(&result),
                    is_tirith: true,
                });
            }
        }
    }

    if let Some(finding) = detect_dangerous_command_detail(command) {
        if !is_approved(&session_key, &finding.pattern_key) {
            warnings.push(GuardWarning {
                pattern_key: finding.pattern_key,
                description: finding.description,
                is_tirith: false,
            });
        }
    }

    if warnings.is_empty() {
        return Ok(CommandGuardResult::approved());
    }

    let combined_desc = warnings
        .iter()
        .map(|warning| warning.description.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    let primary_key = warnings[0].pattern_key.clone();
    let pattern_keys = warnings
        .iter()
        .map(|warning| warning.pattern_key.clone())
        .collect::<Vec<_>>();
    let allow_permanent = !warnings.iter().any(|warning| warning.is_tirith);

    let prompt = ApprovalPrompt {
        command: command.to_string(),
        description: combined_desc.clone(),
        pattern_key: primary_key.clone(),
        pattern_keys,
        allow_permanent,
    };
    let surface = approval_surface(&context);
    emit_approval_hook(
        ApprovalHookKind::PreApprovalRequest,
        surface,
        &session_key,
        command,
        &prompt,
        None,
    );

    let choice = if let Some(callback) = approval_callback.as_mut() {
        callback(prompt.clone())
    } else if context.gateway || context.ask {
        let request = GatewayApprovalRequest {
            session_key: session_key.clone(),
            command: command.to_string(),
            description: combined_desc.clone(),
            pattern_key: primary_key.clone(),
            pattern_keys: warnings
                .iter()
                .map(|warning| warning.pattern_key.clone())
                .collect(),
            allow_permanent,
        };
        match submit_gateway_approval_and_wait(request, context.gateway_approval_timeout) {
            GatewayApprovalWaitOutcome::Resolved(choice) => choice,
            GatewayApprovalWaitOutcome::TimedOut => {
                return Ok(CommandGuardResult::blocked(
                    "BLOCKED: Command approval timed out waiting for a gateway response."
                        .to_string(),
                    Some(primary_key),
                    Some(combined_desc),
                ));
            }
            GatewayApprovalWaitOutcome::NoListener => {
                return Ok(CommandGuardResult::pending_approval(
                    "Command requires approval, but no gateway approval listener is registered."
                        .to_string(),
                    Some(primary_key),
                    Some(combined_desc),
                ));
            }
        }
    } else {
        ApprovalChoice::Deny
    };
    emit_approval_hook(
        ApprovalHookKind::PostApprovalResponse,
        surface,
        &session_key,
        command,
        &prompt,
        Some(choice),
    );

    if choice == ApprovalChoice::Deny {
        return Ok(user_denied_result(primary_key, combined_desc));
    }

    persist_approval_choice(&session_key, &warnings, choice);
    Ok(CommandGuardResult {
        approved: true,
        message: None,
        pattern_key: None,
        description: Some(combined_desc),
        user_approved: true,
        outcome: None,
        status: None,
        approval_pending: false,
    })
}

// ---------------------------------------------------------------------------
// ApprovalManager
// ---------------------------------------------------------------------------

/// Manages command approval checks.
pub struct ApprovalManager {
    /// Custom denied patterns (compiled regexes).
    denied_patterns: Vec<Regex>,
    /// Custom confirm patterns (compiled regexes).
    confirm_patterns: Vec<Regex>,
}

impl ApprovalManager {
    /// Create a new ApprovalManager with built-in patterns.
    pub fn new() -> Self {
        Self {
            denied_patterns: Vec::new(),
            confirm_patterns: Vec::new(),
        }
    }

    /// Add a custom denied pattern.
    pub fn add_denied_pattern(&mut self, pattern: &str) -> Result<(), regex::Error> {
        let re = Regex::new(pattern)?;
        self.denied_patterns.push(re);
        Ok(())
    }

    /// Add a custom confirm-required pattern.
    pub fn add_confirm_pattern(&mut self, pattern: &str) -> Result<(), regex::Error> {
        let re = Regex::new(pattern)?;
        self.confirm_patterns.push(re);
        Ok(())
    }

    /// Check whether a command requires approval.
    ///
    /// Returns:
    /// - `Denied` if the command matches a denied pattern
    /// - `RequiresConfirmation` if the command matches a confirm pattern
    /// - `Approved` if no patterns match
    pub fn check_approval(&self, command: &str) -> ApprovalDecision {
        self.check_approval_with_context(command, "local", false, false)
    }

    /// Check whether a command requires approval for a backend/environment.
    ///
    /// Containerized backends cannot affect the host filesystem directly, so
    /// they intentionally bypass the host-level approval floor.
    pub fn check_approval_for_environment(
        &self,
        command: &str,
        environment: &str,
    ) -> ApprovalDecision {
        self.check_approval_with_context(command, environment, false, false)
    }

    /// Check approval using process environment toggles such as
    /// `HERMES_YOLO_MODE` and `SUDO_PASSWORD`.
    pub fn check_approval_from_env(&self, command: &str, environment: &str) -> ApprovalDecision {
        let cron_approve =
            env_var_enabled("HERMES_CRON_SESSION") && cron_approval_mode_approves_from_env();
        self.check_approval_with_context(
            command,
            environment,
            yolo_mode_from_env() || current_session_yolo_from_env() || cron_approve,
            has_sudo_password_env(),
        )
    }

    /// Check approval with explicit policy inputs for deterministic callers.
    pub fn check_approval_with_context(
        &self,
        command: &str,
        environment: &str,
        yolo_mode: bool,
        sudo_password_configured: bool,
    ) -> ApprovalDecision {
        if environment_bypasses_host_guards(environment) {
            return ApprovalDecision::Approved;
        }

        if hardline_reason(command, sudo_password_configured).is_some() {
            return ApprovalDecision::Denied;
        }

        // Check denied patterns first (built-in then custom)
        for re in DENIED_PATTERNS.iter() {
            if re.is_match(command) {
                return ApprovalDecision::Denied;
            }
        }
        for re in &self.denied_patterns {
            if re.is_match(command) {
                return ApprovalDecision::Denied;
            }
        }

        if yolo_mode {
            return ApprovalDecision::Approved;
        }

        let normalized = collapse_command(command);
        if delete_without_where(&normalized) {
            return ApprovalDecision::RequiresConfirmation;
        }

        // Check confirm patterns (built-in then custom)
        for re in CONFIRM_PATTERNS.iter() {
            if re.is_match(&normalized) {
                return ApprovalDecision::RequiresConfirmation;
            }
        }
        for re in &self.confirm_patterns {
            if re.is_match(&normalized) {
                return ApprovalDecision::RequiresConfirmation;
            }
        }

        ApprovalDecision::Approved
    }

    /// Async version of check_approval (same logic, for trait compatibility).
    pub async fn check_approval_async(&self, command: &str) -> ApprovalDecision {
        self.check_approval(command)
    }
}

impl Default for ApprovalManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience function: check if a command requires approval.
pub fn check_approval(command: &str) -> ApprovalDecision {
    let manager = ApprovalManager::new();
    manager.check_approval(command)
}

#[cfg(test)]
mod tests;
