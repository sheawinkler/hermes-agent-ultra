//! LLM provider implementations.
//!
//! Provides concrete implementations of the `LlmProvider` trait for
//! OpenAI, Anthropic, and OpenRouter APIs.

use async_trait::async_trait;
use base64::{
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
    Engine as _,
};
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use hermes_intelligence::anthropic_adapter::{
    default_anthropic_beta_header_value, forbids_sampling_params, get_anthropic_max_output,
    is_azure_anthropic_endpoint, is_oauth_token, is_third_party_endpoint, requires_bearer_auth,
    supports_fast_mode,
};
use hermes_intelligence::supports_vision;

use hermes_core::{
    AgentError, FunctionCall, FunctionCallDelta, LlmProvider, LlmResponse, Message, MessageRole,
    StreamChunk, StreamDelta, ToolCall, ToolCallDelta, ToolSchema, UsageStats,
};

use crate::credential_pool::CredentialPool;
use crate::provider_profiles;
use crate::rate_limit::RateLimitTracker;
use crate::tool_call_args::arguments_value_to_string;

struct ChatRequestParams<'a> {
    messages: &'a [Message],
    tools: &'a [ToolSchema],
    max_tokens: Option<u32>,
    temperature: Option<f64>,
    effective_model: &'a str,
    extra_body: Option<&'a Value>,
    stream: bool,
}

const ACP_MULTIMODAL_PREFIX: &str = "__hermes_acp_parts_json__:";
pub const OPENAI_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
pub const OPENAI_CODEX_DYNAMIC_WIRE_MODEL: &str = "gpt-5.4";
const CODEX_CLOUDFLARE_ORIGINATOR: &str = "codex_cli_rs";

fn request_timeout_duration(seconds: Option<f64>) -> Option<Duration> {
    seconds.and_then(|value| {
        if value.is_finite() && value > 0.0 {
            Duration::try_from_secs_f64(value).ok()
        } else {
            None
        }
    })
}

fn build_provider_http_client(request_timeout: Option<Duration>) -> Client {
    let mut builder = Client::builder();
    if let Some(timeout) = request_timeout {
        builder = builder.timeout(timeout);
    }
    builder.build().unwrap_or_else(|err| {
        tracing::warn!("failed to build provider HTTP client: {}", err);
        Client::new()
    })
}

pub fn codex_cloudflare_headers(access_token: Option<&str>) -> Vec<(String, String)> {
    let mut headers = vec![
        (
            "originator".to_string(),
            CODEX_CLOUDFLARE_ORIGINATOR.to_string(),
        ),
        (
            "User-Agent".to_string(),
            format!(
                "{CODEX_CLOUDFLARE_ORIGINATOR}/{}",
                env!("CARGO_PKG_VERSION")
            ),
        ),
    ];

    if let Some(account_id) = access_token.and_then(codex_chatgpt_account_id) {
        headers.push(("ChatGPT-Account-ID".to_string(), account_id));
    }

    headers
}

pub fn openai_codex_provider(
    api_key: impl Into<String>,
    model: impl Into<String>,
    base_url: Option<&str>,
) -> OpenAiProvider {
    openai_codex_provider_with_timeout(api_key, model, base_url, None)
}

pub fn openai_codex_provider_with_timeout(
    api_key: impl Into<String>,
    model: impl Into<String>,
    base_url: Option<&str>,
    request_timeout_seconds: Option<f64>,
) -> OpenAiProvider {
    let api_key = api_key.into();
    let base_url = base_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(OPENAI_CODEX_BASE_URL)
        .to_string();
    let mut provider = OpenAiProvider::new(api_key.as_str())
        .with_model(model)
        .with_base_url(base_url.as_str())
        .with_optional_request_timeout_seconds(request_timeout_seconds);
    if is_codex_cloudflare_base_url(base_url.as_str()) {
        provider = provider.with_headers(codex_cloudflare_headers(Some(api_key.as_str())));
    }
    provider
}

fn is_codex_cloudflare_base_url(base_url: &str) -> bool {
    base_url
        .trim()
        .to_ascii_lowercase()
        .contains("chatgpt.com/backend-api/codex")
}

pub fn codex_chatgpt_account_id(token: &str) -> Option<String> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload.as_bytes())
        .or_else(|_| URL_SAFE.decode(payload.as_bytes()))
        .ok()?;
    let claims: Value = serde_json::from_slice(&decoded).ok()?;
    claims
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub fn is_codex_chatgpt_token(token: &str) -> bool {
    codex_chatgpt_account_id(token).is_some()
}

pub fn is_openai_dynamic_model_alias(model: &str) -> bool {
    matches!(
        model.trim().to_ascii_lowercase().as_str(),
        "dynamic" | "openai:dynamic" | "codex:dynamic" | "openai-codex:dynamic"
    )
}

pub fn resolve_openai_chatgpt_dynamic_wire_model(requested_model: &str, api_key: &str) -> String {
    let requested_model = requested_model.trim();
    if is_openai_dynamic_model_alias(requested_model) && is_codex_chatgpt_token(api_key) {
        OPENAI_CODEX_DYNAMIC_WIRE_MODEL.to_string()
    } else {
        requested_model.to_string()
    }
}

fn is_native_openai_base_url(base_url: &str) -> bool {
    let base_url = base_url.trim().to_ascii_lowercase();
    base_url.contains("api.openai.com") || base_url.contains("chatgpt.com/backend-api/codex")
}

fn resolve_openai_compatible_dynamic_wire_model(
    requested_model: &str,
    api_key: &str,
    base_url: &str,
) -> String {
    let requested_model = requested_model.trim();
    if is_openai_dynamic_model_alias(requested_model)
        && (is_codex_chatgpt_token(api_key) || is_native_openai_base_url(base_url))
    {
        OPENAI_CODEX_DYNAMIC_WIRE_MODEL.to_string()
    } else {
        requested_model.to_string()
    }
}

fn parse_acp_multimodal_parts(content: &str) -> Option<Vec<Value>> {
    let payload = content.trim().strip_prefix(ACP_MULTIMODAL_PREFIX)?;
    let parsed: Value = serde_json::from_str(payload).ok()?;
    let parts = parsed.as_array()?.clone();
    if parts.is_empty() {
        return None;
    }
    if !parts.iter().all(|part| {
        part.as_object()
            .and_then(|obj| obj.get("type"))
            .and_then(|v| v.as_str())
            .is_some()
    }) {
        return None;
    }
    Some(parts)
}

fn flatten_multimodal_parts_text(parts: &[Value]) -> String {
    let mut lines = Vec::new();
    for part in parts {
        let Some(obj) = part.as_object() else {
            continue;
        };
        let kind = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "text" => {
                if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        lines.push(text.to_string());
                    }
                }
            }
            "image_url" | "input_image" => {
                let url = obj
                    .get("image_url")
                    .and_then(|v| v.get("url"))
                    .and_then(|v| v.as_str())
                    .or_else(|| obj.get("image_url").and_then(|v| v.as_str()))
                    .or_else(|| obj.get("url").and_then(|v| v.as_str()))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !url.is_empty() {
                    lines.push(format!("[Attached image]\nURL: {url}"));
                }
            }
            _ => {
                if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        lines.push(text.to_string());
                    }
                }
            }
        }
    }
    lines.join("\n")
}

fn anthropic_blocks_from_multimodal_parts(parts: &[Value]) -> Vec<Value> {
    let mut blocks = Vec::new();
    for part in parts {
        let Some(obj) = part.as_object() else {
            continue;
        };
        let kind = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "text" => {
                if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        blocks.push(serde_json::json!({"type": "text", "text": text}));
                    }
                }
            }
            "image_url" | "input_image" => {
                let url = obj
                    .get("image_url")
                    .and_then(|v| v.get("url"))
                    .and_then(|v| v.as_str())
                    .or_else(|| obj.get("image_url").and_then(|v| v.as_str()))
                    .or_else(|| obj.get("url").and_then(|v| v.as_str()))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !url.is_empty() {
                    let source =
                        hermes_intelligence::anthropic_adapter::image_source_from_openai_url(&url);
                    blocks.push(serde_json::json!({"type": "image", "source": source}));
                }
            }
            _ => {
                if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        blocks.push(serde_json::json!({"type": "text", "text": text}));
                    }
                }
            }
        }
    }
    blocks
}

// ---------------------------------------------------------------------------
// GenericProvider — a flexible, config-driven provider
// ---------------------------------------------------------------------------

include!("provider/generic.rs");
include!("provider/openai.rs");
include!("provider/anthropic.rs");

// ---------------------------------------------------------------------------
// OpenRouterProvider
// ---------------------------------------------------------------------------

include!("provider/openrouter.rs");

#[cfg(test)]
mod tests;
