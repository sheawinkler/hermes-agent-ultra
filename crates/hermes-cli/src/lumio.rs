//! Lumio API Gateway — OAuth 2.0 Authorization Code Flow.
//!
//! Lumio (lumio.run) is an API gateway supporting 300+ models.
//! This module handles:
//!   1. Start local HTTP server on a random port (callback listener)
//!   2. Open browser to Lumio's OAuth authorize endpoint
//!   3. User logs in and authorizes on Lumio's website
//!   4. Lumio redirects back to localhost with authorization code
//!   5. Exchange code for access_token
//!   6. Persist token to ~/.hermes/lumio.json and configure provider

use std::collections::HashMap;
use std::io::Write;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hermes_core::AgentError;
use tokio::sync::oneshot;

// ── Lumio defaults ──────────────────────────────────────────────────────

const LUMIO_BASE_URL: &str = "https://lumio.run";
const LUMIO_API_URL: &str = "https://api.lumio.run";
const LUMIO_CLIENT_ID: &str = "hermes-agent";
const LUMIO_DEFAULT_MODEL: &str = "deepseek/deepseek-chat";
const LOGIN_TIMEOUT_SECONDS: u64 = 300;

fn lumio_base_url() -> String {
    std::env::var("LUMIO_BASE_URL").unwrap_or_else(|_| LUMIO_BASE_URL.to_string())
}

fn lumio_api_url() -> String {
    std::env::var("LUMIO_API_URL").unwrap_or_else(|_| LUMIO_API_URL.to_string())
}

fn lumio_client_id() -> String {
    std::env::var("LUMIO_CLIENT_ID").unwrap_or_else(|_| LUMIO_CLIENT_ID.to_string())
}

// ── Token persistence ───────────────────────────────────────────────────

fn token_path() -> PathBuf {
    hermes_config::paths::hermes_home().join("lumio.json")
}

/// Saved Lumio credential.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LumioToken {
    pub token: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub created_at: f64,
    #[serde(default)]
    pub base_url: String,
}

/// Persist Lumio token to ~/.hermes/lumio.json.
pub fn save_token(token: &str, username: &str) -> Result<(), AgentError> {
    let path = token_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("Failed to create dir: {e}")))?;
    }
    let data = LumioToken {
        token: token.to_string(),
        username: username.to_string(),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64(),
        base_url: lumio_api_url(),
    };
    let json = serde_json::to_string_pretty(&data)
        .map_err(|e| AgentError::Config(format!("JSON serialize: {e}")))?;
    std::fs::write(&path, &json)
        .map_err(|e| AgentError::Io(format!("Failed to write {}: {e}", path.display())))?;
    // Restrict permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Load saved Lumio token. Returns None if not found or invalid.
pub fn load_token() -> Option<LumioToken> {
    let path = token_path();
    let content = std::fs::read_to_string(&path).ok()?;
    let data: LumioToken = serde_json::from_str(&content).ok()?;
    if data.token.is_empty() {
        return None;
    }
    Some(data)
}

/// Remove saved Lumio token.
pub fn clear_token() {
    let _ = std::fs::remove_file(token_path());
}

// ── OAuth 2.0 Authorization Code Flow ──────────────────────────────────

/// Result of the OAuth login flow.
#[derive(Debug, Clone)]
pub struct LoginResult {
    pub success: bool,
    pub token: Option<String>,
    pub username: Option<String>,
    pub error: Option<String>,
}

/// Run the Lumio OAuth 2.0 Authorization Code Flow.
///
/// 1. Starts a local HTTP server on a random port
/// 2. Opens the browser to Lumio's authorize endpoint
/// 3. Waits for the callback with the authorization code
/// 4. Exchanges the code for an access token
/// 5. Fetches user info
pub async fn login(open_browser: bool) -> LoginResult {
    let state = format!("{:032x}", rand_u128());

    // Bind to a random port
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            return LoginResult {
                success: false,
                token: None,
                username: None,
                error: Some(format!("Failed to bind local server: {e}")),
            };
        }
    };
    let port = listener.local_addr().unwrap().port();
    let redirect_uri = format!("http://localhost:{port}/callback");

    let authorize_url = format!(
        "{}/api/oauth2/authorize?client_id={}&redirect_uri={}&response_type=code&scope=read&state={}",
        lumio_base_url(),
        lumio_client_id(),
        urlencoding::encode(&redirect_uri),
        &state,
    );

    println!();
    println!("🌐 Please authorize in your browser:");
    println!("   {authorize_url}");
    println!();
    println!("   Waiting for authorization...");
    println!();

    if open_browser {
        open_url(&authorize_url);
    }

    // Channel to receive the callback result
    let (tx, rx) = oneshot::channel::<LoginResult>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let state_clone = state.clone();

    // Spawn blocking listener in a separate thread
    let handle = tokio::task::spawn_blocking(move || {
        listener
            .set_nonblocking(false)
            .expect("set_nonblocking failed");
        // Accept one connection with timeout
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(LOGIN_TIMEOUT_SECONDS);

        loop {
            if std::time::Instant::now() > deadline {
                if let Some(sender) = tx.lock().unwrap().take() {
                    let _ = sender.send(LoginResult {
                        success: false,
                        token: None,
                        username: None,
                        error: Some("Authorization timed out (5 minutes)".into()),
                    });
                }
                return;
            }

            // Set a short accept timeout so we can check the deadline
            listener
                .set_nonblocking(true)
                .expect("set_nonblocking failed");
            match listener.accept() {
                Ok((mut stream, _)) => {
                    // Read the HTTP request
                    let mut buf = [0u8; 4096];
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                        .ok();
                    let n = match stream.read(&mut buf) {
                        Ok(n) => n,
                        Err(_) => continue,
                    };
                    let request = String::from_utf8_lossy(&buf[..n]);

                    // Parse the GET request line
                    let first_line = request.lines().next().unwrap_or("");
                    if !first_line.starts_with("GET /callback") {
                        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\n\r\n");
                        continue;
                    }

                    // Extract query string
                    let path = first_line.split_whitespace().nth(1).unwrap_or("/callback");
                    let query_str = path.split('?').nth(1).unwrap_or("");
                    let params = parse_query(query_str);

                    // CSRF check
                    let returned_state = params.get("state").cloned().unwrap_or_default();
                    if returned_state != state_clone {
                        let html = error_page("State validation failed, please retry");
                        let _ = write_http_response(&mut stream, 400, &html);
                        if let Some(sender) = tx.lock().unwrap().take() {
                            let _ = sender.send(LoginResult {
                                success: false,
                                token: None,
                                username: None,
                                error: Some("State mismatch (CSRF)".into()),
                            });
                        }
                        return;
                    }

                    // Check for error
                    if let Some(err) = params.get("error") {
                        let desc = params
                            .get("error_description")
                            .cloned()
                            .unwrap_or_else(|| err.clone());
                        let html = error_page(&desc);
                        let _ = write_http_response(&mut stream, 200, &html);
                        if let Some(sender) = tx.lock().unwrap().take() {
                            let _ = sender.send(LoginResult {
                                success: false,
                                token: None,
                                username: None,
                                error: Some(desc),
                            });
                        }
                        return;
                    }

                    // Got authorization code
                    let code = match params.get("code") {
                        Some(c) if !c.is_empty() => c.clone(),
                        _ => {
                            let html = error_page("No authorization code received");
                            let _ = write_http_response(&mut stream, 400, &html);
                            if let Some(sender) = tx.lock().unwrap().take() {
                                let _ = sender.send(LoginResult {
                                    success: false,
                                    token: None,
                                    username: None,
                                    error: Some("No authorization code".into()),
                                });
                            }
                            return;
                        }
                    };

                    // Exchange code for token (blocking HTTP call)
                    let redirect = format!("http://localhost:{port}/callback");
                    match exchange_code_blocking(&code, &redirect) {
                        Ok((token, username)) => {
                            let html = success_page(&username);
                            let _ = write_http_response(&mut stream, 200, &html);
                            if let Some(sender) = tx.lock().unwrap().take() {
                                let _ = sender.send(LoginResult {
                                    success: true,
                                    token: Some(token),
                                    username: Some(username),
                                    error: None,
                                });
                            }
                        }
                        Err(e) => {
                            let html = error_page(&format!("Token exchange failed: {e}"));
                            let _ = write_http_response(&mut stream, 200, &html);
                            if let Some(sender) = tx.lock().unwrap().take() {
                                let _ = sender.send(LoginResult {
                                    success: false,
                                    token: None,
                                    username: None,
                                    error: Some(format!("Token exchange failed: {e}")),
                                });
                            }
                        }
                    }
                    return;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    continue;
                }
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    continue;
                }
            }
        }
    });

    // Wait for result
    match tokio::time::timeout(
        std::time::Duration::from_secs(LOGIN_TIMEOUT_SECONDS + 5),
        rx,
    )
    .await
    {
        Ok(Ok(result)) => result,
        _ => {
            handle.abort();
            LoginResult {
                success: false,
                token: None,
                username: None,
                error: Some("Authorization timed out".into()),
            }
        }
    }
}

/// Full Lumio setup flow: login → save token → configure provider.
pub async fn setup(model: Option<&str>, open_browser: bool) -> Result<bool, AgentError> {
    let model = model.unwrap_or(LUMIO_DEFAULT_MODEL);

    println!();
    println!("🔑 Lumio API Gateway Login");
    println!("   Default model: {model}");

    let result = login(open_browser).await;

    if !result.success {
        let err = result.error.unwrap_or_else(|| "Login failed".into());
        println!("\n❌ {err}");
        return Ok(false);
    }

    let token = result.token.unwrap_or_default();
    let username = result.username.unwrap_or_default();

    println!(
        "\n✅ Login successful! User: {}",
        if username.is_empty() {
            "(unknown)"
        } else {
            &username
        }
    );

    // Save token
    save_token(&token, &username)?;
    println!("   Token saved to ~/.hermes/lumio.json");

    // Configure provider
    configure_provider(&token, model)?;

    println!("\n🚀 Lumio configured! Model: {model}");
    println!("   Top up: {LUMIO_BASE_URL}/console/topup");
    println!();
    Ok(true)
}

/// Write Lumio credentials into hermes config.
fn configure_provider(token: &str, model: &str) -> Result<(), AgentError> {
    // Save to .env file
    let env_path = hermes_config::paths::hermes_home().join(".env");
    let mut env_content = std::fs::read_to_string(&env_path).unwrap_or_default();
    // Update or append LUMIO_API_KEY
    if env_content.contains("LUMIO_API_KEY=") {
        let lines: Vec<String> = env_content
            .lines()
            .map(|l| {
                if l.starts_with("LUMIO_API_KEY=") {
                    format!("LUMIO_API_KEY={token}")
                } else {
                    l.to_string()
                }
            })
            .collect();
        env_content = lines.join("\n");
    } else {
        if !env_content.is_empty() && !env_content.ends_with('\n') {
            env_content.push('\n');
        }
        env_content.push_str(&format!("LUMIO_API_KEY={token}\n"));
    }
    std::fs::write(&env_path, &env_content)
        .map_err(|e| AgentError::Io(format!("Failed to write .env: {e}")))?;

    // Update config.yaml
    let config_path = hermes_config::paths::config_path();
    let mut config_content = std::fs::read_to_string(&config_path).unwrap_or_default();
    // Simple YAML update — set provider and model
    if config_content.is_empty() {
        config_content = format!(
            "model:\n  provider: lumio\n  base_url: {}\n  default: {}\n",
            lumio_api_url(),
            model,
        );
    } else {
        // Append or update model section
        if !config_content.contains("provider: lumio") {
            config_content.push_str(&format!(
                "\nmodel:\n  provider: lumio\n  base_url: {}\n  default: {}\n",
                lumio_api_url(),
                model,
            ));
        }
    }
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&config_path, &config_content)
        .map_err(|e| AgentError::Io(format!("Failed to write config: {e}")))?;

    Ok(())
}

/// Format a top-up hint for the user.
pub fn topup_hint() -> String {
    format!("💰 Low balance? Top up: {}/console/topup", lumio_base_url())
}

// ── Internal helpers ────────────────────────────────────────────────────

use std::io::Read;

/// Exchange authorization code for access token (blocking).
fn exchange_code_blocking(code: &str, redirect_uri: &str) -> Result<(String, String), String> {
    let client = reqwest::blocking::Client::new();

    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "code": code,
        "client_id": lumio_client_id(),
        "redirect_uri": redirect_uri,
    });

    let resp = client
        .post(format!("{}/api/oauth2/token", lumio_base_url()))
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .map_err(|e| format!("HTTP error: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(format!("HTTP {status}: {body}"));
    }

    let data: serde_json::Value = resp.json().map_err(|e| format!("JSON parse: {e}"))?;
    let token = data
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or("No access_token in response")?
        .to_string();

    // Fetch user info (optional, don't fail on error)
    let username = fetch_userinfo_blocking(&token).unwrap_or_default();

    Ok((token, username))
}

/// Fetch user info from Lumio (blocking).
fn fetch_userinfo_blocking(token: &str) -> Result<String, String> {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(format!("{}/api/oauth2/userinfo", lumio_base_url()))
        .header("Authorization", format!("Bearer {token}"))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .map_err(|e| format!("{e}"))?;

    let data: serde_json::Value = resp.json().map_err(|e| format!("{e}"))?;
    Ok(data
        .get("username")
        .or_else(|| data.get("email"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string())
}

/// Parse a query string into a HashMap.
fn parse_query(query: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            map.insert(
                urlencoding::decode(k).unwrap_or_default().into_owned(),
                urlencoding::decode(v).unwrap_or_default().into_owned(),
            );
        }
    }
    map
}

/// Write an HTTP response with HTML body.
fn write_http_response(
    stream: &mut std::net::TcpStream,
    status: u16,
    html: &str,
) -> std::io::Result<()> {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        html.len(),
        html,
    );
    stream.write_all(response.as_bytes())
}

/// Open a URL in the default browser.
fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .spawn();
    }
}

/// Generate a random u128 (simple, no external crate needed).
fn rand_u128() -> u128 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let s = RandomState::new();
    let mut h = s.build_hasher();
    h.write_u64(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64,
    );
    let a = h.finish() as u128;
    let mut h2 = s.build_hasher();
    h2.write_u64(a as u64 ^ 0xdeadbeef);
    let b = h2.finish() as u128;
    (a << 64) | b
}

/// HTML success page shown after authorization.
fn success_page(username: &str) -> String {
    let user_line = if username.is_empty() {
        String::new()
    } else {
        format!(r#"<p style="color:#6c63ff;margin-top:12px">User: {username}</p>"#)
    };
    format!(
        r#"<!DOCTYPE html><html><head><meta charset="UTF-8"><title>Authorization Successful</title>
<style>body{{font-family:-apple-system,sans-serif;display:flex;align-items:center;
justify-content:center;min-height:100vh;background:#0f0c29;color:#fff}}
.c{{text-align:center}}.c .i{{font-size:64px;margin-bottom:16px}}</style></head>
<body><div class="c"><div class="i">✅</div><h2>Authorization Successful</h2>
<p style="color:#888">Done! You can close this page and return to the terminal.</p>{user_line}</div></body></html>"#
    )
}

/// HTML error page shown on authorization failure.
fn error_page(message: &str) -> String {
    format!(
        r#"<!DOCTYPE html><html><head><meta charset="UTF-8"><title>Authorization Failed</title>
<style>body{{font-family:-apple-system,sans-serif;display:flex;align-items:center;
justify-content:center;min-height:100vh;background:#0f0c29;color:#fff}}
.c{{text-align:center}}.c .i{{font-size:64px;margin-bottom:16px}}</style></head>
<body><div class="c"><div class="i">❌</div><h2>Authorization Failed</h2>
<p style="color:#ff6b6b">{message}</p></div></body></html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_query() {
        let params = parse_query("code=abc123&state=xyz&scope=read");
        assert_eq!(params.get("code").unwrap(), "abc123");
        assert_eq!(params.get("state").unwrap(), "xyz");
        assert_eq!(params.get("scope").unwrap(), "read");
    }

    #[test]
    fn test_parse_query_encoded() {
        let params = parse_query("redirect_uri=http%3A%2F%2Flocalhost%3A8080%2Fcallback");
        assert_eq!(
            params.get("redirect_uri").unwrap(),
            "http://localhost:8080/callback"
        );
    }

    #[test]
    fn test_parse_query_empty() {
        let params = parse_query("");
        assert!(params.is_empty());
    }

    #[test]
    fn test_success_page_with_username() {
        let html = success_page("testuser");
        assert!(html.contains("testuser"));
        assert!(html.contains("Authorization Successful"));
    }

    #[test]
    fn test_success_page_without_username() {
        let html = success_page("");
        assert!(!html.contains("User:"));
    }

    #[test]
    fn test_error_page() {
        let html = error_page("Something went wrong");
        assert!(html.contains("Something went wrong"));
        assert!(html.contains("Authorization Failed"));
    }

    #[test]
    fn test_save_and_load_token() {
        // Use a temp dir to avoid polluting real config
        let dir = std::env::temp_dir().join("hermes_lumio_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("lumio.json");

        let data = LumioToken {
            token: "sk-test-123".into(),
            username: "testuser".into(),
            created_at: 1234567890.0,
            base_url: "https://api.lumio.run".into(),
        };
        let json = serde_json::to_string_pretty(&data).unwrap();
        std::fs::write(&path, &json).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let loaded: LumioToken = serde_json::from_str(&content).unwrap();
        assert_eq!(loaded.token, "sk-test-123");
        assert_eq!(loaded.username, "testuser");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_topup_hint() {
        let hint = topup_hint();
        assert!(hint.contains("lumio.run"));
        assert!(hint.contains("topup"));
    }

    #[test]
    fn test_rand_u128_not_zero() {
        let a = rand_u128();
        let b = rand_u128();
        assert_ne!(a, 0);
        // Very unlikely to be equal
        assert_ne!(a, b);
    }
}
