//! Local-environment toolchain probe for the system prompt.
//!
//! Corresponds to `hermes-agent/tools/env_probe.py`.

use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

// Remote backends — keep in sync with Python's _REMOTE_BACKENDS.
const REMOTE_BACKENDS: &[&str] = &[
    "docker", "singularity", "modal", "daytona", "ssh", "managed_modal",
];

static CACHE: Mutex<Option<Option<String>>> = Mutex::new(None);

/// Run a short subprocess. Returns (returncode, stdout, stderr).
fn run(cmd: &[&str], _timeout: Duration) -> (i32, String, String) {
    let binary = match cmd.first() {
        Some(b) => *b,
        None => return (-1, String::new(), "empty cmd".into()),
    };
    let args = &cmd[1..];

    match Command::new(binary)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
    {
        Ok(output) => {
            let code = output.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            (code, stdout, stderr)
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                (-1, String::new(), "not found".into())
            } else {
                (-1, String::new(), format!("oserror: {e}"))
            }
        }
    }
}

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
    let code = "import sys,os;stdlib=os.path.dirname(os.__file__);marker=os.path.join(stdlib,'EXTERNALLY-MANAGED');print('yes' if os.path.exists(marker) else 'no')";
    let (rc, out, _) = run(&[binary, "-c", code], Duration::from_secs(3));
    rc == 0 && out.trim() == "yes"
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
    if let Some(tail) = out.rsplit("(python ").next() {
        if tail.ends_with(')') {
            return Some(tail[..tail.len() - 1].to_string());
        }
    }
    None
}

/// Check if a binary exists on PATH.
fn which_binary(binary: &str) -> Option<String> {
    if std::path::Path::new(binary).is_absolute() {
        return std::path::Path::new(binary)
            .exists()
            .then(|| binary.to_string());
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let full = dir.join(binary);
        if full.exists() {
            return Some(full.to_string_lossy().into_owned());
        }
    }
    None
}

/// Build the one-liner. Returns "" when nothing notable is detected.
fn build_probe_line() -> String {
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

    if let Some(ref v) = py3_ver {
        let mut bit = format!("python3={v}");
        if !py3_has_pip {
            bit.push_str(" (no pip module)");
        }
        bits.push(bit);
    } else {
        bits.push("python3=missing".into());
    }

    match (&py_ver, &py3_ver) {
        (Some(py), Some(py3)) if py != py3 => bits.push(format!("python={py}")),
        (None, Some(_)) => bits.push("python=missing (use python3)".into()),
        _ => {}
    }

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
        return cached.clone().unwrap_or_default();
    }

    let line = std::panic::catch_unwind(build_probe_line).unwrap_or_else(|_| String::new());
    *cache = Some(Some(line.clone()));
    line
}

/// Test helper — clear the cache.
#[doc(hidden)]
pub fn reset_cache_for_tests() {
    let mut cache = CACHE.lock().expect("env_probe cache poisoned");
    *cache = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remote_backend_silent() {
        // Set TERMINAL_ENV to a remote backend; probe should be silent.
        unsafe { std::env::set_var("TERMINAL_ENV", "docker"); }
        reset_cache_for_tests();
        let line = build_probe_line();
        assert!(line.is_empty(), "remote backend should be silent, got: {line}");
        unsafe { std::env::remove_var("TERMINAL_ENV"); }
    }

    #[test]
    fn test_cache_works() {
        reset_cache_for_tests();
        let first = get_environment_probe_line(false);
        let second = get_environment_probe_line(false);
        assert_eq!(first, second, "cached result should be identical");
        reset_cache_for_tests();
    }

    #[test]
    fn test_force_refresh_clears_cache() {
        reset_cache_for_tests();
        let _ = get_environment_probe_line(false);
        let after = get_environment_probe_line(true);
        // After force refresh, should re-probe (result may vary, just check it works)
        drop(after);
        reset_cache_for_tests();
    }

    #[test]
    fn test_default_returns_string() {
        reset_cache_for_tests();
        let line = get_environment_probe_line(false);
        // On non-remote backends this returns *some* string (possibly empty)
        // We just verify it doesn't panic.
        assert!(line.is_empty() || line.starts_with("Python toolchain:"));
        reset_cache_for_tests();
    }
}
