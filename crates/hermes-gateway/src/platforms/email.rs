//! Email adapter: IMAP for receiving, SMTP for sending.
//!
//! Uses raw TCP for SMTP (EHLO, AUTH LOGIN, MAIL FROM, RCPT TO, DATA)
//! and TLS+TCP for IMAP polling (LOGIN, SELECT, SEARCH UNSEEN, FETCH).

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};
use crate::gateway::IncomingMessage;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailConfig {
    pub imap_host: String,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    pub username: String,
    pub password: String,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
    /// Require a trustworthy Authentication-Results pass before treating the
    /// RFC5322 From header as an authorization identity.
    #[serde(default = "default_true")]
    pub require_authenticated_sender: bool,
    /// Optional authserv-id that must match the trusted Authentication-Results
    /// header stamped by the receiving mail server.
    #[serde(default)]
    pub authserv_id: Option<String>,
    /// Email/global direct-message allowlist values that authorize From
    /// addresses downstream in the gateway.
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// Email/global admin values that also authorize From addresses downstream.
    #[serde(default)]
    pub admin_users: Vec<String>,
    /// Operator-selected allow-all mode; sender identity no longer gates authz.
    #[serde(default)]
    pub allow_all_users: bool,
}

fn default_imap_port() -> u16 {
    993
}
fn default_smtp_port() -> u16 {
    587
}
fn default_poll_interval() -> u64 {
    60
}
fn default_true() -> bool {
    true
}

fn trim_email_config(mut config: EmailConfig) -> EmailConfig {
    config.imap_host = config.imap_host.trim().to_string();
    config.smtp_host = config.smtp_host.trim().to_string();
    config.username = config.username.trim().to_string();
    config.password = config.password.trim().to_string();
    config.authserv_id = config
        .authserv_id
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    config.allowed_users = normalize_email_identity_list(config.allowed_users);
    config.admin_users = normalize_email_identity_list(config.admin_users);
    config
        .allowed_users
        .extend(env_email_identity_list("EMAIL_ALLOWED_USERS"));
    config
        .allowed_users
        .extend(env_email_identity_list("GATEWAY_ALLOWED_USERS"));
    if env_bool("EMAIL_ALLOW_ALL_USERS") || env_bool("GATEWAY_ALLOW_ALL_USERS") {
        config.allow_all_users = true;
    }
    if env_bool("EMAIL_TRUST_FROM_HEADER") {
        config.require_authenticated_sender = false;
    }
    if config.authserv_id.is_none() {
        config.authserv_id = std::env::var("EMAIL_AUTHSERV_ID")
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty());
    }
    config.allowed_users.sort();
    config.allowed_users.dedup();
    config.admin_users.sort();
    config.admin_users.dedup();
    config
}

fn normalize_email_identity_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .flat_map(|value| {
            value
                .replace('\n', ",")
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_ascii_lowercase)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn env_bool(key: &str) -> bool {
    std::env::var(key).ok().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn env_email_identity_list(key: &str) -> Vec<String> {
    std::env::var(key)
        .ok()
        .map(|value| normalize_email_identity_list(vec![value]))
        .unwrap_or_default()
}

fn require_email_fields(fields: &[(&'static str, &str)]) -> Result<(), GatewayError> {
    let missing = fields
        .iter()
        .filter_map(|(name, value)| {
            if value.trim().is_empty() {
                Some(*name)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    if missing.is_empty() {
        return Ok(());
    }

    Err(GatewayError::Platform(format!(
        "email_missing_configuration: missing {}",
        missing.join(", ")
    )))
}

fn validate_email_config(config: &EmailConfig) -> Result<(), GatewayError> {
    require_email_fields(&[
        ("imap_host", &config.imap_host),
        ("smtp_host", &config.smtp_host),
        ("username", &config.username),
        ("password", &config.password),
    ])
}

pub struct EmailAdapter {
    base: BasePlatformAdapter,
    config: EmailConfig,
    stop_signal: Arc<Notify>,
}

impl EmailAdapter {
    pub fn new(config: EmailConfig) -> Result<Self, GatewayError> {
        let config = trim_email_config(config);
        validate_email_config(&config)?;
        let base = BasePlatformAdapter::new(&config.username).with_proxy(config.proxy.clone());
        base.validate_token()?;
        Ok(Self {
            base,
            config,
            stop_signal: Arc::new(Notify::new()),
        })
    }

    pub fn config(&self) -> &EmailConfig {
        &self.config
    }

    /// Send an email via raw SMTP over TCP.
    pub async fn send_email(
        &self,
        to: &str,
        subject: &str,
        body: &str,
    ) -> Result<(), GatewayError> {
        let smtp_host = self.config.smtp_host.clone();
        let smtp_port = self.config.smtp_port;
        let username = self.config.username.clone();
        let password = self.config.password.clone();
        let to = to.to_string();
        let subject = subject.to_string();
        let body = body.to_string();
        let from = username.clone();

        tokio::task::spawn_blocking(move || {
            smtp_send_raw(SmtpSendRaw {
                host: &smtp_host,
                port: smtp_port,
                username: &username,
                password: &password,
                from: &from,
                to: &to,
                subject: &subject,
                body: &body,
                content_type_override: None,
            })
        })
        .await
        .map_err(|e| GatewayError::SendFailed(format!("Email task join error: {e}")))?
    }

    /// Poll IMAP for unseen messages. Returns a list of incoming messages.
    pub async fn poll_imap(&self) -> Result<Vec<IncomingMessage>, GatewayError> {
        let imap_host = self.config.imap_host.clone();
        let imap_port = self.config.imap_port;
        let username = self.config.username.clone();
        let password = self.config.password.clone();
        let auth_policy = EmailInboundAuthPolicy::from_config(&self.config);

        tokio::task::spawn_blocking(move || {
            imap_fetch_unseen(&imap_host, imap_port, &username, &password, auth_policy)
        })
        .await
        .map_err(|e| GatewayError::Platform(format!("IMAP task join error: {e}")))?
    }
}

fn image_email_body(image_url: &str, caption: Option<&str>) -> String {
    let mut text = caption
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str("Image: ");
    text.push_str(image_url);
    text
}

#[derive(Debug, Clone)]
struct EmailInboundAuthPolicy {
    require_authenticated_sender: bool,
    authserv_id: Option<String>,
    allowed_users: Vec<String>,
    admin_users: Vec<String>,
    allow_all_users: bool,
}

impl EmailInboundAuthPolicy {
    fn from_config(config: &EmailConfig) -> Self {
        Self {
            require_authenticated_sender: config.require_authenticated_sender,
            authserv_id: config.authserv_id.clone(),
            allowed_users: config.allowed_users.clone(),
            admin_users: config.admin_users.clone(),
            allow_all_users: config.allow_all_users,
        }
    }

    fn allowlist_in_effect(&self) -> bool {
        !self.allowed_users.is_empty() || !self.admin_users.is_empty()
    }

    fn requires_authenticated_sender(&self) -> bool {
        self.require_authenticated_sender && self.allowlist_in_effect() && !self.allow_all_users
    }

    fn should_drop_unauthenticated(&self, authentication: &SenderAuthentication) -> bool {
        self.requires_authenticated_sender() && !authentication.authenticated
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SenderAuthentication {
    authenticated: bool,
    reason: String,
}

impl SenderAuthentication {
    fn pass(reason: impl Into<String>) -> Self {
        Self {
            authenticated: true,
            reason: reason.into(),
        }
    }

    fn fail(reason: impl Into<String>) -> Self {
        Self {
            authenticated: false,
            reason: reason.into(),
        }
    }
}

fn extract_email_address(raw: &str) -> String {
    let raw = raw.trim();
    if let (Some(start), Some(end)) = (raw.rfind('<'), raw.rfind('>')) {
        if start < end {
            return raw[start + 1..end]
                .trim()
                .trim_matches('"')
                .to_ascii_lowercase();
        }
    }
    raw.trim_matches('"').to_ascii_lowercase()
}

fn domain_of(address_or_domain: &str) -> String {
    let value = address_or_domain
        .trim()
        .trim_matches('"')
        .trim_matches('<')
        .trim_matches('>')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    value
        .rsplit_once('@')
        .map(|(_, domain)| domain.trim_end_matches('.').to_string())
        .unwrap_or(value)
}

fn domains_aligned(a: &str, b: &str) -> bool {
    let a = domain_of(a);
    let b = domain_of(b);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b || a.ends_with(&format!(".{b}")) || b.ends_with(&format!(".{a}"))
}

fn authentication_results_pairs(header: &str) -> Vec<(String, String)> {
    let normalized = header.replace(['\r', '\n', '\t'], " ");
    let mut pairs = Vec::new();
    for token in normalized.split(|c: char| c == ';' || c == ',' || c.is_whitespace()) {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('<')
            .trim_matches('>')
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if !key.is_empty() && !value.is_empty() {
            pairs.push((key, value));
        }
    }
    pairs
}

fn auth_pair_value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .rev()
        .find_map(|(candidate, value)| (candidate == key).then_some(value.as_str()))
}

fn verify_sender_authentication(
    auth_headers: &[String],
    from_addr: &str,
    authserv_id: Option<&str>,
) -> SenderAuthentication {
    let from_domain = domain_of(from_addr);
    if from_domain.is_empty() {
        return SenderAuthentication::fail("missing From domain");
    }
    if auth_headers.is_empty() {
        return SenderAuthentication::fail("no Authentication-Results header");
    }

    let trusted = auth_headers
        .iter()
        .map(|header| header.split_whitespace().collect::<Vec<_>>().join(" "))
        .find(|header| {
            let Some(authserv_id) = authserv_id.map(str::trim).filter(|value| !value.is_empty())
            else {
                return true;
            };
            let server = header
                .split_once(';')
                .map(|(server, _)| server.trim())
                .unwrap_or(header.as_str());
            domains_aligned(server, authserv_id)
        });

    let Some(trusted) = trusted else {
        return SenderAuthentication::fail("no Authentication-Results from trusted authserv-id");
    };
    let pairs = authentication_results_pairs(&trusted);

    if auth_pair_value(&pairs, "dmarc") == Some("pass") {
        return SenderAuthentication::pass("dmarc=pass");
    }

    if auth_pair_value(&pairs, "spf") == Some("pass") {
        let spf_domain = auth_pair_value(&pairs, "smtp.mailfrom")
            .or_else(|| auth_pair_value(&pairs, "smtp.from"))
            .or_else(|| auth_pair_value(&pairs, "envelope-from"))
            .map(domain_of)
            .unwrap_or_default();
        if domains_aligned(&spf_domain, &from_domain) {
            return SenderAuthentication::pass("spf=pass aligned");
        }
    }

    if auth_pair_value(&pairs, "dkim") == Some("pass") {
        let dkim_domain = auth_pair_value(&pairs, "header.d")
            .map(domain_of)
            .or_else(|| auth_pair_value(&pairs, "header.from").map(domain_of))
            .unwrap_or_default();
        if domains_aligned(&dkim_domain, &from_domain) {
            return SenderAuthentication::pass("dkim=pass aligned");
        }
    }

    let reason = trusted.chars().take(120).collect::<String>();
    SenderAuthentication::fail(format!("authentication failed ({reason})"))
}

fn incoming_message_from_imap_fetch(
    mid: &str,
    fetch_resp: &[String],
    auth_policy: &EmailInboundAuthPolicy,
    done_tag: &str,
) -> Option<IncomingMessage> {
    let mut from_addr = String::new();
    let mut subject = String::new();
    let mut auth_headers: Vec<String> = Vec::new();
    let mut body = String::new();
    let mut in_body = false;
    let mut current_header: Option<&'static str> = None;

    for line in fetch_resp {
        let lower = line.to_lowercase();
        if lower.starts_with("from:") {
            from_addr = extract_email_address(&line[5..]);
            current_header = Some("from");
        } else if lower.starts_with("subject:") {
            subject = line[8..].trim().to_string();
            current_header = Some("subject");
        } else if lower.starts_with("authentication-results:") {
            auth_headers.push(line["authentication-results:".len()..].trim().to_string());
            current_header = Some("authentication-results");
        } else if line.starts_with(' ') || line.starts_with('\t') {
            match current_header {
                Some("subject") => {
                    if !subject.is_empty() {
                        subject.push(' ');
                    }
                    subject.push_str(line.trim());
                }
                Some("authentication-results") => {
                    if let Some(last) = auth_headers.last_mut() {
                        if !last.is_empty() {
                            last.push(' ');
                        }
                        last.push_str(line.trim());
                    }
                }
                _ => {}
            }
        } else if in_body && !line.starts_with(done_tag) && !line.starts_with(')') {
            body.push_str(line);
            current_header = None;
        }
        if line.contains("BODY[TEXT]") {
            in_body = true;
            current_header = None;
        }
    }

    if from_addr.is_empty() {
        return None;
    }

    let sender_auth = verify_sender_authentication(
        &auth_headers,
        &from_addr,
        auth_policy.authserv_id.as_deref(),
    );
    if auth_policy.should_drop_unauthenticated(&sender_auth) {
        warn!(
            sender = from_addr,
            reason = sender_auth.reason,
            "Email sender rejected because From authentication did not pass"
        );
        return None;
    }

    Some(IncomingMessage {
        platform: "email".to_string(),
        chat_id: from_addr.clone(),
        user_id: from_addr,
        text: if body.trim().is_empty() {
            subject
        } else {
            body.trim().to_string()
        },
        message_id: Some(mid.to_string()),
        thread_id: None,
        is_dm: true,
    })
}

#[async_trait]
impl PlatformAdapter for EmailAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "Email adapter starting (user: {}, IMAP: {}:{})",
            self.config.username, self.config.imap_host, self.config.imap_port
        );
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Email adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.send_email(chat_id, "Hermes Agent", text).await
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        debug!("Email does not support message editing");
        Ok(())
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        use crate::platforms::helpers::mime_from_extension;

        let path = std::path::Path::new(file_path);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mime_type = mime_from_extension(ext);
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment");
        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read file: {e}")))?;

        let to = chat_id.to_string();
        let from = self.config.username.clone();
        let caption_owned = caption.unwrap_or("Hermes Agent - Attachment").to_string();
        let subject = caption_owned.clone();
        let smtp_host = self.config.smtp_host.clone();
        let smtp_port = self.config.smtp_port;
        let username = self.config.username.clone();
        let password = self.config.password.clone();
        let file_name = file_name.to_string();
        let mime_type = mime_type.to_string();

        tokio::task::spawn_blocking(move || {
            let boundary = format!(
                "hermes-{}",
                uuid::Uuid::new_v4().to_string().replace('-', "")
            );

            let mut email_body = String::new();
            email_body.push_str(&format!(
                "Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n"
            ));
            email_body.push_str("MIME-Version: 1.0\r\n\r\n");
            email_body.push_str(&format!("--{boundary}\r\n"));
            email_body.push_str("Content-Type: text/plain; charset=utf-8\r\n\r\n");
            email_body.push_str(&caption_owned);
            email_body.push_str("\r\n");
            email_body.push_str(&format!("--{boundary}\r\n"));
            email_body.push_str(&format!(
                "Content-Type: {mime_type}; name=\"{file_name}\"\r\n"
            ));
            email_body.push_str(&format!(
                "Content-Disposition: attachment; filename=\"{file_name}\"\r\n"
            ));
            email_body.push_str("Content-Transfer-Encoding: base64\r\n\r\n");
            email_body.push_str(&base64_encode_lines(&file_bytes));
            email_body.push_str(&format!("\r\n--{boundary}--\r\n"));

            smtp_send_raw(SmtpSendRaw {
                host: &smtp_host,
                port: smtp_port,
                username: &username,
                password: &password,
                from: &from,
                to: &to,
                subject: &subject,
                body: &email_body,
                content_type_override: Some("multipart/mixed"),
            })
        })
        .await
        .map_err(|e| GatewayError::SendFailed(format!("Email task join error: {e}")))?
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let body = image_email_body(image_url, caption);
        self.send_message(chat_id, &body, Some(ParseMode::Plain))
            .await
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }
    fn platform_name(&self) -> &str {
        "email"
    }
}

// ---------------------------------------------------------------------------
// Raw SMTP sender
// ---------------------------------------------------------------------------

struct SmtpSendRaw<'a> {
    host: &'a str,
    port: u16,
    username: &'a str,
    password: &'a str,
    from: &'a str,
    to: &'a str,
    subject: &'a str,
    body: &'a str,
    content_type_override: Option<&'a str>,
}

fn smtp_send_raw(req: SmtpSendRaw<'_>) -> Result<(), GatewayError> {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let host = req.host.trim();
    let username = req.username.trim();
    let password = req.password.trim();
    require_email_fields(&[
        ("smtp_host", host),
        ("username", username),
        ("password", password),
    ])?;

    let addr = format!("{host}:{}", req.port);
    let mut stream = TcpStream::connect(&addr)
        .map_err(|e| GatewayError::SendFailed(format!("SMTP connect {addr}: {e}")))?;
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(30))).ok();

    let read_line = |stream: &TcpStream| -> Result<String, GatewayError> {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| GatewayError::SendFailed(format!("SMTP read: {e}")))?;
        Ok(line)
    };

    let send_cmd = |stream: &mut TcpStream, cmd: &str| -> Result<String, GatewayError> {
        stream
            .write_all(cmd.as_bytes())
            .map_err(|e| GatewayError::SendFailed(format!("SMTP write: {e}")))?;
        stream
            .write_all(b"\r\n")
            .map_err(|e| GatewayError::SendFailed(format!("SMTP write: {e}")))?;
        stream
            .flush()
            .map_err(|e| GatewayError::SendFailed(format!("SMTP flush: {e}")))?;
        read_line(&*stream)
    };

    // Read greeting
    let _greeting = read_line(&stream)?;

    // EHLO
    let _ehlo = send_cmd(&mut stream, "EHLO hermes-agent")?;
    // Drain multi-line EHLO response
    loop {
        let line = read_line(&stream)?;
        if line.len() < 4 || line.as_bytes().get(3) == Some(&b' ') {
            break;
        }
    }

    // AUTH LOGIN
    let _auth = send_cmd(&mut stream, "AUTH LOGIN")?;
    let _user = send_cmd(&mut stream, &base64_encode_simple(username.as_bytes()))?;
    let auth_resp = send_cmd(&mut stream, &base64_encode_simple(password.as_bytes()))?;
    if !auth_resp.starts_with("235") {
        return Err(GatewayError::SendFailed(format!(
            "SMTP AUTH failed: {}",
            auth_resp.trim()
        )));
    }

    // MAIL FROM
    let _from_resp = send_cmd(&mut stream, &format!("MAIL FROM:<{}>", req.from))?;

    // RCPT TO
    let _to_resp = send_cmd(&mut stream, &format!("RCPT TO:<{}>", req.to))?;

    // DATA
    let _data_resp = send_cmd(&mut stream, "DATA")?;

    // Build and send message
    let ct = req
        .content_type_override
        .unwrap_or("text/plain; charset=utf-8");
    let mut msg = format!(
        "From: {}\r\nTo: {}\r\nSubject: {}\r\n",
        req.from, req.to, req.subject
    );
    if req.content_type_override.is_none() {
        msg.push_str(&format!("Content-Type: {ct}\r\nMIME-Version: 1.0\r\n"));
    }
    msg.push_str("\r\n");
    msg.push_str(req.body);
    msg.push_str("\r\n.\r\n");

    stream
        .write_all(msg.as_bytes())
        .map_err(|e| GatewayError::SendFailed(format!("SMTP DATA write: {e}")))?;
    stream
        .flush()
        .map_err(|e| GatewayError::SendFailed(format!("SMTP flush: {e}")))?;
    let data_resp = read_line(&stream)?;
    if !data_resp.starts_with("250") {
        return Err(GatewayError::SendFailed(format!(
            "SMTP DATA rejected: {}",
            data_resp.trim()
        )));
    }

    // QUIT
    let _ = send_cmd(&mut stream, "QUIT");

    debug!("Email sent via SMTP to {}", req.to);
    Ok(())
}

// ---------------------------------------------------------------------------
// Raw IMAP receiver
// ---------------------------------------------------------------------------

fn imap_fetch_unseen(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    auth_policy: EmailInboundAuthPolicy,
) -> Result<Vec<IncomingMessage>, GatewayError> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let host = host.trim();
    let username = username.trim();
    let password = password.trim();
    require_email_fields(&[
        ("imap_host", host),
        ("username", username),
        ("password", password),
    ])?;

    let addr = format!("{host}:{port}");

    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let server_name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|e| GatewayError::Platform(format!("Invalid server name: {e}")))?;
    let conn = rustls::ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| GatewayError::Platform(format!("TLS init: {e}")))?;
    let tcp = TcpStream::connect(&addr)
        .map_err(|e| GatewayError::Platform(format!("IMAP connect {addr}: {e}")))?;
    tcp.set_read_timeout(Some(Duration::from_secs(30))).ok();
    let mut stream = rustls::StreamOwned::new(conn, tcp);

    let mut tag = 0u32;
    let mut next_tag = || -> String {
        tag += 1;
        format!("A{:04}", tag)
    };

    fn read_response(
        stream: &mut rustls::StreamOwned<rustls::ClientConnection, TcpStream>,
        tag: &str,
    ) -> Result<Vec<String>, GatewayError> {
        let mut lines = Vec::new();
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match stream.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    buf.push(byte[0]);
                    if byte[0] == b'\n' {
                        let line = String::from_utf8_lossy(&buf).to_string();
                        let done = line.starts_with(tag);
                        lines.push(line);
                        buf.clear();
                        if done {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        Ok(lines)
    }

    // Read greeting
    {
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
    }

    // LOGIN
    let t = next_tag();
    let cmd = format!("{t} LOGIN {username} {password}\r\n");
    stream
        .write_all(cmd.as_bytes())
        .map_err(|e| GatewayError::Platform(format!("IMAP write: {e}")))?;
    let login_resp = read_response(&mut stream, &t)?;
    let last = login_resp.last().map(|s| s.as_str()).unwrap_or("");
    if !last.contains("OK") {
        return Err(GatewayError::Platform(format!("IMAP LOGIN failed: {last}")));
    }

    // SELECT INBOX
    let t = next_tag();
    let cmd = format!("{t} SELECT INBOX\r\n");
    stream
        .write_all(cmd.as_bytes())
        .map_err(|e| GatewayError::Platform(format!("IMAP write: {e}")))?;
    let _ = read_response(&mut stream, &t)?;

    // SEARCH UNSEEN
    let t = next_tag();
    let cmd = format!("{t} SEARCH UNSEEN\r\n");
    stream
        .write_all(cmd.as_bytes())
        .map_err(|e| GatewayError::Platform(format!("IMAP write: {e}")))?;
    let search_resp = read_response(&mut stream, &t)?;

    let mut msg_ids: Vec<String> = Vec::new();
    for line in &search_resp {
        if line.starts_with("* SEARCH") {
            let ids: Vec<String> = line
                .trim_start_matches("* SEARCH")
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            msg_ids.extend(ids);
        }
    }

    let mut messages = Vec::new();
    for mid in msg_ids.iter().take(10) {
        let t = next_tag();
        let cmd = format!(
            "{t} FETCH {mid} (BODY[TEXT] BODY[HEADER.FIELDS (FROM SUBJECT AUTHENTICATION-RESULTS)])\r\n"
        );
        stream
            .write_all(cmd.as_bytes())
            .map_err(|e| GatewayError::Platform(format!("IMAP write: {e}")))?;
        let fetch_resp = read_response(&mut stream, &t)?;

        if let Some(message) = incoming_message_from_imap_fetch(mid, &fetch_resp, &auth_policy, &t)
        {
            messages.push(message);
        }
    }

    // LOGOUT
    let t = next_tag();
    let cmd = format!("{t} LOGOUT\r\n");
    let _ = stream.write_all(cmd.as_bytes());

    Ok(messages)
}

// ---------------------------------------------------------------------------
// Base64 helpers
// ---------------------------------------------------------------------------

fn base64_encode_simple(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        result.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        result.push(if chunk.len() > 1 {
            ALPHABET[((triple >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            ALPHABET[(triple & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    result
}

fn base64_encode_lines(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4 + data.len() / 57 * 2);
    let mut col = 0;
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        result.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        result.push(if chunk.len() > 1 {
            ALPHABET[((triple >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            ALPHABET[(triple & 0x3F) as usize] as char
        } else {
            '='
        });
        col += 4;
        if col >= 76 {
            result.push_str("\r\n");
            col = 0;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("email env lock poisoned")
    }

    struct EnvGuard {
        original: Vec<(&'static str, Option<String>)>,
        _guard: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let guard = env_lock();
            let original = keys
                .iter()
                .map(|key| (*key, std::env::var(key).ok()))
                .collect::<Vec<_>>();
            for key in keys {
                std::env::remove_var(key);
            }
            Self {
                original,
                _guard: guard,
            }
        }

        fn set(&self, key: &'static str, value: &str) {
            std::env::set_var(key, value);
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.original {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn config() -> EmailConfig {
        EmailConfig {
            imap_host: "imap.example.com".to_string(),
            imap_port: 993,
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 587,
            username: "agent@example.com".to_string(),
            password: "secret".to_string(),
            poll_interval_secs: 60,
            proxy: AdapterProxyConfig::default(),
            require_authenticated_sender: true,
            authserv_id: None,
            allowed_users: Vec::new(),
            admin_users: Vec::new(),
            allow_all_users: false,
        }
    }

    #[test]
    fn email_adapter_trims_config_before_use() {
        let adapter = EmailAdapter::new(EmailConfig {
            imap_host: " imap.example.com ".to_string(),
            smtp_host: "\tsmtp.example.com\n".to_string(),
            username: " agent@example.com ".to_string(),
            password: " secret ".to_string(),
            ..config()
        })
        .expect("adapter");

        assert_eq!(adapter.config().imap_host, "imap.example.com");
        assert_eq!(adapter.config().smtp_host, "smtp.example.com");
        assert_eq!(adapter.config().username, "agent@example.com");
        assert_eq!(adapter.config().password, "secret");
    }

    #[test]
    fn email_adapter_rejects_blank_required_config() {
        let result = EmailAdapter::new(EmailConfig {
            imap_host: " ".to_string(),
            smtp_host: "\t".to_string(),
            username: "".to_string(),
            password: "\n".to_string(),
            ..config()
        });
        let err = match result {
            Ok(_) => panic!("missing config should fail"),
            Err(err) => err,
        };

        let msg = err.to_string();
        assert!(msg.contains("email_missing_configuration"));
        assert!(msg.contains("imap_host"));
        assert!(msg.contains("smtp_host"));
        assert!(msg.contains("username"));
        assert!(msg.contains("password"));
    }

    #[test]
    fn raw_email_paths_reject_blank_host_before_network() {
        let smtp_err = smtp_send_raw(SmtpSendRaw {
            host: " ",
            port: 587,
            username: "agent@example.com",
            password: "secret",
            from: "agent@example.com",
            to: "user@example.com",
            subject: "subject",
            body: "body",
            content_type_override: None,
        })
        .expect_err("blank SMTP host should fail before connect");
        assert!(smtp_err.to_string().contains("email_missing_configuration"));
        assert!(smtp_err.to_string().contains("smtp_host"));

        let imap_err = imap_fetch_unseen(
            " ",
            993,
            "agent@example.com",
            "secret",
            EmailInboundAuthPolicy::from_config(&config()),
        )
        .expect_err("blank IMAP host should fail before TLS/connect");
        assert!(imap_err.to_string().contains("email_missing_configuration"));
        assert!(imap_err.to_string().contains("imap_host"));
    }

    #[test]
    fn email_config_env_trust_from_header_disables_sender_auth_gate() {
        let env = EnvGuard::new(&["EMAIL_TRUST_FROM_HEADER", "EMAIL_ALLOWED_USERS"]);
        env.set("EMAIL_TRUST_FROM_HEADER", "true");
        let adapter = EmailAdapter::new(EmailConfig {
            allowed_users: vec!["admin@example.com".into()],
            ..config()
        })
        .expect("adapter");

        let policy = EmailInboundAuthPolicy::from_config(adapter.config());
        assert!(!policy.requires_authenticated_sender());
    }

    #[test]
    fn email_config_env_allowlist_enables_sender_auth_gate() {
        let env = EnvGuard::new(&["EMAIL_ALLOWED_USERS", "EMAIL_TRUST_FROM_HEADER"]);
        env.set("EMAIL_ALLOWED_USERS", "admin@example.com");
        let adapter = EmailAdapter::new(config()).expect("adapter");

        let policy = EmailInboundAuthPolicy::from_config(adapter.config());
        assert!(policy.requires_authenticated_sender());
    }

    #[test]
    fn email_spoofed_from_rejected_when_allowlist_is_active() {
        let policy = EmailInboundAuthPolicy::from_config(&EmailConfig {
            allowed_users: vec!["admin@example.com".into()],
            ..config()
        });
        let auth = verify_sender_authentication(
            &[String::from(
                "mx.example.net; spf=fail smtp.mailfrom=attacker.evil; dkim=fail header.d=evil.test; dmarc=fail header.from=example.com",
            )],
            "admin@example.com",
            None,
        );

        assert!(!auth.authenticated);
        assert!(policy.should_drop_unauthenticated(&auth));
    }

    #[test]
    fn email_imap_parser_drops_spoofed_allowlisted_from() {
        let policy = EmailInboundAuthPolicy::from_config(&EmailConfig {
            allowed_users: vec!["admin@example.com".into()],
            ..config()
        });
        let response = vec![
            "From: Admin <admin@example.com>\r\n".to_string(),
            "Subject: spoof\r\n".to_string(),
            "Authentication-Results: mx.example.net; spf=fail smtp.mailfrom=evil.test;\r\n"
                .to_string(),
            " dkim=fail header.d=evil.test; dmarc=fail header.from=example.com\r\n".to_string(),
            "* 1 FETCH (BODY[TEXT] {6}\r\n".to_string(),
            "attack\r\n".to_string(),
            "A0001 OK FETCH completed\r\n".to_string(),
        ];

        assert!(incoming_message_from_imap_fetch("1", &response, &policy, "A0001").is_none());
    }

    #[test]
    fn email_imap_parser_emits_normalized_sender_when_auth_aligned() {
        let policy = EmailInboundAuthPolicy::from_config(&EmailConfig {
            allowed_users: vec!["admin@example.com".into()],
            ..config()
        });
        let response = vec![
            "From: Admin <ADMIN@example.com>\r\n".to_string(),
            "Subject: hello\r\n".to_string(),
            "Authentication-Results: mx.example.net; spf=pass\r\n".to_string(),
            " smtp.mailfrom=bounces@mail.example.com\r\n".to_string(),
            "* 2 FETCH (BODY[TEXT] {7}\r\n".to_string(),
            "hello\r\n".to_string(),
            "A0002 OK FETCH completed\r\n".to_string(),
        ];

        let message =
            incoming_message_from_imap_fetch("2", &response, &policy, "A0002").expect("message");
        assert_eq!(message.user_id, "admin@example.com");
        assert_eq!(message.chat_id, "admin@example.com");
        assert_eq!(message.text, "hello");
    }

    #[test]
    fn email_dmarc_pass_authenticates_from_domain() {
        let auth = verify_sender_authentication(
            &[String::from(
                "mx.example.net; dmarc=pass header.from=example.com; spf=fail smtp.mailfrom=evil.test",
            )],
            "Admin <admin@example.com>",
            None,
        );

        assert_eq!(auth, SenderAuthentication::pass("dmarc=pass"));
    }

    #[test]
    fn email_spf_pass_requires_from_domain_alignment() {
        let aligned = verify_sender_authentication(
            &[String::from(
                "mx.example.net; spf=pass smtp.mailfrom=bounces@mail.example.com",
            )],
            "admin@example.com",
            None,
        );
        let unaligned = verify_sender_authentication(
            &[String::from(
                "mx.example.net; spf=pass smtp.mailfrom=attacker.evil",
            )],
            "admin@example.com",
            None,
        );

        assert_eq!(aligned, SenderAuthentication::pass("spf=pass aligned"));
        assert!(!unaligned.authenticated);
    }

    #[test]
    fn email_dkim_pass_requires_from_domain_alignment() {
        let aligned = verify_sender_authentication(
            &[String::from(
                "mx.example.net; dkim=pass header.d=mail.example.com",
            )],
            "admin@example.com",
            None,
        );
        let unaligned = verify_sender_authentication(
            &[String::from("mx.example.net; dkim=pass header.d=evil.test")],
            "admin@example.com",
            None,
        );

        assert_eq!(aligned, SenderAuthentication::pass("dkim=pass aligned"));
        assert!(!unaligned.authenticated);
    }

    #[test]
    fn email_authserv_id_pins_trusted_authentication_results() {
        let auth = verify_sender_authentication(
            &[
                String::from("evil.test; dmarc=pass header.from=example.com"),
                String::from("mx.example.net; spf=pass smtp.mailfrom=example.com"),
            ],
            "admin@example.com",
            Some("mx.example.net"),
        );

        assert_eq!(auth, SenderAuthentication::pass("spf=pass aligned"));
    }

    #[test]
    fn email_allow_all_skips_sender_auth_gate() {
        let policy = EmailInboundAuthPolicy::from_config(&EmailConfig {
            allowed_users: vec!["admin@example.com".into()],
            allow_all_users: true,
            ..config()
        });
        let auth = SenderAuthentication::fail("no Authentication-Results header");

        assert!(!policy.requires_authenticated_sender());
        assert!(!policy.should_drop_unauthenticated(&auth));
    }

    #[test]
    fn image_email_body_with_caption() {
        let body = image_email_body("https://cdn.example.com/a.png", Some("Diagram"));
        assert_eq!(body, "Diagram\n\nImage: https://cdn.example.com/a.png");
    }

    #[test]
    fn image_email_body_without_caption() {
        let body = image_email_body("https://cdn.example.com/a.png", Some("  "));
        assert_eq!(body, "Image: https://cdn.example.com/a.png");
    }
}
