//! Full ACP request handler — implements all ACP protocol methods.
//!
//! Mirrors the Python `acp_adapter/server.py` HermesACPAgent class.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use serde_json::{json, Value};
use url::Url;

use crate::auth::{
    build_auth_methods_for_provider, detect_provider, TERMINAL_SETUP_AUTH_METHOD_ID,
};
use crate::events::{AcpEvent, EventSink};
use crate::permissions::PermissionStore;
use crate::protocol::*;
use crate::session::{SessionManager, SessionPhase, SessionState};

/// Trait for handling ACP requests.
#[async_trait::async_trait]
pub trait AcpHandler: Send + Sync {
    async fn handle_request(&self, request: AcpRequest) -> AcpResponse;
}

/// Output returned by a concrete ACP prompt executor.
#[derive(Debug, Clone, Default)]
pub struct PromptExecutionOutput {
    pub response_text: String,
    pub usage: Option<Usage>,
    pub total_turns: Option<u32>,
    pub events: Vec<AcpEvent>,
}

/// Pluggable ACP prompt executor.
#[async_trait::async_trait]
pub trait AcpPromptExecutor: Send + Sync {
    async fn execute_prompt(
        &self,
        session: &SessionState,
        user_text: &str,
        history: &[Value],
    ) -> Result<PromptExecutionOutput, String>;
}

const MAX_ACP_RESOURCE_BYTES: usize = 512 * 1024;
const IMAGE_EXT_MIME: &[(&str, &str)] = &[
    (".png", "image/png"),
    (".jpg", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".gif", "image/gif"),
    (".webp", "image/webp"),
    (".bmp", "image/bmp"),
    (".svg", "image/svg+xml"),
];

const TEXT_RESOURCE_MIME_PREFIXES: &[&str] = &["text/"];
const TEXT_RESOURCE_MIME_TYPES: &[&str] = &[
    "application/json",
    "application/javascript",
    "application/typescript",
    "application/xml",
    "application/x-yaml",
    "application/yaml",
    "application/toml",
    "application/sql",
];

#[derive(Debug, Clone)]
struct PromptExtraction {
    user_text: String,
    user_content: Value,
    text_only_prompt: bool,
    has_content: bool,
}

fn canonical_mime(mime: Option<&str>) -> Option<String> {
    mime.map(|m| {
        m.split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
    })
    .filter(|m| !m.is_empty())
}

fn is_text_resource_mime(mime: Option<&str>) -> bool {
    let Some(mime) = canonical_mime(mime) else {
        return false;
    };
    TEXT_RESOURCE_MIME_PREFIXES
        .iter()
        .any(|prefix| mime.starts_with(prefix))
        || TEXT_RESOURCE_MIME_TYPES.contains(&mime.as_str())
}

fn is_image_resource_mime(mime: Option<&str>) -> bool {
    canonical_mime(mime)
        .map(|m| m.starts_with("image/"))
        .unwrap_or(false)
}

fn guess_image_mime_from_path(path: &Path) -> Option<&'static str> {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    IMAGE_EXT_MIME
        .iter()
        .find_map(|(ext, mime)| lower.ends_with(ext).then_some(*mime))
}

fn read_resource_prefix(path: &Path, max_bytes: usize) -> Result<(Vec<u8>, usize), std::io::Error> {
    let mut file = File::open(path)?;
    let mut buf = Vec::new();
    let mut take = (&mut file).take(max_bytes as u64);
    take.read_to_end(&mut buf)?;
    let size = file.metadata()?.len() as usize;
    Ok((buf, size))
}

fn decode_text_bytes(data: &[u8], mime: Option<&str>) -> Option<String> {
    if data.contains(&0) && !is_text_resource_mime(mime) {
        return None;
    }
    if let Ok(text) = String::from_utf8(data.to_vec()) {
        return Some(text);
    }
    Some(String::from_utf8_lossy(data).into_owned())
}

fn resource_display_name(uri: &str, name: Option<&str>, title: Option<&str>) -> String {
    let name = name.unwrap_or("").trim();
    let title = title.unwrap_or("").trim();
    if !title.is_empty() && !name.is_empty() && title != name {
        return format!("{title} ({name})");
    }
    if !title.is_empty() {
        return title.to_string();
    }
    if !name.is_empty() {
        return name.to_string();
    }
    path_from_file_uri(uri)
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| uri.to_string())
}

fn format_resource_text(
    uri: &str,
    body: &str,
    name: Option<&str>,
    title: Option<&str>,
    note: Option<&str>,
) -> String {
    let mut header = format!(
        "[Attached file: {}]",
        resource_display_name(uri, name, title)
    );
    if let Some(note) = note.filter(|n| !n.is_empty()) {
        header.push_str(&format!(" ({note})"));
    }
    if uri.trim().is_empty() {
        format!("{header}\n\n{body}")
    } else {
        format!("{header}\nURI: {uri}\n\n{body}")
    }
}

fn path_from_file_uri(uri: &str) -> Option<PathBuf> {
    let raw = uri.trim();
    if raw.is_empty() {
        return None;
    }
    if !raw.contains("://") {
        return Some(PathBuf::from(raw));
    }
    let parsed = Url::parse(raw).ok()?;
    if parsed.scheme() != "file" {
        return None;
    }
    if let Some(host) = parsed.host_str() {
        if host != "localhost" && !host.is_empty() {
            return None;
        }
    }
    let mut path_text = parsed.path().to_string();
    if path_text.starts_with("/%3A") {
        path_text = path_text.replacen("/%3A", ":", 1);
    }
    if path_text.len() >= 3 {
        let bytes = path_text.as_bytes();
        if bytes[0] == b'/' && bytes[2] == b':' && bytes[1].is_ascii_alphabetic() {
            let drive = (bytes[1] as char).to_ascii_lowercase();
            let rest = path_text[3..]
                .trim_start_matches(['/', '\\'])
                .replace('\\', "/");
            return Some(PathBuf::from(format!("/mnt/{drive}/{rest}")));
        }
    }
    if path_text.len() >= 2 {
        let bytes = path_text.as_bytes();
        if bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            let drive = (bytes[0] as char).to_ascii_lowercase();
            let rest = path_text[2..]
                .trim_start_matches(['/', '\\'])
                .replace('\\', "/");
            return Some(PathBuf::from(format!("/mnt/{drive}/{rest}")));
        }
    }
    Some(PathBuf::from(path_text))
}

fn build_image_data_url(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", BASE64_STANDARD.encode(bytes))
}

fn json_text_part(text: impl Into<String>) -> Value {
    json!({
        "type": "text",
        "text": text.into(),
    })
}

fn json_image_part(url: impl Into<String>) -> Value {
    json!({
        "type": "image_url",
        "image_url": {
            "url": url.into(),
        }
    })
}

fn resource_link_to_parts(block: &serde_json::Map<String, Value>) -> Vec<Value> {
    let uri = block
        .get("uri")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if uri.is_empty() {
        return Vec::new();
    }
    let name = block.get("name").and_then(|v| v.as_str());
    let title = block.get("title").and_then(|v| v.as_str());
    let mime = block
        .get("mimeType")
        .or_else(|| block.get("mime_type"))
        .and_then(|v| v.as_str());

    let Some(path) = path_from_file_uri(&uri) else {
        return vec![json_text_part(format_resource_text(
            &uri,
            "[Resource link only; Hermes cannot read non-file ACP resource URIs directly.]",
            name,
            title,
            None,
        ))];
    };

    let guessed_image_mime = if is_image_resource_mime(mime) {
        canonical_mime(mime)
    } else {
        guess_image_mime_from_path(&path).map(ToString::to_string)
    };
    if let Some(image_mime) = guessed_image_mime {
        match std::fs::read(&path) {
            Ok(bytes) => {
                if bytes.len() > MAX_ACP_RESOURCE_BYTES {
                    return vec![json_text_part(format_resource_text(
                        &uri,
                        &format!(
                            "[Image too large to inline: {} bytes, cap={}]",
                            bytes.len(),
                            MAX_ACP_RESOURCE_BYTES
                        ),
                        name,
                        title,
                        None,
                    ))];
                }
                return vec![
                    json_text_part(format!(
                        "[Attached image: {}]\nURI: {}",
                        resource_display_name(&uri, name, title),
                        uri
                    )),
                    json_image_part(build_image_data_url(&image_mime, &bytes)),
                ];
            }
            Err(err) => {
                return vec![json_text_part(format_resource_text(
                    &uri,
                    &format!("[Could not read attached image: {err}]"),
                    name,
                    title,
                    None,
                ))];
            }
        }
    }

    match read_resource_prefix(&path, MAX_ACP_RESOURCE_BYTES) {
        Ok((bytes, size)) => {
            if let Some(text) = decode_text_bytes(&bytes, mime) {
                let note = if size > MAX_ACP_RESOURCE_BYTES {
                    Some(format!(
                        "truncated to {} of {} bytes",
                        MAX_ACP_RESOURCE_BYTES, size
                    ))
                } else {
                    None
                };
                vec![json_text_part(format_resource_text(
                    &uri,
                    &text,
                    name,
                    title,
                    note.as_deref(),
                ))]
            } else {
                vec![json_text_part(format_resource_text(
                    &uri,
                    &format!(
                        "[Binary file omitted: {} bytes, mime={}]",
                        size,
                        canonical_mime(mime).unwrap_or_else(|| "unknown".to_string())
                    ),
                    name,
                    title,
                    None,
                ))]
            }
        }
        Err(err) => vec![json_text_part(format_resource_text(
            &uri,
            &format!("[Could not read attached file: {err}]"),
            name,
            title,
            None,
        ))],
    }
}

fn embedded_resource_to_parts(block: &serde_json::Map<String, Value>) -> Vec<Value> {
    let resource = block
        .get("resource")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    if resource.is_empty() {
        return Vec::new();
    }

    let uri = resource
        .get("uri")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let mime = resource
        .get("mimeType")
        .or_else(|| resource.get("mime_type"))
        .and_then(|v| v.as_str());

    if let Some(text) = resource.get("text").and_then(|v| v.as_str()) {
        return vec![json_text_part(format_resource_text(
            &uri, text, None, None, None,
        ))];
    }

    if let Some(blob) = resource.get("blob").and_then(|v| v.as_str()) {
        let bytes = BASE64_STANDARD
            .decode(blob)
            .unwrap_or_else(|_| blob.as_bytes().to_vec());
        if is_image_resource_mime(mime) {
            if bytes.len() > MAX_ACP_RESOURCE_BYTES {
                return vec![json_text_part(format_resource_text(
                    &uri,
                    &format!(
                        "[Embedded image too large to inline: {} bytes, cap={}]",
                        bytes.len(),
                        MAX_ACP_RESOURCE_BYTES
                    ),
                    None,
                    None,
                    None,
                ))];
            }
            let image_mime = canonical_mime(mime).unwrap_or_else(|| "image/png".to_string());
            return vec![
                json_text_part(if uri.is_empty() {
                    format!(
                        "[Attached image: {}]",
                        resource_display_name("", None, None)
                    )
                } else {
                    format!(
                        "[Attached image: {}]\nURI: {}",
                        resource_display_name(&uri, None, None),
                        uri
                    )
                }),
                json_image_part(build_image_data_url(&image_mime, &bytes)),
            ];
        }

        if let Some(mut text) =
            decode_text_bytes(&bytes[..bytes.len().min(MAX_ACP_RESOURCE_BYTES)], mime)
        {
            if bytes.len() > MAX_ACP_RESOURCE_BYTES {
                text.push_str(&format!(
                    "\n\n[Truncated to {} of {} bytes]",
                    MAX_ACP_RESOURCE_BYTES,
                    bytes.len()
                ));
            }
            return vec![json_text_part(format_resource_text(
                &uri, &text, None, None, None,
            ))];
        }
        return vec![json_text_part(format_resource_text(
            &uri,
            &format!(
                "[Binary embedded file omitted: {} bytes, mime={}]",
                bytes.len(),
                canonical_mime(mime).unwrap_or_else(|| "unknown".to_string())
            ),
            None,
            None,
            None,
        ))];
    }

    Vec::new()
}

fn extract_prompt_payload(p: &serde_json::Map<String, Value>) -> PromptExtraction {
    if let Some(prompt_val) = p.get("prompt") {
        if let Some(s) = prompt_val.as_str() {
            let text = s.to_string();
            return PromptExtraction {
                user_text: text.clone(),
                user_content: Value::String(text.clone()),
                text_only_prompt: true,
                has_content: !text.trim().is_empty(),
            };
        }
        if let Some(arr) = prompt_val.as_array() {
            let mut parts: Vec<Value> = Vec::new();
            let mut text_parts: Vec<String> = Vec::new();
            let mut text_only_prompt = true;

            for block in arr {
                let Some(obj) = block.as_object() else {
                    if let Some(text) = block.as_str() {
                        let text = text.to_string();
                        parts.push(json_text_part(text.clone()));
                        text_parts.push(text);
                    }
                    continue;
                };
                let kind = obj.get("type").and_then(|v| v.as_str()).unwrap_or("text");
                match kind {
                    "text" => {
                        if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                            let text = text.to_string();
                            parts.push(json_text_part(text.clone()));
                            text_parts.push(text);
                        }
                    }
                    "image" => {
                        text_only_prompt = false;
                        let url = obj
                            .get("url")
                            .and_then(|v| v.as_str())
                            .or_else(|| {
                                obj.get("image_url")
                                    .and_then(|v| v.get("url"))
                                    .and_then(|v| v.as_str())
                            })
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        if !url.is_empty() {
                            let header = format!("[Attached image]\nURL: {url}");
                            text_parts.push(header.clone());
                            parts.push(json_text_part(header));
                            parts.push(json_image_part(url));
                        }
                    }
                    "resource_link" => {
                        text_only_prompt = false;
                        let resource_parts = resource_link_to_parts(obj);
                        for part in resource_parts {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                text_parts.push(text.to_string());
                            }
                            parts.push(part);
                        }
                    }
                    "resource" => {
                        text_only_prompt = false;
                        let resource_parts = embedded_resource_to_parts(obj);
                        for part in resource_parts {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                text_parts.push(text.to_string());
                            }
                            parts.push(part);
                        }
                    }
                    _ => {
                        if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                            let text = text.to_string();
                            parts.push(json_text_part(text.clone()));
                            text_parts.push(text);
                        }
                        if kind != "text" {
                            text_only_prompt = false;
                        }
                    }
                }
            }

            let user_text = text_parts.join("\n");
            let has_content = !parts.is_empty() || !user_text.trim().is_empty();
            return PromptExtraction {
                user_text,
                user_content: if parts.is_empty() {
                    Value::String(String::new())
                } else {
                    Value::Array(parts)
                },
                text_only_prompt,
                has_content,
            };
        }
    }

    let fallback = p
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    PromptExtraction {
        user_text: fallback.clone(),
        user_content: Value::String(fallback.clone()),
        text_only_prompt: true,
        has_content: !fallback.trim().is_empty(),
    }
}

// ---------------------------------------------------------------------------
// Slash commands
// ---------------------------------------------------------------------------

struct SlashCommand {
    name: &'static str,
    description: &'static str,
}

const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "help",
        description: "Show available commands",
    },
    SlashCommand {
        name: "model",
        description: "Show or change current model",
    },
    SlashCommand {
        name: "tools",
        description: "List available tools",
    },
    SlashCommand {
        name: "context",
        description: "Show conversation context info",
    },
    SlashCommand {
        name: "reset",
        description: "Clear conversation history",
    },
    SlashCommand {
        name: "compact",
        description: "Compress conversation context",
    },
    SlashCommand {
        name: "version",
        description: "Show Hermes version",
    },
];

// ---------------------------------------------------------------------------
// HermesAcpHandler
// ---------------------------------------------------------------------------

/// Full ACP handler wrapping Hermes agent capabilities.
pub struct HermesAcpHandler {
    pub session_manager: Arc<SessionManager>,
    pub event_sink: Arc<EventSink>,
    pub permission_store: Arc<PermissionStore>,
    version: String,
    prompt_executor: Option<Arc<dyn AcpPromptExecutor>>,
    auth_provider_resolver: Arc<dyn Fn() -> Option<String> + Send + Sync>,
}

impl HermesAcpHandler {
    pub fn new(
        session_manager: Arc<SessionManager>,
        event_sink: Arc<EventSink>,
        permission_store: Arc<PermissionStore>,
    ) -> Self {
        Self {
            session_manager,
            event_sink,
            permission_store,
            version: env!("CARGO_PKG_VERSION").to_string(),
            prompt_executor: None,
            auth_provider_resolver: Arc::new(detect_provider),
        }
    }

    pub fn with_prompt_executor(mut self, prompt_executor: Arc<dyn AcpPromptExecutor>) -> Self {
        self.prompt_executor = Some(prompt_executor);
        self
    }

    pub fn with_auth_provider_resolver(
        mut self,
        resolver: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    ) -> Self {
        self.auth_provider_resolver = resolver;
        self
    }

    fn available_tools(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("bash", "Execute shell commands with approval controls"),
            ("read", "Read files from the local workspace"),
            ("write", "Write or create files in the local workspace"),
            ("edit", "Patch files in-place"),
            ("grep", "Search file contents"),
            ("glob", "Find files by pattern"),
            ("web_search", "Search the web"),
            ("web_fetch", "Fetch and parse URLs"),
            ("memory", "Read/write persistent memory notes"),
            ("session_search", "Search prior session content"),
            ("skills_list", "List installed skills"),
            ("skill_view", "Inspect a specific skill"),
            ("skill_manage", "Install/update/remove skills"),
            ("todo", "Track task progress"),
            ("cronjob", "Schedule recurring jobs"),
        ]
    }

    fn compact_session_history(&self, session_id: &str) -> Option<String> {
        let state = self.session_manager.get_session(session_id)?;
        let total = state.history.len();
        if total == 0 {
            return Some("Conversation is empty (nothing to compact).".to_string());
        }
        if total <= 8 {
            return Some(format!(
                "Conversation is already compact ({} messages).",
                total
            ));
        }

        let keep_recent = 6usize;
        let split = total.saturating_sub(keep_recent);
        let (older, recent) = state.history.split_at(split);

        let mut preserved_system = Vec::new();
        let mut summary_lines = Vec::new();
        for msg in older {
            let role = msg
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            if role == "system" {
                preserved_system.push(msg.clone());
            }

            let content = msg
                .get("content")
                .or_else(|| msg.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .replace('\n', " ");
            if content.is_empty() {
                continue;
            }
            let preview = if content.chars().count() > 140 {
                let head: String = content.chars().take(140).collect();
                format!("{head}...")
            } else {
                content
            };
            summary_lines.push(format!("- {}: {}", role, preview));
            if summary_lines.len() >= 10 {
                break;
            }
        }

        if summary_lines.is_empty() {
            summary_lines.push("- (no textual content in compacted segment)".to_string());
        }

        let summary = format!(
            "Compressed {} earlier messages into summary context.\n{}",
            older.len(),
            summary_lines.join("\n")
        );

        let summary_msg = json!({
            "role": "system",
            "content": summary,
            "meta": {
                "compressed": true,
                "compressed_message_count": older.len()
            }
        });

        let mut new_history = Vec::new();
        new_history.extend(preserved_system);
        new_history.push(summary_msg);
        new_history.extend_from_slice(recent);
        let new_total = new_history.len();

        self.session_manager.set_history(session_id, new_history);
        self.session_manager.save_session(session_id);

        Some(format!(
            "Context compacted: {} -> {} messages (compressed {}).",
            total,
            new_total,
            older.len()
        ))
    }

    fn handle_slash_command(&self, text: &str, session_id: &str) -> Option<String> {
        let parts: Vec<&str> = text.splitn(2, ' ').collect();
        let cmd = parts[0].trim_start_matches('/').to_lowercase();
        let args = if parts.len() > 1 { parts[1].trim() } else { "" };

        match cmd.as_str() {
            "help" => {
                let mut lines = vec!["Available commands:".to_string(), String::new()];
                for sc in SLASH_COMMANDS {
                    lines.push(format!("  /{:<10}  {}", sc.name, sc.description));
                }
                lines.push(String::new());
                lines.push(
                    "Unrecognized /commands are sent to the model as normal messages.".to_string(),
                );
                Some(lines.join("\n"))
            }
            "model" => {
                let state = self.session_manager.get_session(session_id)?;
                let model = state.model.as_deref().unwrap_or("unknown");
                let provider = state.provider.as_deref().unwrap_or("auto");
                Some(format!("Current model: {model}\nProvider: {provider}"))
            }
            "context" => {
                let state = self.session_manager.get_session(session_id)?;
                let n = state.history.len();
                if n == 0 {
                    return Some("Conversation is empty (no messages yet).".to_string());
                }
                let mut roles: HashMap<String, usize> = HashMap::new();
                for msg in &state.history {
                    let role = msg
                        .get("role")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    *roles.entry(role.to_string()).or_default() += 1;
                }
                Some(format!(
                    "Conversation: {} messages\n  user: {}, assistant: {}, tool: {}, system: {}",
                    n,
                    roles.get("user").unwrap_or(&0),
                    roles.get("assistant").unwrap_or(&0),
                    roles.get("tool").unwrap_or(&0),
                    roles.get("system").unwrap_or(&0),
                ))
            }
            "reset" => {
                self.session_manager.set_history(session_id, Vec::new());
                self.session_manager.save_session(session_id);
                Some("Conversation history cleared.".to_string())
            }
            "compact" => self.compact_session_history(session_id),
            "version" => Some(format!("Hermes Agent v{}", self.version)),
            "tools" => {
                let tools = self.available_tools();
                if tools.is_empty() {
                    Some("No tools are currently available.".to_string())
                } else if args.eq_ignore_ascii_case("json") {
                    Some(
                        serde_json::to_string_pretty(
                            &tools
                                .iter()
                                .map(|(name, description)| {
                                    json!({"name": name, "description": description})
                                })
                                .collect::<Vec<_>>(),
                        )
                        .unwrap_or_else(|_| "[]".to_string()),
                    )
                } else {
                    let mut lines =
                        vec![format!("Available tools ({}):", tools.len()), String::new()];
                    for (name, description) in &tools {
                        lines.push(format!("  /tool {:<14} {}", name, description));
                    }
                    lines.push(String::new());
                    lines.push("Tip: use `/tools json` for machine-readable output.".to_string());
                    Some(lines.join("\n"))
                }
            }
            _ => None,
        }
    }
}

fn params_obj(params: &Option<Value>) -> Option<&serde_json::Map<String, Value>> {
    params.as_ref()?.as_object()
}

fn param_str<'a>(p: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    p.get(key)?.as_str()
}

fn param_value_as_string(p: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    let value = p.get(key)?;
    if let Some(s) = value.as_str() {
        Some(s.to_string())
    } else {
        Some(value.to_string())
    }
}

#[async_trait::async_trait]
impl AcpHandler for HermesAcpHandler {
    async fn handle_request(&self, request: AcpRequest) -> AcpResponse {
        let method = AcpMethod::from(request.method.as_str());
        match method {
            // -- Lifecycle --------------------------------------------------
            AcpMethod::Initialize => {
                let auth_provider = (self.auth_provider_resolver)();
                let resp = InitializeResponse {
                    protocol_version: 1,
                    agent_info: Implementation {
                        name: "hermes-agent".to_string(),
                        version: self.version.clone(),
                    },
                    agent_capabilities: AgentCapabilities {
                        load_session: true,
                        session_capabilities: Some(SessionCapabilities {
                            fork: true,
                            list: true,
                            resume: true,
                        }),
                        streaming: true,
                        ..Default::default()
                    },
                    auth_methods: Some(build_auth_methods_for_provider(auth_provider.as_deref())),
                };
                AcpResponse::success(request.id, serde_json::to_value(&resp).unwrap())
            }

            AcpMethod::Authenticate => {
                let method_id = params_obj(&request.params)
                    .and_then(|p| param_str(p, "method_id").or_else(|| param_str(p, "methodId")))
                    .map(str::trim)
                    .unwrap_or("");
                let normalized_method = method_id.to_ascii_lowercase();
                let provider = (self.auth_provider_resolver)()
                    .map(|provider| provider.trim().to_ascii_lowercase())
                    .filter(|provider| !provider.is_empty());
                let accepted = match provider.as_deref() {
                    Some(provider) if normalized_method == provider => true,
                    Some(_) if normalized_method == TERMINAL_SETUP_AUTH_METHOD_ID => true,
                    _ => false,
                };
                if accepted {
                    AcpResponse::success(request.id, json!({}))
                } else {
                    AcpResponse::success(request.id, Value::Null)
                }
            }

            // -- Session management -----------------------------------------
            AcpMethod::NewSession => {
                let cwd = params_obj(&request.params)
                    .and_then(|p| param_str(p, "cwd"))
                    .unwrap_or(".");
                let state = self.session_manager.create_session(cwd);
                AcpResponse::success(request.id, json!({"session_id": state.session_id}))
            }

            AcpMethod::LoadSession => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(request.id, -32602, "Missing params");
                };
                let session_id = param_str(p, "session_id").unwrap_or("");
                let cwd = param_str(p, "cwd").unwrap_or(".");

                match self.session_manager.update_cwd(session_id, cwd) {
                    Some(_) => AcpResponse::success(request.id, json!({})),
                    None => AcpResponse::error(
                        request.id,
                        -32602,
                        format!("Session not found: {}", session_id),
                    ),
                }
            }

            AcpMethod::ResumeSession => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(request.id, -32602, "Missing params");
                };
                let session_id = param_str(p, "session_id").unwrap_or("");
                let cwd = param_str(p, "cwd").unwrap_or(".");

                if self.session_manager.get_session(session_id).is_some() {
                    self.session_manager.update_cwd(session_id, cwd);
                    AcpResponse::success(request.id, json!({}))
                } else {
                    let state = self.session_manager.create_session(cwd);
                    AcpResponse::success(request.id, json!({"session_id": state.session_id}))
                }
            }

            AcpMethod::ForkSession => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(request.id, -32602, "Missing params");
                };
                let session_id = param_str(p, "session_id").unwrap_or("");
                let cwd = param_str(p, "cwd").unwrap_or(".");

                match self.session_manager.fork_session(session_id, cwd) {
                    Some(new_state) => AcpResponse::success(
                        request.id,
                        json!({"session_id": new_state.session_id}),
                    ),
                    None => AcpResponse::error(
                        request.id,
                        -32602,
                        format!("Session not found: {}", session_id),
                    ),
                }
            }

            AcpMethod::ListSessions => {
                let sessions: Vec<Value> = self
                    .session_manager
                    .list_sessions()
                    .iter()
                    .map(|s| {
                        json!({
                            "session_id": s.session_id,
                            "cwd": s.cwd,
                        })
                    })
                    .collect();
                AcpResponse::success(request.id, json!({"sessions": sessions}))
            }

            AcpMethod::Cancel => {
                let session_id = params_obj(&request.params)
                    .and_then(|p| param_str(p, "session_id"))
                    .unwrap_or("");
                self.session_manager
                    .set_phase(session_id, SessionPhase::Cancelled);
                tracing::info!("Cancelled session {}", session_id);
                AcpResponse::success(request.id, json!({"cancelled": true}))
            }

            // -- Prompt (core) ----------------------------------------------
            AcpMethod::Prompt => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(request.id, -32602, "Missing params");
                };
                let session_id = param_str(p, "session_id").unwrap_or("");

                if self.session_manager.get_session(session_id).is_none() {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        format!("Session not found: {}", session_id),
                    );
                }

                let extraction = extract_prompt_payload(p);
                let user_text = extraction.user_text;
                let user_content = extraction.user_content;
                let text_only_prompt = extraction.text_only_prompt;
                let has_content = extraction.has_content;

                if !has_content {
                    return AcpResponse::success(request.id, json!({"stop_reason": "end_turn"}));
                }

                // Intercept slash commands
                if text_only_prompt && user_text.starts_with('/') {
                    if let Some(response_text) = self.handle_slash_command(&user_text, session_id) {
                        self.event_sink
                            .push(AcpEvent::message_complete(session_id, &response_text));
                        return AcpResponse::success(
                            request.id,
                            json!({"stop_reason": "end_turn"}),
                        );
                    }
                }

                self.session_manager
                    .set_phase(session_id, SessionPhase::Active);

                let mut history = self
                    .session_manager
                    .get_session(session_id)
                    .map(|s| s.history)
                    .unwrap_or_default();
                history.push(json!({
                    "role": "user",
                    "content": user_content,
                }));
                self.session_manager
                    .set_history(session_id, history.clone());

                self.event_sink
                    .push(AcpEvent::thinking(session_id, "Processing prompt..."));
                let session_snapshot = self
                    .session_manager
                    .get_session(session_id)
                    .ok_or_else(|| format!("Session not found: {session_id}"));
                let session_snapshot = match session_snapshot {
                    Ok(s) => s,
                    Err(e) => {
                        self.event_sink.push(AcpEvent::error(session_id, &e));
                        self.session_manager
                            .set_phase(session_id, SessionPhase::Failed);
                        return AcpResponse::error(request.id, -32602, e);
                    }
                };

                let prompt_result = if let Some(executor) = &self.prompt_executor {
                    executor
                        .execute_prompt(&session_snapshot, &user_text, &history)
                        .await
                } else {
                    let turn = history
                        .iter()
                        .filter(|m| {
                            m.get("role")
                                .and_then(|v| v.as_str())
                                .map(|r| r == "user")
                                .unwrap_or(false)
                        })
                        .count();
                    let snippet = user_text.chars().take(200).collect::<String>();
                    Ok(PromptExecutionOutput {
                        response_text: format!(
                            "ACP session {} processed turn {}.\n\n{}",
                            session_id, turn, snippet
                        ),
                        usage: None,
                        total_turns: Some(1),
                        events: Vec::new(),
                    })
                };
                let prompt_result = match prompt_result {
                    Ok(r) => r,
                    Err(err) => {
                        self.event_sink.push(AcpEvent::error(session_id, &err));
                        self.session_manager
                            .set_phase(session_id, SessionPhase::Failed);
                        return AcpResponse::error(request.id, -32000, err);
                    }
                };

                let PromptExecutionOutput {
                    response_text,
                    usage,
                    total_turns,
                    events,
                } = prompt_result;

                for event in events {
                    self.event_sink.push(event);
                }

                let response_text = response_text.trim().to_string();
                if !response_text.is_empty() {
                    self.event_sink
                        .push(AcpEvent::message_delta(session_id, &response_text));
                    self.event_sink
                        .push(AcpEvent::message_complete(session_id, &response_text));
                }
                self.event_sink.push(AcpEvent::step_complete(
                    session_id,
                    total_turns.unwrap_or(1),
                ));

                history.push(json!({
                    "role": "assistant",
                    "content": response_text,
                }));
                self.session_manager.set_history(session_id, history);

                if let Some(usage) = usage.as_ref() {
                    self.session_manager.add_usage(
                        session_id,
                        usage.input_tokens,
                        usage.output_tokens,
                    );
                }
                self.session_manager.save_session(session_id);

                self.session_manager
                    .set_phase(session_id, SessionPhase::Idle);

                let mut payload = json!({"stop_reason": "end_turn"});
                if let Some(usage) = usage {
                    payload["usage"] = serde_json::to_value(usage).unwrap_or_else(|_| json!({}));
                }
                AcpResponse::success(request.id, payload)
            }

            // -- Session configuration --------------------------------------
            AcpMethod::SetSessionModel => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(request.id, -32602, "Missing params");
                };
                let session_id = param_str(p, "session_id").unwrap_or("");
                let model_id = param_str(p, "model_id")
                    .or_else(|| param_str(p, "model"))
                    .unwrap_or("");

                if model_id.trim().is_empty() {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "Missing model_id/model for session/set_model",
                    );
                }

                match self.session_manager.update_model(session_id, model_id) {
                    Some(_) => {
                        tracing::info!("Session {}: model switched to {}", session_id, model_id);
                        AcpResponse::success(request.id, json!({}))
                    }
                    None => AcpResponse::error(
                        request.id,
                        -32602,
                        format!("Session not found: {}", session_id),
                    ),
                }
            }

            AcpMethod::SetSessionMode => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(request.id, -32602, "Missing params");
                };
                let session_id = param_str(p, "session_id").unwrap_or("");
                let mode_id = param_str(p, "mode_id")
                    .or_else(|| param_str(p, "mode"))
                    .unwrap_or("");
                if mode_id.trim().is_empty() {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "Missing mode_id/mode for session/set_mode",
                    );
                }
                match self.session_manager.update_mode(session_id, mode_id) {
                    Some(_) => AcpResponse::success(request.id, json!({})),
                    None => AcpResponse::error(
                        request.id,
                        -32602,
                        format!("Session not found: {}", session_id),
                    ),
                }
            }

            AcpMethod::SetConfigOption => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(request.id, -32602, "Missing params");
                };
                let session_id = param_str(p, "session_id").unwrap_or("");
                let key = param_str(p, "key")
                    .or_else(|| param_str(p, "option"))
                    .or_else(|| param_str(p, "name"))
                    .unwrap_or("");
                let value = param_value_as_string(p, "value")
                    .or_else(|| param_value_as_string(p, "option_value"))
                    .unwrap_or_default();

                if key.trim().is_empty() {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "Missing key/option for session/set_config",
                    );
                }

                match self
                    .session_manager
                    .set_config_option(session_id, key, &value)
                {
                    Some(_) => AcpResponse::success(
                        request.id,
                        json!({"config_options": [{"key": key, "value": value}]}),
                    ),
                    None => AcpResponse::error(
                        request.id,
                        -32602,
                        format!("Session not found: {}", session_id),
                    ),
                }
            }

            // -- Legacy methods ---------------------------------------------
            AcpMethod::CreateConversation => {
                let state = self.session_manager.create_session(".");
                AcpResponse::success(request.id, json!({"conversation_id": state.session_id}))
            }

            AcpMethod::SendMessage => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "message.send: missing params object",
                    );
                };
                let conv_id = param_str(p, "conversation_id").unwrap_or("");
                let text = param_str(p, "text")
                    .or_else(|| param_str(p, "content"))
                    .unwrap_or("");
                let msg_id = uuid::Uuid::new_v4().to_string();

                if let Some(state) = self.session_manager.get_session(conv_id) {
                    let mut history = state.history.clone();
                    history.push(json!({
                        "id": msg_id,
                        "role": "user",
                        "content": text,
                    }));
                    self.session_manager.set_history(conv_id, history);
                    AcpResponse::success(
                        request.id,
                        json!({"message_id": msg_id, "conversation_id": conv_id}),
                    )
                } else {
                    AcpResponse::error(
                        request.id,
                        -32602,
                        format!("Unknown conversation_id '{}'", conv_id),
                    )
                }
            }

            AcpMethod::GetHistory => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "history.get: missing params object",
                    );
                };
                let conv_id = param_str(p, "conversation_id").unwrap_or("");
                let messages = self
                    .session_manager
                    .get_session(conv_id)
                    .map(|s| s.history)
                    .unwrap_or_default();
                AcpResponse::success(request.id, json!({"messages": messages}))
            }

            AcpMethod::ListTools => {
                let tools: Vec<Value> = self
                    .available_tools()
                    .into_iter()
                    .map(|(name, description)| {
                        json!({
                            "name": name,
                            "description": description,
                        })
                    })
                    .collect();
                AcpResponse::success(request.id, json!({"tools": tools}))
            }

            AcpMethod::ExecuteTool => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "tools.execute: missing params object",
                    );
                };
                let name = p
                    .get("name")
                    .or_else(|| p.get("tool"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let arguments = p.get("arguments").cloned().unwrap_or(Value::Null);
                AcpResponse::success(
                    request.id,
                    json!({
                        "tool": name,
                        "arguments": arguments,
                        "result": format!("ACP handler echo for tool '{}'", name),
                    }),
                )
            }

            AcpMethod::GetStatus => AcpResponse::success(
                request.id,
                json!({
                    "status": "ready",
                    "version": self.version,
                }),
            ),

            AcpMethod::Unknown(method) => {
                AcpResponse::error(request.id, -32601, format!("Method not found: {}", method))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Default handler (backward compat)
// ---------------------------------------------------------------------------

/// Minimal default ACP handler for backward compatibility.
pub struct DefaultAcpHandler {
    inner: HermesAcpHandler,
}

impl DefaultAcpHandler {
    pub fn new() -> Self {
        Self {
            inner: HermesAcpHandler::new(
                Arc::new(SessionManager::new()),
                Arc::new(EventSink::default()),
                Arc::new(PermissionStore::new()),
            ),
        }
    }
}

impl Default for DefaultAcpHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl AcpHandler for DefaultAcpHandler {
    async fn handle_request(&self, request: AcpRequest) -> AcpResponse {
        self.inner.handle_request(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::AcpEventKind;
    use serde_json::json;

    struct EchoPromptExecutor;

    #[async_trait::async_trait]
    impl AcpPromptExecutor for EchoPromptExecutor {
        async fn execute_prompt(
            &self,
            _session: &SessionState,
            user_text: &str,
            _history: &[Value],
        ) -> Result<PromptExecutionOutput, String> {
            Ok(PromptExecutionOutput {
                response_text: format!("executor:{user_text}"),
                usage: Some(Usage {
                    input_tokens: 3,
                    output_tokens: 5,
                    total_tokens: 8,
                    thought_tokens: None,
                    cached_read_tokens: None,
                }),
                total_turns: Some(2),
                events: Vec::new(),
            })
        }
    }

    struct ToolEventPromptExecutor;

    #[async_trait::async_trait]
    impl AcpPromptExecutor for ToolEventPromptExecutor {
        async fn execute_prompt(
            &self,
            session: &SessionState,
            _user_text: &str,
            _history: &[Value],
        ) -> Result<PromptExecutionOutput, String> {
            Ok(PromptExecutionOutput {
                response_text: "done".to_string(),
                usage: None,
                total_turns: Some(1),
                events: vec![
                    AcpEvent::tool_call_start(
                        &session.session_id,
                        "tc-read",
                        "read_file",
                        Some(json!({"path": "/tmp/a.txt"})),
                    ),
                    AcpEvent::tool_call_complete(
                        &session.session_id,
                        "tc-read",
                        "read_file",
                        Some("contents".to_string()),
                    ),
                ],
            })
        }
    }

    fn make_handler() -> HermesAcpHandler {
        HermesAcpHandler::new(
            Arc::new(SessionManager::new()),
            Arc::new(EventSink::default()),
            Arc::new(PermissionStore::new()),
        )
    }

    fn make_handler_with_auth_provider(provider: Option<&'static str>) -> HermesAcpHandler {
        make_handler().with_auth_provider_resolver(Arc::new(move || provider.map(str::to_string)))
    }

    #[tokio::test]
    async fn test_initialize() {
        let handler = make_handler();
        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: None,
        };
        let resp = handler.handle_request(req).await;
        assert!(resp.result.is_some());
        let result = resp.result.unwrap();
        assert_eq!(result["agent_info"]["name"], "hermes-agent");
    }

    #[tokio::test]
    async fn test_initialize_advertises_provider_and_terminal_auth_methods() {
        let handler = make_handler_with_auth_provider(Some("openrouter"));
        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: None,
        };
        let resp = handler.handle_request(req).await;
        let result = resp.result.unwrap();
        let methods = result["auth_methods"].as_array().expect("auth methods");

        assert_eq!(methods[0]["id"], "openrouter");
        assert_eq!(methods[0]["name"], "openrouter runtime credentials");
        let terminal = methods
            .iter()
            .find(|method| method["id"] == TERMINAL_SETUP_AUTH_METHOD_ID)
            .expect("terminal setup auth method");
        assert_eq!(terminal["type"], "terminal");
        assert_eq!(terminal["args"], json!(["--setup"]));
    }

    #[tokio::test]
    async fn test_initialize_advertises_terminal_setup_auth_when_no_provider() {
        let handler = make_handler_with_auth_provider(None);
        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: None,
        };
        let resp = handler.handle_request(req).await;
        let result = resp.result.unwrap();

        assert_eq!(
            result["auth_methods"],
            json!([{
                "args": ["--setup"],
                "description": "Open Hermes' interactive model/provider setup in a terminal. Use this when Hermes has not been configured on this machine yet.",
                "id": TERMINAL_SETUP_AUTH_METHOD_ID,
                "name": "Configure Hermes provider",
                "type": "terminal",
            }])
        );
    }

    #[tokio::test]
    async fn test_authenticate_accepts_matching_method_id_case_insensitively() {
        let handler = make_handler_with_auth_provider(Some("openrouter"));
        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "authenticate".into(),
            params: Some(json!({"method_id": "OpenRouter"})),
        };

        let resp = handler.handle_request(req).await;
        assert_eq!(resp.result, Some(json!({})));
        assert!(resp.error.is_none());
    }

    #[tokio::test]
    async fn test_authenticate_rejects_mismatched_method_id() {
        let handler = make_handler_with_auth_provider(Some("openrouter"));
        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "authenticate".into(),
            params: Some(json!({"method_id": "totally-invalid-method"})),
        };

        let resp = handler.handle_request(req).await;
        assert_eq!(resp.result, Some(Value::Null));
        assert!(resp.error.is_none());
    }

    #[tokio::test]
    async fn test_authenticate_accepts_terminal_setup_after_provider_configured() {
        let handler = make_handler_with_auth_provider(Some("openrouter"));
        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "authenticate".into(),
            params: Some(json!({"method_id": TERMINAL_SETUP_AUTH_METHOD_ID})),
        };

        let resp = handler.handle_request(req).await;
        assert_eq!(resp.result, Some(json!({})));
        assert!(resp.error.is_none());
    }

    #[tokio::test]
    async fn test_authenticate_rejects_terminal_setup_without_provider() {
        let handler = make_handler_with_auth_provider(None);
        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "authenticate".into(),
            params: Some(json!({"method_id": TERMINAL_SETUP_AUTH_METHOD_ID})),
        };

        let resp = handler.handle_request(req).await;
        assert_eq!(resp.result, Some(Value::Null));
        assert!(resp.error.is_none());
    }

    #[tokio::test]
    async fn test_session_lifecycle() {
        let handler = make_handler();

        // Create session
        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/new".into(),
            params: Some(json!({"cwd": "/tmp"})),
        };
        let resp = handler.handle_request(req).await;
        let session_id = resp.result.unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        // List sessions
        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "session/list".into(),
            params: None,
        };
        let resp = handler.handle_request(req).await;
        let sessions = resp.result.unwrap()["sessions"].as_array().unwrap().clone();
        assert_eq!(sessions.len(), 1);

        // Fork session
        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(3)),
            method: "session/fork".into(),
            params: Some(json!({"session_id": session_id, "cwd": "/other"})),
        };
        let resp = handler.handle_request(req).await;
        assert!(resp.result.is_some());
    }

    #[tokio::test]
    async fn test_prompt_slash_command() {
        let handler = make_handler();

        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/new".into(),
            params: Some(json!({"cwd": "."})),
        };
        let resp = handler.handle_request(req).await;
        let session_id = resp.result.unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "prompt".into(),
            params: Some(json!({
                "session_id": session_id,
                "text": "/help",
            })),
        };
        let resp = handler.handle_request(req).await;
        assert_eq!(
            resp.result.unwrap()["stop_reason"].as_str().unwrap(),
            "end_turn"
        );
    }

    #[tokio::test]
    async fn test_prompt_resource_link_inlines_text_file() {
        let handler = HermesAcpHandler::new(
            Arc::new(SessionManager::new()),
            Arc::new(EventSink::default()),
            Arc::new(PermissionStore::new()),
        )
        .with_prompt_executor(Arc::new(EchoPromptExecutor));

        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/new".into(),
            params: Some(json!({"cwd": "."})),
        };
        let resp = handler.handle_request(req).await;
        let session_id = resp.result.unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let tmp_path = std::env::temp_dir().join(format!("hermes-acp-{}.txt", session_id));
        std::fs::write(&tmp_path, "trade-edge-notes").expect("write resource file");
        let file_uri = format!("file://{}", tmp_path.display());

        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "prompt".into(),
            params: Some(json!({
                "session_id": session_id.clone(),
                "prompt": [
                    {"type": "text", "text": "review this"},
                    {"type": "resource_link", "uri": file_uri, "name": "notes.txt", "mimeType": "text/plain"}
                ]
            })),
        };
        let resp = handler.handle_request(req).await;
        assert_eq!(
            resp.result.as_ref().unwrap()["stop_reason"].as_str(),
            Some("end_turn")
        );
        let state = handler.session_manager.get_session(&session_id).unwrap();
        let user_content = state
            .history
            .iter()
            .find(|v| v.get("role").and_then(|r| r.as_str()) == Some("user"))
            .and_then(|v| v.get("content"))
            .cloned()
            .unwrap_or(Value::Null);
        assert!(user_content.is_array());
        let flattened = user_content
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flattened.contains("trade-edge-notes"));

        let _ = std::fs::remove_file(tmp_path);
    }

    #[tokio::test]
    async fn test_prompt_with_image_and_slash_text_not_intercepted_as_command() {
        let handler = make_handler();

        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/new".into(),
            params: Some(json!({"cwd": "."})),
        };
        let resp = handler.handle_request(req).await;
        let session_id = resp.result.unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "prompt".into(),
            params: Some(json!({
                "session_id": session_id.clone(),
                "prompt": [
                    {"type": "text", "text": "/help"},
                    {"type": "image", "url": "https://example.com/chart.png"}
                ]
            })),
        };
        let _ = handler.handle_request(req).await;

        let state = handler.session_manager.get_session(&session_id).unwrap();
        assert!(state.history.len() >= 2);
        let assistant_text = state
            .history
            .last()
            .and_then(|v| v.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(assistant_text.contains("ACP session"));
    }

    #[tokio::test]
    async fn test_unknown_method() {
        let handler = make_handler();
        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "foo.bar".into(),
            params: None,
        };
        let resp = handler.handle_request(req).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn test_legacy_create_conversation() {
        let handler = DefaultAcpHandler::default();
        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "conversation.create".into(),
            params: None,
        };
        let resp = handler.handle_request(req).await;
        assert!(resp.result.is_some());
        assert!(resp.result.unwrap().get("conversation_id").is_some());
    }

    #[tokio::test]
    async fn test_compact_slash_command_reduces_history() {
        let handler = make_handler();
        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/new".into(),
            params: Some(json!({"cwd": "."})),
        };
        let resp = handler.handle_request(req).await;
        let session_id = resp.result.unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let mut history = Vec::new();
        for i in 0..14 {
            history.push(json!({
                "role": if i % 2 == 0 { "user" } else { "assistant" },
                "content": format!("message {}", i),
            }));
        }
        handler.session_manager.set_history(&session_id, history);

        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "prompt".into(),
            params: Some(json!({
                "session_id": session_id.clone(),
                "text": "/compact",
            })),
        };
        let _ = handler.handle_request(req).await;

        let state = handler.session_manager.get_session(&session_id).unwrap();
        assert!(state.history.len() < 14);
    }

    #[tokio::test]
    async fn test_list_tools_non_empty() {
        let handler = make_handler();
        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools.list".into(),
            params: None,
        };
        let resp = handler.handle_request(req).await;
        let tools = resp
            .result
            .unwrap()
            .get("tools")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(!tools.is_empty());
    }

    #[tokio::test]
    async fn test_prompt_uses_custom_executor_and_records_usage() {
        let handler = HermesAcpHandler::new(
            Arc::new(SessionManager::new()),
            Arc::new(EventSink::default()),
            Arc::new(PermissionStore::new()),
        )
        .with_prompt_executor(Arc::new(EchoPromptExecutor));

        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/new".into(),
            params: Some(json!({"cwd": "."})),
        };
        let resp = handler.handle_request(req).await;
        let session_id = resp.result.unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "prompt".into(),
            params: Some(json!({
                "session_id": session_id.clone(),
                "text": "hello"
            })),
        };
        let resp = handler.handle_request(req).await;
        let usage = resp.result.unwrap()["usage"].clone();
        assert_eq!(usage["input_tokens"], 3);
        assert_eq!(usage["output_tokens"], 5);

        let state = handler.session_manager.get_session(&session_id).unwrap();
        assert_eq!(state.total_prompt_tokens, 3);
        assert_eq!(state.total_completion_tokens, 5);
        assert_eq!(
            state
                .history
                .last()
                .and_then(|v| v.get("content"))
                .and_then(|v| v.as_str()),
            Some("executor:hello")
        );
    }

    #[tokio::test]
    async fn test_prompt_enqueues_executor_tool_events() {
        let event_sink = Arc::new(EventSink::default());
        let handler = HermesAcpHandler::new(
            Arc::new(SessionManager::new()),
            event_sink.clone(),
            Arc::new(PermissionStore::new()),
        )
        .with_prompt_executor(Arc::new(ToolEventPromptExecutor));

        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/new".into(),
            params: Some(json!({"cwd": "."})),
        };
        let resp = handler.handle_request(req).await;
        let session_id = resp.result.unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "prompt".into(),
            params: Some(json!({
                "session_id": session_id.clone(),
                "text": "read file"
            })),
        };
        let resp = handler.handle_request(req).await;
        assert_eq!(
            resp.result.unwrap()["stop_reason"].as_str(),
            Some("end_turn")
        );

        let events = event_sink.drain_for_session(&session_id);
        let tool_events: Vec<_> = events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    AcpEventKind::ToolCallStart | AcpEventKind::ToolCallComplete
                )
            })
            .collect();
        assert_eq!(tool_events.len(), 2);
        assert_eq!(tool_events[0].tool_call_id.as_deref(), Some("tc-read"));
        assert_eq!(tool_events[0].tool_name.as_deref(), Some("read_file"));
        assert_eq!(
            tool_events[0].arguments.as_ref().unwrap()["path"],
            "/tmp/a.txt"
        );
        assert_eq!(tool_events[1].tool_call_id.as_deref(), Some("tc-read"));
        assert_eq!(tool_events[1].result.as_deref(), Some("contents"));
    }

    #[tokio::test]
    async fn test_set_session_fields_persist() {
        let handler = make_handler();

        let req = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/new".into(),
            params: Some(json!({"cwd": "."})),
        };
        let resp = handler.handle_request(req).await;
        let session_id = resp.result.unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let set_model = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "session/set_model".into(),
            params: Some(json!({"session_id": session_id, "model_id": "nous:gpt-5.4"})),
        };
        let _ = handler.handle_request(set_model).await;

        let set_mode = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(3)),
            method: "session/set_mode".into(),
            params: Some(json!({"session_id": session_id, "mode_id": "code"})),
        };
        let _ = handler.handle_request(set_mode).await;

        let set_cfg = AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(4)),
            method: "session/set_config".into(),
            params: Some(json!({
                "session_id": session_id,
                "key": "temperature",
                "value": "0.1"
            })),
        };
        let _ = handler.handle_request(set_cfg).await;

        let state = handler.session_manager.get_session(&session_id).unwrap();
        assert_eq!(state.model.as_deref(), Some("nous:gpt-5.4"));
        assert_eq!(state.mode.as_deref(), Some("code"));
        assert_eq!(
            state.config_options.get("temperature").map(String::as_str),
            Some("0.1")
        );
    }
}
