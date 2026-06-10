//! Session resume logic for the CLI.
//!
//! Handles loading, parsing, and resolving session files for the `resume` subcommand.

use hermes_core::AgentError;
use hermes_core::MessageRole;
use std::path::{Path, PathBuf};

/// Payload extracted from a saved session file for resumption.
#[derive(Debug, Clone)]
pub(crate) struct ResumeSessionPayload {
    pub(crate) resolved_id: String,
    pub(crate) source_path: PathBuf,
    pub(crate) session_id: String,
    pub(crate) model: Option<String>,
    pub(crate) personality: Option<String>,
    pub(crate) messages: Vec<hermes_core::Message>,
}

/// Run the `resume` subcommand: load a saved session and launch the TUI.
pub(crate) async fn run_resume(
    cli: crate::Cli,
    requested_session_id: Option<String>,
) -> Result<(), AgentError> {
    let _session_lock = crate::interactive_lock::InteractiveSessionLockGuard::acquire(
        &crate::hermes_state_root(&cli),
    )?;
    let requested = requested_session_id.as_deref();
    let payload = match load_resume_payload(&cli, requested) {
        Ok(payload) => payload,
        Err(err) if should_resume_fallback_to_fresh(requested, &err) => {
            let mut app = hermes_cli::App::new(cli).await?;
            app.push_ui_assistant(
                "No saved sessions found yet. Started a fresh session; future turns will autosave for `resume`.",
            );
            return hermes_cli::tui::run(app).await;
        }
        Err(err) => return Err(err),
    };
    let mut app = hermes_cli::App::new(cli).await?;

    if let Some(model) = payload.model.clone().filter(|m| !m.trim().is_empty()) {
        if model != app.current_model {
            app.switch_model(&model);
        } else {
            app.current_model = model;
        }
    }

    app.current_personality = payload
        .personality
        .clone()
        .filter(|name| !name.trim().is_empty());
    app.session_id = payload.session_id.clone();
    app.messages = payload.messages;
    app.ui_messages.clear();
    app.input_history.clear();
    app.history_index = 0;
    app.session_objective = extract_session_objective(&app.messages);
    app.push_ui_assistant(format!(
        "Resumed session `{}` from {} ({} messages).",
        payload.resolved_id,
        payload.source_path.display(),
        app.messages.len()
    ));

    hermes_cli::tui::run(app).await
}

/// Load a resume payload from a session file, with legacy fallback and
/// empty-message upgrade logic.
pub(crate) fn load_resume_payload(
    cli: &crate::Cli,
    requested: Option<&str>,
) -> Result<ResumeSessionPayload, AgentError> {
    let sessions_dir = crate::hermes_state_root(cli).join("sessions");
    let (resolved_id, source_path) =
        resolve_resume_session_file_with_legacy_fallback(&sessions_dir, requested)?;
    let mut payload = parse_resume_payload_file(resolved_id, source_path)?;
    if is_latest_resume_request(requested) && payload.messages.is_empty() {
        if let Ok((fallback_id, fallback_path)) =
            resolve_latest_nonempty_session_file_with_legacy_fallback(&sessions_dir)
        {
            if fallback_path != payload.source_path {
                if let Ok(fallback_payload) =
                    parse_resume_payload_file(fallback_id.clone(), fallback_path.clone())
                {
                    tracing::info!(
                        "resume latest selected non-empty snapshot {} from {}",
                        fallback_id,
                        fallback_path.display()
                    );
                    payload = fallback_payload;
                }
            }
        }
    }
    Ok(payload)
}

/// Parse a session JSON file into a `ResumeSessionPayload`.
pub(crate) fn parse_resume_payload_file(
    resolved_id: String,
    source_path: PathBuf,
) -> Result<ResumeSessionPayload, AgentError> {
    let raw = std::fs::read_to_string(&source_path).map_err(|e| {
        AgentError::Io(format!(
            "Failed to read session file {}: {}",
            source_path.display(),
            e
        ))
    })?;
    let doc: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        AgentError::Config(format!(
            "Failed to parse session file {}: {}",
            source_path.display(),
            e
        ))
    })?;

    let info = doc.get("session_info");
    let session_id = info
        .and_then(|v| v.get("session_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| resolved_id.clone());
    let model = info
        .and_then(|v| v.get("model"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            doc.get("model")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
    let personality = info
        .and_then(|v| v.get("personality"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            doc.get("personality")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });

    let messages_value = doc
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            AgentError::Config(format!(
                "Session file {} does not contain a valid `messages` array.",
                source_path.display()
            ))
        })?;

    let messages = parse_resume_messages(messages_value);

    Ok(ResumeSessionPayload {
        resolved_id,
        source_path,
        session_id,
        model,
        personality,
        messages,
    })
}

/// Return the legacy `~/.hermes/sessions` directory, if `HOME` is set.
pub(crate) fn legacy_sessions_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .map(|home| home.join(".hermes").join("sessions"))
}

/// Resolve a session file, falling back to the legacy sessions directory.
pub(crate) fn resolve_resume_session_file_with_legacy_fallback(
    sessions_dir: &Path,
    requested: Option<&str>,
) -> Result<(String, PathBuf), AgentError> {
    match resolve_resume_session_file(sessions_dir, requested) {
        Ok(found) => Ok(found),
        Err(primary_err) => {
            let Some(legacy_dir) = legacy_sessions_dir() else {
                return Err(primary_err);
            };
            if legacy_dir == sessions_dir || !legacy_dir.exists() {
                return Err(primary_err);
            }
            resolve_resume_session_file(&legacy_dir, requested).map_err(|_| primary_err)
        }
    }
}

/// Resolve the latest non-empty session file, falling back to legacy.
pub(crate) fn resolve_latest_nonempty_session_file_with_legacy_fallback(
    sessions_dir: &Path,
) -> Result<(String, PathBuf), AgentError> {
    match resolve_latest_nonempty_session_file(sessions_dir) {
        Ok(found) => Ok(found),
        Err(primary_err) => {
            let Some(legacy_dir) = legacy_sessions_dir() else {
                return Err(primary_err);
            };
            if legacy_dir == sessions_dir || !legacy_dir.exists() {
                return Err(primary_err);
            }
            resolve_latest_nonempty_session_file(&legacy_dir).map_err(|_| primary_err)
        }
    }
}

/// Check whether the user requested the latest session.
pub(crate) fn is_latest_resume_request(requested: Option<&str>) -> bool {
    let requested = requested.unwrap_or("latest").trim();
    requested.is_empty() || requested.eq_ignore_ascii_case("latest")
}

/// Determine whether a resume error should fall back to a fresh session.
pub(crate) fn should_resume_fallback_to_fresh(requested: Option<&str>, err: &AgentError) -> bool {
    if !is_latest_resume_request(requested) {
        return false;
    }
    match err {
        AgentError::Config(msg) | AgentError::Io(msg) => {
            msg.contains("No saved sessions found") || msg.contains("No sessions directory found")
        }
        _ => false,
    }
}

/// Find the most recently modified non-empty canonical session file.
pub(crate) fn resolve_latest_nonempty_session_file(
    sessions_dir: &Path,
) -> Result<(String, PathBuf), AgentError> {
    let mut candidates: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    let rd = std::fs::read_dir(sessions_dir).map_err(|e| {
        AgentError::Io(format!(
            "Failed to read sessions directory {}: {}",
            sessions_dir.display(),
            e
        ))
    })?;
    for entry in rd.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if !path.extension().map(|ext| ext == "json").unwrap_or(false) {
            continue;
        }
        let modified = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        candidates.push((path, modified));
    }
    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    // Prefer canonical snapshots: file stem == session_info.session_id.
    for (path, _) in candidates {
        if let Some(summary) = session_file_summary(&path) {
            if summary.message_count > 0 && summary.canonical {
                let resolved_id = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "latest".to_string());
                return Ok((resolved_id, path));
            }
        }
    }
    Err(AgentError::Config(format!(
        "No non-empty saved sessions found in {}.",
        sessions_dir.display()
    )))
}

/// Summary of a session file: message count and whether the file stem matches
/// the embedded session_id.
#[derive(Debug, Clone, Default)]
pub(crate) struct SessionFileSummary {
    pub(crate) message_count: usize,
    pub(crate) canonical: bool,
}

/// Extract a summary from a session file without fully deserializing every message.
pub(crate) fn session_file_summary(path: &Path) -> Option<SessionFileSummary> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return None;
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return None;
    };
    let message_count = doc
        .get("messages")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    let stem = path
        .file_stem()
        .and_then(|v| v.to_str())
        .map(str::trim)
        .unwrap_or_default();
    let session_id = doc
        .get("session_info")
        .and_then(|v| v.get("session_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default();
    let canonical =
        !stem.is_empty() && !session_id.is_empty() && stem.eq_ignore_ascii_case(session_id);
    Some(SessionFileSummary {
        message_count,
        canonical,
    })
}

/// Resolve a session file by ID or pick the latest.
pub(crate) fn resolve_resume_session_file(
    sessions_dir: &Path,
    requested: Option<&str>,
) -> Result<(String, PathBuf), AgentError> {
    let req = requested.unwrap_or("latest").trim();
    if req.is_empty() || req.eq_ignore_ascii_case("latest") {
        let mut candidates: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        let rd = std::fs::read_dir(sessions_dir).map_err(|e| {
            AgentError::Io(format!(
                "Failed to read sessions directory {}: {}",
                sessions_dir.display(),
                e
            ))
        })?;
        for entry in rd.filter_map(|entry| entry.ok()) {
            let path = entry.path();
            if !path.extension().map(|ext| ext == "json").unwrap_or(false) {
                continue;
            }
            let modified = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            candidates.push((path, modified));
        }
        candidates.sort_by(|a, b| b.1.cmp(&a.1));
        // 1) newest canonical non-empty snapshot
        for (path, _) in &candidates {
            if let Some(summary) = session_file_summary(path) {
                if summary.canonical && summary.message_count > 0 {
                    let resolved_id = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "latest".to_string());
                    return Ok((resolved_id, path.clone()));
                }
            }
        }
        // 2) newest canonical snapshot (may be startup stub)
        for (path, _) in &candidates {
            if let Some(summary) = session_file_summary(path) {
                if summary.canonical {
                    let resolved_id = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "latest".to_string());
                    return Ok((resolved_id, path.clone()));
                }
            }
        }
        let Some((path, _)) = candidates.into_iter().next() else {
            return Err(AgentError::Config(format!(
                "No saved sessions found in {}.",
                sessions_dir.display()
            )));
        };
        let resolved_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "latest".to_string());
        return Ok((resolved_id, path));
    }

    if req.contains('/') || req.contains('\\') {
        return Err(AgentError::Config(
            "Session ID should be a file stem, not a path.".into(),
        ));
    }

    let mut path = sessions_dir.join(req);
    if path.extension().is_none() {
        path.set_extension("json");
    }
    if !path.exists() {
        return Err(AgentError::Config(format!(
            "Session '{}' not found at {}.",
            req,
            path.display()
        )));
    }

    let resolved_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| req.to_string());
    Ok((resolved_id, path))
}

/// Parse a JSON array of message objects into `hermes_core::Message` values.
pub(crate) fn parse_resume_messages(items: &[serde_json::Value]) -> Vec<hermes_core::Message> {
    let mut messages = Vec::new();
    for item in items {
        let role = item
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("user")
            .trim()
            .to_ascii_lowercase();
        let content = item
            .get("content")
            .or_else(|| item.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        match role.as_str() {
            "system" => {
                if !content.is_empty() {
                    messages.push(hermes_core::Message::system(content));
                }
            }
            "assistant" => {
                if let Some(tool_calls_val) = item.get("tool_calls") {
                    if let Ok(tool_calls) =
                        serde_json::from_value::<Vec<hermes_core::ToolCall>>(tool_calls_val.clone())
                    {
                        messages.push(hermes_core::Message::assistant_with_tool_calls(
                            if content.is_empty() {
                                None
                            } else {
                                Some(content.clone())
                            },
                            tool_calls,
                        ));
                        continue;
                    }
                }
                if !content.is_empty() {
                    messages.push(hermes_core::Message::assistant(content));
                }
            }
            "tool" => {
                let tool_call_id = item
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool_call");
                if !content.is_empty() {
                    messages.push(hermes_core::Message::tool_result(tool_call_id, content));
                }
            }
            _ => {
                if !content.is_empty() {
                    messages.push(hermes_core::Message::user(content));
                }
            }
        }
    }
    messages
}

const SESSION_OBJECTIVE_PREFIX: &str = "[SESSION_OBJECTIVE] ";

/// Extract a session objective from system messages.
pub(crate) fn extract_session_objective(messages: &[hermes_core::Message]) -> Option<String> {
    messages.iter().find_map(|message| {
        if message.role != MessageRole::System {
            return None;
        }
        let content = message.content.as_deref()?.trim();
        content
            .strip_prefix(SESSION_OBJECTIVE_PREFIX)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("env lock poisoned")
    }

    fn cli_for_temp_state_root(temp_root: &std::path::Path) -> crate::Cli {
        use clap::Parser;
        crate::Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            temp_root.to_str().expect("utf8 path"),
        ])
    }

    #[test]
    fn resolve_resume_session_file_prefers_latest_modified_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = crate::hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

        let old = sessions_dir.join("old-session.json");
        let new = sessions_dir.join("new-session.json");
        std::fs::write(&old, r#"{"messages":[{"role":"user","content":"old"}]}"#)
            .expect("write old session");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&new, r#"{"messages":[{"role":"user","content":"new"}]}"#)
            .expect("write new session");

        let (resolved, path) =
            resolve_resume_session_file(&sessions_dir, None).expect("resolve latest");
        assert_eq!(resolved, "new-session");
        assert_eq!(path, new);
    }

    #[test]
    fn resolve_resume_session_file_latest_prefers_canonical_session_stem() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = crate::hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

        let canonical = sessions_dir.join("c0ffee00-0000-4000-8000-000000000001.json");
        std::fs::write(
            &canonical,
            r#"{
  "session_info": {"session_id":"c0ffee00-0000-4000-8000-000000000001","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
        )
        .expect("write canonical");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let named = sessions_dir.join("newest.json");
        std::fs::write(
            &named,
            r#"{
  "session_info": {"session_id":"snap-prune","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"snapshot payload"}]
}"#,
        )
        .expect("write named artifact");

        let (resolved, path) =
            resolve_resume_session_file(&sessions_dir, None).expect("resolve latest");
        assert_eq!(resolved, "c0ffee00-0000-4000-8000-000000000001");
        assert_eq!(path, canonical);
    }

    #[test]
    fn should_resume_fallback_to_fresh_only_for_latest_missing_state() {
        let latest_missing = AgentError::Config("No saved sessions found in /tmp".to_string());
        assert!(should_resume_fallback_to_fresh(None, &latest_missing));
        assert!(should_resume_fallback_to_fresh(
            Some("latest"),
            &latest_missing
        ));
        assert!(!should_resume_fallback_to_fresh(
            Some("abc123"),
            &latest_missing
        ));

        let other_error = AgentError::Config("Session 'abc123' not found".to_string());
        assert!(!should_resume_fallback_to_fresh(None, &other_error));
    }

    #[test]
    fn load_resume_payload_restores_metadata_and_messages() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = crate::hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        let session_path = sessions_dir.join("abc123.json");
        std::fs::write(
            &session_path,
            r#"{
  "session_info": {
    "session_id": "session-xyz",
    "model": "nous:openai/gpt-5.5-pro",
    "personality": "technical"
  },
  "messages": [
    {"role":"System","content":"[SESSION_OBJECTIVE] Keep context fresh"},
    {"role":"User","content":"hello"},
    {"role":"Assistant","content":"world"}
  ]
}"#,
        )
        .expect("write session");

        let payload = load_resume_payload(&cli, Some("abc123")).expect("load payload");
        assert_eq!(payload.resolved_id, "abc123");
        assert_eq!(payload.session_id, "session-xyz");
        assert_eq!(payload.model.as_deref(), Some("nous:openai/gpt-5.5-pro"));
        assert_eq!(payload.personality.as_deref(), Some("technical"));
        assert_eq!(payload.messages.len(), 3);
        assert!(matches!(
            payload.messages[0].role,
            hermes_core::MessageRole::System
        ));
        assert!(matches!(
            payload.messages[1].role,
            hermes_core::MessageRole::User
        ));
        assert!(matches!(
            payload.messages[2].role,
            hermes_core::MessageRole::Assistant
        ));
    }

    #[test]
    fn load_resume_payload_falls_back_to_legacy_sessions_dir() {
        let _guard = env_lock();
        let prev_home = std::env::var("HOME").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        let fake_home = tmp.path().join("fake-home");
        let legacy_sessions = fake_home.join(".hermes").join("sessions");
        std::fs::create_dir_all(&legacy_sessions).expect("create legacy sessions dir");
        let legacy_path = legacy_sessions.join("legacy-abc.json");
        std::fs::write(
            &legacy_path,
            r#"{
  "session_info": {
    "session_id": "legacy-session",
    "model": "nous:nousresearch/hermes-4-70b"
  },
  "messages": [
    {"role":"User","content":"from-legacy"}
  ]
}"#,
        )
        .expect("write legacy session");

        hermes_cli::env_vars::set_var("HOME", &fake_home);
        let state_root = tmp.path().join("ultra-state");
        let cli = cli_for_temp_state_root(&state_root);
        let payload = load_resume_payload(&cli, Some("legacy-abc")).expect("load payload");
        assert_eq!(payload.resolved_id, "legacy-abc");
        assert_eq!(payload.session_id, "legacy-session");
        assert_eq!(payload.messages.len(), 1);
        assert!(payload.source_path.starts_with(&legacy_sessions));

        match prev_home {
            Some(home) => hermes_cli::env_vars::set_var("HOME", home),
            None => hermes_cli::env_vars::remove_var("HOME"),
        }
    }

    #[test]
    fn load_resume_payload_accepts_empty_messages_for_startup_stub() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = crate::hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        let session_path = sessions_dir.join("stub-empty.json");
        std::fs::write(
            &session_path,
            r#"{
  "session_info": {
    "session_id": "stub-empty",
    "model": "nous:nousresearch/hermes-4-70b"
  },
  "messages": []
}"#,
        )
        .expect("write stub session");

        let payload = load_resume_payload(&cli, Some("stub-empty")).expect("load payload");
        assert_eq!(payload.resolved_id, "stub-empty");
        assert_eq!(payload.session_id, "stub-empty");
        assert_eq!(
            payload.model.as_deref(),
            Some("nous:nousresearch/hermes-4-70b")
        );
        assert_eq!(payload.messages.len(), 0);
    }

    #[test]
    fn load_resume_payload_latest_prefers_nonempty_snapshot_over_newer_stub() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = crate::hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

        let non_empty = sessions_dir.join("history-real.json");
        std::fs::write(
            &non_empty,
            r#"{
  "session_info": {"session_id":"history-real","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"hello"},{"role":"Assistant","content":"world"}]
}"#,
        )
        .expect("write non-empty session");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let stub = sessions_dir.join("startup-stub.json");
        std::fs::write(
            &stub,
            r#"{
  "session_info": {"session_id":"startup-stub","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
        )
        .expect("write stub session");

        let payload = load_resume_payload(&cli, None).expect("load payload");
        assert_eq!(payload.resolved_id, "history-real");
        assert_eq!(payload.messages.len(), 2);
        assert_eq!(payload.source_path, non_empty);
    }

    #[test]
    fn load_resume_payload_latest_falls_back_to_legacy_nonempty_when_primary_stub_only() {
        let _guard = env_lock();
        let prev_home = std::env::var("HOME").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        let fake_home = tmp.path().join("fake-home");
        let legacy_sessions = fake_home.join(".hermes").join("sessions");
        std::fs::create_dir_all(&legacy_sessions).expect("create legacy sessions dir");

        let legacy_non_empty = legacy_sessions.join("legacy-rich.json");
        std::fs::write(
            &legacy_non_empty,
            r#"{
  "session_info": {"session_id":"legacy-rich","model":"nous:nousresearch/hermes-4-70b"},
  "messages":[{"role":"User","content":"from legacy"}]
}"#,
        )
        .expect("write legacy non-empty session");

        hermes_cli::env_vars::set_var("HOME", &fake_home);
        let state_root = tmp.path().join("ultra-state");
        let cli = cli_for_temp_state_root(&state_root);
        let sessions_dir = crate::hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        std::fs::write(
            sessions_dir.join("stub-only.json"),
            r#"{
  "session_info": {"session_id":"stub-only","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
        )
        .expect("write primary stub");

        let payload = load_resume_payload(&cli, None).expect("load payload");
        assert_eq!(payload.resolved_id, "legacy-rich");
        assert_eq!(payload.messages.len(), 1);
        assert!(payload.source_path.starts_with(&legacy_sessions));

        match prev_home {
            Some(home) => hermes_cli::env_vars::set_var("HOME", home),
            None => hermes_cli::env_vars::remove_var("HOME"),
        }
    }
}
