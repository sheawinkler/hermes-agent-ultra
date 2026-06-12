mod ultra_wrapper;

use std::ffi::OsString;
use std::process::exit;

use ultra_wrapper::{candidate_targets, run_target, unique_targets};

fn main() {
    let (version, commit) = hermes_core::startup_commit_info();
    eprintln!(
        "[WARN] hermes-ultra wrapper startup commit info: version={} commit={}",
        version, commit
    );
    let mut args = std::env::args_os();
    let _ = args.next();
    let args: Vec<OsString> = args.collect();

    let targets = unique_targets(candidate_targets());
    if targets.is_empty() {
        eprintln!("Failed to locate hermes-agent-ultra target binary.");
        exit(1);
    }

    for target in &targets {
        eprintln!(
            "[INFO] hermes-ultra wrapper: trying {}",
            target.to_string_lossy()
        );
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
