use super::*;
use std::ffi::OsString;
use std::path::Path;
use tempfile::tempdir;

static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(future)
}

#[cfg(unix)]
fn pids_for_marker(marker: &str) -> Vec<u32> {
    let output = std::process::Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .expect("ps output");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(marker))
        .filter_map(|line| line.split_whitespace().next()?.parse::<u32>().ok())
        .collect()
}

#[cfg(unix)]
async fn wait_for_marker(marker: &str, present: bool, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let found = !pids_for_marker(marker).is_empty();
        if found == present {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let found = !pids_for_marker(marker).is_empty();
    found == present
}

struct EnvGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, original }
    }

    fn remove(key: &'static str) -> Self {
        let original = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

#[cfg(unix)]
#[test]
fn test_with_login_profile_sources_prepends_profile_loads() {
    let _lock = lock_env();
    let td = tempdir().unwrap();
    for file in [".profile", ".bash_profile", ".bashrc", ".zshrc"] {
        std::fs::write(td.path().join(file), "export HERMES_TEST=1\n").unwrap();
    }
    let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());
    let wrapped = with_login_profile_sources("echo hi", &[], true, &[], None);
    assert!(wrapped.contains("command -v bash"));
    assert!(wrapped.contains("exec bash -lc"));
    assert!(wrapped.contains(".bash_profile"));
    assert!(wrapped.contains(".bashrc"));
    assert!(wrapped.contains("command -v zsh"));
    assert!(wrapped.contains("exec zsh -lc"));
    assert!(wrapped.contains(".zshrc"));
    assert!(wrapped.contains("echo hi"));
}

#[cfg(unix)]
#[test]
fn test_with_login_profile_sources_prefers_user_shell_when_supported() {
    let _lock = lock_env();
    let _shell = EnvGuard::set("SHELL", "/bin/zsh");
    let wrapped = with_login_profile_sources("echo hi", &[], true, &[], None);
    let preferred = "if command -v zsh >/dev/null 2>&1; then exec zsh -lc";
    let fallback = "if command -v bash >/dev/null 2>&1; then exec bash -lc";
    let preferred_idx = wrapped.find(preferred).expect("preferred zsh branch");
    let fallback_idx = wrapped.find(fallback).expect("fallback bash branch");
    assert!(
        preferred_idx < fallback_idx,
        "preferred shell branch should come before fallback"
    );
}

#[test]
fn test_resolve_shell_init_files_auto_profile_before_bashrc() {
    let _lock = lock_env();
    let td = tempdir().unwrap();
    for file in [".profile", ".bash_profile", ".bashrc"] {
        std::fs::write(td.path().join(file), "export HERMES_TEST=1\n").unwrap();
    }
    let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());

    let resolved = resolve_shell_init_files_for_shell("bash", &[], true, None);
    let names = resolved
        .iter()
        .filter_map(|p| p.file_name().and_then(|name| name.to_str()))
        .collect::<Vec<_>>();
    assert_eq!(names, [".profile", ".bash_profile", ".bashrc"]);
}

#[test]
fn test_resolve_shell_init_files_explicit_list_wins_over_auto() {
    let _lock = lock_env();
    let td = tempdir().unwrap();
    std::fs::write(td.path().join(".bashrc"), "export FROM_BASHRC=1\n").unwrap();
    let custom = td.path().join("custom.sh");
    std::fs::write(&custom, "export FROM_CUSTOM=1\n").unwrap();
    let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());

    let resolved = resolve_shell_init_files_for_shell(
        "bash",
        &[custom.to_string_lossy().to_string()],
        true,
        None,
    );
    assert_eq!(resolved, [custom]);
}

#[test]
fn test_resolve_shell_init_files_auto_source_off_suppresses_defaults() {
    let _lock = lock_env();
    let td = tempdir().unwrap();
    std::fs::write(td.path().join(".bashrc"), "export FROM_BASHRC=1\n").unwrap();
    let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());

    let resolved = resolve_shell_init_files_for_shell("bash", &[], false, None);
    assert!(resolved.is_empty());
}

#[test]
fn test_resolve_shell_init_files_expands_home_and_env_vars() {
    let _lock = lock_env();
    let td = tempdir().unwrap();
    let rc_dir = td.path().join("rc");
    std::fs::create_dir_all(&rc_dir).unwrap();
    let custom = rc_dir.join("custom.sh");
    std::fs::write(&custom, "export FROM_CUSTOM=1\n").unwrap();
    let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());
    let _custom_dir = EnvGuard::set("CUSTOM_RC_DIR", rc_dir.to_string_lossy().as_ref());

    let home_resolved =
        resolve_shell_init_files_for_shell("bash", &["~/rc/custom.sh".to_string()], false, None);
    let env_resolved = resolve_shell_init_files_for_shell(
        "bash",
        &["${CUSTOM_RC_DIR}/custom.sh".to_string()],
        false,
        None,
    );
    assert_eq!(home_resolved.as_slice(), std::slice::from_ref(&custom));
    assert_eq!(env_resolved, [custom]);
}

#[test]
fn test_resolve_shell_init_files_uses_subprocess_home_override() {
    let _lock = lock_env();
    let real = tempdir().unwrap();
    let profile = tempdir().unwrap();
    std::fs::write(real.path().join(".bashrc"), "export REAL_HOME_RC=1\n").unwrap();
    std::fs::write(profile.path().join(".bashrc"), "export PROFILE_HOME_RC=1\n").unwrap();
    let _home = EnvGuard::set("HOME", real.path().to_string_lossy().as_ref());

    let resolved = resolve_shell_init_files_for_shell("bash", &[], true, Some(profile.path()));

    assert!(resolved
        .iter()
        .any(|p| p == &profile.path().join(".bashrc")));
    assert!(!resolved.iter().any(|p| p == &real.path().join(".bashrc")));
}

#[test]
fn test_subprocess_env_passthrough_parses_configured_and_env_values() {
    let _lock = lock_env();
    let _passthrough = EnvGuard::set(
        SUBPROCESS_ENV_PASSTHROUGH_VAR,
        "OPENAI_API_KEY,ANTHROPIC_TOKEN:BAD-NAME VALID_NAME",
    );
    let parsed = subprocess_env_passthrough_set(&["GOOGLE_API_KEY".to_string()]);
    assert!(parsed.contains("OPENAI_API_KEY"));
    assert!(parsed.contains("ANTHROPIC_TOKEN"));
    assert!(parsed.contains("VALID_NAME"));
    assert!(parsed.contains("GOOGLE_API_KEY"));
    assert!(!parsed.contains("BAD-NAME"));
}

#[cfg(unix)]
#[test]
fn test_shell_cleanup_keeps_configured_passthrough() {
    let wrapped = with_login_profile_sources(
        "echo hi",
        &[],
        true,
        &[
            "OPENAI_API_KEY".to_string(),
            "HERMES_GATEWAY_SECRET".to_string(),
        ],
        None,
    );
    assert!(wrapped.contains("HERMES_SUBPROCESS_ENV_PASSTHROUGH"));
    assert!(wrapped.contains("OPENAI_API_KEY"));
    assert!(wrapped.contains("HERMES_GATEWAY_SECRET"));
    assert!(wrapped
        .contains("${HERMES_SUBPROCESS_FORCE_TARGETS:-} ${HERMES_SUBPROCESS_ENV_PASSTHROUGH:-}"));
}

#[test]
fn test_profile_home_mode_sets_home_and_real_home() {
    let _lock = lock_env();
    let real = tempdir().unwrap();
    let hermes = tempdir().unwrap();
    let _home = EnvGuard::set("HOME", real.path().to_string_lossy().as_ref());
    let _real_home = EnvGuard::set("HERMES_REAL_HOME", real.path().to_string_lossy().as_ref());
    let _hermes_home = EnvGuard::set("HERMES_HOME", hermes.path().to_string_lossy().as_ref());
    let backend = LocalBackend::new_with_shell_init_and_home_mode(
        10,
        1_048_576,
        Vec::new(),
        false,
        TerminalHomeMode::Profile,
        Vec::new(),
    );

    let output = block_on(backend.execute_command(
        "printf '%s|%s' \"$HOME\" \"$HERMES_REAL_HOME\"",
        None,
        None,
        false,
        false,
    ))
    .unwrap();

    assert_eq!(
        output.stdout,
        format!(
            "{}|{}",
            hermes.path().join("home").display(),
            real.path().display()
        )
    );
}

#[test]
fn test_terminal_sanitizer_blocks_provider_env_by_default() {
    let _lock = lock_env();
    let _api = EnvGuard::set("OPENAI_API_KEY", "secret-value");
    let _passthrough = EnvGuard::remove(SUBPROCESS_ENV_PASSTHROUGH_VAR);
    let backend = LocalBackend::new_with_shell_init(10, 1_048_576, Vec::new(), true, Vec::new());

    let output = block_on(backend.execute_command(
        "printf '%s' \"${OPENAI_API_KEY-unset}\"",
        Some(10),
        None,
        false,
        false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "unset");
}

#[test]
fn test_terminal_config_passthrough_allows_blocklisted_env() {
    let _lock = lock_env();
    let _api = EnvGuard::set("OPENAI_API_KEY", "secret-value");
    let _passthrough = EnvGuard::remove(SUBPROCESS_ENV_PASSTHROUGH_VAR);
    let backend = LocalBackend::new_with_shell_init(
        10,
        1_048_576,
        Vec::new(),
        true,
        vec!["OPENAI_API_KEY".to_string()],
    );

    let output = block_on(backend.execute_command(
        "printf '%s' \"$OPENAI_API_KEY\"",
        Some(10),
        None,
        false,
        false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "secret-value");
}

#[test]
fn test_terminal_registry_env_passthrough_allows_prefix_blocked_env() {
    let _lock = lock_env();
    let _gateway = EnvGuard::set("HERMES_GATEWAY_SECRET", "gateway-secret");
    let _passthrough = EnvGuard::set(SUBPROCESS_ENV_PASSTHROUGH_VAR, "HERMES_GATEWAY_SECRET");
    let backend = LocalBackend::new_with_shell_init(10, 1_048_576, Vec::new(), true, Vec::new());

    let output = block_on(backend.execute_command(
        "printf '%s' \"$HERMES_GATEWAY_SECRET\"",
        Some(10),
        None,
        false,
        false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "gateway-secret");
}

#[test]
fn test_rewrite_compound_background_contract() {
    assert_eq!(rewrite_compound_background("A && B &"), "A && { B & }");
    assert_eq!(rewrite_compound_background("A || B &"), "A || { B & }");
    assert_eq!(
        rewrite_compound_background("A && B && C &"),
        "A && B && { C & }"
    );
    assert_eq!(
        rewrite_compound_background("cd /tmp && server &\nsleep 1"),
        "cd /tmp && { server & }\nsleep 1"
    );
    assert_eq!(rewrite_compound_background("sleep 5 &"), "sleep 5 &");
    assert_eq!(rewrite_compound_background("A && B | C &"), "A && B | C &");
    assert_eq!(
        rewrite_compound_background("A && B &>/dev/null &"),
        "A && { B &>/dev/null & }"
    );
    assert_eq!(
        rewrite_compound_background("echo 'A && B &'"),
        "echo 'A && B &'"
    );
    assert_eq!(
        rewrite_compound_background("   A && B &"),
        "   A && { B & }"
    );
    let once = rewrite_compound_background("A && B &");
    assert_eq!(rewrite_compound_background(&once), once);
}

#[cfg(unix)]
#[test]
fn test_foreground_process_group_avoids_fork_hook() {
    let source = include_str!("../local.rs");
    let forbidden = ["pre", "_exec"].concat();
    assert!(
        !source.contains(&forbidden),
        "{forbidden} fork hook must not be used in local foreground process setup"
    );
    assert!(source.contains("process_group(0)"));
}

#[cfg(not(unix))]
#[test]
fn test_with_login_profile_sources_is_passthrough_off_unix() {
    let wrapped = with_login_profile_sources("echo hi", &[], true, &[], None);
    assert_eq!(wrapped, "echo hi");
}

#[tokio::test]
async fn test_execute_command_echo() {
    let backend = LocalBackend::default();
    let output = backend
        .execute_command("echo hello", None, None, false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.trim().contains("hello"));
}

#[tokio::test]
async fn test_execute_command_with_stdin_data() {
    let backend = LocalBackend::default();
    let output = backend
        .execute_command_with_stdin("cat", None, None, false, false, Some("hello stdin"))
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.contains("hello stdin"));
}

#[tokio::test]
async fn test_execute_command_with_workdir() {
    let backend = LocalBackend::default();
    let output = backend
        .execute_command("pwd", None, Some("/tmp"), false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.trim().contains("/tmp"));
}

#[tokio::test]
async fn test_execute_command_sources_explicit_shell_init_file() {
    let td = tempdir().unwrap();
    let init = td.path().join("custom-init.sh");
    std::fs::write(
        &init,
        "export HERMES_SHELL_INIT_PROBE=probe-ok\nexport PATH=\"/opt/shell-init-probe/bin:$PATH\"\n",
    )
    .unwrap();
    let backend = LocalBackend::new_with_shell_init(
        10,
        1_048_576,
        vec![init.to_string_lossy().to_string()],
        false,
        Vec::new(),
    );

    let output = backend
        .execute_command(
            "printf '%s|%s' \"$HERMES_SHELL_INIT_PROBE\" \"$PATH\"",
            Some(10),
            Some(td.path().to_string_lossy().as_ref()),
            false,
            false,
        )
        .await
        .unwrap();

    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.contains("probe-ok"));
    assert!(output.stdout.contains("/opt/shell-init-probe/bin"));
}

#[tokio::test]
async fn test_execute_command_timeout() {
    let backend = LocalBackend::new(1, 1_048_576);
    let result = backend
        .execute_command("sleep 30", None, None, false, false)
        .await;
    assert!(result.is_err());
    match result {
        Err(AgentError::Timeout(_)) => {}
        _ => panic!("Expected timeout error"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn test_foreground_child_group_is_killed_when_future_is_aborted() {
    let backend = Arc::new(LocalBackend::new(60, 1_048_576));
    let marker = format!("hermes_abort_guard_{}", std::process::id());
    let command = format!("python3 -c 'import time; time.sleep(60)' {marker}");
    let task = {
        let backend = backend.clone();
        tokio::spawn(async move {
            backend
                .execute_command(&command, Some(60), None, false, false)
                .await
        })
    };

    assert!(
        wait_for_marker(&marker, true, std::time::Duration::from_secs(5)).await,
        "test setup failed to observe marker process"
    );
    task.abort();
    let _ = task.await;
    assert!(
        wait_for_marker(&marker, false, std::time::Duration::from_secs(5)).await,
        "foreground process marker survived future cancellation: {:?}",
        pids_for_marker(&marker)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_plain_shell_background_child_does_not_hang_foreground_collection() {
    let backend = LocalBackend::new(10, 1_048_576);
    let marker = "hermes_bg_nohang_marker";
    let probe = "hermes_bg_nohang_probe";
    let command = format!("python3 -c 'import time; time.sleep(60)' {probe} & echo {marker}");
    let started = std::time::Instant::now();
    let output = backend
        .execute_command(&command, Some(5), None, false, false)
        .await
        .unwrap();
    let elapsed = started.elapsed();
    let _ = std::process::Command::new("pkill")
        .args(["-f", probe])
        .status();

    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "foreground collection hung for {elapsed:?}"
    );
    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.contains(marker), "stdout={:?}", output.stdout);
}

#[tokio::test]
async fn test_foreground_collection_preserves_multibyte_utf8_boundaries() {
    let backend = LocalBackend::new(10, 100_000);
    let command = "python3 -c 'import sys; sys.stdout.buffer.write(chr(0x65e5).encode(\"utf-8\") * 10000); sys.stdout.buffer.write(b\"\\n\")'";
    let output = backend
        .execute_command(command, Some(10), None, false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout.matches('\u{65e5}').count(), 10_000);
    assert!(!output.stdout.contains("binary output detected"));
}

#[tokio::test]
async fn test_foreground_collection_preserves_high_volume_line_output() {
    let backend = LocalBackend::new(10, 1_048_576);
    let output = backend
        .execute_command("seq 1 3000", Some(10), None, false, false)
        .await
        .unwrap();
    let lines = output.stdout.trim().split('\n').collect::<Vec<_>>();
    assert_eq!(output.exit_code, 0);
    assert_eq!(lines.len(), 3000);
    assert_eq!(lines.first().copied(), Some("1"));
    assert_eq!(lines.last().copied(), Some("3000"));
}

#[tokio::test]
async fn test_foreground_collection_replaces_invalid_utf8() {
    let backend = LocalBackend::new(10, 1_048_576);
    let command = "python3 -c 'import sys; sys.stdout.buffer.write(b\"before \"); sys.stdout.buffer.write(b\"\\xff\\xfe\"); sys.stdout.buffer.write(b\" after\\n\")'";
    let output = backend
        .execute_command(command, Some(5), None, false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.contains("before"));
    assert!(output.stdout.contains("after"));
    assert!(output.stdout.contains('\u{fffd}'));
    assert!(!output.stdout.contains("binary output detected"));
}

#[tokio::test]
async fn test_execute_command_failure() {
    let backend = LocalBackend::default();
    let output = backend
        .execute_command("exit 42", None, None, false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 42);
}

#[tokio::test]
async fn test_write_and_read_file() {
    let dir = std::env::temp_dir().join("hermes_test_write_read");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("test_file.txt");
    let path_str = path.to_string_lossy().to_string();

    let backend = LocalBackend::default();

    backend
        .write_file(&path_str, "hello\nworld\nfoo\nbar")
        .await
        .unwrap();
    let content = backend.read_file(&path_str, None, None).await.unwrap();
    assert_eq!(content, "hello\nworld\nfoo\nbar");

    // Test with offset
    let content = backend.read_file(&path_str, Some(1), None).await.unwrap();
    assert_eq!(content, "world\nfoo\nbar");

    // Test with offset and limit
    let content = backend
        .read_file(&path_str, Some(1), Some(2))
        .await
        .unwrap();
    assert_eq!(content, "world\nfoo");

    // Cleanup
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn test_relative_file_paths_use_terminal_cwd() {
    let _lock = lock_env();
    let td = tempdir().unwrap();
    let terminal_cwd = td.path().join("worktree");
    std::fs::create_dir_all(&terminal_cwd).unwrap();
    let _cwd_guard = EnvGuard::set("TERMINAL_CWD", terminal_cwd.to_string_lossy().as_ref());
    let backend = LocalBackend::default();

    block_on(backend.write_file("nested/file.txt", "from terminal cwd")).unwrap();

    let expected = terminal_cwd.join("nested/file.txt");
    assert_eq!(
        std::fs::read_to_string(&expected).unwrap(),
        "from terminal cwd"
    );
    assert_eq!(
        block_on(backend.read_file("nested/file.txt", None, None)).unwrap(),
        "from terminal cwd"
    );
}

#[tokio::test]
async fn test_file_exists() {
    let backend = LocalBackend::default();

    // A path that should exist
    assert!(backend.file_exists("/tmp").await.unwrap());

    // A path that should not exist
    assert!(!backend
        .file_exists("/tmp/hermes_nonexistent_test_file_xyz")
        .await
        .unwrap());
}

#[test]
fn test_resolve_path_rejects_tilde_injection() {
    let malicious = "~; echo PWNED > /tmp/hermes_local_backend_injection";
    let resolved = resolve_path(malicious).unwrap();
    assert_eq!(resolved, Path::new(malicious));
    assert!(!Path::new("/tmp/hermes_local_backend_injection").exists());
}

#[test]
fn test_resolve_path_expands_tilde_username_with_suffix() {
    let _lock = lock_env();
    let Some(user) = current_username() else {
        return;
    };
    let Some(home) = home_dir() else {
        return;
    };

    let resolved = resolve_path(&format!("~{user}/workspace/file.txt")).unwrap();
    assert!(resolved.starts_with(&home));
    assert!(resolved.ends_with("workspace/file.txt"));
}

#[test]
fn test_write_file_expands_tilde_home() {
    let _lock = lock_env();
    let td = tempdir().unwrap();
    let _home_guard = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());
    let backend = LocalBackend::default();
    let file = "~/nested/path/test.txt";

    block_on(backend.write_file(file, "ok")).unwrap();
    let expanded = td.path().join("nested/path/test.txt");
    let content = std::fs::read_to_string(&expanded).unwrap();
    assert_eq!(content, "ok");
}

#[test]
fn test_execute_command_strips_gateway_env_vars() {
    let _lock = lock_env();
    let _token_guard = EnvGuard::set("TOOL_GATEWAY_USER_TOKEN", "should-not-leak");
    let _managed_guard = EnvGuard::set("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
    let _http_guard = EnvGuard::set("HERMES_HTTP_API_KEY", "secret-http-key");
    let _safe_guard = EnvGuard::set("SAFE_PASSTHRU_TEST", "ok");
    let backend = LocalBackend::default();

    let output = block_on(backend.execute_command(
            "printf '%s|%s|%s|%s' \"${TOOL_GATEWAY_USER_TOKEN:-}\" \"${HERMES_ENABLE_NOUS_MANAGED_TOOLS:-}\" \"${HERMES_HTTP_API_KEY:-}\" \"${SAFE_PASSTHRU_TEST:-}\"",
            None,
            None,
            false,
            false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "|||ok");
}

#[test]
fn test_execute_command_strips_provider_tool_and_gateway_env_vars() {
    let _lock = lock_env();
    let _openai_key = EnvGuard::set("OPENAI_API_KEY", "sk-should-not-leak");
    let _openai_base = EnvGuard::set("OPENAI_BASE_URL", "http://localhost:8000/v1");
    let _bedrock_bearer = EnvGuard::set("AWS_BEARER_TOKEN_BEDROCK", "bedrock-secret");
    let _github = EnvGuard::set("GITHUB_TOKEN", "ghp-secret");
    let _modal = EnvGuard::set("MODAL_TOKEN_SECRET", "modal-secret");
    let _gateway = EnvGuard::set("GATEWAY_ALLOWED_USERS", "alice,bob");
    let _safe_guard = EnvGuard::set("SAFE_PASSTHRU_TEST", "ok");
    let backend = LocalBackend::default();

    let output = block_on(backend.execute_command(
            "printf '%s|%s|%s|%s|%s|%s|%s' \"${OPENAI_API_KEY:-}\" \"${OPENAI_BASE_URL:-}\" \"${AWS_BEARER_TOKEN_BEDROCK:-}\" \"${GITHUB_TOKEN:-}\" \"${MODAL_TOKEN_SECRET:-}\" \"${GATEWAY_ALLOWED_USERS:-}\" \"${SAFE_PASSTHRU_TEST:-}\"",
            None,
            None,
            false,
            false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "||||||ok");
}

#[test]
fn test_execute_command_preserves_general_aws_credentials() {
    let _lock = lock_env();
    let _access_key = EnvGuard::set("AWS_ACCESS_KEY_ID", "AKIAIOSFODNN7EXAMPLE");
    let _secret_key = EnvGuard::set("AWS_SECRET_ACCESS_KEY", "aws-secret");
    let _session = EnvGuard::set("AWS_SESSION_TOKEN", "aws-session");
    let backend = LocalBackend::default();

    let output = block_on(backend.execute_command(
            "printf '%s|%s|%s' \"${AWS_ACCESS_KEY_ID:-}\" \"${AWS_SECRET_ACCESS_KEY:-}\" \"${AWS_SESSION_TOKEN:-}\"",
            None,
            None,
            false,
            false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "AKIAIOSFODNN7EXAMPLE|aws-secret|aws-session");
    for var in [
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_PROFILE",
        "AWS_DEFAULT_REGION",
        "AWS_REGION",
        "AWS_SHARED_CREDENTIALS_FILE",
        "AWS_CONFIG_FILE",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "AWS_ROLE_ARN",
    ] {
        assert!(
            !should_strip_subprocess_env(var),
            "{var} must not be in the Hermes subprocess blocklist"
        );
    }
}

#[test]
fn test_execute_command_force_prefix_reinjects_blocked_var() {
    let _lock = lock_env();
    let _blocked = EnvGuard::set("OPENAI_API_KEY", "sk-should-not-leak");
    let _forced = EnvGuard::set("_HERMES_FORCE_OPENAI_API_KEY", "sk-explicit");
    let backend = LocalBackend::default();

    let output = block_on(backend.execute_command(
        "printf '%s|%s' \"${OPENAI_API_KEY:-}\" \"${_HERMES_FORCE_OPENAI_API_KEY:-}\"",
        None,
        None,
        false,
        false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "sk-explicit|");
}

#[test]
fn test_execute_command_cleans_profile_reintroduced_blocked_vars() {
    let _lock = lock_env();
    let td = tempdir().unwrap();
    std::fs::write(
        td.path().join(".profile"),
        "export OPENAI_API_KEY=from-profile\nexport SAFE_PASSTHRU_TEST=ok\n",
    )
    .unwrap();
    let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());
    let backend = LocalBackend::default();

    let output = block_on(backend.execute_command(
        "printf '%s|%s' \"${OPENAI_API_KEY:-}\" \"${SAFE_PASSTHRU_TEST:-}\"",
        None,
        None,
        false,
        false,
    ))
    .unwrap();

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "|ok");
}

#[test]
fn test_subprocess_path_appends_homebrew_when_path_is_minimal() {
    let normalized = normalize_subprocess_path(Some("/some/custom/bin"));
    assert!(normalized.contains("/some/custom/bin"));
    assert!(normalized.contains("/usr/bin"));
    assert!(normalized.contains("/opt/homebrew/bin"));
    assert!(normalized.contains("/opt/homebrew/sbin"));
}

#[test]
fn test_subprocess_path_preserves_full_path() {
    assert_eq!(
        normalize_subprocess_path(Some("/usr/bin:/bin")),
        "/usr/bin:/bin"
    );
}

#[tokio::test]
async fn test_background_process_lifecycle_with_stdin() {
    let backend = LocalBackend::new(10, 1_048_576);
    let started = backend
        .execute_command("cat", None, None, true, false)
        .await
        .unwrap();
    assert_eq!(started.exit_code, 0);
    let payload: Value = serde_json::from_str(&started.stdout).expect("valid start payload");
    let session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .expect("session_id")
        .to_string();

    let write = backend
        .write_process_stdin(&session_id, "hello from stdin\n")
        .await
        .unwrap();
    assert_eq!(write["status"], "ok");

    let close = backend.close_process_stdin(&session_id).await.unwrap();
    assert_eq!(close["status"], "ok");

    let wait = backend.wait_process(&session_id, Some(20)).await.unwrap();
    if wait["status"] == "timeout" {
        let poll = backend.poll_process(&session_id).await.unwrap();
        let _ = backend.kill_process(&session_id).await;
        panic!("background process did not exit after closing stdin: wait={wait}, poll={poll}");
    }
    assert_eq!(wait["status"], "exited");
    assert!(wait["output"]
        .as_str()
        .unwrap_or_default()
        .contains("hello from stdin"));
}

#[tokio::test]
async fn test_background_process_not_found_contract() {
    let backend = LocalBackend::default();
    let poll = backend.poll_process("proc_missing").await.unwrap();
    assert_eq!(poll["status"], "not_found");

    let log = backend
        .read_process_log("proc_missing", None, None)
        .await
        .unwrap();
    assert_eq!(log["status"], "not_found");

    let kill = backend.kill_process("proc_missing").await.unwrap();
    assert_eq!(kill["status"], "not_found");
}
