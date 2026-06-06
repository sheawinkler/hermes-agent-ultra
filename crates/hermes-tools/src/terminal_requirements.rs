//! Runtime availability checks for terminal-backed tools.
//!
//! These checks mirror the upstream terminal requirements gate without making
//! tool discovery crash or block indefinitely. They are deliberately scoped to
//! terminal-backed tools; Rust `execute_code` is local and independently gated.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use hermes_config::managed_gateway::{
    has_direct_modal_credentials, is_managed_tool_gateway_ready, managed_nous_tools_enabled,
    resolve_modal_backend_state, ModalMode, ResolveOptions, SelectedBackend,
};

const REQUIREMENT_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const REQUIREMENT_CACHE_TTL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
struct CachedReport {
    fingerprint: String,
    checked_at: Instant,
    report: TerminalRequirementReport,
}

static REQUIREMENT_CACHE: OnceLock<Mutex<Option<CachedReport>>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRequirementReport {
    pub backend: String,
    pub available: bool,
    pub reason: Option<String>,
}

impl TerminalRequirementReport {
    fn ok(backend: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
            available: true,
            reason: None,
        }
    }

    fn unavailable(backend: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
            available: false,
            reason: Some(reason.into()),
        }
    }
}

pub fn check_terminal_requirements() -> bool {
    terminal_requirements_report().available
}

pub fn terminal_requirements_report() -> TerminalRequirementReport {
    let fingerprint = requirement_fingerprint();
    let cache = REQUIREMENT_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock() {
        if let Some(cached) = guard.as_ref() {
            if cached.fingerprint == fingerprint
                && cached.checked_at.elapsed() < REQUIREMENT_CACHE_TTL
            {
                return cached.report.clone();
            }
        }
    }

    let report = terminal_requirements_report_uncached();
    if let Ok(mut guard) = cache.lock() {
        *guard = Some(CachedReport {
            fingerprint,
            checked_at: Instant::now(),
            report: report.clone(),
        });
    }
    report
}

fn terminal_requirements_report_uncached() -> TerminalRequirementReport {
    let backend = env_nonempty("TERMINAL_ENV")
        .unwrap_or_else(|| "local".to_string())
        .trim()
        .to_ascii_lowercase();
    match backend.as_str() {
        "" | "local" => TerminalRequirementReport::ok("local"),
        "docker" => check_command_backend(
            "docker",
            &["docker"],
            &["version"],
            "Docker executable not found in PATH or common install locations",
        ),
        "singularity" | "apptainer" => check_command_backend(
            "singularity",
            &["apptainer", "singularity"],
            &["--version"],
            "Apptainer/Singularity executable not found in PATH",
        ),
        "ssh" => check_ssh_requirements(),
        "modal" => check_modal_requirements(),
        "daytona" => check_daytona_requirements(),
        "vercel_sandbox" => TerminalRequirementReport::unavailable(
            "vercel_sandbox",
            "Vercel Sandbox is not available in the Rust terminal runtime; choose local, docker, ssh, daytona, modal, or singularity.",
        ),
        other => TerminalRequirementReport::unavailable(
            other,
            format!(
                "Unknown TERMINAL_ENV '{other}'. Use one of: local, docker, singularity, modal, daytona, ssh."
            ),
        ),
    }
}

fn requirement_fingerprint() -> String {
    [
        "TERMINAL_ENV",
        "TERMINAL_MODAL_MODE",
        "TERMINAL_SSH_HOST",
        "TERMINAL_SSH_USER",
        "DAYTONA_API_KEY",
        "MODAL_TOKEN_ID",
        "MODAL_TOKEN_SECRET",
        "HOME",
        "USERPROFILE",
        "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
        "TOOL_GATEWAY_USER_TOKEN",
        "TOOL_GATEWAY_DOMAIN",
        "TOOL_GATEWAY_SCHEME",
        "MODAL_GATEWAY_URL",
        "PATH",
        "DOCKER_CONTEXT",
        "DOCKER_HOST",
    ]
    .into_iter()
    .map(|key| format!("{key}={}", env::var(key).unwrap_or_default()))
    .collect::<Vec<_>>()
    .join("\n")
}

fn check_command_backend(
    backend: &str,
    executable_names: &[&str],
    probe_args: &[&str],
    missing_message: &str,
) -> TerminalRequirementReport {
    let Some(executable) = find_executable(executable_names) else {
        return TerminalRequirementReport::unavailable(backend, missing_message);
    };
    if command_success_with_timeout(&executable, probe_args, REQUIREMENT_COMMAND_TIMEOUT) {
        TerminalRequirementReport::ok(backend)
    } else {
        TerminalRequirementReport::unavailable(
            backend,
            format!(
                "{} requirement probe failed or timed out: {} {}",
                backend,
                executable.display(),
                probe_args.join(" ")
            ),
        )
    }
}

fn check_ssh_requirements() -> TerminalRequirementReport {
    let host = env_nonempty("TERMINAL_SSH_HOST");
    let user = env_nonempty("TERMINAL_SSH_USER");
    if host.is_none() || user.is_none() {
        return TerminalRequirementReport::unavailable(
            "ssh",
            "SSH backend selected but TERMINAL_SSH_HOST and TERMINAL_SSH_USER are not both set. Configure both or switch TERMINAL_ENV to 'local'.",
        );
    }
    TerminalRequirementReport::ok("ssh")
}

fn check_modal_requirements() -> TerminalRequirementReport {
    let modal_mode = env_nonempty("TERMINAL_MODAL_MODE");
    let has_direct = has_direct_modal_credentials();
    let managed_ready = is_managed_tool_gateway_ready("modal", ResolveOptions::default());
    let state = resolve_modal_backend_state(modal_mode.as_deref(), has_direct, managed_ready);

    match state.selected_backend {
        Some(SelectedBackend::Managed) => TerminalRequirementReport::ok("modal"),
        Some(SelectedBackend::Direct) => TerminalRequirementReport::ok("modal"),
        None if state.managed_mode_blocked => TerminalRequirementReport::unavailable(
            "modal",
            "Modal backend selected with TERMINAL_MODAL_MODE=managed, but Nous Tool Gateway access is not currently available and no direct Modal credentials/config were found.",
        ),
        None if matches!(state.mode, ModalMode::Managed) => TerminalRequirementReport::unavailable(
            "modal",
            "Modal backend selected with TERMINAL_MODAL_MODE=managed, but the managed tool gateway is unavailable.",
        ),
        None if matches!(state.mode, ModalMode::Direct) => {
            let managed_hint = if managed_nous_tools_enabled() {
                " Configure Modal or choose TERMINAL_MODAL_MODE=managed/auto."
            } else {
                " Configure Modal or choose TERMINAL_MODAL_MODE=auto."
            };
            TerminalRequirementReport::unavailable(
                "modal",
                format!(
                    "Modal backend selected with TERMINAL_MODAL_MODE=direct, but no direct Modal credentials/config were found.{managed_hint}"
                ),
            )
        }
        None => {
            let reason = if managed_nous_tools_enabled() {
                "Modal backend selected but no direct Modal credentials/config or managed tool gateway was found. Configure Modal, set up the managed gateway, or choose a different TERMINAL_ENV."
            } else {
                "Modal backend selected but no direct Modal credentials/config was found. Configure Modal or choose a different TERMINAL_ENV."
            };
            TerminalRequirementReport::unavailable("modal", reason)
        }
    }
}

fn check_daytona_requirements() -> TerminalRequirementReport {
    if env_nonempty("DAYTONA_API_KEY").is_some() {
        TerminalRequirementReport::ok("daytona")
    } else {
        TerminalRequirementReport::unavailable(
            "daytona",
            "Daytona backend selected but DAYTONA_API_KEY is not set.",
        )
    }
}

fn env_nonempty(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.trim().is_empty())
}

fn find_executable(names: &[&str]) -> Option<PathBuf> {
    for name in names {
        let candidate = Path::new(name);
        if candidate.components().count() > 1 && is_executable(candidate) {
            return Some(candidate.to_path_buf());
        }

        if let Some(found) = find_on_path(name) {
            return Some(found);
        }

        for common in common_executable_locations(name) {
            if is_executable(&common) {
                return Some(common);
            }
        }
    }
    None
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    for dir in env::split_paths(&paths) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn common_executable_locations(name: &str) -> Vec<PathBuf> {
    match name {
        "docker" => vec![
            PathBuf::from("/usr/local/bin/docker"),
            PathBuf::from("/opt/homebrew/bin/docker"),
            PathBuf::from("/usr/bin/docker"),
            PathBuf::from("/Applications/Docker.app/Contents/Resources/bin/docker"),
            PathBuf::from("/Applications/OrbStack.app/Contents/MacOS/docker"),
        ],
        _ => Vec::new(),
    }
}

fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn command_success_with_timeout(executable: &Path, args: &[&str], timeout: Duration) -> bool {
    let mut child = match Command::new(executable)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::TEST_ENV_LOCK;

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = env::var(key).ok();
            unsafe { env::set_var(key, value) };
            Self { key, original }
        }

        fn remove(key: &'static str) -> Self {
            let original = env::var(key).ok();
            unsafe { env::remove_var(key) };
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(value) = &self.original {
                    env::set_var(self.key, value);
                } else {
                    env::remove_var(self.key);
                }
            }
        }
    }

    fn clear_terminal_env() -> Vec<EnvGuard> {
        [
            "TERMINAL_ENV",
            "TERMINAL_MODAL_MODE",
            "TERMINAL_SSH_HOST",
            "TERMINAL_SSH_USER",
            "DAYTONA_API_KEY",
            "MODAL_TOKEN_ID",
            "MODAL_TOKEN_SECRET",
            "HOME",
            "USERPROFILE",
            "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
            "TOOL_GATEWAY_USER_TOKEN",
            "TOOL_GATEWAY_DOMAIN",
            "TOOL_GATEWAY_SCHEME",
            "MODAL_GATEWAY_URL",
        ]
        .into_iter()
        .map(EnvGuard::remove)
        .collect()
    }

    #[test]
    fn local_terminal_requirements_pass_by_default() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guards = clear_terminal_env();
        assert!(check_terminal_requirements());
    }

    #[test]
    fn unknown_terminal_env_is_unavailable() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guards = clear_terminal_env();
        let _env = EnvGuard::set("TERMINAL_ENV", "unknown-backend");
        let report = terminal_requirements_report();
        assert!(!report.available);
        assert!(report
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("Unknown TERMINAL_ENV 'unknown-backend'"));
    }

    #[test]
    fn ssh_requires_host_and_user() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guards = clear_terminal_env();
        let _env = EnvGuard::set("TERMINAL_ENV", "ssh");
        let report = terminal_requirements_report();
        assert!(!report.available);
        assert!(report
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("TERMINAL_SSH_HOST and TERMINAL_SSH_USER"));

        let _host = EnvGuard::set("TERMINAL_SSH_HOST", "example.invalid");
        let _user = EnvGuard::set("TERMINAL_SSH_USER", "hermes");
        assert!(terminal_requirements_report().available);
    }

    #[test]
    fn modal_managed_mode_requires_managed_gateway() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guards = clear_terminal_env();
        let _env = EnvGuard::set("TERMINAL_ENV", "modal");
        let _mode = EnvGuard::set("TERMINAL_MODAL_MODE", "managed");
        let unavailable = terminal_requirements_report();
        assert!(!unavailable.available);

        let _enabled = EnvGuard::set("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        let _token = EnvGuard::set("TOOL_GATEWAY_USER_TOKEN", "nous-token");
        let _domain = EnvGuard::set("TOOL_GATEWAY_DOMAIN", "tools.example.invalid");
        let available = terminal_requirements_report();
        assert!(available.available, "{available:?}");
    }

    #[test]
    fn modal_direct_mode_requires_direct_credentials() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guards = clear_terminal_env();
        let _env = EnvGuard::set("TERMINAL_ENV", "modal");
        let _mode = EnvGuard::set("TERMINAL_MODAL_MODE", "direct");
        let unavailable = terminal_requirements_report();
        assert!(!unavailable.available);

        let _id = EnvGuard::set("MODAL_TOKEN_ID", "tok-id");
        let _secret = EnvGuard::set("MODAL_TOKEN_SECRET", "tok-secret");
        assert!(terminal_requirements_report().available);
    }

    #[test]
    fn vercel_sandbox_is_known_but_not_exposed_without_rust_backend() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guards = clear_terminal_env();
        let _env = EnvGuard::set("TERMINAL_ENV", "vercel_sandbox");
        let report = terminal_requirements_report();
        assert!(!report.available);
        assert_eq!(report.backend, "vercel_sandbox");
        assert!(report
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("not available in the Rust terminal runtime"));
    }
}
