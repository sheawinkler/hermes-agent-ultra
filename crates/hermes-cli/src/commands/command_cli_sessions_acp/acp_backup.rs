const ACP_MULTIMODAL_PREFIX: &str = "__hermes_acp_parts_json__:";

fn looks_like_openai_parts(parts: &[serde_json::Value]) -> bool {
    !parts.is_empty()
        && parts.iter().all(|part| {
            part.as_object()
                .and_then(|obj| obj.get("type"))
                .and_then(|v| v.as_str())
                .is_some()
        })
}

fn flatten_openai_parts_to_text(parts: &[serde_json::Value]) -> String {
    let mut chunks: Vec<String> = Vec::new();
    for part in parts {
        let Some(obj) = part.as_object() else {
            continue;
        };
        let kind = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "text" => {
                if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        chunks.push(text.to_string());
                    }
                }
            }
            "image_url" | "input_image" => {
                let url = obj
                    .get("image_url")
                    .and_then(|v| v.get("url"))
                    .and_then(|v| v.as_str())
                    .or_else(|| obj.get("image_url").and_then(|v| v.as_str()))
                    .or_else(|| obj.get("url").and_then(|v| v.as_str()))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !url.is_empty() {
                    chunks.push(format!("[Attached image]\nURL: {url}"));
                }
            }
            _ => {
                if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        chunks.push(text.to_string());
                    }
                }
            }
        }
    }
    chunks.join("\n")
}

fn acp_history_to_messages(
    history: &[serde_json::Value],
    fallback_user_text: &str,
) -> Vec<hermes_core::Message> {
    let mut messages = Vec::new();

    for item in history {
        let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let content_value = item.get("content").or_else(|| item.get("text"));
        let content = match content_value {
            Some(serde_json::Value::String(s)) => s.to_string(),
            Some(serde_json::Value::Array(parts)) if looks_like_openai_parts(parts) => {
                if role == "user" {
                    match serde_json::to_string(parts) {
                        Ok(serialized) => format!("{ACP_MULTIMODAL_PREFIX}{serialized}"),
                        Err(_) => flatten_openai_parts_to_text(parts),
                    }
                } else {
                    flatten_openai_parts_to_text(parts)
                }
            }
            Some(serde_json::Value::Object(obj)) => obj
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            _ => String::new(),
        };

        match role {
            "system" if !content.is_empty() => messages.push(hermes_core::Message::system(content)),
            "user" if !content.is_empty() => messages.push(hermes_core::Message::user(content)),
            "assistant" => {
                if let Some(tool_calls_val) = item.get("tool_calls") {
                    if let Ok(tool_calls) =
                        serde_json::from_value::<Vec<hermes_core::ToolCall>>(tool_calls_val.clone())
                    {
                        let assistant = hermes_core::Message::assistant_with_tool_calls(
                            if content.is_empty() {
                                None
                            } else {
                                Some(content)
                            },
                            tool_calls,
                        );
                        messages.push(assistant);
                        continue;
                    }
                }
                if !content.is_empty() {
                    messages.push(hermes_core::Message::assistant(content));
                }
            }
            "tool" if !content.is_empty() => {
                let tool_call_id = item
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool_call");
                messages.push(hermes_core::Message::tool_result(tool_call_id, content));
            }
            _ => {}
        }
    }

    let has_user_tail = messages
        .last()
        .map(|m| matches!(m.role, hermes_core::MessageRole::User))
        .unwrap_or(false);
    if !has_user_tail && !fallback_user_text.trim().is_empty() {
        messages.push(hermes_core::Message::user(fallback_user_text));
    }

    messages
}

fn acp_tool_arguments(arguments: &str) -> Option<serde_json::Value> {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(
        serde_json::from_str(trimmed)
            .unwrap_or_else(|_| serde_json::Value::String(arguments.to_string())),
    )
}

fn acp_events_from_agent_messages(
    session_id: &str,
    messages: &[hermes_core::Message],
) -> Vec<hermes_acp::AcpEvent> {
    let mut events = Vec::new();
    let mut tool_names_by_id: HashMap<String, String> = HashMap::new();
    let mut generated_ids = 0u64;

    for message in messages {
        match message.role {
            hermes_core::MessageRole::Assistant => {
                for tool_call in message.tool_calls.as_deref().unwrap_or(&[]) {
                    let tool_call_id = if tool_call.id.trim().is_empty() {
                        generated_ids = generated_ids.saturating_add(1);
                        format!("tc-{:08x}", generated_ids)
                    } else {
                        tool_call.id.clone()
                    };
                    tool_names_by_id.insert(tool_call_id.clone(), tool_call.function.name.clone());
                    events.push(hermes_acp::AcpEvent::tool_call_start(
                        session_id,
                        &tool_call_id,
                        &tool_call.function.name,
                        acp_tool_arguments(&tool_call.function.arguments),
                    ));
                }
            }
            hermes_core::MessageRole::Tool => {
                let Some(tool_call_id) = message.tool_call_id.as_deref() else {
                    continue;
                };
                let tool_name = tool_names_by_id
                    .get(tool_call_id)
                    .cloned()
                    .or_else(|| message.name.clone())
                    .unwrap_or_else(|| "tool".to_string());
                events.push(hermes_acp::AcpEvent::tool_call_complete(
                    session_id,
                    tool_call_id,
                    &tool_name,
                    message.content.clone(),
                ));
                if tool_name == "todo" {
                    if let Some(entries) =
                        hermes_acp::plan_entries_from_todo_result(message.content.as_deref())
                    {
                        events.push(hermes_acp::AcpEvent::plan_update(session_id, entries));
                    }
                }
            }
            _ => {}
        }
    }

    events
}

fn acp_usage_from_agent_usage(usage: &hermes_core::UsageStats) -> hermes_acp::Usage {
    hermes_acp::Usage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
        total_tokens: usage.total_tokens,
        thought_tokens: None,
        cached_read_tokens: None,
    }
}

struct CliAcpPromptExecutor {
    config: Arc<hermes_config::GatewayConfig>,
    tool_registry: Arc<hermes_tools::ToolRegistry>,
    interrupts: Arc<Mutex<HashMap<String, hermes_agent::InterruptController>>>,
}

impl CliAcpPromptExecutor {
    fn current_tool_schemas(&self) -> Vec<hermes_core::ToolSchema> {
        hermes_tool_planning::resolve_platform_tool_schemas(
            self.config.as_ref(),
            "cli",
            &self.tool_registry,
        )
    }
}

fn acp_stream_callbacks(
    session_id: &str,
    callback_events: Arc<Mutex<Vec<hermes_acp::AcpEvent>>>,
) -> hermes_agent::AgentCallbacks {
    let thought_events = callback_events.clone();
    let thought_session_id = session_id.to_string();
    let stream_events = callback_events;
    let stream_session_id = session_id.to_string();
    hermes_agent::AgentCallbacks {
        on_thinking: Some(Box::new(move |thinking: &str| {
            if thinking.trim().is_empty() {
                return;
            }
            if let Ok(mut events) = thought_events.lock() {
                events.push(hermes_acp::AcpEvent::agent_thought_chunk(
                    &thought_session_id,
                    thinking,
                ));
            }
        })),
        on_stream_delta: Some(Box::new(move |delta: &str| {
            if delta.is_empty() {
                return;
            }
            if let Ok(mut events) = stream_events.lock() {
                events.push(hermes_acp::AcpEvent::message_delta(
                    &stream_session_id,
                    delta,
                ));
            }
        })),
        ..hermes_agent::AgentCallbacks::default()
    }
}

#[async_trait::async_trait]
impl hermes_acp::AcpPromptExecutor for CliAcpPromptExecutor {
    async fn execute_prompt(
        &self,
        session: &hermes_acp::SessionState,
        user_text: &str,
        history: &[serde_json::Value],
    ) -> Result<hermes_acp::PromptExecutionOutput, String> {
        let model = session
            .model
            .clone()
            .or_else(|| self.config.model.clone())
            .unwrap_or_else(|| "dynamic".to_string());

        let provider = crate::app::build_provider(&self.config, &model);
        let mut agent_config = crate::app::build_agent_config(&self.config, &model);
        agent_config.session_id = Some(session.session_id.clone());

        let agent_tools = Arc::new(crate::app::bridge_tool_registry(&self.tool_registry));
        let interrupt = hermes_agent::InterruptController::new();
        if let Ok(mut active) = self.interrupts.lock() {
            active.insert(session.session_id.clone(), interrupt.clone());
        }
        let callback_events: Arc<Mutex<Vec<hermes_acp::AcpEvent>>> =
            Arc::new(Mutex::new(Vec::new()));
        let callbacks = acp_stream_callbacks(&session.session_id, callback_events.clone());
        let agent = hermes_agent::attach_discovered_memory(
            hermes_agent::AgentLoop::with_interrupt(agent_config, agent_tools, provider, interrupt)
                .with_callbacks(callbacks),
        );
        let messages = acp_history_to_messages(history, user_text);

        let result = agent.run(messages, Some(self.current_tool_schemas())).await;
        if let Ok(mut active) = self.interrupts.lock() {
            active.remove(&session.session_id);
        }
        let result = result.map_err(|e| e.to_string())?;
        let response_text = result
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, hermes_core::MessageRole::Assistant))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();

        let usage = result.usage.as_ref().map(acp_usage_from_agent_usage);
        let mut events = callback_events
            .lock()
            .map(|mut events| std::mem::take(&mut *events))
            .unwrap_or_default();
        events.extend(acp_events_from_agent_messages(
            &session.session_id,
            &result.messages,
        ));

        Ok(hermes_acp::PromptExecutionOutput {
            response_text,
            usage,
            total_turns: Some(result.total_turns),
            events,
        })
    }

    fn steer_prompt(
        &self,
        session: &hermes_acp::SessionState,
        guidance: &str,
    ) -> Result<bool, String> {
        let controller = self
            .interrupts
            .lock()
            .map_err(|_| "ACP interrupt registry poisoned".to_string())?
            .get(&session.session_id)
            .cloned();
        if let Some(controller) = controller {
            controller.interrupt(Some(hermes_agent::format_steer_marker(guidance)));
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AcpSetupDependencyCheck {
    dependency: &'static str,
    command: &'static str,
    interactive: bool,
    available: bool,
}

fn acp_command_exists(command: &str) -> bool {
    command_on_path(command)
}

fn acp_setup_browser_dependency_checks<F>(
    assume_yes: bool,
    mut command_exists: F,
) -> Result<Vec<AcpSetupDependencyCheck>, hermes_core::AgentError>
where
    F: FnMut(&str) -> bool,
{
    let interactive = !assume_yes;
    let mut checks = Vec::new();

    for (dependency, command) in [("node", "node"), ("browser", "agent-browser")] {
        let available = command_exists(command);
        checks.push(AcpSetupDependencyCheck {
            dependency,
            command,
            interactive,
            available,
        });
        if !available {
            return Err(hermes_core::AgentError::Config(format!(
                "ACP browser setup requires {dependency} dependency command `{command}`. Install it, then rerun `hermes acp setup-browser{}`.",
                if assume_yes { " --yes" } else { "" }
            )));
        }
    }

    Ok(checks)
}

/// Handle `hermes acp [action]`.
pub async fn handle_cli_acp(
    action: Option<String>,
    assume_yes: bool,
) -> Result<(), hermes_core::AgentError> {
    match action.as_deref().unwrap_or("status") {
        "start" => {
            let config = hermes_config::load_config(None)
                .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;

            let model = config
                .model
                .clone()
                .unwrap_or_else(|| "dynamic".to_string());
            let max_turns = config.max_turns as usize;

            println!(
                "Starting ACP server (model={}, max_turns={})...",
                model, max_turns
            );

            let tool_registry = Arc::new(hermes_tools::ToolRegistry::new());
            let terminal_backend = crate::terminal_backend::build_terminal_backend(&config);
            let skill_store = Arc::new(hermes_skills::FileSkillStore::new(
                hermes_config::skills_dir(),
            ));
            let skill_provider: Arc<dyn hermes_core::SkillProvider> =
                Arc::new(hermes_skills::SkillManager::new(skill_store));
            hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
            crate::runtime_tool_wiring::wire_stdio_clarify_backend(&tool_registry);
            let cron_data_dir = hermes_config::cron_dir();
            std::fs::create_dir_all(&cron_data_dir)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            let cron_scheduler = Arc::new(build_runtime_cron_scheduler(
                &config,
                &model,
                cron_data_dir,
                &tool_registry,
            ));
            cron_scheduler
                .load_persisted_jobs()
                .await
                .map_err(|e| hermes_core::AgentError::Config(format!("cron load: {e}")))?;
            cron_scheduler.start().await;
            crate::runtime_tool_wiring::wire_cron_scheduler_backend(&tool_registry, cron_scheduler);
            let mcp_manager = Arc::new(tokio::sync::Mutex::new(hermes_mcp::McpManager::new(
                tool_registry.clone(),
            )));

            let prompt_executor = Arc::new(CliAcpPromptExecutor {
                config: Arc::new(config.clone()),
                tool_registry: tool_registry.clone(),
                interrupts: Arc::new(Mutex::new(HashMap::new())),
            });

            let session_manager = Arc::new(hermes_acp::SessionManager::new());
            let event_sink = Arc::new(hermes_acp::EventSink::default());
            let permission_store = Arc::new(hermes_acp::PermissionStore::new());
            let handler = Arc::new(
                hermes_acp::HermesAcpHandler::new(
                    session_manager.clone(),
                    event_sink.clone(),
                    permission_store.clone(),
                )
                .with_mcp_components(tool_registry, mcp_manager)
                .with_prompt_executor(prompt_executor),
            );
            let server = hermes_acp::AcpServer::with_components(
                handler,
                session_manager,
                event_sink,
                permission_store,
            );

            server
                .run()
                .await
                .map_err(|e| hermes_core::AgentError::Io(format!("ACP server error: {}", e)))?;
        }
        "check" => {
            println!("Hermes ACP check OK");
        }
        "version" | "--version" => {
            handle_cli_version()?;
        }
        "setup" => {
            println!("ACP setup is handled by the Rust model/provider setup flow.");
            println!("Run `hermes acp --setup` or `hermes model` to configure a provider/model.");
        }
        "setup-browser" | "setup_browser" => {
            let checks = acp_setup_browser_dependency_checks(assume_yes, acp_command_exists)?;
            for check in checks {
                println!(
                    "ACP browser dependency {} (`{}`): OK{}",
                    check.dependency,
                    check.command,
                    if check.interactive {
                        ""
                    } else {
                        " (non-interactive)"
                    }
                );
            }
            println!("Hermes ACP browser setup OK");
        }
        "status" => {
            println!("ACP server: not running");
            println!("ACP runs as a stdio JSON-RPC server in the foreground.");
            println!("Start with `hermes acp start`.");
        }
        "stop" => {
            println!("ACP stop is not a separate command in stdio mode.");
            println!("If running, stop it by closing the parent process or sending Ctrl+C.");
        }
        "restart" => {
            println!("ACP restart in stdio mode is equivalent to stop + start.");
            println!("Use:");
            println!("  1) Stop the current process (Ctrl+C)");
            println!("  2) Run `hermes acp start`");
        }
        other => {
            println!("Unknown ACP action '{}'.", other);
            println!(
                "Available actions: start, status, stop, restart, check, setup, setup-browser, version"
            );
        }
    }
    Ok(())
}

const CLI_BACKUP_HERMES_PREFIX: &str = "hermes";
const CLI_BACKUP_EXTERNAL_PREFIX: &str = "external";

fn backup_secret_like_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(name, ".env" | "auth.json" | "state.db")
        || path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext, "env" | "json" | "conf"))
}

fn safe_relative_archive_path(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

fn backup_collect_regular_files(base: &Path) -> Vec<PathBuf> {
    let Ok(meta) = std::fs::symlink_metadata(base) else {
        return Vec::new();
    };
    if meta.file_type().is_symlink() {
        return Vec::new();
    }
    if meta.is_file() {
        return vec![base.to_path_buf()];
    }
    if !meta.is_dir() {
        return Vec::new();
    }

    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(base) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            files.extend(backup_collect_regular_files(&path));
        } else if meta.is_file() {
            files.push(path);
        }
    }
    files
}

fn collect_memory_provider_external_backup_files(
    hermes_dir: &Path,
    home_dir: &Path,
) -> Vec<(PathBuf, PathBuf)> {
    let Ok(home_resolved) = home_dir.canonicalize() else {
        return Vec::new();
    };
    let hermes_resolved = hermes_dir.canonicalize().ok();
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for provider in hermes_agent::memory_plugins::discover_available_providers() {
        for declared in provider.backup_paths() {
            let Ok(resolved) = declared.canonicalize() else {
                continue;
            };
            if !resolved.starts_with(&home_resolved) {
                continue;
            }
            if hermes_resolved
                .as_ref()
                .is_some_and(|hermes| resolved.starts_with(hermes))
            {
                continue;
            }
            for file in backup_collect_regular_files(&resolved) {
                let Ok(file_resolved) = file.canonicalize() else {
                    continue;
                };
                if !seen.insert(file_resolved.clone()) {
                    continue;
                }
                let Ok(rel_to_home) = file_resolved.strip_prefix(&home_resolved) else {
                    continue;
                };
                let mut archive_path = PathBuf::from(CLI_BACKUP_EXTERNAL_PREFIX);
                archive_path.push(rel_to_home);
                out.push((file_resolved, archive_path));
            }
        }
    }

    out
}

fn restore_archive_entry_target(
    member: &Path,
    hermes_dir: &Path,
    home_dir: &Path,
) -> Option<PathBuf> {
    let safe = safe_relative_archive_path(member)?;
    let mut components = safe.components();
    let first = components.next()?.as_os_str().to_string_lossy().to_string();
    let rel: PathBuf = components.as_path().to_path_buf();
    if rel.as_os_str().is_empty() {
        return None;
    }
    match first.as_str() {
        CLI_BACKUP_EXTERNAL_PREFIX => Some(home_dir.join(rel)),
        CLI_BACKUP_HERMES_PREFIX => Some(hermes_dir.join(rel)),
        _ => Some(hermes_dir.join(safe)),
    }
}

fn restore_backup_archive(
    archive: &mut tar::Archive<flate2::read::GzDecoder<std::fs::File>>,
    hermes_dir: &Path,
    home_dir: &Path,
) -> Result<(usize, usize), hermes_core::AgentError> {
    std::fs::create_dir_all(hermes_dir).map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
    let mut restored = 0usize;
    let mut restored_external = 0usize;

    let entries = archive
        .entries()
        .map_err(|e| hermes_core::AgentError::Io(format!("Read archive error: {}", e)))?;
    for entry in entries {
        let mut entry = entry
            .map_err(|e| hermes_core::AgentError::Io(format!("Archive entry error: {}", e)))?;
        let entry_path = entry
            .path()
            .map_err(|e| hermes_core::AgentError::Io(format!("Archive path error: {}", e)))?
            .into_owned();
        let Some(target) = restore_archive_entry_target(&entry_path, hermes_dir, home_dir) else {
            continue;
        };
        let is_external = safe_relative_archive_path(&entry_path)
            .and_then(|p| p.components().next().map(|c| c.as_os_str().to_owned()))
            .is_some_and(|first| first == CLI_BACKUP_EXTERNAL_PREFIX);
        let entry_type = entry.header().entry_type();

        if entry_type.is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|e| hermes_core::AgentError::Io(format!("Create dir error: {}", e)))?;
            continue;
        }
        if !(entry_type.is_file() || entry_type == tar::EntryType::Regular) {
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| hermes_core::AgentError::Io(format!("Create dir error: {}", e)))?;
        }
        entry
            .unpack(&target)
            .map_err(|e| hermes_core::AgentError::Io(format!("Extract error: {}", e)))?;
        if is_external {
            restored_external += 1;
            if backup_secret_like_file(&target) {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(mut permissions) = std::fs::metadata(&target).map(|m| m.permissions())
                    {
                        permissions.set_mode(0o600);
                        let _ = std::fs::set_permissions(&target, permissions);
                    }
                }
            }
        }
        restored += 1;
    }

    Ok((restored, restored_external))
}

/// Handle `hermes backup [output]`.
pub async fn handle_cli_backup(output: Option<String>) -> Result<(), hermes_core::AgentError> {
    let hermes_dir = hermes_config::hermes_home();
    if !hermes_dir.exists() {
        println!(
            "Hermes home directory not found at {}",
            hermes_dir.display()
        );
        return Ok(());
    }
    let out = output.unwrap_or_else(|| {
        format!(
            "hermes-backup-{}.tar.gz",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        )
    });
    println!("Backing up {} -> {}", hermes_dir.display(), out);

    let tar_gz = std::fs::File::create(&out)
        .map_err(|e| hermes_core::AgentError::Io(format!("Cannot create {}: {}", out, e)))?;
    let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
    let mut tar = tar::Builder::new(enc);
    tar.append_dir_all("hermes", &hermes_dir)
        .map_err(|e| hermes_core::AgentError::Io(format!("Tar error: {}", e)))?;
    let home_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let external_files = collect_memory_provider_external_backup_files(&hermes_dir, &home_dir);
    for (file, archive_path) in &external_files {
        tar.append_path_with_name(file, archive_path)
            .map_err(|e| hermes_core::AgentError::Io(format!("Tar external error: {}", e)))?;
    }
    tar.finish()
        .map_err(|e| hermes_core::AgentError::Io(format!("Tar finish error: {}", e)))?;

    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    println!("Backup complete: {} ({} KB)", out, size / 1024);
    if !external_files.is_empty() {
        println!(
            "Included {} memory-provider file(s) stored outside HERMES_HOME.",
            external_files.len()
        );
    }
    Ok(())
}

/// Handle `hermes import <path>`.
pub async fn handle_cli_import(path: String) -> Result<(), hermes_core::AgentError> {
    let src = std::path::Path::new(&path);
    if !src.exists() {
        return Err(hermes_core::AgentError::Io(format!(
            "Backup archive not found: {}",
            path
        )));
    }
    println!("Importing configuration from: {}", path);

    let hermes_dir = hermes_config::hermes_home();
    std::fs::create_dir_all(&hermes_dir).map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;

    let file = std::fs::File::open(src)
        .map_err(|e| hermes_core::AgentError::Io(format!("Cannot open {}: {}", path, e)))?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);
    let home_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let (restored, restored_external) =
        restore_backup_archive(&mut archive, &hermes_dir, &home_dir)?;

    println!(
        "Import complete. {} files restored to {}",
        restored,
        hermes_dir.display()
    );
    if restored_external > 0 {
        println!(
            "Restored {} memory-provider file(s) outside HERMES_HOME.",
            restored_external
        );
    }
    Ok(())
}

/// Handle `hermes version`.
pub fn handle_cli_version() -> Result<(), hermes_core::AgentError> {
    println!("{}", hermes_core::version::version_label());
    Ok(())
}
