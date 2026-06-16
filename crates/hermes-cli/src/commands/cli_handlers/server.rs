//! `hermes server` — remote LLM server account commands.

use super::server_config;
use std::time::Duration;

use hermes_config::{ServerConfig, hermes_home, load_config};
use hermes_core::AgentError;
use hermes_server_client::{
    AuthManager, AuthPollResult, AuthUserInput, ClawModelEntry, LoginMethod, ServerClientError,
    run_doctor,
};

pub async fn handle_cli_server(
    action: Option<String>,
    rest: Vec<String>,
    method: Option<String>,
    config_dir: Option<&str>,
) -> Result<(), AgentError> {
    let config = load_config(config_dir).map_err(|e| AgentError::Config(e.to_string()))?;
    let action = action
        .as_deref()
        .unwrap_or("help")
        .trim()
        .to_ascii_lowercase();

    match action.as_str() {
        "config" => {
            server_config::handle_server_config(&rest, config_dir, &config.server).await
        }
        "init" | "setup" => {
            server_config::handle_server_config(
                &["init".to_string()],
                config_dir,
                &config.server,
            )
            .await
        }
        "login" => server_login(&config.server, method.as_deref()).await,
        "logout" => server_logout(&config.server).await,
        "whoami" => server_whoami(&config.server).await,
        "profile" => server_profile(&config.server).await,
        "balance" => server_balance(&config.server).await,
        "models" => server_models(&config.server).await,
        "checkin" => server_checkin(&config.server).await,
        "bind-email" => server_bind_email(&config.server).await,
        "doctor" => server_doctor(&config.server, config_dir).await,
        "help" | "--help" | "-h" => {
            print_server_help();
            Ok(())
        }
        other => Err(AgentError::Config(format!(
            "unknown server subcommand '{other}'. Try: hermes server help"
        ))),
    }
}

fn print_server_help() {
    println!("Remote LLM server account (Flowy user API + OpenAI-compatible gateway)");
    println!();
    println!("Usage:");
    println!("  hermes server config init          Interactive setup (recommended first step)");
    println!("  hermes server config [show|set|get]");
    println!("  hermes server login              Interactive login (WeChat or email)");
    println!("  hermes server login --method wechat|email");
    println!("  hermes server logout");
    println!("  hermes server whoami");
    println!("  hermes server profile");
    println!("  hermes server balance");
    println!("  hermes server models             List cloud models + show current default");
    println!("  hermes server checkin");
    println!("  hermes server bind-email");
    println!("  hermes server doctor");
    println!();
    println!("Run `hermes server config help` for configuration keys.");
}

async fn server_login(config: &ServerConfig, method: Option<&str>) -> Result<(), AgentError> {
    ensure_api_ready(config)?;
    let login_method = match parse_method_arg(method) {
        Some(m) => m,
        None => prompt_login_method(config.auth.preferred_method.into()).await?,
    };

    let manager = AuthManager::new(config.clone(), hermes_home()).map_err(server_client_err)?;

    println!("Login method: {}", login_method.label());
    let mut pending = manager
        .start_login(login_method)
        .await
        .map_err(server_client_err)?;

    match login_method {
        LoginMethod::EmailOtp => run_email_login(&manager, &mut pending).await,
        LoginMethod::WechatQr => run_wechat_login(&manager, config, pending).await,
    }
}

async fn run_email_login(
    manager: &AuthManager,
    pending: &mut hermes_server_client::PendingLogin,
) -> Result<(), AgentError> {
    let email = prompt_line("Email: ").await?;
    let poll = manager
        .continue_login(pending, AuthUserInput::Email { address: email })
        .await
        .map_err(server_client_err)?;
    *pending = match poll {
        AuthPollResult::Pending(next) => next,
        AuthPollResult::Success(_) => {
            println!("Logged in successfully.");
            return Ok(());
        }
        AuthPollResult::Failed(msg) => {
            return Err(AgentError::Config(format!("login failed: {msg}")));
        }
    };

    let code = prompt_line("Verification code: ").await?;
    match manager
        .continue_login(pending, AuthUserInput::OtpCode { code })
        .await
        .map_err(server_client_err)?
    {
        AuthPollResult::Success(_) => {
            print_login_success(manager).await?;
            Ok(())
        }
        AuthPollResult::Pending(_) => Err(AgentError::Config(
            "login still pending after OTP — try again".into(),
        )),
        AuthPollResult::Failed(msg) => Err(AgentError::Config(format!("login failed: {msg}"))),
    }
}

async fn run_wechat_login(
    manager: &AuthManager,
    config: &ServerConfig,
    mut pending: hermes_server_client::PendingLogin,
) -> Result<(), AgentError> {
    println!(
        "WeChat Open Platform appid: {} (channel: {}, oauth_base: {})",
        config.effective_wechat_app_id(),
        config.channel,
        config.effective_wechat_base_url()
    );
    println!("{}", pending.message);
    if let Some(payload) = pending.qr_content.as_deref() {
        print_wechat_qr_terminal(payload)?;
    }
    if let Some(url) = pending.qr_image_url.as_deref() {
        println!("WeChat official QR image: {url}");
    }

    let interval = Duration::from_millis(config.auth.poll_interval_ms.max(500));
    loop {
        tokio::time::sleep(interval).await;
        match manager
            .continue_login(&pending, AuthUserInput::Poll)
            .await
            .map_err(server_client_err)?
        {
            AuthPollResult::Success(_) => {
                println!();
                print_login_success(manager).await?;
                return Ok(());
            }
            AuthPollResult::Pending(next) => {
                if next.message != pending.message {
                    println!("{}", next.message);
                }
                pending = next;
                print!(".");
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
            AuthPollResult::Failed(msg) => {
                println!();
                return Err(AgentError::Config(format!("login failed: {msg}")));
            }
        }
    }
}

fn print_wechat_qr_terminal(qr_payload: &str) -> Result<(), AgentError> {
    let code = qrcode::QrCode::new(qr_payload.as_bytes())
        .map_err(|e| AgentError::Config(format!("generate wechat qr: {e}")))?;
    println!();
    println!("{}", code.render::<char>().quiet_zone(false).build());
    println!();
    Ok(())
}

async fn server_models(config: &ServerConfig) -> Result<(), AgentError> {
    let manager = require_logged_in(config).await?;
    println!(
        "Current default remote model: {}",
        config.effective_default_llm_model()
    );
    if config.llm.default_model.trim().is_empty() {
        println!("  (built-in default — set with `hermes server config set llm_model <id>`)");
    }
    println!();
    let models = manager
        .list_claw_models(None)
        .await
        .map_err(server_client_err)?;
    if models.is_empty() {
        println!("No cloud models returned.");
        return Ok(());
    }
    println!("Available cloud models:");
    for entry in models {
        print_model_entry(&entry);
    }
    Ok(())
}

fn print_model_entry(entry: &ClawModelEntry) {
    println!("  - {} ({})", entry.name, entry.id);
    if !entry.endpoint.is_empty() {
        println!("      endpoint: {}", entry.endpoint);
    }
}

async fn print_login_success(manager: &AuthManager) -> Result<(), AgentError> {
    let profile = manager.fetch_profile().await.map_err(server_client_err)?;
    println!(
        "Logged in as {} (id={}).",
        profile.display_name(),
        profile.id
    );
    Ok(())
}

async fn server_logout(config: &ServerConfig) -> Result<(), AgentError> {
    if std::env::var("HERMES_SERVER_TOKEN")
        .ok()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        println!("HERMES_SERVER_TOKEN is set in the environment; unset it to fully logout.");
        return Ok(());
    }

    ensure_api_ready(config)?;
    let manager = AuthManager::new(config.clone(), hermes_home()).map_err(server_client_err)?;
    let removed = manager.logout().await.map_err(server_client_err)?;
    if removed {
        println!("Logged out from remote server.");
    } else {
        println!("No stored remote server credentials.");
    }
    Ok(())
}

async fn server_whoami(config: &ServerConfig) -> Result<(), AgentError> {
    ensure_api_ready(config)?;
    let manager = AuthManager::new(config.clone(), hermes_home()).map_err(server_client_err)?;
    let status = manager.whoami().await.map_err(server_client_err)?;

    println!("Remote server account");
    println!("  enabled: {}", status.server_enabled);
    println!(
        "  base_url: {}",
        if status.base_url.is_empty() {
            "(not set)"
        } else {
            status.base_url.as_str()
        }
    );
    println!("  token source: {}", status.source);

    if status.is_logged_in() {
        let expired = status.token_expired();
        println!(
            "  status: logged in{}",
            if expired {
                " (token may be expired)"
            } else {
                ""
            }
        );
        if let Some(profile) = status.cached_profile {
            println!("  user: {} (id={})", profile.display_name(), profile.id);
            if let Some(email) = profile.email.filter(|e| !e.is_empty()) {
                println!("  email: {email}");
            }
        }
    } else {
        println!("  status: not logged in — run `hermes server login`");
    }
    Ok(())
}

async fn server_profile(config: &ServerConfig) -> Result<(), AgentError> {
    let manager = require_logged_in(config).await?;
    let profile = manager.fetch_profile().await.map_err(server_client_err)?;
    println!("User profile");
    println!("  id: {}", profile.id);
    println!("  name: {}", profile.display_name());
    if let Some(email) = profile.email.filter(|e| !e.is_empty()) {
        println!("  email: {email}");
    }
    if let Some(phone) = profile.phone.filter(|e| !e.is_empty()) {
        println!("  phone: {phone}");
    }
    if let Some(channel) = profile.channel.filter(|e| !e.is_empty()) {
        println!("  channel: {channel}");
    }
    if let Some(plan) = profile.current_plan {
        println!("  current_plan: {plan}");
    }
    Ok(())
}

async fn server_balance(config: &ServerConfig) -> Result<(), AgentError> {
    let manager = require_logged_in(config).await?;
    let balance = manager.credits_balance().await.map_err(server_client_err)?;
    println!("Credits balance: {}", balance.balance);
    Ok(())
}

async fn server_checkin(config: &ServerConfig) -> Result<(), AgentError> {
    let manager = require_logged_in(config).await?;
    let time_zone = std::env::var("HERMES_TIMEZONE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "Asia/Shanghai".to_string());
    let result = manager
        .credits_checkin(&time_zone)
        .await
        .map_err(server_client_err)?;
    if result.already_checked_in {
        println!("Already checked in today. Balance: {}", result.balance);
    } else {
        println!(
            "Check-in successful: +{} points. Balance: {}",
            result.granted_points, result.balance
        );
    }
    Ok(())
}

async fn server_bind_email(config: &ServerConfig) -> Result<(), AgentError> {
    let manager = require_logged_in(config).await?;
    let email = prompt_line("Email to bind: ").await?;
    let req_no = manager
        .send_bind_email_code(&email)
        .await
        .map_err(server_client_err)?;
    println!("Verification code sent.");
    let code = prompt_line("Verification code: ").await?;
    let name = manager
        .bind_email(&email, &code, &req_no)
        .await
        .map_err(server_client_err)?;
    println!("Email bound. Updated profile: {name}");
    Ok(())
}

async fn server_doctor(config: &ServerConfig, config_dir: Option<&str>) -> Result<(), AgentError> {
    let _ = config_dir;
    let home = hermes_home();
    let report = run_doctor(config, &home).await;
    println!("Remote server diagnostics");
    for line in report.print_lines() {
        println!("  {line}");
    }
    if !report.all_ok() && config.enabled {
        return Err(AgentError::Config(
            "one or more server checks failed".to_string(),
        ));
    }
    Ok(())
}

async fn require_logged_in(config: &ServerConfig) -> Result<AuthManager, AgentError> {
    ensure_api_ready(config)?;
    let manager = AuthManager::new(config.clone(), hermes_home()).map_err(server_client_err)?;
    let status = manager.whoami().await.map_err(server_client_err)?;
    if !status.is_logged_in() {
        return Err(AgentError::Config(
            "not logged in — run `hermes server login` first".into(),
        ));
    }
    Ok(manager)
}

fn ensure_api_ready(config: &ServerConfig) -> Result<(), AgentError> {
    if !config.api_ready() {
        return Err(AgentError::Config(
            "server not configured — run `hermes server config init` first".into(),
        ));
    }
    Ok(())
}

async fn prompt_line(prompt: impl Into<String>) -> Result<String, AgentError> {
    let prompt = prompt.into();
    let line = tokio::task::spawn_blocking(move || {
        use std::io::{self, Write};
        print!("{prompt}");
        let _ = io::stdout().flush();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| AgentError::Io(format!("stdin task: {e}")))?
    .map_err(|e| AgentError::Io(format!("stdin: {e}")))?;
    Ok(line.trim().to_string())
}

fn parse_method_arg(method: Option<&str>) -> Option<LoginMethod> {
    method.and_then(LoginMethod::parse)
}

/// Resolve login method from interactive prompt input (`1`/`2` or aliases).
pub fn resolve_login_method_choice(input: &str, default: LoginMethod) -> LoginMethod {
    match input.trim().to_ascii_lowercase().as_str() {
        "" => default,
        "1" | "wechat" | "wechat_qr" | "wx" | "qr" => LoginMethod::WechatQr,
        "2" | "email" | "email_otp" | "otp" => LoginMethod::EmailOtp,
        other => LoginMethod::parse(other).unwrap_or(default),
    }
}

async fn prompt_login_method(default: LoginMethod) -> Result<LoginMethod, AgentError> {
    println!();
    println!("Choose login method:");
    println!("  1) WeChat QR scan");
    println!("  2) Email verification code");
    let default_hint = match default {
        LoginMethod::WechatQr => "1",
        LoginMethod::EmailOtp => "2",
    };
    let line = prompt_line(format!("Enter [1/2] [{default_hint}]: ")).await?;
    Ok(resolve_login_method_choice(&line, default))
}

fn server_client_err(err: ServerClientError) -> AgentError {
    match err {
        ServerClientError::Agent(e) => e,
        ServerClientError::Disabled => AgentError::Config(
            "server integration disabled (set server.enabled=true for LLM routing)".into(),
        ),
        ServerClientError::MissingBaseUrl => AgentError::Config(
            "server not configured — run `hermes server config init` first".into(),
        ),
        other => AgentError::Config(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_method_aliases() {
        assert_eq!(
            parse_method_arg(Some("wechat")),
            Some(LoginMethod::WechatQr)
        );
        assert_eq!(parse_method_arg(Some("email")), Some(LoginMethod::EmailOtp));
    }

    #[test]
    fn resolve_login_method_choice_respects_default() {
        assert_eq!(
            resolve_login_method_choice("", LoginMethod::EmailOtp),
            LoginMethod::EmailOtp
        );
        assert_eq!(
            resolve_login_method_choice("1", LoginMethod::EmailOtp),
            LoginMethod::WechatQr
        );
        assert_eq!(
            resolve_login_method_choice("2", LoginMethod::WechatQr),
            LoginMethod::EmailOtp
        );
    }
}
