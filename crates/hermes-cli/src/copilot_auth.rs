//! GitHub OAuth device flow — obtain a token suitable for `GITHUB_COPILOT_TOKEN`.
//!
//! Uses the same public OAuth application as [GitHub CLI](https://cli.github.com/)
//! unless overridden with `HERMES_GITHUB_DEVICE_CLIENT_ID`.
//!
//! After authorization, set `GITHUB_COPILOT_TOKEN` (or add to `$HERMES_HOME/.env`).

use std::time::Duration;

use hermes_core::AgentError;
use serde::Deserialize;

/// Default: GitHub CLI OAuth app id (public; used by `gh auth login` device flow).
const DEFAULT_DEVICE_CLIENT_ID: &str = "Iv23liCLQ87g8RVSjNGp";

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    error: Option<String>,
    error_description: Option<String>,
    device_code: Option<String>,
    user_code: Option<String>,
    verification_uri: Option<String>,
    expires_in: Option<u64>,
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AccessTokenResponse {
    access_token: Option<String>,
    token_type: Option<String>,
    scope: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn form_body(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                urlencoding::encode_query_component(k),
                urlencoding::encode_query_component(v)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

mod urlencoding {
    pub fn encode_query_component(s: &str) -> String {
        let mut out = String::new();
        for b in s.as_bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(char::from(*b))
                }
                _ => out.push_str(&format!("%{:02X}", b)),
            }
        }
        out
    }
}

/// Run GitHub device flow and return the OAuth access token (store as `GITHUB_COPILOT_TOKEN`).
pub async fn start_copilot_device_flow() -> Result<String, AgentError> {
    let client_id = std::env::var("HERMES_GITHUB_DEVICE_CLIENT_ID")
        .unwrap_or_else(|_| DEFAULT_DEVICE_CLIENT_ID.to_string());
    let scope = std::env::var("HERMES_GITHUB_DEVICE_SCOPE")
        .unwrap_or_else(|_| "read:org read:user repo".to_string());

    let http = reqwest::Client::builder()
        .user_agent(format!("hermes-agent/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| AgentError::Io(e.to_string()))?;

    let body = form_body(&[("client_id", &client_id), ("scope", &scope)]);

    let dcr: DeviceCodeResponse = http
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("device/code request: {e}")))?
        .json()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("device/code JSON: {e}")))?;

    if let Some(err) = &dcr.error {
        let desc = dcr.error_description.as_deref().unwrap_or("");
        return Err(AgentError::AuthFailed(format!(
            "GitHub device/code: {err}: {desc}"
        )));
    }

    let device_code = dcr
        .device_code
        .clone()
        .ok_or_else(|| AgentError::AuthFailed("GitHub device/code: missing device_code".into()))?;
    let user_code = dcr
        .user_code
        .clone()
        .ok_or_else(|| AgentError::AuthFailed("GitHub device/code: missing user_code".into()))?;
    let verification_uri = dcr.verification_uri.clone().ok_or_else(|| {
        AgentError::AuthFailed("GitHub device/code: missing verification_uri".into())
    })?;
    let expires_in = dcr.expires_in.unwrap_or(900);

    println!("\nGitHub device authorization");
    println!("===========================");
    println!("Open: {}", verification_uri);
    println!("Enter code: {}", user_code);
    println!("\nWaiting for authorization (timeout {}s)…", expires_in);

    let poll_interval = Duration::from_secs(dcr.interval.unwrap_or(5).max(5));
    let deadline = std::time::Instant::now() + Duration::from_secs(expires_in.max(60));

    loop {
        if std::time::Instant::now() > deadline {
            return Err(AgentError::AuthFailed(
                "Device authorization timed out".into(),
            ));
        }

        tokio::time::sleep(poll_interval).await;

        let poll_body = form_body(&[
            ("client_id", &client_id),
            ("device_code", &device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ]);

        let resp = http
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(poll_body)
            .send()
            .await
            .map_err(|e| AgentError::AuthFailed(format!("access_token poll: {e}")))?;

        let txt = resp
            .text()
            .await
            .map_err(|e| AgentError::AuthFailed(format!("access_token body: {e}")))?;

        let token_resp: AccessTokenResponse = serde_json::from_str(&txt).map_err(|_| {
            AgentError::AuthFailed(format!("Unexpected token response (not JSON): {txt}"))
        })?;

        if let Some(tok) = token_resp.access_token.filter(|s| !s.is_empty()) {
            println!("\nAuthorization succeeded.");
            println!("Add to your environment or `$HERMES_HOME/.env`:");
            println!("  GITHUB_COPILOT_TOKEN=<token>");
            println!(
                "\nScopes granted: {}",
                token_resp.scope.as_deref().unwrap_or("(unknown)")
            );
            return Ok(tok);
        }

        match token_resp.error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            Some(other) => {
                let desc = token_resp.error_description.clone().unwrap_or_default();
                return Err(AgentError::AuthFailed(format!(
                    "GitHub OAuth error: {other}: {desc}"
                )));
            }
            None => {
                return Err(AgentError::AuthFailed(
                    "GitHub OAuth: empty response without error field".into(),
                ));
            }
        }
    }
}
