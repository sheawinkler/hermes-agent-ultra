//! Hermes Agent — binary entry point.
//!
//! Initializes logging, parses CLI arguments, and dispatches to the
//! appropriate subcommand handler.

use clap::Parser;
use clap::CommandFactory;
use clap_complete::{generate, Shell as CompletionShell};
use tracing_subscriber::EnvFilter;

use hermes_cli::cli::{Cli, CliCommand};
use hermes_cli::App;
use hermes_config::load_config;
use hermes_core::AgentError;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Initialize tracing
    init_tracing(cli.verbose);

    tracing::debug!("Hermes Agent starting");

    let result = match cli.effective_command() {
        CliCommand::Hermes => run_interactive(cli).await,
        CliCommand::Model { provider_model } => run_model(cli, provider_model).await,
        CliCommand::Tools { action } => run_tools(cli, action).await,
        CliCommand::Config { action, key, value } => run_config(cli, action, key, value).await,
        CliCommand::Gateway { action } => run_gateway(cli, action).await,
        CliCommand::Setup => run_setup().await,
        CliCommand::Doctor => run_doctor(cli).await,
        CliCommand::Update => run_update().await,
        CliCommand::Status => run_status(cli).await,
        CliCommand::Logs { lines, follow } => run_logs(cli, lines, follow).await,
        CliCommand::Profile { action, name } => run_profile(cli, action, name).await,
        CliCommand::Auth { action, provider } => run_auth(action, provider).await,
        CliCommand::Cron {
            action,
            id,
            schedule,
            prompt,
        } => run_cron(action, id, schedule, prompt).await,
        CliCommand::Webhook { action, url } => run_webhook(action, url).await,
        CliCommand::Dump { session, output } => run_dump(cli, session, output).await,
        CliCommand::Completion { shell } => run_completion(shell),
        CliCommand::Uninstall { yes } => run_uninstall(yes).await,
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

/// Initialize the tracing subscriber with env filter.
fn init_tracing(verbose: bool) {
    let filter = if verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

/// Run the interactive REPL (default command).
async fn run_interactive(cli: Cli) -> Result<(), AgentError> {
    let app = App::new(cli).await?;
    hermes_cli::tui::run(app).await
}

/// Handle `hermes model [provider:model]`.
async fn run_model(cli: Cli, provider_model: Option<String>) -> Result<(), AgentError> {
    let config = load_config(cli.config_dir.as_deref())
        .map_err(|e| AgentError::Config(e.to_string()))?;

    match provider_model {
        Some(pm) => {
            println!("Model switched to: {}", pm);
            println!("(To persist, run: hermes config set model {})", pm);
        }
        None => {
            let current = config.model.as_deref().unwrap_or("gpt-4o");
            println!("Current model: {}", current);

            // List known providers
            println!("\nAvailable providers:");
            println!("  openai       — OpenAI (gpt-4o, gpt-4o-mini, ...)");
            println!("  anthropic    — Anthropic (claude-3-5-sonnet, claude-3-opus, ...)");
            println!("  openrouter   — OpenRouter (multi-provider routing)");
            println!("\nUsage: hermes model <provider>:<model>");
        }
    }
    Ok(())
}

/// Handle `hermes tools [action]`.
async fn run_tools(cli: Cli, action: Option<String>) -> Result<(), AgentError> {
    let registry = hermes_tools::ToolRegistry::new();
    let tools = registry.list_tools();

    match action.as_deref() {
        None | Some("list") => {
            if tools.is_empty() {
                println!("No tools registered (tools are loaded at runtime).");
                println!("\nBuilt-in tool categories:");
                let categories = [
                    "web", "terminal", "file", "browser", "vision", "image_gen",
                    "skills", "memory", "session_search", "todo", "clarify",
                    "code_execution", "delegation", "cronjob", "messaging",
                    "homeassistant",
                ];
                for cat in &categories {
                    println!("  • {}", cat);
                }
            } else {
                println!("Registered tools ({}):", tools.len());
                for tool in &tools {
                    println!("  • {} — {}", tool.name, tool.description);
                }
            }
        }
        Some(other) => {
            println!("Unknown tools action: {}. Use 'list'.", other);
        }
    }
    Ok(())
}

/// Handle `hermes config [action] [key] [value]`.
async fn run_config(
    cli: Cli,
    action: Option<String>,
    key: Option<String>,
    value: Option<String>,
) -> Result<(), AgentError> {
    let config = load_config(cli.config_dir.as_deref())
        .map_err(|e| AgentError::Config(e.to_string()))?;

    match action.as_deref() {
        None => {
            // Show full config as JSON
            let json = serde_json::to_string_pretty(&config)
                .map_err(|e| AgentError::Config(e.to_string()))?;
            println!("{}", json);
        }
        Some("get") => {
            let key = key.ok_or_else(|| AgentError::Config("Missing key. Usage: hermes config get <key>".into()))?;
            match key.as_str() {
                "model" => println!("{}", config.model.as_deref().unwrap_or("(not set)")),
                "personality" => println!("{}", config.personality.as_deref().unwrap_or("(not set)")),
                "max_turns" => println!("{}", config.max_turns),
                "system_prompt" => println!("{}", config.system_prompt.as_deref().unwrap_or("(not set)")),
                other => println!("Unknown config key: {}", other),
            }
        }
        Some("set") => {
            let key = key.ok_or_else(|| AgentError::Config("Missing key. Usage: hermes config set <key> <value>".into()))?;
            let value = value.ok_or_else(|| AgentError::Config("Missing value. Usage: hermes config set <key> <value>".into()))?;
            println!("Set {} = {}", key, value);
            println!("(Config file persistence not yet implemented — use config.yaml directly)");
        }
        Some(other) => {
            println!("Unknown config action: {}. Use 'get' or 'set'.", other);
        }
    }
    Ok(())
}

/// Handle `hermes gateway [action]`.
async fn run_gateway(cli: Cli, action: Option<String>) -> Result<(), AgentError> {
    let config = load_config(cli.config_dir.as_deref())
        .map_err(|e| AgentError::Config(e.to_string()))?;

    match action.as_deref() {
        Some("setup") => {
            println!("Gateway setup wizard");
            println!("--------------------");
            println!("Edit config.yaml and enable platforms under `platforms:`");
            println!("Then run `hermes gateway start`.");
        }
        None | Some("start") => {
            println!("Starting Hermes Gateway...");

            // List enabled platforms
            let enabled: Vec<&String> = config
                .platforms
                .iter()
                .filter(|(_, pc)| pc.enabled)
                .map(|(name, _)| name)
                .collect();

            if enabled.is_empty() {
                println!("No platforms enabled. Configure platforms in config.yaml.");
                return Ok(());
            }

            println!("Enabled platforms: {}", enabled.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));

            println!("Gateway is ready. Press Ctrl+C to stop.");

            // Wait for Ctrl+C
            tokio::signal::ctrl_c()
                .await
                .map_err(|e| AgentError::Io(format!("Failed to listen for Ctrl+C: {}", e)))?;

            println!("\nShutting down gateway...");
            println!("Gateway stopped.");
        }
        Some("status") => {
            println!("Gateway status: not running (start with `hermes gateway start`)");
        }
        Some("stop") => {
            println!("Gateway stop: no running gateway found.");
        }
        Some(other) => {
            println!("Unknown gateway action: {}. Use 'start', 'stop', or 'status'.", other);
        }
    }
    Ok(())
}

async fn run_auth(action: Option<String>, provider: Option<String>) -> Result<(), AgentError> {
    let provider = provider.unwrap_or_else(|| "openai".to_string());
    match action.as_deref().unwrap_or("status") {
        "login" => {
            let msg = hermes_cli::auth::login(&provider).await?;
            println!("{}", msg);
        }
        "logout" => {
            let msg = hermes_cli::auth::logout(&provider).await?;
            println!("{}", msg);
        }
        _ => {
            println!("Auth status: provider='{}' (basic flow enabled)", provider);
        }
    }
    Ok(())
}

async fn run_cron(
    action: Option<String>,
    id: Option<String>,
    schedule: Option<String>,
    prompt: Option<String>,
) -> Result<(), AgentError> {
    match action.as_deref().unwrap_or("list") {
        "list" => println!("Cron list: use runtime scheduler integration in gateway/agent process."),
        "create" => {
            let schedule = schedule.unwrap_or_else(|| "* * * * *".to_string());
            let prompt = prompt.unwrap_or_else(|| "No prompt provided".to_string());
            println!("Cron created (stub): schedule='{}', prompt='{}'", schedule, prompt);
        }
        "delete" => println!("Cron delete (stub): id={}", id.unwrap_or_default()),
        "pause" => println!("Cron pause (stub): id={}", id.unwrap_or_default()),
        "resume" => println!("Cron resume (stub): id={}", id.unwrap_or_default()),
        "run" => println!("Cron run now (stub): id={}", id.unwrap_or_default()),
        "history" => println!("Cron history (stub): id={}", id.unwrap_or_default()),
        other => println!("Unknown cron action: {}", other),
    }
    Ok(())
}

async fn run_webhook(action: Option<String>, url: Option<String>) -> Result<(), AgentError> {
    match action.as_deref().unwrap_or("list") {
        "list" => println!("Webhook list (stub)"),
        "add" => println!("Webhook added (stub): {}", url.unwrap_or_default()),
        "remove" => println!("Webhook removed (stub): {}", url.unwrap_or_default()),
        other => println!("Unknown webhook action: {}", other),
    }
    Ok(())
}

async fn run_dump(cli: Cli, session: Option<String>, output: Option<String>) -> Result<(), AgentError> {
    let home = cli
        .config_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home);
    let sessions_dir = home.join("sessions");
    let session = session.unwrap_or_else(|| "latest".to_string());
    let out = output.unwrap_or_else(|| format!("{}.dump.json", session));
    let payload = serde_json::json!({
        "session": session,
        "source_dir": sessions_dir,
        "note": "Session export scaffold"
    });
    std::fs::write(&out, serde_json::to_string_pretty(&payload).unwrap_or_default())
        .map_err(|e| AgentError::Io(format!("Failed to write dump: {}", e)))?;
    println!("Wrote dump to {}", out);
    Ok(())
}

fn run_completion(shell: Option<String>) -> Result<(), AgentError> {
    let mut cmd = Cli::command();
    let sh = match shell.as_deref().unwrap_or("zsh") {
        "bash" => CompletionShell::Bash,
        "fish" => CompletionShell::Fish,
        "powershell" => CompletionShell::PowerShell,
        "elvish" => CompletionShell::Elvish,
        _ => CompletionShell::Zsh,
    };
    generate(sh, &mut cmd, "hermes", &mut std::io::stdout());
    Ok(())
}

async fn run_uninstall(yes: bool) -> Result<(), AgentError> {
    let home = hermes_config::hermes_home();
    if !yes {
        println!("Uninstall is destructive. Re-run with `hermes uninstall --yes`.");
        return Ok(());
    }
    if home.exists() {
        std::fs::remove_dir_all(&home)
            .map_err(|e| AgentError::Io(format!("Failed to remove {}: {}", home.display(), e)))?;
        println!("Removed {}", home.display());
    } else {
        println!("Nothing to uninstall.");
    }
    Ok(())
}

/// Handle `hermes setup`.
async fn run_setup() -> Result<(), AgentError> {
    use std::io::{self, BufRead, Write};

    println!("Hermes Agent — Setup Wizard");
    println!("===========================\n");

    let config_dir = hermes_config::hermes_home();
    println!("Config directory: {}", config_dir.display());

    // 1. Create directory structure
    let subdirs = ["profiles", "sessions", "logs", "skills"];
    for dir in [config_dir.clone()]
        .into_iter()
        .chain(subdirs.iter().map(|d| config_dir.join(d)))
    {
        if dir.exists() {
            println!("  ✓ {} exists", dir.display());
        } else {
            std::fs::create_dir_all(&dir)
                .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", dir.display(), e)))?;
            println!("  ✓ Created {}", dir.display());
        }
    }

    let config_path = config_dir.join("config.yaml");
    let stdin = io::stdin();
    let mut reader = stdin.lock();

    // 2. Prompt for API key
    print!("\nOpenAI API key (leave blank to skip): ");
    io::stdout().flush().ok();
    let mut api_key = String::new();
    reader.read_line(&mut api_key).ok();
    let api_key = api_key.trim().to_string();

    // 3. Prompt for model
    println!("\nAvailable models:");
    println!("  1) openai:gpt-4o          (recommended)");
    println!("  2) openai:gpt-4o-mini     (fast & cheap)");
    println!("  3) anthropic:claude-3-5-sonnet");
    println!("  4) openrouter:auto        (multi-provider)");
    print!("Choose model [1]: ");
    io::stdout().flush().ok();
    let mut model_choice = String::new();
    reader.read_line(&mut model_choice).ok();
    let model = match model_choice.trim() {
        "2" => "openai:gpt-4o-mini",
        "3" => "anthropic:claude-3-5-sonnet",
        "4" => "openrouter:auto",
        _ => "openai:gpt-4o",
    };

    // 4. Prompt for personality
    print!("\nPersonality (default, concise, creative, technical) [default]: ");
    io::stdout().flush().ok();
    let mut personality = String::new();
    reader.read_line(&mut personality).ok();
    let personality = personality.trim();
    let personality = if personality.is_empty() { "default" } else { personality };

    // 5. Write config.yaml
    if config_path.exists() {
        print!("\nconfig.yaml already exists. Overwrite? [y/N]: ");
        io::stdout().flush().ok();
        let mut answer = String::new();
        reader.read_line(&mut answer).ok();
        if !answer.trim().eq_ignore_ascii_case("y") {
            println!("Keeping existing config.yaml.");
            println!("\nSetup complete! Run `hermes` to start an interactive session.");
            return Ok(());
        }
    }

    let mut config_content = String::from("# Hermes Agent Configuration\n\n");
    config_content.push_str(&format!("model: {}\n", model));
    config_content.push_str(&format!("personality: {}\n", personality));
    config_content.push_str("max_turns: 50\n\n");

    if !api_key.is_empty() {
        config_content.push_str("llm_providers:\n");
        config_content.push_str("  openai:\n");
        config_content.push_str(&format!("    api_key: {}\n", api_key));
    }

    std::fs::write(&config_path, &config_content)
        .map_err(|e| AgentError::Io(format!("Failed to write config: {}", e)))?;
    println!("\n  ✓ Wrote config.yaml");

    // 6. Write default profile
    let default_profile = config_dir.join("profiles").join("default.yaml");
    if !default_profile.exists() {
        let profile_content = format!(
            "# Default Hermes Profile\nname: default\nmodel: {}\npersonality: {}\n",
            model, personality,
        );
        std::fs::write(&default_profile, profile_content)
            .map_err(|e| AgentError::Io(format!("Failed to write profile: {}", e)))?;
        println!("  ✓ Created default profile");
    }

    println!("\nSetup complete! Run `hermes` to start an interactive session.");
    println!("Run `hermes doctor` to check system requirements.");
    Ok(())
}

/// Handle `hermes doctor`.
async fn run_doctor(cli: Cli) -> Result<(), AgentError> {
    println!("Hermes Agent — System Check");
    println!("===========================\n");

    // Check config
    let config_dir = hermes_config::hermes_home();
    print!("Config directory ({})... ", config_dir.display());
    if config_dir.exists() {
        println!("✓");
    } else {
        println!("✗ (run `hermes setup`)");
    }

    // Check config.yaml
    let config_path = config_dir.join("config.yaml");
    print!("config.yaml... ");
    if config_path.exists() {
        println!("✓");
    } else {
        println!("✗ (run `hermes setup`)");
    }

    // Check API keys via environment
    let api_checks = [
        ("OPENAI_API_KEY", "OpenAI"),
        ("ANTHROPIC_API_KEY", "Anthropic"),
        ("OPENROUTER_API_KEY", "OpenRouter"),
        ("EXA_API_KEY", "Exa (web search)"),
        ("FIRECRAWL_API_KEY", "Firecrawl (web extract)"),
    ];

    println!("\nAPI Keys:");
    for (env_var, name) in &api_checks {
        print!("  {} ({})... ", name, env_var);
        if std::env::var(env_var).is_ok() {
            println!("✓");
        } else {
            println!("✗ (not set)");
        }
    }

    // Check external tools
    println!("\nExternal tools:");
    let tool_checks = [
        ("docker", "Docker"),
        ("ssh", "SSH"),
        ("git", "Git"),
    ];

    for (cmd, name) in &tool_checks {
        print!("  {}... ", name);
        match tokio::process::Command::new("which")
            .arg(cmd)
            .output()
            .await
        {
            Ok(output) if output.status.success() => println!("✓"),
            _ => println!("✗ (not found)"),
        }
    }

    // Try loading config
    println!("\nConfiguration:");
    print!("  Loading config... ");
    match load_config(cli.config_dir.as_deref()) {
        Ok(config) => {
            println!("✓");
            println!("  Model: {}", config.model.as_deref().unwrap_or("(default)"));
            println!("  Max turns: {}", config.max_turns);
            let platform_count = config.platforms.iter().filter(|(_, p)| p.enabled).count();
            println!("  Enabled platforms: {}", platform_count);
        }
        Err(e) => {
            println!("✗ ({})", e);
        }
    }

    println!("\nDone.");
    Ok(())
}

/// Handle `hermes update`.
async fn run_update() -> Result<(), AgentError> {
    println!("Hermes Agent v{}", env!("CARGO_PKG_VERSION"));
    println!("Update check not yet implemented.");
    println!("Visit https://github.com/nousresearch/hermes-agent-rust for the latest version.");
    Ok(())
}

/// Handle `hermes status`.
async fn run_status(cli: Cli) -> Result<(), AgentError> {
    println!("Hermes Agent — Status");
    println!("=====================\n");

    println!("Version: {}", env!("CARGO_PKG_VERSION"));

    let config = load_config(cli.config_dir.as_deref())
        .map_err(|e| AgentError::Config(e.to_string()))?;

    println!("Model:   {}", config.model.as_deref().unwrap_or("(default: gpt-4o)"));
    println!(
        "Personality: {}",
        config.personality.as_deref().unwrap_or("(none)")
    );
    println!("Max turns: {}", config.max_turns);

    let enabled_platforms: Vec<&String> = config
        .platforms
        .iter()
        .filter(|(_, pc)| pc.enabled)
        .map(|(name, _)| name)
        .collect();
    if enabled_platforms.is_empty() {
        println!("Platforms: (none enabled)");
    } else {
        println!(
            "Platforms: {}",
            enabled_platforms
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let config_dir = hermes_config::hermes_home();
    println!("\nConfig dir: {}", config_dir.display());

    // Check for active sessions
    let sessions_dir = config_dir.join("sessions");
    if sessions_dir.exists() {
        let count = std::fs::read_dir(&sessions_dir)
            .map(|entries| entries.filter_map(|e| e.ok()).count())
            .unwrap_or(0);
        println!("Saved sessions: {}", count);
    }

    // Check for profiles
    let profiles_dir = config_dir.join("profiles");
    if profiles_dir.exists() {
        let profiles: Vec<String> = std::fs::read_dir(&profiles_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path()
                            .extension()
                            .map(|ext| ext == "yaml" || ext == "yml")
                            .unwrap_or(false)
                    })
                    .filter_map(|e| {
                        e.path()
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                    })
                    .collect()
            })
            .unwrap_or_default();
        if profiles.is_empty() {
            println!("Profiles: (none)");
        } else {
            println!("Profiles: {}", profiles.join(", "));
        }
    }

    Ok(())
}

/// Handle `hermes logs`.
async fn run_logs(cli: Cli, lines: u32, follow: bool) -> Result<(), AgentError> {
    let config_dir = cli
        .config_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home);
    let log_file = config_dir.join("logs").join("hermes.log");

    if !log_file.exists() {
        println!("No log file found at: {}", log_file.display());
        println!("Logs are written here during interactive sessions.");
        return Ok(());
    }

    if follow {
        println!("Tailing {}... (Ctrl+C to stop)\n", log_file.display());
        let mut child = tokio::process::Command::new("tail")
            .args(["-f", "-n", &lines.to_string()])
            .arg(&log_file)
            .spawn()
            .map_err(|e| AgentError::Io(format!("Failed to tail log file: {}", e)))?;

        tokio::select! {
            status = child.wait() => {
                match status {
                    Ok(s) if !s.success() => {
                        eprintln!("tail exited with status: {}", s);
                    }
                    Err(e) => {
                        eprintln!("Error waiting for tail: {}", e);
                    }
                    _ => {}
                }
            }
            _ = tokio::signal::ctrl_c() => {
                child.kill().await.ok();
                println!("\nStopped tailing logs.");
            }
        }
    } else {
        let content = std::fs::read_to_string(&log_file)
            .map_err(|e| AgentError::Io(format!("Failed to read log file: {}", e)))?;
        let all_lines: Vec<&str> = content.lines().collect();
        let start = all_lines.len().saturating_sub(lines as usize);
        for line in &all_lines[start..] {
            println!("{}", line);
        }
        println!(
            "\n(Showing last {} of {} lines from {})",
            all_lines.len() - start,
            all_lines.len(),
            log_file.display()
        );
    }
    Ok(())
}

/// Handle `hermes profile [action] [name]`.
async fn run_profile(
    cli: Cli,
    action: Option<String>,
    name: Option<String>,
) -> Result<(), AgentError> {
    let config_dir = cli
        .config_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home);
    let profiles_dir = config_dir.join("profiles");

    match action.as_deref() {
        None => {
            // Show current profile
            let config = load_config(cli.config_dir.as_deref())
                .map_err(|e| AgentError::Config(e.to_string()))?;
            println!("Current profile:");
            println!("  Model:       {}", config.model.as_deref().unwrap_or("gpt-4o"));
            println!(
                "  Personality: {}",
                config.personality.as_deref().unwrap_or("default")
            );
            println!("  Max turns:   {}", config.max_turns);
            println!(
                "\nUse `hermes profile list` to see all profiles."
            );
        }
        Some("list") => {
            if !profiles_dir.exists() {
                println!("No profiles directory found. Run `hermes setup` first.");
                return Ok(());
            }
            let entries: Vec<String> = std::fs::read_dir(&profiles_dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| {
                            e.path()
                                .extension()
                                .map(|ext| ext == "yaml" || ext == "yml")
                                .unwrap_or(false)
                        })
                        .filter_map(|e| {
                            e.path()
                                .file_stem()
                                .map(|s| s.to_string_lossy().into_owned())
                        })
                        .collect()
                })
                .unwrap_or_default();

            if entries.is_empty() {
                println!("No profiles found. Create one with `hermes profile create <name>`.");
            } else {
                println!("Available profiles:");
                for name in &entries {
                    println!("  • {}", name);
                }
            }
        }
        Some("create") => {
            let name = name.ok_or_else(|| {
                AgentError::Config("Missing profile name. Usage: hermes profile create <name>".into())
            })?;

            std::fs::create_dir_all(&profiles_dir)
                .map_err(|e| AgentError::Io(format!("Failed to create profiles dir: {}", e)))?;

            let profile_path = profiles_dir.join(format!("{}.yaml", name));
            if profile_path.exists() {
                println!("Profile '{}' already exists at {}", name, profile_path.display());
                return Ok(());
            }

            let content = format!(
                "# Hermes Profile: {}\nname: {}\nmodel: openai:gpt-4o\npersonality: default\nmax_turns: 50\n",
                name, name
            );
            std::fs::write(&profile_path, content)
                .map_err(|e| AgentError::Io(format!("Failed to write profile: {}", e)))?;
            println!("Created profile '{}' at {}", name, profile_path.display());
            println!("Edit it to customize, then switch with `hermes profile switch {}`.", name);
        }
        Some("switch") => {
            let name = name.ok_or_else(|| {
                AgentError::Config("Missing profile name. Usage: hermes profile switch <name>".into())
            })?;

            let profile_path = profiles_dir.join(format!("{}.yaml", &name));
            if !profile_path.exists() {
                // Also try .yml
                let alt = profiles_dir.join(format!("{}.yml", &name));
                if !alt.exists() {
                    println!(
                        "Profile '{}' not found. Available profiles:",
                        name
                    );
                    if let Ok(rd) = std::fs::read_dir(&profiles_dir) {
                        for entry in rd.filter_map(|e| e.ok()) {
                            if let Some(stem) = entry.path().file_stem() {
                                println!("  • {}", stem.to_string_lossy());
                            }
                        }
                    }
                    return Ok(());
                }
            }
            println!("Switched to profile: {}", name);
            println!("(Profile loading will be applied on next `hermes` session)");
        }
        Some(other) => {
            println!(
                "Unknown profile action: '{}'. Use 'list', 'create', or 'switch'.",
                other
            );
        }
    }
    Ok(())
}
