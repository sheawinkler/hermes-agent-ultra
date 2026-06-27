// ---------------------------------------------------------------------------
// Helper: bridge hermes_tools::ToolRegistry → agent_loop::ToolRegistry
// ---------------------------------------------------------------------------

fn sorted_tool_schema_names(schemas: &[ToolSchema]) -> Vec<String> {
    let mut names = schemas
        .iter()
        .map(|schema| schema.name.clone())
        .collect::<Vec<_>>();
    names.sort();
    names
}

pub fn bridge_tool_registry(tools: &ToolRegistry) -> AgentToolRegistry {
    bridge_tool_registry_excluding(tools, &[])
}

fn bridge_tool_registry_excluding(tools: &ToolRegistry, excluded: &[&str]) -> AgentToolRegistry {
    let mut agent_registry = AgentToolRegistry::new();
    for schema in tools.get_definitions() {
        if excluded.iter().any(|name| schema.name == *name) {
            continue;
        }
        let name = schema.name.clone();
        let tools_clone = tools.clone();
        agent_registry.register(
            name.clone(),
            schema,
            Arc::new(
                move |params: Value| -> Result<String, hermes_core::ToolError> {
                    Ok(tools_clone.dispatch(&name, params))
                },
            ),
        );
    }
    agent_registry
}

/// Build a scheduler for long-running CLI runtimes from the active provider and
/// registered tools. This keeps scheduled jobs equivalent to explicit
/// `hermes cron run` instead of completing through the minimal CRUD scheduler.
pub fn build_runtime_cron_scheduler(
    config: &GatewayConfig,
    model: &str,
    data_dir: PathBuf,
    tools: &ToolRegistry,
) -> CronScheduler {
    let persistence = Arc::new(FileJobPersistence::with_dir(data_dir));
    let model = select_startup_model_with_fallback_and_auth_resolver(
        config,
        model,
        Some(&provider_oauth_token_from_auth_state),
    )
    .selected_model;
    let provider = build_provider(config, &model);
    let runner = Arc::new(CronRunner::new(
        provider,
        Arc::new(bridge_tool_registry_excluding(tools, &["cronjob"])),
    ));
    CronScheduler::new(persistence, runner)
}

fn resolve_startup_model(config: &GatewayConfig, configured_model: &str) -> String {
    let raw = configured_model.trim();
    if raw.is_empty() {
        return "gpt-5.5".to_string();
    }
    if raw.contains(':') {
        return raw.to_string();
    }

    // If config.model is a provider slug (e.g. "nous"), prefer that provider's
    // configured runtime model instead of sending the bare slug as a model id.
    if let Some((provider, provider_cfg)) = config
        .llm_providers
        .iter()
        .find(|(provider, _)| provider.eq_ignore_ascii_case(raw))
    {
        if let Some(runtime_model) = provider_cfg
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .filter(|m| !m.eq_ignore_ascii_case(provider))
        {
            if runtime_model.contains(':') {
                return runtime_model.to_string();
            }
            return format!("{provider}:{runtime_model}");
        }
    }

    raw.to_string()
}

fn sync_runtime_model_env(config: &GatewayConfig, provider_model: &str) {
    let model = provider_model.trim();
    if model.is_empty() {
        return;
    }
    let (provider, _) = resolve_provider_and_model(config, model);
    std::env::set_var("HERMES_MODEL", model);
    std::env::set_var("HERMES_INFERENCE_MODEL", model);
    std::env::set_var("HERMES_INFERENCE_PROVIDER", provider.as_str());
    std::env::set_var("HERMES_TUI_PROVIDER", provider.as_str());
}

fn default_mouse_enabled() -> bool {
    match std::env::var("HERMES_TUI_MOUSE") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        Err(_) => false,
    }
}

fn pet_settings_path() -> PathBuf {
    hermes_home_dir().join("pet.json")
}

fn parse_bool_env(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn default_pet_settings() -> PetSettings {
    let mut settings = PetSettings::default();
    if let Ok(raw) = std::env::var("HERMES_PET") {
        if let Some(enabled) = parse_bool_env(&raw) {
            settings.enabled = enabled;
        }
    }
    if let Ok(raw) = std::env::var("HERMES_PET_SPECIES") {
        settings.species = raw;
    }
    if let Ok(raw) = std::env::var("HERMES_PET_MOOD") {
        settings.mood = raw;
    }
    if let Ok(raw) = std::env::var("HERMES_PET_DOCK") {
        settings.dock = if raw.trim().eq_ignore_ascii_case("left") {
            PetDock::Left
        } else {
            PetDock::Right
        };
    }
    if let Ok(raw) = std::env::var("HERMES_PET_TICK_MS") {
        if let Ok(value) = raw.trim().parse::<u64>() {
            settings.tick_ms = value;
        }
    }
    settings.normalized()
}

fn load_pet_settings() -> PetSettings {
    let path = pet_settings_path();
    let from_file = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<PetSettings>(&raw).ok())
        .map(PetSettings::normalized);
    from_file.unwrap_or_else(default_pet_settings)
}

fn persist_pet_settings(settings: &PetSettings) -> Result<(), AgentError> {
    let path = pet_settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            AgentError::Io(format!(
                "Failed to create pet settings directory '{}': {}",
                parent.display(),
                e
            ))
        })?;
    }
    let body = serde_json::to_string_pretty(settings)
        .map_err(|e| AgentError::Config(format!("pet settings serialization failed: {e}")))?;
    std::fs::write(&path, format!("{body}\n")).map_err(|e| {
        AgentError::Io(format!(
            "Failed to persist pet settings '{}': {}",
            path.display(),
            e
        ))
    })
}

fn default_rtk_raw_mode() -> bool {
    match std::env::var("HERMES_RTK_RAW") {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        ),
        Err(_) => false,
    }
}

pub fn provider_oauth_token_from_auth_state(provider: &str) -> Option<String> {
    let provider_key = match normalize_runtime_provider_name(provider).as_str() {
        "openai" => "openai",
        "openai-codex" | "codex" => "openai-codex",
        _ => return None,
    };
    let state = crate::auth::read_provider_auth_state(provider_key)
        .ok()
        .flatten()?;
    state
        .get("tokens")
        .and_then(|tokens| tokens.get("access_token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

pub fn build_provider(config: &GatewayConfig, model: &str) -> Arc<dyn LlmProvider> {
    hermes_provider_runtime::build_provider_with_auth_resolver(
        config,
        model,
        Some(&provider_oauth_token_from_auth_state),
    )
}
