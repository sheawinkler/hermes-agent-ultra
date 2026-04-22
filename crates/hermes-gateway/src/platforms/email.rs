//! Email adapter: IMAP for receiving, SMTP for sending.
//!
//! Uses raw TCP for SMTP (EHLO, AUTH LOGIN, MAIL FROM, RCPT TO, DATA)
//! and TLS+TCP for IMAP polling (LOGIN, SELECT, SEARCH UNSEEN, FETCH).

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info};

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

pub struct EmailAdapter {
    base: BasePlatformAdapter,
    config: EmailConfig,
    stop_signal: Arc<Notify>,
}

impl EmailAdapter {
    pub fn new(config: EmailConfig) -> Result<Self, GatewayError> {
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
            smtp_send_raw(
                &smtp_host, smtp_port, &username, &password, &from, &to, &subject, &body, None,
            )
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

        tokio::task::spawn_blocking(move || {
            imap_fetch_unseen(&imap_host, imap_port, &username, &password)
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

            smtp_send_raw(
                &smtp_host,
                smtp_port,
                &username,
                &password,
                &from,
                &to,
                &subject,
                &email_body,
                Some("multipart/mixed"),
            )
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

fn smtp_send_raw(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    from: &str,
    to: &str,
    subject: &str,
    body: &str,
    content_type_override: Option<&str>,
) -> Result<(), GatewayError> {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let addr = format!("{host}:{port}");
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
    let _ehlo = send_cmd(&mut stream, &format!("EHLO hermes-agent"))?;
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
    let _from_resp = send_cmd(&mut stream, &format!("MAIL FROM:<{from}>"))?;

    // RCPT TO
    let _to_resp = send_cmd(&mut stream, &format!("RCPT TO:<{to}>"))?;

    // DATA
    let _data_resp = send_cmd(&mut stream, "DATA")?;

    // Build and send message
    let ct = content_type_override.unwrap_or("text/plain; charset=utf-8");
    let mut msg = format!("From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\n");
    if content_type_override.is_none() {
        msg.push_str(&format!("Content-Type: {ct}\r\nMIME-Version: 1.0\r\n"));
    }
    msg.push_str("\r\n");
    msg.push_str(body);
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

    debug!("Email sent via SMTP to {to}");
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
) -> Result<Vec<IncomingMessage>, GatewayError> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

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
        let cmd = format!("{t} FETCH {mid} (BODY[TEXT] BODY[HEADER.FIELDS (FROM SUBJECT)])\r\n");
        stream
            .write_all(cmd.as_bytes())
            .map_err(|e| GatewayError::Platform(format!("IMAP write: {e}")))?;
        let fetch_resp = read_response(&mut stream, &t)?;

        let mut from_addr = String::new();
        let mut subject = String::new();
        let mut body = String::new();
        let mut in_body = false;

        for line in &fetch_resp {
            let lower = line.to_lowercase();
            if lower.starts_with("from:") {
                from_addr = line[5..].trim().to_string();
            } else if lower.starts_with("subject:") {
                subject = line[8..].trim().to_string();
            } else if in_body && !line.starts_with(&t) && !line.starts_with(")") {
                body.push_str(line);
            }
            if line.contains("BODY[TEXT]") {
                in_body = true;
            }
        }

        if !from_addr.is_empty() {
            messages.push(IncomingMessage {
                platform: "email".to_string(),
                chat_id: from_addr.clone(),
                user_id: from_addr,
                text: if body.trim().is_empty() {
                    subject
                } else {
                    body.trim().to_string()
                },
                message_id: Some(mid.clone()),
                is_dm: true,
            });
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
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
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
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4 + data.len() / 57 * 2);
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
