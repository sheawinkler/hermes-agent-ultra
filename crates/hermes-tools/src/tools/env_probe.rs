//! Local-environment toolchain probe for the system prompt.
//!
//! Corresponds to `hermes-agent/tools/env_probe.py`.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

// Remote backends — keep in sync with Python's _REMOTE_BACKENDS.
const REMOTE_BACKENDS: &[&str] = &[
    "docker", "singularity", "modal", "daytona", "ssh", "managed_modal",
];

// None = not probed yet; Some("") = probed, nothing to say; Some(s) = probed result.
static CACHE: Mutex<Option<String>> = Mutex::new(None);

// Serialize tests that mutate global env (TERMINAL_ENV, CACHE, etc.).
#[cfg(test)]
static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Subprocess helpers
// ---------------------------------------------------------------------------

/// Run a short subprocess. Returns (returncode, stdout, stderr).
///
/// Implements a 3-second timeout via `wait_timeout`, matching Python's
/// `subprocess.run(timeout=3.0)`. When the child exceeds the timeout it is
/// killed; the caller receives `(-1, "", "timeout")`.
fn run(cmd: &[&str], timeout: Duration) -> (i32, String, String) {
    let binary = match cmd.first() {
        Some(b) => *b,
        None => return (-1, String::new(), "empty cmd".into()),
    };
    let args = &cmd[1..];

    let child = match Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                return (-1, String::new(), "not found".into());
            }
            return (-1, String::new(), format!("oserror: {e}"));
        }
    };

    wait_timeout_kill(child, timeout)
}

fn wait_timeout_kill(mut child: Child, timeout: Duration) -> (i32, String, String) {
    let start = Instant::now();
    let pid = child.id();

    loop {
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            child.kill().ok();
            child.wait().ok();
            return (-1, String::new(), "timeout".into());
        }
        let remaining = timeout - elapsed;
        // Poll in small increments so we don't oversleep the timeout.
        let chunk = remaining.min(Duration::from_millis(100));
        match child.try_wait() {
            Ok(Some(status)) => {
                let code = status.code().unwrap_or(-1);
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_string(&mut stdout);
                }
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_string(&mut stderr);
                }
                return (code, stdout.trim().to_string(), stderr.trim().to_string());
            }
            Ok(None) => {
                std::thread::sleep(chunk);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return (-1, String::new(), format!("process {pid} wait error"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Python toolchain probing
// ---------------------------------------------------------------------------

/// Return a short version string like `3.12.4` for `binary`, or None.
fn python_version_of(binary: &str) -> Option<String> {
    if which_binary(binary).is_none() {
        return None;
    }
    let (rc, out, _) = run(
        &[
            binary,
            "-c",
            "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}')",
        ],
        Duration::from_secs(3),
    );
    (rc == 0 && !out.is_empty()).then_some(out)
}

/// True if `<binary> -m pip --version` succeeds.
fn has_pip_module(binary: &str) -> bool {
    if which_binary(binary).is_none() {
        return false;
    }
    let (rc, _, _) = run(
        &[binary, "-m", "pip", "--version"],
        Duration::from_secs(3),
    );
    rc == 0
}

/// True when `<binary>`'s install location is PEP-668 externally-managed.
fn detect_pep668(binary: &str) -> bool {
    if which_binary(binary).is_none() {
        return false;
    }
    // Matches Python _detect_pep668 exactly — checks for an
    // EXTERNALLY-MANAGED marker next to the stdlib dir.
    let code = "import sys,os;stdlib=os.path.dirname(os.__file__);\
                marker=os.path.join(stdlib,'EXTERNALLY-MANAGED');\
                print('yes' if os.path.exists(marker) else 'no')";
    let (rc, out, _) = run(&[binary, "-c", code], Duration::from_secs(3));
    rc == 0 && out == "yes"
}

/// If `pip` is on PATH, return the Python version it's bound to.
///
/// `pip --version` output: `pip 24.0 from /usr/lib/... (python 3.12)`
fn pip_python_version() -> Option<String> {
    if which_binary("pip").is_none() {
        return None;
    }
    let (rc, out, _) = run(&["pip", "--version"], Duration::from_secs(3));
    if rc != 0 || out.is_empty() {
        return None;
    }
    // rsplit_once("(python ") returns ("pip 24.0...", "3.12)") — we want the tail.
    if let Some((_before, tail)) = out.rsplit_once("(python ") {
        if tail.ends_with(')') {
            return Some(tail[..tail.len() - 1].to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// which_binary — shutil.which equivalent (PATHEXT-aware on Windows)
// ---------------------------------------------------------------------------

/// Check if a binary exists on PATH.
///
/// On Windows, appends each entry in `PATHEXT` (`.exe`, `.cmd`, `.bat`, …)
/// to the candidate name, matching `shutil.which()`.
fn which_binary(binary: &str) -> Option<String> {
    if std::path::Path::new(binary).is_absolute() {
        return std::path::Path::new(binary)
            .exists()
            .then(|| binary.to_string());
    }

    // If the binary already carries an extension, skip PATHEXT probing.
    let has_ext = std::path::Path::new(binary)
        .extension()
        .is_some();

    let path_var = std::env::var_os("PATH")?;

    #[cfg(windows)]
    let pathexts: Vec<String> = {
        if has_ext {
            vec![String::new()]
        } else {
            std::env::var_os("PATHEXT")
                .unwrap_or_else(|| std::ffi::OsString::from(".EXE;.CMD;.BAT;.COM;.PS1"))
                .to_string_lossy()
                .split(';')
                .map(|e| e.to_string())
                .collect()
        }
    };

    #[cfg(not(windows))]
    let pathexts: Vec<String> = vec![String::new()];

    for dir in std::env::split_paths(&path_var) {
        for ext in &pathexts {
            let mut full = dir.join(binary);
            if !ext.is_empty() {
                full.set_extension(ext.trim_start_matches('.'));
            }
            if full.exists() {
                return Some(full.to_string_lossy().into_owned());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Probe line builder
// ---------------------------------------------------------------------------

/// Build the one-liner. Returns "" when nothing notable is detected.
pub fn build_probe_line() -> String {
    // Bail out for remote terminal backends.
    let backend = std::env::var("TERMINAL_ENV")
        .unwrap_or_else(|_| "local".into())
        .to_lowercase();
    if REMOTE_BACKENDS.contains(&backend.as_str()) {
        return String::new();
    }

    let py3_ver = python_version_of("python3");
    let py_ver = python_version_of("python");
    let py3_has_pip = py3_ver.is_some() && has_pip_module("python3");
    let pip_bound = pip_python_version();
    let py3_pep668 = py3_ver.is_some() && detect_pep668("python3");
    let has_uv = which_binary("uv").is_some();

    // If environment looks clean, stay silent.
    let mismatch = match (&pip_bound, &py3_ver) {
        (Some(pip), Some(py3)) => !py3.starts_with(pip.as_str()),
        _ => false,
    };
    let silent = py3_ver.is_some()
        && py3_has_pip
        && !mismatch
        && (!py3_pep668 || has_uv);
    if silent {
        return String::new();
    }

    let mut bits: Vec<String> = Vec::new();

    // python3
    if let Some(ref v) = py3_ver {
        let mut bit = format!("python3={v}");
        if !py3_has_pip {
            bit.push_str(" (no pip module)");
        }
        bits.push(bit);
    } else {
        bits.push("python3=missing".into());
    }

    // python (only when different from python3 or missing on a python3-only system)
    match (&py_ver, &py3_ver) {
        (Some(py), Some(py3)) if py != py3 => bits.push(format!("python={py}")),
        (None, Some(_)) => bits.push("python=missing (use python3)".into()),
        _ => {}
    }

    // pip
    if let Some(ref pip) = pip_bound {
        if mismatch {
            bits.push(format!("pip→python{pip} (mismatch)"));
        } else if !py3_has_pip {
            bits.push(format!("pip→python{pip}"));
        }
    } else if !py3_has_pip && py3_ver.is_some() {
        bits.push("pip=missing".into());
    }

    if py3_pep668 {
        bits.push("PEP 668=yes (use venv or uv)".into());
    }
    if has_uv {
        bits.push("uv=installed".into());
    }

    if bits.is_empty() {
        return String::new();
    }

    format!("Python toolchain: {}.", bits.join(", "))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Return the cached probe line (building it on first call).
///
/// Returns "" when the environment is clean.
pub fn get_environment_probe_line(force_refresh: bool) -> String {
    if force_refresh {
        let mut cache = CACHE.lock().expect("env_probe cache poisoned");
        *cache = None;
    }

    let mut cache = CACHE.lock().expect("env_probe cache poisoned");
    if let Some(ref cached) = *cache {
        return cached.clone();
    }

    let line = std::panic::catch_unwind(|| build_probe_line()).unwrap_or_else(|_| String::new());
    *cache = Some(line.clone());
    line
}

/// Test helper — clear the cache.
#[doc(hidden)]
pub fn reset_cache_for_tests() {
    let mut cache = CACHE.lock().expect("env_probe cache poisoned");
    *cache = None;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ──────────────────────────────────────────────────────────

    /// Grab the env test lock so concurrent tests don't step on each other's
    /// TERMINAL_ENV / CACHE modifications.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_TEST_LOCK.lock().expect("env test lock poisoned")
    }

    fn with_env(key: &str, value: &str, f: impl FnOnce()) {
        let _lock = env_lock();
        reset_cache_for_tests();
        let prev = std::env::var(key).ok();
        unsafe { std::env::set_var(key, value); }
        f();
        match prev {
            Some(v) => unsafe { std::env::set_var(key, v); },
            None => unsafe { std::env::remove_var(key); },
        }
        reset_cache_for_tests();
    }

    fn with_env_removed(key: &str, f: impl FnOnce()) {
        let _lock = env_lock();
        reset_cache_for_tests();
        let prev = std::env::var(key).ok();
        unsafe { std::env::remove_var(key); }
        f();
        if let Some(v) = prev {
            unsafe { std::env::set_var(key, v); }
        }
        reset_cache_for_tests();
    }

    // ── pure logic ───────────────────────────────────────────────────────

    #[test]
    fn test_pip_version_parses_standard_output() {
        // Matches: pip 24.0 from /usr/lib/python3/dist-packages/pip (python 3.12)
        // rsplit_once("(python ") should yield "3.12)"
        let (_before, tail) = "pip 24.0 from /x/pip (python 3.12)"
            .rsplit_once("(python ")
            .expect("rsplit_once");
        assert_eq!(tail, "3.12)");
    }

    #[test]
    fn test_remote_backend_silent() {
        with_env("TERMINAL_ENV", "docker", || {
            assert_eq!(build_probe_line(), "");
        });
    }

    #[test]
    fn test_remote_backend_modal_silent() {
        with_env("TERMINAL_ENV", "modal", || {
            assert_eq!(build_probe_line(), "");
        });
    }

    #[test]
    fn test_local_backend_produces_something() {
        with_env_removed("TERMINAL_ENV", || {
            let line = build_probe_line();
            // Either clean (empty) or a diagnostic line — never panics.
            assert!(line.is_empty() || line.starts_with("Python toolchain:"));
        });
    }

    #[test]
    fn test_cache_returns_same_value() {
        let _lock = env_lock();
        reset_cache_for_tests();
        let first = get_environment_probe_line(false);
        let second = get_environment_probe_line(false);
        assert_eq!(first, second);
        reset_cache_for_tests();
    }

    #[test]
    fn test_force_refresh_rebuilds() {
        let _lock = env_lock();
        reset_cache_for_tests();
        let _ = get_environment_probe_line(false);
        // Force refresh must not poison.
        let after = get_environment_probe_line(true);
        assert!(after.is_empty() || after.starts_with("Python toolchain:"));
        reset_cache_for_tests();
    }

    // ── subprocess helpers ───────────────────────────────────────────────

    #[test]
    fn test_run_returns_timeout_for_slow_command() {
        #[cfg(windows)]
        let cmd = [
            "powershell",
            "-NoProfile",
            "-Command",
            "Start-Sleep -Seconds 10",
        ];
        #[cfg(not(windows))]
        let cmd = ["sleep", "10"];

        let (rc, _out, err) = run(&cmd, Duration::from_millis(100));
        assert_eq!(rc, -1);
        assert!(err.contains("timeout"), "expected 'timeout', got: {err}");
    }

    #[test]
    fn test_run_returns_not_found_for_nonexistent_binary() {
        let (rc, _out, err) = run(&["nonexistent_binary_12345", "arg"], Duration::from_secs(1));
        assert_eq!(rc, -1);
        assert!(err.contains("not found"), "expected 'not found', got: {err}");
    }

    #[test]
    fn test_which_binary_finds_common_tool() {
        // `cmd.exe` on Windows, `sh` on Unix — always on PATH.
        #[cfg(windows)]
        let name = "cmd";
        #[cfg(not(windows))]
        let name = "sh";

        let found = which_binary(name);
        assert!(found.is_some(), "{name} should be found on PATH");
    }
}
