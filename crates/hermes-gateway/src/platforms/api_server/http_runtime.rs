async fn handle_connection(
    stream: tokio::net::TcpStream,
    _peer: SocketAddr,
    runtime: ApiServerRuntime,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::AsyncWriteExt;

    let ApiServerRuntime {
        mailbox,
        response_store,
        run_cancels,
        run_store,
        cron_scheduler,
        inbound_tx,
        auth_token,
    } = runtime;

    let (mut reader, mut writer) = stream.into_split();
    let raw = read_http_request(&mut reader).await?;
    if raw.is_empty() {
        return Ok(());
    }

    let Some(header_end) = find_bytes(&raw, b"\r\n\r\n") else {
        let resp = json_http_response(
            HTTP_BAD_REQUEST,
            &api_error("Invalid HTTP request", "invalid_request_error", 400),
        )?;
        writer.write_all(resp.as_bytes()).await?;
        return Ok(());
    };

    let header_text = String::from_utf8_lossy(&raw[..header_end]);
    let body_bytes = &raw[(header_end + 4).min(raw.len())..];
    let first_line = header_text.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    let method = parts.first().copied().unwrap_or("GET");
    let raw_path = parts.get(1).copied().unwrap_or("/");
    let (path, query) = split_path_query(raw_path);

    // Extract Authorization header
    let auth_header = header_text
        .lines()
        .find(|l| l.to_lowercase().starts_with("authorization:"))
        .map(|l| l.splitn(2, ':').nth(1).unwrap_or("").trim().to_string());
    let chronos_fire_route = method == "POST" && path == "/api/cron/fire";

    if let Some(ref expected) = auth_token.filter(|_| !chronos_fire_route) {
        let valid = auth_header
            .as_deref()
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|t| t == expected)
            .unwrap_or(false);
        if !valid {
            let resp = json_http_response(
                HTTP_UNAUTHORIZED,
                &api_error("Unauthorized", "auth_error", 401),
            )?;
            writer.write_all(resp.as_bytes()).await?;
            return Ok(());
        }
    }

    if method == "POST" {
        if let Some(run_id) = parse_stop_run_path(path) {
            let stop_record = run_store.write().await.update(run_id, |record| {
                if record.is_active() {
                    record.status = "stopping".to_string();
                    record.push_event("run.stopping", None);
                }
            });

            if let Some(record) = stop_record.filter(|record| record.status == "stopping") {
                if let Some(waiter) = run_cancels.read().await.pending.get(run_id).cloned() {
                    waiter.notify_waiters();
                }
                let body = serde_json::json!({
                    "id": run_id,
                    "run_id": run_id,
                    "object": "hermes.run",
                    "status": record.status,
                });
                let resp = json_http_response(HTTP_OK, &body)?;
                writer.write_all(resp.as_bytes()).await?;
            } else if let Some(waiter) = run_cancels.read().await.pending.get(run_id).cloned() {
                waiter.notify_waiters();
                let body = serde_json::json!({
                    "id": run_id,
                    "run_id": run_id,
                    "object": "hermes.run",
                    "status": "stopping"
                });
                let resp = json_http_response(HTTP_OK, &body)?;
                writer.write_all(resp.as_bytes()).await?;
            } else {
                let resp = json_http_response(
                    HTTP_NOT_FOUND,
                    &api_error("Run not found", "not_found", 404),
                )?;
                writer.write_all(resp.as_bytes()).await?;
            }
            return Ok(());
        }
    }

    match (method, path) {
        ("GET", "/health") | ("GET", "/") => {
            let body = serde_json::json!({"status":"ok","adapter":"api-server"});
            let resp = json_http_response(HTTP_OK, &body)?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("GET", "/health/detailed") | ("GET", "/v1/health") => {
            let body = serde_json::json!({
                "status": "ok",
                "adapter": "api-server",
                "features": capabilities_response_body()["features"].clone(),
            });
            let resp = json_http_response(HTTP_OK, &body)?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("GET", "/v1/models") => {
            let resp = json_http_response(HTTP_OK, &models_response_body())?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("GET", "/v1/capabilities") => {
            let resp = json_http_response(HTTP_OK, &capabilities_response_body())?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("GET", "/v1/skills") => {
            let _category_filter = query_param(query, "category");
            let resp = json_http_response(HTTP_OK, &skills_response_body())?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("GET", "/v1/toolsets") => {
            let _enabled_filter = query_param(query, "enabled");
            let resp = json_http_response(HTTP_OK, &toolsets_response_body())?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("GET", _) if path.starts_with("/v1/responses/") => {
            let response_id = path.trim_start_matches("/v1/responses/");
            let stored = response_store.write().await.get(response_id);
            if let Some(stored) = stored {
                let resp = json_http_response(HTTP_OK, &stored.response)?;
                writer.write_all(resp.as_bytes()).await?;
            } else {
                let resp = json_http_response(
                    HTTP_NOT_FOUND,
                    &api_error("Response not found", "not_found", 404),
                )?;
                writer.write_all(resp.as_bytes()).await?;
            }
        }
        ("DELETE", _) if path.starts_with("/v1/responses/") => {
            let response_id = path.trim_start_matches("/v1/responses/");
            let deleted = response_store.write().await.delete(response_id);
            if deleted {
                let body = serde_json::json!({
                    "id": response_id,
                    "object": "response.deleted",
                    "deleted": true,
                });
                let resp = json_http_response(HTTP_OK, &body)?;
                writer.write_all(resp.as_bytes()).await?;
            } else {
                let resp = json_http_response(
                    HTTP_NOT_FOUND,
                    &api_error("Response not found", "not_found", 404),
                )?;
                writer.write_all(resp.as_bytes()).await?;
            }
        }
        ("GET", "/api/jobs") => {
            let (status, body) = api_jobs_list_response(
                cron_scheduler.clone(),
                boolish_query_param(query, "include_disabled"),
            )
            .await;
            let resp = json_http_response(status, &body)?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("POST", "/api/jobs") => {
            let (status, body) = api_jobs_create_response(cron_scheduler.clone(), body_bytes).await;
            let resp = json_http_response(status, &body)?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("GET", _) if parse_api_job_path(path).is_some() => {
            let job_id = parse_api_job_path(path).expect("guard checked path");
            let (status, body) = api_jobs_get_response(
                cron_scheduler.clone(),
                job_id,
                Some(ApiJobRequestContext {
                    method,
                    raw_path,
                    headers: &header_text,
                }),
            )
            .await;
            let resp = json_http_response(status, &body)?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("PATCH", _) if parse_api_job_path(path).is_some() => {
            let job_id = parse_api_job_path(path).expect("guard checked path");
            let (status, body) = api_jobs_update_response(
                cron_scheduler.clone(),
                job_id,
                body_bytes,
                Some(ApiJobRequestContext {
                    method,
                    raw_path,
                    headers: &header_text,
                }),
            )
            .await;
            let resp = json_http_response(status, &body)?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("DELETE", _) if parse_api_job_path(path).is_some() => {
            let job_id = parse_api_job_path(path).expect("guard checked path");
            let (status, body) = api_jobs_delete_response(
                cron_scheduler.clone(),
                job_id,
                Some(ApiJobRequestContext {
                    method,
                    raw_path,
                    headers: &header_text,
                }),
            )
            .await;
            let resp = json_http_response(status, &body)?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("POST", _) if parse_api_job_action_path(path).is_some() => {
            let (job_id, action) = parse_api_job_action_path(path).expect("guard checked path");
            let (status, body) = api_jobs_action_response(
                cron_scheduler.clone(),
                job_id,
                action,
                Some(ApiJobRequestContext {
                    method,
                    raw_path,
                    headers: &header_text,
                }),
            )
            .await;
            let resp = json_http_response(status, &body)?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("POST", "/api/cron/fire") => {
            let (status, body) =
                api_cron_fire_response(cron_scheduler.clone(), auth_header.as_deref(), body_bytes)
                    .await;
            let resp = json_http_response(status, &body)?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("POST", "/v1/runs") => {
            let body_str = String::from_utf8_lossy(body_bytes);
            let parsed: Result<RunRequest, _> = serde_json::from_str(&body_str);
            match parsed {
                Ok(req) => {
                    let run_id = make_run_id();
                    let mut input_messages = response_input_messages(req.input);
                    if !input_messages_have_non_empty_user_content(&input_messages) {
                        let resp = json_http_response(
                            HTTP_BAD_REQUEST,
                            &api_error(
                                "Request must include non-empty input",
                                "invalid_request_error",
                                400,
                            ),
                        )?;
                        writer.write_all(resp.as_bytes()).await?;
                        return Ok(());
                    }

                    let mut messages = req.conversation_history.unwrap_or_default();
                    messages.append(&mut input_messages);
                    let prompt = build_prompt_from_messages(&messages).unwrap_or_default();
                    if prompt.trim().is_empty() {
                        let resp = json_http_response(
                            HTTP_BAD_REQUEST,
                            &api_error(
                                "Request must include at least one user message",
                                "invalid_request_error",
                                400,
                            ),
                        )?;
                        writer.write_all(resp.as_bytes()).await?;
                        return Ok(());
                    }

                    let session_id = req.session_id.clone().unwrap_or_else(|| run_id.clone());
                    let user_id = req
                        .user
                        .clone()
                        .filter(|u| !u.trim().is_empty())
                        .unwrap_or_else(|| "api-client".to_string());
                    let record = RunRecord::new(
                        run_id.clone(),
                        session_id.clone(),
                        user_id.clone(),
                        req.model.clone(),
                        req.provider.clone(),
                        req.personality.clone(),
                    );
                    run_store.write().await.insert(record);

                    let inbound = ApiInboundRequest {
                        request_id: run_id.clone(),
                        session_id: session_id.clone(),
                        user_id,
                        model: req.model.clone(),
                        provider: req.provider.clone(),
                        personality: req.personality.clone(),
                        prompt,
                    };

                    tokio::spawn(run_background_request(
                        mailbox.clone(),
                        run_cancels.clone(),
                        run_store.clone(),
                        inbound_tx.clone(),
                        inbound,
                        session_id.clone(),
                    ));

                    let body = serde_json::json!({
                        "id": run_id,
                        "run_id": run_id,
                        "object": "hermes.run",
                        "status": "started",
                        "session_id": session_id,
                    });
                    let resp = json_http_response(
                        HttpStatus {
                            code: 202,
                            reason: "Accepted",
                        },
                        &body,
                    )?;
                    writer.write_all(resp.as_bytes()).await?;
                }
                Err(e) => {
                    let resp = json_http_response(
                        HTTP_BAD_REQUEST,
                        &api_error(
                            format!("Invalid request: {e}"),
                            "invalid_request_error",
                            400,
                        ),
                    )?;
                    writer.write_all(resp.as_bytes()).await?;
                }
            }
        }
        ("GET", _) if parse_run_events_path(path).is_some() => {
            let run_id = parse_run_events_path(path).expect("guard checked path");
            let record = wait_for_run_event_snapshot(run_store.clone(), run_id).await;
            if let Some(record) = record {
                let header = sse_http_header();
                writer.write_all(header.as_bytes()).await?;
                let data = run_events_sse_body(&record);
                writer.write_all(data.as_bytes()).await?;
            } else {
                let resp = json_http_response(
                    HTTP_NOT_FOUND,
                    &api_error("Run not found", "not_found", 404),
                )?;
                writer.write_all(resp.as_bytes()).await?;
            }
        }
        ("GET", _) if parse_get_run_path(path).is_some() => {
            let run_id = parse_get_run_path(path).expect("guard checked path");
            let record = run_store.read().await.get(run_id);
            if let Some(record) = record {
                let resp = json_http_response(HTTP_OK, &run_response_body(&record))?;
                writer.write_all(resp.as_bytes()).await?;
            } else {
                let resp = json_http_response(
                    HTTP_NOT_FOUND,
                    &api_error("Run not found", "not_found", 404),
                )?;
                writer.write_all(resp.as_bytes()).await?;
            }
        }
        ("POST", _) if parse_run_approval_path(path).is_some() => {
            let run_id = parse_run_approval_path(path).expect("guard checked path");
            if run_store.read().await.get(run_id).is_none() {
                let resp = json_http_response(
                    HTTP_NOT_FOUND,
                    &api_error("Run not found", "not_found", 404),
                )?;
                writer.write_all(resp.as_bytes()).await?;
                return Ok(());
            }
            let body_str = String::from_utf8_lossy(body_bytes);
            let parsed: Result<RunApprovalRequest, _> = serde_json::from_str(&body_str);
            match parsed {
                Ok(req) => {
                    let _choice = req.choice;
                    let _all = req.all;
                    let resp = json_http_response(
                        HTTP_CONFLICT,
                        &api_error("Run has no pending approval", "approval_not_pending", 409),
                    )?;
                    writer.write_all(resp.as_bytes()).await?;
                }
                Err(e) => {
                    let resp = json_http_response(
                        HTTP_BAD_REQUEST,
                        &api_error(
                            format!("Invalid request: {e}"),
                            "invalid_request_error",
                            400,
                        ),
                    )?;
                    writer.write_all(resp.as_bytes()).await?;
                }
            }
        }
        ("POST", "/v1/chat/completions") => {
            let body_str = String::from_utf8_lossy(body_bytes);

            let parsed: Result<ChatCompletionRequest, _> = serde_json::from_str(&body_str);
            match parsed {
                Ok(req) => {
                    let request_id = ApiServerAdapter::make_completion_id();
                    let model = req.model.as_deref().unwrap_or("hermes").to_string();
                    let prompt = build_prompt_from_messages(&req.messages).unwrap_or_default();
                    if prompt.trim().is_empty() {
                        let resp = json_http_response(
                            HTTP_BAD_REQUEST,
                            &api_error(
                                "Request must include at least one user message",
                                "invalid_request_error",
                                400,
                            ),
                        )?;
                        writer.write_all(resp.as_bytes()).await?;
                        return Ok(());
                    }

                    let session_id = req.session_id.unwrap_or_else(|| request_id.clone());
                    let mailbox_key = session_id.clone();
                    let user_id = req
                        .user
                        .filter(|u| !u.trim().is_empty())
                        .unwrap_or_else(|| "api-client".to_string());
                    let inbound = ApiInboundRequest {
                        request_id: request_id.clone(),
                        session_id,
                        user_id,
                        model: req.model.clone(),
                        provider: req.provider.clone(),
                        personality: req.personality.clone(),
                        prompt,
                    };

                    let reply = match run_api_request(
                        mailbox.clone(),
                        run_cancels.clone(),
                        inbound_tx.clone(),
                        inbound,
                        mailbox_key,
                    )
                    .await
                    {
                        Ok(reply) => reply,
                        Err((status, body)) => {
                            let resp = json_http_response(status, &body)?;
                            writer.write_all(resp.as_bytes()).await?;
                            return Ok(());
                        }
                    };

                    if req.stream {
                        let header = sse_http_header();
                        writer.write_all(header.as_bytes()).await?;

                        // Role chunk
                        let role_chunk =
                            ApiServerAdapter::make_stream_chunk(&request_id, &model, None, false);
                        let data = format!("data: {}\n\n", serde_json::to_string(&role_chunk)?);
                        writer.write_all(data.as_bytes()).await?;

                        // Content chunks
                        for chunk in reply.as_bytes().chunks(20) {
                            let text = String::from_utf8_lossy(chunk);
                            let sc = ApiServerAdapter::make_stream_chunk(
                                &request_id,
                                &model,
                                Some(&text),
                                false,
                            );
                            let data = format!("data: {}\n\n", serde_json::to_string(&sc)?);
                            writer.write_all(data.as_bytes()).await?;
                        }

                        // Finish chunk
                        let done_chunk =
                            ApiServerAdapter::make_stream_chunk(&request_id, &model, None, true);
                        let data = format!(
                            "data: {}\n\ndata: [DONE]\n\n",
                            serde_json::to_string(&done_chunk)?
                        );
                        writer.write_all(data.as_bytes()).await?;
                    } else {
                        let response = ApiServerAdapter::make_non_streaming_response(
                            &request_id,
                            &model,
                            &reply,
                        );
                        let resp = http_response(
                            HTTP_OK,
                            "application/json",
                            &serde_json::to_string(&response)?,
                        );
                        writer.write_all(resp.as_bytes()).await?;
                    }
                }
                Err(e) => {
                    let resp = json_http_response(
                        HTTP_BAD_REQUEST,
                        &api_error(
                            format!("Invalid request: {e}"),
                            "invalid_request_error",
                            400,
                        ),
                    )?;
                    writer.write_all(resp.as_bytes()).await?;
                }
            }
        }
        ("POST", "/v1/responses") => {
            let body_str = String::from_utf8_lossy(body_bytes);
            let parsed: Result<ResponsesRequest, _> = serde_json::from_str(&body_str);
            match parsed {
                Ok(req) => {
                    let request_id = format!(
                        "resp_{}",
                        uuid::Uuid::new_v4().to_string().replace('-', "")[..24].to_string()
                    );
                    let model = req.model.as_deref().unwrap_or("hermes").to_string();
                    let mut messages = response_input_messages(req.input);
                    let previous_response_id = if let Some(id) = req.previous_response_id.clone() {
                        let exists = response_store.write().await.get(&id).is_some();
                        if !exists {
                            let resp = json_http_response(
                                HTTP_NOT_FOUND,
                                &api_error("Previous response not found", "not_found", 404),
                            )?;
                            writer.write_all(resp.as_bytes()).await?;
                            return Ok(());
                        }
                        Some(id)
                    } else if let Some(conversation) = req.conversation.as_deref() {
                        response_store.write().await.get_conversation(conversation)
                    } else {
                        None
                    };

                    if let Some(previous_id) = previous_response_id.as_deref() {
                        if let Some(previous) = response_store.write().await.get(previous_id) {
                            let mut history = previous.conversation_history;
                            history.append(&mut messages);
                            messages = history;
                        }
                    }

                    let prompt = build_prompt_from_messages(&messages).unwrap_or_default();
                    if prompt.trim().is_empty() {
                        let resp = json_http_response(
                            HTTP_BAD_REQUEST,
                            &api_error(
                                "Request must include non-empty input",
                                "invalid_request_error",
                                400,
                            ),
                        )?;
                        writer.write_all(resp.as_bytes()).await?;
                        return Ok(());
                    }

                    let session_id = req
                        .session_id
                        .clone()
                        .or_else(|| req.conversation.clone())
                        .unwrap_or_else(|| request_id.clone());
                    let user_id = req
                        .user
                        .clone()
                        .filter(|u| !u.trim().is_empty())
                        .unwrap_or_else(|| "api-client".to_string());
                    let inbound = ApiInboundRequest {
                        request_id: request_id.clone(),
                        session_id: session_id.clone(),
                        user_id,
                        model: req.model.clone(),
                        provider: req.provider.clone(),
                        personality: req.personality.clone(),
                        prompt,
                    };

                    let reply = match run_api_request(
                        mailbox.clone(),
                        run_cancels.clone(),
                        inbound_tx.clone(),
                        inbound,
                        session_id,
                    )
                    .await
                    {
                        Ok(reply) => reply,
                        Err((status, body)) => {
                            let resp = json_http_response(status, &body)?;
                            writer.write_all(resp.as_bytes()).await?;
                            return Ok(());
                        }
                    };

                    let response = make_responses_api_body(
                        &request_id,
                        &model,
                        &reply,
                        previous_response_id.as_deref(),
                    );

                    if req.store {
                        let mut conversation_history = messages;
                        conversation_history.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: reply.clone(),
                        });
                        let mut guard = response_store.write().await;
                        guard.put(
                            request_id.clone(),
                            StoredApiResponse {
                                response: response.clone(),
                                conversation_history,
                            },
                        );
                        if let Some(conversation) = req.conversation {
                            guard.set_conversation(conversation, request_id.clone());
                        }
                    }

                    if req.stream {
                        let header = sse_http_header();
                        writer.write_all(header.as_bytes()).await?;
                        let data = format!("data: {}\n\ndata: [DONE]\n\n", response);
                        writer.write_all(data.as_bytes()).await?;
                    } else {
                        let resp = json_http_response(HTTP_OK, &response)?;
                        writer.write_all(resp.as_bytes()).await?;
                    }
                }
                Err(e) => {
                    let resp = json_http_response(
                        HTTP_BAD_REQUEST,
                        &api_error(
                            format!("Invalid request: {e}"),
                            "invalid_request_error",
                            400,
                        ),
                    )?;
                    writer.write_all(resp.as_bytes()).await?;
                }
            }
        }
        _ => {
            let resp =
                json_http_response(HTTP_NOT_FOUND, &api_error("Not found", "not_found", 404))?;
            writer.write_all(resp.as_bytes()).await?;
        }
    }

    Ok(())
}

fn build_prompt_from_messages(messages: &[ChatMessage]) -> Option<String> {
    if messages.is_empty() {
        return None;
    }

    let has_user_message = messages
        .iter()
        .any(|m| m.role.trim().eq_ignore_ascii_case("user"));
    if !has_user_message {
        return None;
    }

    if messages.len() == 1 {
        let only = &messages[0];
        if only.role.trim().eq_ignore_ascii_case("user") {
            return Some(only.content.clone());
        }
    }

    let mut prompt = String::new();
    for (idx, msg) in messages.iter().enumerate() {
        let role = msg.role.trim();
        let role_upper = role.to_ascii_uppercase();
        if idx > 0 {
            prompt.push_str("\n\n");
        }
        prompt.push('[');
        prompt.push_str(if role.is_empty() {
            "MESSAGE"
        } else {
            role_upper.as_str()
        });
        prompt.push_str("]\n");
        prompt.push_str(&msg.content);
    }

    if prompt.trim().is_empty() {
        None
    } else {
        Some(prompt)
    }
}

fn parse_stop_run_path(path: &str) -> Option<&str> {
    let run_id = path.strip_prefix("/v1/runs/")?.strip_suffix("/stop")?;
    if run_id.is_empty() {
        None
    } else {
        Some(run_id)
    }
}

fn parse_run_events_path(path: &str) -> Option<&str> {
    let run_id = path.strip_prefix("/v1/runs/")?.strip_suffix("/events")?;
    if run_id.is_empty() || run_id.contains('/') {
        None
    } else {
        Some(run_id)
    }
}

fn parse_run_approval_path(path: &str) -> Option<&str> {
    let run_id = path.strip_prefix("/v1/runs/")?.strip_suffix("/approval")?;
    if run_id.is_empty() || run_id.contains('/') {
        None
    } else {
        Some(run_id)
    }
}

fn parse_get_run_path(path: &str) -> Option<&str> {
    let run_id = path.strip_prefix("/v1/runs/")?;
    if run_id.is_empty() || run_id.contains('/') {
        None
    } else {
        Some(run_id)
    }
}

fn parse_api_job_action_path(path: &str) -> Option<(&str, &str)> {
    let tail = path.strip_prefix("/api/jobs/")?;
    let (job_id, action) = tail.split_once('/')?;
    if job_id.is_empty() || action.contains('/') {
        None
    } else {
        Some((job_id, action))
    }
}

fn parse_api_job_path(path: &str) -> Option<&str> {
    let job_id = path.strip_prefix("/api/jobs/")?;
    if job_id.is_empty() || job_id.contains('/') {
        None
    } else {
        Some(job_id)
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_content_length(headers: &str) -> usize {
    headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0)
}

async fn read_http_request(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::AsyncReadExt;

    let mut buf = Vec::with_capacity(16 * 1024);
    let mut chunk = [0_u8; 8192];
    let mut expected_total: Option<usize> = None;

    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 2 * 1024 * 1024 {
            break;
        }

        if expected_total.is_none() {
            if let Some(header_end) = find_bytes(&buf, b"\r\n\r\n") {
                let header_text = String::from_utf8_lossy(&buf[..header_end]);
                let body_len = parse_content_length(&header_text);
                expected_total = Some(header_end + 4 + body_len);
            }
        }
        if let Some(total) = expected_total {
            if buf.len() >= total {
                break;
            }
        }
    }

    Ok(buf)
}
