use hermes_cli::App;
use hermes_cli::cli::{Cli, CliCommand};
use hermes_cli::config_env::hydrate_env_from_config;
use hermes_config::load_config;
use hermes_core::AgentError;
use hermes_core::init_global_clock;

use crate::auth_main::{hydrate_provider_env_from_vault_for_cli, run_auth, run_secrets};
use crate::doctor::run_doctor;
use crate::misc_main::{
    run_completion, run_dump, run_elite_check, run_kanban, run_lumio, run_portal,
    run_rotate_provenance_key, run_uninstall, run_update, run_verify_provenance,
};
use crate::profile_main::run_profile_command;
use crate::route_learning::{
    apply_route_autotune_env_overrides, run_incident_pack, run_route_autotune, run_route_health,
    run_route_learning,
};
use crate::session_resume::run_resume;
use crate::{
    handle_local_slash_query, hermes_state_root, init_tracing, log_legacy_home_env_hint,
    oneshot_auto_verify_oauth_provider, oneshot_should_use_app_runtime, print_app_oneshot_result,
    run_config, run_cron, run_dashboard, run_debug, run_gateway, run_interactive, run_logs,
    run_model, run_status, run_tools, run_webhook,
};

pub(crate) async fn run(cli: Cli) {
    if let Some(config_dir) = cli.config_dir.as_deref() {
        hermes_cli::env_vars::set_var("HERMES_HOME", config_dir);
    }
    let prior_home = std::env::var("HERMES_HOME").ok();
    let migrated_home = hermes_config::ensure_migrated_hermes_home(cli.config_dir.as_deref());
    hermes_cli::env_vars::set_var("HERMES_HOME", migrated_home.to_string_lossy().as_ref());
    log_legacy_home_env_hint(prior_home.as_deref(), &migrated_home);
    if cli.ignore_user_config {
        hermes_cli::env_vars::set_var("HERMES_IGNORE_USER_CONFIG", "1");
    }
    if cli.ignore_rules {
        hermes_cli::env_vars::set_var("HERMES_IGNORE_RULES", "1");
        hermes_cli::env_vars::set_var("HERMES_AGENT_SKIP_CONTEXT_FILES", "1");
    }
    if cli.accept_hooks {
        hermes_cli::env_vars::set_var("HERMES_ACCEPT_HOOKS", "1");
        hermes_agent::shell_hooks::set_process_accept_hooks(true);
    }
    let effective_command = cli.effective_command();
    let global_model_override = cli.model.clone();
    let global_provider_override = cli.provider.clone();
    let global_allow_tools_override = cli.allow_tools;

    // Initialize tracing
    init_tracing(
        cli.verbose,
        matches!(
            effective_command,
            CliCommand::Hermes | CliCommand::Resume { .. }
        ),
        matches!(effective_command, CliCommand::Gateway { .. }),
    );
    if let Err(err) = hydrate_provider_env_from_vault_for_cli(&cli).await {
        tracing::warn!("Secret-vault hydration skipped: {}", err);
    }
    if let Ok(cfg) = load_config(cli.config_dir.as_deref()) {
        init_global_clock(cfg.timezone.as_deref());
        let applied = hydrate_env_from_config(&cfg);
        tracing::trace!(
            applied_env_vars = applied,
            "Hydrated environment from config.yaml"
        );
    } else {
        init_global_clock(None);
    }
    let route_autotune_applied = apply_route_autotune_env_overrides(&hermes_state_root(&cli));
    if !route_autotune_applied.is_empty() {
        tracing::debug!(
            applied_env_vars = ?route_autotune_applied,
            "Hydrated environment from route-autotune overrides"
        );
    }

    tracing::debug!("Hermes Agent starting");

    if let Some(prompt) = cli.oneshot.clone() {
        match handle_local_slash_query(cli.clone(), &prompt).await {
            Ok(true) => return,
            Ok(false) => {}
            Err(err) => {
                eprintln!("Error: {}", err);
                std::process::exit(1);
            }
        }
        if oneshot_should_use_app_runtime(&prompt) {
            let mut app = match App::new(cli.clone()).await {
                Ok(app) => app,
                Err(err) => {
                    eprintln!("Error: {}", err);
                    std::process::exit(1);
                }
            };
            if let Err(err) = app.handle_input(&prompt).await {
                eprintln!("Error: {}", err);
                std::process::exit(1);
            }
            print_app_oneshot_result(&app);
            return;
        }
        let mut result = hermes_cli::commands::handle_cli_chat(
            Some(prompt),
            None,
            false,
            false,
            global_model_override.clone(),
            global_provider_override.clone(),
            global_allow_tools_override,
        )
        .await;
        if let Err(err) = &result {
            if let Some(provider) = oneshot_auto_verify_oauth_provider(
                err,
                global_provider_override.as_deref(),
                global_model_override.as_deref(),
            ) {
                eprintln!(
                    "Detected OAuth auth failure for provider '{}' in one-shot mode; running `hermes-ultra auth verify {}` and retrying once...",
                    provider, provider
                );
                if let Err(verify_err) = run_auth(
                    cli.clone(),
                    Some("verify".to_string()),
                    Some(provider.clone()),
                    None,
                    None,
                    None,
                    None,
                    false,
                )
                .await
                {
                    eprintln!(
                        "Warning: automatic `auth verify {}` failed: {}",
                        provider, verify_err
                    );
                }
                result = hermes_cli::commands::handle_cli_chat(
                    Some(cli.oneshot.clone().unwrap_or_default()),
                    None,
                    false,
                    false,
                    global_model_override.clone(),
                    global_provider_override.clone(),
                    global_allow_tools_override,
                )
                .await;
                if provider == "nous" {
                    if let Err(retry_err) = &result {
                        if oneshot_auto_verify_oauth_provider(
                            retry_err,
                            Some(provider.as_str()),
                            global_model_override.as_deref(),
                        )
                        .as_deref()
                            == Some("nous")
                        {
                            eprintln!(
                                "Nous OAuth still invalid; launching `hermes-ultra auth login nous` and retrying once..."
                            );
                            if let Err(login_err) = run_auth(
                                cli.clone(),
                                Some("login".to_string()),
                                Some("nous".to_string()),
                                None,
                                None,
                                None,
                                None,
                                false,
                            )
                            .await
                            {
                                eprintln!(
                                    "Warning: automatic `auth login nous` failed: {}",
                                    login_err
                                );
                            } else {
                                result = hermes_cli::commands::handle_cli_chat(
                                    Some(cli.oneshot.clone().unwrap_or_default()),
                                    None,
                                    false,
                                    false,
                                    global_model_override.clone(),
                                    global_provider_override.clone(),
                                    global_allow_tools_override,
                                )
                                .await;
                            }
                        }
                    }
                }
            }
        }
        if let Err(e) = result {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        return;
    }

    let result = match effective_command {
        CliCommand::Hermes => run_interactive(cli).await,
        CliCommand::Chat {
            query,
            preload_skill,
            yolo,
            plan,
        } => {
            run_chat_command(
                cli,
                query,
                preload_skill,
                yolo,
                plan,
                global_model_override.clone(),
                global_provider_override.clone(),
                global_allow_tools_override,
            )
            .await
        }
        CliCommand::Model { provider_model } => run_model(cli, provider_model).await,
        CliCommand::Tools {
            action,
            name,
            platform,
            summary,
        } => run_tools(cli, action, name, platform, summary).await,
        CliCommand::Config { action, key, value } => run_config(cli, action, key, value).await,
        CliCommand::Gateway {
            action,
            system,
            all,
            force,
            run_as_user,
            replace,
            dry_run,
            yes,
            deep,
        } => {
            run_gateway_command(
                cli,
                action,
                system,
                all,
                force,
                run_as_user,
                replace,
                dry_run,
                yes,
                deep,
            )
            .await
        }
        CliCommand::Setup { portal } => {
            if portal {
                run_portal(cli, Some("setup".to_string())).await
            } else {
                crate::setup::run_setup(cli).await
            }
        }
        CliCommand::Portal { action } => run_portal(cli, action).await,
        CliCommand::Doctor {
            deep,
            self_heal,
            snapshot,
            snapshot_path,
            bundle,
        } => run_doctor(cli, deep, self_heal, snapshot, snapshot_path, bundle).await,
        CliCommand::Update {
            check,
            yes,
            rollback,
            force,
            source,
            channel,
        } => run_update(check, yes, rollback, force, source, channel).await,
        CliCommand::EliteCheck { json, strict } => run_elite_check(cli, json, strict).await,
        CliCommand::VerifyProvenance {
            path,
            signature,
            strict,
            json,
        } => run_verify_provenance(cli, path, signature, strict, json).await,
        CliCommand::RotateProvenanceKey { json } => run_rotate_provenance_key(cli, json).await,
        CliCommand::RouteLearning { action, json } => run_route_learning(cli, action, json).await,
        CliCommand::RouteHealth { action, json } => run_route_health(cli, action, json).await,
        CliCommand::RouteAutotune {
            action,
            apply,
            strict,
            json,
        } => run_route_autotune(cli, action, apply, strict, json).await,
        CliCommand::IncidentPack {
            snapshot,
            output,
            json,
        } => run_incident_pack(cli, snapshot, output, json).await,
        CliCommand::Status => run_status(cli).await,
        CliCommand::Kanban { args } => run_kanban(args),
        CliCommand::Systems {
            action,
            topic,
            json,
            output,
            host,
            port,
            once,
        } => {
            hermes_cli::systems::handle_cli_systems(hermes_cli::systems::SystemsCliOptions {
                config_dir: cli.config_dir.clone(),
                action,
                topic,
                json_only: json,
                output,
                host,
                port,
                once,
            })
            .await
        }
        CliCommand::TeamsPipeline {
            action,
            id,
            limit,
            status,
            store_path,
            meeting_id,
            join_web_url,
            tenant_id,
            call_record_id,
            resource,
            notification_url,
            change_type,
            expiration,
            client_state,
            lifecycle_notification_url,
            latest_supported_tls_version,
            force_refresh,
            renew_within_hours,
            extend_hours,
            dry_run,
        } => {
            hermes_cli::teams_pipeline_cli::handle_cli_teams_pipeline(
                hermes_cli::teams_pipeline_cli::TeamsPipelineCliOptions {
                    config_dir: cli.config_dir.clone(),
                    action,
                    id,
                    limit,
                    status,
                    store_path,
                    meeting_id,
                    join_web_url,
                    tenant_id,
                    call_record_id,
                    resource,
                    notification_url,
                    change_type,
                    expiration,
                    client_state,
                    lifecycle_notification_url,
                    latest_supported_tls_version,
                    force_refresh,
                    renew_within_hours,
                    extend_hours,
                    dry_run,
                },
            )
            .await
        }
        CliCommand::Dashboard {
            host,
            port,
            no_open,
            insecure,
        } => run_dashboard(cli, host, port, no_open, insecure).await,
        CliCommand::Debug {
            action,
            url,
            lines,
            expire,
            local,
        } => run_debug(cli, action, url, lines, expire, local).await,
        CliCommand::Logs { lines, follow } => run_logs(cli, lines, follow).await,
        CliCommand::Profile {
            action,
            name,
            secondary,
            output,
            import_name,
            alias_name,
            remove,
            yes,
            clone,
            clone_all,
            clone_from,
            no_alias,
            no_skills,
        } => {
            run_profile_command(
                cli,
                action,
                name,
                secondary,
                output,
                import_name,
                alias_name,
                remove,
                yes,
                clone,
                clone_all,
                clone_from,
                no_alias,
                no_skills,
            )
            .await
        }
        CliCommand::Auth {
            action,
            provider,
            target,
            auth_type,
            label,
            api_key,
            qr,
        } => run_auth(cli, action, provider, target, auth_type, label, api_key, qr).await,
        CliCommand::Secrets {
            action,
            provider,
            value,
            show,
        } => run_secrets(cli, action, provider, value, show).await,
        CliCommand::Skills {
            action,
            name,
            extra,
        } => hermes_cli::commands::skills::handle_cli_skills(action, name, extra).await,
        CliCommand::Plugins {
            action,
            name,
            git_ref,
            allow_untrusted_git_host,
        } => {
            hermes_cli::commands::handle_cli_plugins(
                action,
                name,
                git_ref,
                allow_untrusted_git_host,
            )
            .await
        }
        CliCommand::Memory {
            action,
            target,
            yes,
        } => hermes_cli::commands::handle_cli_memory(action, target, yes).await,
        CliCommand::Interest {
            action,
            mode,
            llm_on_session_end,
            rest,
        } => {
            hermes_cli::commands::handle_cli_interest(action, mode, llm_on_session_end, rest).await
        }
        CliCommand::Contribute {
            action,
            poi_only,
            skills_only,
            last_session,
            outbox_clear,
        } => {
            hermes_cli::commands::handle_cli_contribute(
                action,
                poi_only,
                skills_only,
                last_session,
                outbox_clear,
            )
            .await
        }
        CliCommand::Server { action, rest, method } => {
            hermes_cli::commands::handle_cli_server(
                action,
                rest,
                method,
                cli.config_dir.as_deref(),
            )
            .await
        }
        CliCommand::Mcp {
            action,
            name,
            server,
            url,
            command,
            parallel_tools,
        } => {
            hermes_cli::commands::handle_cli_mcp(action, name, server, url, command, parallel_tools)
                .await
        }
        CliCommand::Sessions { action, id, name } => {
            hermes_cli::commands::handle_cli_sessions(action, id, name).await
        }
        CliCommand::Resume { session_id } => run_resume(cli, session_id).await,
        CliCommand::Insights { days, source } => {
            hermes_cli::commands::handle_cli_insights(days, source).await
        }
        CliCommand::Login { provider } => {
            run_auth(
                cli,
                Some("login".to_string()),
                provider,
                None,
                None,
                None,
                None,
                false,
            )
            .await
        }
        CliCommand::Logout { provider } => {
            run_auth(
                cli,
                Some("logout".to_string()),
                provider,
                None,
                None,
                None,
                None,
                false,
            )
            .await
        }
        CliCommand::Whatsapp { action } => hermes_cli::commands::handle_cli_whatsapp(action).await,
        CliCommand::Pairing {
            action,
            device_id,
            args,
        } => hermes_cli::commands::handle_cli_pairing(action, device_id, args).await,
        CliCommand::Claw { action } => hermes_cli::commands::handle_cli_claw(action).await,
        CliCommand::Acp { action } => hermes_cli::commands::handle_cli_acp(action).await,
        CliCommand::Backup { output } => hermes_cli::commands::handle_cli_backup(output).await,
        CliCommand::Import { path } => hermes_cli::commands::handle_cli_import(path).await,
        CliCommand::Version => hermes_cli::commands::handle_cli_version(),
        CliCommand::Cron {
            action,
            job_id,
            id,
            schedule,
            prompt,
            name,
            deliver,
            repeat,
            skills,
            add_skills,
            remove_skills,
            clear_skills,
            script,
            no_agent,
            agent,
            script_timeout_seconds,
            script_shell,
            all,
        } => {
            run_cron(
                cli,
                action,
                job_id,
                id,
                schedule,
                prompt,
                name,
                deliver,
                repeat,
                skills,
                add_skills,
                remove_skills,
                clear_skills,
                script,
                no_agent,
                agent,
                script_timeout_seconds,
                script_shell,
                all,
            )
            .await
        }
        CliCommand::Webhook {
            action,
            name,
            url,
            id,
            prompt,
            events,
            description,
            skills,
            deliver,
            deliver_chat_id,
            secret,
            deliver_only,
            payload,
        } => {
            run_webhook(
                cli,
                action,
                name,
                url,
                id,
                prompt,
                events,
                description,
                skills,
                deliver,
                deliver_chat_id,
                secret,
                deliver_only,
                payload,
            )
            .await
        }
        CliCommand::Dump { session, output } => run_dump(cli, session, output).await,
        CliCommand::Completion { shell } => run_completion(shell),
        CliCommand::Uninstall { yes } => run_uninstall(yes).await,
        CliCommand::Lumio { action, model } => run_lumio(action, model).await,
        CliCommand::Meeting {
            action,
            audio,
            title,
            mode,
            diarize,
        } => hermes_cli::commands::handle_cli_meeting(action, audio, title, mode, diarize).await,
        CliCommand::PluginExternal(raw) => {
            hermes_cli::commands::handle_cli_external_plugin_subcommand(raw).await
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

async fn run_chat_command(
    cli: Cli,
    query: Option<String>,
    preload_skill: Option<String>,
    yolo: bool,
    plan: bool,
    global_model_override: Option<String>,
    global_provider_override: Option<String>,
    global_allow_tools_override: bool,
) -> Result<(), AgentError> {
    if let Some(prompt) = query.clone() {
        match handle_local_slash_query(cli, &prompt).await {
            Ok(true) => Ok(()),
            Ok(false) => {
                hermes_cli::commands::handle_cli_chat(
                    query,
                    preload_skill,
                    yolo,
                    plan,
                    global_model_override,
                    global_provider_override,
                    global_allow_tools_override,
                )
                .await
            }
            Err(err) => Err(err),
        }
    } else {
        hermes_cli::commands::handle_cli_chat(
            query,
            preload_skill,
            yolo,
            plan,
            global_model_override,
            global_provider_override,
            global_allow_tools_override,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_gateway_command(
    cli: Cli,
    action: Option<String>,
    system: bool,
    all: bool,
    force: bool,
    run_as_user: Option<String>,
    replace: bool,
    dry_run: bool,
    yes: bool,
    deep: bool,
) -> Result<(), AgentError> {
    run_gateway(
        cli,
        action,
        system,
        all,
        force,
        run_as_user,
        replace,
        dry_run,
        yes,
        deep,
    )
    .await
}
