//! Oneshot query handling, OAuth auto-verify, and gateway keep-awake helpers.

use std::collections::HashSet;

use crate::App;
use crate::cli::Cli;
use hermes_core::AgentError;
use hermes_core::MessageRole;

use crate::auth_main::{normalize_auth_provider, provider_supports_oauth};

pub fn auth_error_message(err: &AgentError) -> Option<String> {
    match err {
        AgentError::LlmApi(msg)
        | AgentError::Config(msg)
        | AgentError::ToolExecution(msg)
        | AgentError::Gateway(msg)
        | AgentError::AuthFailed(msg) => Some(msg.to_ascii_lowercase()),
        _ => None,
    }
}

pub fn oneshot_auth_is_refreshable(message: &str) -> bool {
    message.contains("401")
        || message.contains("403")
        || message.contains("unauthorized")
        || message.contains("invalid token")
        || message.contains("token expired")
        || message.contains("authentication failed")
        || message.contains("invalid_grant")
        || message.contains("expired")
}

pub fn infer_oauth_provider_from_error_message(message: &str) -> Option<String> {
    if message.contains("portal.nousresearch.com")
        || message.contains("inference-api.nousresearch.com")
        || message.contains(" provider nous")
        || message.contains("nous:")
    {
        return Some("nous".to_string());
    }
    if message.contains("console.anthropic.com")
        || message.contains("claude.ai")
        || message.contains("anthropic")
    {
        return Some("anthropic".to_string());
    }
    if message.contains("chat.qwen.ai") || message.contains("dashscope") || message.contains("qwen")
    {
        return Some("qwen-oauth".to_string());
    }
    if message.contains("oauth2.googleapis.com")
        || message.contains("googleapis.com")
        || message.contains("gemini")
        || message.contains("google")
    {
        return Some("google-gemini-cli".to_string());
    }
    if message.contains("auth.openai.com")
        || message.contains("chatgpt.com")
        || message.contains("openai")
        || message.contains("codex")
    {
        if message.contains("codex") || message.contains("chatgpt.com") {
            return Some("openai-codex".to_string());
        }
        return Some("openai".to_string());
    }
    None
}

pub fn query_is_local_slash_command(query: &str) -> bool {
    query.trim_start().starts_with('/')
}

pub fn env_truthy(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on" | "auto"
            )
        })
        .unwrap_or(false)
}

pub fn oneshot_should_use_app_runtime(query: &str) -> bool {
    !query_is_local_slash_command(query)
        && (env_truthy("HERMES_ONESHOT_APP_RUNTIME") || env_truthy("HERMES_QUORUM_AUTO_ARM"))
}

#[cfg(target_os = "windows")]
pub fn start_gateway_keepawake_guard() -> Option<keepawake::KeepAwake> {
    if !gateway_running_on_ac_power() {
        tracing::info!("gateway keep-awake skipped on Windows: system is on battery");
        return None;
    }
    match keepawake::Builder::default()
        .idle(true)
        .sleep(true)
        .reason("Hermes Gateway is running")
        .app_name("Hermes Gateway")
        .create()
    {
        Ok(guard) => {
            tracing::info!("gateway keep-awake guard enabled on Windows");
            Some(guard)
        }
        Err(err) => {
            tracing::warn!("gateway keep-awake unavailable on Windows: {err}");
            None
        }
    }
}

#[cfg(target_os = "windows")]
fn gateway_running_on_ac_power() -> bool {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    let mut status = SYSTEM_POWER_STATUS {
        ACLineStatus: 0,
        BatteryFlag: 0,
        BatteryLifePercent: 0,
        SystemStatusFlag: 0,
        BatteryLifeTime: 0,
        BatteryFullLifeTime: 0,
    };
    let ok = unsafe { GetSystemPowerStatus(&mut status) } != 0;
    if !ok {
        tracing::warn!("failed to read Windows power status; defaulting to battery mode");
        return false;
    }

    status.ACLineStatus == 1
}

#[cfg(not(target_os = "windows"))]
pub fn start_gateway_keepawake_guard() {}

pub fn print_app_oneshot_result(app: &App) {
    if let Some(reply) = app.session.messages.iter().rev().find_map(|message| {
        if message.role == MessageRole::Assistant {
            message
                .content
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        } else {
            None
        }
    }) {
        println!("{}", reply);
    }
}

pub async fn handle_local_slash_query(cli: Cli, query: &str) -> Result<bool, AgentError> {
    if !query_is_local_slash_command(query) {
        return Ok(false);
    }
    let mut app = App::new(cli).await?;
    app.handle_input(query).await?;
    Ok(true)
}

pub fn oneshot_auto_verify_oauth_provider(
    err: &AgentError,
    provider_override: Option<&str>,
    model_override: Option<&str>,
) -> Option<String> {
    let Some(message) = auth_error_message(err) else {
        return None;
    };

    if !oneshot_auth_is_refreshable(&message) {
        return None;
    }

    let mut candidates: Vec<String> = Vec::new();
    if let Some(raw_provider) = provider_override.map(str::trim).filter(|v| !v.is_empty()) {
        candidates.push(normalize_auth_provider(raw_provider));
    }
    if let Some(raw_model_provider) = model_override
        .and_then(|m| m.split_once(':').map(|(provider, _)| provider.trim()))
        .filter(|v| !v.is_empty())
    {
        candidates.push(normalize_auth_provider(raw_model_provider));
    }
    if let Some(from_message) = infer_oauth_provider_from_error_message(&message) {
        candidates.push(from_message);
    }

    let mut seen = HashSet::new();
    for candidate in candidates {
        let normalized = normalize_auth_provider(&candidate);
        if !seen.insert(normalized.clone()) {
            continue;
        }
        if provider_supports_oauth(&normalized) {
            return Some(normalized);
        }
    }
    None
}
