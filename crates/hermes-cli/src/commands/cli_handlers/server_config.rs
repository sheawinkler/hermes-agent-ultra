//! `hermes server config` — command-line server connection setup.

use std::path::{Path, PathBuf};

use hermes_config::{
    DEFAULT_WECHAT_FLOWY_SERVER_BASE, ServerConfig, apply_user_config_patch,
    default_wechat_app_id_for_channel, hermes_home, load_user_config_file, save_config_yaml,
    user_config_field_display, validate_config,
};
use hermes_core::AgentError;

const DEFAULT_BASE_URL: &str = "https://server.flowyaipc.cn/claw";
const DEFAULT_CHANNEL: &str = "flowy";
const DEFAULT_APP: &str = "flowymes";

pub fn config_yaml_path(config_dir: Option<&str>) -> PathBuf {
    config_dir
        .map(PathBuf::from)
        .unwrap_or_else(hermes_home)
        .join("config.yaml")
}

pub fn normalize_server_config_key(key: &str) -> Result<String, AgentError> {
    let normalized = key.trim().to_ascii_lowercase().replace('-', "_");
    let full = match normalized.as_str() {
        "base_url" | "url" | "baseurl" => "server.base_url".to_string(),
        "wechat_base_url" | "wechat_url" | "wechat_baseurl" => {
            "server.wechat_base_url".to_string()
        }
        "channel" => "server.channel".to_string(),
        "app" => "server.app".to_string(),
        "invite_code" | "invite" => "server.invite_code".to_string(),
        "enabled" => "server.enabled".to_string(),
        "preferred_method" | "login_method" | "login_default" => {
            "server.auth.preferred_method".to_string()
        }
        "llm_model" | "default_model" | "server_model" => {
            "server.llm.default_model".to_string()
        }
        "wechat_app_id" | "wx_app_id" => "server.auth.wechat_app_id".to_string(),
        "poll_interval_ms" | "poll_interval" => "server.auth.poll_interval_ms".to_string(),
        "otp_ttl_seconds" | "otp_ttl" => "server.auth.otp_ttl_seconds".to_string(),
        "heartbeat_interval_secs" | "heartbeat_interval" => {
            "server.auth.heartbeat_interval_secs".to_string()
        }
        other if other.starts_with("server.") => other.to_string(),
        _other => {
            return Err(AgentError::Config(format!(
                "unknown server config key '{key}' (try: base_url, channel, app, enabled, llm_model, login_default)"
            )));
        }
    };
    Ok(full)
}

pub fn save_server_field(
    config_dir: Option<&str>,
    key: &str,
    value: &str,
) -> Result<PathBuf, AgentError> {
    let cfg_path = config_yaml_path(config_dir);
    let mut disk = load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;
    let full_key = normalize_server_config_key(key)?;
    apply_user_config_patch(&mut disk, &full_key, value)
        .map_err(|e| AgentError::Config(e.to_string()))?;
    validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
    save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
    Ok(cfg_path)
}

pub fn print_server_config(config: &ServerConfig, cfg_path: &Path) {
    println!("Remote server configuration");
    println!("  config file: {}", cfg_path.display());
    println!("  enabled: {}", config.enabled);
    println!(
        "  base_url: {}",
        display_opt_str(config.base_url.trim())
    );
    println!(
        "  wechat_base_url: {}",
        if config.wechat_base_url.trim().is_empty() {
            format!(
                "(domestic default → {})",
                config.effective_wechat_base_url()
            )
        } else {
            config.wechat_base_url.clone()
        }
    );
    println!("  channel: {}", config.channel);
    println!("  app: {}", config.app);
    println!(
        "  wechat_app_id: {} (channel={})",
        config.effective_wechat_app_id(),
        config.channel
    );
    println!(
        "  invite_code: {}",
        display_opt_str(config.invite_code.trim())
    );
    println!(
        "  login_default: {} (Enter key at login prompt)",
        config.auth.preferred_method.as_str()
    );
    println!(
        "  llm_model: {}",
        if config.llm.default_model.trim().is_empty() {
            format!(
                "(built-in default → {})",
                config.effective_default_llm_model()
            )
        } else {
            config.llm.default_model.clone()
        }
    );
    println!("  poll_interval_ms: {}", config.auth.poll_interval_ms);
    println!("  otp_ttl_seconds: {}", config.auth.otp_ttl_seconds);
    println!(
        "  heartbeat_interval_secs: {}",
        config.auth.heartbeat_interval_secs
    );
}

pub fn print_config_help() {
    println!("Configure remote Flowy server connection (saved to config.yaml)");
    println!();
    println!("Usage:");
    println!("  hermes server config              Show current settings");
    println!("  hermes server config init         Interactive setup wizard");
    println!("  hermes server config set <key> <value>");
    println!("  hermes server config get <key>");
    println!("  hermes server config path         Show config.yaml path");
    println!();
    println!("Keys (short names for `set` / `get`):");
    println!("  base_url          Flowy API root (required for login)");
    println!("  wechat_base_url   WeChat OAuth API root (default: domestic server.flowyaipc.cn/claw)");
    println!("  channel           Brand channel (default: flowy)");
    println!("  app               Client app id (default: flowymes)");
    println!("  wechat_app_id     WeChat Open Platform app id (default: by channel)");
    println!("  invite_code       Optional invite code");
    println!("  enabled           Enable remote LLM routing (true/false)");
    println!("  llm_model         Remote LLM model id (default: AIPC-glm-4.7)");
    println!("  login_default     Default option when login prompt is skipped (wechat|email)");
    println!();
    println!("Examples:");
    println!("  hermes server config init");
    println!("  hermes server config set base_url https://server.flowyaipc.cn/claw");
    println!("  hermes server config set llm_model AIPC-glm-4.7");
}

pub async fn handle_server_config(
    rest: &[String],
    config_dir: Option<&str>,
    loaded: &ServerConfig,
) -> Result<(), AgentError> {
    let cfg_path = config_yaml_path(config_dir);
    match rest.first().map(|s| s.as_str()) {
        None | Some("show") => {
            print_server_config(loaded, &cfg_path);
            Ok(())
        }
        Some("help") | Some("--help") | Some("-h") => {
            print_config_help();
            Ok(())
        }
        Some("path") => {
            println!("{}", cfg_path.display());
            Ok(())
        }
        Some("init") | Some("setup") => run_config_init(config_dir).await,
        Some("set") => {
            let key = rest.get(1).ok_or_else(|| {
                AgentError::Config("usage: hermes server config set <key> <value>".into())
            })?;
            let value = rest.get(2..).ok_or_else(|| {
                AgentError::Config("usage: hermes server config set <key> <value>".into())
            })?;
            if value.is_empty() {
                return Err(AgentError::Config(
                    "usage: hermes server config set <key> <value>".into(),
                ));
            }
            let value = value.join(" ");
            let saved = save_server_field(config_dir, key, &value)?;
            let full_key = normalize_server_config_key(key)?;
            println!("Saved {full_key} = {value} → {}", saved.display());
            Ok(())
        }
        Some("get") => {
            let key = rest.get(1).ok_or_else(|| {
                AgentError::Config("usage: hermes server config get <key>".into())
            })?;
            let full_key = normalize_server_config_key(key)?;
            let disk = load_user_config_file(&cfg_path)
                .map_err(|e| AgentError::Config(e.to_string()))?;
            match user_config_field_display(&disk, &full_key) {
                Ok(value) => println!("{value}"),
                Err(e) => return Err(AgentError::Config(e.to_string())),
            }
            Ok(())
        }
        Some(other) => Err(AgentError::Config(format!(
            "unknown server config subcommand '{other}'. Try: hermes server config help"
        ))),
    }
}

async fn run_config_init(config_dir: Option<&str>) -> Result<(), AgentError> {
    println!("Remote server setup wizard");
    println!("Press Enter to accept [default] values.\n");

    let base_url = prompt_with_default(
        "Flowy API base URL",
        DEFAULT_BASE_URL,
    )
    .await?;
    if base_url.trim().is_empty() {
        return Err(AgentError::Config("base_url is required".into()));
    }

    let channel = prompt_with_default("Brand channel", DEFAULT_CHANNEL).await?;
    let app = prompt_with_default("Client app identifier", DEFAULT_APP).await?;
    let wechat_app_id = default_wechat_app_id_for_channel(channel.trim());

    let wechat_default = DEFAULT_WECHAT_FLOWY_SERVER_BASE.to_string();
    let wechat_raw = prompt_with_default(
        "WeChat OAuth API base (Enter = domestic default)",
        &wechat_default,
    )
    .await?;
    let wechat_base_url = if wechat_raw.trim().is_empty()
        || wechat_raw.trim() == DEFAULT_WECHAT_FLOWY_SERVER_BASE
    {
        String::new()
    } else {
        wechat_raw.trim().trim_end_matches('/').to_string()
    };

    let invite_code = prompt_optional("Invite code (optional, Enter to skip)").await?;

    let enable_llm = prompt_with_default(
        "Enable remote LLM routing now? (true/false)",
        "false",
    )
    .await?;

    let llm_model = if enable_llm.trim().eq_ignore_ascii_case("true") {
        prompt_optional(
            "Default remote LLM model id (Enter = AIPC-glm-4.7)",
        )
        .await?
        .unwrap_or_default()
    } else {
        String::new()
    };

    let cfg_path = config_yaml_path(config_dir);
    let patches = [
        ("base_url", base_url.trim().trim_end_matches('/')),
        ("channel", channel.trim()),
        ("app", app.trim()),
        ("wechat_app_id", wechat_app_id.as_str()),
        ("wechat_base_url", wechat_base_url.as_str()),
        ("invite_code", invite_code.as_deref().unwrap_or("")),
        ("enabled", enable_llm.trim()),
        ("llm_model", llm_model.as_str()),
    ];

    for (key, value) in patches {
        save_server_field(config_dir, key, value)?;
    }

    println!();
    println!("Server configuration saved → {}", cfg_path.display());
    println!("Next: run `hermes server doctor`, then `hermes server login` (choose WeChat or email).");
    Ok(())
}

fn display_opt_str(value: &str) -> String {
    if value.is_empty() {
        "(not set)".to_string()
    } else {
        value.to_string()
    }
}

async fn prompt_with_default(label: &str, default: &str) -> Result<String, AgentError> {
    let prompt = if default.is_empty() {
        format!("{label}: ")
    } else {
        format!("{label} [{default}]: ")
    };
    let line = prompt_line(&prompt).await?;
    if line.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(line)
    }
}

async fn prompt_optional(label: &str) -> Result<Option<String>, AgentError> {
    let line = prompt_line(&format!("{label}: ")).await?;
    if line.is_empty() {
        Ok(None)
    } else {
        Ok(Some(line))
    }
}

async fn prompt_line(prompt: &str) -> Result<String, AgentError> {
    let line = tokio::task::spawn_blocking({
        let prompt = prompt.to_string();
        move || {
            use std::io::{self, Write};
            print!("{prompt}");
            let _ = io::stdout().flush();
            let mut buf = String::new();
            io::stdin().read_line(&mut buf).map(|_| buf)
        }
    })
    .await
    .map_err(|e| AgentError::Io(format!("stdin task: {e}")))?
    .map_err(|e| AgentError::Io(format!("stdin: {e}")))?;
    Ok(line.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_key_aliases() {
        assert_eq!(
            normalize_server_config_key("base-url").unwrap(),
            "server.base_url"
        );
        assert_eq!(
            normalize_server_config_key("login_method").unwrap(),
            "server.auth.preferred_method"
        );
        assert_eq!(
            normalize_server_config_key("llm_model").unwrap(),
            "server.llm.default_model"
        );
        assert_eq!(
            normalize_server_config_key("server.channel").unwrap(),
            "server.channel"
        );
    }
}
