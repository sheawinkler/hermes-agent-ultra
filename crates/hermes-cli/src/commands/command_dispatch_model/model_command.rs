async fn handle_model_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if let Some(sub) = args.first().map(|v| v.trim()) {
        if sub.eq_ignore_ascii_case("failover") {
            return handle_model_failover_command(app, &args[1..]);
        }
        if sub.eq_ignore_ascii_case("backend") {
            return handle_model_backend_command(app, &args[1..]);
        }
        if sub.eq_ignore_ascii_case("harness") {
            return handle_model_harness_command(app, &args[1..]).await;
        }
        if sub.eq_ignore_ascii_case("refresh") {
            return handle_model_refresh_command(app, &args[1..]).await;
        }
        if sub.eq_ignore_ascii_case("explain") {
            return handle_model_explain_command(app, &args[1..], false).await;
        }
        if sub.eq_ignore_ascii_case("why-not")
            || sub.eq_ignore_ascii_case("whynot")
            || sub.eq_ignore_ascii_case("diagnose")
        {
            return handle_model_explain_command(app, &args[1..], true).await;
        }
    }

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
    let positional_refs: Vec<&str> = positional.iter().map(String::as_str).collect();
    let known_providers = provider_slugs_for_config(&app.config);
    match parse_model_switch_request(&positional_refs, &known_providers) {
        ModelSwitchRequest::SetDirect(raw) => {
            let provider_model = normalize_model_target(&app.current_model, &raw)?;
            let (guarded, note) =
                guard_provider_model_selection_for_config(&provider_model, &app.config).await?;
            if !requirements.is_empty() {
                let (provider, model_id) = split_provider_model(&guarded);
                let client = default_client();
                client.fetch(false).await;
                let caps = resolve_model_capabilities(provider, model_id, client, Some(&app.config));
                if !model_meets_requirements(caps, requirements) {
                    return Err(AgentError::Config(format!(
                        "Requested model '{}' does not satisfy required capabilities: {}.",
                        guarded,
                        requirements.summary()
                    )));
                }
            }
            let warning = app.model_switch_preflight_warning(&guarded);
            if !try_switch_model_or_emit_failure(app, &guarded) {
                return Ok(CommandResult::Handled);
            }
            let mut msg = format!("Model switched to: {}", guarded);
            if let Some(n) = note {
                msg.push('\n');
                msg.push_str(&n);
            }
            if !requirements.is_empty() {
                msg.push('\n');
                msg.push_str(&format!(
                    "Capability constraints satisfied: {}.",
                    requirements.summary()
                ));
            }
            msg.push('\n');
            msg.push_str(&format_model_persistence_note(app));
            if let Some(warning) = warning {
                msg.push('\n');
                msg.push_str(&warning);
            }
            emit_command_output(app, msg);
        }
        ModelSwitchRequest::PickModelFromProvider(provider) => {
            let current_model = app.current_model.clone();
            pick_model_for_provider(app, &provider, &current_model, requirements).await?;
        }
        ModelSwitchRequest::PickProviderThenModel => {
            emit_command_output(app, format!("Current model: {}", app.current_model));
            let providers: Vec<String> = known_providers.clone();
            if providers.is_empty() {
                emit_command_output(app, "No providers are registered for selection.");
                return Ok(CommandResult::Handled);
            }
            let (current_provider, _) = split_provider_model(&app.current_model);
            let default_provider_index = providers
                .iter()
                .position(|p| p.eq_ignore_ascii_case(current_provider))
                .unwrap_or(0);
            let provider_pick =
                run_model_picker_select(app, "Select provider", &providers, default_provider_index);
            if !provider_pick.confirmed || provider_pick.index >= providers.len() {
                emit_command_output(app, "Model switch cancelled.");
                return Ok(CommandResult::Handled);
            }
            let provider = providers[provider_pick.index].as_str();
            let current_model = app.current_model.clone();
            pick_model_for_provider(app, provider, &current_model, requirements).await?;
        }
    }
    Ok(CommandResult::Handled)
}
