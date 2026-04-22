//! Integration tests for terminal backends.
//!
//! Tests command execution, file operations, and timeout handling
//! using the LocalBackend (which doesn't require external services).

use hermes_core::{AgentError, TerminalBackend};
use hermes_environments::LocalBackend;

// ---------------------------------------------------------------------------
// LocalBackend: execute_command
// ---------------------------------------------------------------------------

#[tokio::test]
async fn local_execute_simple_echo() {
    let backend = LocalBackend::default();
    let output = backend
        .execute_command("echo hello world", None, None, false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.contains("hello world"));
}

#[tokio::test]
async fn local_execute_with_workdir() {
    let backend = LocalBackend::default();
    let output = backend
        .execute_command("pwd", None, Some("/tmp"), false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    // macOS may resolve /tmp to /private/tmp
    assert!(
        output.stdout.trim().contains("/tmp"),
        "Expected /tmp in output, got: {}",
        output.stdout.trim()
    );
}

#[tokio::test]
async fn local_execute_with_timeout() {
    let backend = LocalBackend::new(2, 1_048_576);
    let result = backend
        .execute_command("sleep 30", Some(1), None, false, false)
        .await;
    assert!(result.is_err());
    match result {
        Err(AgentError::Timeout(_)) => {} // expected
        other => panic!("Expected Timeout error, got: {:?}", other),
    }
}

#[tokio::test]
async fn local_execute_background() {
    let backend = LocalBackend::default();
    let output = backend
        .execute_command("sleep 60", None, None, true, false)
        .await
        .unwrap();
    // Background mode returns immediately with structured session metadata.
    assert_eq!(output.exit_code, 0);
    let payload: serde_json::Value =
        serde_json::from_str(&output.stdout).expect("background mode should return json payload");
    assert_eq!(payload.get("exit_code").and_then(|v| v.as_i64()), Some(0));
    assert_eq!(
        payload.get("output").and_then(|v| v.as_str()),
        Some("Background process started")
    );
    assert!(payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
async fn local_execute_nonzero_exit() {
    let backend = LocalBackend::default();
    let output = backend
        .execute_command("exit 42", None, None, false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 42);
}

#[tokio::test]
async fn local_execute_stderr() {
    let backend = LocalBackend::default();
    let output = backend
        .execute_command("echo error >&2", None, None, false, false)
        .await
        .unwrap();
    assert!(output.stderr.contains("error"));
}

// ---------------------------------------------------------------------------
// LocalBackend: file operations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn local_write_read_file() {
    let backend = LocalBackend::default();
    let dir = std::env::temp_dir().join("hermes_backend_test");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("test_rw.txt");
    let path_str = path.to_string_lossy().to_string();

    backend
        .write_file(&path_str, "line1\nline2\nline3")
        .await
        .unwrap();

    let content = backend.read_file(&path_str, None, None).await.unwrap();
    assert_eq!(content, "line1\nline2\nline3");

    // Read with offset
    let content = backend.read_file(&path_str, Some(1), None).await.unwrap();
    assert_eq!(content, "line2\nline3");

    // Read with offset + limit
    let content = backend
        .read_file(&path_str, Some(1), Some(1))
        .await
        .unwrap();
    assert_eq!(content, "line2");

    // Cleanup
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[tokio::test]
async fn local_file_exists() {
    let backend = LocalBackend::default();
    assert!(backend.file_exists("/tmp").await.unwrap());
    assert!(!backend
        .file_exists("/tmp/hermes_nonexistent_xyz_12345")
        .await
        .unwrap());
}

#[tokio::test]
async fn local_write_creates_parent_dirs() {
    let backend = LocalBackend::default();
    let dir = std::env::temp_dir().join("hermes_backend_test/nested/deep");
    let path = dir.join("file.txt");
    let path_str = path.to_string_lossy().to_string();

    backend
        .write_file(&path_str, "nested content")
        .await
        .unwrap();
    assert!(backend.file_exists(&path_str).await.unwrap());

    let content = backend.read_file(&path_str, None, None).await.unwrap();
    assert_eq!(content, "nested content");

    // Cleanup
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join("hermes_backend_test/nested"));
}

// ---------------------------------------------------------------------------
// LocalBackend: output truncation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn local_output_truncation() {
    // Create a backend with a very small max output size
    let backend = LocalBackend::new(120, 50);
    let output = backend
        .execute_command(
            "python3 -c 'print(\"x\" * 200)' 2>/dev/null || echo $(printf 'x%.0s' {1..200})",
            None,
            None,
            false,
            false,
        )
        .await
        .unwrap();
    // Output should be truncated to max_output_size
    assert!(output.stdout.len() <= 50);
}
