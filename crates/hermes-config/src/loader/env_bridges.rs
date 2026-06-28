fn trimmed_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn expand_home_path(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(trimmed)
}

fn env_var_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn parse_bool_env(name: &str, value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => {
            tracing::warn!("{name} is not a valid bool-like value: {value}");
            None
        }
    }
}

fn parse_list_env(value: &str, split_colon: bool) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.starts_with('[') {
        if let Ok(values) = serde_json::from_str::<Vec<String>>(trimmed) {
            return values
                .into_iter()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect();
        }
    }
    let delimiter = if trimmed.contains(',') || !split_colon {
        ','
    } else {
        ':'
    };
    trimmed
        .split(delimiter)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn set_env_if_missing(name: &str, value: String) {
    if value.trim().is_empty() || env_var_nonempty(name).is_some() {
        return;
    }
    // SAFETY: configuration loading runs during CLI/gateway startup.
    unsafe { std::env::set_var(name, value) };
}

fn bridge_terminal_config_to_env(terminal: &TerminalConfig) {
    let default = TerminalConfig::default();
    if terminal.backend != default.backend {
        set_env_if_missing(
            "TERMINAL_ENV",
            match terminal.backend {
                TerminalBackendType::Local => "local",
                TerminalBackendType::Docker => "docker",
                TerminalBackendType::Ssh => "ssh",
                TerminalBackendType::Daytona => "daytona",
                TerminalBackendType::Modal => "modal",
                TerminalBackendType::Singularity => "singularity",
            }
            .to_string(),
        );
    }
    if terminal.timeout != default.timeout {
        set_env_if_missing("TERMINAL_TIMEOUT", terminal.timeout.to_string());
    }
    if terminal.max_output_size != default.max_output_size {
        set_env_if_missing(
            "TERMINAL_MAX_OUTPUT_SIZE",
            terminal.max_output_size.to_string(),
        );
    }
    if let Some(value) = &terminal.workdir {
        set_env_if_missing("TERMINAL_CWD", value.clone());
    }
    if let Some(value) = &terminal.docker_container_id {
        set_env_if_missing("TERMINAL_DOCKER_CONTAINER_ID", value.clone());
    }
    if let Some(value) = &terminal.docker_image {
        set_env_if_missing("TERMINAL_DOCKER_IMAGE", value.clone());
    }
    if terminal.docker_mount_cwd_to_workspace != default.docker_mount_cwd_to_workspace {
        set_env_if_missing(
            "TERMINAL_DOCKER_MOUNT_CWD_TO_WORKSPACE",
            terminal.docker_mount_cwd_to_workspace.to_string(),
        );
    }
    if terminal.docker_run_as_host_user != default.docker_run_as_host_user {
        set_env_if_missing(
            "TERMINAL_DOCKER_RUN_AS_HOST_USER",
            terminal.docker_run_as_host_user.to_string(),
        );
    }
    if let Some(value) = terminal.container_cpu {
        set_env_if_missing("TERMINAL_CONTAINER_CPU", value.to_string());
    }
    if let Some(value) = terminal.container_memory {
        set_env_if_missing("TERMINAL_CONTAINER_MEMORY", value.to_string());
    }
    if let Some(value) = terminal.container_disk {
        set_env_if_missing("TERMINAL_CONTAINER_DISK", value.to_string());
    }
    if terminal.container_persistent != default.container_persistent {
        set_env_if_missing(
            "TERMINAL_CONTAINER_PERSISTENT",
            terminal.container_persistent.to_string(),
        );
    }
    if let Some(value) = &terminal.docker_env {
        set_env_if_missing("TERMINAL_DOCKER_ENV", value.clone());
    }
    if !terminal.docker_forward_env.is_empty() {
        set_env_if_missing(
            "TERMINAL_DOCKER_FORWARD_ENV",
            terminal.docker_forward_env.join(","),
        );
    }
    if !terminal.docker_volumes.is_empty() {
        set_env_if_missing("TERMINAL_DOCKER_VOLUMES", terminal.docker_volumes.join(","));
    }
    if let Some(value) = &terminal.vercel_runtime {
        set_env_if_missing("TERMINAL_VERCEL_RUNTIME", value.clone());
    }
    if let Some(value) = &terminal.modal_mode {
        set_env_if_missing("TERMINAL_MODAL_MODE", value.clone());
    }
    if !terminal.shell_init_files.is_empty() {
        set_env_if_missing(
            "TERMINAL_SHELL_INIT_FILES",
            terminal.shell_init_files.join(","),
        );
    }
    if terminal.auto_source_bashrc != default.auto_source_bashrc {
        set_env_if_missing(
            "TERMINAL_AUTO_SOURCE_BASHRC",
            terminal.auto_source_bashrc.to_string(),
        );
    }
    if terminal.home_mode != default.home_mode {
        set_env_if_missing(
            "TERMINAL_HOME_MODE",
            terminal.home_mode.as_env_name().to_string(),
        );
    }
    if let Some(value) = &terminal.ssh_host {
        set_env_if_missing("TERMINAL_SSH_HOST", value.clone());
    }
    if let Some(value) = terminal.ssh_port {
        set_env_if_missing("TERMINAL_SSH_PORT", value.to_string());
    }
    if let Some(value) = &terminal.ssh_user {
        set_env_if_missing("TERMINAL_SSH_USER", value.clone());
    }
    if let Some(value) = &terminal.ssh_key_path {
        set_env_if_missing("TERMINAL_SSH_KEY_PATH", value.clone());
    }
}

fn bridge_web_config_to_env(web: &WebConfig) {
    if !web.backend.trim().is_empty() {
        set_env_if_missing("HERMES_WEB_BACKEND", web.backend.clone());
    }
    if !web.search_backend.trim().is_empty() {
        set_env_if_missing("HERMES_WEB_SEARCH_BACKEND", web.search_backend.clone());
    }
    if !web.extract_backend.trim().is_empty() {
        set_env_if_missing("HERMES_WEB_EXTRACT_BACKEND", web.extract_backend.clone());
    }
    if !web.crawl_backend.trim().is_empty() {
        set_env_if_missing("HERMES_WEB_CRAWL_BACKEND", web.crawl_backend.clone());
    }
}

fn apply_web_env_overrides(config: &mut WebConfig) {
    if let Some(v) = env_var_nonempty("HERMES_WEB_BACKEND") {
        config.backend = v;
    }
    if let Some(v) = env_var_nonempty("HERMES_WEB_SEARCH_BACKEND") {
        config.search_backend = v;
    }
    if let Some(v) = env_var_nonempty("HERMES_WEB_EXTRACT_BACKEND") {
        config.extract_backend = v;
    }
    if let Some(v) = env_var_nonempty("HERMES_WEB_CRAWL_BACKEND") {
        config.crawl_backend = v;
    }
}

fn bridge_display_config_to_env(display: &DisplayConfig) {
    if let Some(mode) = display
        .busy_input_mode
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        set_env_if_missing("HERMES_GATEWAY_BUSY_INPUT_MODE", mode.to_string());
    }
    if let Some(enabled) = display.busy_ack_enabled {
        set_env_if_missing("HERMES_GATEWAY_BUSY_ACK_ENABLED", enabled.to_string());
    }
    if let Some(enabled) = display.memory_notifications {
        set_env_if_missing("HERMES_MEMORY_NOTIFICATIONS_ENABLED", enabled.to_string());
    }
}

fn apply_display_env_overrides(config: &mut DisplayConfig) {
    if let Some(v) = env_var_nonempty("HERMES_GATEWAY_BUSY_INPUT_MODE") {
        config.busy_input_mode = Some(v);
    }
    if let Some(v) = env_var_nonempty("HERMES_GATEWAY_BUSY_ACK_ENABLED") {
        if let Some(parsed) = parse_bool_env("HERMES_GATEWAY_BUSY_ACK_ENABLED", &v) {
            config.busy_ack_enabled = Some(parsed);
        }
    }
    if let Some(v) = env_var_nonempty("HERMES_MEMORY_NOTIFICATIONS_ENABLED") {
        if let Some(parsed) = parse_bool_env("HERMES_MEMORY_NOTIFICATIONS_ENABLED", &v) {
            config.memory_notifications = Some(parsed);
        }
    }
}

fn apply_terminal_env_overrides(config: &mut TerminalConfig) {
    if let Some(v) =
        env_var_nonempty("TERMINAL_ENV").or_else(|| env_var_nonempty("TERMINAL_BACKEND"))
    {
        match TerminalBackendType::from_env_name(&v) {
            Some(backend) => config.backend = backend,
            None => tracing::warn!("Unknown TERMINAL_ENV '{v}'"),
        }
    }
    if let Some(v) = env_var_nonempty("TERMINAL_TIMEOUT") {
        if let Ok(n) = v.parse::<u64>() {
            config.timeout = n;
        } else {
            tracing::warn!("TERMINAL_TIMEOUT is not a valid u64: {v}");
        }
    }
    if let Some(v) = env_var_nonempty("TERMINAL_MAX_OUTPUT_SIZE") {
        if let Ok(n) = v.parse::<usize>() {
            config.max_output_size = n;
        } else {
            tracing::warn!("TERMINAL_MAX_OUTPUT_SIZE is not a valid usize: {v}");
        }
    }
    if let Some(v) = env_var_nonempty("TERMINAL_CWD") {
        config.workdir = Some(v);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_DOCKER_CONTAINER_ID") {
        config.docker_container_id = Some(v);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_DOCKER_IMAGE") {
        config.docker_image = Some(v);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_DOCKER_MOUNT_CWD_TO_WORKSPACE")
        .and_then(|v| parse_bool_env("TERMINAL_DOCKER_MOUNT_CWD_TO_WORKSPACE", &v))
    {
        config.docker_mount_cwd_to_workspace = v;
    }
    if let Some(v) = env_var_nonempty("TERMINAL_DOCKER_RUN_AS_HOST_USER")
        .and_then(|v| parse_bool_env("TERMINAL_DOCKER_RUN_AS_HOST_USER", &v))
    {
        config.docker_run_as_host_user = v;
    }
    if let Some(v) = env_var_nonempty("TERMINAL_CONTAINER_CPU") {
        if let Ok(n) = v.parse::<u32>() {
            config.container_cpu = Some(n);
        } else {
            tracing::warn!("TERMINAL_CONTAINER_CPU is not a valid u32: {v}");
        }
    }
    if let Some(v) = env_var_nonempty("TERMINAL_CONTAINER_MEMORY") {
        if let Ok(n) = v.parse::<u64>() {
            config.container_memory = Some(n);
        } else {
            tracing::warn!("TERMINAL_CONTAINER_MEMORY is not a valid u64: {v}");
        }
    }
    if let Some(v) = env_var_nonempty("TERMINAL_CONTAINER_DISK") {
        if let Ok(n) = v.parse::<u64>() {
            config.container_disk = Some(n);
        } else {
            tracing::warn!("TERMINAL_CONTAINER_DISK is not a valid u64: {v}");
        }
    }
    if let Some(v) = env_var_nonempty("TERMINAL_CONTAINER_PERSISTENT")
        .and_then(|v| parse_bool_env("TERMINAL_CONTAINER_PERSISTENT", &v))
    {
        config.container_persistent = v;
    }
    if let Some(v) = env_var_nonempty("TERMINAL_DOCKER_ENV") {
        config.docker_env = Some(v);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_DOCKER_FORWARD_ENV") {
        config.docker_forward_env = parse_list_env(&v, false);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_DOCKER_VOLUMES") {
        config.docker_volumes = parse_list_env(&v, false);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_VERCEL_RUNTIME") {
        config.vercel_runtime = Some(v);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_MODAL_MODE") {
        config.modal_mode = Some(v);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_SHELL_INIT_FILES") {
        config.shell_init_files = parse_list_env(&v, true);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_AUTO_SOURCE_BASHRC")
        .and_then(|v| parse_bool_env("TERMINAL_AUTO_SOURCE_BASHRC", &v))
    {
        config.auto_source_bashrc = v;
    }
    if let Some(v) = env_var_nonempty("TERMINAL_HOME_MODE") {
        match TerminalHomeMode::from_env_name(&v) {
            Some(mode) => config.home_mode = mode,
            None => tracing::warn!("Unknown TERMINAL_HOME_MODE '{v}'"),
        }
    }
    if let Some(v) = env_var_nonempty("TERMINAL_SSH_HOST") {
        config.ssh_host = Some(v);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_SSH_PORT") {
        if let Ok(n) = v.parse::<u16>() {
            config.ssh_port = Some(n);
        } else {
            tracing::warn!("TERMINAL_SSH_PORT is not a valid u16: {v}");
        }
    }
    if let Some(v) = env_var_nonempty("TERMINAL_SSH_USER") {
        config.ssh_user = Some(v);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_SSH_KEY_PATH") {
        config.ssh_key_path = Some(v);
    }
}

// ---------------------------------------------------------------------------
// apply_env_overrides
// ---------------------------------------------------------------------------

/// Override configuration fields from environment variables.
///
/// Environment variable mapping:
///   HERMES_MODEL           -> config.model
///   HERMES_PERSONALITY     -> config.personality
///   HERMES_HOME            -> config.home_dir
///   HERMES_MAX_TURNS       -> config.max_turns
///   HERMES_SYSTEM_PROMPT   -> config.system_prompt
///   HERMES_PROXY_HTTP      -> config.proxy.http_proxy
///   HERMES_PROXY_SOCKS     -> config.proxy.socks_proxy
///   HERMES_LLM_API_KEY     -> all llm_providers[*].api_key
///   HERMES_BUDGET_MAX_RESULT_CHARS -> config.budget.max_result_size_chars
///   HERMES_BUDGET_MAX_AGGREGATE_CHARS -> config.budget.max_aggregate_chars
///   HERMES_OPENAI_API_KEY      -> llm_providers["openai"].api_key
///   OPENAI_API_KEY             -> llm_providers["openai"].api_key (legacy fallback)
///   ANTHROPIC_API_KEY          -> llm_providers["anthropic"].api_key
///   OPENROUTER_API_KEY         -> llm_providers["openrouter"].api_key
///   DASHSCOPE_API_KEY          -> llm_providers["qwen"].api_key
///   MOONSHOT_API_KEY           -> llm_providers["kimi"].api_key
///   MINIMAX_API_KEY            -> llm_providers["minimax"].api_key
///   NOUS_API_KEY               -> llm_providers["nous"].api_key
///   COPILOT_GITHUB_TOKEN/GITHUB_COPILOT_TOKEN
///                              -> llm_providers["copilot"].api_key
///   HERMES_BASE_URL            -> all llm_providers[*].base_url
///
/// 另见 [`crate::python_platform_env::apply_python_named_platform_env`]：
/// `WEIXIN_*`、`DINGTALK_*` 等与 Python `gateway/platforms/*.py` 一致的键写入 `platforms`。
pub fn apply_env_overrides(config: &mut GatewayConfig) {
    apply_terminal_env_overrides(&mut config.terminal);
    apply_web_env_overrides(&mut config.web);
    apply_display_env_overrides(&mut config.display);

    if let Ok(v) = std::env::var("HERMES_MODEL") {
        config.model = Some(v);
    }
    if let Ok(v) = std::env::var("HERMES_PERSONALITY") {
        config.personality = Some(v);
    }
    if let Ok(v) = std::env::var("HERMES_HOME") {
        config.home_dir = Some(v);
    }
    if let Ok(v) = std::env::var("HERMES_MAX_TURNS") {
        if let Ok(n) = v.parse::<u32>() {
            config.max_turns = n;
        } else {
            tracing::warn!("HERMES_MAX_TURNS is not a valid u32: {v}");
        }
    }
    if let Ok(v) = std::env::var("HERMES_SYSTEM_PROMPT") {
        config.system_prompt = Some(v);
    }
    if let Some(v) = env_var_nonempty("HERMES_PREFILL_MESSAGES_FILE") {
        config.prefill_messages_file = Some(v);
    }
    if let Ok(v) = std::env::var("HERMES_KANBAN_DISPATCH_IN_GATEWAY") {
        if let Some(parsed) = parse_bool_env("HERMES_KANBAN_DISPATCH_IN_GATEWAY", &v) {
            config.kanban.dispatch_in_gateway = parsed;
        }
    }
    if let Ok(v) = std::env::var("HERMES_AGENT_API_MAX_RETRIES") {
        if let Ok(parsed) = v.parse::<u32>() {
            config.agent.api_max_retries = Some(parsed);
        } else {
            tracing::warn!("HERMES_AGENT_API_MAX_RETRIES is not a valid u32: {v}");
        }
    }
    if env_truthy("HERMES_AGENT_SKIP_CONTEXT_FILES") || env_truthy("HERMES_IGNORE_RULES") {
        config.agent.skip_context_files = true;
    }
    if let Ok(v) = std::env::var("HERMES_ALLOW_PRIVATE_URLS") {
        let normalized = v.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "1" | "true" | "yes" | "on" => config.security.allow_private_urls = true,
            "0" | "false" | "no" | "off" => config.security.allow_private_urls = false,
            _ => tracing::warn!("HERMES_ALLOW_PRIVATE_URLS is not a valid bool-like value: {v}"),
        }
    }
    if let Ok(v) = std::env::var("HERMES_PROXY_HTTP") {
        let proxy = config
            .proxy
            .get_or_insert_with(crate::config::ProxyConfig::default);
        proxy.http_proxy = Some(v);
    }
    if let Ok(v) = std::env::var("HERMES_PROXY_SOCKS") {
        let proxy = config
            .proxy
            .get_or_insert_with(crate::config::ProxyConfig::default);
        proxy.socks_proxy = Some(v);
    }
    if let Ok(v) = std::env::var("HERMES_LLM_API_KEY") {
        if !v.trim().is_empty() {
            for provider in config.llm_providers.values_mut() {
                provider.api_key = Some(v.clone());
            }
        }
    }
    if let Ok(v) = std::env::var("HERMES_BUDGET_MAX_RESULT_CHARS") {
        if let Ok(n) = v.parse::<usize>() {
            config.budget.max_result_size_chars = n;
        }
    }
    if let Ok(v) = std::env::var("HERMES_BUDGET_MAX_AGGREGATE_CHARS") {
        if let Ok(n) = v.parse::<usize>() {
            config.budget.max_aggregate_chars = n;
        }
    }

    // Provider-specific API keys (prefer HERMES_OPENAI_API_KEY over legacy OPENAI_API_KEY).
    let openai_env = std::env::var("HERMES_OPENAI_API_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var("OPENAI_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
        });
    if let Some(v) = openai_env {
        config
            .llm_providers
            .entry("openai".to_string())
            .or_insert_with(LlmProviderConfig::default)
            .api_key = Some(v);
    }
    let mut env_overridden_providers = std::collections::HashSet::new();
    for (env_var, provider_name) in [
        ("ANTHROPIC_API_KEY", "anthropic"),
        ("OPENROUTER_API_KEY", "openrouter"),
        ("HERMES_OPENAI_CODEX_API_KEY", "openai-codex"),
        ("DASHSCOPE_API_KEY", "qwen"),
        ("HERMES_QWEN_OAUTH_API_KEY", "qwen-oauth"),
        ("MOONSHOT_API_KEY", "kimi"),
        ("MINIMAX_API_KEY", "minimax"),
        ("NOUS_API_KEY", "nous"),
        ("GMI_API_KEY", "gmi"),
        ("ARCEEAI_API_KEY", "arcee"),
        ("ARCEE_API_KEY", "arcee"),
        ("XIAOMI_API_KEY", "xiaomi"),
        ("TOKENHUB_API_KEY", "tencent-tokenhub"),
        ("COPILOT_GITHUB_TOKEN", "copilot"),
        ("GITHUB_COPILOT_TOKEN", "copilot"),
        ("OLLAMA_LOCAL_API_KEY", "ollama-local"),
        ("LLAMA_CPP_API_KEY", "llama-cpp"),
        ("VLLM_API_KEY", "vllm"),
        ("MLX_API_KEY", "mlx"),
        ("APPLE_ANE_API_KEY", "apple-ane"),
        ("SGLANG_API_KEY", "sglang"),
        ("TGI_API_KEY", "tgi"),
        ("LMSTUDIO_API_KEY", "lmstudio"),
        ("LMDEPLOY_API_KEY", "lmdeploy"),
        ("LOCALAI_API_KEY", "localai"),
        ("KOBOLDCPP_API_KEY", "koboldcpp"),
        ("TEXT_GENERATION_WEBUI_API_KEY", "text-generation-webui"),
        ("TABBYAPI_API_KEY", "tabbyapi"),
    ] {
        if let Ok(v) = std::env::var(env_var) {
            if v.trim().is_empty() {
                continue;
            }
            if !env_overridden_providers.insert(provider_name) {
                continue;
            }
            config
                .llm_providers
                .entry(provider_name.to_string())
                .or_insert_with(LlmProviderConfig::default)
                .api_key = Some(v);
        }
    }
    for (env_var, provider_name) in [
        ("GMI_BASE_URL", "gmi"),
        ("ARCEE_BASE_URL", "arcee"),
        ("XIAOMI_BASE_URL", "xiaomi"),
        ("TOKENHUB_BASE_URL", "tencent-tokenhub"),
        ("OLLAMA_BASE_URL", "ollama-local"),
        ("LLAMA_CPP_BASE_URL", "llama-cpp"),
        ("VLLM_BASE_URL", "vllm"),
        ("MLX_BASE_URL", "mlx"),
        ("APPLE_ANE_BASE_URL", "apple-ane"),
        ("SGLANG_BASE_URL", "sglang"),
        ("TGI_BASE_URL", "tgi"),
        ("LMSTUDIO_BASE_URL", "lmstudio"),
        ("LMDEPLOY_BASE_URL", "lmdeploy"),
        ("LOCALAI_BASE_URL", "localai"),
        ("KOBOLDCPP_BASE_URL", "koboldcpp"),
        ("TEXT_GENERATION_WEBUI_BASE_URL", "text-generation-webui"),
        ("TABBYAPI_BASE_URL", "tabbyapi"),
    ] {
        if let Ok(v) = std::env::var(env_var) {
            if v.trim().is_empty() {
                continue;
            }
            config
                .llm_providers
                .entry(provider_name.to_string())
                .or_insert_with(LlmProviderConfig::default)
                .base_url = Some(v);
        }
    }

    if let Ok(v) = std::env::var("HERMES_BASE_URL") {
        if !v.trim().is_empty() {
            for provider in config.llm_providers.values_mut() {
                provider.base_url = Some(v.clone());
            }
        }
    }

    crate::python_platform_env::apply_python_named_platform_env(config);
}

// ---------------------------------------------------------------------------
// validate_config
// ---------------------------------------------------------------------------

/// Validate a fully-loaded configuration.
///
/// Checks:
/// - max_turns > 0
/// - SessionResetPolicy::Daily at_hour in 0..=23
/// - All LLM providers with an api_key set have a non-empty value
/// - Terminal timeout > 0
pub fn validate_config(config: &GatewayConfig) -> Result<(), ConfigError> {
    if config.max_turns == 0 {
        return Err(ConfigError::ValidationError(
            "max_turns must be greater than 0".into(),
        ));
    }

    if config.terminal.timeout == 0 {
        return Err(ConfigError::ValidationError(
            "terminal.timeout must be greater than 0".into(),
        ));
    }

    // Validate session reset policy (clamping already done during merge)
    let _ = config.session.reset_policy.validate();

    for (name, provider) in &config.llm_providers {
        if let Some(key) = &provider.api_key {
            if key.trim().is_empty() {
                return Err(ConfigError::ValidationError(format!(
                    "llm_providers.{name}.api_key must not be empty"
                )));
            }
        }
        if let Some(api_mode) = &provider.api_mode {
            normalize_provider_api_mode(api_mode).map_err(|_| {
                ConfigError::ValidationError(format!(
                    "llm_providers.{name}.api_mode must be one of chat_completions, anthropic_messages, codex_responses, bedrock_converse"
                ))
            })?;
        }
        if let Some(timeout) = provider.request_timeout_seconds {
            if !timeout.is_finite() || timeout <= 0.0 {
                return Err(ConfigError::ValidationError(format!(
                    "llm_providers.{name}.request_timeout_seconds must be a positive finite number"
                )));
            }
        }
        if matches!(provider.max_tokens, Some(0)) {
            return Err(ConfigError::ValidationError(format!(
                "llm_providers.{name}.max_tokens must be a positive integer"
            )));
        }
        if provider.models.iter().any(|model| model.trim().is_empty()) {
            return Err(ConfigError::ValidationError(format!(
                "llm_providers.{name}.models must not contain empty model ids"
            )));
        }
    }

    if let Some(api_key) = &config.delegation.api_key {
        if api_key.trim().is_empty() {
            return Err(ConfigError::ValidationError(
                "delegation.api_key must not be empty".into(),
            ));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
