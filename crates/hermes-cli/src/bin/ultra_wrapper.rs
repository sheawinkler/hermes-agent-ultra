use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const AGENT_ULTRA_BIN: &str = "hermes-agent-ultra";

pub fn candidate_targets() -> Vec<OsString> {
    let mut out: Vec<OsString> = Vec::new();
    let current_exe = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok());

    if let Ok(explicit) = std::env::var("HERMES_ULTRA_BIN") {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            let explicit_path = PathBuf::from(explicit);
            if current_exe
                .as_ref()
                .and_then(|current| {
                    explicit_path
                        .canonicalize()
                        .ok()
                        .map(|path| path == *current)
                })
                .unwrap_or(false)
            {
                eprintln!("Ignoring HERMES_ULTRA_BIN because it points back to this wrapper.");
            } else {
                out.push(OsString::from(explicit));
            }
        }
    }

    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            if let Some(local) = sibling_agent_ultra(dir) {
                out.push(local.into_os_string());
            }
        }
    }

    if let Some(home) = user_home_dir() {
        let cargo_bin = home.join(".cargo/bin").join(AGENT_ULTRA_BIN);
        if let Some(path) = resolve_existing_binary(&cargo_bin) {
            out.push(path.into_os_string());
        }
    }

    out.push(OsString::from(AGENT_ULTRA_BIN));
    out
}

pub fn unique_targets(candidates: Vec<OsString>) -> Vec<OsString> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for candidate in candidates {
        let key = candidate.to_string_lossy().to_string();
        if seen.insert(key) {
            out.push(candidate);
        }
    }
    out
}

pub fn run_target(target: OsString, args: &[OsString]) -> Result<i32, String> {
    #[cfg(unix)]
    {
        let err = Command::new(&target).args(args).exec();
        Err(format!("{} ({})", target.to_string_lossy(), err))
    }

    #[cfg(not(unix))]
    {
        Command::new(&target)
            .args(args)
            .status()
            .map(|status| status.code().unwrap_or(1))
            .map_err(|err| format!("{} ({})", target.to_string_lossy(), err))
    }
}

fn sibling_agent_ultra(dir: &Path) -> Option<PathBuf> {
    resolve_existing_binary(&dir.join(AGENT_ULTRA_BIN))
}

fn resolve_existing_binary(base: &Path) -> Option<PathBuf> {
    if base.is_file() {
        return Some(base.to_path_buf());
    }
    #[cfg(windows)]
    {
        let with_exe = base.with_extension("exe");
        if with_exe.is_file() {
            return Some(with_exe);
        }
    }
    None
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}
