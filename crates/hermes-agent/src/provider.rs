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
const CODEX_CLOUDFLARE_ORIGINATOR: &str = "codex_cli_rs";

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
    let api_key = api_key.into();
    let base_url = base_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(OPENAI_CODEX_BASE_URL)
        .to_string();
    let mut provider = OpenAiProvider::new(api_key.as_str())
        .with_model(model)
        .with_base_url(base_url.as_str());
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

fn codex_chatgpt_account_id(token: &str) -> Option<String> {
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

/// A generic LLM provider that can be configured for any OpenAI-compatible API.
///
/// This is the primary provider used by the agent loop. It supports
/// OpenAI-compatible APIs via configuration.
#[derive(Debug, Clone)]
pub struct GenericProvider {
    /// Base URL for the API endpoint.
    pub base_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Default model identifier.
    pub model: String,
    /// HTTP client.
    client: Arc<Mutex<Client>>,
    /// Last time we rebuilt the client transport.
    client_refreshed_at: Arc<Mutex<Instant>>,
    /// Optional custom headers to send with every request.
    pub extra_headers: Vec<(String, String)>,
    /// Optional rate limit tracker.
    pub rate_limiter: Option<Arc<RateLimitTracker>>,
    /// Optional credential pool for key rotation.
    pub credential_pool: Option<Arc<CredentialPool>>,
    /// Optional OpenAI-compatible provider profile used for request shaping.
    pub provider_profile: Option<String>,
}

impl GenericProvider {
    /// Create a new generic provider.
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            client: Arc::new(Mutex::new(Client::new())),
            client_refreshed_at: Arc::new(Mutex::new(Instant::now())),
            extra_headers: Vec::new(),
            rate_limiter: None,
            credential_pool: None,
            provider_profile: None,
        }
    }

    /// Add a custom header to be sent with every request.
    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.push((key.into(), value.into()));
        self
    }

    /// Set a custom base URL.
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Set the default model.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Attach a Rust-native provider profile for request shaping.
    pub fn with_provider_profile(mut self, profile: impl Into<String>) -> Self {
        self.provider_profile =
            provider_profiles::canonical_provider_profile_id(&profile.into()).map(str::to_string);
        self
    }

    /// Attach a rate limit tracker.
    pub fn with_rate_limiter(mut self, tracker: Arc<RateLimitTracker>) -> Self {
        self.rate_limiter = Some(tracker);
        self
    }

    /// Attach a credential pool for API key rotation.
    pub fn with_credential_pool(mut self, pool: Arc<CredentialPool>) -> Self {
        self.credential_pool = Some(pool);
        self
    }

    /// Get the effective API key, using the credential pool if available.
    fn effective_api_key(&self) -> String {
        if let Some(ref pool) = self.credential_pool {
            pool.get_key()
        } else {
            self.api_key.clone()
        }
    }

    /// Check rate limits before making a request. Waits if needed.
    async fn check_rate_limit(&self) {
        if let Some(ref tracker) = self.rate_limiter {
            if let Some(wait_duration) = tracker.should_wait() {
                tracing::info!(
                    "Rate limited, waiting {:?} before next request",
                    wait_duration
                );
                tokio::time::sleep(wait_duration).await;
            }
        }
    }

    /// Update rate limit state from response headers.
    fn update_rate_limit(&self, headers: &reqwest::header::HeaderMap) {
        if let Some(ref tracker) = self.rate_limiter {
            tracker.update_from_headers(headers);
        }
    }

    /// Inject optional runtime hints: reasoning effort, vision preprocessing,
    /// and service tier.
    fn apply_runtime_hints(
        &self,
        body: &mut Value,
        messages: &[Message],
        extra_body: Option<&Value>,
    ) {
        // Reasoning effort passthrough (`low|medium|high`) using extra_body.reasoning_effort.
        if let Some(eb) = extra_body
            .and_then(|v| v.get("reasoning_effort"))
            .and_then(|v| v.as_str())
        {
            body["reasoning_effort"] = serde_json::json!(eb);
        }

        // OpenAI service tier passthrough.
        if let Some(st) = extra_body
            .and_then(|v| v.get("service_tier"))
            .and_then(|v| v.as_str())
        {
            body["service_tier"] = serde_json::json!(st);
        }

        // Vision preprocessing: if user content contains local file-like paths,
        // add a hint field used by downstream adapters.
        let needs_vision_preprocess = messages.iter().any(|m| {
            m.content.as_ref().is_some_and(|c| {
                c.contains(".png") || c.contains(".jpg") || c.contains("data:image/")
            })
        });
        if needs_vision_preprocess {
            body["vision_preprocessed"] = serde_json::json!(true);
        }
    }

    fn apply_opencode_go_reasoning_controls(&self, body: &mut Value, effective_model: &str) {
        if !self
            .base_url
            .to_ascii_lowercase()
            .contains("opencode.ai/zen/go")
        {
            return;
        }

        let model = flat_model_name(effective_model);
        let is_kimi_k2 = model.starts_with("kimi-k2");
        let is_deepseek_thinking = (model.starts_with("deepseek-v")
            && !model.starts_with("deepseek-v3"))
            || model == "deepseek-reasoner";

        let reasoning = body.get("reasoning").cloned();
        let mut enabled = true;
        let mut effort = body
            .get("reasoning_effort")
            .and_then(|value| value.as_str())
            .map(|value| value.trim().to_ascii_lowercase());

        if let Some(reasoning_obj) = reasoning.as_ref().and_then(|value| value.as_object()) {
            enabled = reasoning_obj
                .get("enabled")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            if effort.is_none() {
                effort = reasoning_obj
                    .get("effort")
                    .and_then(|value| value.as_str())
                    .map(|value| value.trim().to_ascii_lowercase());
            }
        }

        if let Some(map) = body.as_object_mut() {
            map.remove("reasoning");
            map.remove("reasoning_effort");
        }

        if is_kimi_k2 {
            if reasoning.is_none() && effort.is_none() {
                return;
            }
            body["thinking"] =
                serde_json::json!({ "type": if enabled { "enabled" } else { "disabled" } });
            if enabled {
                if let Some(mapped) = opencode_go_kimi_reasoning_effort(effort.as_deref()) {
                    body["reasoning_effort"] = serde_json::json!(mapped);
                }
            }
            return;
        }

        if is_deepseek_thinking {
            body["thinking"] =
                serde_json::json!({ "type": if enabled { "enabled" } else { "disabled" } });
            if enabled {
                if let Some(mapped) = opencode_go_deepseek_reasoning_effort(effort.as_deref()) {
                    body["reasoning_effort"] = serde_json::json!(mapped);
                }
            }
        }
    }

    /// Force-close helper for future explicit TCP cleanup hooks.
    pub fn force_close_tcp_sockets(&self) {
        // reqwest handles connection pooling internally; dropping clones and relying
        // on idle timeout is currently sufficient for our runtime.
    }

    fn current_client(&self) -> Client {
        self.client
            .lock()
            .map(|c| c.clone())
            .unwrap_or_else(|_| Client::new())
    }

    fn refresh_client(&self, reason: &str) {
        tracing::warn!("rebuilding primary HTTP client: {}", reason);
        if let Ok(mut c) = self.client.lock() {
            *c = Client::new();
        }
        if let Ok(mut t) = self.client_refreshed_at.lock() {
            *t = Instant::now();
        }
    }

    async fn maybe_refresh_stale_client(&self, probe_url: &str) {
        const STALE_CLIENT_REFRESH_SECS: u64 = 300;
        let stale_after = Duration::from_secs(STALE_CLIENT_REFRESH_SECS);
        let should_refresh = self
            .client_refreshed_at
            .lock()
            .map(|t| t.elapsed() >= stale_after)
            .unwrap_or(false);
        if !should_refresh {
            return;
        }
        let probe_client = self.current_client();
        match probe_client
            .get(probe_url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            Ok(_) => {
                if let Ok(mut t) = self.client_refreshed_at.lock() {
                    *t = Instant::now();
                }
            }
            Err(e) => {
                if Self::is_connection_recoverable(&e) {
                    self.refresh_client(&format!("stale connection probe failed: {e}"));
                } else if let Ok(mut t) = self.client_refreshed_at.lock() {
                    *t = Instant::now();
                }
            }
        }
    }

    fn is_connection_recoverable(err: &reqwest::Error) -> bool {
        if err.is_connect() || err.is_timeout() || err.is_request() {
            return true;
        }
        let msg = err.to_string().to_lowercase();
        msg.contains("connection reset")
            || msg.contains("connection closed")
            || msg.contains("broken pipe")
            || msg.contains("pool")
            || msg.contains("eof")
    }

    fn should_sanitize_tool_calls(extra_body: Option<&Value>) -> bool {
        extra_body
            .and_then(|v| {
                v.get("strict_tool_calls")
                    .or_else(|| v.get("strict_api"))
                    .or_else(|| v.get("provider_strict"))
            })
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    fn is_local_request_control_key(key: &str) -> bool {
        matches!(key, "strict_tool_calls" | "strict_api" | "provider_strict")
            || provider_profiles::local_control_key_for_profile(None, key)
    }

    fn merge_extra_body_fields(body: &mut Value, extra_body: Option<&Value>) {
        let Some(Value::Object(map)) = extra_body else {
            return;
        };
        for (k, v) in map {
            if Self::is_local_request_control_key(k) {
                continue;
            }
            body[k] = v.clone();
        }
    }

    fn profile_for_extra_body<'a>(&'a self, extra_body: Option<&'a Value>) -> Option<&'a str> {
        extra_body
            .and_then(|value| value.get("provider_profile"))
            .and_then(Value::as_str)
            .and_then(provider_profiles::canonical_provider_profile_id)
            .or(self.provider_profile.as_deref())
    }

    fn merge_extra_body_fields_for_profile(
        body: &mut Value,
        profile: Option<&str>,
        extra_body: Option<&Value>,
    ) {
        let cleaned = provider_profiles::clean_extra_body_for_profile(profile, extra_body);
        Self::merge_extra_body_fields(body, cleaned.as_ref());
    }

    fn sanitize_messages_for_api(
        messages: &[Message],
        enabled: bool,
        effective_model: &str,
        profile: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Value {
        let model_supports_vision =
            Self::supports_multimodal_tool_results(profile, effective_model, extra_body);
        let mut out = Vec::with_capacity(messages.len());
        for msg in messages {
            let mut api_msg = serde_json::to_value(msg).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(parts) = api_msg
                .get("content")
                .and_then(|v| v.as_str())
                .and_then(parse_acp_multimodal_parts)
            {
                api_msg["content"] = if model_supports_vision {
                    Value::Array(parts)
                } else {
                    Value::String(flatten_multimodal_parts_text(&parts))
                };
            }
            if !enabled {
                out.push(api_msg);
                continue;
            }
            if let Some(tool_calls) = api_msg.get_mut("tool_calls").and_then(|v| v.as_array_mut()) {
                for tc in tool_calls.iter_mut() {
                    if let Some(obj) = tc.as_object_mut() {
                        let id = obj.get("id").cloned();
                        let function = obj.get("function").cloned().or_else(|| {
                            let name = obj.get("name").and_then(|v| v.as_str())?.to_string();
                            let args_raw = obj
                                .get("arguments")
                                .cloned()
                                .unwrap_or_else(|| Value::String("{}".to_string()));
                            let (args, _) = arguments_value_to_string(Some(&args_raw));
                            Some(serde_json::json!({
                                "name": name,
                                "arguments": args,
                            }))
                        });
                        let mut stripped = serde_json::Map::new();
                        if let Some(v) = id {
                            stripped.insert("id".to_string(), v);
                        }
                        stripped.insert(
                            "type".to_string(),
                            obj.get("type")
                                .cloned()
                                .unwrap_or_else(|| Value::String("function".to_string())),
                        );
                        if let Some(v) = function {
                            stripped.insert("function".to_string(), v);
                        }
                        *obj = stripped;
                    }
                }
            }
            out.push(api_msg);
        }
        Value::Array(out)
    }

    fn supports_multimodal_tool_results(
        profile: Option<&str>,
        effective_model: &str,
        extra_body: Option<&Value>,
    ) -> bool {
        if let Some(value) = extra_body
            .and_then(|body| body.get("supports_vision"))
            .and_then(Value::as_bool)
        {
            return value;
        }
        profile.is_some_and(provider_profiles::supports_vision) || supports_vision(effective_model)
    }

    fn format_tools_for_openai_api(tools: &[ToolSchema]) -> Value {
        let formatted = Value::Array(
            tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        }
                    })
                })
                .collect(),
        );
        hermes_core::sanitize_tool_schemas(Some(&formatted)).unwrap_or(formatted)
    }

    fn chat_request_body(&self, request: ChatRequestParams<'_>) -> Value {
        let ChatRequestParams {
            messages,
            tools,
            max_tokens,
            temperature,
            effective_model,
            extra_body,
            stream,
        } = request;
        let profile = self.profile_for_extra_body(extra_body);
        let strict_tool_sanitize = Self::should_sanitize_tool_calls(extra_body);
        let mut api_messages = Self::sanitize_messages_for_api(
            messages,
            strict_tool_sanitize,
            effective_model,
            profile,
            extra_body,
        );
        provider_profiles::normalize_messages_for_profile(profile, &mut api_messages);

        let mut body = serde_json::json!({
            "model": effective_model,
            "messages": api_messages,
        });
        if stream {
            body["stream"] = Value::Bool(true);
        }

        if let Some(mt) =
            max_tokens.or_else(|| profile.and_then(provider_profiles::default_max_tokens))
        {
            body["max_tokens"] = serde_json::json!(mt);
        }
        if !profile.is_some_and(provider_profiles::omit_temperature) {
            if let Some(temp) = temperature {
                body["temperature"] = serde_json::json!(temp);
            }
        }
        if !tools.is_empty() {
            body["tools"] =
                format_tools_for_openai_api_with_model(tools, effective_model, &self.base_url);
        }
        Self::merge_extra_body_fields_for_profile(&mut body, profile, extra_body);
        self.apply_runtime_hints(&mut body, messages, extra_body);
        provider_profiles::apply_profile_to_body(profile, &mut body, effective_model, extra_body);
        self.apply_opencode_go_reasoning_controls(&mut body, effective_model);
        body
    }

    fn build_request(
        &self,
        client: &Client,
        url: &str,
        api_key: &str,
        body: &Value,
    ) -> reqwest::RequestBuilder {
        let mut req = client
            .post(url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(body);
        for (key, value) in &self.extra_headers {
            req = req.header(key.as_str(), value.as_str());
        }
        req
    }

    async fn send_with_dead_connection_recovery(
        &self,
        url: &str,
        api_key: &str,
        body: &Value,
    ) -> Result<reqwest::Response, AgentError> {
        self.maybe_refresh_stale_client(url).await;
        let client = self.current_client();
        match self.build_request(&client, url, api_key, body).send().await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                if !Self::is_connection_recoverable(&e) {
                    return Err(AgentError::LlmApi(format!("HTTP request failed: {e}")));
                }
                self.refresh_client(&format!("recoverable transport error: {e}"));
                let retry_client = self.current_client();
                self.build_request(&retry_client, url, api_key, body)
                    .send()
                    .await
                    .map_err(|e2| {
                        AgentError::LlmApi(format!(
                            "HTTP request failed after reconnect retry: {e2}"
                        ))
                    })
            }
        }
    }
}

fn flat_model_name(model: &str) -> String {
    let normalized = model.trim().to_ascii_lowercase();
    let parts = normalized
        .split(['/', ':'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    for part in parts.iter().rev() {
        if part.starts_with("kimi-k2")
            || part.starts_with("deepseek-v")
            || *part == "deepseek-reasoner"
        {
            return (*part).to_string();
        }
    }

    parts
        .last()
        .copied()
        .unwrap_or(normalized.as_str())
        .to_string()
}

fn opencode_go_kimi_reasoning_effort(effort: Option<&str>) -> Option<&'static str> {
    match effort.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" | "max" => Some("high"),
        _ => None,
    }
}

fn opencode_go_deepseek_reasoning_effort(effort: Option<&str>) -> Option<&'static str> {
    match effort.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" | "max" => Some("max"),
        _ => None,
    }
}

fn is_moonshot_model(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    flat_model_name(&lower).starts_with("kimi-k2") || lower.contains("moonshotai/")
}

fn format_tools_for_openai_api_with_model(
    tools: &[ToolSchema],
    effective_model: &str,
    base_url: &str,
) -> Value {
    let mut formatted = GenericProvider::format_tools_for_openai_api(tools);
    if is_moonshot_model(effective_model)
        || AnthropicProvider::is_kimi_coding_endpoint(Some(base_url))
    {
        sanitize_moonshot_tools_value(&mut formatted);
    }
    formatted
}

fn sanitize_moonshot_tools_value(tools: &mut Value) {
    let Some(items) = tools.as_array_mut() else {
        return;
    };
    for tool in items {
        let Some(function) = tool.get_mut("function").and_then(Value::as_object_mut) else {
            continue;
        };
        let params = function
            .remove("parameters")
            .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
        function.insert(
            "parameters".to_string(),
            sanitize_moonshot_tool_parameters(&params),
        );
    }
}

fn sanitize_moonshot_tool_parameters(params: &Value) -> Value {
    let mut root = match params.as_object() {
        Some(obj) => Value::Object(obj.clone()),
        None => serde_json::json!({"type": "object", "properties": {}}),
    };
    sanitize_moonshot_schema_node(&mut root, true);
    root
}

fn sanitize_moonshot_schema_node(node: &mut Value, top_level: bool) {
    let Some(obj) = node.as_object_mut() else {
        if top_level {
            *node = serde_json::json!({"type": "object", "properties": {}});
        }
        return;
    };

    let has_ref = obj.contains_key("$ref");

    if let Some(any_of) = obj.get_mut("anyOf").and_then(Value::as_array_mut) {
        for branch in any_of.iter_mut() {
            sanitize_moonshot_schema_node(branch, false);
        }
        any_of.retain(|branch| {
            branch
                .get("type")
                .and_then(Value::as_str)
                .is_none_or(|kind| kind != "null")
        });
    }

    if obj.contains_key("anyOf") {
        let non_null = obj
            .get("anyOf")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if non_null.len() == 1 {
            obj.remove("anyOf");
            if let Some(branch_obj) = non_null
                .into_iter()
                .next()
                .and_then(|v| v.as_object().cloned())
            {
                for (key, value) in branch_obj {
                    obj.insert(key, value);
                }
            }
        } else {
            obj.remove("type");
        }
    }

    obj.remove("nullable");

    if let Some(props) = obj.get_mut("properties").and_then(Value::as_object_mut) {
        for value in props.values_mut() {
            sanitize_moonshot_schema_node(value, false);
        }
    }

    if let Some(items) = obj.get_mut("items") {
        sanitize_moonshot_schema_node(items, false);
    }

    if let Some(any_of) = obj.get_mut("anyOf").and_then(Value::as_array_mut) {
        for branch in any_of {
            sanitize_moonshot_schema_node(branch, false);
        }
    }

    clean_moonshot_enum(obj);

    if top_level {
        obj.insert("type".to_string(), Value::String("object".to_string()));
        if !obj.get("properties").is_some_and(Value::is_object) {
            obj.insert(
                "properties".to_string(),
                Value::Object(serde_json::Map::new()),
            );
        }
        return;
    }

    if !has_ref && !obj.contains_key("type") && !obj.contains_key("anyOf") {
        let inferred = infer_moonshot_schema_type(obj);
        obj.insert("type".to_string(), Value::String(inferred.to_string()));
        clean_moonshot_enum(obj);
    }
}

fn clean_moonshot_enum(obj: &mut serde_json::Map<String, Value>) {
    let scalar = matches!(
        obj.get("type").and_then(Value::as_str),
        Some("string" | "integer" | "number" | "boolean")
    );
    if !scalar {
        return;
    }
    let Some(values) = obj.get_mut("enum").and_then(Value::as_array_mut) else {
        return;
    };
    values.retain(|value| !value.is_null() && !matches!(value.as_str(), Some("")));
    if values.is_empty() {
        obj.remove("enum");
    }
}

fn infer_moonshot_schema_type(obj: &serde_json::Map<String, Value>) -> &'static str {
    if obj.get("properties").is_some_and(Value::is_object) {
        return "object";
    }
    if obj.contains_key("items") {
        return "array";
    }
    if let Some(values) = obj.get("enum").and_then(Value::as_array) {
        if let Some(first) = values
            .iter()
            .find(|value| !value.is_null() && !matches!(value.as_str(), Some("")))
        {
            if first.is_boolean() {
                return "boolean";
            }
            if first.is_i64() || first.is_u64() {
                return "integer";
            }
            if first.is_number() {
                return "number";
            }
        }
    }
    "string"
}

#[async_trait]
impl LlmProvider for GenericProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.check_rate_limit().await;

        let effective_model = model.unwrap_or(&self.model);
        let api_key = self.effective_api_key();
        let body = self.chat_request_body(ChatRequestParams {
            messages,
            tools,
            max_tokens,
            temperature,
            effective_model,
            extra_body,
            stream: false,
        });

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let resp = self
            .send_with_dead_connection_recovery(&url, &api_key, &body)
            .await?;

        self.update_rate_limit(resp.headers());

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            return Err(AgentError::LlmApi(format!(
                "API error {status}: {body_text}"
            )));
        }

        let resp_json: Value = resp
            .json()
            .await
            .map_err(|e| AgentError::LlmApi(format!("Failed to parse response: {e}")))?;

        parse_openai_response(&resp_json)
    }

    fn chat_completion_stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        let provider = self.clone();
        let messages = messages.to_vec();
        let tools = tools.to_vec();
        let model = model.map(|s| s.to_string());
        let extra_body = extra_body.cloned();

        async_stream::stream! {
            provider.check_rate_limit().await;

            let effective_model = model.as_deref().unwrap_or(&provider.model);
            let api_key = provider.effective_api_key();
            let mut body = provider.chat_request_body(ChatRequestParams {
                messages: &messages,
                tools: &tools,
                max_tokens,
                temperature,
                effective_model,
                extra_body: extra_body.as_ref(),
                stream: true,
            });
            // Request usage in the final streaming chunk
            body["stream_options"] = serde_json::json!({"include_usage": true});

            let url = format!("{}/chat/completions", provider.base_url.trim_end_matches('/'));

            let resp = match provider
                .send_with_dead_connection_recovery(&url, &api_key, &body)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            provider.update_rate_limit(resp.headers());

            if !resp.status().is_success() {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_else(|_| "<no body>".to_string());
                yield Err(AgentError::LlmApi(format!("API error {status}: {body_text}")));
                return;
            }

            // Read the SSE byte stream line by line
            let mut byte_stream = resp.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk_result) = byte_stream.next().await {
                let chunk_bytes = match chunk_result {
                    Ok(b) => b,
                    Err(e) => {
                        yield Err(AgentError::LlmApi(format!("Stream read error: {e}")));
                        return;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk_bytes));

                // Process complete SSE events (separated by double newlines)
                while let Some(event_end) = buffer.find("\n\n") {
                    let event_block = buffer[..event_end].to_string();
                    buffer = buffer[event_end + 2..].to_string();

                    for line in event_block.lines() {
                        let line = line.trim();
                        if line.is_empty() || line.starts_with(':') {
                            continue;
                        }
                        if let Some(data) = line.strip_prefix("data: ") {
                            let data = data.trim();
                            if data == "[DONE]" {
                                // Stream finished
                                return;
                            }
                            match serde_json::from_str::<Value>(data) {
                                Ok(json) => {
                                    if let Some(chunk) = parse_sse_chunk(&json) {
                                        yield Ok(chunk);
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to parse SSE data: {e}");
                                }
                            }
                        }
                    }
                }
            }

            // Process any remaining data in the buffer
            if !buffer.trim().is_empty() {
                for line in buffer.lines() {
                    let line = line.trim();
                    if let Some(data) = line.strip_prefix("data: ") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            return;
                        }
                        if let Ok(json) = serde_json::from_str::<Value>(data) {
                            if let Some(chunk) = parse_sse_chunk(&json) {
                                yield Ok(chunk);
                            }
                        }
                    }
                }
            }
        }
        .boxed()
    }
}

// ---------------------------------------------------------------------------
// OpenAiProvider
// ---------------------------------------------------------------------------

/// OpenAI API provider.
#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    inner: GenericProvider,
}

impl OpenAiProvider {
    /// Create a new OpenAI provider with the given API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: GenericProvider::new("https://api.openai.com/v1", api_key, "gpt-4o"),
        }
    }

    /// Use a custom base URL (e.g., for Azure OpenAI).
    pub fn with_base_url(self, base_url: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_base_url(base_url),
        }
    }

    /// Set the default model.
    pub fn with_model(self, model: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_model(model),
        }
    }

    /// Add a custom header to every OpenAI-compatible request.
    pub fn with_header(self, key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_header(key, value),
        }
    }

    /// Add several custom headers to every OpenAI-compatible request.
    pub fn with_headers<I, K, V>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (key, value) in headers {
            self = self.with_header(key, value);
        }
        self
    }

    /// Attach a credential pool for API key rotation.
    pub fn with_credential_pool(self, pool: Arc<CredentialPool>) -> Self {
        Self {
            inner: self.inner.with_credential_pool(pool),
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.inner
            .chat_completion(messages, tools, max_tokens, temperature, model, extra_body)
            .await
    }

    fn chat_completion_stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        self.inner.chat_completion_stream(
            messages,
            tools,
            max_tokens,
            temperature,
            model,
            extra_body,
        )
    }
}

// ---------------------------------------------------------------------------
// AnthropicProvider
// ---------------------------------------------------------------------------

/// Anthropic API provider with native Messages API support.
///
/// Uses Anthropic's own message format rather than OpenAI-compatible format:
/// - System message goes in `system` parameter, not in messages array
/// - Uses `x-api-key` header instead of `Authorization: Bearer`
/// - Content blocks use array format with typed blocks
/// - Tool use returns `type: "tool_use"` content blocks
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    /// Base URL for the Anthropic API.
    pub base_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Default model identifier.
    pub model: String,
    /// HTTP client.
    client: Arc<Mutex<Client>>,
    /// Last time we rebuilt the client transport.
    client_refreshed_at: Arc<Mutex<Instant>>,
    /// Anthropic API version header.
    pub api_version: String,
    /// Optional rate limit tracker.
    pub rate_limiter: Option<Arc<RateLimitTracker>>,
    /// Optional credential pool.
    pub credential_pool: Option<Arc<CredentialPool>>,
}

impl AnthropicProvider {
    /// Create a new Anthropic provider with the given API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            base_url: "https://api.anthropic.com".to_string(),
            api_key: api_key.into(),
            model: "claude-3-5-sonnet-20241022".to_string(),
            client: Arc::new(Mutex::new(Client::new())),
            client_refreshed_at: Arc::new(Mutex::new(Instant::now())),
            api_version: "2023-06-01".to_string(),
            rate_limiter: None,
            credential_pool: None,
        }
    }

    /// Set the default model.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Set a custom base URL.
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Attach a rate limit tracker.
    pub fn with_rate_limiter(mut self, tracker: Arc<RateLimitTracker>) -> Self {
        self.rate_limiter = Some(tracker);
        self
    }

    /// Attach a credential pool.
    pub fn with_credential_pool(mut self, pool: Arc<CredentialPool>) -> Self {
        self.credential_pool = Some(pool);
        self
    }

    fn effective_api_key(&self) -> String {
        if let Some(ref pool) = self.credential_pool {
            pool.get_key()
        } else {
            self.api_key.clone()
        }
    }

    async fn check_rate_limit(&self) {
        if let Some(ref tracker) = self.rate_limiter {
            if let Some(wait_duration) = tracker.should_wait() {
                tracing::info!("Rate limited, waiting {:?}", wait_duration);
                tokio::time::sleep(wait_duration).await;
            }
        }
    }

    fn update_rate_limit(&self, headers: &reqwest::header::HeaderMap) {
        if let Some(ref tracker) = self.rate_limiter {
            tracker.update_from_headers(headers);
        }
    }

    fn current_client(&self) -> Client {
        self.client
            .lock()
            .map(|c| c.clone())
            .unwrap_or_else(|_| Client::new())
    }

    fn refresh_client(&self, reason: &str) {
        tracing::warn!("rebuilding anthropic HTTP client: {}", reason);
        if let Ok(mut c) = self.client.lock() {
            *c = Client::new();
        }
        if let Ok(mut t) = self.client_refreshed_at.lock() {
            *t = Instant::now();
        }
    }

    async fn maybe_refresh_stale_client(&self, probe_url: &str) {
        const STALE_CLIENT_REFRESH_SECS: u64 = 300;
        let stale_after = Duration::from_secs(STALE_CLIENT_REFRESH_SECS);
        let should_refresh = self
            .client_refreshed_at
            .lock()
            .map(|t| t.elapsed() >= stale_after)
            .unwrap_or(false);
        if !should_refresh {
            return;
        }
        let probe_client = self.current_client();
        match probe_client
            .get(probe_url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            Ok(_) => {
                if let Ok(mut t) = self.client_refreshed_at.lock() {
                    *t = Instant::now();
                }
            }
            Err(e) => {
                if GenericProvider::is_connection_recoverable(&e) {
                    self.refresh_client(&format!("stale connection probe failed: {e}"));
                } else if let Ok(mut t) = self.client_refreshed_at.lock() {
                    *t = Instant::now();
                }
            }
        }
    }

    fn build_request(
        &self,
        client: &Client,
        url: &str,
        api_key: &str,
        body: &Value,
    ) -> reqwest::RequestBuilder {
        let base_url = self.base_url.as_str();
        let native_oauth = !is_third_party_endpoint(Some(base_url)) && is_oauth_token(api_key);
        let bearer_auth = native_oauth || requires_bearer_auth(Some(base_url));
        let beta_header = default_anthropic_beta_header_value(Some(base_url), native_oauth);

        let mut request = client
            .post(url)
            .header("anthropic-version", &self.api_version)
            .header("Content-Type", "application/json")
            .header("anthropic-beta", beta_header)
            .json(body);

        if bearer_auth {
            request = request.header("Authorization", format!("Bearer {api_key}"));
        } else {
            request = request.header("x-api-key", api_key);
        }
        if native_oauth {
            request = request
                .header("user-agent", "claude-cli/2.1.74 (external, cli)")
                .header("x-app", "cli");
        }
        request
    }

    fn messages_url(&self) -> String {
        Self::messages_url_for_base_url(&self.base_url)
    }

    fn messages_url_for_base_url(base_url: &str) -> String {
        let trimmed = base_url.trim().trim_end_matches('/');
        if let Ok(mut url) = reqwest::Url::parse(trimmed) {
            let mut path = url.path().trim_end_matches('/').to_string();
            if path.is_empty() {
                path.push_str("/v1/messages");
            } else if path.ends_with("/v1") {
                path.push_str("/messages");
            } else {
                path.push_str("/v1/messages");
            }
            url.set_path(&path);
            if is_azure_anthropic_endpoint(Some(trimmed))
                && !url
                    .query_pairs()
                    .any(|(key, _)| key.eq_ignore_ascii_case("api-version"))
            {
                url.query_pairs_mut()
                    .append_pair("api-version", "2025-04-15");
            }
            return url.to_string();
        }

        let mut url = format!("{trimmed}/v1/messages");
        if is_azure_anthropic_endpoint(Some(trimmed)) && !url.contains("api-version=") {
            let sep = if url.contains('?') { '&' } else { '?' };
            url.push(sep);
            url.push_str("api-version=2025-04-15");
        }
        url
    }

    fn strip_unsupported_anthropic_controls(body: &mut Value, model: &str) {
        let Some(obj) = body.as_object_mut() else {
            return;
        };
        if forbids_sampling_params(model) {
            obj.remove("temperature");
            obj.remove("top_p");
            obj.remove("top_k");
        }
        if obj.get("speed").and_then(Value::as_str) == Some("fast") && !supports_fast_mode(model) {
            obj.remove("speed");
        }
    }

    async fn send_with_dead_connection_recovery(
        &self,
        url: &str,
        api_key: &str,
        body: &Value,
    ) -> Result<reqwest::Response, AgentError> {
        self.maybe_refresh_stale_client(url).await;
        let client = self.current_client();
        match self.build_request(&client, url, api_key, body).send().await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                if !GenericProvider::is_connection_recoverable(&e) {
                    return Err(AgentError::LlmApi(format!("HTTP request failed: {e}")));
                }
                self.refresh_client(&format!("recoverable transport error: {e}"));
                let retry_client = self.current_client();
                self.build_request(&retry_client, url, api_key, body)
                    .send()
                    .await
                    .map_err(|e2| {
                        AgentError::LlmApi(format!(
                            "HTTP request failed after reconnect retry: {e2}"
                        ))
                    })
            }
        }
    }

    fn is_kimi_coding_endpoint(base_url: Option<&str>) -> bool {
        let Some(url) = base_url else {
            return false;
        };
        let lower = url.to_lowercase();
        lower.contains("api.kimi.com")
            || lower.contains("moonshot.ai")
            || lower.contains("moonshot.cn")
    }

    /// Resolve Anthropic Messages `max_tokens` to a strictly positive value.
    ///
    /// `Some(0)` is treated as invalid and falls back to the model ceiling,
    /// preventing avoidable 400s from upstream APIs.
    fn resolve_messages_max_tokens(requested: Option<u32>, model: &str) -> u32 {
        if let Some(value) = requested.filter(|v| *v > 0) {
            return value;
        }
        get_anthropic_max_output(model).max(1)
    }

    /// Convert internal messages to Anthropic format, extracting system message.
    fn convert_messages(
        messages: &[Message],
        base_url: Option<&str>,
    ) -> (Option<String>, Vec<Value>) {
        let mut system_text: Option<String> = None;
        let mut anthropic_messages: Vec<Value> = Vec::new();
        let is_kimi_endpoint = Self::is_kimi_coding_endpoint(base_url);

        for msg in messages {
            match msg.role {
                MessageRole::System => {
                    // Anthropic: system goes in a separate `system` parameter
                    let content = msg.content.as_deref().unwrap_or("");
                    system_text = Some(match system_text {
                        Some(existing) => format!("{existing}\n\n{content}"),
                        None => content.to_string(),
                    });
                }
                MessageRole::User => {
                    let mut content_blocks = Vec::new();
                    if let Some(ref text) = msg.content {
                        if let Some(parts) = parse_acp_multimodal_parts(text) {
                            content_blocks.extend(anthropic_blocks_from_multimodal_parts(&parts));
                            if content_blocks.is_empty() {
                                let fallback = flatten_multimodal_parts_text(&parts);
                                if !fallback.is_empty() {
                                    content_blocks.push(
                                        serde_json::json!({"type": "text", "text": fallback}),
                                    );
                                }
                            }
                        } else {
                            let mut block = serde_json::json!({"type": "text", "text": text});
                            if let Some(ref cc) = msg.cache_control {
                                block["cache_control"] = serde_json::json!({"type": format!("{:?}", cc.cache_type).to_lowercase()});
                            }
                            content_blocks.push(block);
                        }
                    }
                    anthropic_messages.push(serde_json::json!({
                        "role": "user",
                        "content": content_blocks,
                    }));
                }
                MessageRole::Assistant => {
                    let mut content_blocks = Vec::new();
                    if is_kimi_endpoint
                        && msg
                            .tool_calls
                            .as_ref()
                            .is_some_and(|calls| !calls.is_empty())
                        && msg.reasoning_content.is_some()
                    {
                        let thinking = msg.reasoning_content.as_deref().unwrap_or("");
                        // Kimi /coding expects assistant tool-call replay messages
                        // to include reasoning_content semantics; preserve it as
                        // a thinking block before any text/tool_use blocks.
                        content_blocks
                            .push(serde_json::json!({"type": "thinking", "thinking": thinking}));
                    }
                    if let Some(ref text) = msg.content {
                        if !text.is_empty() {
                            content_blocks.push(serde_json::json!({"type": "text", "text": text}));
                        }
                    }
                    // Convert tool_calls to Anthropic tool_use blocks
                    if let Some(ref tool_calls) = msg.tool_calls {
                        for tc in tool_calls {
                            let input: Value = serde_json::from_str(&tc.function.arguments)
                                .unwrap_or(serde_json::json!({}));
                            content_blocks.push(serde_json::json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.function.name,
                                "input": input,
                            }));
                        }
                    }
                    if !content_blocks.is_empty() {
                        anthropic_messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": content_blocks,
                        }));
                    }
                }
                MessageRole::Tool => {
                    // Anthropic: tool results go as user messages with tool_result content blocks
                    let content_blocks = vec![serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": msg.tool_call_id.as_deref().unwrap_or(""),
                        "content": msg.content.as_deref().unwrap_or(""),
                    })];
                    anthropic_messages.push(serde_json::json!({
                        "role": "user",
                        "content": content_blocks,
                    }));
                }
            }
        }

        (system_text, anthropic_messages)
    }

    /// Convert tool schemas to Anthropic tool format.
    fn convert_tools(tools: &[ToolSchema]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect()
    }

    /// Parse an Anthropic Messages API response into LlmResponse.
    fn parse_response(json: &Value) -> Result<LlmResponse, AgentError> {
        let mut content_text = String::new();
        let mut reasoning_content = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        if let Some(content_arr) = json.get("content").and_then(|c| c.as_array()) {
            for block in content_arr {
                let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match block_type {
                    "text" => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            if !content_text.is_empty() {
                                content_text.push('\n');
                            }
                            content_text.push_str(text);
                        }
                    }
                    "tool_use" => {
                        let id = block
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let input = block.get("input").cloned().unwrap_or(serde_json::json!({}));
                        let arguments =
                            serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                        tool_calls.push(ToolCall {
                            id,
                            function: FunctionCall { name, arguments },
                            extra_content: None,
                        });
                    }
                    "thinking" => {
                        if let Some(thinking) = block.get("thinking").and_then(|t| t.as_str()) {
                            if !reasoning_content.is_empty() {
                                reasoning_content.push('\n');
                            }
                            reasoning_content.push_str(thinking);
                        }
                    }
                    _ => {}
                }
            }
        }

        let usage = json.get("usage").and_then(|u| {
            let input = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let output = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            Some(UsageStats {
                prompt_tokens: input,
                completion_tokens: output,
                total_tokens: input + output,
                estimated_cost: None,
            })
        });

        let stop_reason = json
            .get("stop_reason")
            .and_then(|s| s.as_str())
            .map(|s| match s {
                "end_turn" => "stop".to_string(),
                "tool_use" => "tool_calls".to_string(),
                other => other.to_string(),
            });

        let model = json
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
            .to_string();

        let message = Message {
            role: MessageRole::Assistant,
            content: Some(content_text),
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
            name: None,
            reasoning_content: if reasoning_content.is_empty() {
                None
            } else {
                Some(reasoning_content)
            },
            cache_control: None,
        };

        Ok(LlmResponse {
            message,
            usage,
            model,
            finish_reason: stop_reason,
        })
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.check_rate_limit().await;

        let effective_model = model.unwrap_or(&self.model);
        let api_key = self.effective_api_key();
        let (system_text, anthropic_messages) =
            Self::convert_messages(messages, Some(self.base_url.as_str()));
        let resolved_max_tokens = Self::resolve_messages_max_tokens(max_tokens, effective_model);

        let mut body = serde_json::json!({
            "model": effective_model,
            "messages": anthropic_messages,
            "max_tokens": resolved_max_tokens,
        });

        if let Some(ref sys) = system_text {
            body["system"] = serde_json::json!(sys);
        }
        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(Self::convert_tools(tools));
        }
        GenericProvider::merge_extra_body_fields(&mut body, extra_body);
        Self::strip_unsupported_anthropic_controls(&mut body, effective_model);

        let url = self.messages_url();
        let resp = self
            .send_with_dead_connection_recovery(&url, &api_key, &body)
            .await?;

        self.update_rate_limit(resp.headers());

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            return Err(AgentError::LlmApi(format!(
                "API error {status}: {body_text}"
            )));
        }

        let resp_json: Value = resp
            .json()
            .await
            .map_err(|e| AgentError::LlmApi(format!("Failed to parse response: {e}")))?;

        Self::parse_response(&resp_json)
    }

    fn chat_completion_stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        let provider = self.clone();
        let messages = messages.to_vec();
        let tools = tools.to_vec();
        let model = model.map(|s| s.to_string());
        let extra_body = extra_body.cloned();

        async_stream::stream! {
            provider.check_rate_limit().await;

            let effective_model = model.as_deref().unwrap_or(&provider.model);
            let api_key = provider.effective_api_key();
            let (system_text, anthropic_messages) = AnthropicProvider::convert_messages(
                &messages,
                Some(provider.base_url.as_str()),
            );
            let resolved_max_tokens =
                AnthropicProvider::resolve_messages_max_tokens(max_tokens, effective_model);

            let mut body = serde_json::json!({
                "model": effective_model,
                "messages": anthropic_messages,
                "max_tokens": resolved_max_tokens,
                "stream": true,
            });

            if let Some(ref sys) = system_text {
                body["system"] = serde_json::json!(sys);
            }
            if let Some(temp) = temperature {
                body["temperature"] = serde_json::json!(temp);
            }
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(AnthropicProvider::convert_tools(&tools));
            }
            GenericProvider::merge_extra_body_fields(&mut body, extra_body.as_ref());
            AnthropicProvider::strip_unsupported_anthropic_controls(&mut body, effective_model);

            let url = provider.messages_url();

            let resp = match provider.send_with_dead_connection_recovery(&url, &api_key, &body).await {
                Ok(r) => r,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            provider.update_rate_limit(resp.headers());

            if !resp.status().is_success() {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_else(|_| "<no body>".to_string());
                yield Err(AgentError::LlmApi(format!("API error {status}: {body_text}")));
                return;
            }

            let mut byte_stream = resp.bytes_stream();
            let mut buffer = String::new();
            // Track current tool_use block index for delta accumulation
            let mut current_tool_index: u32 = 0;

            while let Some(chunk_result) = byte_stream.next().await {
                let chunk_bytes = match chunk_result {
                    Ok(b) => b,
                    Err(e) => {
                        yield Err(AgentError::LlmApi(format!("Stream read error: {e}")));
                        return;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk_bytes));

                while let Some(event_end) = buffer.find("\n\n") {
                    let event_block = buffer[..event_end].to_string();
                    buffer = buffer[event_end + 2..].to_string();

                    let mut event_type = String::new();
                    let mut event_data = String::new();

                    for line in event_block.lines() {
                        let line = line.trim();
                        if let Some(et) = line.strip_prefix("event: ") {
                            event_type = et.trim().to_string();
                        } else if let Some(d) = line.strip_prefix("data: ") {
                            event_data = d.trim().to_string();
                        }
                    }

                    if event_data.is_empty() {
                        continue;
                    }

                    let json: Value = match serde_json::from_str(&event_data) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    match event_type.as_str() {
                        "content_block_start" => {
                            let block = json.get("content_block").unwrap_or(&json);
                            let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            if block_type == "tool_use" {
                                let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                                let idx = json.get("index").and_then(|i| i.as_u64()).unwrap_or(current_tool_index as u64) as u32;
                                current_tool_index = idx;
                                yield Ok(StreamChunk {
                                    delta: Some(StreamDelta {
                                        content: None,
                                        tool_calls: Some(vec![ToolCallDelta {
                                            index: idx,
                                            id: Some(id),
                                            function: Some(FunctionCallDelta {
                                                name: Some(name),
                                                arguments: None,
                                            }),
                                        }]),
                                        extra: None,
                                    }),
                                    finish_reason: None,
                                    usage: None,
                                });
                            }
                        }
                        "content_block_delta" => {
                            let delta = json.get("delta").unwrap_or(&json);
                            let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            match delta_type {
                                "text_delta" => {
                                    let text = delta.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                                    yield Ok(StreamChunk {
                                        delta: Some(StreamDelta {
                                            content: Some(text),
                                            tool_calls: None,
                                            extra: None,
                                        }),
                                        finish_reason: None,
                                        usage: None,
                                    });
                                }
                                "input_json_delta" => {
                                    let partial = delta.get("partial_json").and_then(|p| p.as_str()).unwrap_or("").to_string();
                                    yield Ok(StreamChunk {
                                        delta: Some(StreamDelta {
                                            content: None,
                                            tool_calls: Some(vec![ToolCallDelta {
                                                index: current_tool_index,
                                                id: None,
                                                function: Some(FunctionCallDelta {
                                                    name: None,
                                                    arguments: Some(partial),
                                                }),
                                            }]),
                                            extra: None,
                                        }),
                                        finish_reason: None,
                                        usage: None,
                                    });
                                }
                                "thinking_delta" => {
                                    if let Some(thinking) =
                                        delta.get("thinking").and_then(|t| t.as_str())
                                    {
                                        yield Ok(StreamChunk {
                                            delta: Some(StreamDelta {
                                                content: None,
                                                tool_calls: None,
                                                extra: Some(
                                                    serde_json::json!({"thinking": thinking}),
                                                ),
                                            }),
                                            finish_reason: None,
                                            usage: None,
                                        });
                                    }
                                }
                                _ => {}
                            }
                        }
                        "message_delta" => {
                            let stop_reason = json
                                .get("delta")
                                .and_then(|d| d.get("stop_reason"))
                                .and_then(|s| s.as_str())
                                .map(|s| match s {
                                    "end_turn" => "stop".to_string(),
                                    "tool_use" => "tool_calls".to_string(),
                                    other => other.to_string(),
                                });
                            let usage = json.get("usage").and_then(|u| {
                                let output = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                Some(UsageStats {
                                    prompt_tokens: 0,
                                    completion_tokens: output,
                                    total_tokens: output,
                                    estimated_cost: None,
                                })
                            });
                            yield Ok(StreamChunk {
                                delta: None,
                                finish_reason: stop_reason,
                                usage,
                            });
                        }
                        "message_start" => {
                            // Extract usage from the initial message
                            let usage = json.get("message").and_then(|m| m.get("usage")).and_then(|u| {
                                let input = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                Some(UsageStats {
                                    prompt_tokens: input,
                                    completion_tokens: 0,
                                    total_tokens: input,
                                    estimated_cost: None,
                                })
                            });
                            if let Some(u) = usage {
                                yield Ok(StreamChunk {
                                    delta: None,
                                    finish_reason: None,
                                    usage: Some(u),
                                });
                            }
                        }
                        "message_stop" => {
                            return;
                        }
                        _ => {}
                    }
                }
            }
        }
        .boxed()
    }
}

// ---------------------------------------------------------------------------
// OpenRouterProvider
// ---------------------------------------------------------------------------

/// OpenRouter API provider with support for OpenRouter-specific parameters.
///
/// Adds:
/// - `HTTP-Referer` and `X-Title` headers (required by OpenRouter)
/// - Support for `transforms`, `provider` preferences, `route` in extra_body
/// - Parsing of `reasoning_details` array from responses
/// - `reasoning_content` extraction
#[derive(Debug, Clone)]
pub struct OpenRouterProvider {
    inner: GenericProvider,
    /// HTTP-Referer header value (required by OpenRouter).
    pub http_referer: Option<String>,
    /// X-Title header value (required by OpenRouter).
    pub x_title: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct OpenRouterResponseCacheControl {
    enabled: bool,
    clear: bool,
    ttl_secs: u64,
}

#[derive(Debug, Clone)]
struct OpenRouterResponseCacheEntry {
    response: LlmResponse,
    expires_at: Instant,
}

#[derive(Debug, Default)]
struct OpenRouterResponseCache {
    entries: HashMap<String, OpenRouterResponseCacheEntry>,
    order: VecDeque<String>,
}

static OPENROUTER_RESPONSE_CACHE: OnceLock<Mutex<OpenRouterResponseCache>> = OnceLock::new();

impl OpenRouterProvider {
    /// Create a new OpenRouter provider with the given API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: GenericProvider::new("https://openrouter.ai/api/v1", api_key, "openai/gpt-4o")
                .with_provider_profile("openrouter"),
            http_referer: None,
            x_title: None,
        }
    }

    /// Set the default model.
    pub fn with_model(self, model: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_model(model),
            ..self
        }
    }

    /// Set the HTTP-Referer header (required by OpenRouter).
    pub fn with_http_referer(mut self, referer: impl Into<String>) -> Self {
        self.http_referer = Some(referer.into());
        self
    }

    /// Set the X-Title header (required by OpenRouter).
    pub fn with_x_title(mut self, title: impl Into<String>) -> Self {
        self.x_title = Some(title.into());
        self
    }

    /// Attach a credential pool for API key rotation.
    pub fn with_credential_pool(self, pool: Arc<CredentialPool>) -> Self {
        Self {
            inner: self.inner.with_credential_pool(pool),
            ..self
        }
    }

    /// Build the extra headers including OpenRouter-specific ones.
    fn build_headers(&self) -> Vec<(String, String)> {
        let mut headers = self.inner.extra_headers.clone();
        if let Some(ref referer) = self.http_referer {
            headers.push(("HTTP-Referer".to_string(), referer.clone()));
        }
        if let Some(ref title) = self.x_title {
            headers.push(("X-Title".to_string(), title.clone()));
        }
        headers
    }

    fn openrouter_response_cache_enabled() -> bool {
        std::env::var("HERMES_OPENROUTER_RESPONSE_CACHE")
            .ok()
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on" | "enabled"
                )
            })
            .unwrap_or(false)
    }

    fn openrouter_response_cache_ttl_secs() -> u64 {
        std::env::var("HERMES_OPENROUTER_RESPONSE_CACHE_TTL_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(300)
    }

    fn openrouter_response_cache_max_entries() -> usize {
        std::env::var("HERMES_OPENROUTER_RESPONSE_CACHE_MAX_ENTRIES")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(256)
    }

    fn parse_response_cache_control(extra_body: Option<&Value>) -> OpenRouterResponseCacheControl {
        let mut enabled = Self::openrouter_response_cache_enabled();
        let mut clear = false;
        let mut ttl_secs = Self::openrouter_response_cache_ttl_secs();

        if let Some(Value::Object(map)) = extra_body {
            if let Some(v) = map.get("response_cache_enabled").and_then(Value::as_bool) {
                enabled = v;
            }
            if let Some(v) = map.get("response_cache_clear").and_then(Value::as_bool) {
                clear = v;
            }
            if let Some(v) = map
                .get("response_cache_ttl_secs")
                .and_then(Value::as_u64)
                .filter(|v| *v > 0)
            {
                ttl_secs = v;
            }
            if let Some(Value::Bool(flag)) = map.get("response_cache") {
                enabled = *flag;
            }
            if let Some(Value::Object(cache_cfg)) = map.get("response_cache") {
                if let Some(v) = cache_cfg.get("enabled").and_then(Value::as_bool) {
                    enabled = v;
                }
                if let Some(v) = cache_cfg.get("clear").and_then(Value::as_bool) {
                    clear = v;
                }
                if let Some(v) = cache_cfg
                    .get("ttl_secs")
                    .and_then(Value::as_u64)
                    .filter(|v| *v > 0)
                {
                    ttl_secs = v;
                }
            }
        }

        OpenRouterResponseCacheControl {
            enabled,
            clear,
            ttl_secs,
        }
    }

    /// Merge OpenRouter-specific parameters into extra_body.
    fn merge_extra_body(extra_body: Option<&Value>) -> Option<Value> {
        let Some(Value::Object(map)) = extra_body else {
            return extra_body.cloned();
        };
        let mut cleaned = map.clone();
        cleaned.remove("response_cache");
        cleaned.remove("response_cache_enabled");
        cleaned.remove("response_cache_ttl_secs");
        cleaned.remove("response_cache_clear");
        cleaned.remove("strict_tool_calls");
        cleaned.remove("strict_api");
        cleaned.remove("provider_strict");
        if !cleaned.contains_key("reasoning") {
            if let Some(effort) = cleaned.remove("reasoning_effort") {
                cleaned.insert(
                    "reasoning".to_string(),
                    serde_json::json!({ "effort": effort }),
                );
            }
        } else {
            cleaned.remove("reasoning_effort");
        }
        Some(Value::Object(cleaned))
    }

    fn response_cache_key(model: &str, body: &Value) -> Option<String> {
        let encoded = serde_json::to_vec(body).ok()?;
        let mut hasher = Sha256::new();
        hasher.update(model.as_bytes());
        hasher.update(b"\n");
        hasher.update(encoded);
        Some(format!("{:x}", hasher.finalize()))
    }

    fn response_cache_get(key: &str) -> Option<LlmResponse> {
        let cache = OPENROUTER_RESPONSE_CACHE
            .get_or_init(|| Mutex::new(OpenRouterResponseCache::default()));
        let mut guard = cache.lock().expect("openrouter cache lock poisoned");
        let now = Instant::now();
        if let Some(entry) = guard.entries.get(key) {
            if now < entry.expires_at {
                return Some(entry.response.clone());
            }
        }
        guard.entries.remove(key);
        guard.order.retain(|k| k != key);
        None
    }

    fn response_cache_insert(key: String, response: &LlmResponse, ttl_secs: u64) {
        let cache = OPENROUTER_RESPONSE_CACHE
            .get_or_init(|| Mutex::new(OpenRouterResponseCache::default()));
        let mut guard = cache.lock().expect("openrouter cache lock poisoned");
        let now = Instant::now();
        guard.entries.insert(
            key.clone(),
            OpenRouterResponseCacheEntry {
                response: response.clone(),
                expires_at: now + Duration::from_secs(ttl_secs.max(1)),
            },
        );
        guard.order.retain(|k| k != &key);
        guard.order.push_back(key);
        while guard.entries.len() > Self::openrouter_response_cache_max_entries() {
            if let Some(evict) = guard.order.pop_front() {
                guard.entries.remove(&evict);
            } else {
                break;
            }
        }
    }

    /// Parse an OpenRouter response, extracting reasoning_details if present.
    fn parse_openrouter_response(json: &Value) -> Result<LlmResponse, AgentError> {
        let mut response = parse_openai_response(json)?;

        // Extract reasoning_content from various locations
        if let Some(reasoning) = crate::reasoning::parse_reasoning(json) {
            response.message.reasoning_content = Some(reasoning);
        }

        Ok(response)
    }
}

#[async_trait]
impl LlmProvider for OpenRouterProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        // Build a provider clone with OpenRouter headers
        let mut provider = self.inner.clone();
        provider.extra_headers = self.build_headers();
        let effective_model = model.unwrap_or(&self.inner.model);
        provider
            .extra_headers
            .extend(provider_profiles::extra_headers_for_profile(
                Some("openrouter"),
                effective_model,
                extra_body,
            ));
        let cache_control = Self::parse_response_cache_control(extra_body);
        if cache_control.enabled {
            provider
                .extra_headers
                .push(("X-OpenRouter-Cache".to_string(), "true".to_string()));
            if cache_control.clear {
                provider
                    .extra_headers
                    .push(("X-OpenRouter-Cache-Clear".to_string(), "true".to_string()));
            }
        }

        let merged_extra = Self::merge_extra_body(extra_body);

        // Use GenericProvider for the actual request
        provider.check_rate_limit().await;

        let api_key = provider.effective_api_key();
        let body = provider.chat_request_body(ChatRequestParams {
            messages,
            tools,
            max_tokens,
            temperature,
            effective_model,
            extra_body: merged_extra.as_ref(),
            stream: false,
        });

        let cache_key = if cache_control.enabled && !cache_control.clear {
            Self::response_cache_key(effective_model, &body)
        } else {
            None
        };
        if let Some(ref key) = cache_key {
            if let Some(hit) = Self::response_cache_get(key) {
                return Ok(hit);
            }
        }

        let url = format!(
            "{}/chat/completions",
            provider.base_url.trim_end_matches('/')
        );

        let resp = provider
            .send_with_dead_connection_recovery(&url, &api_key, &body)
            .await?;

        provider.update_rate_limit(resp.headers());

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            return Err(AgentError::LlmApi(format!(
                "API error {status}: {body_text}"
            )));
        }

        let resp_json: Value = resp
            .json()
            .await
            .map_err(|e| AgentError::LlmApi(format!("Failed to parse response: {e}")))?;
        let parsed = Self::parse_openrouter_response(&resp_json)?;
        if let Some(key) = cache_key {
            Self::response_cache_insert(key, &parsed, cache_control.ttl_secs);
        }
        Ok(parsed)
    }

    fn chat_completion_stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        // Use GenericProvider's streaming with OpenRouter headers
        let mut provider = self.inner.clone();
        provider.extra_headers = self.build_headers();
        let effective_model = model.unwrap_or(&self.inner.model);
        provider
            .extra_headers
            .extend(provider_profiles::extra_headers_for_profile(
                Some("openrouter"),
                effective_model,
                extra_body,
            ));
        let merged_extra = Self::merge_extra_body(extra_body);

        provider.chat_completion_stream(
            messages,
            tools,
            max_tokens,
            temperature,
            model,
            merged_extra.as_ref(),
        )
    }
}

// ---------------------------------------------------------------------------
// SSE chunk parsing helpers
// ---------------------------------------------------------------------------

/// Parse a single SSE data JSON object into a StreamChunk (OpenAI format).
fn parse_sse_chunk(json: &Value) -> Option<StreamChunk> {
    let choices = json.get("choices").and_then(|c| c.as_array())?;
    let choice = choices.first()?;

    let delta_obj = choice.get("delta")?;

    let content = delta_obj
        .get("content")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());

    let tool_calls = delta_obj
        .get("tool_calls")
        .and_then(|tc| tc.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let index = tc.get("index").and_then(|i| i.as_u64())? as u32;
                    let id = tc.get("id").and_then(|i| i.as_str()).map(|s| s.to_string());
                    let function = tc.get("function").map(|f| FunctionCallDelta {
                        name: f
                            .get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string()),
                        arguments: f
                            .get("arguments")
                            .and_then(|a| a.as_str())
                            .map(|s| s.to_string()),
                    });
                    Some(ToolCallDelta {
                        index,
                        id,
                        function,
                    })
                })
                .collect::<Vec<_>>()
        });

    let delta = if content.is_some() || tool_calls.is_some() {
        Some(StreamDelta {
            content,
            tool_calls,
            extra: None,
        })
    } else {
        None
    };

    let finish_reason = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .map(|s| s.to_string());

    // Usage may appear in the final chunk
    let usage = json.get("usage").and_then(|u| {
        Some(UsageStats {
            prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            completion_tokens: u
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            total_tokens: u.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            estimated_cost: None,
        })
    });

    Some(StreamChunk {
        delta,
        finish_reason,
        usage,
    })
}

// ---------------------------------------------------------------------------
// Response parsing helpers
// ---------------------------------------------------------------------------

/// Parse an OpenAI-style chat completion response.
fn parse_openai_response(json: &Value) -> Result<LlmResponse, AgentError> {
    let choices = json
        .get("choices")
        .and_then(|c| c.as_array())
        .ok_or_else(|| {
            AgentError::LlmApi(format!(
                "No choices in response ({})",
                summarize_openai_response_shape(json)
            ))
        })?;

    let choice = choices.first().ok_or_else(|| {
        AgentError::LlmApi(format!(
            "Empty choices array ({})",
            summarize_openai_response_shape(json)
        ))
    })?;

    let message_obj = choice
        .get("message")
        .ok_or_else(|| AgentError::LlmApi("No message in choice".to_string()))?;

    // Parse content
    let content = message_obj
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    // Parse tool calls
    let tool_calls = message_obj
        .get("tool_calls")
        .and_then(|tc| tc.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let id = tc.get("id")?.as_str()?.to_string();
                    let function = tc.get("function")?;
                    let name = function.get("name")?.as_str()?.to_string();
                    let (arguments, _) = arguments_value_to_string(function.get("arguments"));
                    let extra_content = tc.get("extra_content").filter(|v| !v.is_null()).cloned();

                    Some(hermes_core::ToolCall {
                        id,
                        function: hermes_core::FunctionCall { name, arguments },
                        extra_content,
                    })
                })
                .collect::<Vec<_>>()
        });

    // Parse usage
    let usage = json.get("usage").and_then(|u| {
        Some(UsageStats {
            prompt_tokens: u.get("prompt_tokens")?.as_u64()? as u64,
            completion_tokens: u.get("completion_tokens")?.as_u64()? as u64,
            total_tokens: u.get("total_tokens")?.as_u64()? as u64,
            estimated_cost: None,
        })
    });

    let role = message_obj
        .get("role")
        .and_then(|r| r.as_str())
        .unwrap_or("assistant");

    // Extract reasoning content
    let reasoning_content = message_obj
        .get("reasoning_content")
        .and_then(|r| r.as_str())
        .map(|s| s.to_string());

    let message = Message {
        role: match role {
            "user" => MessageRole::User,
            "system" => MessageRole::System,
            "tool" => MessageRole::Tool,
            _ => MessageRole::Assistant,
        },
        content: Some(content),
        tool_calls,
        tool_call_id: None,
        name: None,
        reasoning_content,
        cache_control: None,
    };

    let finish_reason = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .map(|s| s.to_string());

    let model = json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();

    Ok(LlmResponse {
        message,
        usage,
        model,
        finish_reason,
    })
}

fn summarize_openai_response_shape(json: &Value) -> String {
    fn truncate_chars(value: &str, max_chars: usize) -> String {
        if value.chars().count() <= max_chars {
            return value.to_string();
        }
        let mut out = value
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect::<String>();
        out.push('…');
        out
    }

    let mut parts = Vec::new();
    if let Some(status) = json.get("status").and_then(|v| v.as_i64()) {
        parts.push(format!("status={status}"));
    }
    if let Some(message) = json.get("message").and_then(|v| v.as_str()) {
        parts.push(format!("message={}", truncate_chars(message, 240)));
    }
    if let Some(error) = json.get("error") {
        let error_text = error
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| error.to_string());
        parts.push(format!("error={}", truncate_chars(&error_text, 240)));
    }
    if parts.is_empty() {
        let keys = json
            .as_object()
            .map(|obj| obj.keys().cloned().collect::<Vec<_>>().join(","))
            .unwrap_or_else(|| json.to_string());
        parts.push(format!("keys={}", truncate_chars(&keys, 240)));
    }
    parts.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex_jwt_with_account(account_id: Option<&str>) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let claims = match account_id {
            Some(account_id) => serde_json::json!({
                "sub": "user-xyz",
                "exp": 9_999_999_999_i64,
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": account_id,
                    "chatgpt_plan_type": "plus"
                }
            }),
            None => serde_json::json!({
                "sub": "user-xyz",
                "exp": 9_999_999_999_i64
            }),
        };
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        format!("{header}.{payload}.sig")
    }

    fn header_value<'a>(headers: &'a [(String, String)], key: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn codex_cloudflare_headers_match_codex_cli_rs_contract() {
        let token = codex_jwt_with_account(Some("acct-abc-999"));
        let headers = codex_cloudflare_headers(Some(token.as_str()));

        assert_eq!(header_value(&headers, "originator"), Some("codex_cli_rs"));
        assert!(header_value(&headers, "User-Agent")
            .expect("user agent")
            .starts_with("codex_cli_rs/"));
        assert_eq!(
            header_value(&headers, "ChatGPT-Account-ID"),
            Some("acct-abc-999")
        );
        assert!(header_value(&headers, "chatgpt-account-id").is_none());
        assert!(header_value(&headers, "ChatGPT-Account-Id").is_none());
    }

    #[test]
    fn codex_cloudflare_headers_ignore_malformed_or_missing_account_tokens() {
        for token in ["not-a-jwt", "", "only.one", "  ", "...."] {
            let headers = codex_cloudflare_headers(Some(token));
            assert_eq!(header_value(&headers, "originator"), Some("codex_cli_rs"));
            assert!(header_value(&headers, "ChatGPT-Account-ID").is_none());
        }

        let token = codex_jwt_with_account(None);
        let headers = codex_cloudflare_headers(Some(token.as_str()));
        assert_eq!(header_value(&headers, "originator"), Some("codex_cli_rs"));
        assert!(header_value(&headers, "ChatGPT-Account-ID").is_none());
    }

    #[test]
    fn openai_codex_provider_attaches_cloudflare_headers_to_requests() {
        let token = codex_jwt_with_account(Some("acct-request"));
        let provider = openai_codex_provider(token.as_str(), "gpt-5.4", None);
        assert_eq!(provider.inner.base_url, OPENAI_CODEX_BASE_URL);

        let request = provider
            .inner
            .build_request(
                &Client::new(),
                &format!("{}/chat/completions", OPENAI_CODEX_BASE_URL),
                token.as_str(),
                &serde_json::json!({"model": "gpt-5.4", "messages": []}),
            )
            .build()
            .expect("request");
        let headers = request.headers();

        assert_eq!(headers.get("originator").unwrap(), "codex_cli_rs");
        assert!(headers
            .get("User-Agent")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("codex_cli_rs/"));
        assert_eq!(headers.get("ChatGPT-Account-ID").unwrap(), "acct-request");
    }

    #[test]
    fn openai_codex_provider_skips_cloudflare_headers_for_non_chatgpt_override() {
        let token = codex_jwt_with_account(Some("acct-request"));
        let provider = openai_codex_provider(
            token.as_str(),
            "gpt-5.4",
            Some("https://openrouter.ai/api/v1"),
        );

        let request = provider
            .inner
            .build_request(
                &Client::new(),
                "https://openrouter.ai/api/v1/chat/completions",
                token.as_str(),
                &serde_json::json!({"model": "gpt-5.4", "messages": []}),
            )
            .build()
            .expect("request");

        assert!(request.headers().get("originator").is_none());
        assert!(request.headers().get("ChatGPT-Account-ID").is_none());
    }

    #[test]
    fn test_format_tools_for_openai_api_shape() {
        let tools = vec![ToolSchema::new(
            "read_file",
            "Read file content",
            hermes_core::JsonSchema::new("object"),
        )];
        let formatted = GenericProvider::format_tools_for_openai_api(&tools);
        let rows = formatted.as_array().expect("tools array");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["type"], "function");
        assert_eq!(rows[0]["function"]["name"], "read_file");
        assert_eq!(rows[0]["function"]["description"], "Read file content");
        assert_eq!(rows[0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn test_moonshot_tool_schema_sanitizer_repairs_mcp_shapes() {
        let params = serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"description": "search text"},
                "filter": {
                    "type": "string",
                    "anyOf": [
                        {"type": "string"},
                        {"type": "null"}
                    ]
                },
                "tags": {
                    "type": "array",
                    "items": {"description": "tag"}
                },
                "db_type": {
                    "anyOf": [
                        {"enum": ["mysql", "postgresql", "", null]},
                        {"type": "null"}
                    ],
                    "nullable": true
                },
                "payload": {"$ref": "#/$defs/Payload"}
            },
            "$defs": {"Payload": {"type": "object", "properties": {}}}
        });

        let out = sanitize_moonshot_tool_parameters(&params);
        assert_eq!(out["type"], "object");
        assert_eq!(out["properties"]["query"]["type"], "string");
        assert_eq!(out["properties"]["filter"]["type"], "string");
        assert!(out["properties"]["filter"].get("anyOf").is_none());
        assert_eq!(out["properties"]["tags"]["items"]["type"], "string");
        assert_eq!(out["properties"]["db_type"]["type"], "string");
        assert_eq!(
            out["properties"]["db_type"]["enum"],
            serde_json::json!(["mysql", "postgresql"])
        );
        assert!(out["properties"]["db_type"].get("nullable").is_none());
        assert!(out["properties"]["payload"].get("type").is_none());
        assert_eq!(out["properties"]["payload"]["$ref"], "#/$defs/Payload");
    }

    #[test]
    fn test_moonshot_model_tool_formatter_applies_sanitizer() {
        let mut params = hermes_core::JsonSchema::new("object");
        params.properties = Some(Default::default());
        params
            .properties
            .as_mut()
            .expect("properties")
            .insert("q".to_string(), serde_json::json!({"description": "query"}));
        let tools = vec![ToolSchema::new("search", "Search", params)];

        let formatted = format_tools_for_openai_api_with_model(
            &tools,
            "openrouter/moonshotai/kimi-k2.6",
            "https://openrouter.ai/api/v1",
        );
        assert_eq!(
            formatted[0]["function"]["parameters"]["properties"]["q"]["type"],
            "string"
        );
        assert!(is_moonshot_model("nous/moonshotai/kimi-k2-thinking"));
        assert!(!is_moonshot_model("anthropic/claude-sonnet-4.6"));
    }

    #[test]
    fn test_merge_extra_body_fields_strips_local_request_controls() {
        let extra = serde_json::json!({
            "strict_api": true,
            "strict_tool_calls": true,
            "provider_strict": true,
            "provider_profile": "openrouter",
            "provider_preferences": {"allow": ["anthropic"]},
            "supports_vision": true,
            "temperature": 0.2
        });
        let mut body = serde_json::json!({"model": "m", "messages": []});
        GenericProvider::merge_extra_body_fields(&mut body, Some(&extra));
        assert!(body.get("strict_api").is_none());
        assert!(body.get("strict_tool_calls").is_none());
        assert!(body.get("provider_strict").is_none());
        assert!(body.get("provider_profile").is_none());
        assert!(body.get("provider_preferences").is_none());
        assert!(body.get("supports_vision").is_none());
        assert_eq!(body["temperature"], 0.2);
    }

    #[test]
    fn test_provider_profile_request_body_defaults_and_qwen_messages() {
        let provider = GenericProvider::new("https://dashscope.example/v1", "key", "qwen3.5")
            .with_provider_profile("qwen-oauth");
        let messages = vec![Message::system("Be helpful"), Message::user("hello")];
        let extra = serde_json::json!({
            "qwen_session_metadata": {"sessionId": "s123", "promptId": "p456"},
            "provider_profile": "qwen-oauth"
        });

        let body = provider.chat_request_body(ChatRequestParams {
            messages: &messages,
            tools: &[],
            max_tokens: None,
            temperature: Some(0.4),
            effective_model: "qwen3.5",
            extra_body: Some(&extra),
            stream: false,
        });

        assert_eq!(body["max_tokens"], 65_536);
        assert_eq!(body["temperature"], 0.4);
        assert_eq!(body["vl_high_resolution_images"], true);
        assert_eq!(
            body["metadata"],
            serde_json::json!({"sessionId": "s123", "promptId": "p456"})
        );
        assert!(body.get("qwen_session_metadata").is_none());
        assert!(body.get("provider_profile").is_none());
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"],
            serde_json::json!({"type": "ephemeral"})
        );
        assert_eq!(
            body["messages"][1]["content"][0],
            serde_json::json!({"type": "text", "text": "hello"})
        );
    }

    #[test]
    fn test_provider_profile_request_body_kimi_reasoning_contract() {
        let provider = GenericProvider::new("https://api.moonshot.ai/v1", "key", "kimi-k2")
            .with_provider_profile("kimi");
        let messages = vec![Message::user("hello")];

        let enabled = provider.chat_request_body(ChatRequestParams {
            messages: &messages,
            tools: &[],
            max_tokens: None,
            temperature: Some(0.7),
            effective_model: "kimi-k2",
            extra_body: Some(
                &serde_json::json!({"reasoning": {"enabled": true, "effort": "high"}}),
            ),
            stream: false,
        });
        assert_eq!(enabled["max_tokens"], 32_000);
        assert!(enabled.get("temperature").is_none());
        assert_eq!(enabled["thinking"], serde_json::json!({"type": "enabled"}));
        assert_eq!(enabled["reasoning_effort"], "high");
        assert!(enabled.get("reasoning").is_none());

        let disabled = provider.chat_request_body(ChatRequestParams {
            messages: &messages,
            tools: &[],
            max_tokens: None,
            temperature: Some(0.7),
            effective_model: "kimi-k2",
            extra_body: Some(&serde_json::json!({"reasoning_config": {"enabled": false}})),
            stream: false,
        });
        assert_eq!(
            disabled["thinking"],
            serde_json::json!({"type": "disabled"})
        );
        assert!(disabled.get("reasoning_effort").is_none());
    }

    #[test]
    fn test_provider_profile_request_body_openrouter_nous_and_custom_contracts() {
        let messages = vec![Message::user("hello")];

        let openrouter = GenericProvider::new(
            "https://openrouter.ai/api/v1",
            "key",
            "openrouter/pareto-code",
        )
        .with_provider_profile("openrouter");
        let or_body = openrouter.chat_request_body(ChatRequestParams {
            messages: &messages,
            tools: &[],
            max_tokens: None,
            temperature: None,
            effective_model: "openrouter/pareto-code",
            extra_body: Some(&serde_json::json!({
                "provider_preferences": {"allow": ["anthropic"], "sort": "price"},
                "openrouter_min_coding_score": 0.65,
                "supports_reasoning": true,
                "session_id": "sess-123"
            })),
            stream: false,
        });
        assert_eq!(
            or_body["provider"],
            serde_json::json!({"allow": ["anthropic"], "sort": "price"})
        );
        assert_eq!(or_body["session_id"], "sess-123");
        assert_eq!(
            or_body["plugins"],
            serde_json::json!([{"id": "pareto-router", "min_coding_score": 0.65}])
        );
        assert_eq!(
            or_body["reasoning"],
            serde_json::json!({"enabled": true, "effort": "medium"})
        );
        assert!(or_body.get("provider_preferences").is_none());

        let nous = GenericProvider::new(
            "https://inference-api.nousresearch.com/v1",
            "key",
            "hermes-3",
        )
        .with_provider_profile("nous");
        let nous_body = nous.chat_request_body(ChatRequestParams {
            messages: &messages,
            tools: &[],
            max_tokens: None,
            temperature: None,
            effective_model: "hermes-3",
            extra_body: Some(&serde_json::json!({
                "supports_reasoning": true,
                "reasoning": {"enabled": false}
            })),
            stream: false,
        });
        assert_eq!(
            nous_body["tags"],
            serde_json::json!(["product=hermes-agent"])
        );
        assert!(nous_body.get("reasoning").is_none());

        let custom = GenericProvider::new("http://127.0.0.1:11434/v1", "key", "qwen3:72b")
            .with_provider_profile("ollama-local");
        let custom_body = custom.chat_request_body(ChatRequestParams {
            messages: &messages,
            tools: &[],
            max_tokens: None,
            temperature: None,
            effective_model: "qwen3:72b",
            extra_body: Some(&serde_json::json!({"ollama_num_ctx": 131072})),
            stream: false,
        });
        assert_eq!(custom_body["options"]["num_ctx"], 131_072);
        assert!(custom_body.get("ollama_num_ctx").is_none());
    }

    #[test]
    fn test_opencode_go_kimi_reasoning_uses_moonshot_shape() {
        let provider =
            GenericProvider::new("https://opencode.ai/zen/go/v1", "test-key", "kimi-k2.6");
        let extra = serde_json::json!({"reasoning": {"effort": "xhigh"}});
        let mut body = serde_json::json!({"model": "kimi-k2.6", "messages": []});

        GenericProvider::merge_extra_body_fields(&mut body, Some(&extra));
        provider.apply_opencode_go_reasoning_controls(&mut body, "moonshotai/kimi-k2.6");

        assert_eq!(body["thinking"], serde_json::json!({"type": "enabled"}));
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn test_opencode_go_deepseek_reasoning_uses_thinking_shape() {
        let provider = GenericProvider::new(
            "https://opencode.ai/zen/go/v1",
            "test-key",
            "deepseek-v4-pro",
        );
        let extra = serde_json::json!({"reasoning": {"effort": "max"}});
        let mut body = serde_json::json!({"model": "deepseek-v4-pro", "messages": []});

        GenericProvider::merge_extra_body_fields(&mut body, Some(&extra));
        provider.apply_opencode_go_reasoning_controls(&mut body, "deepseek/deepseek-v4-pro");

        assert_eq!(body["thinking"], serde_json::json!({"type": "enabled"}));
        assert_eq!(body["reasoning_effort"], "max");
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn test_opencode_go_non_target_model_drops_reasoning_controls() {
        let provider = GenericProvider::new("https://opencode.ai/zen/go/v1", "test-key", "glm-5.1");
        let extra = serde_json::json!({"reasoning": {"effort": "high"}});
        let mut body = serde_json::json!({"model": "glm-5.1", "messages": []});

        GenericProvider::merge_extra_body_fields(&mut body, Some(&extra));
        provider.apply_opencode_go_reasoning_controls(&mut body, "glm-5.1");

        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn test_opencode_go_model_detection_handles_prefixes_and_variants() {
        assert_eq!(flat_model_name("opencode-go:kimi-k2.6"), "kimi-k2.6");
        assert_eq!(flat_model_name("opencode-go:kimi-k2.6:fast"), "kimi-k2.6");
        assert_eq!(flat_model_name("moonshotai/kimi-k2.6:fast"), "kimi-k2.6");
        assert_eq!(
            flat_model_name("openrouter:deepseek/deepseek-reasoner:max"),
            "deepseek-reasoner"
        );
    }

    #[test]
    fn test_sanitize_messages_for_strict_api_reconstructs_flattened_tool_call_function() {
        let messages = vec![Message::assistant_with_tool_calls(
            None,
            vec![ToolCall {
                id: "call_123".to_string(),
                function: FunctionCall {
                    name: "skills_list".to_string(),
                    arguments: "{\"category\":\"builtin\"}".to_string(),
                },
                extra_content: None,
            }],
        )];

        let sanitized =
            GenericProvider::sanitize_messages_for_api(&messages, true, "gpt-4o", None, None);
        let tc = &sanitized[0]["tool_calls"][0];
        assert_eq!(tc["id"], "call_123");
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "skills_list");
        assert_eq!(tc["function"]["arguments"], "{\"category\":\"builtin\"}");
        assert!(tc.get("name").is_none());
        assert!(tc.get("arguments").is_none());
    }

    #[test]
    fn test_sanitize_messages_for_strict_api_disabled_preserves_flattened_shape() {
        let messages = vec![Message::assistant_with_tool_calls(
            None,
            vec![ToolCall {
                id: "call_abc".to_string(),
                function: FunctionCall {
                    name: "read_file".to_string(),
                    arguments: "{\"path\":\"a.txt\"}".to_string(),
                },
                extra_content: None,
            }],
        )];
        let sanitized =
            GenericProvider::sanitize_messages_for_api(&messages, false, "gpt-4o", None, None);
        let tc = &sanitized[0]["tool_calls"][0];
        assert_eq!(tc["name"], "read_file");
        assert_eq!(tc["arguments"], "{\"path\":\"a.txt\"}");
        assert!(tc.get("function").is_none());
    }

    #[test]
    fn test_sanitize_messages_for_api_decodes_acp_multimodal_user_parts_for_vision_models() {
        let parts = serde_json::json!([
            {"type": "text", "text": "inspect"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
        ]);
        let marker = format!("{}{}", ACP_MULTIMODAL_PREFIX, parts);
        let messages = vec![Message::user(marker)];
        let sanitized =
            GenericProvider::sanitize_messages_for_api(&messages, false, "gpt-4o", None, None);
        assert!(sanitized[0]["content"].is_array());
        assert_eq!(sanitized[0]["content"][1]["type"], "image_url");
    }

    #[test]
    fn test_sanitize_messages_for_api_collapses_images_for_non_vision_models() {
        let parts = serde_json::json!([
            {"type": "text", "text": "inspect"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
        ]);
        let marker = format!("{}{}", ACP_MULTIMODAL_PREFIX, parts);
        let messages = vec![Message::user(marker)];
        let sanitized = GenericProvider::sanitize_messages_for_api(
            &messages,
            false,
            "deepseek-chat",
            None,
            None,
        );
        let content = sanitized[0]["content"].as_str().expect("collapsed text");
        assert!(content.contains("inspect"));
        assert!(content.contains("[Attached image]"));
        assert!(!content.contains("image_url"));
    }

    #[test]
    fn test_provider_profile_vision_preserves_acp_multimodal_parts() {
        let parts = serde_json::json!([
            {"type": "text", "text": "inspect"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
        ]);
        let messages = vec![Message::user(format!("{ACP_MULTIMODAL_PREFIX}{parts}"))];
        let provider = GenericProvider::new("https://api.xiaomimimo.com/v1", "key", "mimo-v2-omni")
            .with_provider_profile("mimo");

        let body = provider.chat_request_body(ChatRequestParams {
            messages: &messages,
            tools: &[],
            max_tokens: None,
            temperature: None,
            effective_model: "mimo-v2-omni",
            extra_body: None,
            stream: false,
        });

        assert_eq!(body["messages"][0]["content"][1]["type"], "image_url");
    }

    #[test]
    fn test_supports_vision_override_can_disable_multimodal_parts() {
        let parts = serde_json::json!([
            {"type": "text", "text": "inspect"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
        ]);
        let messages = vec![Message::user(format!("{ACP_MULTIMODAL_PREFIX}{parts}"))];
        let provider = GenericProvider::new("https://api.openai.com/v1", "key", "gpt-4o");
        let extra = serde_json::json!({"supports_vision": false});

        let body = provider.chat_request_body(ChatRequestParams {
            messages: &messages,
            tools: &[],
            max_tokens: None,
            temperature: None,
            effective_model: "gpt-4o",
            extra_body: Some(&extra),
            stream: false,
        });

        let content = body["messages"][0]["content"]
            .as_str()
            .expect("collapsed text");
        assert!(content.contains("[Attached image]"));
        assert!(body.get("supports_vision").is_none());
    }

    #[test]
    fn test_parse_openai_response_basic() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hello!"
                },
                "finish_reason": "stop"
            }],
            "model": "gpt-4o",
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });
        let resp = parse_openai_response(&json).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("Hello!"));
        assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
        assert_eq!(resp.usage.as_ref().unwrap().total_tokens, 15);
    }

    #[test]
    fn test_parse_openai_response_null_content_is_safe() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_null_content",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"Cargo.toml\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "model": "reasoning-tool-only"
        });

        let resp = parse_openai_response(&json).expect("null content response should parse");

        assert_eq!(resp.message.content.as_deref(), Some(""));
        let calls = resp.message.tool_calls.as_ref().expect("tool calls");
        assert_eq!(calls[0].id, "call_null_content");
        assert_eq!(calls[0].function.name, "read_file");
    }

    #[test]
    fn test_parse_openai_response_no_choices_includes_provider_context() {
        let json = serde_json::json!({
            "status": 400,
            "message": "This request is not valid. Check the model name and other parameters. Additional info: Provider returned error",
        });
        let err = parse_openai_response(&json).unwrap_err().to_string();
        assert!(err.contains("No choices in response"));
        assert!(err.contains("status=400"));
        assert!(err.contains("Provider returned error"));
    }

    #[test]
    fn test_parse_openai_response_empty_choices_includes_error_context() {
        let json = serde_json::json!({
            "choices": [],
            "error": {"message": "Check that you're sending a valid payload."},
        });
        let err = parse_openai_response(&json).unwrap_err().to_string();
        assert!(err.contains("Empty choices array"));
        assert!(err.contains("valid payload"));
    }

    #[test]
    fn test_parse_openai_response_with_tool_calls() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_123",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\": \"test.txt\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "model": "gpt-4o"
        });
        let resp = parse_openai_response(&json).unwrap();
        let tc = resp.message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].function.name, "read_file");
    }

    #[test]
    fn test_parse_openai_response_accepts_object_valued_tool_arguments() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_dict_args",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": {"path": "README.md"}
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "model": "local-openai-compatible"
        });

        let resp = parse_openai_response(&json).expect("object arguments should parse");
        let tc = resp.message.tool_calls.as_ref().unwrap();
        let args: Value = serde_json::from_str(&tc[0].function.arguments).unwrap();
        assert_eq!(args["path"], "README.md");
    }

    #[test]
    fn test_parse_openai_response_with_tool_call_extra_content() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_123",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\": \"test.txt\"}"
                        },
                        "extra_content": {
                            "google": {
                                "thought_signature": "SIG_ABC123"
                            }
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "model": "gemini-2.5-pro"
        });
        let resp = parse_openai_response(&json).unwrap();
        let tc = resp.message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].function.name, "read_file");
        assert_eq!(
            tc[0].extra_content,
            Some(serde_json::json!({
                "google": {
                    "thought_signature": "SIG_ABC123"
                }
            }))
        );
    }

    #[test]
    fn test_parse_sse_chunk_content() {
        let json = serde_json::json!({
            "choices": [{
                "delta": {
                    "content": "Hello"
                },
                "finish_reason": null
            }]
        });
        let chunk = parse_sse_chunk(&json).unwrap();
        assert_eq!(
            chunk.delta.as_ref().unwrap().content.as_deref(),
            Some("Hello")
        );
        assert!(chunk.finish_reason.is_none());
    }

    #[test]
    fn test_parse_sse_chunk_tool_call() {
        let json = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc",
                        "function": {
                            "name": "search",
                            "arguments": ""
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        let chunk = parse_sse_chunk(&json).unwrap();
        let tc = chunk.delta.as_ref().unwrap().tool_calls.as_ref().unwrap();
        assert_eq!(tc[0].index, 0);
        assert_eq!(tc[0].id.as_deref(), Some("call_abc"));
    }

    #[test]
    fn test_parse_sse_chunk_finish() {
        let json = serde_json::json!({
            "choices": [{
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50,
                "total_tokens": 150
            }
        });
        let chunk = parse_sse_chunk(&json).unwrap();
        assert_eq!(chunk.finish_reason.as_deref(), Some("stop"));
        assert_eq!(chunk.usage.as_ref().unwrap().total_tokens, 150);
    }

    #[test]
    fn test_anthropic_convert_messages() {
        let messages = vec![
            Message::system("You are helpful"),
            Message::user("Hello"),
            Message::assistant("Hi there!"),
        ];
        let (system, msgs) = AnthropicProvider::convert_messages(&messages, None);
        assert_eq!(system.as_deref(), Some("You are helpful"));
        assert_eq!(msgs.len(), 2); // user + assistant, system extracted
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
    }

    #[test]
    fn test_anthropic_convert_messages_decodes_acp_multimodal_user_parts() {
        let parts = serde_json::json!([
            {"type": "text", "text": "see attachment"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
        ]);
        let messages = vec![Message::user(format!("{ACP_MULTIMODAL_PREFIX}{parts}"))];
        let (_, msgs) = AnthropicProvider::convert_messages(&messages, None);
        let blocks = msgs[0]["content"].as_array().expect("blocks");
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "image");
    }

    #[test]
    fn test_anthropic_convert_messages_with_tool_result() {
        let messages = vec![
            Message::system("System"),
            Message::user("Do something"),
            Message {
                role: MessageRole::Assistant,
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "tc_1".to_string(),
                    function: FunctionCall {
                        name: "read_file".to_string(),
                        arguments: r#"{"path":"test.txt"}"#.to_string(),
                    },
                    extra_content: None,
                }]),
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                cache_control: None,
            },
            Message::tool_result("tc_1", "file contents here"),
        ];
        let (system, msgs) = AnthropicProvider::convert_messages(&messages, None);
        assert_eq!(system.as_deref(), Some("System"));
        assert_eq!(msgs.len(), 3); // user, assistant with tool_use, user with tool_result
                                   // Assistant message should have tool_use block
        let assistant_content = msgs[1]["content"].as_array().unwrap();
        assert_eq!(assistant_content[0]["type"], "tool_use");
        assert_eq!(assistant_content[0]["name"], "read_file");
        // Tool result should be a user message with tool_result block
        let tool_content = msgs[2]["content"].as_array().unwrap();
        assert_eq!(tool_content[0]["type"], "tool_result");
        assert_eq!(tool_content[0]["tool_use_id"], "tc_1");
    }

    #[test]
    fn test_anthropic_convert_messages_kimi_tool_replay_preserves_reasoning_content() {
        let messages = vec![Message {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc_kimi".to_string(),
                function: FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"date"}"#.to_string(),
                },
                extra_content: None,
            }]),
            tool_call_id: None,
            name: None,
            reasoning_content: Some("provider scratchpad".to_string()),
            cache_control: None,
        }];
        let (_, msgs) =
            AnthropicProvider::convert_messages(&messages, Some("https://api.kimi.com/coding/v1"));
        let content = msgs[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "provider scratchpad");
        assert_eq!(content[1]["type"], "tool_use");
    }

    #[test]
    fn test_anthropic_convert_messages_kimi_accepts_empty_reasoning_content() {
        let messages = vec![Message {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc_empty".to_string(),
                function: FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"ls"}"#.to_string(),
                },
                extra_content: None,
            }]),
            tool_call_id: None,
            name: None,
            reasoning_content: Some(String::new()),
            cache_control: None,
        }];
        let (_, msgs) =
            AnthropicProvider::convert_messages(&messages, Some("https://api.moonshot.ai/v1"));
        let content = msgs[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "");
    }

    #[test]
    fn test_anthropic_convert_messages_non_kimi_skips_thinking_block() {
        let messages = vec![Message {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc_other".to_string(),
                function: FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"pwd"}"#.to_string(),
                },
                extra_content: None,
            }]),
            tool_call_id: None,
            name: None,
            reasoning_content: Some("scratchpad".to_string()),
            cache_control: None,
        }];
        let (_, msgs) =
            AnthropicProvider::convert_messages(&messages, Some("https://api.anthropic.com"));
        let content = msgs[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_use");
    }

    #[test]
    fn test_anthropic_messages_url_adds_azure_api_version_query() {
        let url = AnthropicProvider::messages_url_for_base_url(
            "https://my-resource.openai.azure.com/anthropic",
        );
        assert_eq!(
            url,
            "https://my-resource.openai.azure.com/anthropic/v1/messages?api-version=2025-04-15"
        );

        let existing = AnthropicProvider::messages_url_for_base_url(
            "https://my-resource.openai.azure.com/anthropic?api-version=2024-01-01",
        );
        assert_eq!(
            existing,
            "https://my-resource.openai.azure.com/anthropic/v1/messages?api-version=2024-01-01"
        );
    }

    #[test]
    fn test_anthropic_request_uses_bearer_auth_and_azure_betas_for_foundry() {
        let provider = AnthropicProvider::new("azure-foundry-secret")
            .with_base_url("https://my-resource.openai.azure.com/anthropic");
        let url = provider.messages_url();
        let request = provider
            .build_request(
                &Client::new(),
                &url,
                "azure-foundry-secret",
                &serde_json::json!({}),
            )
            .build()
            .expect("request");
        let headers = request.headers();
        assert_eq!(
            headers.get("Authorization").and_then(|h| h.to_str().ok()),
            Some("Bearer azure-foundry-secret")
        );
        assert!(headers.get("x-api-key").is_none());
        let betas = headers
            .get("anthropic-beta")
            .and_then(|h| h.to_str().ok())
            .expect("anthropic-beta");
        assert!(betas.contains("context-1m-2025-08-07"));
        assert!(betas.contains("fine-grained-tool-streaming-2025-05-14"));
    }

    #[test]
    fn test_anthropic_request_uses_api_key_for_native_api_key() {
        let provider = AnthropicProvider::new("sk-ant-api03-secret");
        let request = provider
            .build_request(
                &Client::new(),
                "https://api.anthropic.com/v1/messages",
                "sk-ant-api03-secret",
                &serde_json::json!({}),
            )
            .build()
            .expect("request");
        let headers = request.headers();
        assert_eq!(
            headers.get("x-api-key").and_then(|h| h.to_str().ok()),
            Some("sk-ant-api03-secret")
        );
        assert!(headers.get("Authorization").is_none());
        let betas = headers
            .get("anthropic-beta")
            .and_then(|h| h.to_str().ok())
            .expect("anthropic-beta");
        assert!(betas.contains("interleaved-thinking-2025-05-14"));
        assert!(!betas.contains("oauth-2025-04-20"));
        assert!(!betas.contains("context-1m-2025-08-07"));
    }

    #[test]
    fn test_anthropic_request_uses_bearer_and_oauth_betas_for_native_oauth() {
        let provider = AnthropicProvider::new("sk-ant-oat01-secret");
        let request = provider
            .build_request(
                &Client::new(),
                "https://api.anthropic.com/v1/messages",
                "sk-ant-oat01-secret",
                &serde_json::json!({}),
            )
            .build()
            .expect("request");
        let headers = request.headers();
        assert_eq!(
            headers.get("Authorization").and_then(|h| h.to_str().ok()),
            Some("Bearer sk-ant-oat01-secret")
        );
        assert!(headers.get("x-api-key").is_none());
        let betas = headers
            .get("anthropic-beta")
            .and_then(|h| h.to_str().ok())
            .expect("anthropic-beta");
        assert!(betas.contains("oauth-2025-04-20"));
        assert!(betas.contains("claude-code-20250219"));
    }

    #[test]
    fn test_anthropic_strips_sampling_and_unsupported_fast_controls() {
        let mut body = serde_json::json!({
            "model": "claude-opus-4-8-fast",
            "messages": [],
            "temperature": 0.7,
            "top_p": 0.9,
            "top_k": 20,
            "speed": "fast"
        });
        AnthropicProvider::strip_unsupported_anthropic_controls(&mut body, "claude-opus-4-8-fast");
        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
        assert!(body.get("top_k").is_none());
        assert!(body.get("speed").is_none());

        let mut supported = serde_json::json!({
            "model": "claude-opus-4-6",
            "messages": [],
            "temperature": 0.7,
            "speed": "fast"
        });
        AnthropicProvider::strip_unsupported_anthropic_controls(&mut supported, "claude-opus-4-6");
        assert_eq!(supported["temperature"], serde_json::json!(0.7));
        assert_eq!(supported["speed"], serde_json::json!("fast"));
    }

    #[test]
    fn test_anthropic_parse_response() {
        let json = serde_json::json!({
            "content": [
                {"type": "text", "text": "Here is the answer."}
            ],
            "model": "claude-3-5-sonnet-20241022",
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50
            }
        });
        let resp = AnthropicProvider::parse_response(&json).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("Here is the answer."));
        assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
        assert_eq!(resp.usage.as_ref().unwrap().prompt_tokens, 100);
        assert_eq!(resp.usage.as_ref().unwrap().completion_tokens, 50);
    }

    #[test]
    fn test_anthropic_parse_response_preserves_thinking_as_reasoning_content() {
        let json = serde_json::json!({
            "content": [
                {"type": "thinking", "thinking": "step 1"},
                {"type": "text", "text": "answer"}
            ],
            "model": "claude-3-5-sonnet-20241022",
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5
            }
        });
        let resp = AnthropicProvider::parse_response(&json).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("answer"));
        assert_eq!(resp.message.reasoning_content.as_deref(), Some("step 1"));
    }

    #[test]
    fn test_anthropic_parse_response_with_tool_use() {
        let json = serde_json::json!({
            "content": [
                {"type": "text", "text": "Let me read that file."},
                {
                    "type": "tool_use",
                    "id": "toolu_123",
                    "name": "read_file",
                    "input": {"path": "test.txt"}
                }
            ],
            "model": "claude-3-5-sonnet-20241022",
            "stop_reason": "tool_use",
            "usage": {
                "input_tokens": 200,
                "output_tokens": 80
            }
        });
        let resp = AnthropicProvider::parse_response(&json).unwrap();
        assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
        let tc = resp.message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].id, "toolu_123");
        assert_eq!(tc[0].function.name, "read_file");
    }

    #[test]
    fn test_openrouter_parse_response_with_reasoning() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "The answer is 42.",
                    "reasoning_content": "Let me think step by step..."
                },
                "finish_reason": "stop"
            }],
            "model": "deepseek/deepseek-r1",
            "usage": {
                "prompt_tokens": 50,
                "completion_tokens": 30,
                "total_tokens": 80
            }
        });
        let resp = OpenRouterProvider::parse_openrouter_response(&json).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("The answer is 42."));
        assert_eq!(
            resp.message.reasoning_content.as_deref(),
            Some("Let me think step by step...")
        );
    }

    #[test]
    fn test_openrouter_parse_response_with_reasoning_details() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Final answer.",
                    "reasoning_details": [
                        {"type": "text", "text": "Step 1"},
                        {"type": "text", "text": "Step 2"}
                    ]
                },
                "finish_reason": "stop"
            }],
            "model": "openai/o1-preview"
        });
        let resp = OpenRouterProvider::parse_openrouter_response(&json).unwrap();
        let reasoning = resp.message.reasoning_content.as_deref().unwrap();
        assert!(reasoning.contains("Step 1"));
        assert!(reasoning.contains("Step 2"));
    }

    #[test]
    fn test_openrouter_parse_response_null_content_preserves_reasoning() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "reasoning_content": "Tool-only reasoning path"
                },
                "finish_reason": "stop"
            }],
            "model": "deepseek/deepseek-r1"
        });

        let resp = OpenRouterProvider::parse_openrouter_response(&json)
            .expect("reasoning-only OpenRouter response should parse");

        assert_eq!(resp.message.content.as_deref(), Some(""));
        assert_eq!(
            resp.message.reasoning_content.as_deref(),
            Some("Tool-only reasoning path")
        );
    }

    #[test]
    fn test_openrouter_build_headers() {
        let provider = OpenRouterProvider::new("key")
            .with_http_referer("https://example.com")
            .with_x_title("My App");
        let headers = provider.build_headers();
        assert!(headers
            .iter()
            .any(|(k, v)| k == "HTTP-Referer" && v == "https://example.com"));
        assert!(headers.iter().any(|(k, v)| k == "X-Title" && v == "My App"));
    }

    #[test]
    fn test_openrouter_parse_response_cache_control_from_extra_body() {
        let extra = serde_json::json!({
            "response_cache": {
                "enabled": true,
                "ttl_secs": 42,
                "clear": false
            }
        });
        let control = OpenRouterProvider::parse_response_cache_control(Some(&extra));
        assert!(control.enabled);
        assert_eq!(control.ttl_secs, 42);
        assert!(!control.clear);
    }

    #[test]
    fn test_openrouter_merge_extra_body_strips_local_cache_fields() {
        let extra = serde_json::json!({
            "response_cache": {"enabled": true},
            "response_cache_enabled": true,
            "response_cache_ttl_secs": 30,
            "response_cache_clear": false,
            "strict_api": true,
            "strict_tool_calls": true,
            "provider_strict": true,
            "reasoning_effort": "high",
            "route": "fallback",
            "provider": {"order": ["openai"]}
        });
        let merged = OpenRouterProvider::merge_extra_body(Some(&extra)).expect("merged body");
        assert!(merged.get("response_cache").is_none());
        assert!(merged.get("response_cache_enabled").is_none());
        assert!(merged.get("response_cache_ttl_secs").is_none());
        assert!(merged.get("response_cache_clear").is_none());
        assert!(merged.get("strict_api").is_none());
        assert!(merged.get("strict_tool_calls").is_none());
        assert!(merged.get("provider_strict").is_none());
        assert!(merged.get("reasoning_effort").is_none());
        assert_eq!(merged["reasoning"]["effort"], "high");
        assert_eq!(
            merged.get("route").and_then(|v| v.as_str()),
            Some("fallback")
        );
        assert!(merged.get("provider").is_some());
    }

    #[test]
    fn test_anthropic_convert_tools() {
        let tools = vec![ToolSchema::new(
            "read_file",
            "Read a file",
            hermes_core::JsonSchema::new("object"),
        )];
        let converted = AnthropicProvider::convert_tools(&tools);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0]["name"], "read_file");
        assert_eq!(converted[0]["description"], "Read a file");
        assert!(converted[0].get("input_schema").is_some());
    }

    #[test]
    fn test_anthropic_resolve_messages_max_tokens_prefers_positive_request() {
        let resolved =
            AnthropicProvider::resolve_messages_max_tokens(Some(8192), "claude-opus-4-1");
        assert_eq!(resolved, 8192);
    }

    #[test]
    fn test_anthropic_resolve_messages_max_tokens_zero_falls_back_to_model_default() {
        let resolved = AnthropicProvider::resolve_messages_max_tokens(Some(0), "claude-opus-4-6");
        assert!(resolved > 0);
        assert_eq!(resolved, get_anthropic_max_output("claude-opus-4-6"));
    }

    #[test]
    fn test_anthropic_resolve_messages_max_tokens_none_falls_back_to_model_default() {
        let resolved = AnthropicProvider::resolve_messages_max_tokens(None, "claude-sonnet-4-6");
        assert!(resolved > 0);
        assert_eq!(resolved, get_anthropic_max_output("claude-sonnet-4-6"));
    }
}
