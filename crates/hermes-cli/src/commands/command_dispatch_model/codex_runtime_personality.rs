fn emit_command_output(app: &mut App, text: impl Into<String>) {
    let rendered = text.into();
    if app.stream_handle.is_some() {
        app.push_ui_assistant(rendered);
    } else {
        println!("{}", rendered);
    }
}

fn normalize_codex_runtime_value(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Some("auto"),
        "codex_app_server" | "codex-app-server" => Some("codex_app_server"),
        _ => None,
    }
}

fn parse_codex_runtime_args(args: &[&str]) -> Result<Option<&'static str>, String> {
    let raw = args.join(" ");
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return Ok(None);
    }
    match value.as_str() {
        "on" | "codex" | "enable" => Ok(Some("codex_app_server")),
        "off" | "default" | "disable" | "hermes" => Ok(Some("auto")),
        _ => normalize_codex_runtime_value(&value)
            .map(Some)
            .ok_or_else(|| {
                format!(
                    "Unknown runtime '{}'. Use one of: auto, codex_app_server, on, off",
                    value
                )
            }),
    }
}

fn yaml_key(name: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(name.to_string())
}

fn read_codex_runtime_config(path: &Path) -> Result<serde_yaml::Value, AgentError> {
    if !path.exists() {
        return Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_yaml::from_str::<serde_yaml::Value>(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", path.display(), e)))
}

fn codex_runtime_from_config(root: &serde_yaml::Value) -> &'static str {
    root.as_mapping()
        .and_then(|map| map.get(yaml_key("model")))
        .and_then(|model| model.as_mapping())
        .and_then(|model| model.get(yaml_key("openai_runtime")))
        .and_then(|value| value.as_str())
        .and_then(normalize_codex_runtime_value)
        .unwrap_or("auto")
}

fn model_string_to_mapping(model: &str) -> serde_yaml::Mapping {
    let mut mapping = serde_yaml::Mapping::new();
    let model = model.trim();
    if model.is_empty() {
        return mapping;
    }
    if let Some((provider, default)) = model.split_once(':') {
        if !provider.trim().is_empty() {
            mapping.insert(
                yaml_key("provider"),
                serde_yaml::Value::String(provider.trim().to_string()),
            );
        }
        if !default.trim().is_empty() {
            mapping.insert(
                yaml_key("default"),
                serde_yaml::Value::String(default.trim().to_string()),
            );
        }
    } else {
        mapping.insert(
            yaml_key("default"),
            serde_yaml::Value::String(model.to_string()),
        );
    }
    mapping
}

fn set_codex_runtime_config_value(root: &mut serde_yaml::Value, runtime: &str) {
    if !matches!(root, serde_yaml::Value::Mapping(_)) {
        *root = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    let root_map = root.as_mapping_mut().expect("root mapping");
    let model_key = yaml_key("model");
    let mut model_map = match root_map.remove(&model_key) {
        Some(serde_yaml::Value::Mapping(map)) => map,
        Some(serde_yaml::Value::String(model)) => model_string_to_mapping(&model),
        Some(other) => {
            let mut map = serde_yaml::Mapping::new();
            map.insert(yaml_key("default"), other);
            map
        }
        None => serde_yaml::Mapping::new(),
    };
    model_map.insert(
        yaml_key("openai_runtime"),
        serde_yaml::Value::String(runtime.to_string()),
    );
    root_map.insert(model_key, serde_yaml::Value::Mapping(model_map));
}

fn write_codex_runtime_config(path: &Path, root: &serde_yaml::Value) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("create {}: {}", parent.display(), e)))?;
    }
    let yaml = serde_yaml::to_string(root)
        .map_err(|e| AgentError::Config(format!("serialize {}: {}", path.display(), e)))?;
    std::fs::write(path, yaml)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

fn check_codex_binary_status() -> (bool, String) {
    let output = std::process::Command::new("codex")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let fallback = String::from_utf8_lossy(&output.stderr).trim().to_string();
            (true, if text.is_empty() { fallback } else { text })
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let detail = if stderr.is_empty() {
                format!("codex exited with {}", output.status)
            } else {
                stderr
            };
            (false, detail)
        }
        Err(e) => (false, e.to_string()),
    }
}

fn handle_codex_runtime_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let config_path = app.state_root.join("config.yaml");
    let mut root = read_codex_runtime_config(&config_path)?;
    let current = codex_runtime_from_config(&root);
    let new_value = match parse_codex_runtime_args(args) {
        Ok(value) => value,
        Err(message) => {
            emit_command_output(app, format!("Codex runtime error: {}", message));
            return Ok(CommandResult::Handled);
        }
    };

    let Some(new_value) = new_value else {
        let (ok, detail) = check_codex_binary_status();
        let binary_status = if ok {
            format!("OK {}", detail)
        } else {
            format!(
                "not available - {}. Install with `npm i -g @openai/codex`",
                detail
            )
        };
        emit_command_output(
            app,
            format!(
                "openai_runtime: {}\ncodex CLI: {}\nconfig: {}",
                current,
                binary_status,
                config_path.display()
            ),
        );
        return Ok(CommandResult::Handled);
    };

    if new_value == current {
        emit_command_output(app, format!("openai_runtime already set to {}", current));
        return Ok(CommandResult::Handled);
    }

    if new_value == "codex_app_server" {
        let (ok, detail) = check_codex_binary_status();
        if !ok {
            emit_command_output(
                app,
                format!(
                    "Cannot enable codex_app_server runtime: {}\nInstall with: npm i -g @openai/codex",
                    detail
                ),
            );
            return Ok(CommandResult::Handled);
        }
    }

    set_codex_runtime_config_value(&mut root, new_value);
    write_codex_runtime_config(&config_path, &root)?;
    let mut msg = format!("openai_runtime: {} -> {}", current, new_value);
    if new_value == "codex_app_server" {
        msg.push_str("\nOpenAI/Codex turns will use the Codex app-server runtime.");
    } else {
        msg.push_str("\nOpenAI/Codex turns will use the default Hermes runtime.");
    }
    msg.push_str("\nEffective on next session.");
    emit_command_output(app, msg);
    Ok(CommandResult::Handled)
}

fn format_personality_catalog(
    current_personality: Option<&str>,
    builtin_descriptions: &[(&str, &str)],
) -> String {
    let mut out = String::from("## Built-in personalities\n\n");
    if let Some(current) = current_personality.filter(|v| !v.trim().is_empty()) {
        out.push_str(&format!("Current: `{}`\n\n", current));
    } else {
        out.push_str("Current: `(none)`\n\n");
    }
    out.push_str("Use `/personality <name>` to switch.\n\n");
    for (name, usage) in builtin_descriptions {
        out.push_str(&format!("- `{}`\n  {}\n\n", name, usage));
    }
    out.trim_end().to_string()
}

fn handle_personality_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let builtin = hermes_agent::builtin_personality_names();
    let builtin_descriptions = hermes_agent::builtin_personality_descriptions();
    if args.is_empty() || (args.len() == 1 && args[0].eq_ignore_ascii_case("list")) {
        emit_command_output(
            app,
            format_personality_catalog(app.current_personality.as_deref(), builtin_descriptions),
        );
    } else {
        let name = args.join(" ");
        app.switch_personality(&name);
        let mut response = format!("Switched personality to `{}`.", name);
        if !name.contains(char::is_whitespace)
            && !name.eq_ignore_ascii_case("default")
            && !builtin.iter().any(|n| n.eq_ignore_ascii_case(&name))
        {
            response.push_str(&format!(
                "\n\nNote: `{}` is not built-in. Hermes will look for `personalities/{}.md` or treat inline text as compatibility mode.",
                name, name,
            ));
        }
        emit_command_output(app, response);
    }
    Ok(CommandResult::Handled)
}
