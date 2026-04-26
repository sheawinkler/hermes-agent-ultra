use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{exit, Command};

fn candidate_targets() -> Vec<OsString> {
    let mut out: Vec<OsString> = Vec::new();

    if let Ok(explicit) = std::env::var("HERMES_ULTRA_BIN") {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            out.push(OsString::from(explicit));
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        let cargo_bin = PathBuf::from(home).join(".cargo/bin/hermes-agent-ultra");
        if cargo_bin.exists() {
            out.push(cargo_bin.into_os_string());
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
        match Command::new(&target).args(args.clone()).status() {
            Ok(status) => match status.code() {
                Some(code) => exit(code),
                None => exit(1),
            },
            Err(err) => {
                launch_errors.push(format!("{} ({})", target.to_string_lossy(), err));
            }
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
