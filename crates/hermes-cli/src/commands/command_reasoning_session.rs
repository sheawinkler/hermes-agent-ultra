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

include!("command_reasoning_session/skills_memory_cron.rs");
