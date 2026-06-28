#[async_trait::async_trait]
impl AcpHandler for HermesAcpHandler {
    async fn handle_request(&self, request: AcpRequest) -> AcpResponse {
        let method = AcpMethod::from(request.method.as_str());
        match method {
            // -- Lifecycle --------------------------------------------------
            AcpMethod::Initialize => {
                let auth_provider = (self.auth_provider_resolver)();
                let resp = InitializeResponse {
                    protocol_version: 1,
                    agent_info: Implementation {
                        name: "hermes-agent".to_string(),
                        version: self.version.clone(),
                    },
                    agent_capabilities: AgentCapabilities {
                        load_session: true,
                        prompt_capabilities: Some(PromptCapabilities { image: true }),
                        session_capabilities: Some(SessionCapabilities {
                            fork: true,
                            list: true,
                            resume: true,
                        }),
                        tools: Some(
                            self.available_tools()
                                .into_iter()
                                .map(|(name, _)| name)
                                .collect(),
                        ),
                        streaming: true,
                        ..Default::default()
                    },
                    auth_methods: Some(build_auth_methods_for_provider(auth_provider.as_deref())),
                };
                AcpResponse::success(request.id, serde_json::to_value(&resp).unwrap())
            }

            AcpMethod::Authenticate => {
                let method_id = params_obj(&request.params)
                    .and_then(|p| param_str(p, "method_id").or_else(|| param_str(p, "methodId")))
                    .map(str::trim)
                    .unwrap_or("");
                let normalized_method = method_id.to_ascii_lowercase();
                let provider = (self.auth_provider_resolver)()
                    .map(|provider| provider.trim().to_ascii_lowercase())
                    .filter(|provider| !provider.is_empty());
                let accepted = match provider.as_deref() {
                    Some(provider) if normalized_method == provider => true,
                    Some(_) if normalized_method == TERMINAL_SETUP_AUTH_METHOD_ID => true,
                    _ => false,
                };
                if accepted {
                    AcpResponse::success(request.id, json!({}))
                } else {
                    AcpResponse::success(request.id, Value::Null)
                }
            }

            // -- Session management -----------------------------------------
            AcpMethod::NewSession => {
                let p = params_obj(&request.params);
                let cwd = p.and_then(|p| param_str(p, "cwd")).unwrap_or(".");
                let mcp_servers = acp_mcp_servers_from_params(p);
                let meta = match p.map(session_meta_from_params).transpose() {
                    Ok(meta) => meta.unwrap_or_default(),
                    Err(err) => return AcpResponse::error(request.id, -32602, err),
                };
                let state = self.session_manager.create_session_with_meta(cwd, meta);
                self.register_session_mcp_servers(&state, mcp_servers).await;
                self.advertise_available_commands(&state.session_id);
                self.emit_usage_update(&state.session_id);
                AcpResponse::success(request.id, session_id_response(&state.session_id))
            }

            AcpMethod::LoadSession => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(request.id, -32602, "Missing params");
                };
                let session_id = param_str_any(p, &["sessionId", "session_id"]).unwrap_or("");
                let cwd = param_str(p, "cwd").unwrap_or(".");
                let mcp_servers = acp_mcp_servers_from_params(Some(p));
                let mut meta = match session_meta_from_params(p) {
                    Ok(meta) => meta,
                    Err(err) => return AcpResponse::error(request.id, -32602, err),
                };
                meta.cwd = Some(cwd.to_string());

                match self.session_manager.update_session_meta(session_id, meta) {
                    Some(state) => {
                        self.register_session_mcp_servers(&state, mcp_servers).await;
                        self.replay_session_history(&state);
                        self.advertise_available_commands(session_id);
                        self.emit_usage_update(session_id);
                        AcpResponse::success(request.id, json!({}))
                    }
                    None => AcpResponse::error(
                        request.id,
                        -32602,
                        format!("Session not found: {}", session_id),
                    ),
                }
            }

            AcpMethod::ResumeSession => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(request.id, -32602, "Missing params");
                };
                let session_id = param_str_any(p, &["sessionId", "session_id"]).unwrap_or("");
                let cwd = param_str(p, "cwd").unwrap_or(".");
                let mcp_servers = acp_mcp_servers_from_params(Some(p));
                let mut meta = match session_meta_from_params(p) {
                    Ok(meta) => meta,
                    Err(err) => return AcpResponse::error(request.id, -32602, err),
                };
                meta.cwd = Some(cwd.to_string());

                if let Some(state) = self.session_manager.update_session_meta(session_id, meta) {
                    self.register_session_mcp_servers(&state, mcp_servers).await;
                    self.emit_session_info_update(&state.session_id);
                    self.replay_session_history(&state);
                    self.advertise_available_commands(&state.session_id);
                    self.emit_usage_update(&state.session_id);
                    AcpResponse::success(request.id, json!({}))
                } else {
                    let meta = match session_meta_from_params(p) {
                        Ok(meta) => meta,
                        Err(err) => return AcpResponse::error(request.id, -32602, err),
                    };
                    let state = self.session_manager.create_session_with_meta(cwd, meta);
                    self.register_session_mcp_servers(&state, mcp_servers).await;
                    self.advertise_available_commands(&state.session_id);
                    self.emit_usage_update(&state.session_id);
                    AcpResponse::success(request.id, session_id_response(&state.session_id))
                }
            }

            AcpMethod::ForkSession => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(request.id, -32602, "Missing params");
                };
                let session_id = param_str_any(p, &["sessionId", "session_id"]).unwrap_or("");
                let cwd = param_str(p, "cwd").unwrap_or(".");
                let mcp_servers = acp_mcp_servers_from_params(Some(p));
                let meta = match session_meta_from_params(p) {
                    Ok(meta) => meta,
                    Err(err) => return AcpResponse::error(request.id, -32602, err),
                };

                match self
                    .session_manager
                    .fork_session_with_meta(session_id, cwd, meta)
                {
                    Some(new_state) => {
                        self.register_session_mcp_servers(&new_state, mcp_servers)
                            .await;
                        self.advertise_available_commands(&new_state.session_id);
                        self.emit_usage_update(&new_state.session_id);
                        AcpResponse::success(request.id, session_id_response(&new_state.session_id))
                    }
                    None => AcpResponse::error(
                        request.id,
                        -32602,
                        format!("Session not found: {}", session_id),
                    ),
                }
            }

            AcpMethod::ListSessions => {
                let cwd_filter = params_obj(&request.params)
                    .and_then(|p| param_str(p, "cwd"))
                    .filter(|cwd| !cwd.trim().is_empty());
                let cursor = params_obj(&request.params)
                    .and_then(|p| param_str(p, "cursor"))
                    .filter(|cursor| !cursor.trim().is_empty());
                let profile_filter = params_obj(&request.params)
                    .and_then(|p| param_str(p, "profile"))
                    .map(str::trim)
                    .filter(|profile| !profile.is_empty());
                let mut sessions = self.session_manager.list_session_states();
                if let Some(cwd) = cwd_filter {
                    sessions.retain(|s| s.cwd == cwd);
                }
                if let Some(profile) = profile_filter {
                    sessions.retain(|s| s.profile.as_deref() == Some(profile));
                }
                sessions.sort_by(|a, b| {
                    b.updated_at
                        .cmp(&a.updated_at)
                        .then_with(|| a.session_id.cmp(&b.session_id))
                });
                if let Some(cursor) = cursor {
                    if let Some(index) = sessions.iter().position(|s| s.session_id == cursor) {
                        sessions = sessions.into_iter().skip(index + 1).collect();
                    } else {
                        sessions.clear();
                    }
                }
                const LIST_SESSIONS_PAGE_SIZE: usize = 50;
                let next_cursor = (sessions.len() > LIST_SESSIONS_PAGE_SIZE)
                    .then(|| sessions[LIST_SESSIONS_PAGE_SIZE - 1].session_id.clone());
                let page = sessions
                    .into_iter()
                    .take(LIST_SESSIONS_PAGE_SIZE)
                    .map(|s| session_info_value(&s))
                    .collect::<Vec<_>>();
                AcpResponse::success(
                    request.id,
                    json!({
                        "sessions": page,
                        "nextCursor": next_cursor,
                    }),
                )
            }

            AcpMethod::Cancel => {
                let session_id = params_obj(&request.params)
                    .and_then(|p| param_str_any(p, &["sessionId", "session_id"]))
                    .unwrap_or("");
                if let Some(state) = self.session_manager.get_session(session_id) {
                    if state.phase == SessionPhase::Active {
                        self.session_manager.set_interrupted_prompt_text(
                            session_id,
                            latest_user_prompt_text(&state.history),
                        );
                    }
                }
                self.session_manager
                    .set_phase(session_id, SessionPhase::Cancelled);
                tracing::info!("Cancelled session {}", session_id);
                AcpResponse::success(request.id, json!({"cancelled": true}))
            }

            // -- Prompt (core) ----------------------------------------------
            AcpMethod::Prompt => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(request.id, -32602, "Missing params");
                };
                let session_id = param_str_any(p, &["sessionId", "session_id"]).unwrap_or("");

                if self.session_manager.get_session(session_id).is_none() {
                    return AcpResponse::success(
                        request.id,
                        prompt_response_value(StopReason::Refusal, None),
                    );
                }

                let extraction = extract_prompt_payload(p);
                let mut user_text = extraction.user_text;
                let mut user_content = extraction.user_content;
                let text_only_prompt = extraction.text_only_prompt;
                let has_content = extraction.has_content;

                if !has_content {
                    return AcpResponse::success(
                        request.id,
                        prompt_response_value(StopReason::EndTurn, None),
                    );
                }

                if text_only_prompt {
                    if let Some((cmd, args)) = slash_command_parts(&user_text) {
                        if cmd == "steer" {
                            if args.is_empty() {
                                self.event_sink.push(AcpEvent::message_complete(
                                    session_id,
                                    "Usage: /steer <guidance>",
                                ));
                                return AcpResponse::success(
                                    request.id,
                                    prompt_response_value(StopReason::EndTurn, None),
                                );
                            }

                            let active = self
                                .session_manager
                                .get_session(session_id)
                                .map(|s| s.phase == SessionPhase::Active)
                                .unwrap_or(false);
                            if !active {
                                if let Some(interrupted) = self
                                    .session_manager
                                    .take_interrupted_prompt_text(session_id)
                                {
                                    user_text = format!(
                                        "{interrupted}\n\nUser correction/guidance after interrupt: {args}"
                                    );
                                } else {
                                    user_text = args.to_string();
                                }
                                user_content = Value::String(user_text.clone());
                            }
                        }
                    }
                }

                // Intercept slash commands
                if text_only_prompt && user_text.starts_with('/') {
                    if let Some(response_text) = self.handle_slash_command(&user_text, session_id) {
                        self.event_sink
                            .push(AcpEvent::message_complete(session_id, &response_text));
                        self.emit_usage_update(session_id);
                        return AcpResponse::success(
                            request.id,
                            prompt_response_value(StopReason::EndTurn, None),
                        );
                    }
                }

                if self
                    .session_manager
                    .get_session(session_id)
                    .map(|s| s.phase == SessionPhase::Active)
                    .unwrap_or(false)
                {
                    let queued_text = if user_text.trim().is_empty() {
                        "[Image attachment]"
                    } else {
                        user_text.trim()
                    };
                    self.session_manager
                        .push_queued_prompt(session_id, queued_text);
                    self.event_sink.push(AcpEvent::message_complete(
                        session_id,
                        "Queued prompt for the next turn.",
                    ));
                    return AcpResponse::success(
                        request.id,
                        prompt_response_value(StopReason::EndTurn, None),
                    );
                }

                self.session_manager
                    .set_phase(session_id, SessionPhase::Active);

                let mut usage = match self
                    .execute_prompt_turn(session_id, user_text, user_content)
                    .await
                {
                    Ok(usage) => usage,
                    Err(err) => {
                        self.event_sink.push(AcpEvent::error(session_id, &err));
                        self.session_manager
                            .set_phase(session_id, SessionPhase::Failed);
                        return AcpResponse::error(request.id, -32000, err);
                    }
                };

                while let Some(queued_prompt) = self.session_manager.pop_queued_prompt(session_id) {
                    let queued_usage = match self
                        .execute_prompt_turn(
                            session_id,
                            queued_prompt.clone(),
                            Value::String(queued_prompt),
                        )
                        .await
                    {
                        Ok(usage) => usage,
                        Err(err) => {
                            self.event_sink.push(AcpEvent::error(session_id, &err));
                            self.session_manager
                                .set_phase(session_id, SessionPhase::Failed);
                            return AcpResponse::error(request.id, -32000, err);
                        }
                    };
                    usage = merge_usage(usage, queued_usage);
                }

                self.session_manager
                    .set_phase(session_id, SessionPhase::Idle);
                self.emit_usage_update(session_id);

                AcpResponse::success(
                    request.id,
                    prompt_response_value(StopReason::EndTurn, usage),
                )
            }

            // -- Session configuration --------------------------------------
            AcpMethod::SetSessionModel => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(request.id, -32602, "Missing params");
                };
                let session_id = param_str_any(p, &["sessionId", "session_id"]).unwrap_or("");
                let model_id = param_str_any(p, &["modelId", "model_id"])
                    .or_else(|| param_str(p, "model"))
                    .unwrap_or("");

                if model_id.trim().is_empty() {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "Missing model_id/model for session/set_model",
                    );
                }

                match self.session_manager.update_model(session_id, model_id) {
                    Some(_) => {
                        tracing::info!("Session {}: model switched to {}", session_id, model_id);
                        self.emit_usage_update(session_id);
                        AcpResponse::success(request.id, json!({}))
                    }
                    None => AcpResponse::error(
                        request.id,
                        -32602,
                        format!("Session not found: {}", session_id),
                    ),
                }
            }

            AcpMethod::SetSessionMode => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(request.id, -32602, "Missing params");
                };
                let session_id = param_str_any(p, &["sessionId", "session_id"]).unwrap_or("");
                let mode_id = param_str_any(p, &["modeId", "mode_id"])
                    .or_else(|| param_str(p, "mode"))
                    .unwrap_or("");
                if mode_id.trim().is_empty() {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "Missing mode_id/mode for session/set_mode",
                    );
                }
                match self.session_manager.update_mode(session_id, mode_id) {
                    Some(_) => AcpResponse::success(request.id, json!({})),
                    None => AcpResponse::error(
                        request.id,
                        -32602,
                        format!("Session not found: {}", session_id),
                    ),
                }
            }

            AcpMethod::SetConfigOption => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(request.id, -32602, "Missing params");
                };
                let session_id = param_str_any(p, &["sessionId", "session_id"]).unwrap_or("");
                let key = param_str_any(p, &["configId", "config_id"])
                    .or_else(|| param_str(p, "key"))
                    .or_else(|| param_str(p, "option"))
                    .or_else(|| param_str(p, "name"))
                    .unwrap_or("");
                let value = param_value_as_string(p, "value")
                    .or_else(|| param_value_as_string_any(p, &["optionValue", "option_value"]))
                    .unwrap_or_default();

                if key.trim().is_empty() {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "Missing key/option for session/set_config",
                    );
                }

                match self
                    .session_manager
                    .set_config_option(session_id, key, &value)
                {
                    Some(_) => AcpResponse::success(
                        request.id,
                        json!({"configOptions": [{"configId": key, "value": value}]}),
                    ),
                    None => AcpResponse::error(
                        request.id,
                        -32602,
                        format!("Session not found: {}", session_id),
                    ),
                }
            }

            AcpMethod::SessionTitle => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(request.id, -32602, "Missing params");
                };
                let session_id = param_str_any(p, &["sessionId", "session_id"]).unwrap_or("");
                let title = param_str(p, "title").map(str::trim).unwrap_or("");

                if session_id.trim().is_empty() {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "Missing session_id/sessionId for session.title",
                    );
                }
                if title.is_empty() {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "Missing non-empty title for session.title",
                    );
                }

                match self.session_manager.update_session_meta(
                    session_id,
                    SessionMetaUpdate {
                        title: Some(title.to_string()),
                        ..SessionMetaUpdate::default()
                    },
                ) {
                    Some(state) => {
                        self.emit_session_info_update(session_id);
                        let title = state.title.clone().unwrap_or_else(|| title.to_string());
                        AcpResponse::success(
                            request.id,
                            json!({
                                "sessionId": state.session_id,
                                "session_id": state.session_id,
                                "title": title,
                            }),
                        )
                    }
                    None => AcpResponse::error(
                        request.id,
                        -32602,
                        format!("Session not found: {}", session_id),
                    ),
                }
            }

            // -- Legacy methods ---------------------------------------------
            AcpMethod::CreateConversation => {
                let state = self.session_manager.create_session(".");
                AcpResponse::success(request.id, json!({"conversation_id": state.session_id}))
            }

            AcpMethod::SendMessage => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "message.send: missing params object",
                    );
                };
                let conv_id = param_str(p, "conversation_id").unwrap_or("");
                let text = param_str(p, "text")
                    .or_else(|| param_str(p, "content"))
                    .unwrap_or("");
                let msg_id = uuid::Uuid::new_v4().to_string();

                if let Some(state) = self.session_manager.get_session(conv_id) {
                    let mut history = state.history.clone();
                    history.push(json!({
                        "id": msg_id,
                        "role": "user",
                        "content": text,
                    }));
                    self.session_manager.set_history(conv_id, history);
                    AcpResponse::success(
                        request.id,
                        json!({"message_id": msg_id, "conversation_id": conv_id}),
                    )
                } else {
                    AcpResponse::error(
                        request.id,
                        -32602,
                        format!("Unknown conversation_id '{}'", conv_id),
                    )
                }
            }

            AcpMethod::GetHistory => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "history.get: missing params object",
                    );
                };
                let conv_id = param_str(p, "conversation_id").unwrap_or("");
                let messages = self
                    .session_manager
                    .get_session(conv_id)
                    .map(|s| s.history)
                    .unwrap_or_default();
                AcpResponse::success(request.id, json!({"messages": messages}))
            }

            AcpMethod::ListTools => {
                let tools: Vec<Value> = self
                    .available_tools()
                    .into_iter()
                    .map(|(name, description)| {
                        json!({
                            "name": name,
                            "description": description,
                        })
                    })
                    .collect();
                AcpResponse::success(request.id, json!({"tools": tools}))
            }

            AcpMethod::ExecuteTool => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "tools.execute: missing params object",
                    );
                };
                let name = p
                    .get("name")
                    .or_else(|| p.get("tool"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let arguments = p.get("arguments").cloned().unwrap_or(Value::Null);
                AcpResponse::success(
                    request.id,
                    json!({
                        "tool": name,
                        "arguments": arguments,
                        "result": format!("ACP handler echo for tool '{}'", name),
                    }),
                )
            }

            AcpMethod::GetStatus => AcpResponse::success(
                request.id,
                json!({
                    "status": "ready",
                    "version": self.version,
                }),
            ),

            AcpMethod::Unknown(method) => {
                AcpResponse::error(request.id, -32601, format!("Method not found: {}", method))
            }
        }
    }
}
