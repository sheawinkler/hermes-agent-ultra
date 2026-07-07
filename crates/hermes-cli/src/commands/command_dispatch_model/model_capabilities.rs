#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelSwitchRequest {
    PickProviderThenModel,
    PickModelFromProvider(String),
    SetDirect(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ModelCapabilityRequirements {
    require_tools: bool,
    require_vision: bool,
    require_reasoning: bool,
    require_long_context: bool,
    min_context_window: Option<u64>,
}

impl ModelCapabilityRequirements {
    const LONG_CONTEXT_DEFAULT: u64 = 128_000;

    fn is_empty(self) -> bool {
        !self.require_tools
            && !self.require_vision
            && !self.require_reasoning
            && !self.require_long_context
            && self.min_context_window.is_none()
    }

    fn effective_min_context(self) -> Option<u64> {
        match (self.require_long_context, self.min_context_window) {
            (true, Some(value)) => Some(value.max(Self::LONG_CONTEXT_DEFAULT)),
            (true, None) => Some(Self::LONG_CONTEXT_DEFAULT),
            (false, value) => value,
        }
    }

    fn summary(self) -> String {
        let mut parts = Vec::new();
        if self.require_tools {
            parts.push("tools".to_string());
        }
        if self.require_vision {
            parts.push("vision".to_string());
        }
        if self.require_reasoning {
            parts.push("reasoning".to_string());
        }
        if let Some(min_ctx) = self.effective_min_context() {
            parts.push(format!("context>={min_ctx}"));
        }
        if parts.is_empty() {
            "none".to_string()
        } else {
            parts.join(", ")
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ResolvedModelCapabilities {
    supports_tools: bool,
    supports_vision: bool,
    supports_reasoning: bool,
    context_window: u64,
}

const OPENAI_CODEX_OAUTH_CONTEXT_WINDOW: u64 = 272_000;

fn model_context_override_for_config(
    provider: &str,
    model_id: &str,
    config: &GatewayConfig,
) -> Option<u64> {
    let (_, provider_cfg) = crate::model_switch::configured_llm_provider(config, provider)?;
    let model_id = model_id.trim();
    let provider_model = format!("{}:{}", provider.trim(), model_id);
    provider_cfg
        .model_context_windows
        .iter()
        .find_map(|(key, value)| {
            let key = key.trim();
            if key.eq_ignore_ascii_case(model_id) || key.eq_ignore_ascii_case(&provider_model) {
                Some((*value).max(1))
            } else {
                None
            }
        })
}

fn provider_is_openai_codex_oauth(provider: &str, config: Option<&GatewayConfig>) -> bool {
    let canonical = canonical_provider_id(provider);
    if canonical == "openai-codex" {
        return true;
    }
    let Some(config) = config else {
        return false;
    };
    crate::model_switch::configured_llm_provider(config, provider)
        .and_then(|(_, provider_cfg)| provider_cfg.base_url.as_deref())
        .map(|base_url| {
            base_url
                .trim()
                .trim_end_matches('/')
                .eq_ignore_ascii_case("https://chatgpt.com/backend-api/codex")
        })
        .unwrap_or(false)
}

fn resolve_display_context_window(
    provider: &str,
    model_id: &str,
    config: Option<&GatewayConfig>,
    models_dev_context_window: Option<u64>,
) -> u64 {
    if provider_is_openai_codex_oauth(provider, config) {
        return OPENAI_CODEX_OAUTH_CONTEXT_WINDOW;
    }
    if let Some(config) = config {
        if let Some(value) = model_context_override_for_config(provider, model_id, config) {
            return value;
        }
    }
    if let Some(value) = models_dev_context_window.filter(|value| *value > 0) {
        return value;
    }
    let provider_model = format!("{}:{}", provider.trim(), model_id.trim());
    get_model_context_length(&provider_model)
}

fn normalize_model_capability_name(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "tools" | "tool" | "function-calling" | "function_calling" => Some("tools"),
        "vision" | "image" | "images" => Some("vision"),
        "reasoning" | "reason" => Some("reasoning"),
        "long-context" | "long_context" | "longcontext" | "context" => Some("long-context"),
        _ => None,
    }
}

fn apply_model_capability_token(
    requirements: &mut ModelCapabilityRequirements,
    token: &str,
) -> Result<(), AgentError> {
    let Some(normalized) = normalize_model_capability_name(token) else {
        return Err(AgentError::Config(format!(
            "Unknown model capability '{}' (expected one of: tools, vision, reasoning, long-context).",
            token
        )));
    };
    match normalized {
        "tools" => requirements.require_tools = true,
        "vision" => requirements.require_vision = true,
        "reasoning" => requirements.require_reasoning = true,
        "long-context" => requirements.require_long_context = true,
        _ => {}
    }
    Ok(())
}

fn parse_model_command_args(
    args: &[&str],
) -> Result<(Vec<String>, ModelCapabilityRequirements, Option<String>), AgentError> {
    let mut requirements = ModelCapabilityRequirements::default();
    let mut positional = Vec::new();
    let mut provider_override: Option<String> = None;
    let mut idx = 0usize;

    while idx < args.len() {
        let token = args[idx].trim();
        if token.is_empty() {
            idx += 1;
            continue;
        }

        if matches!(
            token.to_ascii_lowercase().as_str(),
            "--vision" | "--tools" | "--reasoning" | "--long-context" | "--long_context"
        ) {
            apply_model_capability_token(&mut requirements, token.trim_start_matches('-'))?;
            idx += 1;
            continue;
        }

        if matches!(
            token.to_ascii_lowercase().as_str(),
            "--cap" | "--caps" | "--require" | "--requires"
        ) {
            let value = args
                .get(idx + 1)
                .ok_or_else(|| AgentError::Config(format!("{} requires a value.", token)))?;
            for raw in value.split(',') {
                let candidate = raw.trim();
                if candidate.is_empty() {
                    continue;
                }
                apply_model_capability_token(&mut requirements, candidate)?;
            }
            idx += 2;
            continue;
        }

        if token.eq_ignore_ascii_case("--provider") || token.eq_ignore_ascii_case("-p") {
            let provider = args
                .get(idx + 1)
                .ok_or_else(|| AgentError::Config(format!("{} requires a provider slug.", token)))?
                .trim();
            if provider.is_empty() {
                return Err(AgentError::Config(
                    "provider override cannot be empty.".to_string(),
                ));
            }
            provider_override = Some(provider.to_ascii_lowercase());
            idx += 2;
            continue;
        }

        if token.eq_ignore_ascii_case("--min-context")
            || token.eq_ignore_ascii_case("--min_context")
        {
            let value = args
                .get(idx + 1)
                .ok_or_else(|| {
                    AgentError::Config("--min-context requires a numeric value.".into())
                })?
                .trim();
            let parsed = value.parse::<u64>().map_err(|_| {
                AgentError::Config(format!(
                    "Invalid --min-context value '{}'; expected integer token count.",
                    value
                ))
            })?;
            requirements.min_context_window = Some(parsed);
            idx += 2;
            continue;
        }

        positional.push(token.to_string());
        idx += 1;
    }

    Ok((positional, requirements, provider_override))
}

fn resolve_model_capabilities(
    provider: &str,
    model_id: &str,
    client: &hermes_intelligence::models_dev::ModelsDevClient,
    config: Option<&GatewayConfig>,
) -> ResolvedModelCapabilities {
    let provider_model = format!("{}:{}", provider.trim(), model_id.trim());
    let models_dev_caps = client.capabilities(provider, model_id);
    let info = get_model_info(&provider_model).or_else(|| get_model_info(model_id));
    ResolvedModelCapabilities {
        supports_tools: models_dev_caps
            .as_ref()
            .map(|entry| entry.supports_tools)
            .or_else(|| info.as_ref().map(|entry| entry.supports_tools))
            .unwrap_or(true),
        supports_vision: models_dev_caps
            .as_ref()
            .map(|entry| entry.supports_vision)
            .or_else(|| info.as_ref().map(|entry| entry.supports_vision))
            .unwrap_or(false),
        supports_reasoning: models_dev_caps
            .as_ref()
            .map(|entry| entry.supports_reasoning)
            .or_else(|| info.as_ref().map(|entry| entry.supports_reasoning))
            .unwrap_or(false),
        context_window: resolve_display_context_window(
            provider,
            model_id,
            config,
            models_dev_caps.map(|entry| entry.context_window),
        ),
    }
}

fn model_meets_requirements(
    capabilities: ResolvedModelCapabilities,
    requirements: ModelCapabilityRequirements,
) -> bool {
    if requirements.require_tools && !capabilities.supports_tools {
        return false;
    }
    if requirements.require_vision && !capabilities.supports_vision {
        return false;
    }
    if requirements.require_reasoning && !capabilities.supports_reasoning {
        return false;
    }
    if let Some(min_context) = requirements.effective_min_context() {
        if capabilities.context_window < min_context {
            return false;
        }
    }
    true
}

fn unmet_model_requirements(
    capabilities: ResolvedModelCapabilities,
    requirements: ModelCapabilityRequirements,
) -> Vec<String> {
    let mut missing = Vec::new();
    if requirements.require_tools && !capabilities.supports_tools {
        missing.push("tools".to_string());
    }
    if requirements.require_vision && !capabilities.supports_vision {
        missing.push("vision".to_string());
    }
    if requirements.require_reasoning && !capabilities.supports_reasoning {
        missing.push("reasoning".to_string());
    }
    if let Some(min_context) = requirements.effective_min_context() {
        if capabilities.context_window < min_context {
            missing.push(format!(
                "context>={} (actual={})",
                min_context, capabilities.context_window
            ));
        }
    }
    missing
}

async fn handle_model_explain_command(
    app: &mut App,
    args: &[&str],
    strict_why_not: bool,
) -> Result<CommandResult, AgentError> {
    let (mut positional, requirements, provider_override) = parse_model_command_args(args)?;
    if let Some(provider) = provider_override {
        if positional.is_empty() {
            positional.push(provider);
        } else if let Some(first) = positional.first().cloned() {
            let model_id = first
                .split_once(':')
                .map(|(_, rhs)| rhs.to_string())
                .unwrap_or(first);
            positional[0] = format!("{}:{}", provider, model_id.trim());
        }
    }
    let target = if positional.is_empty() {
        app.current_model.clone()
    } else {
        normalize_model_target(&app.current_model, &positional[0])?
    };
    let (guarded, remap_note) =
        guard_provider_model_selection_for_config(&target, &app.config).await?;
    let (provider, model_id) = split_provider_model(&guarded);
    let client = default_client();
    client.fetch(false).await;
    let capabilities = resolve_model_capabilities(provider, model_id, client, Some(&app.config));

    let mut out = String::new();
    let _ = writeln!(out, "Model capability report");
    let _ = writeln!(out, "-----------------------");
    let _ = writeln!(out, "target: {}", guarded);
    let _ = writeln!(out, "provider: {}", provider.trim());
    let _ = writeln!(out, "tools: {}", capabilities.supports_tools);
    let _ = writeln!(out, "vision: {}", capabilities.supports_vision);
    let _ = writeln!(out, "reasoning: {}", capabilities.supports_reasoning);
    let _ = writeln!(out, "context_window: {}", capabilities.context_window);
    let _ = writeln!(
        out,
        "acp_multimodal_parts: {}",
        if capabilities.supports_vision {
            "supported"
        } else {
            "text-only fallback"
        }
    );
    if let Some(note) = remap_note.as_deref() {
        let _ = writeln!(out, "catalog_guard: {}", note);
    }

    if !requirements.is_empty() {
        let unmet = unmet_model_requirements(capabilities, requirements);
        if unmet.is_empty() {
            let _ = writeln!(out, "requirements: satisfied ({})", requirements.summary());
        } else {
            let _ = writeln!(out, "requirements: FAILED ({})", requirements.summary());
            let _ = writeln!(out, "missing: {}", unmet.join(", "));
            let catalog = provider_model_ids_for_config(provider, &app.config).await;
            let alternatives: Vec<String> = catalog
                .into_iter()
                .filter(|candidate| {
                    model_meets_requirements(
                    resolve_model_capabilities(provider, candidate, client, Some(&app.config)),
                        requirements,
                    )
                })
                .take(8)
                .collect();
            if alternatives.is_empty() {
                let _ = writeln!(out, "alternatives: none in provider catalog");
            } else {
                let _ = writeln!(
                    out,
                    "alternatives: {}",
                    alternatives
                        .iter()
                        .map(|m| format!("{}:{}", provider, m))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if strict_why_not {
                return Err(AgentError::Config(out.trim_end().to_string()));
            }
        }
    } else if strict_why_not {
        let _ = writeln!(
            out,
            "why-not mode requires constraints. Example: `/model why-not --cap tools,reasoning --min-context 200000`"
        );
    }

    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn parse_model_switch_request<S: AsRef<str>>(
    args: &[&str],
    known_providers: &[S],
) -> ModelSwitchRequest {
    if args.is_empty() {
        return ModelSwitchRequest::PickProviderThenModel;
    }
    let raw = args.join(" ");
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return ModelSwitchRequest::PickProviderThenModel;
    }
    if trimmed.contains(':') {
        return ModelSwitchRequest::SetDirect(trimmed.to_string());
    }
    if known_providers
        .iter()
        .any(|p| p.as_ref().eq_ignore_ascii_case(trimmed))
    {
        return ModelSwitchRequest::PickModelFromProvider(trimmed.to_ascii_lowercase());
    }
    ModelSwitchRequest::SetDirect(trimmed.to_string())
}

fn model_catalog_guard_enabled() -> bool {
    !matches!(
        std::env::var("HERMES_MODEL_CATALOG_GUARD")
            .ok()
            .as_deref()
            .map(|v| v.trim().to_ascii_lowercase()),
        Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
    )
}

async fn guard_provider_model_selection_for_config(
    provider_model: &str,
    config: &GatewayConfig,
) -> Result<(String, Option<String>), AgentError> {
    if !model_catalog_guard_enabled() {
        return Ok((provider_model.to_string(), None));
    }

    let (provider, model_id) = split_provider_model(provider_model);
    let provider = provider.trim().to_ascii_lowercase();
    if provider.is_empty() {
        return Ok((provider_model.to_string(), None));
    }
    if matches!(provider.as_str(), "openai-codex" | "codex")
        || (provider == "openai" && model_id.to_ascii_lowercase().contains("codex"))
    {
        return Ok((
            provider_model.to_string(),
            Some(format!(
                "Catalog guard soft-accepted unlisted Codex model `{}`.",
                model_id.trim()
            )),
        ));
    }
    if !provider_slugs_for_config(config)
        .iter()
        .any(|slug| slug.eq_ignore_ascii_case(&provider))
    {
        return Ok((provider_model.to_string(), None));
    }

    let catalog = provider_model_ids_for_config(&provider, config).await;
    if catalog.is_empty() {
        return Ok((provider_model.to_string(), None));
    }
    let Some(candidate) = resolve_catalog_model_candidate(model_id, &catalog) else {
        let suggestions = rank_catalog_model_candidates(model_id, &catalog, 5);
        return Err(AgentError::Config(format!(
            "Model '{}' is not available for provider '{}'. Close matches: {}. Use `/model {}` to pick a valid catalog entry.",
            model_id.trim(),
            provider,
            if suggestions.is_empty() {
                "(none)".to_string()
            } else {
                suggestions.join(", ")
            },
            provider,
        )));
    };
    let guarded = format!("{}:{}", provider, candidate);
    if guarded.eq_ignore_ascii_case(provider_model) {
        return Ok((provider_model.to_string(), None));
    }
    Ok((
        guarded.clone(),
        Some(format!(
            "Model catalog guard remapped `{}` -> `{}` based on provider catalog.",
            provider_model, guarded
        )),
    ))
}

fn normalize_model_target(current_model: &str, raw: &str) -> Result<String, AgentError> {
    let trimmed = raw.trim();
    if trimmed.contains(':') {
        return normalize_provider_model(trimmed);
    }
    let (provider, _) = split_provider_model(current_model);
    normalize_provider_model(&format!("{}:{}", provider.trim(), trimmed))
}

/// Run `curses_select` safely from both plain CLI and active TUI sessions.
///
/// In TUI mode, use an embedded selector that does not toggle terminal mode.
fn run_model_picker_select(
    app: &App,
    title: &str,
    items: &[String],
    initial_index: usize,
) -> crate::SelectResult {
    if app.stream_handle.is_some() {
        crate::curses_select_embedded(title, items, initial_index)
    } else {
        crate::curses_select(title, items, initial_index)
    }
}

fn persist_current_model_selection(app: &App) -> Result<PathBuf, AgentError> {
    let cfg_path = app.state_root.join("config.yaml");
    if config_uses_nested_model_block(&cfg_path)? {
        let (provider, model_id) = split_provider_model(&app.current_model);
        let model_id = model_id.trim();
        let provider = provider.trim();
        set_user_config_value(&app.state_root, "model.default", model_id)
            .map_err(|e| AgentError::Config(e.to_string()))?;
        if !provider.is_empty() {
            set_user_config_value(&app.state_root, "model.provider", provider)
                .map_err(|e| AgentError::Config(e.to_string()))?;
        }
        if let Some(base_url) = app
            .config
            .llm_providers
            .get(provider)
            .and_then(|cfg| cfg.base_url.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            set_user_config_value(&app.state_root, "model.base_url", base_url)
                .map_err(|e| AgentError::Config(e.to_string()))?;
        }
    } else {
        set_user_config_value(&app.state_root, "model", &app.current_model)
            .map_err(|e| AgentError::Config(e.to_string()))?;
    }
    Ok(cfg_path)
}

fn config_uses_nested_model_block(path: &Path) -> Result<bool, AgentError> {
    if !path.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    if text.trim().is_empty() {
        return Ok(false);
    }
    let value: serde_yaml::Value =
        serde_yaml::from_str(&text).map_err(|e| AgentError::Config(e.to_string()))?;
    let Some(root) = value.as_mapping() else {
        return Ok(false);
    };
    Ok(root
        .get(serde_yaml::Value::String("model".to_string()))
        .is_some_and(serde_yaml::Value::is_mapping))
}

fn format_model_persistence_note(app: &App) -> String {
    let mut note = match persist_current_model_selection(app) {
        Ok(path) => format!("Persisted default model in {}.", path.display()),
        Err(err) => format!(
            "Warning: switched for this session, but failed to persist default model: {}",
            err
        ),
    };
    let main_provider = provider_slug_from_provider_model(&app.current_model);
    let stale_aux = app
        .config
        .stale_auxiliary_assignments_for_main_provider(main_provider);
    if let Some(warning) = format_stale_auxiliary_warning(main_provider, &stale_aux) {
        note.push('\n');
        note.push_str(&warning);
    }
    note
}

fn try_switch_model_or_emit_failure(app: &mut App, provider_model: &str) -> bool {
    let previous_model = app.current_model.clone();
    match app.try_switch_model(provider_model) {
        Ok(()) => true,
        Err(err) => {
            emit_command_output(
                app,
                format!(
                    "Model switch to {} failed ({}); staying on {}.",
                    provider_model, err, previous_model
                ),
            );
            false
        }
    }
}

async fn pick_model_for_provider(
    app: &mut App,
    provider: &str,
    current_model: &str,
    requirements: ModelCapabilityRequirements,
) -> Result<bool, AgentError> {
    let models = provider_model_ids_for_config(provider, &app.config).await;
    if models.is_empty() {
        emit_command_output(
            app,
            format!("No models available for provider '{}'.", provider),
        );
        return Ok(false);
    }

    let normalized_provider = provider.trim().to_ascii_lowercase();
    let mut filtered_models = models.clone();
    if !requirements.is_empty() {
        let client = default_client();
        client.fetch(false).await;
        filtered_models = models
            .iter()
            .filter(|model_id| {
                model_meets_requirements(
                    resolve_model_capabilities(
                        &normalized_provider,
                        model_id,
                        client,
                        Some(&app.config),
                    ),
                    requirements,
                )
            })
            .cloned()
            .collect();
    }

    if filtered_models.is_empty() {
        emit_command_output(
            app,
            format!(
                "No models for provider '{}' satisfy required capabilities: {}.",
                provider,
                requirements.summary()
            ),
        );
        return Ok(false);
    }

    let (_, current_model_id) = split_provider_model(current_model);
    let default_index = filtered_models
        .iter()
        .position(|m| m.eq_ignore_ascii_case(current_model_id))
        .unwrap_or(0);
    let labels: Vec<String> = filtered_models.clone();
    let title = format!("Select {} model ({} available)", provider, labels.len());
    let pick = run_model_picker_select(app, &title, &labels, default_index);
    if !pick.confirmed || pick.index >= filtered_models.len() {
        emit_command_output(app, "Model switch cancelled.");
        return Ok(false);
    }
    let provider_model = format!("{}:{}", provider, filtered_models[pick.index].trim());
    let (guarded, note) =
        guard_provider_model_selection_for_config(&provider_model, &app.config).await?;
    let warning = app.model_switch_preflight_warning(&guarded);
    if !try_switch_model_or_emit_failure(app, &guarded) {
        return Ok(false);
    }
    let mut msg = format!("Model switched to: {}", guarded);
    if let Some(n) = note {
        msg.push('\n');
        msg.push_str(&n);
    }
    msg.push('\n');
    msg.push_str(&format_model_persistence_note(app));
    if let Some(warning) = warning {
        msg.push('\n');
        msg.push_str(&warning);
    }
    emit_command_output(app, msg);
    Ok(true)
}

fn parse_failover_chain(raw: &str) -> Result<Vec<String>, AgentError> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for token in raw.split(',') {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = normalize_provider_model(trimmed)?;
        let key = normalized.to_ascii_lowercase();
        if seen.insert(key) {
            out.push(normalized);
        }
    }
    Ok(out)
}

fn read_failover_chain_from_env() -> Vec<String> {
    if let Ok(raw) = std::env::var("HERMES_FALLBACK_MODELS") {
        let parsed: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string)
            .collect();
        if !parsed.is_empty() {
            return parsed;
        }
    }
    if let Ok(raw) = std::env::var("HERMES_FALLBACK_MODEL") {
        let value = raw.trim();
        if !value.is_empty() {
            return vec![value.to_string()];
        }
    }
    Vec::new()
}

fn handle_model_failover_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match action.as_str() {
        "status" | "show" => {
            let chain_items = read_failover_chain_from_env();
            let fallback = chain_items.first().map(|s| s.as_str()).unwrap_or("(none)");
            let chain = if chain_items.is_empty() {
                "(none)".to_string()
            } else {
                chain_items.join(", ")
            };
            emit_command_output(
                app,
                format!(
                    "Failover fabric\nprimary_fallback: {}\nchain: {}\nusage: `/model failover set provider:model[,provider:model...]` or `/model failover clear`",
                    fallback, chain
                ),
            );
        }
        "clear" | "reset" => {
            std::env::remove_var("HERMES_FALLBACK_MODEL");
            std::env::remove_var("HERMES_FALLBACK_MODELS");
            let current = app.current_model.clone();
            app.switch_model(&current);
            emit_command_output(app, "Cleared retry failover chain.");
        }
        "set" => {
            let raw = args
                .get(1)
                .ok_or_else(|| {
                    AgentError::Config(
                        "Usage: /model failover set provider:model[,provider:model...]".to_string(),
                    )
                })?
                .trim();
            let chain = parse_failover_chain(raw)?;
            if chain.is_empty() {
                return Err(AgentError::Config(
                    "Failover chain cannot be empty.".to_string(),
                ));
            }
            std::env::set_var("HERMES_FALLBACK_MODELS", chain.join(","));
            if let Some(first) = chain.first() {
                std::env::set_var("HERMES_FALLBACK_MODEL", first);
            }
            let current = app.current_model.clone();
            app.switch_model(&current);
            emit_command_output(app, format!("Failover chain set: {}", chain.join(", ")));
        }
        _ => {
            emit_command_output(
                app,
                "Usage: /model failover [status|set provider:model[,provider:model...]|clear]",
            );
        }
    }
    Ok(CommandResult::Handled)
}
