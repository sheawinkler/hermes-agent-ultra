//! Config-driven shell hooks bridged into [`PluginManager`] (Python `agent.shell_hooks` parity).
//!
//! Hooks declared under `hooks:` in `config.yaml` / `cli-config.yaml` run as subprocesses
//! with a JSON payload on stdin, matching the Python wire format used by `invoke_hook`.
//!
//! Allowlist 文件：{hermes_home}/shell-hooks-allowlist.json，格式 {"approvals": [{"event", "command", "approved_at", ...}]}

use std::collections::HashSet;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chrono::Utc;
use serde_json::{Map, Value};
use shlex::split;

use crate::plugins::{HookResult, HookType, PluginManager};

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 300;
const ALLOWLIST_FILENAME: &str = "shell-hooks-allowlist.json";

static PROCESS_ACCEPT_HOOKS: AtomicBool = AtomicBool::new(false);
static ALLOWLIST_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// CLI / gateway may set this once at process startup (`--accept-hooks`).
pub fn set_process_accept_hooks(accept: bool) {
    PROCESS_ACCEPT_HOOKS.store(accept, Ordering::Relaxed);
}

/// Test-only: reset process-wide accept override and env.
#[cfg(test)]
pub fn reset_process_accept_hooks_for_tests() {
    PROCESS_ACCEPT_HOOKS.store(false, Ordering::Relaxed);
    hermes_core::test_env::remove_var("HERMES_ACCEPT_HOOKS");
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ShellHookKey {
    event: String,
    matcher: Option<String>,
    command: String,
}

#[derive(Debug, Clone)]
struct ShellHookSpec {
    event: HookType,
    command: String,
    matcher: Option<String>,
    timeout: Duration,
}

/// Parse `hooks:` from Hermes config and register subprocess callbacks on `mgr`.
///
/// Each `(event, matcher, command)` triple is registered at most once per call.
/// Unapproved hooks require an interactive TTY prompt unless auto-accept is enabled
/// (`--accept-hooks`, `HERMES_ACCEPT_HOOKS=1`, or `hooks_auto_accept: true`) or the
/// pair is already present in `{hermes_home}/shell-hooks-allowlist.json`.
pub fn register_config_shell_hooks(mgr: &mut PluginManager, hermes_home: &Path) {
    let Some(root) = load_config_root(hermes_home) else {
        return;
    };
    let effective_accept = effective_accept_hooks(&root);
    let Some(hooks_cfg) = root.get("hooks") else {
        return;
    };
    let specs = parse_hooks_block(hooks_cfg);
    if specs.is_empty() {
        return;
    }

    let mut registered_keys = HashSet::new();
    for spec in specs {
        let event_name = spec.event.as_str().to_string();
        let key = ShellHookKey {
            event: event_name.clone(),
            matcher: spec.matcher.clone(),
            command: spec.command.clone(),
        };
        if !registered_keys.insert(key.clone()) {
            continue;
        }

        let allowlisted = is_allowlisted(hermes_home, &event_name, &spec.command);
        if !allowlisted && !prompt_and_record(hermes_home, &event_name, &spec.command, effective_accept) {
            tracing::warn!(
                event = %event_name,
                command = %spec.command,
                "shell hook not allowlisted — skipped. Use --accept-hooks / HERMES_ACCEPT_HOOKS=1 / \
                 hooks_auto_accept: true, or approve at the TTY prompt on the next interactive run."
            );
            continue;
        }

        let cb = make_shell_callback(spec);
        mgr.register_hook_callback(cb.0, cb.1);
        tracing::info!(
            event = %event_name,
            command = %key.command,
            matcher = ?key.matcher,
            "shell hook registered"
        );
    }
}

fn effective_accept_hooks(root: &Value) -> bool {
    PROCESS_ACCEPT_HOOKS.load(Ordering::Relaxed) || hooks_auto_accept(root)
}

fn hooks_auto_accept(root: &Value) -> bool {
    if env_truthy("HERMES_ACCEPT_HOOKS") {
        return true;
    }
    match root.get("hooks_auto_accept") {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        _ => false,
    }
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name).ok().is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn allowlist_path(hermes_home: &Path) -> PathBuf {
    hermes_home.join(ALLOWLIST_FILENAME)
}

fn load_allowlist(hermes_home: &Path) -> Map<String, Value> {
    let path = allowlist_path(hermes_home);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return empty_allowlist();
    };
    let Ok(raw) = serde_json::from_str::<Value>(&content) else {
        return empty_allowlist();
    };
    let Some(obj) = raw.as_object() else {
        return empty_allowlist();
    };
    let mut data = obj.clone();
    if !data.get("approvals").is_some_and(Value::is_array) {
        data.insert("approvals".to_string(), Value::Array(Vec::new()));
    }
    data
}

fn empty_allowlist() -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("approvals".to_string(), Value::Array(Vec::new()));
    map
}

fn save_allowlist(hermes_home: &Path, data: &Map<String, Value>) {
    let path = allowlist_path(hermes_home);
    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                path = %path.display(),
                "failed to create shell hook allowlist directory: {err}"
            );
            return;
        }
    }
    let serialized = match serde_json::to_string_pretty(data) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!("failed to serialize shell hook allowlist: {err}");
            return;
        }
    };
    let tmp = path.with_extension("json.tmp");
    if let Err(err) = std::fs::write(&tmp, serialized) {
        tracing::warn!(
            path = %path.display(),
            "failed to write shell hook allowlist temp file: {err}"
        );
        return;
    }
    if let Err(err) = std::fs::rename(&tmp, &path) {
        tracing::warn!(
            path = %path.display(),
            "failed to persist shell hook allowlist: {err}. Approval is in-memory for this run."
        );
        let _ = std::fs::remove_file(&tmp);
    }
}

fn with_allowlist_update<F>(hermes_home: &Path, mutator: F)
where
    F: FnOnce(&mut Map<String, Value>),
{
    let lock = ALLOWLIST_WRITE_LOCK.get_or_init(|| Mutex::new(()));
    let Ok(_guard) = lock.lock() else {
        return;
    };
    let mut data = load_allowlist(hermes_home);
    mutator(&mut data);
    save_allowlist(hermes_home, &data);
}

fn is_allowlisted(hermes_home: &Path, event: &str, command: &str) -> bool {
    load_allowlist(hermes_home)
        .get("approvals")
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry.get("event").and_then(Value::as_str) == Some(event)
                    && entry.get("command").and_then(Value::as_str) == Some(command)
            })
        })
}

fn prompt_and_record(hermes_home: &Path, event: &str, command: &str, accept_hooks: bool) -> bool {
    if accept_hooks {
        record_approval(hermes_home, event, command);
        tracing::info!(
            event = %event,
            command = %command,
            "shell hook auto-approved via --accept-hooks / env / config"
        );
        return true;
    }

    let stdin_tty = std::io::stdin().is_terminal();
    let stdout_tty = std::io::stdout().is_terminal();
    if !(stdin_tty && stdout_tty) {
        return false;
    }

    eprintln!(
        "\n⚠ Hermes is about to register a shell hook that will run a\n\
          command on your behalf.\n\n\
            Event:   {event}\n\
            Command: {command}\n\n\
          Commands run with your full user credentials.  Only approve\n\
          commands you trust."
    );
    eprint!("Allow this hook to run? [y/N]: ");
    let _ = std::io::stderr().flush();

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        eprintln!();
        return false;
    }
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        record_approval(hermes_home, event, command);
        return true;
    }
    false
}

fn record_approval(hermes_home: &Path, event: &str, command: &str) {
    let entry = serde_json::json!({
        "event": event,
        "command": command,
        "approved_at": utc_now_iso(),
        "script_mtime_at_approval": script_mtime_iso(command),
    });
    with_allowlist_update(hermes_home, |data| {
        let approvals = data
            .entry("approvals".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(items) = approvals {
            items.retain(|e| {
                !(e.get("event").and_then(Value::as_str) == Some(event)
                    && e.get("command").and_then(Value::as_str) == Some(command))
            });
            items.push(entry.clone());
        }
    });
}

fn utc_now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn script_mtime_iso(command: &str) -> Option<String> {
    let path = command_script_path(command)?;
    let expanded = expand_user(&path);
    let metadata = std::fs::metadata(expanded).ok()?;
    let modified = metadata.modified().ok()?;
    let datetime: chrono::DateTime<Utc> = modified.into();
    Some(
        datetime
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    )
}

fn command_script_path(command: &str) -> Option<String> {
    let expanded = expand_user(command);
    let argv = split(&expanded)?;
    argv.first().map(|s| s.to_string())
}

fn expand_user(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
            return format!("{home}/{rest}");
        }
    } else if path == "~" {
        if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
            return home;
        }
    }
    path.to_string()
}

fn load_config_root(hermes_home: &Path) -> Option<Value> {
    for name in ["config.yaml", "cli-config.yaml"] {
        let path = hermes_home.join(name);
        let content = std::fs::read_to_string(&path).ok()?;
        let root: Value = serde_yaml::from_str(&content).ok()?;
        if root.get("hooks").is_some() || root.get("hooks_auto_accept").is_some() {
            return Some(root);
        }
        if name == "cli-config.yaml" {
            return Some(root);
        }
    }
    None
}

fn parse_hooks_block(hooks_cfg: &Value) -> Vec<ShellHookSpec> {
    let Some(map) = hooks_cfg.as_object() else {
        return Vec::new();
    };
    let mut specs = Vec::new();
    for (event_name, entries) in map {
        let Some(hook_type) = hook_type_from_str(event_name) else {
            tracing::warn!("unknown hook event {event_name:?} in hooks: config");
            continue;
        };
        let entry_list = match entries {
            Value::Array(items) => items.clone(),
            one => vec![one.clone()],
        };
        for (index, entry) in entry_list.into_iter().enumerate() {
            if let Some(spec) = parse_hook_entry(hook_type, &entry, event_name, index) {
                specs.push(spec);
            }
        }
    }
    specs
}

fn hook_type_from_str(name: &str) -> Option<HookType> {
    HookType::all()
        .iter()
        .copied()
        .find(|h| h.as_str() == name)
}

fn parse_hook_entry(
    event: HookType,
    raw: &Value,
    event_name: &str,
    index: usize,
) -> Option<ShellHookSpec> {
    let Some(obj) = raw.as_object() else {
        tracing::warn!("hooks.{event_name}[{index}] must be a mapping");
        return None;
    };
    let command = obj
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let matcher = obj
        .get("matcher")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    if matcher.is_some() && !matches!(event, HookType::PreToolCall | HookType::PostToolCall) {
        tracing::warn!(
            "hooks.{event_name}[{index}].matcher ignored — only valid for pre/post_tool_call"
        );
    }
    let timeout_secs = obj
        .get("timeout")
        .and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|n| n.max(0) as u64)))
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(1, MAX_TIMEOUT_SECS);
    Some(ShellHookSpec {
        event,
        command: command.to_string(),
        matcher,
        timeout: Duration::from_secs(timeout_secs),
    })
}

fn make_shell_callback(
    spec: ShellHookSpec,
) -> (
    HookType,
    Arc<dyn Fn(&Value) -> HookResult + Send + Sync>,
) {
    let hook = spec.event;
    let command = spec.command.clone();
    let matcher = spec.matcher.clone();
    let timeout = spec.timeout;
    let event_name = hook.as_str().to_string();
    (
        hook,
        Arc::new(move |ctx: &Value| {
            if matches!(hook, HookType::PreToolCall | HookType::PostToolCall) {
                if let Some(ref pattern) = matcher {
                    let tool_name = ctx
                        .get("tool_name")
                        .or_else(|| ctx.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !tool_name.contains(pattern) {
                        return HookResult::Ok;
                    }
                }
            }
            let payload = serialize_payload(&event_name, ctx);
            match spawn_shell_hook(&command, &payload, timeout) {
                Ok(stdout) => parse_hook_stdout(hook, &stdout),
                Err(err) => {
                    tracing::warn!(
                        event = event_name,
                        command = %command,
                        "shell hook failed: {err}"
                    );
                    HookResult::Ok
                }
            }
        }),
    )
}

fn serialize_payload(event: &str, ctx: &Value) -> String {
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let session_id = ctx
        .get("session_id")
        .or_else(|| ctx.get("parent_session_id"))
        .cloned()
        .unwrap_or(Value::String(String::new()));
    let payload = serde_json::json!({
        "hook_event_name": event,
        "tool_name": ctx.get("tool_name").or_else(|| ctx.get("name")),
        "tool_input": ctx.get("args").filter(|v| v.is_object()),
        "session_id": session_id,
        "cwd": cwd,
        "extra": ctx,
    });
    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string())
}

fn spawn_shell_hook(command: &str, stdin_json: &str, timeout: Duration) -> Result<String, String> {
    use std::io::Write;

    let expanded = expand_user(command);
    let argv = split(&expanded).ok_or_else(|| format!("invalid shell command: {command}"))?;
    if argv.is_empty() {
        return Err("empty command".to_string());
    }
    let mut child = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_json.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let started = std::time::Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => {
                use std::io::Read;
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_string(&mut stdout);
                }
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_string(&mut stderr);
                }
                if status.success() || !stdout.trim().is_empty() {
                    return Ok(stdout);
                }
                return Err(format!("exit {status}: {}", stderr.trim()));
            }
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                return Err(format!("timed out after {}s", timeout.as_secs()));
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn parse_hook_stdout(hook: HookType, stdout: &str) -> HookResult {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return HookResult::Ok;
    }
    let Ok(data) = serde_json::from_str::<Value>(trimmed) else {
        return HookResult::Ok;
    };
    match hook {
        HookType::PreLlmCall | HookType::PreApiRequest => {
            if let Some(ctx) = data.get("context").and_then(Value::as_str) {
                if !ctx.trim().is_empty() {
                    return HookResult::InjectContext(ctx.to_string());
                }
            }
        }
        HookType::PostLlmCall | HookType::PostApiRequest => {
            if let Some(text) = data.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    return HookResult::TransformLlmOutput(text.to_string());
                }
            }
        }
        _ => {}
    }
    HookResult::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn write_config(dir: &Path, yaml: &str) {
        std::fs::write(dir.join("config.yaml"), yaml).unwrap();
    }

    fn with_isolated_hook_env<F: FnOnce()>(f: F) {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_process_accept_hooks_for_tests();
        f();
        reset_process_accept_hooks_for_tests();
    }

    #[test]
    fn golden_hook_fixtures_match_plugin_schema() {
        use crate::plugins::{validate_hook_payload, HookType};
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_payloads");
        for hook in HookType::all() {
            let path = root.join(format!("{}.json", hook.as_str()));
            let ctx: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            validate_hook_payload(*hook, &ctx)
                .unwrap_or_else(|e| panic!("{}: {e}", hook.as_str()));
        }
    }

    #[test]
    fn parse_hooks_block_reads_on_session_end() {
        let yaml = r#"
on_session_end:
  - command: echo ok
"#;
        let hooks: Value = serde_yaml::from_str(yaml).unwrap();
        let specs = parse_hooks_block(&hooks);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].event, HookType::OnSessionEnd);
        assert_eq!(specs[0].command, "echo ok");
    }

    #[test]
    fn register_config_shell_hooks_respects_auto_accept() {
        with_isolated_hook_env(|| {
            let tmp = TempDir::new().unwrap();
            write_config(
                tmp.path(),
                r#"
hooks_auto_accept: true
hooks:
  pre_api_request:
    - command: echo test
"#,
            );
            let mut mgr = PluginManager::new();
            register_config_shell_hooks(&mut mgr, tmp.path());
            assert!(mgr.has_hooks());
            let allowlist = load_allowlist(tmp.path());
            assert_eq!(
                allowlist.get("approvals").unwrap().as_array().unwrap().len(),
                1
            );
        });
    }

    #[test]
    fn register_config_shell_hooks_skipped_without_auto_accept() {
        with_isolated_hook_env(|| {
            let tmp = TempDir::new().unwrap();
            write_config(
                tmp.path(),
                r#"
hooks:
  on_session_end:
    - command: echo test
"#,
            );
            let mut mgr = PluginManager::new();
            register_config_shell_hooks(&mut mgr, tmp.path());
            assert!(!mgr.has_hooks());
        });
    }

    #[test]
    fn register_config_shell_hooks_uses_existing_allowlist() {
        with_isolated_hook_env(|| {
            let tmp = TempDir::new().unwrap();
            write_config(
                tmp.path(),
                r#"
hooks:
  on_session_end:
    - command: echo test
"#,
            );
            let allowlist = serde_json::json!({
                "approvals": [{
                    "event": "on_session_end",
                    "command": "echo test",
                    "approved_at": "2026-01-01T00:00:00.000Z"
                }]
            });
            std::fs::write(
                tmp.path().join(ALLOWLIST_FILENAME),
                serde_json::to_string_pretty(&allowlist).unwrap(),
            )
            .unwrap();

            let mut mgr = PluginManager::new();
            register_config_shell_hooks(&mut mgr, tmp.path());
            assert!(mgr.has_hooks());
        });
    }

    #[test]
    fn process_accept_hooks_override_registers_without_tty() {
        with_isolated_hook_env(|| {
            set_process_accept_hooks(true);
            let tmp = TempDir::new().unwrap();
            write_config(
                tmp.path(),
                r#"
hooks:
  on_session_end:
    - command: echo test
"#,
            );
            let mut mgr = PluginManager::new();
            register_config_shell_hooks(&mut mgr, tmp.path());
            assert!(mgr.has_hooks());
        });
    }

    #[test]
    fn register_dedupes_identical_specs_in_one_pass() {
        with_isolated_hook_env(|| {
            set_process_accept_hooks(true);
            let tmp = TempDir::new().unwrap();
            write_config(
                tmp.path(),
                r#"
hooks:
  on_session_end:
    - command: echo test
    - command: echo test
"#,
            );
            let mut mgr = PluginManager::new();
            register_config_shell_hooks(&mut mgr, tmp.path());
            assert!(mgr.has_hooks());
        });
    }
}
