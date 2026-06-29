use super::*;

// Terminal (placeholder)
// ============================================================================

#[tauri::command]
pub async fn terminal_start(
    app: AppHandle,
    window: Window,
    payload: Option<serde_json::Value>,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let id = generate_token();
    let (command, args, shell_name) = terminal_shell_command();
    let event_target = terminal_event_target(Some(window.label())).to_string();
    let cwd = safe_terminal_cwd(
        payload
            .as_ref()
            .and_then(|value| value.get("cwd"))
            .and_then(|value| value.as_str()),
    );
    let cols = payload
        .as_ref()
        .and_then(|value| value.get("cols"))
        .and_then(|value| value.as_u64())
        .map(|value| value as u16)
        .unwrap_or(80)
        .max(2);
    let rows = payload
        .as_ref()
        .and_then(|value| value.get("rows"))
        .and_then(|value| value.as_u64())
        .map(|value| value as u16)
        .unwrap_or(24)
        .max(2);

    let pty_system = portable_pty::native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("Failed to open PTY: {}", e))?;

    let mut builder = CommandBuilder::new(command);
    builder.args(args);
    builder.cwd(cwd.clone());
    configure_terminal_env(&mut builder);

    let master = pair.master;
    let reader = master
        .try_clone_reader()
        .map_err(|e| format!("Failed to clone PTY reader: {}", e))?;
    let writer = master
        .take_writer()
        .map_err(|e| format!("Failed to take PTY writer: {}", e))?;
    let child = pair
        .slave
        .spawn_command(builder)
        .map_err(|e| format!("Failed to spawn PTY shell: {}", e))?;

    let session = Arc::new(TerminalSession {
        master: StdMutex::new(master),
        child: StdMutex::new(child),
        writer: StdMutex::new(writer),
        event_target,
        alive: AtomicBool::new(true),
        exited: AtomicBool::new(false),
    });

    {
        let mut sessions = state
            .terminal_sessions
            .lock()
            .map_err(|_| "Failed to access terminal sessions".to_string())?;
        sessions.insert(id.clone(), session.clone());
    }

    spawn_terminal_reader(
        app.clone(),
        id.clone(),
        reader,
        state.terminal_sessions.clone(),
        session.clone(),
    );

    Ok(serde_json::json!({
        "cwd": cwd.to_string_lossy().to_string(),
        "id": id,
        "shell": shell_name,
    }))
}

#[tauri::command]
pub async fn terminal_write(
    id: String,
    data: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let session = {
        let sessions = state
            .terminal_sessions
            .lock()
            .map_err(|_| "Failed to access terminal sessions".to_string())?;
        sessions.get(&id).cloned()
    };

    let Some(session) = session else {
        return Ok(false);
    };

    let mut writer = session
        .writer
        .lock()
        .map_err(|_| "Failed to access terminal writer".to_string())?;
    writer
        .write_all(data.as_bytes())
        .and_then(|_| writer.flush())
        .map_err(|e| format!("Failed to write terminal input: {}", e))?;
    Ok(true)
}

#[tauri::command]
pub async fn terminal_resize(
    id: String,
    size: serde_json::Value,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let session = {
        let sessions = state
            .terminal_sessions
            .lock()
            .map_err(|_| "Failed to access terminal sessions".to_string())?;
        sessions.get(&id).cloned()
    };

    let Some(session) = session else {
        return Ok(false);
    };

    let cols = size
        .get("cols")
        .and_then(|value| value.as_u64())
        .map(|value| value as u16)
        .unwrap_or(80)
        .max(2);
    let rows = size
        .get("rows")
        .and_then(|value| value.as_u64())
        .map(|value| value as u16)
        .unwrap_or(24)
        .max(2);

    let master = session
        .master
        .lock()
        .map_err(|_| "Failed to access terminal pty".to_string())?;
    master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("Failed to resize terminal: {}", e))?;
    Ok(true)
}

#[tauri::command]
pub async fn terminal_dispose(id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let session = {
        let mut sessions = state
            .terminal_sessions
            .lock()
            .map_err(|_| "Failed to access terminal sessions".to_string())?;
        sessions.remove(&id)
    };

    let Some(session) = session else {
        return Ok(false);
    };

    dispose_terminal_session_impl(session.as_ref());

    Ok(true)
}
