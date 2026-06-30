use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_repo_file(path: &str) -> String {
    let full_path = repo_root().join(path);
    fs::read_to_string(&full_path)
        .unwrap_or_else(|err| panic!("failed reading {}: {err}", full_path.display()))
}

#[test]
fn subprocess_no_window_helper_exposes_std_and_tokio_chokepoints() {
    let helper = read_repo_file("crates/hermes-core/src/subprocess.rs");
    assert!(helper.contains("WINDOWS_CREATE_NO_WINDOW"));
    assert!(helper.contains("windows_no_window_creation_flags(existing: u32)"));
    assert!(helper.contains("impl CommandNoWindowExt for std::process::Command"));
    assert!(helper.contains("impl CommandNoWindowExt for tokio::process::Command"));
    assert!(helper.contains("self.creation_flags(windows_no_window_creation_flags(0));"));
}

#[test]
fn backend_helper_subprocesses_opt_into_windows_no_window_launch() {
    let required = [
        (
            "crates/hermes-environments/src/local.rs",
            "cmd.suppress_windows_console();",
        ),
        (
            "crates/hermes-environments/src/docker.rs",
            ".suppress_windows_console()",
        ),
        (
            "crates/hermes-environments/src/singularity.rs",
            ".suppress_windows_console()",
        ),
        (
            "crates/hermes-environments/src/ssh.rs",
            "command.suppress_windows_console();",
        ),
        (
            "crates/hermes-mcp/src/transport.rs",
            "cmd.suppress_windows_console();",
        ),
        (
            "crates/hermes-gateway/src/hooks.rs",
            "cmd.suppress_windows_console();",
        ),
        (
            "crates/hermes-gateway/src/gateway/routing_methods.rs",
            ".suppress_windows_console()",
        ),
        (
            "crates/hermes-gateway/src/platforms/matrix/adapter_impl/sync_parsing.rs",
            ".suppress_windows_console()",
        ),
        (
            "crates/hermes-tools/src/backends/computer_use.rs",
            ".suppress_windows_console()",
        ),
        (
            "crates/hermes-tools/src/backends/code_execution.rs",
            "cmd.suppress_windows_console();",
        ),
        (
            "crates/hermes-tools/src/backends/tts.rs",
            "cmd.suppress_windows_console();",
        ),
        (
            "crates/hermes-tools/src/backends/video.rs",
            ".suppress_windows_console()",
        ),
        (
            "crates/hermes-tools/src/terminal_requirements.rs",
            ".suppress_windows_console()",
        ),
        (
            "crates/hermes-tools/src/teams_pipeline.rs",
            ".suppress_windows_console()",
        ),
        (
            "crates/hermes-agent/src/coding_context.rs",
            ".suppress_windows_console()",
        ),
        (
            "crates/hermes-agent/src/context_references.rs",
            ".suppress_windows_console()",
        ),
        (
            "crates/hermes-agent/src/memory_plugins/byterover.rs",
            ".suppress_windows_console()",
        ),
        (
            "crates/hermes-cli/src/commands/command_cli_plugins_memory_mcp.rs",
            ".suppress_windows_console()",
        ),
        (
            "crates/hermes-cli/src/commands/command_background_browser/triage_background.rs",
            "cmd.suppress_windows_console();",
        ),
        (
            "crates/hermes-cli/src/main/doctor_routes.rs",
            ".suppress_windows_console()",
        ),
        (
            "crates/hermes-cron/src/runner.rs",
            "command.suppress_windows_console();",
        ),
    ];

    for (path, needle) in required {
        let text = read_repo_file(path);
        assert!(
            text.contains(needle),
            "{path} must keep backend helper subprocesses routed through Windows no-window launch"
        );
    }
}

#[test]
fn detached_chrome_debug_launch_preserves_existing_flags_and_adds_no_window() {
    let text = read_repo_file("crates/hermes-cli/src/commands/command_background_browser.rs");
    assert!(text.contains("windows_no_window_creation_flags("));
    assert!(text.contains("0x00000008 | 0x00000200"));
}
