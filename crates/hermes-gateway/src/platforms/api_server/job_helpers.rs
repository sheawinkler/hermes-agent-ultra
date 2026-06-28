fn boolish_query_param(query: Option<&str>, key: &str) -> bool {
    query_param(query, key)
        .as_deref()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn validate_api_job_id(job_id: &str) -> Result<(), serde_json::Value> {
    if job_id.is_empty() || job_id.len() > 64 {
        return Err(api_error("Invalid job id", "invalid_request_error", 400));
    }
    if job_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        Ok(())
    } else {
        Err(api_error("Invalid job id", "invalid_request_error", 400))
    }
}

fn clean_audit_log_value(raw: Option<&str>, max_len: usize) -> String {
    raw.unwrap_or_default()
        .replace(['\r', '\n'], " ")
        .trim()
        .chars()
        .take(max_len)
        .collect()
}

fn request_header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|line| {
        let (header_name, value) = line.split_once(':')?;
        header_name
            .trim()
            .eq_ignore_ascii_case(name)
            .then_some(value.trim())
    })
}

fn audit_log_suffix(ctx: ApiJobRequestContext<'_>) -> String {
    let mut fields = Vec::new();
    let forwarded_for =
        clean_audit_log_value(request_header_value(ctx.headers, "X-Forwarded-For"), 200);
    let real_ip = clean_audit_log_value(request_header_value(ctx.headers, "X-Real-IP"), 200);
    let user_agent = clean_audit_log_value(request_header_value(ctx.headers, "User-Agent"), 300);
    let method = clean_audit_log_value(Some(ctx.method), 16);
    let path = clean_audit_log_value(Some(ctx.raw_path), 500);

    if !forwarded_for.is_empty() {
        fields.push(format!("forwarded_for={forwarded_for:?}"));
    }
    if !real_ip.is_empty() {
        fields.push(format!("real_ip={real_ip:?}"));
    }
    if !method.is_empty() {
        fields.push(format!("method={method:?}"));
    }
    if !path.is_empty() {
        fields.push(format!("path={path:?}"));
    }
    if !user_agent.is_empty() {
        fields.push(format!("user_agent={user_agent:?}"));
    }
    if fields.is_empty() {
        "source='unknown'".to_string()
    } else {
        fields.join(" ")
    }
}

fn invalid_api_job_id_response(
    job_id: &str,
    ctx: Option<ApiJobRequestContext<'_>>,
) -> Option<(HttpStatus, serde_json::Value)> {
    match validate_api_job_id(job_id) {
        Ok(()) => None,
        Err(body) => {
            if let Some(ctx) = ctx {
                warn!(
                    "Cron jobs API rejected invalid job_id {:?}: {}",
                    job_id,
                    audit_log_suffix(ctx)
                );
            }
            Some((HTTP_BAD_REQUEST, body))
        }
    }
}

fn deliver_target_name(target: &DeliverTarget) -> &'static str {
    match target {
        DeliverTarget::Origin => "origin",
        DeliverTarget::Local => "local",
        DeliverTarget::Telegram => "telegram",
        DeliverTarget::Discord => "discord",
        DeliverTarget::Slack => "slack",
        DeliverTarget::Email => "email",
        DeliverTarget::WhatsApp => "whatsapp",
        DeliverTarget::Signal => "signal",
        DeliverTarget::Matrix => "matrix",
        DeliverTarget::Mattermost => "mattermost",
        DeliverTarget::DingTalk => "dingtalk",
        DeliverTarget::Feishu => "feishu",
        DeliverTarget::WeCom => "wecom",
        DeliverTarget::Weixin => "weixin",
        DeliverTarget::BlueBubbles => "bluebubbles",
        DeliverTarget::Sms => "sms",
        DeliverTarget::HomeAssistant => "homeassistant",
        DeliverTarget::Ntfy => "ntfy",
    }
}

fn parse_deliver_target(raw: &str) -> Option<DeliverTarget> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "origin" => Some(DeliverTarget::Origin),
        "local" => Some(DeliverTarget::Local),
        "telegram" => Some(DeliverTarget::Telegram),
        "discord" => Some(DeliverTarget::Discord),
        "slack" => Some(DeliverTarget::Slack),
        "email" => Some(DeliverTarget::Email),
        "whatsapp" => Some(DeliverTarget::WhatsApp),
        "signal" => Some(DeliverTarget::Signal),
        "matrix" => Some(DeliverTarget::Matrix),
        "mattermost" => Some(DeliverTarget::Mattermost),
        "dingtalk" => Some(DeliverTarget::DingTalk),
        "feishu" => Some(DeliverTarget::Feishu),
        "wecom" => Some(DeliverTarget::WeCom),
        "weixin" | "wechat" => Some(DeliverTarget::Weixin),
        "bluebubbles" | "blue_bubbles" => Some(DeliverTarget::BlueBubbles),
        "sms" => Some(DeliverTarget::Sms),
        "homeassistant" | "home_assistant" => Some(DeliverTarget::HomeAssistant),
        "ntfy" => Some(DeliverTarget::Ntfy),
        _ => None,
    }
}

fn parse_api_deliver_config(
    raw: Option<&serde_json::Value>,
) -> Result<Option<DeliverConfig>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    if let Some(value) = raw.as_str() {
        let target = parse_deliver_target(value)
            .ok_or_else(|| format!("Unknown deliver target '{value}'"))?;
        return Ok(Some(DeliverConfig {
            target,
            platform: None,
        }));
    }
    let Some(obj) = raw.as_object() else {
        return Err("deliver must be a string or object".to_string());
    };
    let target_raw = obj
        .get("target")
        .or_else(|| obj.get("type"))
        .or_else(|| obj.get("name"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "deliver.target is required".to_string())?;
    let target = parse_deliver_target(target_raw)
        .ok_or_else(|| format!("Unknown deliver target '{target_raw}'"))?;
    let platform = obj
        .get("platform")
        .or_else(|| obj.get("recipient"))
        .or_else(|| obj.get("chat_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);
    Ok(Some(DeliverConfig { target, platform }))
}

fn api_cron_job_body(job: &CronJob) -> serde_json::Value {
    let deliver = job.deliver.as_ref().map(|deliver| {
        let mut value = serde_json::json!(deliver_target_name(&deliver.target));
        if let Some(platform) = deliver.platform.as_deref() {
            value = serde_json::json!({
                "target": deliver_target_name(&deliver.target),
                "platform": platform,
            });
        }
        value
    });

    serde_json::json!({
        "id": job.id,
        "name": job.name,
        "schedule": job.schedule,
        "prompt": job.prompt,
        "deliver": deliver,
        "enabled": job.status == JobStatus::Active,
        "status": job.status.to_string(),
        "created_at": job.created_at,
        "last_run": job.last_run,
        "next_run": job.next_run,
        "repeat": job.repeat,
        "run_count": job.run_count,
        "skills": job.skills,
        "script": job.script,
        "no_agent": job.no_agent,
        "script_timeout_seconds": job.script_timeout_seconds,
        "script_shell": job.script_shell,
        "workdir": job.workdir,
        "context_from": job.context_from,
        "last_output": job.last_output,
    })
}

fn cron_error_to_http(error: CronError) -> (HttpStatus, serde_json::Value) {
    match error {
        CronError::JobNotFound(id) => (
            HTTP_NOT_FOUND,
            api_error(format!("Job not found: {id}"), "not_found", 404),
        ),
        CronError::InvalidJob(message) => (
            HTTP_BAD_REQUEST,
            api_error(message, "invalid_request_error", 400),
        ),
        CronError::JobAlreadyExists(id) => (
            HTTP_CONFLICT,
            api_error(format!("Job already exists: {id}"), "conflict_error", 409),
        ),
        other => (
            HTTP_BAD_GATEWAY,
            api_error(other.to_string(), "internal_error", 502),
        ),
    }
}

fn validate_api_cron_create(req: ApiCronJobCreateRequest) -> Result<CronJob, serde_json::Value> {
    let name = req
        .name
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| api_error("name is required", "invalid_request_error", 400))?;
    if name.chars().count() > 200 {
        return Err(api_error(
            "Name must be 200 characters or fewer",
            "invalid_request_error",
            400,
        ));
    }

    let schedule = req
        .schedule
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| api_error("schedule is required", "invalid_request_error", 400))?;

    let prompt = req.prompt.unwrap_or_default().trim().to_string();
    if prompt.chars().count() > 5000 {
        return Err(api_error(
            "Prompt must be 5000 characters or fewer",
            "invalid_request_error",
            400,
        ));
    }
    if prompt.is_empty() && req.script.as_ref().is_none_or(|v| v.trim().is_empty()) {
        return Err(api_error(
            "prompt is required",
            "invalid_request_error",
            400,
        ));
    }
    if matches!(req.repeat, Some(0)) {
        return Err(api_error(
            "repeat must be greater than zero",
            "invalid_request_error",
            400,
        ));
    }

    let mut job = CronJob::new(schedule, prompt);
    job.name = Some(name);
    job.deliver = match req.deliver.as_ref() {
        Some(raw) => parse_api_deliver_config(Some(raw))
            .map_err(|message| api_error(message, "invalid_request_error", 400))?,
        None => Some(DeliverConfig {
            target: DeliverTarget::Local,
            platform: None,
        }),
    };
    job.repeat = req.repeat;
    job.skills = req
        .skills
        .map(|skills| {
            skills
                .into_iter()
                .map(|skill| skill.trim().to_string())
                .filter(|skill| !skill.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|skills| !skills.is_empty());
    job.script = req
        .script
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    job.no_agent = req.no_agent.unwrap_or(false);
    if req.enabled == Some(false) {
        job.status = JobStatus::Paused;
        job.next_run = None;
    }
    Ok(job)
}

fn apply_api_cron_updates(
    mut job: CronJob,
    updates: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<CronJob>, serde_json::Value> {
    let mut changed = false;
    for (key, value) in updates {
        match key.as_str() {
            "name" => {
                let name = value.as_str().unwrap_or_default().trim().to_string();
                if name.chars().count() > 200 {
                    return Err(api_error(
                        "Name must be 200 characters or fewer",
                        "invalid_request_error",
                        400,
                    ));
                }
                job.name = (!name.is_empty()).then_some(name);
                changed = true;
            }
            "schedule" => {
                let schedule = value.as_str().unwrap_or_default().trim().to_string();
                if schedule.is_empty() {
                    return Err(api_error(
                        "schedule is required",
                        "invalid_request_error",
                        400,
                    ));
                }
                job.schedule = schedule;
                job.next_run = None;
                changed = true;
            }
            "prompt" => {
                let prompt = value.as_str().unwrap_or_default().trim().to_string();
                if prompt.chars().count() > 5000 {
                    return Err(api_error(
                        "Prompt must be 5000 characters or fewer",
                        "invalid_request_error",
                        400,
                    ));
                }
                job.prompt = prompt;
                changed = true;
            }
            "deliver" => {
                job.deliver = parse_api_deliver_config(Some(value))
                    .map_err(|message| api_error(message, "invalid_request_error", 400))?;
                changed = true;
            }
            "enabled" => {
                if let Some(enabled) = value.as_bool() {
                    job.status = if enabled {
                        JobStatus::Active
                    } else {
                        JobStatus::Paused
                    };
                    if enabled {
                        job.next_run = None;
                    }
                    changed = true;
                }
            }
            "repeat" => {
                let repeat = if value.is_null() {
                    None
                } else {
                    let Some(raw) = value.as_u64() else {
                        return Err(api_error(
                            "repeat must be an integer",
                            "invalid_request_error",
                            400,
                        ));
                    };
                    if raw == 0 {
                        return Err(api_error(
                            "repeat must be greater than zero",
                            "invalid_request_error",
                            400,
                        ));
                    }
                    let repeat = u32::try_from(raw).map_err(|_| {
                        api_error("repeat is too large", "invalid_request_error", 400)
                    })?;
                    Some(repeat)
                };
                job.repeat = repeat;
                changed = true;
            }
            "skills" => {
                if value.is_null() {
                    job.skills = None;
                } else {
                    let Some(items) = value.as_array() else {
                        return Err(api_error(
                            "skills must be an array",
                            "invalid_request_error",
                            400,
                        ));
                    };
                    let skills = items
                        .iter()
                        .filter_map(|item| item.as_str())
                        .map(str::trim)
                        .filter(|skill| !skill.is_empty())
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>();
                    job.skills = (!skills.is_empty()).then_some(skills);
                }
                changed = true;
            }
            "skill" => {
                job.skills = value
                    .as_str()
                    .map(str::trim)
                    .filter(|skill| !skill.is_empty())
                    .map(|skill| vec![skill.to_string()]);
                changed = true;
            }
            "script" => {
                job.script = value
                    .as_str()
                    .map(str::trim)
                    .filter(|script| !script.is_empty())
                    .map(ToOwned::to_owned);
                changed = true;
            }
            "no_agent" => {
                if let Some(no_agent) = value.as_bool() {
                    job.no_agent = no_agent;
                    changed = true;
                }
            }
            _ => {}
        }
    }
    Ok(changed.then_some(job))
}

fn response_input_messages(input: ResponseInput) -> Vec<ChatMessage> {
    match input {
        ResponseInput::Text(text) => vec![ChatMessage {
            role: "user".to_string(),
            content: text,
        }],
        ResponseInput::Messages(messages) => messages,
    }
}

fn input_messages_have_non_empty_user_content(messages: &[ChatMessage]) -> bool {
    messages.iter().any(|message| {
        message.role.trim().eq_ignore_ascii_case("user") && !message.content.trim().is_empty()
    })
}

fn estimated_usage(prompt: &str, content: &str) -> UsageInfo {
    let prompt_tokens = (prompt.len() as u32 / 4).max(1);
    let completion_tokens = (content.len() as u32 / 4).max(1);
    UsageInfo {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens + completion_tokens,
    }
}
