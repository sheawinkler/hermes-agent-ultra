async fn handle_model_harness_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let (current_provider, current_model_id) = model_current_provider_and_id(&app.current_model);
    let target = args.first().copied().unwrap_or_default().trim();
    let (provider, requested_model) = if target.is_empty() {
        (current_provider.clone(), current_model_id.clone())
    } else if target.contains(':') {
        let normalized = normalize_provider_model(target)?;
        let (prov, model_id) = model_current_provider_and_id(&normalized);
        (prov, model_id)
    } else {
        (normalize_backend_provider(target), current_model_id.clone())
    };

    let catalog = provider_model_ids(&provider).await;
    let catalog_total = catalog.len();
    let selected_model = requested_model.trim().to_string();
    let selected_lc = selected_model.to_ascii_lowercase();
    let selected_ok = catalog.iter().any(|candidate| {
        let lower = candidate.trim().to_ascii_lowercase();
        lower == selected_lc || lower.ends_with(&format!("/{selected_lc}"))
    });
    let credential_present = crate::app::provider_api_key_from_env(&provider).is_some();
    let auth_state_present = crate::auth::read_provider_auth_state(&provider)
        .ok()
        .flatten()
        .is_some();
    let cache_status = cached_provider_catalog_status(&provider);
    let mut out = String::new();
    let _ = writeln!(out, "Model/provider harness");
    let _ = writeln!(out, "provider: {}", provider);
    let _ = writeln!(out, "selected_model: {}", selected_model);
    let _ = writeln!(
        out,
        "credentials: api_key={} oauth_state={}",
        yes_no(credential_present),
        yes_no(auth_state_present)
    );
    let _ = writeln!(out, "catalog_total: {}", catalog_total);
    let _ = writeln!(out, "selected_in_catalog: {}", yes_no(selected_ok));
    if let Some(status) = cache_status {
        let _ = writeln!(
            out,
            "catalog_cache: verified={} age_secs={}",
            yes_no(status.verified),
            status
                .age_secs
                .map(|v| v.to_string())
                .unwrap_or_else(|| "n/a".to_string())
        );
    } else {
        let _ = writeln!(out, "catalog_cache: unavailable");
    }
    if !selected_ok {
        let sample = catalog
            .iter()
            .take(6)
            .cloned()
            .collect::<Vec<String>>()
            .join(", ");
        let _ = writeln!(
            out,
            "remediation: switch via `/model {} --provider {}` (or run `/model {}`)",
            selected_model, provider, provider
        );
        if !sample.is_empty() {
            let _ = writeln!(out, "catalog_sample: {}", sample);
        }
    }
    if provider == "openrouter" && !credential_present && !auth_state_present {
        let _ = writeln!(
            out,
            "openrouter_hint: set OPENROUTER_API_KEY or use a provider with OAuth (`/auth refresh`)."
        );
    }
    if provider == "huggingface" {
        let _ = writeln!(
            out,
            "huggingface_hint: prefer HF_TOKEN + HF_BASE_URL for full catalog enumeration."
        );
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn resolve_model_refresh_provider(app: &App, args: &[&str]) -> String {
    let raw = args.first().copied().unwrap_or_default().trim();
    if raw.is_empty() {
        return canonical_provider_id(provider_slug_from_provider_model(&app.current_model));
    }
    if let Some((provider, _)) = raw.split_once(':') {
        return canonical_provider_id(provider.trim());
    }
    canonical_provider_id(raw)
}

async fn handle_model_refresh_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let provider = resolve_model_refresh_provider(app, args);
    if provider.trim().is_empty() {
        emit_command_output(app, "Usage: /model refresh [provider|provider:model]");
        return Ok(CommandResult::Handled);
    }

    let known_provider = provider_slugs_for_config(&app.config)
        .iter()
        .any(|slug| slug.eq_ignore_ascii_case(&provider));
    let cache_cleared = clear_provider_catalog_cache(&provider)?;
    let models = provider_model_ids_for_config(&provider, &app.config).await;
    let cache_status = cached_provider_catalog_status(&provider);

    let mut out = String::new();
    let _ = writeln!(out, "Model catalog refreshed");
    let _ = writeln!(out, "provider: {}", provider);
    let _ = writeln!(out, "known_provider: {}", yes_no(known_provider));
    let _ = writeln!(out, "cache_cleared: {}", yes_no(cache_cleared));
    let _ = writeln!(out, "catalog_total: {}", models.len());
    match cache_status {
        Some(status) => {
            let _ = writeln!(
                out,
                "catalog_cache: verified={} age_secs={}",
                yes_no(status.verified),
                status
                    .age_secs
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "n/a".to_string())
            );
        }
        None => {
            let _ = writeln!(out, "catalog_cache: unavailable");
        }
    }
    if models.is_empty() {
        let _ = writeln!(out, "models_sample: (none)");
        if !known_provider {
            let _ = writeln!(
                out,
                "note: provider is not registered; configure llm_providers or use a known provider."
            );
        }
    } else {
        let sample = models
            .iter()
            .take(8)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "models_sample: {}", sample);
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_model_backend_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args.first().copied().unwrap_or("list").to_ascii_lowercase();
    match action.as_str() {
        "list" | "status" => {
            let provider = args.get(1).copied();
            emit_command_output(app, render_backend_profiles(provider));
        }
        "show" => {
            let Some(provider) = args.get(1).copied() else {
                emit_command_output(app, "Usage: /model backend show <provider> [profile]");
                return Ok(CommandResult::Handled);
            };
            let profile = args.get(2).copied();
            let Some(row) = backend_profile_lookup(provider, profile) else {
                emit_command_output(
                    app,
                    format!(
                        "No backend profile found for {}:{}.",
                        provider,
                        profile.unwrap_or("balanced")
                    ),
                );
                return Ok(CommandResult::Handled);
            };
            emit_command_output(
                app,
                format!(
                    "{}:{}\n{}\nlaunch: {}\nenv: {}",
                    row.provider,
                    row.profile,
                    row.summary,
                    row.launch_hint,
                    row.env_overrides
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<String>>()
                        .join(", ")
                ),
            );
        }
        "apply" => {
            let Some(provider) = args.get(1).copied() else {
                emit_command_output(app, "Usage: /model backend apply <provider> [profile]");
                return Ok(CommandResult::Handled);
            };
            let profile = args.get(2).copied().unwrap_or("balanced");
            let Some(row) = backend_profile_lookup(provider, Some(profile)) else {
                emit_command_output(
                    app,
                    format!("No backend profile found for {}:{}.", provider, profile),
                );
                return Ok(CommandResult::Handled);
            };
            for (key, value) in row.env_overrides {
                std::env::set_var(key, value);
            }
            std::env::set_var("HERMES_LOCAL_BACKEND_PROFILE", row.profile);
            std::env::set_var("HERMES_LOCAL_BACKEND_PROVIDER", row.provider);
            let persisted = persist_backend_profile_env(row.provider, row.profile, row.env_overrides)?;
            let (current_provider, _) = model_current_provider_and_id(&app.current_model);
            if current_provider == row.provider {
                let current = app.current_model.clone();
                app.switch_model(&current);
            }
            emit_command_output(
                app,
                format!(
                    "Applied backend profile {}:{}.\nlaunch: {}\npersisted_env_file: {}\nUse `set -a && source {}` before launching external backend processes.",
                    row.provider,
                    row.profile,
                    row.launch_hint,
                    persisted.display(),
                    persisted.display()
                ),
            );
        }
        _ => emit_command_output(
            app,
            "Usage: /model backend [list|status [provider]|show <provider> [profile]|apply <provider> [profile]]",
        ),
    }
    Ok(CommandResult::Handled)
}

