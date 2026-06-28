impl HermesAcpHandler {
    pub fn new(
        session_manager: Arc<SessionManager>,
        event_sink: Arc<EventSink>,
        permission_store: Arc<PermissionStore>,
    ) -> Self {
        let tool_registry = Arc::new(ToolRegistry::new());
        let mcp_manager = Arc::new(AsyncMutex::new(McpManager::new(Arc::clone(&tool_registry))));
        Self {
            session_manager,
            event_sink,
            permission_store,
            tool_registry,
            mcp_manager,
            version: env!("CARGO_PKG_VERSION").to_string(),
            prompt_executor: None,
            auth_provider_resolver: Arc::new(detect_provider),
        }
    }

    pub fn with_prompt_executor(mut self, prompt_executor: Arc<dyn AcpPromptExecutor>) -> Self {
        self.prompt_executor = Some(prompt_executor);
        self
    }

    pub fn with_auth_provider_resolver(
        mut self,
        resolver: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    ) -> Self {
        self.auth_provider_resolver = resolver;
        self
    }

    pub fn with_mcp_components(
        mut self,
        tool_registry: Arc<ToolRegistry>,
        mcp_manager: Arc<AsyncMutex<McpManager>>,
    ) -> Self {
        self.tool_registry = tool_registry;
        self.mcp_manager = mcp_manager;
        self
    }

    pub fn tool_registry(&self) -> &Arc<ToolRegistry> {
        &self.tool_registry
    }

    fn available_tools(&self) -> Vec<(String, String)> {
        let mut tools = vec![
            (
                "bash".to_string(),
                "Execute shell commands with approval controls".to_string(),
            ),
            (
                "read".to_string(),
                "Read files from the local workspace".to_string(),
            ),
            (
                "write".to_string(),
                "Write or create files in the local workspace".to_string(),
            ),
            ("edit".to_string(), "Patch files in-place".to_string()),
            ("grep".to_string(), "Search file contents".to_string()),
            ("glob".to_string(), "Find files by pattern".to_string()),
            ("web_search".to_string(), "Search the web".to_string()),
            ("web_fetch".to_string(), "Fetch and parse URLs".to_string()),
            (
                "memory".to_string(),
                "Read/write persistent memory notes".to_string(),
            ),
            (
                "session_search".to_string(),
                "Search prior session content".to_string(),
            ),
            (
                "skills_list".to_string(),
                "List installed skills".to_string(),
            ),
            (
                "skill_view".to_string(),
                "Inspect a specific skill".to_string(),
            ),
            (
                "skill_manage".to_string(),
                "Install/update/remove skills".to_string(),
            ),
            ("todo".to_string(), "Track task progress".to_string()),
            ("cronjob".to_string(), "Schedule recurring jobs".to_string()),
        ];
        for entry in self
            .tool_registry
            .list_tools()
            .into_iter()
            .filter(|entry| entry.toolset.starts_with("mcp-"))
        {
            tools.push((entry.name, entry.description));
        }
        tools.sort_by(|a, b| a.0.cmp(&b.0));
        tools.dedup_by(|a, b| a.0 == b.0);
        tools
    }

    fn available_commands() -> Vec<AvailableCommand> {
        SLASH_COMMANDS
            .iter()
            .map(|command| AvailableCommand {
                name: command.name.to_string(),
                description: command.description.to_string(),
                input_hint: command.input_hint.map(str::to_string),
            })
            .collect()
    }

    fn advertise_available_commands(&self, session_id: &str) {
        self.event_sink.push(AcpEvent::available_commands_update(
            session_id,
            Self::available_commands(),
        ));
    }

    async fn register_session_mcp_servers(
        &self,
        state: &SessionState,
        servers: Vec<McpServerConfig>,
    ) {
        if servers.is_empty() {
            return;
        }
        let configs: Vec<(String, HermesMcpServerConfig)> = servers
            .iter()
            .filter_map(acp_mcp_server_to_hermes_config)
            .collect();
        if configs.is_empty() {
            return;
        }
        let enabled_toolsets = expand_acp_enabled_toolsets(
            vec!["hermes-acp".to_string()],
            configs.iter().map(|(name, _)| name.clone()),
        );
        tracing::debug!(
            "ACP session {} enabling toolsets after MCP registration: {:?}",
            state.session_id,
            enabled_toolsets
        );
        let reports = {
            let mut manager = self.mcp_manager.lock().await;
            manager.connect_all_parallel(configs).await
        };
        let connected = reports.iter().filter(|report| report.connected).count();
        let failed = reports.len().saturating_sub(connected);
        if failed > 0 {
            tracing::warn!(
                "ACP session {} registered {} MCP server(s), {} failed",
                state.session_id,
                connected,
                failed
            );
        } else {
            tracing::info!(
                "ACP session {} registered {} MCP server(s)",
                state.session_id,
                connected
            );
        }
    }

    fn context_usage_for_state(state: &SessionState) -> Option<(u64, u64)> {
        let model = match (state.provider.as_deref(), state.model.as_deref()) {
            (Some(provider), Some(model)) if !model.contains(':') => {
                format!("{provider}:{model}")
            }
            (_, Some(model)) => model.to_string(),
            _ => "unknown-model".to_string(),
        };
        let size = get_model_context_length(&model);
        if size == 0 {
            return None;
        }
        let used = estimate_request_tokens_rough(&state.history, "", None);
        Some((size, used))
    }

    fn emit_usage_update(&self, session_id: &str) {
        let Some(state) = self.session_manager.get_session(session_id) else {
            return;
        };
        if let Some((size, used)) = Self::context_usage_for_state(&state) {
            self.event_sink
                .push(AcpEvent::usage_update(session_id, size, used));
        }
    }

    fn emit_session_info_update(&self, session_id: &str) {
        let Some(state) = self.session_manager.get_session(session_id) else {
            return;
        };
        self.event_sink.push(AcpEvent::session_info_update(
            session_id,
            session_display_title(&state),
            session_info_refresh_timestamp(),
            Some(session_info_value(&state)),
        ));
    }

    fn replay_session_history(&self, state: &SessionState) {
        let mut active_tool_calls: HashMap<String, String> = HashMap::new();
        for message in &state.history {
            let role = message.get("role").and_then(Value::as_str).unwrap_or("");
            match role {
                "user" => {
                    let text = history_message_text(message);
                    if !text.is_empty() {
                        self.event_sink
                            .push(AcpEvent::user_message_chunk(&state.session_id, &text));
                    }
                }
                "assistant" => {
                    let thought = history_reasoning_text(message);
                    if !thought.is_empty() {
                        self.event_sink
                            .push(AcpEvent::agent_thought_chunk(&state.session_id, &thought));
                    }

                    let text = history_message_text(message);
                    if !text.is_empty() {
                        self.event_sink
                            .push(AcpEvent::agent_message_chunk(&state.session_id, &text));
                    }

                    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
                        for tool_call in tool_calls {
                            let Some(tool_call_id) = tool_call
                                .get("id")
                                .and_then(Value::as_str)
                                .filter(|id| !id.trim().is_empty())
                            else {
                                continue;
                            };
                            let tool_name = history_tool_call_name(tool_call);
                            active_tool_calls.insert(tool_call_id.to_string(), tool_name.clone());
                            self.event_sink.push(AcpEvent::tool_call_start(
                                &state.session_id,
                                tool_call_id,
                                &tool_name,
                                history_tool_call_arguments(tool_call),
                            ));
                        }
                    }
                }
                "tool" => {
                    let Some(tool_call_id) = message
                        .get("tool_call_id")
                        .or_else(|| message.get("toolCallId"))
                        .and_then(Value::as_str)
                        .filter(|id| !id.trim().is_empty())
                    else {
                        continue;
                    };
                    let tool_name = active_tool_calls
                        .get(tool_call_id)
                        .cloned()
                        .or_else(|| {
                            message
                                .get("name")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .unwrap_or_else(|| "tool".to_string());
                    let text = history_message_text(message);
                    let result = (!text.is_empty()).then_some(text.as_str());
                    self.event_sink.push(AcpEvent::tool_call_complete(
                        &state.session_id,
                        tool_call_id,
                        &tool_name,
                        result.map(str::to_string),
                    ));
                    if tool_name == "todo" {
                        if let Some(entries) = plan_entries_from_todo_result(result) {
                            self.event_sink
                                .push(AcpEvent::plan_update(&state.session_id, entries));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn compact_session_history(&self, session_id: &str) -> Option<String> {
        let state = self.session_manager.get_session(session_id)?;
        let total = state.history.len();
        if total == 0 {
            return Some("Conversation is empty (nothing to compact).".to_string());
        }
        if total <= 8 {
            return Some(format!(
                "Conversation is already compact ({} messages).",
                total
            ));
        }

        let keep_recent = 6usize;
        let split = total.saturating_sub(keep_recent);
        let (older, recent) = state.history.split_at(split);

        let mut preserved_system = Vec::new();
        let mut summary_lines = Vec::new();
        for msg in older {
            let role = msg
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            if role == "system" {
                preserved_system.push(msg.clone());
            }

            let content = msg
                .get("content")
                .or_else(|| msg.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .replace('\n', " ");
            if content.is_empty() {
                continue;
            }
            let preview = if content.chars().count() > 140 {
                let head: String = content.chars().take(140).collect();
                format!("{head}...")
            } else {
                content
            };
            summary_lines.push(format!("- {}: {}", role, preview));
            if summary_lines.len() >= 10 {
                break;
            }
        }

        if summary_lines.is_empty() {
            summary_lines.push("- (no textual content in compacted segment)".to_string());
        }

        let summary = format!(
            "Compressed {} earlier messages into summary context.\n{}",
            older.len(),
            summary_lines.join("\n")
        );

        let summary_msg = json!({
            "role": "system",
            "content": summary,
            "meta": {
                "compressed": true,
                "compressed_message_count": older.len()
            }
        });

        let mut new_history = Vec::new();
        new_history.extend(preserved_system);
        new_history.push(summary_msg);
        new_history.extend_from_slice(recent);
        let new_total = new_history.len();

        self.session_manager.set_history(session_id, new_history);
        self.session_manager.save_session(session_id);

        Some(format!(
            "Context compacted: {} -> {} messages (compressed {}).",
            total,
            new_total,
            older.len()
        ))
    }

    async fn execute_prompt_turn(
        &self,
        session_id: &str,
        user_text: String,
        user_content: Value,
    ) -> Result<Option<Usage>, String> {
        let mut history = self
            .session_manager
            .get_session(session_id)
            .map(|s| s.history)
            .unwrap_or_default();
        history.push(json!({
            "role": "user",
            "content": user_content,
        }));
        self.session_manager
            .set_history(session_id, history.clone());

        self.event_sink
            .push(AcpEvent::thinking(session_id, "Processing prompt..."));
        let session_snapshot = self
            .session_manager
            .get_session(session_id)
            .ok_or_else(|| format!("Session not found: {session_id}"))?;

        let prompt_result = if let Some(executor) = &self.prompt_executor {
            executor
                .execute_prompt(&session_snapshot, &user_text, &history)
                .await
        } else {
            let turn = history
                .iter()
                .filter(|m| {
                    m.get("role")
                        .and_then(|v| v.as_str())
                        .map(|r| r == "user")
                        .unwrap_or(false)
                })
                .count();
            let snippet = user_text.chars().take(200).collect::<String>();
            Ok(PromptExecutionOutput {
                response_text: format!(
                    "ACP session {} processed turn {}.\n\n{}",
                    session_id, turn, snippet
                ),
                usage: None,
                total_turns: Some(1),
                events: Vec::new(),
            })
        }?;

        let PromptExecutionOutput {
            response_text,
            usage,
            total_turns,
            events,
        } = prompt_result;

        let streamed_message = events.iter().any(|event| {
            matches!(
                event.kind,
                AcpEventKind::MessageDelta | AcpEventKind::MessageComplete
            ) && event.text.as_deref().is_some_and(|text| !text.is_empty())
        });
        for event in events {
            self.event_sink.push(event);
        }

        let response_text = response_text.trim().to_string();
        if !response_text.is_empty() && !streamed_message {
            self.event_sink
                .push(AcpEvent::message_delta(session_id, &response_text));
            self.event_sink
                .push(AcpEvent::message_complete(session_id, &response_text));
        }
        self.event_sink.push(AcpEvent::step_complete(
            session_id,
            total_turns.unwrap_or(1),
        ));

        history.push(json!({
            "role": "assistant",
            "content": response_text,
        }));
        self.session_manager.set_history(session_id, history);

        if let Some(usage) = usage.as_ref() {
            self.session_manager
                .add_usage(session_id, usage.input_tokens, usage.output_tokens);
        }
        self.session_manager.save_session(session_id);
        self.emit_session_info_update(session_id);

        Ok(usage)
    }

    fn handle_slash_command(&self, text: &str, session_id: &str) -> Option<String> {
        let (cmd, args) = slash_command_parts(text)?;

        match cmd.as_str() {
            "help" => {
                let mut lines = vec!["Available commands:".to_string(), String::new()];
                for sc in SLASH_COMMANDS {
                    lines.push(format!("  /{:<10}  {}", sc.name, sc.description));
                }
                lines.push(String::new());
                lines.push(
                    "Unrecognized /commands are sent to the model as normal messages.".to_string(),
                );
                Some(lines.join("\n"))
            }
            "model" => {
                let state = self.session_manager.get_session(session_id)?;
                let model = state.model.as_deref().unwrap_or("unknown");
                let provider = state.provider.as_deref().unwrap_or("auto");
                Some(format!("Current model: {model}\nProvider: {provider}"))
            }
            "context" => {
                let state = self.session_manager.get_session(session_id)?;
                let n = state.history.len();
                let mut roles: HashMap<String, usize> = HashMap::new();
                for msg in &state.history {
                    let role = msg
                        .get("role")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    *roles.entry(role.to_string()).or_default() += 1;
                }
                let mut lines = if n == 0 {
                    vec!["Conversation is empty (no messages yet).".to_string()]
                } else {
                    vec![format!(
                        "Conversation: {} messages\n  user: {}, assistant: {}, tool: {}, system: {}",
                        n,
                        roles.get("user").unwrap_or(&0),
                        roles.get("assistant").unwrap_or(&0),
                        roles.get("tool").unwrap_or(&0),
                        roles.get("system").unwrap_or(&0),
                    )]
                };
                if let Some((size, used)) = Self::context_usage_for_state(&state) {
                    let percent = if size == 0 {
                        0.0
                    } else {
                        (used as f64 / size as f64) * 100.0
                    };
                    let threshold = ((size as f64) * 0.80).round() as u64;
                    lines.push(format!(
                        "Context usage: ~{} / {} tokens ({percent:.1}%)",
                        format_token_count_plain(used),
                        format_token_count_plain(size)
                    ));
                    if used >= threshold {
                        lines.push(format!(
                            "Compression: due now (threshold ~{}, 80%). Run /compact.",
                            format_token_count_plain(threshold)
                        ));
                    } else {
                        lines.push(format!(
                            "Compression: ~{} tokens until threshold (~{}, 80%).",
                            format_token_count_plain(threshold.saturating_sub(used)),
                            format_token_count_plain(threshold)
                        ));
                    }
                }
                Some(lines.join("\n"))
            }
            "reset" => {
                self.session_manager.set_history(session_id, Vec::new());
                self.session_manager.save_session(session_id);
                Some("Conversation history cleared.".to_string())
            }
            "compact" => self.compact_session_history(session_id),
            "queue" => {
                if args.is_empty() {
                    return Some("Usage: /queue <prompt>".to_string());
                }
                if self.session_manager.push_queued_prompt(session_id, args) {
                    Some("Queued prompt for the next turn.".to_string())
                } else {
                    Some(format!("Session not found: {session_id}"))
                }
            }
            "steer" => {
                if args.is_empty() {
                    return Some("Usage: /steer <guidance>".to_string());
                }
                let state = self.session_manager.get_session(session_id)?;
                if state.phase == SessionPhase::Active {
                    self.session_manager.push_queued_prompt(session_id, args);
                    let steered = match self.prompt_executor.as_ref() {
                        Some(executor) => match executor.steer_prompt(&state, args) {
                            Ok(steered) => steered,
                            Err(err) => {
                                return Some(format!(
                                    "Steer failed; queued prompt for the next turn: {err}"
                                ));
                            }
                        },
                        None => false,
                    };
                    if steered {
                        Some("Steered the active ACP session.".to_string())
                    } else {
                        Some("Queued prompt for the next turn.".to_string())
                    }
                } else {
                    None
                }
            }
            "version" => Some(hermes_core::version::version_label()),
            "tools" => {
                let tools = self.available_tools();
                if tools.is_empty() {
                    Some("No tools are currently available.".to_string())
                } else if args.eq_ignore_ascii_case("json") {
                    Some(
                        serde_json::to_string_pretty(
                            &tools
                                .iter()
                                .map(|(name, description)| {
                                    json!({"name": name, "description": description})
                                })
                                .collect::<Vec<_>>(),
                        )
                        .unwrap_or_else(|_| "[]".to_string()),
                    )
                } else {
                    let mut lines =
                        vec![format!("Available tools ({}):", tools.len()), String::new()];
                    for (name, description) in &tools {
                        lines.push(format!("  /tool {:<14} {}", name, description));
                    }
                    lines.push(String::new());
                    lines.push("Tip: use `/tools json` for machine-readable output.".to_string());
                    Some(lines.join("\n"))
                }
            }
            _ => None,
        }
    }
}
