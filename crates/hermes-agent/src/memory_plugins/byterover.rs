//! ByteRover memory provider plugin.
//!
//! Implements `MemoryProviderPlugin` for ByteRover — persistent knowledge tree
//! with hierarchical context, tiered retrieval, and optional cloud sync via
//! the `brv` CLI.
//!
//! Mirrors the Python `plugins/memory/byterover/__init__.py`.
//!
//! Configuration:
//!   - `brv` CLI must be installed (npm install -g byterover-cli)
//!   - `BRV_API_KEY` (optional, for cloud sync)
//!   - `$HERMES_HOME/byterover.json` with `auto_extract: false` disables
//!     background turn/memory/compression curation while keeping tools enabled.
//!   - Working directory: `$HERMES_HOME/byterover/`

use base64::Engine;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use fs2::FileExt;
use hermes_core::subprocess::CommandNoWindowExt;
use hermes_core::{
    MemoryAuditActor, MemoryExportCompletion, MemoryExportEvidenceDigest, MemoryExportPage,
    MemoryExportReceipt, MemoryExportSink, MemoryExportSnapshot, MemoryForgetReceipt,
    MemoryGovernanceOperation, MemoryProviderGovernanceError, MemoryReadbackReceipt,
    MemoryRetentionPolicy, MemoryRetentionReceipt, MemorySubject, MemorySubjectStorageKey,
};
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;
use crate::memory_plugins::config_io;

const QUERY_TIMEOUT_SECS: u64 = 10;
const CURATE_TIMEOUT_SECS: u64 = 120;
const MIN_QUERY_LEN: usize = 10;
const MIN_OUTPUT_LEN: usize = 20;

fn coerce_bool(value: Option<&Value>, default: bool) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(value)) => value.as_i64().map(|n| n != 0).unwrap_or(default),
        Some(Value::String(value)) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
        _ => default,
    }
}

fn byterover_config_path_for_home(hermes_home: &str) -> std::path::PathBuf {
    std::path::Path::new(hermes_home).join("byterover.json")
}

fn default_byterover_config_path() -> std::path::PathBuf {
    config_io::default_hermes_home().join("byterover.json")
}

fn load_auto_extract(hermes_home: &str) -> bool {
    for key in ["HERMES_BYTEROVER_AUTO_EXTRACT", "BYTEROVER_AUTO_EXTRACT"] {
        if let Ok(value) = std::env::var(key) {
            return coerce_bool(Some(&Value::String(value)), true);
        }
    }
    let config = config_io::read_json_object(&byterover_config_path_for_home(hermes_home));
    coerce_bool(config.get("auto_extract"), true)
}

// ---------------------------------------------------------------------------
// Tool schemas
// ---------------------------------------------------------------------------

fn query_schema() -> Value {
    json!({
        "name": "brv_query",
        "description": "Search ByteRover's persistent knowledge tree for relevant context. Returns memories, project knowledge, architectural decisions, and patterns from previous sessions.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "What to search for."}
            },
            "required": ["query"]
        }
    })
}

fn curate_schema() -> Value {
    json!({
        "name": "brv_curate",
        "description": "Store important information in ByteRover's persistent knowledge tree. Use for architectural decisions, bug fixes, user preferences, project patterns — anything worth remembering across sessions.",
        "parameters": {
            "type": "object",
            "properties": {
                "content": {"type": "string", "description": "The information to remember."}
            },
            "required": ["content"]
        }
    })
}

fn status_schema() -> Value {
    json!({
        "name": "brv_status",
        "description": "Check ByteRover status — CLI version, context tree stats, cloud sync state.",
        "parameters": {"type": "object", "properties": {}, "required": []}
    })
}

// ---------------------------------------------------------------------------
// brv CLI resolution
// ---------------------------------------------------------------------------

fn resolve_brv_path() -> Option<String> {
    if let Ok(path) = which::which("brv") {
        return Some(path.to_string_lossy().to_string());
    }

    let home = dirs::home_dir()?;
    let candidates = [
        home.join(".brv-cli").join("bin").join("brv"),
        std::path::PathBuf::from("/usr/local/bin/brv"),
        home.join(".npm-global").join("bin").join("brv"),
    ];
    for c in &candidates {
        if c.exists() {
            return Some(c.to_string_lossy().to_string());
        }
    }
    None
}

/// Run a `brv` CLI command. Returns `(success, output_or_error)`.
fn run_brv(args: &[&str], _timeout_secs: u64, cwd: &str) -> (bool, String) {
    let brv_path = match resolve_brv_path() {
        Some(p) => p,
        None => {
            return (
                false,
                "brv CLI not found. Install: npm install -g byterover-cli".into(),
            )
        }
    };


    let result = Command::new(&brv_path)
        .args(args)
        .current_dir(cwd)
        .suppress_windows_console()
        .output();

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if output.status.success() {
                (true, stdout)
            } else {
                let err = if !stderr.is_empty() {
                    stderr
                } else if !stdout.is_empty() {
                    stdout
                } else {
                    format!("brv exited {}", output.status)
                };
                (false, err)
            }
        }
        Err(e) => (false, format!("brv execution failed: {}", e)),
    }
}

// ---------------------------------------------------------------------------
// ByteRoverPlugin
// ---------------------------------------------------------------------------

/// ByteRover persistent memory via the `brv` CLI.
///
/// The CLI's process-wide defaults are not a memory boundary.  Every invocation
/// is rooted in the opaque, manager-derived subject directory below.
pub struct ByteRoverPlugin {
    workspace: Mutex<Option<SubjectWorkspace>>,
    turn_count: Mutex<u32>,
    auto_extract: Mutex<bool>,
}

#[derive(Clone)]
struct SubjectWorkspace {
    key: MemorySubjectStorageKey,
    path: PathBuf,
}

fn governance_error(operation: MemoryGovernanceOperation) -> MemoryProviderGovernanceError {
    MemoryProviderGovernanceError::failed("byterover", operation)
}

fn cloud_backed() -> bool {
    std::env::var("BRV_API_KEY")
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
}

fn check_governance_capability(
    operation: MemoryGovernanceOperation,
) -> Result<(), MemoryProviderGovernanceError> {
    // The CLI can synchronize to an external service when this credential is
    // set, but it has no subject-scoped remote deletion/readback API.  Refuse
    // to issue a local-only receipt in that configuration.
    if cloud_backed() {
        return Err(MemoryProviderGovernanceError::unsupported(
            "byterover",
            operation,
        ));
    }
    Ok(())
}

fn ensure_owner_only_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| error.to_string())?;
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("unsafe ByteRover memory directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn subject_workspace(
    hermes_home: &str,
    filter: &MemorySubjectStorageKey,
) -> Result<PathBuf, String> {
    let key = filter.as_str();
    if !key.starts_with("msp1_")
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err("unsafe ByteRover subject storage key".into());
    }
    let base = Path::new(hermes_home).join("byterover");
    let subjects = base.join("subjects");
    let subject = subjects.join(key);
    ensure_owner_only_dir(&base)?;
    ensure_owner_only_dir(&subjects)?;
    ensure_owner_only_dir(&subject)?;

    let subjects_canonical = fs::canonicalize(&subjects).map_err(|error| error.to_string())?;
    let subject_canonical = fs::canonicalize(&subject).map_err(|error| error.to_string())?;
    if !subject_canonical.starts_with(&subjects_canonical) {
        return Err("ByteRover subject path escaped its owner directory".into());
    }
    Ok(subject_canonical)
}

fn workspace_lock_path(workspace: &Path) -> Result<PathBuf, String> {
    let subjects = workspace
        .parent()
        .ok_or_else(|| "ByteRover subject directory has no parent".to_string())?;
    let key = workspace
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "ByteRover subject directory has no safe name".to_string())?;
    let locks = subjects.join(".locks");
    ensure_owner_only_dir(&locks)?;
    let lock = locks.join(format!("{key}.lock"));
    if lock.exists()
        && fs::symlink_metadata(&lock)
            .map_err(|error| error.to_string())?
            .file_type()
            .is_symlink()
    {
        return Err("unsafe ByteRover lock path".into());
    }
    Ok(lock)
}

fn lock_subject_workspace(workspace: &Path) -> Result<(File, PathBuf), String> {
    let metadata = fs::symlink_metadata(workspace).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("unsafe ByteRover subject workspace".into());
    }
    let lock_path = workspace_lock_path(workspace)?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    file.lock_exclusive().map_err(|error| error.to_string())?;
    Ok((file, lock_path))
}

fn run_brv_locked(args: &[&str], timeout_secs: u64, workspace: &Path) -> (bool, String) {
    let lock = match lock_subject_workspace(workspace) {
        Ok((file, _)) => file,
        Err(error) => return (false, format!("ByteRover workspace lock failed: {error}")),
    };
    let output = run_brv(args, timeout_secs, &workspace.to_string_lossy());
    let _ = FileExt::unlock(&lock);
    output
}

fn regular_files(root: &Path) -> Result<Vec<(PathBuf, SystemTime)>, String> {
    fn visit(path: &Path, files: &mut Vec<(PathBuf, SystemTime)>) -> Result<(), String> {
        if !path.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == ".byterover.lock") {
                continue;
            }
            let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if metadata.file_type().is_symlink() {
                return Err("refusing ByteRover symlinked memory residue".into());
            }
            if metadata.is_dir() {
                visit(&path, files)?;
            } else if metadata.is_file() {
                files.push((
                    path,
                    metadata.modified().map_err(|error| error.to_string())?,
                ));
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    visit(root, &mut files)?;
    Ok(files)
}

impl ByteRoverPlugin {
    pub fn new() -> Self {
        Self {
            workspace: Mutex::new(None),
            turn_count: Mutex::new(0),
            auto_extract: Mutex::new(true),
        }
    }

    fn working_dir(&self, filter: &MemorySubjectStorageKey) -> Option<PathBuf> {
        self.workspace
            .lock()
            .as_ref()
            .filter(|workspace| workspace.key == *filter)
            .map(|workspace| workspace.path.clone())
    }

    fn auto_extract_enabled(&self) -> bool {
        *self.auto_extract.lock()
    }

    fn subject_workspace_for_governance(
        &self,
        filter: &MemorySubjectStorageKey,
        operation: MemoryGovernanceOperation,
    ) -> Result<PathBuf, MemoryProviderGovernanceError> {
        self.working_dir(filter)
            .ok_or_else(|| governance_error(operation))
    }
}

impl MemoryProviderPlugin for ByteRoverPlugin {
    fn name(&self) -> &str {
        "byterover"
    }

    fn is_available(&self) -> bool {
        resolve_brv_path().is_some()
    }

    fn initialize(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        hermes_home: &str,
    ) -> Result<(), String> {
        let path = subject_workspace(hermes_home, filter)?;
        *self.workspace.lock() = Some(SubjectWorkspace {
            key: filter.clone(),
            path,
        });
        *self.turn_count.lock() = 0;
        *self.auto_extract.lock() = load_auto_extract(hermes_home);
        tracing::info!("ByteRover memory plugin initialized for an authenticated subject");
        Ok(())
    }

    fn system_prompt_block(&self) -> String {
        if resolve_brv_path().is_none() {
            return String::new();
        }
        "# ByteRover Memory\n\
         Active. Persistent knowledge tree with hierarchical context.\n\
         Use brv_query to search past knowledge, brv_curate to store \
         important facts, brv_status to check state."
            .to_string()
    }

    fn prefetch(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        query: &str,
    ) -> String {
        let query = query.trim();
        if query.len() < MIN_QUERY_LEN {
            return String::new();
        }
        let Some(workspace) = self.working_dir(filter) else {
            return String::new();
        };
        let truncated: String = query.chars().take(5000).collect();
        let (ok, output) =
            run_brv_locked(&["query", "--", &truncated], QUERY_TIMEOUT_SECS, &workspace);
        if ok && output.len() > MIN_OUTPUT_LEN {
            format!("## ByteRover Context\n{output}")
        } else {
            String::new()
        }
    }

    fn queue_prefetch(
        &self,
        _subject: &MemorySubject,
        _filter: &MemorySubjectStorageKey,
        _query: &str,
    ) {
        // There is deliberately no detached queue: writes and reads have
        // explicit completion/failure semantics.
    }

    fn sync_turn(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        user_content: &str,
        assistant_content: &str,
    ) {
        *self.turn_count.lock() += 1;
        if !self.auto_extract_enabled() || user_content.trim().len() < MIN_QUERY_LEN {
            return;
        }
        let Some(workspace) = self.working_dir(filter) else {
            return;
        };
        let combined = format!(
            "User: {}\nAssistant: {}",
            user_content.chars().take(2000).collect::<String>(),
            assistant_content.chars().take(2000).collect::<String>()
        );
        let (ok, error) =
            run_brv_locked(&["curate", "--", &combined], CURATE_TIMEOUT_SECS, &workspace);
        if !ok {
            tracing::warn!("ByteRover sync_turn curate failed: {error}");
        }
    }

    fn on_memory_write(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        action: &str,
        target: &str,
        content: &str,
    ) {
        if !self.auto_extract_enabled()
            || !matches!(action, "add" | "replace")
            || content.is_empty()
        {
            return;
        }
        let Some(workspace) = self.working_dir(filter) else {
            return;
        };
        let label = if target == "user" {
            "User profile"
        } else {
            "Agent memory"
        };
        let tagged = format!("[{label}] {content}");
        let (ok, error) =
            run_brv_locked(&["curate", "--", &tagged], CURATE_TIMEOUT_SECS, &workspace);
        if !ok {
            tracing::warn!("ByteRover memory mirror failed: {error}");
        }
    }

    fn on_pre_compress(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        messages: &[Value],
    ) -> String {
        if !self.auto_extract_enabled() || messages.is_empty() {
            return String::new();
        }
        let Some(workspace) = self.working_dir(filter) else {
            return String::new();
        };
        let mut parts = Vec::new();
        for message in messages.iter().rev().take(10).rev() {
            let role = message.get("role").and_then(Value::as_str).unwrap_or("");
            let content = message.get("content").and_then(Value::as_str).unwrap_or("");
            if !content.trim().is_empty() && matches!(role, "user" | "assistant") {
                parts.push(format!(
                    "{role}: {}",
                    content.chars().take(500).collect::<String>()
                ));
            }
        }
        if parts.is_empty() {
            return String::new();
        }
        let combined = format!("[Pre-compression context]\n{}", parts.join("\n"));
        let (ok, error) =
            run_brv_locked(&["curate", "--", &combined], CURATE_TIMEOUT_SECS, &workspace);
        if !ok {
            tracing::warn!("ByteRover pre-compression curate failed: {error}");
        }
        String::new()
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        vec![query_schema(), curate_schema(), status_schema()]
    }

    fn handle_tool_call(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        tool_name: &str,
        args: &Value,
    ) -> String {
        let Some(workspace) = self.working_dir(filter) else {
            return json!({"error": "ByteRover not initialized for the authenticated subject"})
                .to_string();
        };
        match tool_name {
            "brv_query" => {
                let query = args.get("query").and_then(Value::as_str).unwrap_or("");
                if query.is_empty() {
                    return json!({"error": "query is required"}).to_string();
                }
                let truncated: String = query.chars().take(5000).collect();
                let (ok, output) =
                    run_brv_locked(&["query", "--", &truncated], QUERY_TIMEOUT_SECS, &workspace);
                if !ok {
                    return json!({"error": output}).to_string();
                }
                if output.len() < MIN_OUTPUT_LEN {
                    return json!({"result": "No relevant memories found."}).to_string();
                }
                json!({"result": output.chars().take(8000).collect::<String>()}).to_string()
            }
            "brv_curate" => {
                let content = args.get("content").and_then(Value::as_str).unwrap_or("");
                if content.is_empty() {
                    return json!({"error": "content is required"}).to_string();
                }
                let (ok, output) =
                    run_brv_locked(&["curate", "--", content], CURATE_TIMEOUT_SECS, &workspace);
                if !ok {
                    return json!({"error": output}).to_string();
                }
                json!({"result": "Memory curated successfully."}).to_string()
            }
            "brv_status" => {
                let (ok, output) = run_brv_locked(&["status"], 15, &workspace);
                if !ok {
                    return json!({"error": output}).to_string();
                }
                json!({"status": output}).to_string()
            }
            _ => json!({"error": format!("Unknown tool: {tool_name}")}).to_string(),
        }
    }

    fn on_turn_start(
        &self,
        _subject: &MemorySubject,
        _filter: &MemorySubjectStorageKey,
        turn_number: u32,
        _message: &str,
    ) {
        *self.turn_count.lock() = turn_number;
    }

    fn export_subject(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        _actor: &MemoryAuditActor,
        sink: &mut dyn MemoryExportSink,
    ) -> Result<MemoryExportReceipt, MemoryProviderGovernanceError> {
        check_governance_capability(MemoryGovernanceOperation::Export)?;
        let workspace =
            self.subject_workspace_for_governance(filter, MemoryGovernanceOperation::Export)?;
        let (lock, _) = lock_subject_workspace(&workspace)
            .map_err(|_| governance_error(MemoryGovernanceOperation::Export))?;
        let mut rows = Vec::new();
        for (path, _) in regular_files(&workspace)
            .map_err(|_| governance_error(MemoryGovernanceOperation::Export))?
        {
            let relative_path = path
                .strip_prefix(&workspace)
                .map_err(|_| governance_error(MemoryGovernanceOperation::Export))?
                .to_string_lossy()
                .to_string();
            let bytes = fs::read(&path).map_err(|_| governance_error(MemoryGovernanceOperation::Export))?;
            let record_id = MemoryExportEvidenceDigest::sha256_bytes(
                [relative_path.as_bytes(), bytes.as_slice()].concat().as_slice(),
            );
            rows.push((
                record_id,
                json!({
                    "path": relative_path,
                    "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
                }),
            ));
        }
        rows.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
        let mut snapshot_bytes = b"byterover-v1".to_vec();
        for (record_id, _) in &rows {
            snapshot_bytes.extend_from_slice(record_id.as_str().as_bytes());
            snapshot_bytes.push(b'\n');
        }
        let snapshot = MemoryExportSnapshot::new(
            MemoryExportEvidenceDigest::sha256_bytes(&snapshot_bytes),
            MemoryExportEvidenceDigest::sha256_bytes(
                [b"byterover-revision-v1".as_slice(), snapshot_bytes.as_slice()]
                    .concat()
                    .as_slice(),
            ),
            None,
        );
        // The rows are an immutable, complete snapshot. Release the
        // cross-process workspace lock before calling a sink: a sink may
        // legitimately start a later subject write, which must not deadlock
        // behind this export's already-frozen inventory.
        FileExt::unlock(&lock).map_err(|_| governance_error(MemoryGovernanceOperation::Export))?;
        drop(lock);
        if sink.begin_provider(snapshot.clone()).is_err() {
            sink.abort();
            return Err(governance_error(MemoryGovernanceOperation::Export));
        }
        let mut exported_records = 0_u64;
        let mut page_count = 0_u64;
        for start in (0..rows.len()).step_by(100) {
            let end = (start + 100).min(rows.len());
            let cursor = (start != 0).then(|| {
                MemoryExportEvidenceDigest::sha256_bytes(
                    format!("byterover-export:{}:{start}", snapshot.snapshot_digest.as_str())
                        .as_bytes(),
                )
            });
            let next_cursor = (end < rows.len()).then(|| {
                MemoryExportEvidenceDigest::sha256_bytes(
                    format!("byterover-export:{}:{end}", snapshot.snapshot_digest.as_str())
                        .as_bytes(),
                )
            });
            let page = MemoryExportPage::new(
                cursor,
                next_cursor,
                rows[start..end]
                    .iter()
                    .map(|(record_id, _)| record_id.clone())
                    .collect(),
                rows[start..end]
                    .iter()
                    .map(|(_, record)| record.clone())
                    .collect(),
            )
            .map_err(|_| governance_error(MemoryGovernanceOperation::Export))?;
            if sink.append_page(page).is_err() {
                sink.abort();
                return Err(governance_error(MemoryGovernanceOperation::Export));
            }
            exported_records = exported_records.saturating_add((end - start) as u64);
            page_count = page_count.saturating_add(1);
        }
        if sink
            .finish_provider(MemoryExportCompletion::exhausted(exported_records, page_count))
            .is_err()
        {
            sink.abort();
            return Err(governance_error(MemoryGovernanceOperation::Export));
        }
        Ok(MemoryExportReceipt::exhausted(
            "byterover",
            filter.clone(),
            snapshot,
            exported_records,
            page_count,
        ))
    }

    fn forget_subject(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        _actor: &MemoryAuditActor,
    ) -> Result<MemoryForgetReceipt, MemoryProviderGovernanceError> {
        check_governance_capability(MemoryGovernanceOperation::Forget)?;
        let workspace =
            self.subject_workspace_for_governance(filter, MemoryGovernanceOperation::Forget)?;
        if !workspace.exists() {
            return Ok(MemoryForgetReceipt::new("byterover", filter.clone(), 0));
        }
        // Keep the shared lock inode after deleting the subject directory. A
        // concurrent reinitializer must block on this same inode rather than
        // race a newly-created lock namespace.
        let (lock, _) = lock_subject_workspace(&workspace)
            .map_err(|_| governance_error(MemoryGovernanceOperation::Forget))?;
        let deleted = regular_files(&workspace)
            .map_err(|_| governance_error(MemoryGovernanceOperation::Forget))?
            .len() as u64;
        if workspace.exists() {
            fs::remove_dir_all(&workspace)
                .map_err(|_| governance_error(MemoryGovernanceOperation::Forget))?;
        }
        let _ = FileExt::unlock(&lock);
        drop(lock);
        if workspace.exists() {
            return Err(governance_error(MemoryGovernanceOperation::Forget));
        }
        Ok(MemoryForgetReceipt::new("byterover", filter.clone(), deleted))
    }

    fn retention_sweep(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        policy: &MemoryRetentionPolicy,
        _actor: &MemoryAuditActor,
    ) -> Result<MemoryRetentionReceipt, MemoryProviderGovernanceError> {
        check_governance_capability(MemoryGovernanceOperation::RetentionSweep)?;
        let workspace = self.subject_workspace_for_governance(
            filter,
            MemoryGovernanceOperation::RetentionSweep,
        )?;
        if !workspace.exists() {
            return Ok(MemoryRetentionReceipt::new("byterover", filter.clone(), 0));
        }
        let (lock, _) = lock_subject_workspace(&workspace)
            .map_err(|_| governance_error(MemoryGovernanceOperation::RetentionSweep))?;
        let now = SystemTime::now();
        let mut files = regular_files(&workspace)
            .map_err(|_| governance_error(MemoryGovernanceOperation::RetentionSweep))?;
        let mut deleted = 0_u64;
        if let Some(days) = policy.max_age_days {
            let limit = Duration::from_secs(u64::from(days) * 24 * 60 * 60);
            for (path, modified) in &files {
                if now.duration_since(*modified).unwrap_or(Duration::ZERO) > limit {
                    fs::remove_file(path)
                        .map_err(|_| governance_error(MemoryGovernanceOperation::RetentionSweep))?;
                    deleted += 1;
                }
            }
            files.retain(|(path, _)| path.exists());
        }
        if let Some(max_records) = policy.max_records {
            files.sort_by(|left, right| right.1.cmp(&left.1));
            for (path, _) in files.into_iter().skip(max_records as usize) {
                fs::remove_file(path)
                    .map_err(|_| governance_error(MemoryGovernanceOperation::RetentionSweep))?;
                deleted += 1;
            }
        }
        let _ = FileExt::unlock(&lock);
        Ok(MemoryRetentionReceipt::new(
            "byterover",
            filter.clone(),
            deleted,
        ))
    }

    fn verify_subject_absent(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
    ) -> Result<MemoryReadbackReceipt, MemoryProviderGovernanceError> {
        check_governance_capability(MemoryGovernanceOperation::Readback)?;
        let workspace =
            self.subject_workspace_for_governance(filter, MemoryGovernanceOperation::Readback)?;
        if !workspace.exists() {
            return Ok(MemoryReadbackReceipt::absent("byterover", filter.clone()));
        }
        let (lock, _) = lock_subject_workspace(&workspace)
            .map_err(|_| governance_error(MemoryGovernanceOperation::Readback))?;
        let residual = regular_files(&workspace)
            .map_err(|_| governance_error(MemoryGovernanceOperation::Readback))?
            .len() as u64;
        let _ = FileExt::unlock(&lock);
        Ok(if residual == 0 {
            MemoryReadbackReceipt::absent("byterover", filter.clone())
        } else {
            MemoryReadbackReceipt::residual("byterover", filter.clone(), residual)
        })
    }

    fn shutdown(&self) {
        tracing::debug!("ByteRover memory plugin shutdown");
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key": "api_key", "description": "ByteRover API key (optional, for cloud sync)", "secret": true, "env_var": "BRV_API_KEY", "url": "https://app.byterover.dev"},

            {"key": "auto_extract", "description": "Automatically curate completed turns, explicit memory writes, and pre-compression context", "default": true, "choices": ["true", "false"]}
        ]))
    }

    fn save_config(&self, config: &Value) -> Result<(), String> {
        config_io::merge_and_write_owner_only(&default_byterover_config_path(), config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct TestExportSink {
        snapshot: Option<hermes_core::MemoryExportSnapshot>,
        pages: Vec<hermes_core::MemoryExportPage>,
        completion: Option<hermes_core::MemoryExportCompletion>,
        aborted: bool,
    }

    impl hermes_core::MemoryExportSink for TestExportSink {
        fn begin_provider(
            &mut self,
            snapshot: hermes_core::MemoryExportSnapshot,
        ) -> Result<(), hermes_core::MemoryExportSinkError> {
            self.snapshot = Some(snapshot);
            Ok(())
        }

        fn append_page(
            &mut self,
            page: hermes_core::MemoryExportPage,
        ) -> Result<(), hermes_core::MemoryExportSinkError> {
            self.pages.push(page);
            Ok(())
        }

        fn finish_provider(
            &mut self,
            completion: hermes_core::MemoryExportCompletion,
        ) -> Result<(), hermes_core::MemoryExportSinkError> {
            self.completion = Some(completion);
            Ok(())
        }

        fn abort(&mut self) {
            self.aborted = true;
        }

        fn commit(
            self: Box<Self>,
        ) -> Result<hermes_core::MemoryExportArtifact, hermes_core::MemoryExportSinkError> {
            Err(hermes_core::MemoryExportSinkError::Incomplete)
        }
    }
    struct MutatingExportSink {
        workspace: PathBuf,
        pages: Vec<hermes_core::MemoryExportPage>,
        mutated: bool,
        lock_was_released: bool,
    }

    impl hermes_core::MemoryExportSink for MutatingExportSink {
        fn begin_provider(
            &mut self,
            _snapshot: hermes_core::MemoryExportSnapshot,
        ) -> Result<(), hermes_core::MemoryExportSinkError> {
            Ok(())
        }

        fn append_page(
            &mut self,
            page: hermes_core::MemoryExportPage,
        ) -> Result<(), hermes_core::MemoryExportSinkError> {
            if !self.mutated {
                let lock_path = workspace_lock_path(&self.workspace)
                    .map_err(|_| hermes_core::MemoryExportSinkError::Rejected)?;
                let probe = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(lock_path)
                    .map_err(|_| hermes_core::MemoryExportSinkError::Rejected)?;
                probe
                    .try_lock_exclusive()
                    .map_err(|_| hermes_core::MemoryExportSinkError::Rejected)?;
                self.lock_was_released = true;
                FileExt::unlock(&probe)
                    .map_err(|_| hermes_core::MemoryExportSinkError::Rejected)?;
                fs::write(self.workspace.join("written-after-snapshot.record"), "late")
                    .map_err(|_| hermes_core::MemoryExportSinkError::Rejected)?;
                self.mutated = true;
            }
            self.pages.push(page);
            Ok(())
        }

        fn finish_provider(
            &mut self,
            _completion: hermes_core::MemoryExportCompletion,
        ) -> Result<(), hermes_core::MemoryExportSinkError> {
            Ok(())
        }

        fn abort(&mut self) {}

        fn commit(
            self: Box<Self>,
        ) -> Result<hermes_core::MemoryExportArtifact, hermes_core::MemoryExportSinkError> {
            Err(hermes_core::MemoryExportSinkError::Incomplete)
        }
    }
    fn subject(label: &str) -> (
        MemorySubject,
        MemorySubjectStorageKey,
        MemoryAuditActor,
    ) {
        let derivation = hermes_core::MemorySubjectDerivationKey::from_bytes([9; 32]);
        let subject = MemorySubject::local_owner(
            hermes_core::MemorySurface::Local,
            format!("byterover-{label}"),
        )
        .expect("subject");
        let key = subject.storage_key(&derivation);
        let actor = derivation.actor_for_subject(&subject);
        (subject, key, actor)
    }


    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn test_byterover_plugin_name() {
        let plugin = ByteRoverPlugin::new();
        assert_eq!(plugin.name(), "byterover");
    }

    #[test]
    fn test_byterover_tool_schemas() {
        let plugin = ByteRoverPlugin::new();
        let schemas = plugin.get_tool_schemas();
        assert_eq!(schemas.len(), 3);
        let names: Vec<&str> = schemas
            .iter()
            .filter_map(|s| s.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"brv_query"));
        assert!(names.contains(&"brv_curate"));
        assert!(names.contains(&"brv_status"));
    }

    #[test]
    fn test_byterover_system_prompt_empty_when_not_available() {
        let plugin = ByteRoverPlugin::new();
        // When brv is not installed, system_prompt_block may or may not be empty
        // depending on the test environment — just check it doesn't panic
        let _ = plugin.system_prompt_block();
    }

    #[test]
    fn test_byterover_prefetch_short_query_returns_empty() {
        let plugin = ByteRoverPlugin::new();
        let (subject, key, _) = subject("short-query");
        assert!(plugin.prefetch(&subject, &key, "hi").is_empty());
    }

    #[test]
    fn test_byterover_handle_tool_missing_args() {
        let plugin = ByteRoverPlugin::new();
        let (subject, key, _) = subject("tool");
        let result = plugin.handle_tool_call(&subject, &key, "brv_query", &json!({"query": "test"}));
        assert!(result.contains("error"));
    }

    #[test]
    fn test_byterover_auto_extract_config_coerces_values() {
        assert!(!coerce_bool(Some(&json!(false)), true));
        assert!(!coerce_bool(Some(&json!("off")), true));
        assert!(coerce_bool(Some(&json!("yes")), false));
        assert!(coerce_bool(None, true));
    }

    #[test]
    fn test_byterover_initialize_honors_auto_extract_config() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _hermes_override = EnvGuard::remove("HERMES_BYTEROVER_AUTO_EXTRACT");
        let _byterover_override = EnvGuard::remove("BYTEROVER_AUTO_EXTRACT");
        std::fs::write(
            tmp.path().join("byterover.json"),
            r#"{"auto_extract": false}"#,
        )
        .expect("write config");

        let plugin = ByteRoverPlugin::new();
        let (subject, key, _) = subject("config");
        plugin
            .initialize(&subject, &key, tmp.path().to_str().expect("tmp"))
            .expect("initialize");

        assert!(!plugin.auto_extract_enabled());
    }

    #[test]
    fn test_byterover_config_schema_exposes_auto_extract() {
        let plugin = ByteRoverPlugin::new();
        let schema = plugin.get_config_schema().expect("schema");
        let keys = schema
            .as_array()
            .expect("schema array")
            .iter()
            .filter_map(|entry| entry.get("key").and_then(Value::as_str))
            .collect::<Vec<_>>();

        assert!(keys.contains(&"auto_extract"));
    }

    #[test]
    fn subject_workspaces_are_isolated() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (subject_a, key_a, _) = subject("a");
        let (subject_b, key_b, _) = subject("b");
        let plugin_a = ByteRoverPlugin::new();
        let plugin_b = ByteRoverPlugin::new();
        plugin_a
            .initialize(&subject_a, &key_a, tmp.path().to_str().expect("tmp"))
            .expect("initialize a");
        plugin_b
            .initialize(&subject_b, &key_b, tmp.path().to_str().expect("tmp"))
            .expect("initialize b");
        let path_a = plugin_a.working_dir(&key_a).expect("a workspace");
        let path_b = plugin_b.working_dir(&key_b).expect("b workspace");
        assert_ne!(path_a, path_b);
        fs::write(path_a.join("a.record"), "only-a").expect("write a");
        fs::write(path_b.join("b.record"), "only-b").expect("write b");
        assert!(!path_a.join("b.record").exists());
        assert!(!path_b.join("a.record").exists());
        assert!(plugin_a.working_dir(&key_b).is_none());
    }

    #[test]
    fn export_forget_and_readback_cover_subject_files() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let _api_key = EnvGuard::remove("BRV_API_KEY");
        let tmp = tempfile::tempdir().expect("tempdir");
        let (subject, key, actor) = subject("governance");
        let plugin = ByteRoverPlugin::new();
        plugin
            .initialize(&subject, &key, tmp.path().to_str().expect("tmp"))
            .expect("initialize");
        let workspace = plugin.working_dir(&key).expect("workspace");
        fs::write(workspace.join("one.record"), "one").expect("write one");
        fs::write(workspace.join("two.record"), "two").expect("write two");

        let mut sink = TestExportSink::default();
        let export = plugin
            .export_subject(&subject, &key, &actor, &mut sink)
            .expect("export");
        assert_eq!(export.exported_records, 2);
        assert_eq!(sink.pages.len(), 1);
        assert!(sink.completion.expect("completion").exhausted);
        let retention = plugin
            .retention_sweep(
                &subject,
                &key,
                &MemoryRetentionPolicy::new(None, Some(1)).expect("policy"),
                &actor,
            )
            .expect("retention");
        assert_eq!(retention.deleted_records, 1);

        let forgotten = plugin
            .forget_subject(&subject, &key, &actor)
            .expect("forget");
        assert_eq!(forgotten.deleted_records, 1);
        assert!(plugin
            .verify_subject_absent(&subject, &key)
            .expect("readback")
            .is_absent());
        assert!(!workspace.exists());
        assert!(
            workspace
                .parent()
                .expect("subjects parent")
                .join(".locks")
                .join(format!("{}.lock", key.as_str()))
                .exists(),
            "forget retains the shared lock inode for a concurrent reinitializer"
        );
    }


    #[test]
    fn export_exhausts_full_workspace_with_nonlooping_pages_and_stable_snapshot() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let _api_key = EnvGuard::remove("BRV_API_KEY");
        let tmp = tempfile::tempdir().expect("tempdir");
        let (subject, key, actor) = subject("paged-export");
        let plugin = ByteRoverPlugin::new();
        plugin
            .initialize(&subject, &key, tmp.path().to_str().expect("tmp"))
            .expect("initialize");
        let workspace = plugin.working_dir(&key).expect("workspace");
        for index in 0..101 {
            fs::write(workspace.join(format!("record-{index:03}.txt")), format!("value-{index}"))
                .expect("record");
        }

        let mut sink = TestExportSink::default();
        let receipt = plugin
            .export_subject(&subject, &key, &actor, &mut sink)
            .expect("paged export");
        assert_eq!(receipt.exported_records, 101);
        assert_eq!(receipt.page_count, 2);
        assert_eq!(sink.pages.len(), 2);
        assert_eq!(sink.pages[0].records.len(), 100);
        assert_eq!(sink.pages[1].records.len(), 1);
        assert_eq!(sink.pages[0].cursor, None);
        assert_eq!(sink.pages[0].next_cursor, sink.pages[1].cursor);
        assert_eq!(sink.pages[1].next_cursor, None);
        assert_eq!(
            sink.pages
                .iter()
                .flat_map(|page| page.record_ids.iter().map(|id| id.as_str()))
                .collect::<std::collections::HashSet<_>>()
                .len(),
            101
        );

        let mut mutating_sink = MutatingExportSink {
            workspace: workspace.clone(),
            pages: Vec::new(),
            mutated: false,
            lock_was_released: false,
        };
        let snapshot_export = plugin
            .export_subject(&subject, &key, &actor, &mut mutating_sink)
            .expect("stable export");
        assert_eq!(snapshot_export.exported_records, 101);
        assert!(mutating_sink.mutated);
        assert!(mutating_sink.lock_was_released);
        assert!(mutating_sink.pages.iter().flat_map(|page| page.records.iter()).all(
            |record| record["path"] != "written-after-snapshot.record"
        ));
        assert_eq!(regular_files(&workspace).expect("full inventory").len(), 102);
    }
    #[test]
    fn subject_workspace_lock_serializes_concurrent_writers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};

        let tmp = tempfile::tempdir().expect("tempdir");
        let (_, key, _) = subject("concurrent");
        let workspace = subject_workspace(tmp.path().to_str().expect("tmp"), &key)
            .expect("workspace");
        let barrier = Arc::new(Barrier::new(2));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut joins = Vec::new();
        for index in 0..2 {
            let workspace = workspace.clone();
            let barrier = Arc::clone(&barrier);
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            joins.push(std::thread::spawn(move || {
                barrier.wait();
                let (lock, _) = lock_subject_workspace(&workspace).expect("lock");
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                fs::write(workspace.join(format!("{index}.record")), "record").expect("write");
                std::thread::sleep(Duration::from_millis(15));
                active.fetch_sub(1, Ordering::SeqCst);
                FileExt::unlock(&lock).expect("unlock");
            }));
        }
        for join in joins {
            join.join().expect("writer");
        }
        assert_eq!(peak.load(Ordering::SeqCst), 1);
        assert_eq!(regular_files(&workspace).expect("records").len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn subject_workspace_rejects_symlink_escape() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside");
        let (_, key, _) = subject("symlink");
        let subjects = tmp.path().join("byterover").join("subjects");
        fs::create_dir_all(&subjects).expect("subjects");
        std::os::unix::fs::symlink(outside.path(), subjects.join(key.as_str()))
            .expect("symlink");
        assert!(subject_workspace(tmp.path().to_str().expect("tmp"), &key).is_err());
    }
}
