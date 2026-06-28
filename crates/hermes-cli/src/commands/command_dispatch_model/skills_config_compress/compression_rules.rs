#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CompressionRulePlaneConfig {
    #[serde(default)]
    max_assistant_render_lines: Option<usize>,
    #[serde(default)]
    max_tool_output_lines: Option<usize>,
    #[serde(default)]
    max_tool_output_line_chars: Option<usize>,
    #[serde(default)]
    max_tool_output_total_chars: Option<usize>,
}

#[derive(Debug, Clone)]
struct CompressionRenderPolicy {
    max_assistant_render_lines: usize,
    max_tool_output_lines: usize,
    max_tool_output_line_chars: usize,
    max_tool_output_total_chars: usize,
}

impl CompressionRenderPolicy {
    fn builtin_defaults() -> Self {
        Self {
            max_assistant_render_lines: 260,
            max_tool_output_lines: 180,
            max_tool_output_line_chars: 600,
            max_tool_output_total_chars: 48_000,
        }
    }

    fn apply_plane(&mut self, plane: &CompressionRulePlaneConfig) {
        if let Some(v) = plane.max_assistant_render_lines {
            self.max_assistant_render_lines = v.clamp(40, 4000);
        }
        if let Some(v) = plane.max_tool_output_lines {
            self.max_tool_output_lines = v.clamp(20, 5000);
        }
        if let Some(v) = plane.max_tool_output_line_chars {
            self.max_tool_output_line_chars = v.clamp(120, 4000);
        }
        if let Some(v) = plane.max_tool_output_total_chars {
            self.max_tool_output_total_chars = v.clamp(2000, 500_000);
        }
    }
}

fn compression_rules_dir() -> PathBuf {
    hermes_config::hermes_home().join("compression")
}

fn compression_user_rules_path() -> PathBuf {
    compression_rules_dir().join("user-rules.json")
}

fn compression_project_rules_path() -> Option<PathBuf> {
    hermes_tools::repo::detect_repo_root_from_cwd()
        .map(|root| root.join(".hermes-ultra").join("compression-rules.json"))
}

fn load_compression_plane(path: &Path) -> Option<CompressionRulePlaneConfig> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<CompressionRulePlaneConfig>(&raw).ok()
}

fn save_compression_plane(
    path: &Path,
    plane: &CompressionRulePlaneConfig,
) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", parent.display(), e)))?;
    }
    let payload = serde_json::to_string_pretty(plane)
        .map_err(|e| AgentError::Io(format!("Failed to encode compression rules: {}", e)))?;
    std::fs::write(path, payload)
        .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", path.display(), e)))?;
    Ok(())
}

fn merged_compression_policy() -> (
    CompressionRenderPolicy,
    Option<CompressionRulePlaneConfig>,
    Option<CompressionRulePlaneConfig>,
) {
    let mut merged = CompressionRenderPolicy::builtin_defaults();
    let user = load_compression_plane(&compression_user_rules_path());
    let project = compression_project_rules_path()
        .as_deref()
        .and_then(load_compression_plane);
    if let Some(ref user_plane) = user {
        merged.apply_plane(user_plane);
    }
    if let Some(ref project_plane) = project {
        merged.apply_plane(project_plane);
    }
    (merged, user, project)
}

fn apply_compression_policy_env(policy: &CompressionRenderPolicy) {
    std::env::set_var(
        "HERMES_TUI_MAX_ASSISTANT_RENDER_LINES",
        policy.max_assistant_render_lines.to_string(),
    );
    std::env::set_var(
        "HERMES_TUI_MAX_TOOL_OUTPUT_LINES",
        policy.max_tool_output_lines.to_string(),
    );
    std::env::set_var(
        "HERMES_TUI_MAX_TOOL_OUTPUT_LINE_CHARS",
        policy.max_tool_output_line_chars.to_string(),
    );
    std::env::set_var(
        "HERMES_TUI_MAX_TOOL_OUTPUT_TOTAL_CHARS",
        policy.max_tool_output_total_chars.to_string(),
    );
}

fn render_compression_policy_status() -> String {
    let (merged, user, project) = merged_compression_policy();
    let project_path = compression_project_rules_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(repo root unavailable)".to_string());
    let mut out = String::new();
    out.push_str("Compression policy planes\n");
    out.push_str("-------------------------\n");
    out.push_str("builtin: max_assistant_render_lines=260, max_tool_output_lines=180, max_tool_output_line_chars=600, max_tool_output_total_chars=48000\n");
    let _ = writeln!(
        out,
        "user: {} ({})",
        if user.is_some() {
            "configured"
        } else {
            "not configured"
        },
        compression_user_rules_path().display()
    );
    let _ = writeln!(
        out,
        "project: {} ({})",
        if project.is_some() {
            "configured"
        } else {
            "not configured"
        },
        project_path
    );
    let _ = writeln!(
        out,
        "\nmerged:\n  - max_assistant_render_lines={}\n  - max_tool_output_lines={}\n  - max_tool_output_line_chars={}\n  - max_tool_output_total_chars={}",
        merged.max_assistant_render_lines,
        merged.max_tool_output_lines,
        merged.max_tool_output_line_chars,
        merged.max_tool_output_total_chars
    );
    out.push_str(
        "\nUse `/compress rules recommend` to generate heuristics from current transcript shape.\n\
         Use `/compress rules autotune` for dry-run tuning, or `/compress rules autotune apply [user|project]` to persist + apply.\n\
         Use `/compress rules apply` to push merged settings into live runtime env.\n\
         Use `/compress rules set user <key> <value>` or `/compress rules set project <key> <value>`.\n\
         Keys: assistant_lines | tool_lines | tool_line_chars | tool_total_chars",
    );
    out
}

fn parse_compression_rule_key(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "assistant_lines" | "max_assistant_render_lines" | "assistant" => Some("assistant_lines"),
        "tool_lines" | "max_tool_output_lines" | "tool" => Some("tool_lines"),
        "tool_line_chars" | "max_tool_output_line_chars" | "tool_chars" => Some("tool_line_chars"),
        "tool_total_chars" | "max_tool_output_total_chars" | "tool_total" => {
            Some("tool_total_chars")
        }
        _ => None,
    }
}

fn set_compression_rule_field(
    plane: &mut CompressionRulePlaneConfig,
    key: &str,
    value: usize,
) -> Result<(), AgentError> {
    let normalized = parse_compression_rule_key(key).ok_or_else(|| {
        AgentError::Config(format!(
            "Unknown compression rule key '{}'. Use assistant_lines|tool_lines|tool_line_chars|tool_total_chars.",
            key
        ))
    })?;
    match normalized {
        "assistant_lines" => plane.max_assistant_render_lines = Some(value.clamp(40, 4000)),
        "tool_lines" => plane.max_tool_output_lines = Some(value.clamp(20, 5000)),
        "tool_line_chars" => plane.max_tool_output_line_chars = Some(value.clamp(120, 4000)),
        "tool_total_chars" => plane.max_tool_output_total_chars = Some(value.clamp(2000, 500_000)),
        _ => {}
    }
    Ok(())
}

fn resolve_compression_plane_path(target: &str) -> Result<PathBuf, AgentError> {
    let normalized = target.trim().to_ascii_lowercase();
    if normalized == "user" {
        return Ok(compression_user_rules_path());
    }
    if normalized == "project" {
        return compression_project_rules_path().ok_or_else(|| {
            AgentError::Config(
                "Project plane unavailable: run inside a repository checkout.".to_string(),
            )
        });
    }
    Err(AgentError::Config(
        "Plane must be `user` or `project`.".to_string(),
    ))
}

fn recommend_compression_policy_for_app(
    app: &App,
    base: &CompressionRenderPolicy,
) -> CompressionRenderPolicy {
    let mut next = base.clone();
    let mut assistant_msgs = 0usize;
    let mut assistant_lines = 0usize;
    let mut assistant_peak_line_chars = 0usize;
    let mut tool_msgs = 0usize;
    let mut tool_lines = 0usize;
    let mut tool_peak_line_chars = 0usize;
    let mut tool_total_chars = 0usize;

    for msg in &app.messages {
        let Some(content) = msg.content.as_ref() else {
            continue;
        };
        let lines = content.lines().count().max(1);
        let peak_line_chars = content
            .lines()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or_else(|| content.chars().count());
        match msg.role {
            hermes_core::MessageRole::Assistant => {
                assistant_msgs = assistant_msgs.saturating_add(1);
                assistant_lines = assistant_lines.saturating_add(lines);
                assistant_peak_line_chars = assistant_peak_line_chars.max(peak_line_chars);
            }
            hermes_core::MessageRole::Tool => {
                tool_msgs = tool_msgs.saturating_add(1);
                tool_lines = tool_lines.saturating_add(lines);
                tool_peak_line_chars = tool_peak_line_chars.max(peak_line_chars);
                tool_total_chars = tool_total_chars.saturating_add(content.chars().count());
            }
            _ => {}
        }
    }

    if assistant_msgs >= 6 {
        let avg = assistant_lines / assistant_msgs.max(1);
        if avg > 60 {
            next.max_assistant_render_lines = next.max_assistant_render_lines.clamp(320, 4000);
        } else if avg < 24 {
            next.max_assistant_render_lines = next.max_assistant_render_lines.clamp(40, 220);
        }
        if assistant_peak_line_chars > 160 {
            next.max_tool_output_line_chars = next.max_tool_output_line_chars.clamp(720, 4000);
        }
    }

    if tool_msgs >= 2 {
        let avg_tool_lines = tool_lines / tool_msgs.max(1);
        if avg_tool_lines > 120 {
            next.max_tool_output_lines = next.max_tool_output_lines.clamp(260, 5000);
        } else if avg_tool_lines < 40 {
            next.max_tool_output_lines = next.max_tool_output_lines.clamp(20, 160);
        }
        if tool_peak_line_chars > 720 {
            next.max_tool_output_line_chars = next.max_tool_output_line_chars.clamp(920, 4000);
        }
        if tool_total_chars > 120_000 {
            next.max_tool_output_total_chars =
                next.max_tool_output_total_chars.clamp(96_000, 500_000);
        } else if tool_total_chars < 24_000 {
            next.max_tool_output_total_chars =
                next.max_tool_output_total_chars.clamp(2000, 40_000);
        }
    }

    if app.messages.len() >= 140 {
        next.max_assistant_render_lines = next.max_assistant_render_lines.clamp(40, 240);
        next.max_tool_output_total_chars = next.max_tool_output_total_chars.clamp(2000, 64_000);
    }
    next
}

fn render_compression_recommendation(
    current: &CompressionRenderPolicy,
    recommended: &CompressionRenderPolicy,
) -> String {
    let mut out = String::new();
    out.push_str("Compression policy recommendation\n");
    out.push_str("---------------------------------\n");
    let _ = writeln!(
        out,
        "assistant_lines: {} -> {}",
        current.max_assistant_render_lines, recommended.max_assistant_render_lines
    );
    let _ = writeln!(
        out,
        "tool_lines: {} -> {}",
        current.max_tool_output_lines, recommended.max_tool_output_lines
    );
    let _ = writeln!(
        out,
        "tool_line_chars: {} -> {}",
        current.max_tool_output_line_chars, recommended.max_tool_output_line_chars
    );
    let _ = writeln!(
        out,
        "tool_total_chars: {} -> {}",
        current.max_tool_output_total_chars, recommended.max_tool_output_total_chars
    );
    out.push_str(
        "\nApply with `/compress rules autotune apply` (user plane) or `/compress rules autotune apply project`.",
    );
    out
}

fn handle_compress_rules_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match action.as_str() {
        "status" | "show" | "preview" => {
            emit_command_output(app, render_compression_policy_status());
        }
        "recommend" => {
            let (merged, _, _) = merged_compression_policy();
            let rec = recommend_compression_policy_for_app(app, &merged);
            emit_command_output(app, render_compression_recommendation(&merged, &rec));
        }
        "autotune" => {
            let (merged, _, _) = merged_compression_policy();
            let rec = recommend_compression_policy_for_app(app, &merged);
            if args
                .get(1)
                .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "apply" | "--apply"))
            {
                let target = args
                    .get(2)
                    .copied()
                    .unwrap_or("user")
                    .to_ascii_lowercase();
                let path = match resolve_compression_plane_path(&target) {
                    Ok(path) => path,
                    Err(err) => {
                        emit_command_output(app, err.to_string());
                        return Ok(CommandResult::Handled);
                    }
                };
                let plane = CompressionRulePlaneConfig {
                    max_assistant_render_lines: Some(rec.max_assistant_render_lines),
                    max_tool_output_lines: Some(rec.max_tool_output_lines),
                    max_tool_output_line_chars: Some(rec.max_tool_output_line_chars),
                    max_tool_output_total_chars: Some(rec.max_tool_output_total_chars),
                };
                save_compression_plane(&path, &plane)?;
                apply_compression_policy_env(&rec);
                emit_command_output(
                    app,
                    format!(
                        "{}\n\nAutotune applied to {} plane ({}) and runtime env updated.",
                        render_compression_recommendation(&merged, &rec),
                        target,
                        path.display()
                    ),
                );
            } else {
                emit_command_output(
                    app,
                    format!(
                        "{}\n\nDry-run only. Add `apply` to persist: `/compress rules autotune apply [user|project]`.",
                        render_compression_recommendation(&merged, &rec)
                    ),
                );
            }
        }
        "apply" => {
            let (merged, _, _) = merged_compression_policy();
            apply_compression_policy_env(&merged);
            emit_command_output(
                app,
                format!(
                    "Applied compression policy to runtime env.\n\
                     HERMES_TUI_MAX_ASSISTANT_RENDER_LINES={}\n\
                     HERMES_TUI_MAX_TOOL_OUTPUT_LINES={}\n\
                     HERMES_TUI_MAX_TOOL_OUTPUT_LINE_CHARS={}\n\
                     HERMES_TUI_MAX_TOOL_OUTPUT_TOTAL_CHARS={}",
                    merged.max_assistant_render_lines,
                    merged.max_tool_output_lines,
                    merged.max_tool_output_line_chars,
                    merged.max_tool_output_total_chars
                ),
            );
        }
        "set" => {
            let Some(plane_name) = args.get(1).copied() else {
                emit_command_output(
                    app,
                    "Usage: /compress rules set <user|project> <key> <value>",
                );
                return Ok(CommandResult::Handled);
            };
            let Some(key) = args.get(2).copied() else {
                emit_command_output(
                    app,
                    "Usage: /compress rules set <user|project> <key> <value>",
                );
                return Ok(CommandResult::Handled);
            };
            let Some(value_raw) = args.get(3).copied() else {
                emit_command_output(
                    app,
                    "Usage: /compress rules set <user|project> <key> <value>",
                );
                return Ok(CommandResult::Handled);
            };
            let value = value_raw.parse::<usize>().map_err(|_| {
                AgentError::Config(format!("Invalid value '{}' (expected positive integer).", value_raw))
            })?;
            let target = plane_name.trim().to_ascii_lowercase();
            let path = match resolve_compression_plane_path(&target) {
                Ok(path) => path,
                Err(err) => {
                    emit_command_output(app, err.to_string());
                    return Ok(CommandResult::Handled);
                }
            };
            let mut plane = load_compression_plane(&path).unwrap_or_default();
            set_compression_rule_field(&mut plane, key, value)?;
            save_compression_plane(&path, &plane)?;
            emit_command_output(
                app,
                format!(
                    "Updated {} compression rule: {}={} ({})",
                    target,
                    key,
                    value,
                    path.display()
                ),
            );
        }
        "clear" => {
            let Some(plane_name) = args.get(1).copied() else {
                emit_command_output(app, "Usage: /compress rules clear <user|project>");
                return Ok(CommandResult::Handled);
            };
            let target = plane_name.trim().to_ascii_lowercase();
            let path = match resolve_compression_plane_path(&target) {
                Ok(path) => path,
                Err(err) => {
                    emit_command_output(app, err.to_string());
                    return Ok(CommandResult::Handled);
                }
            };
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| AgentError::Io(format!("Failed to remove {}: {}", path.display(), e)))?;
                emit_command_output(app, format!("Cleared {} plane rules at {}.", target, path.display()));
            } else {
                emit_command_output(app, format!("{} plane rules already clear.", target));
            }
        }
        _ => emit_command_output(
            app,
            "Usage: /compress rules [status|preview|recommend|autotune [apply [user|project]]|apply|set <user|project> <key> <value>|clear <user|project>]",
        ),
    }
    Ok(CommandResult::Handled)
}

fn estimate_message_tokens_for_compress(messages: &[hermes_core::Message]) -> usize {
    messages
        .iter()
        .map(|m| m.content.as_ref().map_or(0, |c| c.len()) / 4)
        .sum()
}

fn handle_compress_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args
        .first()
        .map(|v| v.eq_ignore_ascii_case("rules"))
        .unwrap_or(false)
    {
        return handle_compress_rules_command(app, &args[1..]);
    }
    let msg_count = app.messages.len();
    if msg_count <= 2 {
        emit_command_output(
            app,
            format!("Context too small to compress ({} messages).", msg_count),
        );
        return Ok(CommandResult::Handled);
    }

    let before_count = msg_count;
    let before_tokens = estimate_message_tokens_for_compress(&app.messages);
    let keep = std::cmp::max(2, msg_count / 3);
    let removed = msg_count - keep;
    let summary_text = format!(
        "[Compressed: {} earlier messages summarized. {} messages retained.]",
        removed, keep,
    );

    let split_at = app.messages.len() - keep;
    let retained = app.messages.split_off(split_at);
    app.messages.clear();
    app.messages
        .push(hermes_core::Message::system(summary_text));
    app.messages.extend(retained);
    let after_count = app.messages.len();
    let after_tokens = estimate_message_tokens_for_compress(&app.messages);

    emit_command_output(
        app,
        format!(
            "Compressed: {} → {} messages / ~{} → ~{} tokens.\nCompressed context: removed {} messages, kept {}. Total now: {}.",
            before_count, after_count, before_tokens, after_tokens, removed, keep, after_count
        ),
    );
    Ok(CommandResult::Handled)
}
