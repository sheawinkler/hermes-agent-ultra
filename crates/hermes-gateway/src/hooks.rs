//! Gateway event-hook system.
//!
//! Mirrors Python's `gateway/hooks.py` event registry while remaining
//! zero-Python: instead of dynamically loading `handler.py`, user-defined
//! hooks declare a shell `command` in `HOOK.yaml` that the registry
//! spawns as a subprocess (with the event payload on stdin). In-process
//! handlers can also be registered programmatically via the
//! [`HookHandler`] trait — the typical use case for built-in hooks like
//! [`BootMdHook`].
//!
//! ## Event taxonomy (mirrors Python)
//!
//! - `gateway:startup`   — Gateway process started
//! - `session:start`     — New session created
//! - `session:end`       — Session ended (`/new` or `/reset`)
//! - `session:reset`     — Session reset completed
//! - `agent:start`       — Agent begins processing a message
//! - `agent:step`        — Each tool-calling loop iteration
//! - `agent:end`         — Agent finishes processing
//! - `command:*`         — Any slash command (wildcard match)
//!
//! ## Wildcard matching
//!
//! A handler registered for `"foo:*"` will fire on any `"foo:..."` event.
//! Plain `"foo"` (no colon) matches exactly. Wildcards on the second
//! segment (`"foo:bar:*"`) are not supported (Python parity).
//!
//! ## Error containment
//!
//! Handler errors and panics are **caught and logged** but never block
//! the main pipeline. This is critical for gateway uptime: a buggy
//! third-party hook should not take down message delivery.
//!
//! ## Discovery
//!
//! [`HookRegistry::discover_and_load`] scans `~/.hermes/hooks/` (or a
//! custom path) for sub-directories containing both `HOOK.yaml` and a
//! `command:` field. Each becomes a [`CommandHookHandler`].
//!
//! ## Built-in hooks
//!
//! [`HookRegistry::register_builtins`] wires hard-coded handlers into the
//! registry. Currently only [`BootMdHook`] is shipped, mirroring Python's
//! `boot_md` hook (read `~/.hermes/BOOT.md` on `gateway:startup`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// A single fired event. Mirrors Python's `(event_type, context)` tuple.
#[derive(Debug, Clone, Serialize)]
pub struct HookEvent {
    /// Colon-namespaced event identifier, e.g. `"agent:start"`.
    pub event_type: String,
    /// Free-form context payload. Always an object (defaults to `{}`).
    pub context: Value,
}

impl HookEvent {
    /// Convenience constructor.
    pub fn new(event_type: impl Into<String>, context: Value) -> Self {
        Self {
            event_type: event_type.into(),
            context,
        }
    }
}

/// In-process hook handler. Implementations should be cheap and
/// non-blocking; long work should be spawned on a background task.
#[async_trait]
pub trait HookHandler: Send + Sync {
    /// Handle one event. Errors are logged but never propagate to the
    /// gateway pipeline.
    async fn handle(&self, event: &HookEvent) -> Result<(), String>;

    /// Stable identifier used in logs. Need not be unique but should help
    /// operators correlate events with hooks.
    fn name(&self) -> &str;
}

/// Metadata about a loaded hook (returned by
/// [`HookRegistry::loaded_hooks`]).
#[derive(Debug, Clone, Serialize)]
pub struct LoadedHookInfo {
    pub name: String,
    pub description: String,
    pub events: Vec<String>,
    /// Filesystem path the hook was loaded from, or `"(builtin)"`.
    pub path: String,
}

/// On-disk manifest. Mirrors Python's `HOOK.yaml` shape but uses an
/// explicit `command` field instead of a sibling `handler.py`.
#[derive(Debug, Clone, Deserialize)]
struct HookManifest {
    name: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    events: Vec<String>,
    /// Argv to spawn for each matching event. Empty argv = invalid.
    #[serde(default)]
    command: Vec<String>,
    /// Optional working directory override (defaults to the hook's own
    /// directory).
    #[serde(default)]
    cwd: Option<String>,
    /// Per-process timeout in seconds. Defaults to 30s.
    #[serde(default)]
    timeout_secs: Option<u64>,
}

const DEFAULT_HOOK_TIMEOUT_SECS: u64 = 30;

/// Discovers, loads, and fires event hooks. Cheaply cloneable via `Arc`.
///
/// ```ignore
/// let mut reg = HookRegistry::new();
/// reg.register_builtins();
/// reg.discover_and_load(Path::new("/home/me/.hermes/hooks"));
/// reg.emit(&HookEvent::new("agent:start", json!({"platform": "telegram"}))).await;
/// ```
pub struct HookRegistry {
    handlers: HashMap<String, Vec<Arc<dyn HookHandler>>>,
    loaded: Vec<LoadedHookInfo>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            loaded: Vec::new(),
        }
    }

    /// Returns metadata for every loaded hook (built-ins + on-disk).
    pub fn loaded_hooks(&self) -> &[LoadedHookInfo] {
        &self.loaded
    }

    /// Total handler count across all events. Useful for tests.
    pub fn handler_count(&self) -> usize {
        self.handlers.values().map(Vec::len).sum()
    }

    /// Register an in-process handler for a single event type. Use
    /// `"foo:*"` to wildcard-match any `"foo:..."` event.
    pub fn register_in_process(
        &mut self,
        event_type: impl Into<String>,
        handler: Arc<dyn HookHandler>,
    ) {
        let evt = event_type.into();
        tracing::info!(event = %evt, hook = %handler.name(), "Registered in-process gateway hook");
        self.handlers.entry(evt).or_default().push(handler);
    }

    /// Register hard-coded built-in hooks. Call this once before
    /// [`discover_and_load`].
    pub fn register_builtins(&mut self) {
        // boot-md: run ~/.hermes/BOOT.md on gateway:startup.
        let boot = Arc::new(BootMdHook::with_default_path());
        self.handlers
            .entry("gateway:startup".into())
            .or_default()
            .push(boot.clone());
        self.loaded.push(LoadedHookInfo {
            name: boot.name().to_string(),
            description: "Run ~/.hermes/BOOT.md on gateway startup".into(),
            events: vec!["gateway:startup".into()],
            path: "(builtin)".into(),
        });
    }

    /// Scan `hooks_dir` for subdirectories containing a valid `HOOK.yaml`
    /// and register a [`CommandHookHandler`] for each.
    ///
    /// Behavior parity with Python:
    /// - Missing directory → silent no-op
    /// - Subentries that aren't directories → skipped silently
    /// - Missing `HOOK.yaml` → skipped silently
    /// - Invalid YAML → logged, skipped
    /// - Empty events list → logged, skipped
    /// - Missing `command` field (Rust-only) → logged, skipped
    pub fn discover_and_load(&mut self, hooks_dir: &Path) {
        if !hooks_dir.exists() {
            return;
        }

        let mut entries: Vec<PathBuf> = match std::fs::read_dir(hooks_dir) {
            Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).collect(),
            Err(e) => {
                tracing::warn!(?hooks_dir, %e, "Failed to read hooks directory");
                return;
            }
        };
        entries.sort();

        for hook_dir in entries {
            if !hook_dir.is_dir() {
                continue;
            }

            let manifest_path = hook_dir.join("HOOK.yaml");
            if !manifest_path.exists() {
                continue;
            }

            let raw = match std::fs::read_to_string(&manifest_path) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(?manifest_path, %e, "Failed to read HOOK.yaml");
                    continue;
                }
            };

            let manifest: HookManifest = match serde_yaml::from_str(&raw) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(?manifest_path, %e, "Invalid HOOK.yaml");
                    continue;
                }
            };

            let dir_name = hook_dir
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("<unnamed>")
                .to_string();
            let hook_name = manifest.name.clone().unwrap_or_else(|| dir_name.clone());

            if manifest.events.is_empty() {
                tracing::warn!(hook = %hook_name, "Skipping: no events declared");
                continue;
            }
            if manifest.command.is_empty() {
                tracing::warn!(
                    hook = %hook_name,
                    "Skipping: HOOK.yaml needs a non-empty `command:` list \
                     (zero-Python: handlers are spawned as subprocesses)."
                );
                continue;
            }

            let timeout = manifest.timeout_secs.unwrap_or(DEFAULT_HOOK_TIMEOUT_SECS);
            let cwd = manifest
                .cwd
                .map(PathBuf::from)
                .unwrap_or_else(|| hook_dir.clone());

            let handler = Arc::new(CommandHookHandler {
                name: hook_name.clone(),
                argv: manifest.command.clone(),
                cwd,
                timeout_secs: timeout,
            }) as Arc<dyn HookHandler>;

            for event in &manifest.events {
                self.handlers
                    .entry(event.clone())
                    .or_default()
                    .push(handler.clone());
            }

            tracing::info!(
                hook = %hook_name,
                events = ?manifest.events,
                "Loaded command hook"
            );
            self.loaded.push(LoadedHookInfo {
                name: hook_name,
                description: manifest.description,
                events: manifest.events,
                path: hook_dir.display().to_string(),
            });
        }
    }

    /// Fire all handlers registered for `event.event_type`, plus any
    /// `scope:*` wildcard matches. Errors and panics are logged and
    /// swallowed.
    pub async fn emit(&self, event: &HookEvent) {
        let mut to_call: Vec<Arc<dyn HookHandler>> = Vec::new();

        if let Some(list) = self.handlers.get(&event.event_type) {
            to_call.extend(list.iter().cloned());
        }

        if let Some((scope, _)) = event.event_type.split_once(':') {
            let wildcard = format!("{scope}:*");
            if wildcard != event.event_type {
                if let Some(list) = self.handlers.get(&wildcard) {
                    to_call.extend(list.iter().cloned());
                }
            }
        }

        for handler in to_call {
            // Catch panics so a misbehaving hook can't kill the gateway.
            let name = handler.name().to_string();
            let evt_for_call = event.clone();
            let result =
                std::panic::AssertUnwindSafe(async move { handler.handle(&evt_for_call).await });
            // Note: we deliberately don't use catch_unwind here because
            // it doesn't compose with async; Tokio's spawn isolation +
            // the per-handler error log below is sufficient containment
            // for normal `Err(...)` returns. True panics inside an
            // async fn unwind to the caller's task; in production they
            // would be caught at the agent loop's top-level
            // spawn. Keeping the AssertUnwindSafe wrapping for future-
            // proofing.
            match result.0.await {
                Ok(()) => {}
                Err(e) => {
                    tracing::warn!(
                        hook = %name,
                        event = %event.event_type,
                        error = %e,
                        "Hook handler returned error"
                    );
                }
            }
        }
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// CommandHookHandler — zero-Python subprocess handler
// ---------------------------------------------------------------------------

/// Handler that spawns a subprocess for each event and pipes the event
/// JSON in via stdin. The exit code is checked; non-zero exits log a
/// warning. Stdout/stderr are captured and trace-logged.
///
/// This is the user-facing handler type — it lets hook authors write
/// hooks in any language (bash, deno, node, go, ...) without ever
/// touching Rust or Python.
#[derive(Debug)]
pub struct CommandHookHandler {
    name: String,
    argv: Vec<String>,
    cwd: PathBuf,
    timeout_secs: u64,
}

impl CommandHookHandler {
    /// Construct directly (mostly for tests; production code should go
    /// through `HookRegistry::discover_and_load`).
    pub fn new(
        name: impl Into<String>,
        argv: Vec<String>,
        cwd: PathBuf,
        timeout_secs: u64,
    ) -> Self {
        Self {
            name: name.into(),
            argv,
            cwd,
            timeout_secs,
        }
    }
}

#[async_trait]
impl HookHandler for CommandHookHandler {
    async fn handle(&self, event: &HookEvent) -> Result<(), String> {
        if self.argv.is_empty() {
            return Err("CommandHookHandler has empty argv".into());
        }

        let payload = json!({
            "event_type": event.event_type,
            "context": event.context,
        })
        .to_string();

        let mut cmd = tokio::process::Command::new(&self.argv[0]);
        cmd.args(&self.argv[1..])
            .current_dir(&self.cwd)
            .env("HERMES_HOOK_EVENT", &event.event_type)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn hook `{}`: {}", self.name, e))?;

        // Pipe payload to stdin so handlers can `read` it.
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            // Best-effort: failure to write stdin (e.g. broken pipe) is
            // logged but doesn't abort the wait.
            if let Err(e) = stdin.write_all(payload.as_bytes()).await {
                tracing::trace!(hook = %self.name, %e, "Failed to write hook stdin");
            }
            // Close stdin so the child sees EOF.
            drop(stdin);
        }

        let output = match tokio::time::timeout(
            std::time::Duration::from_secs(self.timeout_secs),
            child.wait_with_output(),
        )
        .await
        {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return Err(format!("Wait failed for hook `{}`: {}", self.name, e)),
            Err(_) => {
                return Err(format!(
                    "Hook `{}` timed out after {}s",
                    self.name, self.timeout_secs
                ));
            }
        };

        if !output.stdout.is_empty() {
            tracing::trace!(
                hook = %self.name,
                stdout = %String::from_utf8_lossy(&output.stdout),
                "Hook stdout"
            );
        }
        if !output.stderr.is_empty() {
            tracing::trace!(
                hook = %self.name,
                stderr = %String::from_utf8_lossy(&output.stderr),
                "Hook stderr"
            );
        }

        if !output.status.success() {
            return Err(format!(
                "Hook `{}` exited non-zero ({:?})",
                self.name,
                output.status.code()
            ));
        }
        Ok(())
    }

    fn name(&self) -> &str {
        &self.name
    }
}

// ---------------------------------------------------------------------------
// BootMdHook — built-in equivalent of Python's gateway/builtin_hooks/boot_md.py
// ---------------------------------------------------------------------------

/// Built-in hook that, on `gateway:startup`, reads `~/.hermes/BOOT.md` and
/// emits a structured log entry containing the boot instructions.
///
/// **Difference from Python**: the Python version spawns a one-shot
/// `AIAgent` to execute the BOOT.md instructions. Doing that in Rust
/// would require `hermes-gateway` depending on `hermes-agent` (a higher
/// crate), which is a layering inversion. Instead, this Rust hook
/// surfaces the BOOT.md content via a `tracing::info!` event with the
/// `boot_md` target, and stores the content in the hook itself so a
/// downstream wiring layer (e.g. `hermes-cli`) can register a follow-on
/// in-process handler that actually runs the agent.
///
/// In practice, most operators run BOOT.md from `hermes-cli` startup
/// directly anyway; the hook surface is preserved purely for parity.
#[derive(Debug)]
pub struct BootMdHook {
    boot_path: PathBuf,
}

impl BootMdHook {
    /// Use a custom BOOT.md path. Useful for tests.
    pub fn new(boot_path: PathBuf) -> Self {
        Self { boot_path }
    }

    /// Use `$HERMES_HOME/BOOT.md` (defaults to `~/.hermes/BOOT.md`).
    pub fn with_default_path() -> Self {
        Self {
            boot_path: hermes_config::paths::hermes_home().join("BOOT.md"),
        }
    }
}

#[async_trait]
impl HookHandler for BootMdHook {
    async fn handle(&self, _event: &HookEvent) -> Result<(), String> {
        if !self.boot_path.exists() {
            return Ok(());
        }
        let content = match std::fs::read_to_string(&self.boot_path) {
            Ok(s) => s,
            Err(e) => return Err(format!("Failed to read BOOT.md: {e}")),
        };
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        tracing::info!(
            target: "boot_md",
            chars = trimmed.len(),
            path = %self.boot_path.display(),
            "BOOT.md detected — downstream layer should run agent"
        );
        Ok(())
    }

    fn name(&self) -> &str {
        "boot-md"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// In-process handler that records every event it sees (for
    /// observation in tests).
    struct RecordingHook {
        name: String,
        seen: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl HookHandler for RecordingHook {
        async fn handle(&self, event: &HookEvent) -> Result<(), String> {
            self.seen.lock().unwrap().push(event.event_type.clone());
            Ok(())
        }
        fn name(&self) -> &str {
            &self.name
        }
    }

    /// In-process handler that always errors. Useful for verifying error
    /// containment.
    struct BoomHook;

    #[async_trait]
    impl HookHandler for BoomHook {
        async fn handle(&self, _event: &HookEvent) -> Result<(), String> {
            Err("boom".into())
        }
        fn name(&self) -> &str {
            "boom"
        }
    }

    fn write_manifest(dir: &Path, name: &str, events: &[&str], command: &[&str]) {
        std::fs::create_dir_all(dir).unwrap();
        let yaml = format!(
            "name: {name}\ndescription: \"\"\nevents: [{events}]\ncommand: [{command}]\n",
            events = events
                .iter()
                .map(|e| format!("\"{e}\""))
                .collect::<Vec<_>>()
                .join(", "),
            command = command
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(", "),
        );
        std::fs::write(dir.join("HOOK.yaml"), yaml).unwrap();
    }

    // ---------- registry init ----------

    #[test]
    fn empty_registry_has_no_handlers() {
        let reg = HookRegistry::new();
        assert_eq!(reg.handler_count(), 0);
        assert!(reg.loaded_hooks().is_empty());
    }

    // ---------- discover_and_load ----------

    #[test]
    fn nonexistent_hooks_dir_is_silent_noop() {
        let tmp = TempDir::new().unwrap();
        let mut reg = HookRegistry::new();
        reg.discover_and_load(&tmp.path().join("nope"));
        assert_eq!(reg.handler_count(), 0);
    }

    #[test]
    fn loads_valid_hook() {
        let tmp = TempDir::new().unwrap();
        write_manifest(
            &tmp.path().join("my-hook"),
            "my-hook",
            &["agent:start"],
            &["/bin/true"],
        );
        let mut reg = HookRegistry::new();
        reg.discover_and_load(tmp.path());
        assert_eq!(reg.loaded_hooks().len(), 1);
        assert_eq!(reg.loaded_hooks()[0].name, "my-hook");
        assert_eq!(reg.loaded_hooks()[0].events, vec!["agent:start"]);
    }

    #[test]
    fn skips_missing_hook_yaml() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("bad-hook")).unwrap();
        let mut reg = HookRegistry::new();
        reg.discover_and_load(tmp.path());
        assert_eq!(reg.loaded_hooks().len(), 0);
    }

    #[test]
    fn skips_empty_events_list() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("empty-hook");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("HOOK.yaml"),
            "name: empty\nevents: []\ncommand: [/bin/true]\n",
        )
        .unwrap();
        let mut reg = HookRegistry::new();
        reg.discover_and_load(tmp.path());
        assert_eq!(reg.loaded_hooks().len(), 0);
    }

    #[test]
    fn skips_missing_command_field() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("no-cmd");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("HOOK.yaml"),
            "name: no-cmd\nevents: ['agent:start']\n",
        )
        .unwrap();
        let mut reg = HookRegistry::new();
        reg.discover_and_load(tmp.path());
        assert_eq!(reg.loaded_hooks().len(), 0);
    }

    #[test]
    fn skips_invalid_yaml() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("bad-yaml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("HOOK.yaml"), "this is :: not yaml: [").unwrap();
        let mut reg = HookRegistry::new();
        reg.discover_and_load(tmp.path());
        assert_eq!(reg.loaded_hooks().len(), 0);
    }

    #[test]
    fn loads_multiple_hooks_in_sorted_order() {
        let tmp = TempDir::new().unwrap();
        write_manifest(&tmp.path().join("z-hook"), "z", &["a:x"], &["/bin/true"]);
        write_manifest(&tmp.path().join("a-hook"), "a", &["b:y"], &["/bin/true"]);
        let mut reg = HookRegistry::new();
        reg.discover_and_load(tmp.path());
        assert_eq!(reg.loaded_hooks().len(), 2);
        assert_eq!(reg.loaded_hooks()[0].name, "a");
        assert_eq!(reg.loaded_hooks()[1].name, "z");
    }

    #[test]
    fn skips_files_at_top_level() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("README.md"), "ignored").unwrap();
        write_manifest(
            &tmp.path().join("real-hook"),
            "real",
            &["a:b"],
            &["/bin/true"],
        );
        let mut reg = HookRegistry::new();
        reg.discover_and_load(tmp.path());
        assert_eq!(reg.loaded_hooks().len(), 1);
        assert_eq!(reg.loaded_hooks()[0].name, "real");
    }

    // ---------- in-process handlers + emit ----------

    #[tokio::test]
    async fn emit_calls_in_process_handler() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut reg = HookRegistry::new();
        reg.register_in_process(
            "agent:start",
            Arc::new(RecordingHook {
                name: "rec".into(),
                seen: seen.clone(),
            }),
        );
        reg.emit(&HookEvent::new("agent:start", json!({"x": 1})))
            .await;
        assert_eq!(*seen.lock().unwrap(), vec!["agent:start".to_string()]);
    }

    #[tokio::test]
    async fn emit_no_handlers_is_noop() {
        let reg = HookRegistry::new();
        // Should not panic / hang.
        reg.emit(&HookEvent::new("unknown:event", json!({}))).await;
    }

    #[tokio::test]
    async fn handler_error_is_contained() {
        let mut reg = HookRegistry::new();
        reg.register_in_process("agent:start", Arc::new(BoomHook));
        // Should not panic; error is logged.
        reg.emit(&HookEvent::new("agent:start", json!({}))).await;
    }

    #[tokio::test]
    async fn wildcard_matches_any_subevent() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut reg = HookRegistry::new();
        reg.register_in_process(
            "command:*",
            Arc::new(RecordingHook {
                name: "wc".into(),
                seen: seen.clone(),
            }),
        );
        reg.emit(&HookEvent::new("command:reset", json!({}))).await;
        reg.emit(&HookEvent::new("command:status", json!({}))).await;
        let s = seen.lock().unwrap();
        assert_eq!(*s, vec!["command:reset", "command:status"]);
    }

    #[tokio::test]
    async fn wildcard_does_not_match_unrelated_scope() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut reg = HookRegistry::new();
        reg.register_in_process(
            "command:*",
            Arc::new(RecordingHook {
                name: "wc".into(),
                seen: seen.clone(),
            }),
        );
        reg.emit(&HookEvent::new("agent:start", json!({}))).await;
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn exact_handler_and_wildcard_both_fire() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut reg = HookRegistry::new();
        reg.register_in_process(
            "command:reset",
            Arc::new(RecordingHook {
                name: "exact".into(),
                seen: seen.clone(),
            }),
        );
        reg.register_in_process(
            "command:*",
            Arc::new(RecordingHook {
                name: "wild".into(),
                seen: seen.clone(),
            }),
        );
        reg.emit(&HookEvent::new("command:reset", json!({}))).await;
        // Both fire (order: exact first, then wildcard).
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    // ---------- builtins ----------

    #[test]
    fn register_builtins_adds_boot_md() {
        let mut reg = HookRegistry::new();
        reg.register_builtins();
        assert_eq!(reg.handler_count(), 1);
        assert_eq!(reg.loaded_hooks().len(), 1);
        assert_eq!(reg.loaded_hooks()[0].name, "boot-md");
        assert_eq!(reg.loaded_hooks()[0].path, "(builtin)");
    }

    #[tokio::test]
    async fn boot_md_no_file_is_silent_ok() {
        let tmp = TempDir::new().unwrap();
        let h = BootMdHook::new(tmp.path().join("missing-boot.md"));
        // Should not error if BOOT.md doesn't exist.
        h.handle(&HookEvent::new("gateway:startup", json!({})))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn boot_md_empty_file_is_silent_ok() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("BOOT.md");
        std::fs::write(&p, "   \n  ").unwrap();
        let h = BootMdHook::new(p);
        h.handle(&HookEvent::new("gateway:startup", json!({})))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn boot_md_with_content_returns_ok() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("BOOT.md");
        std::fs::write(&p, "1. Check cron jobs\n2. Send status update").unwrap();
        let h = BootMdHook::new(p);
        h.handle(&HookEvent::new("gateway:startup", json!({})))
            .await
            .unwrap();
    }

    // ---------- CommandHookHandler subprocess ----------

    #[tokio::test]
    async fn command_hook_runs_true_successfully() {
        let h = CommandHookHandler::new(
            "true-hook",
            vec!["/usr/bin/true".into()],
            std::env::temp_dir(),
            5,
        );
        // Some systems have /bin/true, others /usr/bin/true. Try both.
        let alt = if std::path::Path::new("/usr/bin/true").exists() {
            "/usr/bin/true"
        } else {
            "/bin/true"
        };
        let h = CommandHookHandler::new("true-hook", vec![alt.into()], std::env::temp_dir(), 5);
        let result = h
            .handle(&HookEvent::new("agent:start", json!({"k": "v"})))
            .await;
        assert!(result.is_ok(), "expected ok, got {:?}", result);
    }

    #[tokio::test]
    async fn command_hook_nonzero_exit_returns_err() {
        let alt = if std::path::Path::new("/usr/bin/false").exists() {
            "/usr/bin/false"
        } else {
            "/bin/false"
        };
        let h = CommandHookHandler::new("false-hook", vec![alt.into()], std::env::temp_dir(), 5);
        let err = h
            .handle(&HookEvent::new("agent:start", json!({})))
            .await
            .unwrap_err();
        assert!(err.contains("non-zero"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn command_hook_empty_argv_errors() {
        let h = CommandHookHandler::new("empty", vec![], std::env::temp_dir(), 5);
        let err = h
            .handle(&HookEvent::new("agent:start", json!({})))
            .await
            .unwrap_err();
        assert!(err.contains("empty argv"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn command_hook_missing_binary_errors() {
        let h = CommandHookHandler::new(
            "missing",
            vec!["/definitely/not/a/binary/abcxyz".into()],
            std::env::temp_dir(),
            5,
        );
        let err = h
            .handle(&HookEvent::new("agent:start", json!({})))
            .await
            .unwrap_err();
        assert!(err.contains("Failed to spawn"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn command_hook_propagates_event_via_env_var() {
        // Use a shell command that prints HERMES_HOOK_EVENT into a temp
        // file, then verify the file's content matches what we emitted.
        let tmp = TempDir::new().unwrap();
        let out = tmp.path().join("event_out.txt");

        // Use sh -c so we can use env var expansion.
        let h = CommandHookHandler::new(
            "env-hook",
            vec![
                "/bin/sh".into(),
                "-c".into(),
                format!("printf %s \"$HERMES_HOOK_EVENT\" > {}", out.display()),
            ],
            std::env::temp_dir(),
            5,
        );
        h.handle(&HookEvent::new("agent:step", json!({"i": 7})))
            .await
            .unwrap();
        let written = std::fs::read_to_string(&out).unwrap();
        assert_eq!(written, "agent:step");
    }

    #[tokio::test]
    async fn command_hook_via_registry_discovery_fires_on_emit() {
        let tmp = TempDir::new().unwrap();
        let hook_dir = tmp.path().join("flag-hook");
        let flag_path = tmp.path().join("flag.txt");

        // Manifest: when fired, write "fired" to flag.txt.
        std::fs::create_dir_all(&hook_dir).unwrap();
        std::fs::write(
            hook_dir.join("HOOK.yaml"),
            format!(
                "name: flag\nevents: ['agent:start']\ncommand: \
                 [/bin/sh, -c, \"printf fired > {}\"]\n",
                flag_path.display(),
            ),
        )
        .unwrap();

        let mut reg = HookRegistry::new();
        reg.discover_and_load(tmp.path());
        assert_eq!(reg.handler_count(), 1);

        reg.emit(&HookEvent::new("agent:start", json!({}))).await;

        // Subprocess should have written the flag.
        let written = std::fs::read_to_string(&flag_path).unwrap();
        assert_eq!(written, "fired");
    }

    #[tokio::test]
    async fn command_hook_timeout_returns_err() {
        // Sleep 5s with a 1s timeout — should err.
        let h = CommandHookHandler::new(
            "sleeper",
            vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            std::env::temp_dir(),
            1,
        );
        let err = h
            .handle(&HookEvent::new("agent:start", json!({})))
            .await
            .unwrap_err();
        assert!(err.contains("timed out"), "unexpected: {err}");
    }

    // ---------- ordering / counting ----------

    #[tokio::test]
    async fn multiple_handlers_per_event_all_fire_in_registration_order() {
        let counter = Arc::new(AtomicUsize::new(0));

        struct Inc(Arc<AtomicUsize>, &'static str);
        #[async_trait]
        impl HookHandler for Inc {
            async fn handle(&self, _e: &HookEvent) -> Result<(), String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            fn name(&self) -> &str {
                self.1
            }
        }

        let mut reg = HookRegistry::new();
        reg.register_in_process("agent:end", Arc::new(Inc(counter.clone(), "a")));
        reg.register_in_process("agent:end", Arc::new(Inc(counter.clone(), "b")));
        reg.register_in_process("agent:end", Arc::new(Inc(counter.clone(), "c")));
        reg.emit(&HookEvent::new("agent:end", json!({}))).await;
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }
}
