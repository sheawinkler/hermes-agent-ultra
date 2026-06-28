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

