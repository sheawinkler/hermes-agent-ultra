//! Hermes Agent — binary entry point.
//!
//! Initializes logging, parses CLI arguments, and dispatches to the
//! appropriate subcommand handler.

mod auth_main;
mod cli_setup;
mod cron_main;
mod dispatch;
mod doctor;
mod gateway_handlers;
mod gateway_main;
mod gateway_plan_mode;
mod gateway_process;
mod gateway_runtime;
mod interactive_lock;
mod misc_main;
mod oneshot;
mod profile_main;
mod provenance;
mod route_learning;
mod session_resume;
mod setup;
mod state_paths;
mod status_main;

#[cfg(test)]
mod main_tests;

pub(crate) use cli_setup::{run_config, run_model, run_tools};
pub(crate) use cron_main::{run_cron, run_webhook};
pub(crate) use gateway_runtime::run_gateway;
pub(crate) use misc_main::read_setup_stdin_line;
pub(crate) use oneshot::{
    handle_local_slash_query, oneshot_auto_verify_oauth_provider, oneshot_should_use_app_runtime,
    print_app_oneshot_result,
};
pub(crate) use state_paths::{hermes_state_root, log_legacy_home_env_hint};
pub(crate) use status_main::{run_dashboard, run_debug, run_logs, run_status};

use auth_main::lookup_secret_from_vault;
use hermes_auth::FileTokenStore;
use hermes_cli::App;
use hermes_cli::app::provider_api_key_from_env;
use hermes_cli::cli::Cli;
use hermes_cli::paths::CliStateRoot;
use hermes_config::load_config;
use hermes_core::AgentError;
use hermes_telemetry::init_telemetry_from_env;
use interactive_lock::InteractiveSessionLockGuard;

fn main() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let handle = std::thread::Builder::new()
        .name("rHermes".into())
        .spawn(main_thread_entry)
        .expect("failed to spawn main thread");
    match handle.join() {
        Ok(Ok(())) => {}
        Ok(Err(code)) => std::process::exit(code),
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

fn main_thread_entry() -> Result<(), i32> {
    let (version, commit) = hermes_core::startup_commit_info();
    eprintln!(
        "[WARN] hermes-cli startup commit info: version={} commit={}",
        version, commit
    );

    if cfg!(debug_assertions) {
        if std::env::var("HERMES_CLI_PARSE_PROBE").ok().as_deref() == Some("1") {
            eprintln!("[probe] before Cli::try_parse()");
            let parse_result = Cli::try_parse();
            eprintln!("[probe] after Cli::try_parse()");
            match parse_result {
                Ok(_) => {
                    eprintln!("[probe] parse ok");
                    return Ok(());
                }
                Err(err) => err.exit(),
            }
        }
    }

    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("Error: failed to initialize async runtime: {}", err);
            return Err(1);
        }
    };
    // Spawn as a task so the large dispatch::run state machine lives on the heap,
    // not on the hermes-ultra-main thread stack (which would overflow even at 8 MB
    // in debug builds).
    let result = runtime.block_on(async move {
        tokio::spawn(async_main(cli))
            .await
            .unwrap_or_else(|join_err| {
                if join_err.is_panic() {
                    std::panic::resume_unwind(join_err.into_panic());
                }
            })
    });
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    let _ = result;
    Ok(())
}

async fn async_main(cli: Cli) {
    dispatch::run(cli).await;
}

fn init_tracing(verbose: bool, interactive_tui: bool, gateway: bool) {
    let default = if interactive_tui {
        if verbose {
            "info,rustls=warn,hyper=warn,h2=warn"
        } else {
            "error,rustls=warn,hyper=warn,h2=warn"
        }
    } else if verbose {
        "debug,hermes_cron=debug,rustls=warn,hyper=warn,h2=warn"
    } else if gateway {
        "warn,hermes_cron=info,rustls=warn,hyper=warn,h2=warn"
    } else {
        "warn,rustls=warn,hyper=warn,h2=warn"
    };
    if interactive_tui
        && std::env::var("HERMES_TUI_ALLOW_STDERR_LOGS")
            .ok()
            .as_deref()
            != Some("1")
    {
        hermes_cli::env_vars::set_var("RUST_LOG", default);
    }
    init_telemetry_from_env("hermes-cli", default);
}

async fn run_interactive(cli: Cli) -> Result<(), AgentError> {
    // Install a panic hook that restores the terminal before printing the backtrace,
    // so panics are visible in the shell instead of being swallowed by the TUI's
    // alternate screen buffer.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stderr(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::cursor::Show,
        );
        default_hook(info);
    }));

    let _session_lock = InteractiveSessionLockGuard::acquire(&hermes_state_root(&cli))?;
    let app = App::new(cli).await?;
    hermes_cli::tui::run(app).await
}

pub(crate) async fn prompt_line(prompt: impl Into<String>) -> Result<String, AgentError> {
    let prompt = prompt.into();
    let line = tokio::task::spawn_blocking(move || {
        use std::io::{self, Write};
        print!("{}", prompt);
        let _ = io::stdout().flush();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| AgentError::Io(format!("stdin task: {}", e)))?
    .map_err(|e| AgentError::Io(format!("stdin: {}", e)))?;
    Ok(line.trim().to_string())
}

pub(crate) async fn resolve_llm_login_token(
    cli: &Cli,
    provider: &str,
) -> Result<String, AgentError> {
    if let Some(k) = provider_api_key_from_env(provider) {
        return Ok(k);
    }
    let vault_path = CliStateRoot::from_state_root(&hermes_state_root(cli)).secret_vault();
    if vault_path.exists() {
        let store = FileTokenStore::new(vault_path).await?;
        if let Some((_provider, token)) = lookup_secret_from_vault(&store, provider).await {
            return Ok(token);
        }
    }
    let cfg =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;
    if let Some(k) = cfg
        .llm_providers
        .get(provider)
        .and_then(|c| c.api_key.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(k.to_string());
    }
    let fallback_var = format!("{}_API_KEY", provider.to_uppercase().replace('-', "_"));
    let msg = format!(
        "No API key in env or config for provider '{}'.\n\
         Set {} (or `hermes secrets set {}`; plaintext fallback: `hermes config set llm.{}.api_key ...`) or paste key now: ",
        provider, fallback_var, provider, provider
    );
    let pasted = prompt_line(msg).await?;
    if pasted.is_empty() {
        return Err(AgentError::Config(format!(
            "Missing API key for provider '{}'",
            provider
        )));
    }
    Ok(pasted)
}
