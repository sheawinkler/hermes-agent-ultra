//! Anthropic Messages API adapter for Hermes Agent.
//!
//! Translates between Hermes's internal OpenAI-style message format and
//! Anthropic's Messages API.  All provider-specific logic is isolated here.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const THINKING_BUDGET_XHIGH: u32 = 32_000;
pub const THINKING_BUDGET_HIGH: u32 = 16_000;
pub const THINKING_BUDGET_MEDIUM: u32 = 8_000;
pub const THINKING_BUDGET_LOW: u32 = 4_000;

const MCP_TOOL_PREFIX: &str = "mcp_";

/// Max output token limits per Anthropic model.
static ANTHROPIC_OUTPUT_LIMITS: &[(&str, u32)] = &[
    ("claude-opus-4-8", 128_000),
    ("claude-opus-4-7", 128_000),
    ("claude-opus-4-6", 128_000),
    ("claude-sonnet-4-6", 64_000),
    ("claude-opus-4-5", 64_000),
    ("claude-sonnet-4-5", 64_000),
    ("claude-haiku-4-5", 64_000),
    ("claude-opus-4", 32_000),
    ("claude-sonnet-4", 64_000),
    ("claude-3-7-sonnet", 128_000),
    ("claude-3-5-sonnet", 8_192),
    ("claude-3-5-haiku", 8_192),
    ("claude-3-opus", 4_096),
    ("claude-3-sonnet", 4_096),
    ("claude-3-haiku", 4_096),
    ("minimax", 131_072),
    ("qwen3", 65_536),
];

const ANTHROPIC_DEFAULT_OUTPUT_LIMIT: u32 = 128_000;

// Beta headers
const COMMON_BETAS: &[&str] = &[
    "interleaved-thinking-2025-05-14",
    "fine-grained-tool-streaming-2025-05-14",
];
const TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";
const CONTEXT_1M_BETA: &str = "context-1m-2025-08-07";
const FAST_MODE_BETA: &str = "fast-mode-2026-02-01";
const OAUTH_ONLY_BETAS: &[&str] = &["claude-code-20250219", "oauth-2025-04-20"];
const ADAPTIVE_THINKING_SUBSTRINGS: &[&str] = &["4-6", "4.6", "4-7", "4.7", "4-8", "4.8"];
const XHIGH_EFFORT_SUBSTRINGS: &[&str] = &["4-7", "4.7", "4-8", "4.8"];
const NO_SAMPLING_PARAMS_SUBSTRINGS: &[&str] = &["4-7", "4.7", "4-8", "4.8"];
const FAST_MODE_SUPPORTED_SUBSTRINGS: &[&str] = &["opus-4-6", "opus-4.6"];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Content block types used by Anthropic's Messages API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicContentBlock {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<Value>,
    },
    Image {
        source: ImageSource,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<Value>,
    },
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    RedactedThinking {
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

/// An Anthropic-format message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: AnthropicContent,
}

/// Content can be either a plain string or a list of blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

/// Anthropic tool definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Normalized tool call extracted from Anthropic response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Normalized assistant message from Anthropic response.
#[derive(Debug, Clone)]
pub struct NormalizedAssistantMessage {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<NormalizedToolCall>>,
    pub reasoning: Option<String>,
    pub reasoning_details: Option<Vec<Value>>,
}

/// Reasoning effort levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    XHigh,
    High,
    Medium,
    Low,
    Minimal,
}

/// Reasoning configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningConfig {
    pub enabled: bool,
    pub effort: ReasoningEffort,
}

// ---------------------------------------------------------------------------
// Model name normalization
// ---------------------------------------------------------------------------

/// Return true for AWS Bedrock model IDs where dots are semantic delimiters.
pub fn is_bedrock_model_id(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    [
        "anthropic.",
        "us.anthropic.",
        "eu.anthropic.",
        "ap.anthropic.",
        "global.anthropic.",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

/// Normalize a model name for the Anthropic API.
///
/// Strips `anthropic/` prefix and converts dots to hyphens unless
/// `preserve_dots` is true. AWS Bedrock model IDs keep dots because those
/// dots are part of Bedrock's provider model identifier.
pub fn normalize_model_name(model: &str, preserve_dots: bool) -> String {
    let mut result = model.to_string();
    let lower = result.to_lowercase();
    if lower.starts_with("anthropic/") {
        result = result[10..].to_string();
    }
    if !preserve_dots && !is_bedrock_model_id(&result) {
        result = result.replace('.', "-");
    }
    result
}

/// Look up the max output token limit for an Anthropic model.
/// Uses substring matching; longest-prefix match wins.
pub fn get_anthropic_max_output(model: &str) -> u32 {
    let m = model.to_lowercase().replace('.', "-");
    let mut best_key = "";
    let mut best_val = ANTHROPIC_DEFAULT_OUTPUT_LIMIT;
    for &(key, val) in ANTHROPIC_OUTPUT_LIMITS {
        if m.contains(key) && key.len() > best_key.len() {
            best_key = key;
            best_val = val;
        }
    }
    best_val
}

/// Return true for Claude 4.6 models that support adaptive thinking.
pub fn supports_adaptive_thinking(model: &str) -> bool {
    ADAPTIVE_THINKING_SUBSTRINGS
        .iter()
        .any(|needle| model.contains(needle))
}

/// Return true for models that accept Anthropic's `xhigh` adaptive effort.
pub fn supports_xhigh_effort(model: &str) -> bool {
    XHIGH_EFFORT_SUBSTRINGS
        .iter()
        .any(|needle| model.contains(needle))
}

/// Return true for models that reject explicit sampling parameters.
pub fn forbids_sampling_params(model: &str) -> bool {
    NO_SAMPLING_PARAMS_SUBSTRINGS
        .iter()
        .any(|needle| model.contains(needle))
}

/// Return true for models that support the Anthropic `speed=fast` parameter.
pub fn supports_fast_mode(model: &str) -> bool {
    FAST_MODE_SUPPORTED_SUBSTRINGS
        .iter()
        .any(|needle| model.contains(needle))
}

// ---------------------------------------------------------------------------
// Tool / content conversion
// ---------------------------------------------------------------------------

/// Sanitize a tool call ID for the Anthropic API.
pub fn sanitize_tool_id(tool_id: &str) -> String {
    if tool_id.is_empty() {
        return "tool_0".to_string();
    }
    let sanitized: String = tool_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "tool_0".to_string()
    } else {
        sanitized
    }
}

/// Convert OpenAI tool definitions to Anthropic format.
pub fn convert_tools_to_anthropic(tools: &[Value]) -> Vec<AnthropicTool> {
    tools
        .iter()
        .filter_map(|t| {
            let func = t.get("function")?;
            Some(AnthropicTool {
                name: func.get("name")?.as_str()?.to_string(),
                description: func
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
                input_schema: func
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}})),
            })
        })
        .collect()
}

/// Convert an OpenAI-style image URL/data URL into an Anthropic image source.
pub fn image_source_from_openai_url(url: &str) -> ImageSource {
    let url = url.trim();
    if url.starts_with("data:") {
        let after_data = &url[5..];
        let (header, data) = match after_data.find(',') {
            Some(idx) => (&after_data[..idx], &after_data[idx + 1..]),
            None => ("image/jpeg", after_data),
        };
        let media_type = header.split(';').next().unwrap_or("image/jpeg");
        let media_type = if media_type.starts_with("image/") {
            media_type.to_string()
        } else {
            "image/jpeg".to_string()
        };
        ImageSource::Base64 {
            media_type,
            data: data.to_string(),
        }
    } else {
        ImageSource::Url {
            url: url.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Message conversion: OpenAI → Anthropic
// ---------------------------------------------------------------------------

/// Convert OpenAI-format messages to Anthropic format.
///
/// Returns `(system_prompt, anthropic_messages)`.
/// System messages are extracted since Anthropic takes them as a separate param.
pub fn convert_messages_to_anthropic(
    messages: &[Value],
    base_url: Option<&str>,
) -> (Option<Value>, Vec<AnthropicMessage>) {
    let mut system: Option<Value> = None;
    let mut result: Vec<AnthropicMessage> = Vec::new();

    for m in messages {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let content = m
            .get("content")
            .cloned()
            .unwrap_or(Value::String(String::new()));

        match role {
            "system" => {
                system = if content.is_array() {
                    Some(content)
                } else {
                    Some(content)
                };
            }
            "assistant" => {
                let mut blocks: Vec<AnthropicContentBlock> = Vec::new();

                if let Some(text) = content.as_str() {
                    if !text.is_empty() {
                        blocks.push(AnthropicContentBlock::Text {
                            text: text.to_string(),
                            cache_control: None,
                        });
                    }
                } else if let Some(arr) = content.as_array() {
                    for part in arr {
                        if let Some(block) = convert_content_part(part) {
                            blocks.push(block);
                        }
                    }
                }

                if let Some(tool_calls) = m.get("tool_calls").and_then(|tc| tc.as_array()) {
                    for tc in tool_calls {
                        let func = tc.get("function").cloned().unwrap_or_default();
                        let args_str = func
                            .get("arguments")
                            .and_then(|a| a.as_str())
                            .unwrap_or("{}");
                        let parsed_args: Value = serde_json::from_str(args_str)
                            .unwrap_or(Value::Object(Default::default()));
                        blocks.push(AnthropicContentBlock::ToolUse {
                            id: sanitize_tool_id(
                                tc.get("id").and_then(|i| i.as_str()).unwrap_or(""),
                            ),
                            name: func
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_string(),
                            input: parsed_args,
                        });
                    }
                }

                if blocks.is_empty() {
                    blocks.push(AnthropicContentBlock::Text {
                        text: "(empty)".to_string(),
                        cache_control: None,
                    });
                }

                result.push(AnthropicMessage {
                    role: "assistant".to_string(),
                    content: AnthropicContent::Blocks(blocks),
                });
            }
            "tool" => {
                let result_content = if let Some(s) = content.as_str() {
                    if s.is_empty() {
                        "(no output)".to_string()
                    } else {
                        s.to_string()
                    }
                } else {
                    serde_json::to_string(&content).unwrap_or_else(|_| "(no output)".to_string())
                };

                let tool_result = AnthropicContentBlock::ToolResult {
                    tool_use_id: sanitize_tool_id(
                        m.get("tool_call_id").and_then(|i| i.as_str()).unwrap_or(""),
                    ),
                    content: result_content,
                    cache_control: m.get("cache_control").cloned(),
                };

                if let Some(last) = result.last_mut() {
                    if last.role == "user" {
                        if let AnthropicContent::Blocks(ref mut blocks) = last.content {
                            if blocks.first().map_or(false, |b| {
                                matches!(b, AnthropicContentBlock::ToolResult { .. })
                            }) {
                                blocks.push(tool_result);
                                continue;
                            }
                        }
                    }
                }

                result.push(AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Blocks(vec![tool_result]),
                });
            }
            _ => {
                // Regular user message
                if let Some(arr) = content.as_array() {
                    let blocks: Vec<AnthropicContentBlock> =
                        arr.iter().filter_map(convert_content_part).collect();
                    let blocks = if blocks.is_empty() {
                        vec![AnthropicContentBlock::Text {
                            text: "(empty message)".to_string(),
                            cache_control: None,
                        }]
                    } else {
                        blocks
                    };
                    result.push(AnthropicMessage {
                        role: "user".to_string(),
                        content: AnthropicContent::Blocks(blocks),
                    });
                } else {
                    let text = content.as_str().unwrap_or("(empty message)");
                    let text = if text.trim().is_empty() {
                        "(empty message)"
                    } else {
                        text
                    };
                    result.push(AnthropicMessage {
                        role: "user".to_string(),
                        content: AnthropicContent::Text(text.to_string()),
                    });
                }
            }
        }
    }

    // Strip orphaned tool_use/tool_result blocks
    strip_orphaned_blocks(&mut result);

    // Enforce strict role alternation
    enforce_role_alternation(&mut result);

    // Strip thinking blocks from non-latest assistant messages
    strip_stale_thinking_blocks(&mut result, base_url);

    (system, result)
}

fn convert_content_part(part: &Value) -> Option<AnthropicContentBlock> {
    if let Some(s) = part.as_str() {
        return Some(AnthropicContentBlock::Text {
            text: s.to_string(),
            cache_control: None,
        });
    }
    let ptype = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match ptype {
        "text" | "input_text" => Some(AnthropicContentBlock::Text {
            text: part
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            cache_control: part.get("cache_control").cloned(),
        }),
        "image_url" | "input_image" => {
            let image_value = part.get("image_url").cloned().unwrap_or_default();
            let url = if let Some(obj) = image_value.as_object() {
                obj.get("url").and_then(|u| u.as_str()).unwrap_or("")
            } else {
                image_value.as_str().unwrap_or("")
            };
            Some(AnthropicContentBlock::Image {
                source: image_source_from_openai_url(url),
            })
        }
        _ => {
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                Some(AnthropicContentBlock::Text {
                    text: text.to_string(),
                    cache_control: part.get("cache_control").cloned(),
                })
            } else {
                None
            }
        }
    }
}

fn strip_orphaned_blocks(messages: &mut Vec<AnthropicMessage>) {
    // Collect tool_result IDs
    let mut tool_result_ids: HashSet<String> = HashSet::new();
    for m in messages.iter() {
        if m.role == "user" {
            if let AnthropicContent::Blocks(blocks) = &m.content {
                for b in blocks {
                    if let AnthropicContentBlock::ToolResult { tool_use_id, .. } = b {
                        tool_result_ids.insert(tool_use_id.clone());
                    }
                }
            }
        }
    }

    // Strip orphaned tool_use blocks (no matching tool_result)
    for m in messages.iter_mut() {
        if m.role == "assistant" {
            if let AnthropicContent::Blocks(blocks) = &mut m.content {
                blocks.retain(|b| {
                    if let AnthropicContentBlock::ToolUse { id, .. } = b {
                        tool_result_ids.contains(id)
                    } else {
                        true
                    }
                });
                if blocks.is_empty() {
                    blocks.push(AnthropicContentBlock::Text {
                        text: "(tool call removed)".to_string(),
                        cache_control: None,
                    });
                }
            }
        }
    }

    // Collect tool_use IDs
    let mut tool_use_ids: HashSet<String> = HashSet::new();
    for m in messages.iter() {
        if m.role == "assistant" {
            if let AnthropicContent::Blocks(blocks) = &m.content {
                for b in blocks {
                    if let AnthropicContentBlock::ToolUse { id, .. } = b {
                        tool_use_ids.insert(id.clone());
                    }
                }
            }
        }
    }

    // Strip orphaned tool_result blocks (no matching tool_use)
    for m in messages.iter_mut() {
        if m.role == "user" {
            if let AnthropicContent::Blocks(blocks) = &mut m.content {
                blocks.retain(|b| {
                    if let AnthropicContentBlock::ToolResult { tool_use_id, .. } = b {
                        tool_use_ids.contains(tool_use_id)
                    } else {
                        true
                    }
                });
                if blocks.is_empty() {
                    blocks.push(AnthropicContentBlock::Text {
                        text: "(tool result removed)".to_string(),
                        cache_control: None,
                    });
                }
            }
        }
    }
}

fn enforce_role_alternation(messages: &mut Vec<AnthropicMessage>) {
    let mut fixed: Vec<AnthropicMessage> = Vec::with_capacity(messages.len());
    for m in messages.drain(..) {
        if let Some(last) = fixed.last_mut() {
            if last.role == m.role {
                merge_same_role_content(last, m);
                continue;
            }
        }
        fixed.push(m);
    }
    *messages = fixed;
}

fn merge_same_role_content(target: &mut AnthropicMessage, source: AnthropicMessage) {
    match (&mut target.content, source.content) {
        (AnthropicContent::Text(a), AnthropicContent::Text(b)) => {
            a.push('\n');
            a.push_str(&b);
        }
        (AnthropicContent::Blocks(a), AnthropicContent::Blocks(b)) => {
            a.extend(b);
        }
        (AnthropicContent::Text(a), AnthropicContent::Blocks(b)) => {
            let mut blocks = vec![AnthropicContentBlock::Text {
                text: a.clone(),
                cache_control: None,
            }];
            blocks.extend(b);
            target.content = AnthropicContent::Blocks(blocks);
        }
        (AnthropicContent::Blocks(a), AnthropicContent::Text(b)) => {
            a.push(AnthropicContentBlock::Text {
                text: b,
                cache_control: None,
            });
        }
    }
}

fn strip_stale_thinking_blocks(messages: &mut [AnthropicMessage], base_url: Option<&str>) {
    let is_third_party = base_url
        .map(|url| !url.is_empty() && !url.contains("anthropic.com"))
        .unwrap_or(false);

    let last_assistant_idx = messages.iter().rposition(|m| m.role == "assistant");

    for (idx, m) in messages.iter_mut().enumerate() {
        if m.role != "assistant" {
            continue;
        }
        if let AnthropicContent::Blocks(blocks) = &mut m.content {
            if is_third_party || Some(idx) != last_assistant_idx {
                blocks.retain(|b| {
                    !matches!(
                        b,
                        AnthropicContentBlock::Thinking { .. }
                            | AnthropicContentBlock::RedactedThinking { .. }
                    )
                });
                if blocks.is_empty() {
                    blocks.push(AnthropicContentBlock::Text {
                        text: "(thinking elided)".to_string(),
                        cache_control: None,
                    });
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Response normalization: Anthropic → OpenAI-like
// ---------------------------------------------------------------------------

/// Normalize an Anthropic response into the shape expected by the agent.
pub fn normalize_anthropic_response(
    content_blocks: &[Value],
    stop_reason: &str,
    strip_tool_prefix: bool,
) -> (NormalizedAssistantMessage, String) {
    let mut text_parts: Vec<String> = Vec::new();
    let mut reasoning_parts: Vec<String> = Vec::new();
    let mut reasoning_details: Vec<Value> = Vec::new();
    let mut tool_calls: Vec<NormalizedToolCall> = Vec::new();

    for block in content_blocks {
        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    text_parts.push(text.to_string());
                }
            }
            "thinking" => {
                if let Some(thinking) = block.get("thinking").and_then(|t| t.as_str()) {
                    reasoning_parts.push(thinking.to_string());
                }
                reasoning_details.push(block.clone());
            }
            "redacted_thinking" => {
                // No plaintext reasoning; preserve the block for auditing (matches message-path handling).
                reasoning_details.push(block.clone());
            }
            "tool_use" => {
                let mut name = block
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                if strip_tool_prefix && name.starts_with(MCP_TOOL_PREFIX) {
                    name = name[MCP_TOOL_PREFIX.len()..].to_string();
                }
                tool_calls.push(NormalizedToolCall {
                    id: block
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string(),
                    name,
                    arguments: serde_json::to_string(
                        block
                            .get("input")
                            .unwrap_or(&Value::Object(Default::default())),
                    )
                    .unwrap_or_else(|_| "{}".to_string()),
                });
            }
            _ => {}
        }
    }

    let stop_reason_map: HashMap<&str, &str> = [
        ("end_turn", "stop"),
        ("tool_use", "tool_calls"),
        ("max_tokens", "length"),
        ("stop_sequence", "stop"),
    ]
    .into();

    let finish_reason = stop_reason_map
        .get(stop_reason)
        .unwrap_or(&"stop")
        .to_string();

    let msg = NormalizedAssistantMessage {
        content: if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join("\n"))
        },
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        reasoning: if reasoning_parts.is_empty() {
            None
        } else {
            Some(reasoning_parts.join("\n\n"))
        },
        reasoning_details: if reasoning_details.is_empty() {
            None
        } else {
            Some(reasoning_details)
        },
    };

    (msg, finish_reason)
}

// ---------------------------------------------------------------------------
// Auth helpers
// ---------------------------------------------------------------------------

/// Check if a key is an Anthropic OAuth/setup token.
pub fn is_oauth_token(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    if key.starts_with("sk-ant-api") {
        return false;
    }
    if key.starts_with("sk-ant-") {
        return true;
    }
    if key.starts_with("eyJ") {
        return true;
    }
    if key.starts_with("cc-") {
        return true;
    }
    false
}

/// Check if a base URL points to a third-party Anthropic-compatible endpoint.
pub fn is_third_party_endpoint(base_url: Option<&str>) -> bool {
    match base_url {
        None => false,
        Some(url) => {
            let normalized = url.trim().trim_end_matches('/').to_lowercase();
            if normalized.is_empty() {
                return false;
            }
            !normalized.contains("anthropic.com")
        }
    }
}

/// Return true for MiniMax's Anthropic-compatible endpoints.
pub fn is_minimax_anthropic_endpoint(base_url: Option<&str>) -> bool {
    match base_url {
        None => false,
        Some(url) => {
            let normalized = url.trim().trim_end_matches('/').to_lowercase();
            normalized.starts_with("https://api.minimax.io/anthropic")
                || normalized.starts_with("https://api.minimaxi.com/anthropic")
        }
    }
}

/// Return true for Azure-hosted Anthropic Messages endpoints.
pub fn is_azure_anthropic_endpoint(base_url: Option<&str>) -> bool {
    let Some(raw) = base_url.map(str::trim).filter(|url| !url.is_empty()) else {
        return false;
    };
    let Ok(parsed) = reqwest::Url::parse(raw) else {
        return false;
    };
    let host = parsed
        .host_str()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_lowercase();
    let path = parsed.path().to_lowercase();
    let host_padded = format!(".{host}.");
    let is_foundry_host = host_padded.contains(".services.ai.azure.");
    let is_legacy_azoai_host = host_padded.contains(".openai.azure.");
    (is_foundry_host || is_legacy_azoai_host) && path.contains("/anthropic")
}

/// Return true for endpoints that still gate 1M context behind a beta header.
pub fn base_url_needs_context_1m_beta(base_url: Option<&str>) -> bool {
    base_url
        .map(|url| url.trim().to_lowercase().contains("azure.com"))
        .unwrap_or(false)
}

/// Return beta headers safe for the configured endpoint.
pub fn common_betas_for_base_url(base_url: Option<&str>) -> Vec<&'static str> {
    let mut betas = COMMON_BETAS.to_vec();
    if base_url_needs_context_1m_beta(base_url) {
        betas.push(CONTEXT_1M_BETA);
    }
    if is_minimax_anthropic_endpoint(base_url) {
        betas.retain(|beta| *beta != TOOL_STREAMING_BETA && *beta != CONTEXT_1M_BETA);
    }
    betas
}

/// Beta list for `default_headers["anthropic-beta"]` when constructing an Anthropic client.
///
/// Mirrors Python: common betas for the endpoint, plus OAuth-only betas when using Bearer
/// (setup-tokens, managed keys, JWTs) on native Anthropic.
pub fn default_anthropic_beta_list(base_url: Option<&str>, is_oauth: bool) -> Vec<&'static str> {
    let mut betas = common_betas_for_base_url(base_url);
    if is_oauth {
        betas.extend_from_slice(OAUTH_ONLY_BETAS);
    }
    betas
}

/// Comma-separated `anthropic-beta` header value for [`default_anthropic_beta_list`].
pub fn default_anthropic_beta_header_value(base_url: Option<&str>, is_oauth: bool) -> String {
    default_anthropic_beta_list(base_url, is_oauth).join(",")
}

/// Full beta list for a per-request `extra_headers` override when enabling fast mode
/// (`speed: "fast"`). Only valid for native Anthropic — third-party proxies reject the
/// unknown fast-mode beta.
///
/// Returns `None` when fast mode must not be used (third-party endpoint).
pub fn fast_mode_request_beta_list(
    base_url: Option<&str>,
    is_oauth: bool,
) -> Option<Vec<&'static str>> {
    if is_third_party_endpoint(base_url) {
        return None;
    }
    let mut betas = common_betas_for_base_url(base_url);
    if is_oauth {
        betas.extend_from_slice(OAUTH_ONLY_BETAS);
    }
    betas.push(FAST_MODE_BETA);
    Some(betas)
}

/// Check if a base URL requires Bearer auth (e.g. MiniMax or Azure Foundry).
pub fn requires_bearer_auth(base_url: Option<&str>) -> bool {
    match base_url {
        None => false,
        Some(url) => {
            let normalized = url.trim().trim_end_matches('/').to_lowercase();
            is_minimax_anthropic_endpoint(Some(normalized.as_str()))
                || normalized.contains("azure.com")
        }
    }
}

/// Get thinking budget for a given effort level.
pub fn thinking_budget(effort: ReasoningEffort) -> u32 {
    match effort {
        ReasoningEffort::XHigh => THINKING_BUDGET_XHIGH,
        ReasoningEffort::High => THINKING_BUDGET_HIGH,
        ReasoningEffort::Medium => THINKING_BUDGET_MEDIUM,
        ReasoningEffort::Low | ReasoningEffort::Minimal => THINKING_BUDGET_LOW,
    }
}

/// Map effort level to Anthropic's adaptive effort string.
pub fn adaptive_effort_string(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::XHigh => "max",
        ReasoningEffort::High => "high",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::Low | ReasoningEffort::Minimal => "low",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_model_name() {
        assert_eq!(
            normalize_model_name("anthropic/claude-opus-4.6", false),
            "claude-opus-4-6"
        );
        assert_eq!(
            normalize_model_name("anthropic/claude-opus-4.6", true),
            "claude-opus-4.6"
        );
        assert_eq!(
            normalize_model_name("claude-sonnet-4", false),
            "claude-sonnet-4"
        );
        assert_eq!(
            normalize_model_name("global.anthropic.claude-opus-4-7", false),
            "global.anthropic.claude-opus-4-7"
        );
        assert_eq!(
            normalize_model_name("claude-opus-4.6", false),
            "claude-opus-4-6"
        );
    }

    #[test]
    fn test_get_anthropic_max_output() {
        assert_eq!(get_anthropic_max_output("claude-opus-4-6"), 128_000);
        assert_eq!(get_anthropic_max_output("claude-opus-4-8-fast"), 128_000);
        assert_eq!(
            get_anthropic_max_output("claude-sonnet-4-6-20260101"),
            64_000
        );
        assert_eq!(get_anthropic_max_output("claude-3-opus-20240229"), 4_096);
        assert_eq!(
            get_anthropic_max_output("unknown-model"),
            ANTHROPIC_DEFAULT_OUTPUT_LIMIT
        );
    }

    #[test]
    fn test_sanitize_tool_id() {
        assert_eq!(sanitize_tool_id("abc-123_def"), "abc-123_def");
        assert_eq!(sanitize_tool_id("abc.123"), "abc_123");
        assert_eq!(sanitize_tool_id(""), "tool_0");
    }

    #[test]
    fn test_is_oauth_token() {
        assert!(!is_oauth_token("sk-ant-api03-xxx"));
        assert!(is_oauth_token("sk-ant-oat-xxx"));
        assert!(is_oauth_token("eyJhbGci..."));
        assert!(is_oauth_token("cc-claude-code-oauth"));
        assert!(!is_oauth_token("sk-proj-xxx"));
        assert!(!is_oauth_token(""));
    }

    #[test]
    fn test_supports_adaptive_thinking() {
        assert!(supports_adaptive_thinking("claude-opus-4-6"));
        assert!(supports_adaptive_thinking("claude-sonnet-4.6"));
        assert!(supports_adaptive_thinking("claude-opus-4-7"));
        assert!(supports_adaptive_thinking("claude-opus-4.8"));
        assert!(!supports_adaptive_thinking("claude-sonnet-4"));
    }

    #[test]
    fn test_sampling_and_fast_mode_predicates_cover_opus_4_8() {
        assert!(supports_xhigh_effort("claude-opus-4-7"));
        assert!(supports_xhigh_effort("claude-opus-4.8-fast"));
        assert!(forbids_sampling_params("claude-opus-4-7"));
        assert!(forbids_sampling_params("claude-opus-4-8-fast"));
        assert!(!forbids_sampling_params("claude-opus-4-6"));
        assert!(supports_fast_mode("claude-opus-4-6"));
        assert!(supports_fast_mode("anthropic/claude-opus-4.6"));
        assert!(!supports_fast_mode("claude-opus-4-7"));
        assert!(!supports_fast_mode("claude-opus-4-8-fast"));
        assert!(!supports_fast_mode("claude-sonnet-4-6"));
    }

    #[test]
    fn test_convert_tools() {
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a file",
                "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}
            }
        })];
        let result = convert_tools_to_anthropic(&tools);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "read_file");
    }

    #[test]
    fn test_normalize_response() {
        let blocks = vec![
            serde_json::json!({"type": "text", "text": "Hello!"}),
            serde_json::json!({
                "type": "tool_use",
                "id": "toolu_01",
                "name": "read_file",
                "input": {"path": "/tmp/test.txt"}
            }),
        ];
        let (msg, reason) = normalize_anthropic_response(&blocks, "tool_use", false);
        assert_eq!(msg.content, Some("Hello!".to_string()));
        assert_eq!(msg.tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(reason, "tool_calls");
    }

    #[test]
    fn test_normalize_redacted_thinking_in_reasoning_details() {
        let redacted = serde_json::json!({"type": "redacted_thinking", "data": "opaque"});
        let blocks = vec![
            serde_json::json!({"type": "text", "text": "Hi"}),
            redacted.clone(),
        ];
        let (msg, _) = normalize_anthropic_response(&blocks, "end_turn", false);
        assert_eq!(msg.reasoning, None);
        assert_eq!(msg.reasoning_details.as_ref().unwrap().len(), 1);
        assert_eq!(msg.reasoning_details.unwrap()[0], redacted);
    }

    #[test]
    fn test_default_anthropic_beta_list_oauth_appends() {
        let api = default_anthropic_beta_list(None, false);
        assert!(api
            .iter()
            .any(|b| *b == "fine-grained-tool-streaming-2025-05-14"));

        let oauth = default_anthropic_beta_list(None, true);
        assert!(oauth.iter().any(|b| *b == "oauth-2025-04-20"));
        assert!(oauth.len() > api.len());
    }

    #[test]
    fn test_azure_anthropic_endpoint_detection_is_host_and_path_scoped() {
        assert!(is_azure_anthropic_endpoint(Some(
            "https://example.services.ai.azure.com/models/anthropic"
        )));
        assert!(is_azure_anthropic_endpoint(Some(
            "https://example.services.ai.azure.us/anthropic"
        )));
        assert!(!is_azure_anthropic_endpoint(Some(
            "https://example.openai.azure.com/openai/v1"
        )));
        assert!(!is_azure_anthropic_endpoint(Some(
            "https://management.azure.com/anthropic"
        )));
    }

    #[test]
    fn test_azure_endpoint_keeps_context_1m_and_tool_streaming_betas() {
        let betas =
            common_betas_for_base_url(Some("https://my-resource.openai.azure.com/anthropic"));
        assert!(betas.iter().any(|b| *b == CONTEXT_1M_BETA));
        assert!(betas.iter().any(|b| *b == TOOL_STREAMING_BETA));
    }

    #[test]
    fn test_requires_bearer_auth_covers_minimax_and_azure() {
        assert!(requires_bearer_auth(Some(
            "https://api.minimax.io/anthropic"
        )));
        assert!(requires_bearer_auth(Some(
            "https://my-resource.openai.azure.com/anthropic"
        )));
        assert!(!requires_bearer_auth(Some("https://api.anthropic.com")));
    }

    #[test]
    fn test_fast_mode_beta_list_third_party_none() {
        assert!(fast_mode_request_beta_list(Some("https://example.com/v1"), false).is_none());
        assert!(fast_mode_request_beta_list(None, false).is_some());
    }
}
