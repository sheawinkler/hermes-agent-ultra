use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{exit, Command};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

fn candidate_targets() -> Vec<OsString> {
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
            let local = dir.join("hermes-agent-ultra");
            if local.exists() {
                out.push(local.into_os_string());
            }
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        let cargo_bin = PathBuf::from(home).join(".cargo/bin/hermes-agent-ultra");
        if cargo_bin.exists() {
            out.push(cargo_bin.into_os_string());
        }
    }

    out.push(OsString::from("hermes-agent-ultra"));
    out
}

fn unique_targets(candidates: Vec<OsString>) -> Vec<OsString> {
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

fn run_target(target: OsString, args: &[OsString]) -> Result<i32, String> {
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

fn main() {
    let mut args = std::env::args_os();
    let _ = args.next();
    let args: Vec<OsString> = args.collect();

    let targets = unique_targets(candidate_targets());
    if targets.is_empty() {
        eprintln!("Failed to locate hermes-agent-ultra target binary.");
        exit(1);
    }

    let mut launch_errors: Vec<String> = Vec::new();
    for target in targets {
        match run_target(target, &args) {
            Ok(code) => exit(code),
            Err(err) => launch_errors.push(err),
        }
    }

    if launch_errors.is_empty() {
        eprintln!("Failed to launch hermes-agent-ultra.");
    } else {
        eprintln!(
            "Failed to launch hermes-agent-ultra. Attempts: {}",
            launch_errors.join(" ; ")
        );
    };
    exit(1);
}
