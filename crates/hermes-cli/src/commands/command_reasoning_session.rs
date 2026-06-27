fn reasoning_display_flag() -> &'static std::sync::atomic::AtomicBool {
    static SHOW_REASONING: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    &SHOW_REASONING
}

fn reasoning_full_flag() -> &'static std::sync::atomic::AtomicBool {
    static FULL_REASONING: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    &FULL_REASONING
}

fn set_reasoning_display(enabled: bool) {
    reasoning_display_flag().store(enabled, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn set_reasoning_full(enabled: bool) {
    reasoning_full_flag().store(enabled, std::sync::atomic::Ordering::Relaxed);
}

fn toggle_reasoning_display() -> bool {
    let prev = reasoning_display_flag().fetch_xor(true, std::sync::atomic::Ordering::Relaxed);
    !prev
}

fn reasoning_display_enabled() -> bool {
    reasoning_display_flag().load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn reasoning_full_enabled() -> bool {
    reasoning_full_flag().load(std::sync::atomic::Ordering::Relaxed)
}

fn parse_reasoning_effort(raw: &str) -> Result<Option<&'static str>, AgentError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "minimal" | "min" => Ok(Some("minimal")),
        "low" => Ok(Some("low")),
        "medium" | "med" => Ok(Some("medium")),
        "high" => Ok(Some("high")),
        "xhigh" | "max" => Ok(Some("xhigh")),
        "auto" | "default" | "clear" | "reset" | "none" => Ok(None),
        other => Err(AgentError::Config(format!(
            "Unknown reasoning effort '{}'. Use one of: minimal, low, medium, high, xhigh, auto.",
            other
        ))),
    }
}

fn resolve_provider_key(cfg: &GatewayConfig, provider: &str) -> String {
    cfg.llm_providers
        .keys()
        .find(|key| key.eq_ignore_ascii_case(provider))
        .cloned()
        .unwrap_or_else(|| provider.trim().to_ascii_lowercase())
}

fn gemini_thinking_level_for_effort(effort: &str) -> &'static str {
    match effort {
        "minimal" | "low" => "low",
        "medium" => "medium",
        "high" | "xhigh" => "high",
        _ => "medium",
    }
}

fn openai_reasoning_effort_for_level(effort: &str) -> &'static str {
    match effort {
        "minimal" => "low",
        "xhigh" => "high",
        "low" => "low",
        "medium" => "medium",
        "high" => "high",
        _ => "medium",
    }
}

fn set_provider_reasoning_effort(cfg: &mut GatewayConfig, provider: &str, effort: Option<&str>) {
    let provider_key = resolve_provider_key(cfg, provider);
    let provider_cfg = cfg
        .llm_providers
        .entry(provider_key.clone())
        .or_default();

    let mut body_map = provider_cfg
        .extra_body
        .take()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    match effort {
        Some(level) => {
            // Keep request payloads OpenAI-compatible for Nous/OpenRouter/OpenAI routes:
            // use `reasoning.effort` (`low|medium|high`) instead of legacy top-level
            // `reasoning_effort` which can trigger schema validation errors.
            body_map.remove("reasoning_effort");
            let mut reasoning_obj = body_map
                .get("reasoning")
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            let mapped_reasoning = if provider_key.eq_ignore_ascii_case("opencode-go") {
                level
            } else {
                openai_reasoning_effort_for_level(level)
            };
            reasoning_obj.insert(
                "effort".to_string(),
                serde_json::Value::String(mapped_reasoning.to_string()),
            );
            body_map.insert(
                "reasoning".to_string(),
                serde_json::Value::Object(reasoning_obj),
            );

            if provider_key.contains("gemini") || provider_key == "google" {
                let level_mapped = gemini_thinking_level_for_effort(level);
                let mut google_obj = body_map
                    .get("google")
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                let mut thinking_cfg = google_obj
                    .get("thinking_config")
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                thinking_cfg.insert(
                    "thinking_level".to_string(),
                    serde_json::Value::String(level_mapped.to_string()),
                );
                google_obj.insert(
                    "thinking_config".to_string(),
                    serde_json::Value::Object(thinking_cfg.clone()),
                );
                body_map.insert("google".to_string(), serde_json::Value::Object(google_obj));
                body_map.insert(
                    "thinking_config".to_string(),
                    serde_json::Value::Object(thinking_cfg),
                );
            }
        }
        None => {
            body_map.remove("reasoning_effort");
            if let Some(reasoning_obj) = body_map
                .get_mut("reasoning")
                .and_then(|value| value.as_object_mut())
            {
                reasoning_obj.remove("effort");
                if reasoning_obj.is_empty() {
                    body_map.remove("reasoning");
                }
            }
            body_map.remove("thinking_config");
            if let Some(google_obj) = body_map
                .get_mut("google")
                .and_then(|value| value.as_object_mut())
            {
                google_obj.remove("thinking_config");
                if google_obj.is_empty() {
                    body_map.remove("google");
                }
            }
        }
    }

    provider_cfg.extra_body = if body_map.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(body_map))
    };
}

fn provider_reasoning_effort(cfg: &GatewayConfig, provider: &str) -> Option<String> {
    let provider_key = resolve_provider_key(cfg, provider);
    cfg.llm_providers
        .get(&provider_key)
        .and_then(|entry| entry.extra_body.as_ref())
        .and_then(|body| {
            body.get("reasoning")
                .and_then(|value| value.get("effort"))
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
                .or_else(|| {
                    body.get("reasoning_effort")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string)
                })
        })
}

fn handle_reasoning_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        let enabled = toggle_reasoning_display();
        if enabled {
            emit_command_output(
                app,
                "Reasoning display: ON — model reasoning will be shown.",
            );
        } else {
            emit_command_output(
                app,
                "Reasoning display: OFF — model reasoning will be hidden.",
            );
        }
        return Ok(CommandResult::Handled);
    }

    match args[0].trim().to_ascii_lowercase().as_str() {
        "status" => {
            let (provider, _) = split_provider_model(&app.current_model);
            let effort = provider_reasoning_effort(&app.config, provider)
                .unwrap_or_else(|| "auto".to_string());
            emit_command_output(
                app,
                format!(
                    "Reasoning status\n- display: {}\n- mode: {}\n- effort: {}\n- provider: {}",
                    if reasoning_display_enabled() {
                        "ON"
                    } else {
                        "OFF"
                    },
                    if reasoning_full_enabled() {
                        "full"
                    } else {
                        "clamp"
                    },
                    effort,
                    provider
                ),
            );
        }
        "toggle" => {
            let enabled = toggle_reasoning_display();
            emit_command_output(
                app,
                format!(
                    "Reasoning display: {} — model reasoning will be {}.",
                    if enabled { "ON" } else { "OFF" },
                    if enabled { "shown" } else { "hidden" }
                ),
            );
        }
        "on" | "show" => {
            set_reasoning_display(true);
            emit_command_output(
                app,
                "Reasoning display: ON — model reasoning will be shown.",
            );
        }
        "off" | "hide" => {
            set_reasoning_display(false);
            emit_command_output(
                app,
                "Reasoning display: OFF — model reasoning will be hidden.",
            );
        }
        "full" => {
            set_reasoning_full(true);
            emit_command_output(
                app,
                "Reasoning mode: full — live thinking previews keep complete text.",
            );
        }
        "clamp" => {
            set_reasoning_full(false);
            emit_command_output(
                app,
                "Reasoning mode: clamp — live thinking previews use compact caps.",
            );
        }
        "set" | "level" | "effort" => {
            if args.len() < 2 {
                emit_command_output(
                    app,
                    "Usage: /reasoning set <minimal|low|medium|high|xhigh|auto>",
                );
                return Ok(CommandResult::Handled);
            }
            let effort = parse_reasoning_effort(args[1])?;
            let provider = split_provider_model(&app.current_model).0.to_string();
            let current_model = app.current_model.clone();
            app.config = Arc::new({
                let mut cfg = (*app.config).clone();
                set_provider_reasoning_effort(&mut cfg, &provider, effort);
                cfg
            });
            app.switch_model(&current_model);
            let effort_label = effort.unwrap_or("auto");
            emit_command_output(
                app,
                format!(
                    "Reasoning effort set to `{}` for provider `{}` (model `{}`).",
                    effort_label, provider, current_model
                ),
            );
        }
        "help" => {
            emit_command_output(
                app,
                "Reasoning controls:\n\
                 - /reasoning                 Toggle reasoning display\n\
                 - /reasoning status          Show display + mode + effort state\n\
                 - /reasoning on|off          Explicitly show/hide reasoning\n\
                 - /reasoning full|clamp      Keep full thinking previews or compact them\n\
                 - /reasoning set <level>     Set provider reasoning effort\n\
                 Levels: minimal, low, medium, high, xhigh, auto",
            );
        }
        shorthand => {
            let effort = parse_reasoning_effort(shorthand)?;
            let provider = split_provider_model(&app.current_model).0.to_string();
            let current_model = app.current_model.clone();
            app.config = Arc::new({
                let mut cfg = (*app.config).clone();
                set_provider_reasoning_effort(&mut cfg, &provider, effort);
                cfg
            });
            app.switch_model(&current_model);
            emit_command_output(
                app,
                format!(
                    "Reasoning effort set to `{}` for provider `{}` (model `{}`).",
                    effort.unwrap_or("auto"),
                    provider,
                    current_model
                ),
            );
        }
    }
    Ok(CommandResult::Handled)
}

fn replay_enabled_runtime() -> bool {
    std::env::var("HERMES_REPLAY_ENABLED")
        .ok()
        .is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

fn replay_log_path_for_session(session_id: &str) -> PathBuf {
    let sid = if session_id.trim().is_empty() {
        "session".to_string()
    } else {
        session_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    hermes_config::hermes_home()
        .join("logs")
        .join("replay")
        .join(format!("{sid}.jsonl"))
}

fn render_replay_trace_tail(path: &Path, limit: usize) -> Result<String, AgentError> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        AgentError::Io(format!(
            "Failed to read replay log {}: {}",
            path.display(),
            e
        ))
    })?;
    let lines: Vec<&str> = raw
        .lines()
        .rev()
        .take(limit.max(1))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if lines.is_empty() {
        return Ok("Replay log is empty.".to_string());
    }
    let mut out = String::new();
    for line in lines {
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                let _ = writeln!(out, "{}", line);
                continue;
            }
        };
        let seq = value
            .get("seq")
            .and_then(|v| v.as_u64())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".to_string());
        let event = value
            .get("event")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let trace_id = value
            .get("trace_id")
            .and_then(|v| v.as_str())
            .unwrap_or("missing");
        let prev_hash = value
            .get("prev_hash")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let event_hash = value
            .get("event_hash")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let turn = value
            .get("payload")
            .and_then(|payload| payload.get("turn"))
            .and_then(|turn| turn.as_u64())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_string());
        let _ = writeln!(
            out,
            "#{seq:<4} turn={turn:<3} event={event:<24} trace={trace_id} prev={prev_hash} hash={event_hash}"
        );
    }
    Ok(out.trim_end().to_string())
}

fn replay_entries(path: &Path, limit: usize) -> Result<Vec<serde_json::Value>, AgentError> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        AgentError::Io(format!(
            "Failed to read replay log {}: {}",
            path.display(),
            e
        ))
    })?;
    Ok(raw
        .lines()
        .rev()
        .take(limit.max(1))
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect())
}

fn render_replay_trace_focus(
    path: &Path,
    trace_id: &str,
    limit: usize,
) -> Result<String, AgentError> {
    let trace_filter = trace_id.trim();
    if trace_filter.is_empty() {
        return Ok("Usage: /raw trace focus <trace-id> [N]".to_string());
    }
    let rows = replay_entries(path, limit)?;
    let filtered: Vec<serde_json::Value> = rows
        .into_iter()
        .filter(|row| {
            row.get("trace_id")
                .and_then(|v| v.as_str())
                .is_some_and(|v| v == trace_filter || v.contains(trace_filter))
        })
        .collect();
    if filtered.is_empty() {
        return Ok(format!(
            "No replay events found for trace '{}' in {}.",
            trace_filter,
            path.display()
        ));
    }
    let mut out = String::new();
    let _ = writeln!(out, "Replay trace focus: {}", trace_filter);
    let _ = writeln!(out, "events: {}", filtered.len());
    let _ = writeln!(out, "path: {}", path.display());
    let _ = writeln!(out);
    for row in filtered {
        let seq = row.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
        let event = row
            .get("event")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let turn = row
            .get("payload")
            .and_then(|payload| payload.get("turn"))
            .and_then(|turn| turn.as_u64())
            .unwrap_or(0);
        let preview = row
            .get("payload")
            .map(|v| truncate_chars(&v.to_string(), 120))
            .unwrap_or_else(|| "{}".to_string());
        let _ = writeln!(out, "#{seq:<4} turn={turn:<3} event={event:<24} {preview}");
    }
    Ok(out.trim_end().to_string())
}

fn render_replay_trace_graph(path: &Path, limit: usize) -> Result<String, AgentError> {
    let rows = replay_entries(path, limit)?;
    if rows.is_empty() {
        return Ok("Replay graph: no entries in current window.".to_string());
    }
    let mut out = String::new();
    let _ = writeln!(out, "Replay lineage graph");
    let _ = writeln!(out, "--------------------");
    let _ = writeln!(out, "window={} path={}", rows.len(), path.display());
    for row in rows {
        let seq = row
            .get("seq")
            .and_then(|value| value.as_u64())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".to_string());
        let event = row
            .get("event")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        let trace_id = row
            .get("trace_id")
            .and_then(|value| value.as_str())
            .unwrap_or("missing");
        let prev = row
            .get("prev_hash")
            .and_then(|value| value.as_str())
            .unwrap_or("-");
        let hash = row
            .get("event_hash")
            .and_then(|value| value.as_str())
            .unwrap_or("-");
        let _ = writeln!(
            out,
            "#{:<4} {:<20} trace={} {} -> {}",
            seq, event, trace_id, prev, hash
        );
    }
    Ok(out.trim_end().to_string())
}

fn replay_trace_integrity(path: &Path) -> Result<(usize, usize, usize), AgentError> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        AgentError::Io(format!(
            "Failed to read replay log {}: {}",
            path.display(),
            e
        ))
    })?;
    let mut entries = 0usize;
    let mut parse_errors = 0usize;
    let mut chain_breaks = 0usize;
    let mut last_event_hash: Option<String> = None;
    for line in raw.lines() {
        let parsed: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                parse_errors = parse_errors.saturating_add(1);
                continue;
            }
        };
        entries = entries.saturating_add(1);
        let prev_hash = parsed
            .get("prev_hash")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let event_hash = parsed
            .get("event_hash")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let (Some(last), Some(prev)) = (last_event_hash.as_ref(), prev_hash.as_ref()) {
            if last != prev {
                chain_breaks = chain_breaks.saturating_add(1);
            }
        }
        if let Some(curr) = event_hash {
            last_event_hash = Some(curr);
        }
    }
    Ok((entries, parse_errors, chain_breaks))
}

fn export_replay_trace_json(
    replay_path: &Path,
    limit: usize,
    output_path: &Path,
) -> Result<usize, AgentError> {
    let raw = std::fs::read_to_string(replay_path).map_err(|e| {
        AgentError::Io(format!(
            "Failed to read replay log {}: {}",
            replay_path.display(),
            e
        ))
    })?;
    let rows: Vec<serde_json::Value> = raw
        .lines()
        .rev()
        .take(limit.max(1))
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    let payload = serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "source_replay": replay_path.display().to_string(),
        "rows": rows,
    });
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            AgentError::Io(format!(
                "Failed to create replay export directory {}: {}",
                parent.display(),
                e
            ))
        })?;
    }
    std::fs::write(
        output_path,
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string()),
    )
    .map_err(|e| {
        AgentError::Io(format!(
            "Failed to write replay export {}: {}",
            output_path.display(),
            e
        ))
    })?;
    Ok(payload["rows"].as_array().map(|arr| arr.len()).unwrap_or(0))
}

fn handle_raw_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args
        .first()
        .is_some_and(|sub| sub.eq_ignore_ascii_case("trace"))
    {
        let replay_path = replay_log_path_for_session(&app.session_id);
        let sub = args.get(1).map(|s| s.trim().to_ascii_lowercase());
        match sub.as_deref() {
            None | Some("status") => {
                emit_command_output(
                    app,
                    format!(
                        "Replay trace: {}{}\nSession: {}\nPath: {}\nUsage: /raw trace [on|off|toggle|status|tail [N]|focus <trace-id> [N]|graph [N]|verify|export [N] [PATH]|path]",
                        if replay_enabled_runtime() { "ON" } else { "OFF" },
                        if replay_path.exists() { "" } else { " (no log yet)" },
                        app.session_id,
                        replay_path.display()
                    ),
                );
            }
            Some("path") => {
                emit_command_output(app, format!("Replay path: {}", replay_path.display()));
            }
            Some("tail") => {
                let limit = args
                    .get(2)
                    .and_then(|raw| raw.trim().parse::<usize>().ok())
                    .unwrap_or(20)
                    .clamp(1, 200);
                if !replay_path.exists() {
                    emit_command_output(
                        app,
                        format!(
                            "Replay log not found for current session yet: {}",
                            replay_path.display()
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                let rendered = render_replay_trace_tail(&replay_path, limit)?;
                emit_command_output(app, rendered);
            }
            Some("focus") => {
                let Some(trace_id) = args.get(2).copied() else {
                    emit_command_output(app, "Usage: /raw trace focus <trace-id> [N]");
                    return Ok(CommandResult::Handled);
                };
                let limit = args
                    .get(3)
                    .and_then(|raw| raw.trim().parse::<usize>().ok())
                    .unwrap_or(150)
                    .clamp(1, 1000);
                if !replay_path.exists() {
                    emit_command_output(
                        app,
                        format!(
                            "Replay log not found for current session yet: {}",
                            replay_path.display()
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                let rendered = render_replay_trace_focus(&replay_path, trace_id, limit)?;
                emit_command_output(app, rendered);
            }
            Some("graph") => {
                let limit = args
                    .get(2)
                    .and_then(|raw| raw.trim().parse::<usize>().ok())
                    .unwrap_or(80)
                    .clamp(1, 500);
                if !replay_path.exists() {
                    emit_command_output(
                        app,
                        format!(
                            "Replay log not found for current session yet: {}",
                            replay_path.display()
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                let rendered = render_replay_trace_graph(&replay_path, limit)?;
                emit_command_output(app, rendered);
            }
            Some("verify") => {
                if !replay_path.exists() {
                    emit_command_output(
                        app,
                        format!(
                            "Replay log not found for current session yet: {}",
                            replay_path.display()
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                let (entries, parse_errors, chain_breaks) = replay_trace_integrity(&replay_path)?;
                let ok = parse_errors == 0 && chain_breaks == 0;
                emit_command_output(
                    app,
                    format!(
                        "Replay integrity: {}\nentries: {}\nparse_errors: {}\nchain_breaks: {}\npath: {}",
                        if ok { "PASS" } else { "FAIL" },
                        entries,
                        parse_errors,
                        chain_breaks,
                        replay_path.display()
                    ),
                );
            }
            Some("export") => {
                let limit = args
                    .get(2)
                    .and_then(|raw| raw.trim().parse::<usize>().ok())
                    .unwrap_or(100)
                    .clamp(1, 1000);
                let output_path = args.get(3).map(PathBuf::from).unwrap_or_else(|| {
                    hermes_config::hermes_home()
                        .join("logs")
                        .join("replay")
                        .join("exports")
                        .join(format!("{}-tail.json", app.session_id))
                });
                if !replay_path.exists() {
                    emit_command_output(
                        app,
                        format!(
                            "Replay log not found for current session yet: {}",
                            replay_path.display()
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                let written = export_replay_trace_json(&replay_path, limit, &output_path)?;
                emit_command_output(
                    app,
                    format!(
                        "Replay export written.\nrows: {}\nsource: {}\noutput: {}",
                        written,
                        replay_path.display(),
                        output_path.display()
                    ),
                );
            }
            Some("on") | Some("off") | Some("toggle") => {
                let next = match sub.as_deref().unwrap_or("status") {
                    "on" => true,
                    "off" => false,
                    "toggle" => !replay_enabled_runtime(),
                    _ => replay_enabled_runtime(),
                };
                std::env::set_var("HERMES_REPLAY_ENABLED", if next { "1" } else { "0" });
                emit_command_output(
                    app,
                    format!(
                        "Replay trace mode: {}.\nThis applies to new turns in the current process.",
                        if next { "ON" } else { "OFF" }
                    ),
                );
            }
            Some("help") | Some("--help") | Some("-h") => emit_command_output(
                app,
                "Replay trace controls:\n  /raw trace status              Show enabled state + current log path\n  /raw trace on|off              Enable or disable deterministic replay trace logs\n  /raw trace toggle              Toggle replay trace logs\n  /raw trace tail [N]            Show latest trace events with lineage hashes\n  /raw trace focus <id> [N]      Filter replay rows by trace_id\n  /raw trace graph [N]           Show lineage edges for recent rows\n  /raw trace verify              Validate replay hash-chain integrity\n  /raw trace export [N] [PATH]   Export tail events to JSON\n  /raw trace path                Show trace log file for current session",
            ),
            _ => emit_command_output(
                app,
                "Usage: /raw trace [on|off|toggle|status|tail [N]|focus <trace-id> [N]|graph [N]|verify|export [N] [PATH]|path]",
            ),
        }
        return Ok(CommandResult::Handled);
    }

    let state = app.tool_registry.raw_mode_state();
    let log_dir = app.tool_registry.rtk_log_dir();
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        emit_command_output(
            app,
            format!(
                "RTK raw mode: {}{}\nDual logs: {}\nReplay trace: {}\nUsage: /raw [on|off|toggle|once|status|trace]",
                if state.enabled { "ON" } else { "OFF" },
                if state.once_pending {
                    " (one-shot pending)"
                } else {
                    ""
                },
                log_dir.display(),
                if replay_enabled_runtime() { "ON" } else { "OFF" }
            ),
        );
        return Ok(CommandResult::Handled);
    }

    match args[0].trim().to_ascii_lowercase().as_str() {
        "help" => emit_command_output(
            app,
            "RTK raw controls:\n  /raw status        Show current mode + log path\n  /raw on            Disable output filtering for all tool calls\n  /raw off           Re-enable RTK output filtering\n  /raw toggle        Toggle global raw mode\n  /raw once          Raw pass-through for next tool call only\n  /raw trace ...     Deterministic replay trace controls",
        ),
        "once" => {
            app.tool_registry.set_raw_mode_once();
            emit_command_output(
                app,
                "RTK raw mode armed for next tool call only. It auto-resets after one dispatch.",
            );
        }
        "on" | "off" | "toggle" | "true" | "false" | "yes" | "no" | "1" | "0" => {
            let next = match args[0].trim().to_ascii_lowercase().as_str() {
                "on" | "true" | "yes" | "1" => true,
                "off" | "false" | "no" | "0" => false,
                "toggle" => !state.enabled,
                _ => state.enabled,
            };
            app.tool_registry.set_raw_mode(next);
            std::env::set_var("HERMES_RTK_RAW", if next { "1" } else { "0" });
            emit_command_output(
                app,
                format!(
                    "RTK raw mode: {} (dual logs: {})",
                    if next { "ON" } else { "OFF" },
                    log_dir.display()
                ),
            );
        }
        _ => emit_command_output(app, "Usage: /raw [on|off|toggle|once|status|trace]"),
    }
    Ok(CommandResult::Handled)
}

#[derive(Debug, Clone, Copy)]
struct PolicyProfile {
    name: &'static str,
    preset: &'static str,
    mode: &'static str,
    sandbox: &'static str,
    skills_tier: &'static str,
    description: &'static str,
}

const POLICY_PROFILES: &[PolicyProfile] = &[
    PolicyProfile {
        name: "strict",
        preset: "strict",
        mode: "enforce",
        sandbox: "strict",
        skills_tier: "trusted",
        description: "maximum guardrails; strongest deny + sandbox posture",
    },
    PolicyProfile {
        name: "standard",
        preset: "balanced",
        mode: "enforce",
        sandbox: "balanced",
        skills_tier: "balanced",
        description: "default production posture with balanced safety and throughput",
    },
    PolicyProfile {
        name: "dev",
        preset: "dev",
        mode: "audit",
        sandbox: "dev",
        skills_tier: "open",
        description: "development posture with audit/simulate-friendly behavior",
    },
];

fn resolve_policy_profile(input: &str) -> Option<PolicyProfile> {
    let token = input.trim().to_ascii_lowercase();
    POLICY_PROFILES.iter().copied().find(|profile| {
        profile.name == token
            || (token == "balanced" && profile.name == "standard")
            || (token == "prod" && profile.name == "standard")
    })
}

fn current_policy_profile_name() -> &'static str {
    let preset = std::env::var("HERMES_TOOL_POLICY_PRESET")
        .ok()
        .unwrap_or_else(|| "off".to_string())
        .trim()
        .to_ascii_lowercase();
    match preset.as_str() {
        "strict" => "strict",
        "dev" => "dev",
        _ => "standard",
    }
}

fn apply_policy_profile(app: &mut App, profile: PolicyProfile) {
    std::env::set_var("HERMES_TOOL_POLICY_PRESET", profile.preset);
    std::env::set_var("HERMES_TOOL_POLICY_MODE", profile.mode);
    std::env::set_var("HERMES_EXECUTION_SANDBOX_PROFILE", profile.sandbox);
    std::env::set_var("HERMES_SKILLS_EXECUTION_TIER", profile.skills_tier);
    app.tool_registry.set_policy(ToolPolicyEngine::from_env());
}

fn handle_policy_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        let counters = app.tool_registry.policy_counters();
        emit_command_output(
            app,
            format!(
                "Policy profile: {}\nPreset: {}\nMode: {}\nSandbox: {}\nSkills tier: {}\nCounters: allow={} deny={} audit_only={} simulate={} would_block={}\n\nUse `/policy list` or `/policy strict|standard|dev`.",
                current_policy_profile_name(),
                std::env::var("HERMES_TOOL_POLICY_PRESET").unwrap_or_else(|_| "balanced".into()),
                std::env::var("HERMES_TOOL_POLICY_MODE").unwrap_or_else(|_| "enforce".into()),
                std::env::var("HERMES_EXECUTION_SANDBOX_PROFILE")
                    .unwrap_or_else(|_| "balanced".into()),
                std::env::var("HERMES_SKILLS_EXECUTION_TIER")
                    .unwrap_or_else(|_| "balanced".into()),
                counters.allow,
                counters.deny,
                counters.audit_only,
                counters.simulate,
                counters.would_block
            ),
        );
        return Ok(CommandResult::Handled);
    }

    if args[0].eq_ignore_ascii_case("list") {
        let mut out = String::from("Policy profiles:\n");
        for profile in POLICY_PROFILES {
            let marker = if current_policy_profile_name() == profile.name {
                "*"
            } else {
                " "
            };
            let _ = writeln!(
                out,
                "{} {:<9} preset={} mode={} sandbox={} skills_tier={} — {}",
                marker,
                profile.name,
                profile.preset,
                profile.mode,
                profile.sandbox,
                profile.skills_tier,
                profile.description
            );
        }
        out.push_str("\nSelect with `/policy strict`, `/policy standard`, or `/policy dev`.");
        emit_command_output(app, out.trim_end());
        return Ok(CommandResult::Handled);
    }

    if let Some(profile) = resolve_policy_profile(args[0]) {
        apply_policy_profile(app, profile);
        emit_command_output(
            app,
            format!(
                "Policy profile switched to `{}`.\nPreset={} Mode={} Sandbox={} SkillsTier={}",
                profile.name, profile.preset, profile.mode, profile.sandbox, profile.skills_tier
            ),
        );
        return Ok(CommandResult::Handled);
    }

    emit_command_output(app, "Usage: /policy [status|list|strict|standard|dev]");
    Ok(CommandResult::Handled)
}

fn handle_history_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let transcript = app.transcript_messages();
    if transcript.is_empty() {
        emit_command_output(app, "No conversation history yet.");
        return Ok(CommandResult::Handled);
    }
    let mut out = String::from("Recent conversation history:\n");
    for (idx, msg) in transcript.iter().enumerate().rev().take(12).rev() {
        let role = match msg.role {
            hermes_core::MessageRole::User => "USER",
            hermes_core::MessageRole::Assistant => "HERMES",
            hermes_core::MessageRole::System => "SYSTEM",
            hermes_core::MessageRole::Tool => "TOOL",
        };
        let preview = msg
            .content
            .as_deref()
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .trim();
        let clipped = if preview.chars().count() > 96 {
            let mut s: String = preview.chars().take(95).collect();
            s.push('…');
            s
        } else {
            preview.to_string()
        };
        let _ = writeln!(out, "{:>3}. {:<7} {}", idx + 1, role, clipped);
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn truncate_chars(input: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    if input.chars().count() <= max_len {
        return input.to_string();
    }
    let mut out: String = input.chars().take(max_len.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn handle_recap_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let requested = args
        .first()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(24)
        .clamp(1, 200);
    let transcript = app.transcript_messages();
    if transcript.is_empty() {
        emit_command_output(app, "No activity yet. Start with a prompt first.");
        return Ok(CommandResult::Handled);
    }

    let start = transcript.len().saturating_sub(requested);
    let window = &transcript[start..];
    let mut user_msgs = 0usize;
    let mut assistant_msgs = 0usize;
    let mut tool_msgs = 0usize;
    let mut system_msgs = 0usize;
    let mut tool_call_count = 0usize;
    let mut char_count = 0usize;

    for msg in window {
        match msg.role {
            hermes_core::MessageRole::User => user_msgs += 1,
            hermes_core::MessageRole::Assistant => assistant_msgs += 1,
            hermes_core::MessageRole::Tool => tool_msgs += 1,
            hermes_core::MessageRole::System => system_msgs += 1,
        }
        tool_call_count += msg.tool_calls.as_ref().map(|c| c.len()).unwrap_or(0);
        char_count += msg.content.as_deref().map(str::len).unwrap_or(0);
    }

    let latest_user = window
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::User))
        .and_then(|m| m.content.as_deref())
        .map(|c| truncate_chars(c.trim(), 120))
        .unwrap_or_else(|| "(none)".to_string());
    let latest_assistant = window
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::Assistant))
        .and_then(|m| m.content.as_deref())
        .map(|c| truncate_chars(c.trim(), 120))
        .unwrap_or_else(|| "(none)".to_string());

    let approx_tokens = (char_count / 4).max(1);
    emit_command_output(
        app,
        format!(
            "Session recap (last {} messages)\n\
             model: {}\n\
             roles: user={} assistant={} tool={} system={}\n\
             tool_calls: {}\n\
             approx_tokens: {}\n\
             latest_user: {}\n\
             latest_hermes: {}",
            window.len(),
            app.current_model,
            user_msgs,
            assistant_msgs,
            tool_msgs,
            system_msgs,
            tool_call_count,
            approx_tokens,
            latest_user,
            latest_assistant
        ),
    );
    Ok(CommandResult::Handled)
}

fn handle_context_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match action.as_str() {
        "status" => {
            let transcript = app.transcript_messages();
            let total_chars: usize = transcript
                .iter()
                .map(|m| m.content.as_deref().map(str::len).unwrap_or(0))
                .sum();
            let approx_tokens = (total_chars / 4).max(1);
            let context_files = if app.config.agent.skip_context_files {
                "disabled"
            } else {
                "enabled"
            };
            emit_command_output(
                app,
                format!(
                    "Context status\n\
                     model: {}\n\
                     transcript_messages: {}\n\
                     approx_tokens: {}\n\
                     context_files: {}\n\
                     hint: run `/context breakdown` for per-message footprint or `/context compress` for immediate compaction",
                    app.current_model,
                    transcript.len(),
                    approx_tokens,
                    context_files
                ),
            );
        }
        "breakdown" => {
            let transcript = app.transcript_messages();
            if transcript.is_empty() {
                emit_command_output(app, "No transcript yet.");
                return Ok(CommandResult::Handled);
            }
            let mut out = String::from("Context breakdown (recent)\n");
            for (idx, msg) in transcript.iter().enumerate().rev().take(20).rev() {
                let role = match msg.role {
                    hermes_core::MessageRole::User => "USER",
                    hermes_core::MessageRole::Assistant => "HERMES",
                    hermes_core::MessageRole::Tool => "TOOL",
                    hermes_core::MessageRole::System => "SYSTEM",
                };
                let chars = msg.content.as_deref().map(str::len).unwrap_or(0);
                let est_tokens = (chars / 4).max(1);
                let preview = msg
                    .content
                    .as_deref()
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim();
                let _ = writeln!(
                    out,
                    "{:>3}. {:<7} chars={:<5} tok≈{:<5} {}",
                    idx + 1,
                    role,
                    chars,
                    est_tokens,
                    truncate_chars(preview, 70)
                );
            }
            emit_command_output(app, out.trim_end());
        }
        "compress" | "compact" => {
            return handle_compress_command(app, &[]);
        }
        _ => {
            emit_command_output(
                app,
                "Usage: /context [status|breakdown|compress]\nAlias: /summary -> /recap",
            );
        }
    }
    Ok(CommandResult::Handled)
}

async fn handle_provider_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let providers = provider_slugs_for_config(&app.config);
    if providers.is_empty() {
        emit_command_output(app, "No providers registered.");
        return Ok(CommandResult::Handled);
    }
    let entries = provider_catalog_entries_for_config(&app.config).await;
    if entries.is_empty() {
        emit_command_output(
            app,
            format!(
                "Configured providers: {}\nCurrent model: {}",
                providers.join(", "),
                app.current_model
            ),
        );
        return Ok(CommandResult::Handled);
    }
    let mut out = format!("Current model: {}\n\nProviders:\n", app.current_model);
    for entry in entries {
        let preview = entry.models.join(", ");
        let suffix = if entry.total_models > entry.models.len() {
            format!(" (+{} more)", entry.total_models - entry.models.len())
        } else {
            String::new()
        };
        let _ = writeln!(out, "  - {:<14} {}{}", entry.provider, preview, suffix);
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

async fn handle_auth_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match action.as_str() {
        "status" => {
            let provider = app.current_runtime_provider();
            let credential_present = crate::app::provider_api_key_from_env(&provider).is_some();
            let state = if credential_present {
                "present"
            } else {
                "missing"
            };
            let gate_line = oauth_runtime_gate_for_provider(&provider)
                .map(|(ok, detail)| {
                    format!(
                        "oauth_runtime_gate: {} ({})",
                        if ok { "PASS" } else { "FAIL" },
                        detail
                    )
                })
                .unwrap_or_else(|| "oauth_runtime_gate: n/a".to_string());
            emit_command_output(
                app,
                format!(
                    "Auth status\nprovider: {}\nmodel: {}\ncredential: {}\n{}\nnext: `/auth verify` (passive refresh check) or `/auth refresh` (forced token refresh)",
                    provider, app.current_model, state, gate_line
                ),
            );
        }
        "verify" => {
            let provider = app.current_runtime_provider();
            if let Some((ok, detail)) = oauth_runtime_gate_for_provider(&provider) {
                if !ok {
                    emit_command_output(
                        app,
                        format!(
                            "Auth verify blocked by OAuth runtime gate for `{}`.\n{}\nUpgrade runtime and retry.",
                            provider, detail
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
            }
            let summary = app.verify_runtime_auth(false).await?;
            emit_command_output(
                app,
                format!(
                    "{}\nnext: if provider rejects again, run `/auth refresh` then retry.",
                    summary
                ),
            );
        }
        "refresh" | "force" => {
            let provider = app.current_runtime_provider();
            if let Some((ok, detail)) = oauth_runtime_gate_for_provider(&provider) {
                if !ok {
                    emit_command_output(
                        app,
                        format!(
                            "Auth refresh blocked by OAuth runtime gate for `{}`.\n{}\nUpgrade runtime and retry.",
                            provider, detail
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
            }
            let summary = app.verify_runtime_auth(true).await?;
            emit_command_output(
                app,
                format!(
                    "{}\nforced refresh complete; retry your request now.",
                    summary
                ),
            );
        }
        _ => emit_command_output(
            app,
            "Usage: /auth [status|verify|refresh]\n- status: show active provider auth state\n- verify: passive credential hydration + verification\n- refresh: force OAuth/session token refresh",
        ),
    }
    Ok(CommandResult::Handled)
}

fn handle_telemetry_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let provider = app
        .current_model
        .split_once(':')
        .map(|(p, _)| p.to_string())
        .unwrap_or_else(|| "openai".to_string());
    let provider_health =
        hermes_tools::tools::telemetry_snapshot::provider_health_snapshot(&provider);
    let session = app.session_info();
    let mut out = String::new();
    let _ = writeln!(out, "Telemetry snapshot");
    let _ = writeln!(out, "session: {}", session.session_id);
    let _ = writeln!(out, "model: {}", app.current_model);
    let _ = writeln!(out, "messages: {}", session.message_count);
    let _ = writeln!(out, "provider health: {}", provider_health);

    if let Some(repo_root) = hermes_tools::repo::detect_repo_root_from_cwd() {
        let _ = writeln!(
            out,
            "{}",
            hermes_tools::tools::telemetry_snapshot::telemetry_gate_line(&repo_root)
        );
    }

    if action == "lane" {
        let _ = writeln!(out, "lane hints:");
        for hint in hermes_tools::tools::telemetry_snapshot::lane_hints() {
            let _ = writeln!(out, "- {}", hint);
        }
    } else if action != "status" {
        emit_command_output(
            app,
            "Usage: /telemetry [status|lane]\n- status: session/provider + gate snapshots\n- lane: status plus TUI activity-lane controls",
        );
        return Ok(CommandResult::Handled);
    }

    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_runbook_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args.first().copied().unwrap_or("list").to_ascii_lowercase();
    if action == "list" || action == "status" {
        emit_command_output(app, hermes_tools::tools::runbook::render_runbook_list());
        return Ok(CommandResult::Handled);
    }
    if action == "show" {
        let Some(name) = args.get(1).map(|v| v.to_ascii_lowercase()) else {
            emit_command_output(app, "Usage: /runbook show <name>");
            return Ok(CommandResult::Handled);
        };
        let Some(runbook) = hermes_tools::tools::runbook::find_runbook(&name) else {
            emit_command_output(
                app,
                format!(
                    "Unknown runbook `{}`. Use `/runbook list` for available entries.",
                    name
                ),
            );
            return Ok(CommandResult::Handled);
        };
        emit_command_output(app, hermes_tools::tools::runbook::render_runbook(runbook));
        return Ok(CommandResult::Handled);
    }
    emit_command_output(app, "Usage: /runbook [list|show <name>]");
    Ok(CommandResult::Handled)
}

fn handle_profile_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let home = hermes_config::hermes_home();
    let selected = app.config.profile.current.as_deref().unwrap_or("default");
    let mut out = String::new();
    let _ = writeln!(out, "Active profile: {}", selected);
    let _ = writeln!(out, "Hermes home: {}", home.display());
    let _ = writeln!(out, "Session id: {}", app.session_id);
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_runtime_ui_mode_command(
    app: &mut App,
    cmd: &str,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let msg = match cmd {
        "/skin" => {
            let first = args.first().copied().unwrap_or("status");
            if first.eq_ignore_ascii_case("list") {
                let active = std::env::var("HERMES_THEME").unwrap_or_else(|_| "ultra-neon".to_string());
                let active_canonical = canonical_skin_name(&active).unwrap_or("ultra-neon");
                let mut out = String::new();
                let _ = writeln!(out, "Built-in skins (active: {}):", active_canonical);
                for (name, detail) in BUILTIN_SKINS {
                    let marker = if *name == active_canonical { "✓" } else { " " };
                    let _ = writeln!(out, "  {} {:<30} {}", marker, name, detail);
                }
                let _ = writeln!(
                    out,
                    "\nUse `/skin <name>` or `/skin set <name>` to switch immediately."
                );
                out.trim_end().to_string()
            } else if first.eq_ignore_ascii_case("status") || first.eq_ignore_ascii_case("show") {
                let active = std::env::var("HERMES_THEME").unwrap_or_else(|_| "ultra-neon".to_string());
                let active_canonical = canonical_skin_name(&active).unwrap_or("ultra-neon");
                format!(
                    "Current skin: {}\nUse `/skin list` to browse options.\nUse `/skin <name>` to switch now.",
                    active_canonical
                )
            } else {
                let requested = if first.eq_ignore_ascii_case("set") {
                    args.get(1).copied().unwrap_or("")
                } else {
                    first
                };
                if requested.trim().is_empty() {
                    "Usage: `/skin list` or `/skin <name>`".to_string()
                } else if let Some(canonical) = canonical_skin_name(requested) {
                    std::env::set_var("HERMES_THEME", canonical);
                    app.request_theme_change(canonical);
                    format!(
                        "Skin switched to `{}`.\nApplied in this TUI session and exported as HERMES_THEME for child processes.",
                        canonical
                    )
                } else {
                    format!(
                        "Unknown skin `{}`. Use `/skin list` for built-ins.",
                        requested
                    )
                }
            }
        }
        "/fast" => format!(
            "Fast mode compatibility command received (`{}`).\nCurrent model: {}\nTip: switch to a lower-latency model via `/model`.",
            args.first().copied().unwrap_or("status"),
            app.current_model
        ),
        "/voice" => "Voice mode uses provider/platform capabilities; no separate TUI voice engine is active in this session.".to_string(),
        _ => "Unsupported runtime UI mode command.".to_string(),
    };
    emit_command_output(app, msg);
    Ok(CommandResult::Handled)
}

fn render_pet_status(settings: &PetSettings) -> String {
    format!(
        "Pet status:\n  - enabled: {}\n  - species: {}\n  - mood: {}\n  - dock: {}\n  - speed_ms: {}\n\nUse `/pet on`, `/pet off`, `/pet toggle`, `/pet set <species>`, `/pet mood <mood>`, `/pet dock <left|right>`, `/pet speed <ms>`, `/pet list`.",
        if settings.enabled { "ON" } else { "OFF" },
        settings.species,
        settings.mood,
        settings.dock.as_str(),
        settings.tick_ms
    )
}

fn parse_pet_species(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    PetSettings::species_catalog()
        .iter()
        .find(|candidate| **candidate == normalized)
        .map(|candidate| (*candidate).to_string())
}

fn parse_pet_mood(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    PetSettings::mood_catalog()
        .iter()
        .find(|candidate| **candidate == normalized)
        .map(|candidate| (*candidate).to_string())
}

fn parse_pet_dock(value: &str) -> Option<PetDock> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "left" => Some(PetDock::Left),
        "right" => Some(PetDock::Right),
        _ => None,
    }
}

fn handle_pet_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args.first().copied().unwrap_or("status");
    let mut settings = app.pet_settings().clone();

    match action.to_ascii_lowercase().as_str() {
        "status" => {
            emit_command_output(app, render_pet_status(&settings));
        }
        "list" => {
            emit_command_output(
                app,
                format!(
                    "Available pets:\n  - species: {}\n  - moods: {}\n  - dock: left, right",
                    PetSettings::species_catalog().join(", "),
                    PetSettings::mood_catalog().join(", ")
                ),
            );
        }
        "on" | "off" | "toggle" | "wake" | "sleep" | "tuck" => {
            let action_lc = action.to_ascii_lowercase();
            let normalized_toggle = match action_lc.as_str() {
                "wake" => Some("on"),
                "sleep" | "tuck" => Some("off"),
                other => Some(other),
            };
            match parse_toggle_arg(normalized_toggle, settings.enabled) {
                Ok(enabled) => {
                    settings.enabled = enabled;
                    app.set_pet_settings(settings.clone())?;
                    emit_command_output(
                        app,
                        format!(
                            "Pet {}.\n{}",
                            if settings.enabled { "enabled" } else { "hidden" },
                            render_pet_status(&settings)
                        ),
                    );
                }
                Err(_) => emit_command_output(
                    app,
                    "Usage: /pet [status|on|off|toggle|wake|tuck|list|set <species>|mood <mood>|dock <left|right>|speed <ms>]",
                ),
            }
        }
        "set" | "species" => {
            let Some(raw) = args.get(1).copied() else {
                emit_command_output(
                    app,
                    format!(
                        "Usage: /pet set <species>\nAvailable species: {}",
                        PetSettings::species_catalog().join(", ")
                    ),
                );
                return Ok(CommandResult::Handled);
            };
            if let Some(species) = parse_pet_species(raw) {
                settings.species = species;
                app.set_pet_settings(settings.clone())?;
                emit_command_output(app, render_pet_status(&settings));
            } else {
                emit_command_output(
                    app,
                    format!(
                        "Unknown species '{}'. Available: {}",
                        raw,
                        PetSettings::species_catalog().join(", ")
                    ),
                );
            }
        }
        "mood" => {
            let Some(raw) = args.get(1).copied() else {
                emit_command_output(
                    app,
                    format!(
                        "Usage: /pet mood <mood>\nAvailable moods: {}",
                        PetSettings::mood_catalog().join(", ")
                    ),
                );
                return Ok(CommandResult::Handled);
            };
            if let Some(mood) = parse_pet_mood(raw) {
                settings.mood = mood;
                app.set_pet_settings(settings.clone())?;
                emit_command_output(app, render_pet_status(&settings));
            } else {
                emit_command_output(
                    app,
                    format!(
                        "Unknown mood '{}'. Available: {}",
                        raw,
                        PetSettings::mood_catalog().join(", ")
                    ),
                );
            }
        }
        "dock" => {
            let Some(raw) = args.get(1).copied() else {
                emit_command_output(app, "Usage: /pet dock <left|right>");
                return Ok(CommandResult::Handled);
            };
            if let Some(dock) = parse_pet_dock(raw) {
                settings.dock = dock;
                app.set_pet_settings(settings.clone())?;
                emit_command_output(app, render_pet_status(&settings));
            } else {
                emit_command_output(app, "Usage: /pet dock <left|right>");
            }
        }
        "speed" => {
            let Some(raw) = args.get(1).copied() else {
                emit_command_output(app, "Usage: /pet speed <ms>");
                return Ok(CommandResult::Handled);
            };
            match raw.trim().parse::<u64>() {
                Ok(ms) => {
                    settings.tick_ms = ms;
                    app.set_pet_settings(settings.clone())?;
                    emit_command_output(app, render_pet_status(&settings));
                }
                Err(_) => emit_command_output(app, "Usage: /pet speed <ms>"),
            }
        }
        _ => emit_command_output(
            app,
            "Usage: /pet [status|on|off|toggle|wake|tuck|list|set <species>|mood <mood>|dock <left|right>|speed <ms>]",
        ),
    }

    Ok(CommandResult::Handled)
}

fn handle_toolsets_command(app: &mut App) -> Result<CommandResult, AgentError> {
    if app.config.platform_toolsets.is_empty() {
        emit_command_output(app, "No explicit platform toolsets configured.");
        return Ok(CommandResult::Handled);
    }
    let mut rows: Vec<_> = app.config.platform_toolsets.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = String::from("Configured toolsets by platform:\n");
    for (platform, toolsets) in rows {
        let _ = writeln!(out, "  - {:<10} {}", platform, toolsets.join(", "));
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

#[derive(Debug, Deserialize)]
struct SkillBundleManifest {
    name: Option<String>,
    description: Option<String>,
    skills: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct SkillBundleSummary {
    slug: String,
    description: String,
    skills: Vec<String>,
}

fn slugify_skill_bundle_name(name: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in name.trim().to_ascii_lowercase().chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            Some(ch)
        } else if matches!(ch, '-' | '_' | ' ') {
            Some('-')
        } else {
            None
        };
        if let Some(ch) = normalized {
            if ch == '-' {
                if !last_was_dash && !slug.is_empty() {
                    slug.push('-');
                    last_was_dash = true;
                }
            } else {
                slug.push(ch);
                last_was_dash = false;
            }
        }
    }
    slug.trim_matches('-').to_string()
}

fn list_skill_bundles(root: &Path) -> Vec<SkillBundleSummary> {
    let dir = root.join("skill-bundles");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut bundles = Vec::new();
    let mut seen = HashSet::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|v| v.to_str()) else {
            continue;
        };
        if !matches!(ext, "yaml" | "yml") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(manifest) = serde_yaml::from_str::<SkillBundleManifest>(&raw) else {
            continue;
        };
        let name = manifest
            .name
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|v| v.to_str())
                    .unwrap_or("bundle")
                    .to_string()
            });
        let slug = slugify_skill_bundle_name(&name);
        if slug.is_empty() || !seen.insert(slug.clone()) {
            continue;
        }
        let skills: Vec<String> = manifest
            .skills
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if skills.is_empty() {
            continue;
        }
        let description = manifest
            .description
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Load {} skills as a bundle", skills.len()));
        bundles.push(SkillBundleSummary {
            slug,
            description,
            skills,
        });
    }
    bundles.sort_by(|a, b| a.slug.cmp(&b.slug));
    bundles
}

fn handle_bundles_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let bundles = list_skill_bundles(&app.state_root);
    if bundles.is_empty() {
        emit_command_output(
            app,
            format!(
                "No skill bundles installed.\nCreate one with `hermes bundles create <name> --skill <s1> --skill <s2>`.\nDirectory: {}",
                app.state_root.join("skill-bundles").display()
            ),
        );
        return Ok(CommandResult::Handled);
    }

    let mut out = format!("Skill Bundles ({} installed):\n", bundles.len());
    for bundle in bundles {
        let _ = writeln!(
            out,
            "  - /{} -- {} ({} skills)",
            bundle.slug,
            bundle.description,
            bundle.skills.len()
        );
        for skill in bundle.skills {
            let _ = writeln!(out, "      - {}", skill);
        }
    }
    out.push_str("\nInvoke a bundle with `/<slug>` to load all its skills.");
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_plugins_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let rows = discover_plugin_surface(true);
    if rows.is_empty() {
        let plugins_dir = hermes_config::hermes_home().join("plugins");
        emit_command_output(
            app,
            format!(
                "No plugin bundles discovered.\nUser plugin dir: {}\nInstall with `hermes plugins install <owner/repo>`.",
                plugins_dir.display()
            ),
        );
    } else {
        emit_command_output(
            app,
            format!(
                "Plugin surface ({} entries):\n{}",
                rows.len(),
                render_plugin_surface_table(&rows)
            ),
        );
    }
    Ok(CommandResult::Handled)
}

fn handle_disk_cleanup_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let cleaner = hermes_tools::tools::disk_cleanup::DiskCleanup::new(app.state_root.clone());
    let output = hermes_tools::tools::disk_cleanup::handle_slash_args(&cleaner, args);
    emit_command_output(app, output);
    Ok(CommandResult::Handled)
}

fn render_mcp_runtime_status(
    yaml_servers: &[hermes_config::McpServerEntry],
    json_config: Option<&crate::mcp_config::McpConfig>,
    json_path: &Path,
) -> String {
    let json_servers = json_config.map(|cfg| cfg.servers.as_slice()).unwrap_or(&[]);
    let mut names = HashSet::new();
    for server in yaml_servers {
        names.insert(server.name.clone());
    }
    for server in json_servers {
        names.insert(server.name.clone());
    }

    if names.is_empty() {
        return format!(
            "No MCP servers configured.\n  config.yaml entries: 0\n  mcp_servers.json: {} ({})\nAdd one with `hermes mcp add <name> --url <url>` or `hermes mcp add <name> --command <cmd>`.",
            if json_path.exists() { "present" } else { "missing" },
            json_path.display()
        );
    }

    let mut out = String::new();
    let _ = writeln!(out, "MCP runtime status");
    let _ = writeln!(out, "  config.yaml entries: {}", yaml_servers.len());
    let _ = writeln!(
        out,
        "  mcp_servers.json entries: {} ({})",
        json_servers.len(),
        json_path.display()
    );

    if let Some(config) = json_config {
        for warning in config.warnings() {
            let _ = writeln!(out, "  warning: {warning}");
        }
    }

    let yaml_names: HashSet<_> = yaml_servers
        .iter()
        .map(|server| server.name.as_str())
        .collect();
    let json_names: HashSet<_> = json_servers
        .iter()
        .map(|server| server.name.as_str())
        .collect();
    let mut sorted: Vec<_> = names.into_iter().collect();
    sorted.sort();

    out.push_str("Configured MCP servers:\n");
    for name in sorted {
        if let Some(server) = yaml_servers.iter().find(|server| server.name == name) {
            let endpoint = server
                .url
                .as_deref()
                .filter(|u| !u.is_empty())
                .or(server.command.as_deref())
                .unwrap_or("<stdio>");
            let _ = writeln!(
                out,
                "  - {:<18} {}  [source:config.yaml; parallel_tool_calls:{}; keepalive:{}]",
                server.name,
                endpoint,
                if server.supports_parallel_tool_calls {
                    "on"
                } else {
                    "off"
                },
                server
                    .keepalive_interval
                    .map(|secs| format!("{secs}s"))
                    .unwrap_or_else(|| "default".to_string())
            );
        }
        if let Some(server) = json_servers.iter().find(|server| server.name == name) {
            let _ = writeln!(
                out,
                "  - {:<18} {}  [source:mcp_servers.json; {}; enabled:{}; parallel_tool_calls:{}; keepalive:{}]",
                server.name,
                server.transport_display(),
                server.transport_kind().as_str(),
                if server.enabled { "on" } else { "off" },
                if server.supports_parallel_tool_calls {
                    "on"
                } else {
                    "off"
                },
                server
                    .keepalive_interval
                    .map(|secs| format!("{secs}s"))
                    .unwrap_or_else(|| "default".to_string())
            );
        }
    }

    let mut yaml_only: Vec<_> = yaml_names.difference(&json_names).copied().collect();
    let mut json_only: Vec<_> = json_names.difference(&yaml_names).copied().collect();
    yaml_only.sort();
    json_only.sort();
    if !yaml_only.is_empty() || !json_only.is_empty() {
        let _ = writeln!(
            out,
            "Drift: config_only=[{}] json_only=[{}]",
            yaml_only.join(","),
            json_only.join(",")
        );
    }

    out.trim_end().to_string()
}

fn handle_mcp_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let mcp_config_path = app.state_root.join("mcp_servers.json");
    let json_config = load_mcp_config_if_exists(&mcp_config_path)?;
    let out = render_mcp_runtime_status(
        &app.config.mcp_servers,
        json_config.as_ref(),
        &mcp_config_path,
    );
    emit_command_output(app, out);
    Ok(CommandResult::Handled)
}

fn render_memory_backend_status(hermes_home: &Path) -> String {
    let memories_dir = hermes_home.join("memories");
    let memory_md = memories_dir.join("MEMORY.md");
    let user_md = memories_dir.join("USER.md");
    let legacy_memory_db = hermes_home.join("memory.db");
    let disabled_marker = hermes_home.join(".memory_disabled");
    let mut out = String::new();

    if disabled_marker.exists() {
        out.push_str("Memory provider: disabled\n");
        let _ = writeln!(out, "  Marker: {}", disabled_marker.display());
        out.push_str("Run `hermes memory setup` to re-enable.");
        return out;
    }

    if memory_md.exists() || user_md.exists() {
        let mem_size = std::fs::metadata(&memory_md).map(|m| m.len()).unwrap_or(0);
        let user_size = std::fs::metadata(&user_md).map(|m| m.len()).unwrap_or(0);
        out.push_str("Memory provider: files (MEMORY.md + USER.md)\n");
        let _ = writeln!(out, "  Directory: {}", memories_dir.display());
        let _ = writeln!(
            out,
            "  MEMORY.md: {} ({:.1} KB)",
            memory_md.display(),
            mem_size as f64 / 1024.0
        );
        let _ = writeln!(
            out,
            "  USER.md:   {} ({:.1} KB)",
            user_md.display(),
            user_size as f64 / 1024.0
        );
        if legacy_memory_db.exists() {
            let _ = writeln!(
                out,
                "  Legacy file detected (unused by current memory backend): {}",
                legacy_memory_db.display()
            );
        }
        return out.trim_end().to_string();
    }

    if legacy_memory_db.exists() {
        let size = std::fs::metadata(&legacy_memory_db)
            .map(|m| m.len())
            .unwrap_or(0);
        out.push_str("Memory provider: legacy sqlite artifact only\n");
        let _ = writeln!(out, "  File: {}", legacy_memory_db.display());
        let _ = writeln!(out, "  Size: {} KB", size / 1024);
        out.push_str("Run `hermes memory setup` to initialize the current file backend.");
        return out;
    }

    out.push_str("Memory provider: not configured\n");
    out.push_str("Run `hermes memory setup` to initialize.");
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalSkillSummary {
    name: String,
    title: String,
    relative_dir: String,
    skill_md: PathBuf,
}

fn collect_local_skill_summaries(skills_dir: &Path) -> Vec<LocalSkillSummary> {
    let mut summaries = Vec::new();
    collect_local_skill_summaries_rec(skills_dir, skills_dir, &mut summaries);
    summaries.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.relative_dir.cmp(&b.relative_dir))
    });
    summaries
}

fn collect_local_skill_summaries_rec(root: &Path, dir: &Path, out: &mut Vec<LocalSkillSummary>) {
    let skill_md = dir.join("SKILL.md");
    if skill_md.exists() {
        if let Some(summary) = read_local_skill_summary(root, &skill_md) {
            out.push(summary);
        }
        return;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        collect_local_skill_summaries_rec(root, &entry.path(), out);
    }
}

fn read_local_skill_summary(root: &Path, skill_md: &Path) -> Option<LocalSkillSummary> {
    let content = std::fs::read_to_string(skill_md).ok()?;
    let parent = skill_md.parent()?;
    let fallback_name = parent.file_name()?.to_string_lossy().to_string();
    let relative_dir = parent
        .strip_prefix(root)
        .ok()
        .map(path_to_forward_slashes)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback_name.clone());
    let name = frontmatter_value(&content, "name").unwrap_or(fallback_name);
    let title = frontmatter_value(&content, "description")
        .or_else(|| {
            content
                .lines()
                .find(|line| line.starts_with('#'))
                .map(|line| line.trim_start_matches('#').trim().to_string())
        })
        .filter(|line| !line.trim().is_empty())
        .unwrap_or_else(|| "(no description)".to_string());

    Some(LocalSkillSummary {
        name,
        title,
        relative_dir,
        skill_md: skill_md.to_path_buf(),
    })
}

fn frontmatter_value(content: &str, key: &str) -> Option<String> {
    let mut lines = content.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }

    let needle = format!("{}:", key);
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        let Some(value) = trimmed.strip_prefix(&needle) else {
            continue;
        };
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim()
            .to_string();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

fn find_local_skill_markdown(skills_dir: &Path, query: &str) -> Option<PathBuf> {
    let query = query.trim().trim_start_matches('/');
    if query.is_empty() {
        return None;
    }

    collect_local_skill_summaries(skills_dir)
        .into_iter()
        .find(|summary| local_skill_summary_matches(summary, query))
        .map(|summary| summary.skill_md)
}

fn local_skill_summary_matches(summary: &LocalSkillSummary, query: &str) -> bool {
    summary.name == query
        || summary.relative_dir == query
        || summary
            .skill_md
            .parent()
            .and_then(|dir| dir.file_name())
            .map(|name| name.to_string_lossy() == query)
            .unwrap_or(false)
        || summary.name.eq_ignore_ascii_case(query)
        || summary.relative_dir.eq_ignore_ascii_case(query)
}

fn format_skill_display_name(summary: &LocalSkillSummary) -> String {
    if summary.relative_dir == summary.name {
        summary.name.clone()
    } else {
        format!("{} ({})", summary.name, summary.relative_dir)
    }
}

fn path_to_forward_slashes(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn handle_memory_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args.first().copied().unwrap_or("pending");
    match action {
        "status" => {
            emit_command_output(app, render_memory_backend_status(&app.state_root));
        }
        "pending" => {
            let mut out = String::from("memory.write_approval = off\n\nNo pending memory writes.");
            out.push_str("\n\n");
            out.push_str(&render_memory_backend_status(&app.state_root));
            emit_command_output(app, out.trim_end());
        }
        "setup" | "off" | "reset" => {
            emit_command_output(
                app,
                "Use `hermes memory status|setup|off|reset` outside the chat session for memory backend changes. Slash `/memory` is read-only: `/memory [status|pending]`.",
            );
        }
        _ => {
            emit_command_output(app, "Usage: /memory [status|pending]");
        }
    }
    Ok(CommandResult::Handled)
}

fn handle_reload_command(app: &mut App, cmd: &str) -> Result<CommandResult, AgentError> {
    if cmd == "/reload-mcp" {
        let refresh = app.refresh_agent_tool_snapshot();
        let mut out = format!(
            "MCP reload complete: refreshed agent tool snapshot ({} -> {} tools).",
            refresh.before_count, refresh.after_count
        );
        if refresh.changed() {
            if !refresh.added.is_empty() {
                let _ = write!(out, "\nAdded: {}", refresh.added.join(", "));
            }
            if !refresh.removed.is_empty() {
                let _ = write!(out, "\nRemoved: {}", refresh.removed.join(", "));
            }
        } else {
            out.push_str("\nNo tool changes detected.");
        }
        out.push_str("\nConnector renegotiation still requires a process restart.");
        emit_command_output(app, out);
    } else if cmd == "/reload-skills" {
        let config = SkillCommandResolverConfig {
            enabled: app.config.skills.enabled.clone(),
            disabled: app.config.skills.disabled.clone(),
            ..SkillCommandResolverConfig::default()
        };
        let snapshot = installed_skill_slash_command_snapshot(&config);
        app.queue_next_turn_system_note(build_skill_reload_system_note(&snapshot));
        emit_command_output(app, render_skill_slash_command_snapshot(&snapshot));
    } else {
        hermes_config::loader::load_dotenv();
        match hermes_config::load_config(app.state_root.to_str()) {
            Ok(cfg) => {
                app.config = Arc::new(cfg);
                emit_command_output(
                    app,
                    "Reload complete: env + config rehydrated for this session.",
                );
            }
            Err(err) => {
                emit_command_output(
                    app,
                    format!(
                        "Reload partially applied (.env refreshed), but config parse failed: {}",
                        err
                    ),
                );
            }
        }
    }
    Ok(CommandResult::Handled)
}

fn handle_cron_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let cron_data = hermes_config::cron_dir();
    let jobs_file = cron_data.join("jobs.json");
    let count = std::fs::read_to_string(&jobs_file)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_array().map(|arr| arr.len()))
        .unwrap_or(0);
    emit_command_output(
        app,
        format!(
            "Cron scheduler data dir: {}\nPersisted jobs: {}\nUse `hermes cron list` for full job table.",
            cron_data.display(),
            count
        ),
    );
    Ok(CommandResult::Handled)
}

fn blueprint_deliver_config(raw: &str) -> Option<DeliverConfig> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Some(DeliverConfig {
            target: DeliverTarget::Origin,
            platform: None,
        });
    }
    let (target_raw, platform) = raw
        .split_once(':')
        .map(|(target, platform)| (target, Some(platform.trim().to_string())))
        .unwrap_or((raw, None));
    let normalized = target_raw
        .trim()
        .to_ascii_lowercase()
        .replace(['-', '_'], "");
    let target = match normalized.as_str() {
        "origin" => DeliverTarget::Origin,
        "local" => DeliverTarget::Local,
        "telegram" => DeliverTarget::Telegram,
        "discord" => DeliverTarget::Discord,
        "slack" => DeliverTarget::Slack,
        "email" => DeliverTarget::Email,
        "whatsapp" => DeliverTarget::WhatsApp,
        "signal" => DeliverTarget::Signal,
        "matrix" => DeliverTarget::Matrix,
        "mattermost" => DeliverTarget::Mattermost,
        "dingtalk" => DeliverTarget::DingTalk,
        "feishu" => DeliverTarget::Feishu,
        "wecom" => DeliverTarget::WeCom,
        "weixin" | "wechat" | "wx" => DeliverTarget::Weixin,
        "bluebubbles" | "imessage" => DeliverTarget::BlueBubbles,
        "sms" => DeliverTarget::Sms,
        "homeassistant" | "ha" => DeliverTarget::HomeAssistant,
        "ntfy" => DeliverTarget::Ntfy,
        _ => return None,
    };
    Some(DeliverConfig {
        target,
        platform: platform.filter(|value| !value.trim().is_empty()),
    })
}

async fn handle_blueprint_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let raw = args.join(" ");
    match hermes_cron::resolve_blueprint_command(&raw) {
        Ok(BlueprintCommandAction::Catalog(text) | BlueprintCommandAction::Detail(text)) => {
            emit_command_output(app, text);
        }
        Ok(BlueprintCommandAction::Filled(spec)) => {
            let Some(deliver) = blueprint_deliver_config(&spec.deliver) else {
                emit_command_output(
                    app,
                    format!(
                        "Blueprint `{}` has unsupported deliver target `{}`.",
                        spec.key, spec.deliver
                    ),
                );
                return Ok(CommandResult::Handled);
            };

            let mut job = CronJob::new(spec.schedule.clone(), spec.prompt.clone());
            job.name = Some(spec.title.clone());
            if !spec.skills.is_empty() {
                job.skills = Some(spec.skills.clone());
            }
            job.deliver = Some(deliver);
            let job_id = app
                .cron_scheduler
                .create_job(job)
                .await
                .map_err(|e| AgentError::Config(format!("blueprint cron create: {e}")))?;
            emit_command_output(
                app,
                format!(
                    "Scheduled `{}` from blueprint `{}`.\nJob: {}\nSchedule: {}\nDeliver: {}\nManage it with `hermes cron list` or `/cron`.",
                    spec.title, spec.key, job_id, spec.schedule, spec.deliver
                ),
            );
        }
        Err(err) => {
            emit_command_output(
                app,
                format!("Blueprint error: {err}\nRun `/blueprint` to see the catalog."),
            );
        }
    }
    Ok(CommandResult::Handled)
}

fn suggestion_error(err: hermes_cron::SuggestionError) -> AgentError {
    AgentError::Config(format!("suggestions: {err}"))
}

fn render_pending_suggestions(pending: &[hermes_cron::SuggestionRecord]) -> String {
    if pending.is_empty() {
        return "No suggested automations right now.\nTry `/suggestions catalog` to see the curated starter set, or install a blueprint skill to get one.".to_string();
    }

    let mut out = String::from("Suggested automations - `/suggestions accept N` or `dismiss N`:\n");
    for (idx, suggestion) in pending.iter().enumerate() {
        let _ = writeln!(
            out,
            "\n  {}. {}  [{}]  ({})",
            idx + 1,
            suggestion.title,
            suggestion.job_spec.schedule,
            suggestion.source
        );
        if !suggestion.description.trim().is_empty() {
            let _ = writeln!(out, "     {}", suggestion.description.trim());
        }
    }
    out
}

fn render_suggestions_usage() -> &'static str {
    "Usage:\n  /suggestions              list pending\n  /suggestions accept N     schedule suggestion N\n  /suggestions dismiss N    dismiss suggestion N\n  /suggestions catalog      add curated starter automations\n  /suggestions clear        housekeeping"
}

fn cron_job_from_suggestion_spec(spec: &SuggestionJobSpec) -> Result<CronJob, String> {
    let Some(deliver) = blueprint_deliver_config(&spec.deliver) else {
        return Err(format!(
            "unsupported deliver target `{}`",
            spec.deliver.trim()
        ));
    };
    let mut job = CronJob::new(spec.schedule.clone(), spec.prompt.clone());
    job.name = Some(spec.name.clone());
    job.deliver = Some(deliver);
    if !spec.skills.is_empty() {
        job.skills = Some(spec.skills.clone());
    }
    Ok(job)
}

async fn handle_suggestions_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let store = hermes_cron::SuggestionStore::default();
    let sub = args
        .first()
        .map(|arg| arg.to_ascii_lowercase())
        .unwrap_or_default();
    let rest = args.get(1..).unwrap_or_default().join(" ");

    match sub.as_str() {
        "" => {
            let pending = store.list_pending().map_err(suggestion_error)?;
            emit_command_output(app, render_pending_suggestions(&pending));
        }
        "accept" | "add" | "schedule" => {
            if rest.trim().is_empty() {
                emit_command_output(app, "Usage: /suggestions accept <number|id>");
                return Ok(CommandResult::Handled);
            }
            let Some(suggestion) = store.get_pending(&rest).map_err(suggestion_error)? else {
                emit_command_output(
                    app,
                    format!(
                        "No pending suggestion matches '{}'. Run /suggestions to list them.",
                        rest.trim()
                    ),
                );
                return Ok(CommandResult::Handled);
            };
            let job = match cron_job_from_suggestion_spec(&suggestion.job_spec) {
                Ok(job) => job,
                Err(err) => {
                    emit_command_output(
                        app,
                        format!(
                            "Suggestion `{}` cannot be scheduled: {err}.",
                            suggestion.title
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
            };
            let job_id = app
                .cron_scheduler
                .create_job(job)
                .await
                .map_err(|e| AgentError::Config(format!("suggestion cron create: {e}")))?;
            store
                .mark_accepted(&suggestion.id)
                .map_err(suggestion_error)?;
            emit_command_output(
                app,
                format!(
                    "Scheduled '{}' ({}).\nJob: {}\nManage it with /cron.",
                    suggestion.job_spec.name, suggestion.job_spec.schedule, job_id
                ),
            );
        }
        "dismiss" | "no" | "reject" => {
            if rest.trim().is_empty() {
                emit_command_output(app, "Usage: /suggestions dismiss <number|id>");
                return Ok(CommandResult::Handled);
            }
            let dismissed = store.dismiss_suggestion(&rest).map_err(suggestion_error)?;
            if dismissed {
                emit_command_output(app, "Dismissed. Won't suggest that again.");
            } else {
                emit_command_output(
                    app,
                    format!("No pending suggestion matches '{}'.", rest.trim()),
                );
            }
        }
        "catalog" => {
            let created = store.seed_catalog_suggestions().map_err(suggestion_error)?;
            if created.is_empty() {
                emit_command_output(
                    app,
                    "No new catalog automations to add (already offered, dismissed, or your suggestion list is full). Run /suggestions to see pending.",
                );
            } else {
                let added = created
                    .iter()
                    .map(|record| record.title.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                emit_command_output(
                    app,
                    format!(
                        "Added {} suggestion(s): {}.\nRun /suggestions to review.",
                        created.len(),
                        added
                    ),
                );
            }
        }
        "clear" => {
            let removed = store.clear_resolved().map_err(suggestion_error)?;
            emit_command_output(
                app,
                format!("Cleared {removed} resolved suggestion record(s)."),
            );
        }
        _ => emit_command_output(app, render_suggestions_usage()),
    }

    Ok(CommandResult::Handled)
}

fn background_status_rows() -> Vec<String> {
    let jobs_dir = hermes_config::hermes_home().join("background_jobs");
    let mut rows = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(&jobs_dir) else {
        return rows;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("unknown");
        let status = v
            .get("status")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown");
        let task = v
            .get("task")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .replace('\n', " ");
        rows.push(format!("{id}  [{status}]  {task}"));
    }
    rows.sort();
    rows
}

#[derive(Debug, Clone, Default)]
struct BackgroundQueueAudit {
    dir: PathBuf,
    total_json: usize,
    valid_json: usize,
    malformed_json: usize,
    running_jobs: usize,
    stale_running_jobs: usize,
    duplicate_ids: usize,
}

fn audit_background_queue_manifests() -> BackgroundQueueAudit {
    let dir = hermes_config::hermes_home().join("background_jobs");
    let mut audit = BackgroundQueueAudit {
        dir: dir.clone(),
        ..Default::default()
    };
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return audit;
    };
    let mut ids = HashMap::<String, usize>::new();
    let now = SystemTime::now();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        audit.total_json += 1;
        let modified = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            audit.malformed_json += 1;
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            audit.malformed_json += 1;
            continue;
        };
        audit.valid_json += 1;
        let id = v
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
            .to_string();
        *ids.entry(id).or_insert(0) += 1;
        let status = v
            .get("status")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown");
        if status.eq_ignore_ascii_case("running") {
            audit.running_jobs += 1;
            let stale = now
                .duration_since(modified)
                .map(|age| age > Duration::from_secs(24 * 60 * 60))
                .unwrap_or(false);
            if stale {
                audit.stale_running_jobs += 1;
            }
        }
    }
    audit.duplicate_ids = ids.values().filter(|count| **count > 1).count();
    audit
}

fn render_background_queue_audit(audit: &BackgroundQueueAudit) -> String {
    format!(
        "Queue manifest audit (native)\n  dir: {}\n  json={} valid={} malformed={} running={} stale_running={} duplicate_ids={}",
        audit.dir.display(),
        audit.total_json,
        audit.valid_json,
        audit.malformed_json,
        audit.running_jobs,
        audit.stale_running_jobs,
        audit.duplicate_ids
    )
}

fn env_truthy(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn handle_agents_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sub = args.first().map(|s| s.trim().to_ascii_lowercase());

    if matches!(sub.as_deref(), Some("pause")) {
        std::env::set_var("HERMES_DELEGATION_PAUSED", "1");
        emit_command_output(
            app,
            "Delegation spawning paused for this runtime.\nSet with `/agents resume`.\nStatus: `/agents status`.",
        );
        return Ok(CommandResult::Handled);
    }
    if matches!(sub.as_deref(), Some("resume" | "unpause")) {
        std::env::set_var("HERMES_DELEGATION_PAUSED", "0");
        emit_command_output(
            app,
            "Delegation spawning resumed for this runtime.\nStatus: `/agents status`.",
        );
        return Ok(CommandResult::Handled);
    }
    if matches!(sub.as_deref(), Some("doctor")) {
        let audit = render_background_queue_audit(&audit_background_queue_manifests());
        emit_command_output(
            app,
            format!(
                "Agents doctor\n{}\n- delegation state: `/agents status`\n- spawn tree UI: `/agents` (TUI overlay)",
                audit
            ),
        );
        return Ok(CommandResult::Handled);
    }

    if matches!(sub.as_deref(), Some(other) if other != "status" && other != "list") {
        emit_command_output(app, "Usage: /agents [status|pause|resume|doctor]");
        return Ok(CommandResult::Handled);
    }

    let paused = std::env::var("HERMES_DELEGATION_PAUSED")
        .ok()
        .map(|raw| env_truthy(&raw))
        .unwrap_or(false);
    let rows = background_status_rows();
    if rows.is_empty() {
        let audit = render_background_queue_audit(&audit_background_queue_manifests());
        emit_command_output(
            app,
            format!(
                "Delegation spawning: {}\nBackground jobs: 0\n\nNo background jobs found.\n{}",
                if paused { "paused" } else { "active" },
                audit
            ),
        );
    } else {
        let audit = render_background_queue_audit(&audit_background_queue_manifests());
        let joined = rows.into_iter().take(20).collect::<Vec<_>>().join("\n");
        emit_command_output(
            app,
            format!(
                "Delegation spawning: {}\nBackground jobs (top 20):\n{}\n\n{}\nPause/resume: `/agents pause` or `/agents resume`",
                if paused { "paused" } else { "active" },
                joined,
                audit,
            ),
        );
    }
    Ok(CommandResult::Handled)
}
