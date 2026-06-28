/// Serialize `GatewayConfig` to YAML. Creates parent directories. Omits `home_dir` from output.
pub fn save_config_yaml(path: &Path, config: &GatewayConfig) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_to_config_error)?;
    }
    let mut to_save = config.clone();
    to_save.home_dir = None;
    let yaml = serde_yaml::to_string(&to_save)
        .map(|yaml| normalize_yaml_sequence_indent(&yaml))
        .map_err(yaml_to_config_error)?;
    atomic_write_bytes(path, yaml.as_bytes())?;
    secure_config_file(path)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSetResult {
    pub config_path: Option<PathBuf>,
    pub env_path: Option<PathBuf>,
    pub env_key: Option<String>,
    pub config_key: Option<String>,
}

impl ConfigSetResult {
    pub fn wrote_config(&self) -> bool {
        self.config_path.is_some()
    }

    pub fn wrote_env(&self) -> bool {
        self.env_path.is_some()
    }
}

const EXPLICIT_ENV_CONFIG_KEYS: &[&str] = &[
    "OPENROUTER_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "WANDB_API_KEY",
    "TINKER_API_KEY",
    "HONCHO_API_KEY",
    "FIRECRAWL_API_KEY",
    "BROWSERBASE_API_KEY",
    "FAL_KEY",
    "SUDO_PASSWORD",
    "GITHUB_TOKEN",
    "TELEGRAM_BOT_TOKEN",
    "DISCORD_BOT_TOKEN",
    "SLACK_BOT_TOKEN",
    "SLACK_APP_TOKEN",
];

/// Python-compatible `hermes config set` persistence.
///
/// Secret-like all-caps keys are written to `$HERMES_HOME/.env`; normal dotted
/// keys are patched into `config.yaml` as raw YAML so list indices and future
/// upstream keys survive round-trips instead of being dropped by the typed
/// `GatewayConfig` serializer.
pub fn set_user_config_value(
    home_dir: &Path,
    key: &str,
    value: &str,
) -> Result<ConfigSetResult, ConfigError> {
    let key = key.trim();
    if key.is_empty() {
        return Err(ConfigError::ValidationError(
            "config key must not be empty".to_string(),
        ));
    }

    let config_path = home_dir.join("config.yaml");
    let env_path = home_dir.join(".env");
    let bridge_env_key = config_env_bridge_key(key);
    let env_key = config_key_routes_to_env(key).or_else(|| bridge_env_key.clone());
    let writes_config = bridge_env_key.is_some() || env_key.is_none();

    let mut result = ConfigSetResult {
        config_path: None,
        env_path: None,
        env_key: None,
        config_key: None,
    };

    if writes_config {
        let mut root = load_user_config_yaml_value(&config_path)?;
        set_yaml_path(&mut root, &split_config_key(key), scalar_yaml_value(value))?;
        validate_user_config_value(&root)?;
        atomic_yaml_write(&config_path, &root, None)?;
        secure_config_file(&config_path)?;
        result.config_path = Some(config_path);
        result.config_key = Some(key.to_string());
    }

    if let Some(env_key) = env_key {
        save_env_key_value(&env_path, &env_key, value)?;
        // SAFETY: config writes run on the foreground CLI path.
        unsafe { std::env::set_var(&env_key, value) };
        result.env_path = Some(env_path);
        result.env_key = Some(env_key);
    }

    Ok(result)
}

fn canonical_env_key(key: &str) -> String {
    key.trim().replace(['.', '-'], "_").to_ascii_uppercase()
}

fn config_key_routes_to_env(key: &str) -> Option<String> {
    if key.contains('.') {
        return None;
    }
    let canonical = canonical_env_key(key);
    if EXPLICIT_ENV_CONFIG_KEYS.contains(&canonical.as_str())
        || canonical.ends_with("_API_KEY")
        || canonical.ends_with("_TOKEN")
        || canonical.starts_with("TERMINAL_SSH_")
    {
        Some(canonical)
    } else {
        None
    }
}

pub fn terminal_config_env_bridge_pairs() -> &'static [(&'static str, &'static str)] {
    &[
        ("backend", "TERMINAL_ENV"),
        ("env_type", "TERMINAL_ENV"),
        ("workdir", "TERMINAL_CWD"),
        ("cwd", "TERMINAL_CWD"),
        ("timeout", "TERMINAL_TIMEOUT"),
        ("max_output_size", "TERMINAL_MAX_OUTPUT_SIZE"),
        ("docker_container_id", "TERMINAL_DOCKER_CONTAINER_ID"),
        ("docker_image", "TERMINAL_DOCKER_IMAGE"),
        (
            "docker_mount_cwd_to_workspace",
            "TERMINAL_DOCKER_MOUNT_CWD_TO_WORKSPACE",
        ),
        (
            "docker_run_as_host_user",
            "TERMINAL_DOCKER_RUN_AS_HOST_USER",
        ),
        ("container_cpu", "TERMINAL_CONTAINER_CPU"),
        ("container_memory", "TERMINAL_CONTAINER_MEMORY"),
        ("container_disk", "TERMINAL_CONTAINER_DISK"),
        ("container_persistent", "TERMINAL_CONTAINER_PERSISTENT"),
        ("docker_env", "TERMINAL_DOCKER_ENV"),
        ("docker_forward_env", "TERMINAL_DOCKER_FORWARD_ENV"),
        ("docker_volumes", "TERMINAL_DOCKER_VOLUMES"),
        ("vercel_runtime", "TERMINAL_VERCEL_RUNTIME"),
        ("modal_mode", "TERMINAL_MODAL_MODE"),
        ("shell_init_files", "TERMINAL_SHELL_INIT_FILES"),
        ("auto_source_bashrc", "TERMINAL_AUTO_SOURCE_BASHRC"),
        ("home_mode", "TERMINAL_HOME_MODE"),
        ("ssh_host", "TERMINAL_SSH_HOST"),
        ("ssh_port", "TERMINAL_SSH_PORT"),
        ("ssh_user", "TERMINAL_SSH_USER"),
        ("ssh_key_path", "TERMINAL_SSH_KEY_PATH"),
    ]
}

pub fn terminal_config_env_bridge_key(key: &str) -> Option<&'static str> {
    let normalized = key
        .trim()
        .strip_prefix("terminal.")
        .unwrap_or_else(|| key.trim())
        .replace('-', "_")
        .to_ascii_lowercase();
    terminal_config_env_bridge_pairs()
        .iter()
        .find_map(|(config_key, env_key)| (*config_key == normalized).then_some(*env_key))
}

pub fn web_config_env_bridge_pairs() -> &'static [(&'static str, &'static str)] {
    &[
        ("backend", "HERMES_WEB_BACKEND"),
        ("search_backend", "HERMES_WEB_SEARCH_BACKEND"),
        ("extract_backend", "HERMES_WEB_EXTRACT_BACKEND"),
        ("crawl_backend", "HERMES_WEB_CRAWL_BACKEND"),
    ]
}

pub fn web_config_env_bridge_key(key: &str) -> Option<&'static str> {
    let normalized = key
        .trim()
        .strip_prefix("web.")
        .unwrap_or_else(|| key.trim())
        .replace('-', "_")
        .to_ascii_lowercase();
    web_config_env_bridge_pairs()
        .iter()
        .find_map(|(config_key, env_key)| (*config_key == normalized).then_some(*env_key))
}

pub fn display_config_env_bridge_pairs() -> &'static [(&'static str, &'static str)] {
    &[
        ("busy_input_mode", "HERMES_GATEWAY_BUSY_INPUT_MODE"),
        ("busy_ack_enabled", "HERMES_GATEWAY_BUSY_ACK_ENABLED"),
        (
            "memory_notifications",
            "HERMES_MEMORY_NOTIFICATIONS_ENABLED",
        ),
    ]
}

pub fn display_config_env_bridge_key(key: &str) -> Option<&'static str> {
    let normalized = key
        .trim()
        .strip_prefix("display.")
        .unwrap_or_else(|| key.trim())
        .replace('-', "_")
        .to_ascii_lowercase();
    display_config_env_bridge_pairs()
        .iter()
        .find_map(|(config_key, env_key)| (*config_key == normalized).then_some(*env_key))
}

fn config_env_bridge_key(key: &str) -> Option<String> {
    terminal_config_env_bridge_key(key)
        .or_else(|| web_config_env_bridge_key(key))
        .or_else(|| display_config_env_bridge_key(key))
        .map(ToString::to_string)
}

fn split_config_key(key: &str) -> Vec<String> {
    key.split('.')
        .filter(|part| !part.is_empty())
        .enumerate()
        .map(|(index, part)| {
            if index == 0 && part == "llm" {
                "llm_providers".to_string()
            } else {
                part.to_string()
            }
        })
        .collect()
}

fn scalar_yaml_value(value: &str) -> serde_yaml::Value {
    if value.is_empty() {
        return serde_yaml::Value::String(String::new());
    }
    let trimmed = value.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => return serde_yaml::Value::Bool(true),
        "false" | "no" | "off" => return serde_yaml::Value::Bool(false),
        _ => {}
    }
    if let Ok(n) = trimmed.parse::<i64>() {
        return serde_yaml::Value::Number(n.into());
    }
    serde_yaml::Value::String(value.to_string())
}

fn load_user_config_yaml_value(path: &Path) -> Result<serde_yaml::Value, ConfigError> {
    if !path.exists() {
        return Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    }
    let contents = std::fs::read_to_string(path).map_err(io_to_config_error)?;
    if contents.trim().is_empty() {
        return Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    }
    serde_yaml::from_str(&contents).map_err(yaml_to_config_error)
}

fn validate_user_config_value(root: &serde_yaml::Value) -> Result<(), ConfigError> {
    let mut normalized = root.clone();
    if let serde_yaml::Value::Mapping(ref mut m) = normalized {
        crate::python_yaml_compat::normalize_config_yaml_root(m);
    }
    mark_platform_enabled_explicit(&mut normalized, "slack");
    mark_platform_enabled_explicit(&mut normalized, "ntfy");
    let mut cfg: GatewayConfig =
        serde_yaml::from_value(normalized).map_err(yaml_to_config_error)?;
    normalize_platform_aliases(&mut cfg);
    normalize_provider_secrets(&mut cfg);
    validate_config(&cfg)
}

fn set_yaml_path(
    current: &mut serde_yaml::Value,
    parts: &[String],
    new_value: serde_yaml::Value,
) -> Result<(), ConfigError> {
    if parts.is_empty() {
        *current = new_value;
        return Ok(());
    }

    let head = parts[0].as_str();
    let tail = &parts[1..];
    if let Ok(index) = head.parse::<usize>() {
        ensure_sequence(current);
        let serde_yaml::Value::Sequence(seq) = current else {
            unreachable!("ensure_sequence always leaves a sequence")
        };
        while seq.len() <= index {
            seq.push(default_container_for(tail));
        }
        if tail.is_empty() {
            seq[index] = new_value;
        } else {
            set_yaml_path(&mut seq[index], tail, new_value)?;
        }
        return Ok(());
    }

    ensure_mapping(current);
    let serde_yaml::Value::Mapping(map) = current else {
        unreachable!("ensure_mapping always leaves a mapping")
    };
    let key = serde_yaml::Value::String(head.to_string());
    if tail.is_empty() {
        map.insert(key, new_value);
    } else {
        let entry = map
            .entry(key)
            .or_insert_with(|| default_container_for(tail));
        set_yaml_path(entry, tail, new_value)?;
    }
    Ok(())
}

fn ensure_mapping(value: &mut serde_yaml::Value) {
    if !matches!(value, serde_yaml::Value::Mapping(_)) {
        *value = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
}

fn ensure_sequence(value: &mut serde_yaml::Value) {
    if !matches!(value, serde_yaml::Value::Sequence(_)) {
        *value = serde_yaml::Value::Sequence(Vec::new());
    }
}

fn default_container_for(tail: &[String]) -> serde_yaml::Value {
    if tail
        .first()
        .is_some_and(|part| part.parse::<usize>().is_ok())
    {
        serde_yaml::Value::Sequence(Vec::new())
    } else {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    }
}

fn save_env_key_value(path: &Path, key: &str, value: &str) -> Result<(), ConfigError> {
    let original = std::fs::read_to_string(path).unwrap_or_default();
    let sanitized_value = value.replace('\n', "\\n");
    let mut lines = Vec::new();
    let mut replaced = false;

    for line in original.lines() {
        let line_key = line
            .split_once('=')
            .map(|(k, _)| k.trim())
            .filter(|k| !k.is_empty());
        if line_key == Some(key) {
            if !replaced {
                lines.push(format!("{key}={sanitized_value}"));
                replaced = true;
            }
        } else {
            lines.push(line.to_string());
        }
    }

    if !replaced {
        lines.push(format!("{key}={sanitized_value}"));
    }

    let mut out = lines.join("\n");
    out.push('\n');
    atomic_write_bytes(path, out.as_bytes())
}
