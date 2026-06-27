const INTERACTIVE_SESSION_LOCK_FILE: &str = "interactive.session.lock";
const INTERACTIVE_SESSION_LOCK_BYPASS_ENV: &str = "HERMES_ALLOW_PARALLEL_INTERACTIVE";

fn interactive_tty_error_message() -> &'static str {
    "interactive Hermes requires a terminal (TTY). Run `hermes-ultra setup` first, \
     use `hermes-ultra chat --query \"...\"` for non-interactive prompts, or run \
     `hermes-ultra doctor --deep --snapshot --bundle` for diagnostics."
}

fn require_interactive_tty() -> Result<(), AgentError> {
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        Ok(())
    } else {
        Err(AgentError::Config(interactive_tty_error_message().into()))
    }
}

fn interactive_lock_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli).join(INTERACTIVE_SESSION_LOCK_FILE)
}

fn read_interactive_lock_pid(path: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(pid) = trimmed.parse::<u32>() {
        return Some(pid);
    }
    let json: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let pid = json.get("pid")?.as_u64()?;
    u32::try_from(pid).ok()
}

#[cfg(unix)]
fn process_pid_is_alive(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    )
}

#[cfg(not(unix))]
fn process_pid_is_alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
#[derive(Debug, Clone)]
struct InteractivePidSnapshot {
    ppid: u32,
    tty: String,
    command: String,
}

#[cfg(unix)]
fn parse_pid_snapshot_line(line: &str) -> Option<InteractivePidSnapshot> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut parts = trimmed.split_whitespace();
    let ppid = parts.next()?.parse::<u32>().ok()?;
    let tty = parts.next()?.to_string();
    let command = parts.collect::<Vec<_>>().join(" ");
    if command.is_empty() {
        return None;
    }
    Some(InteractivePidSnapshot { ppid, tty, command })
}

#[cfg(unix)]
fn interactive_pid_snapshot(pid: u32) -> Option<InteractivePidSnapshot> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid=,tty=,command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout);
    parse_pid_snapshot_line(line.as_ref())
}

#[cfg(unix)]
fn looks_like_interactive_hermes_process(command: &str) -> bool {
    let cmd = command.to_ascii_lowercase();
    (cmd.contains("hermes-agent-ultra") || cmd.contains("hermes-ultra")) && !cmd.contains("gateway")
}

#[cfg(unix)]
fn interactive_lock_holder_is_reapable_orphan(pid: u32) -> bool {
    let snapshot = match interactive_pid_snapshot(pid) {
        Some(snapshot) => snapshot,
        None => return false,
    };
    // Reap only obvious abandoned interactive agents:
    // orphaned from shell (ppid=1) and detached from a terminal.
    looks_like_interactive_hermes_process(&snapshot.command)
        && snapshot.ppid == 1
        && (snapshot.tty == "??" || snapshot.tty == "?")
}

#[cfg(unix)]
fn reap_interactive_orphan(pid: u32) -> bool {
    let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    std::thread::sleep(std::time::Duration::from_millis(250));
    if !process_pid_is_alive(pid) {
        return true;
    }
    let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    std::thread::sleep(std::time::Duration::from_millis(150));
    !process_pid_is_alive(pid)
}

#[cfg(unix)]
fn reap_interactive_orphans_except(own_pid: u32) -> usize {
    let output = match std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return 0,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut reaped = 0usize;
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let Some(pid) = parts.next().and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        let Some(ppid) = parts.next().and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        if pid == own_pid || ppid != 1 {
            continue;
        }
        let command = parts.collect::<Vec<_>>().join(" ");
        if looks_like_interactive_hermes_process(&command) && reap_interactive_orphan(pid) {
            reaped = reaped.saturating_add(1);
        }
    }
    reaped
}

struct InteractiveSessionLockGuard {
    lock_path: PathBuf,
    pid: u32,
    _lock_file: std::fs::File,
}

impl InteractiveSessionLockGuard {
    fn acquire(cli: &Cli) -> Result<Option<Self>, AgentError> {
        if hermes_config::env_var_enabled(INTERACTIVE_SESSION_LOCK_BYPASS_ENV) {
            return Ok(None);
        }
        let lock_path = interactive_lock_path_for_cli(cli);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AgentError::Io(format!(
                    "failed to create lock parent {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }
        let own_pid = std::process::id();
        #[cfg(unix)]
        {
            let _ = reap_interactive_orphans_except(own_pid);
        }

        // Use create_new for atomic lock acquisition. This closes the race where
        // two interactive sessions read "no lock" and both write concurrently.
        let lock_file = loop {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(file) => break file,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if let Some(existing_pid) = read_interactive_lock_pid(&lock_path) {
                        if existing_pid != own_pid && process_pid_is_alive(existing_pid) {
                            #[cfg(unix)]
                            {
                                if interactive_lock_holder_is_reapable_orphan(existing_pid)
                                    && reap_interactive_orphan(existing_pid)
                                {
                                    let _ = std::fs::remove_file(&lock_path);
                                    continue;
                                }
                            }
                            return Err(AgentError::Config(format!(
                                "Another Hermes interactive session is running (PID {}). Close it first or set {}=1 to allow parallel sessions.",
                                existing_pid, INTERACTIVE_SESSION_LOCK_BYPASS_ENV
                            )));
                        }
                    }
                    let _ = std::fs::remove_file(&lock_path);
                    continue;
                }
                Err(err) => {
                    return Err(AgentError::Io(format!(
                        "failed to create interactive lock {}: {}",
                        lock_path.display(),
                        err
                    )));
                }
            }
        };

        let mut lock_file = lock_file;
        lock_file
            .write_all(format!("{}\n", own_pid).as_bytes())
            .map_err(|e| {
                AgentError::Io(format!(
                    "failed to write interactive lock {}: {}",
                    lock_path.display(),
                    e
                ))
            })?;
        let _ = lock_file.flush();

        Ok(Some(Self {
            lock_path,
            pid: own_pid,
            _lock_file: lock_file,
        }))
    }
}

impl Drop for InteractiveSessionLockGuard {
    fn drop(&mut self) {
        if let Some(current_pid) = read_interactive_lock_pid(&self.lock_path) {
            if current_pid == self.pid {
                let _ = std::fs::remove_file(&self.lock_path);
            }
        }
    }
}

/// Run the interactive REPL (default command).
async fn run_interactive(cli: Cli) -> Result<(), AgentError> {
    require_interactive_tty()?;
    let _session_lock = InteractiveSessionLockGuard::acquire(&cli)?;
    let app = App::new(cli).await?;
    hermes_cli::tui::run(app).await
}

fn run_kanban(args: Vec<String>) -> Result<(), AgentError> {
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    println!("{}", hermes_cli::commands::run_kanban_command(&arg_refs)?);
    Ok(())
}

#[derive(Debug, Clone)]
struct ResumeSessionPayload {
    resolved_id: String,
    source_path: PathBuf,
    session_id: String,
    model: Option<String>,
    personality: Option<String>,
    system_prompt: Option<String>,
    session_start: Option<String>,
    messages: Vec<hermes_core::Message>,
}

async fn run_resume(cli: Cli, requested_session_id: Option<String>) -> Result<(), AgentError> {
    require_interactive_tty()?;
    let _session_lock = InteractiveSessionLockGuard::acquire(&cli)?;
    let requested = requested_session_id.as_deref();
    let payload = match load_resume_payload(&cli, requested) {
        Ok(payload) => payload,
        Err(err) if should_resume_fallback_to_fresh(requested, &err) => {
            let mut app = App::new(cli).await?;
            app.push_ui_assistant(
                "No saved sessions found yet. Started a fresh session; future turns will autosave for `resume`.",
            );
            return hermes_cli::tui::run(app).await;
        }
        Err(err) => return Err(err),
    };
    let mut app = App::new(cli).await?;

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

fn load_resume_payload(
    cli: &Cli,
    requested: Option<&str>,
) -> Result<ResumeSessionPayload, AgentError> {
    let sessions_dir = hermes_state_root(cli).join("sessions");
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
    payload = follow_resume_compression_tip(payload)?;
    Ok(payload)
}

fn follow_resume_compression_tip(
    payload: ResumeSessionPayload,
) -> Result<ResumeSessionPayload, AgentError> {
    let Some(source_sessions_dir) = payload.source_path.parent() else {
        return Ok(payload);
    };
    let Some(state_root) = source_sessions_dir.parent() else {
        return Ok(payload);
    };
    let persistence = SessionPersistence::new(state_root);
    let tip = match persistence.resolve_resume_session_id(&payload.session_id) {
        Ok(tip) => tip,
        Err(err) => {
            tracing::debug!(
                "resume compression-tip resolution skipped for {}: {}",
                payload.session_id,
                err
            );
            return Ok(payload);
        }
    };
    if tip.trim().is_empty() || tip == payload.session_id {
        return Ok(payload);
    }
    match resolve_resume_session_file(source_sessions_dir, Some(&tip)) {
        Ok((resolved_id, source_path)) => parse_resume_payload_file(resolved_id, source_path),
        Err(err) => {
            tracing::debug!(
                "resume compression tip {} resolved from {} but snapshot lookup failed: {}",
                tip,
                payload.session_id,
                err
            );
            Ok(payload)
        }
    }
}

fn parse_resume_payload_file(
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
    let system_prompt = doc
        .get("system_prompt")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let session_start = doc
        .get("session_start")
        .and_then(|v| v.as_str())
        .or_else(|| {
            info.and_then(|v| v.get("created_at"))
                .and_then(|v| v.as_str())
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

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
        system_prompt,
        session_start,
        messages,
    })
}

fn legacy_sessions_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .map(|home| home.join(".hermes").join("sessions"))
}

fn resolve_resume_session_file_with_legacy_fallback(
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

fn resolve_latest_nonempty_session_file_with_legacy_fallback(
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

fn is_latest_resume_request(requested: Option<&str>) -> bool {
    let requested = requested.unwrap_or("latest").trim();
    requested.is_empty() || requested.eq_ignore_ascii_case("latest")
}

fn should_resume_fallback_to_fresh(requested: Option<&str>, err: &AgentError) -> bool {
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

fn resolve_latest_nonempty_session_file(
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

#[derive(Debug, Clone, Default)]
struct SessionFileSummary {
    message_count: usize,
    canonical: bool,
    session_id: Option<String>,
}

fn session_file_summary(path: &Path) -> Option<SessionFileSummary> {
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
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let canonical = !stem.is_empty()
        && session_id
            .as_deref()
            .is_some_and(|id| stem.eq_ignore_ascii_case(id));
    Some(SessionFileSummary {
        message_count,
        canonical,
        session_id,
    })
}

fn resume_session_id_match_score(
    query: &str,
    stem: &str,
    summary: Option<&SessionFileSummary>,
) -> Option<u8> {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return None;
    }
    let mut ids = vec![stem.trim().to_ascii_lowercase()];
    if let Some(session_id) = summary.and_then(|s| s.session_id.as_deref()) {
        ids.push(session_id.trim().to_ascii_lowercase());
    }
    if ids.iter().any(|id| id == &needle) {
        return Some(0);
    }
    if ids.iter().any(|id| id.starts_with(&needle)) {
        return Some(1);
    }
    if ids.iter().any(|id| id.contains(&needle)) {
        return Some(2);
    }
    None
}

fn resolve_resume_session_file_by_id_query(
    sessions_dir: &Path,
    requested: &str,
) -> Result<Option<(String, PathBuf)>, AgentError> {
    #[derive(Debug)]
    struct Candidate {
        score: u8,
        canonical: bool,
        modified: std::time::SystemTime,
        resolved_id: String,
        path: PathBuf,
    }

    let mut candidates = Vec::new();
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
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let summary = session_file_summary(&path);
        let Some(score) = resume_session_id_match_score(requested, stem, summary.as_ref()) else {
            continue;
        };
        let modified = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        candidates.push(Candidate {
            score,
            canonical: summary.as_ref().is_some_and(|s| s.canonical),
            modified,
            resolved_id: stem.to_string(),
            path,
        });
    }
    candidates.sort_by(|a, b| {
        a.score
            .cmp(&b.score)
            .then_with(|| b.canonical.cmp(&a.canonical))
            .then_with(|| b.modified.cmp(&a.modified))
            .then_with(|| a.resolved_id.cmp(&b.resolved_id))
    });
    Ok(candidates
        .into_iter()
        .next()
        .map(|candidate| (candidate.resolved_id, candidate.path)))
}

fn resolve_resume_session_file(
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
        // 2) newest canonical snapshot (may be startup empty snapshot)
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
        if let Some(found) = resolve_resume_session_file_by_id_query(sessions_dir, req)? {
            return Ok(found);
        }
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

fn parse_resume_messages(items: &[serde_json::Value]) -> Vec<hermes_core::Message> {
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

fn extract_session_objective(messages: &[hermes_core::Message]) -> Option<String> {
    const SESSION_OBJECTIVE_PREFIX: &str = "[SESSION_OBJECTIVE] ";
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

