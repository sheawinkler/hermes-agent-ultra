//! AWS Bedrock Converse provider and catalog helpers.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::{DateTime, Utc};
use futures::{stream::BoxStream, StreamExt};
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use hermes_core::{
    AgentError, FunctionCall, FunctionCallDelta, LlmProvider, LlmResponse, Message, MessageRole,
    StreamChunk, StreamDelta, ToolCall, ToolCallDelta, ToolSchema, UsageStats,
};

type HmacSha256 = Hmac<Sha256>;

pub const BEDROCK_AUTH_MARKER: &str = "aws-sdk";
pub const BEDROCK_DEFAULT_REGION: &str = "us-east-1";
pub const BEDROCK_DEFAULT_CONTEXT_LENGTH: u64 = 200_000;
pub const CONTEXT_1M_BETA: &str = "context-1m-2025-08-07";
const ACP_MULTIMODAL_PREFIX: &str = "__hermes_acp_parts_json__:";
const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";
const FINE_GRAINED_TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";
const BEDROCK_NOVA_PRO_CONTEXT_LENGTH: u64 = 300_000;
const BEDROCK_NOVA_MICRO_CONTEXT_LENGTH: u64 = 128_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BedrockAuth {
    Bearer(String),
    SigV4(AwsCredentials),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BedrockErrorClass {
    ContextOverflow,
    RateLimit,
    Overloaded,
    Unknown,
}

impl BedrockErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContextOverflow => "context_overflow",
            Self::RateLimit => "rate_limit",
            Self::Overloaded => "overloaded",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Default)]
struct StreamToolAccumulator {
    id: Option<String>,
    name: Option<String>,
    input_fragments: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AwsEventStreamMessage {
    headers: HashMap<String, String>,
    payload: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct BedrockProvider {
    base_url: Option<String>,
    region: String,
    model: String,
    client: Arc<Mutex<Client>>,
}

impl Default for BedrockProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl BedrockProvider {
    pub fn new() -> Self {
        Self {
            base_url: None,
            region: resolve_bedrock_region(),
            model: "anthropic.claude-3-5-sonnet-20241022-v2:0".to_string(),
            client: Arc::new(Mutex::new(Client::new())),
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        let region = region.into();
        if !region.trim().is_empty() {
            self.region = region.trim().to_string();
        }
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        let base_url = base_url.into();
        if !base_url.trim().is_empty() {
            self.base_url = Some(base_url.trim_end_matches('/').to_string());
        }
        self
    }

    fn effective_base_url(&self) -> String {
        self.base_url
            .clone()
            .unwrap_or_else(|| bedrock_runtime_base_url(&self.region))
    }
}

#[async_trait]
impl LlmProvider for BedrockProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        let effective_model = model
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or(self.model.as_str());
        let body = build_converse_body(
            effective_model,
            messages,
            tools,
            max_tokens,
            temperature,
            extra_body,
        );
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|err| AgentError::Config(format!("serialize Bedrock request: {err}")))?;
        let url = format!(
            "{}/model/{}/converse",
            self.effective_base_url().trim_end_matches('/'),
            percent_encode_path_segment(effective_model)
        );
        let auth = resolve_bedrock_auth().ok_or_else(|| {
            AgentError::AuthFailed(
                "No AWS credentials for Bedrock; set AWS_BEARER_TOKEN_BEDROCK, AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY, or a shared credentials profile".to_string(),
            )
        })?;
        let mut request = {
            let client = self
                .client
                .lock()
                .map(|c| c.clone())
                .unwrap_or_else(|_| Client::new());
            client.post(url.as_str()).body(body_bytes.clone())
        };
        let anthropic_beta = bedrock_anthropic_beta_header(effective_model);
        for (key, value) in bedrock_request_headers(
            BedrockHeaderRequest {
                method: "POST",
                url: url.as_str(),
                region: &self.region,
                service: "bedrock",
                body: &body_bytes,
                anthropic_beta: anthropic_beta.as_deref(),
                now: Utc::now(),
            },
            &auth,
        )? {
            request = request.header(key, value);
        }
        let response = request
            .send()
            .await
            .map_err(|err| AgentError::LlmApi(format!("Bedrock Converse request failed: {err}")))?;
        let status = response.status();
        let payload = response
            .text()
            .await
            .map_err(|err| AgentError::LlmApi(format!("Bedrock response read failed: {err}")))?;
        if !status.is_success() {
            return Err(map_bedrock_error(status.as_u16(), &payload));
        }
        let json: Value = serde_json::from_str(&payload).map_err(|err| {
            AgentError::LlmApi(format!(
                "Bedrock response JSON parse failed: {err}; body={payload}"
            ))
        })?;
        parse_bedrock_response(&json, effective_model)
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
        let model = model.map(str::to_string);
        let extra_body = extra_body.cloned();
        async_stream::stream! {
            let effective_model = model
                .as_deref()
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .unwrap_or(provider.model.as_str())
                .to_string();
            let body = build_converse_body(
                &effective_model,
                &messages,
                &tools,
                max_tokens,
                temperature,
                extra_body.as_ref(),
            );
            let body_bytes = match serde_json::to_vec(&body) {
                Ok(bytes) => bytes,
                Err(err) => {
                    yield Err(AgentError::Config(format!("serialize Bedrock stream request: {err}")));
                    return;
                }
            };
            let url = format!(
                "{}/model/{}/converse-stream",
                provider.effective_base_url().trim_end_matches('/'),
                percent_encode_path_segment(&effective_model)
            );
            let auth = match resolve_bedrock_auth() {
                Some(auth) => auth,
                None => {
                    yield Err(AgentError::AuthFailed(
                        "No AWS credentials for Bedrock; set AWS_BEARER_TOKEN_BEDROCK, AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY, or a shared credentials profile".to_string(),
                    ));
                    return;
                }
            };
            let anthropic_beta = bedrock_anthropic_beta_header(&effective_model);
            let headers = match bedrock_request_headers(
                BedrockHeaderRequest {
                    method: "POST",
                    url: url.as_str(),
                    region: &provider.region,
                    service: "bedrock",
                    body: &body_bytes,
                    anthropic_beta: anthropic_beta.as_deref(),
                    now: Utc::now(),
                },
                &auth,
            ) {
                Ok(headers) => headers,
                Err(err) => {
                    yield Err(err);
                    return;
                }
            };
            let mut request = {
                let client = provider
                    .client
                    .lock()
                    .map(|c| c.clone())
                    .unwrap_or_else(|_| Client::new());
                client.post(url.as_str()).body(body_bytes)
            };
            for (key, value) in headers {
                request = request.header(key, value);
            }
            let response = match request.send().await {
                Ok(response) => response,
                Err(err) => {
                    yield Err(AgentError::LlmApi(format!("Bedrock ConverseStream request failed: {err}")));
                    return;
                }
            };
            let status = response.status();
            if !status.is_success() {
                let payload = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "<no body>".to_string());
                yield Err(map_bedrock_error(status.as_u16(), &payload));
                return;
            }

            let mut byte_stream = response.bytes_stream();
            let mut buffer = Vec::new();
            let mut saw_tool_delta = false;
            while let Some(chunk_result) = byte_stream.next().await {
                let bytes = match chunk_result {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        yield Err(AgentError::LlmApi(format!("Bedrock ConverseStream read failed: {err}")));
                        return;
                    }
                };
                buffer.extend_from_slice(&bytes);
                loop {
                    let message = match take_aws_event_stream_message(&mut buffer) {
                        Ok(Some(message)) => message,
                        Ok(None) => break,
                        Err(err) => {
                            yield Err(err);
                            return;
                        }
                    };
                    let event = match decode_bedrock_event_stream_message(&message) {
                        Ok(Some(event)) => event,
                        Ok(None) => continue,
                        Err(err) => {
                            yield Err(err);
                            return;
                        }
                    };
                    let chunks = match bedrock_stream_event_to_chunks(&event) {
                        Ok(chunks) => chunks,
                        Err(err) => {
                            yield Err(err);
                            return;
                        }
                    };
                    for mut chunk in chunks {
                        if chunk
                            .delta
                            .as_ref()
                            .and_then(|delta| delta.tool_calls.as_ref())
                            .is_some_and(|calls| !calls.is_empty())
                        {
                            saw_tool_delta = true;
                        }
                        if saw_tool_delta && chunk.finish_reason.as_deref() == Some("stop") {
                            chunk.finish_reason = Some("tool_calls".to_string());
                        }
                        yield Ok(chunk);
                    }
                }
            }
            if !buffer.is_empty() {
                yield Err(AgentError::LlmApi(format!(
                    "Incomplete Bedrock ConverseStream event frame: {} trailing bytes",
                    buffer.len()
                )));
            }
        }
        .boxed()
    }
}

pub fn bedrock_runtime_base_url(region: &str) -> String {
    format!(
        "https://bedrock-runtime.{}.amazonaws.com",
        normalized_region_or_default(region)
    )
}

pub fn bedrock_control_base_url(region: &str) -> String {
    format!(
        "https://bedrock.{}.amazonaws.com",
        normalized_region_or_default(region)
    )
}

pub fn resolve_bedrock_region() -> String {
    std::env::var("AWS_REGION")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| {
            std::env::var("AWS_DEFAULT_REGION")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        })
        .or_else(resolve_region_from_aws_config)
        .unwrap_or_else(|| BEDROCK_DEFAULT_REGION.to_string())
}

pub fn has_aws_credentials() -> bool {
    std::env::var("AWS_BEARER_TOKEN_BEDROCK")
        .ok()
        .is_some_and(|v| !v.trim().is_empty())
        || resolve_env_credentials().is_some()
        || resolve_shared_credentials().is_some()
        || std::env::var("AWS_PROFILE")
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
        || std::env::var("AWS_WEB_IDENTITY_TOKEN_FILE")
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
        || std::env::var("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
        || std::env::var("AWS_CONTAINER_CREDENTIALS_FULL_URI")
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
}

pub fn curated_bedrock_models_for_region(region: &str) -> Vec<String> {
    let anthropic_prefix = anthropic_inference_profile_prefix(region);
    let amazon_prefix = amazon_inference_profile_prefix(region);
    vec![
        "anthropic.claude-sonnet-4-6".to_string(),
        format!("{anthropic_prefix}.anthropic.claude-sonnet-4-6"),
        "anthropic.claude-haiku-4-5-20251001-v1:0".to_string(),
        format!("{anthropic_prefix}.anthropic.claude-haiku-4-5-20251001-v1:0"),
        "anthropic.claude-3-5-sonnet-20241022-v2:0".to_string(),
        "amazon.nova-pro-v1:0".to_string(),
        format!("{amazon_prefix}.amazon.nova-pro-v1:0"),
        "amazon.nova-micro-v1:0".to_string(),
        format!("{amazon_prefix}.amazon.nova-micro-v1:0"),
    ]
}

pub async fn discover_bedrock_model_ids(region: &str) -> Vec<String> {
    if cfg!(test) {
        return Vec::new();
    }
    let Some(auth) = resolve_bedrock_auth() else {
        return Vec::new();
    };
    let region = normalized_region_or_default(region);
    let base = bedrock_control_base_url(&region);
    let mut ids = Vec::new();
    ids.extend(
        fetch_bedrock_catalog_endpoint(&format!("{base}/foundation-models"), &region, &auth).await,
    );
    ids.extend(
        fetch_bedrock_catalog_endpoint(&format!("{base}/inference-profiles"), &region, &auth).await,
    );
    dedup_model_ids(ids)
}

async fn fetch_bedrock_catalog_endpoint(
    url: &str,
    region: &str,
    auth: &BedrockAuth,
) -> Vec<String> {
    let headers = match bedrock_request_headers(
        BedrockHeaderRequest {
            method: "GET",
            url,
            region,
            service: "bedrock",
            body: b"",
            anthropic_beta: None,
            now: Utc::now(),
        },
        auth,
    ) {
        Ok(headers) => headers,
        Err(_) => return Vec::new(),
    };
    let client = Client::new();
    let mut request = client.get(url);
    for (key, value) in headers {
        request = request.header(key, value);
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(_) => return Vec::new(),
    };
    if !response.status().is_success() {
        return Vec::new();
    }
    let payload: Value = match response.json().await {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    parse_bedrock_catalog_model_ids(&payload)
}

pub fn parse_bedrock_catalog_model_ids(payload: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    collect_model_ids(payload, "modelSummaries", "modelId", true, &mut ids);
    collect_model_ids(
        payload,
        "inferenceProfileSummaries",
        "inferenceProfileId",
        true,
        &mut ids,
    );
    collect_model_ids(payload, "data", "id", false, &mut ids);
    dedup_model_ids(ids)
}

fn collect_model_ids(
    payload: &Value,
    array_key: &str,
    id_key: &str,
    apply_bedrock_filters: bool,
    out: &mut Vec<String>,
) {
    if let Some(rows) = payload.get(array_key).and_then(Value::as_array) {
        for row in rows {
            if apply_bedrock_filters && !bedrock_catalog_row_is_supported(row) {
                continue;
            }
            if let Some(id) = row
                .get(id_key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                out.push(id.to_string());
            }
        }
    }
}

fn bedrock_catalog_row_is_supported(row: &Value) -> bool {
    bedrock_catalog_row_is_active(row)
        && bedrock_catalog_row_supports_streaming(row)
        && bedrock_catalog_row_supports_text_output(row)
}

fn bedrock_catalog_row_is_active(row: &Value) -> bool {
    let status = row
        .get("modelLifecycle")
        .and_then(|v| v.get("status"))
        .or_else(|| row.get("status"))
        .or_else(|| row.get("modelStatus"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    status.is_none_or(|status| status.eq_ignore_ascii_case("ACTIVE"))
}

fn bedrock_catalog_row_supports_streaming(row: &Value) -> bool {
    row.get("responseStreamingSupported")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

fn bedrock_catalog_row_supports_text_output(row: &Value) -> bool {
    row.get("outputModalities")
        .and_then(Value::as_array)
        .map(|modalities| {
            modalities.iter().any(|value| {
                value
                    .as_str()
                    .is_some_and(|modality| modality.eq_ignore_ascii_case("TEXT"))
            })
        })
        .unwrap_or(true)
}

fn dedup_model_ids(ids: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut dedup = Vec::new();
    for id in ids {
        if seen.insert(id.to_ascii_lowercase()) {
            dedup.push(id);
        }
    }
    let mut global = Vec::new();
    let mut regional = Vec::new();
    for id in dedup {
        if id.to_ascii_lowercase().starts_with("global.") {
            global.push(id);
        } else {
            regional.push(id);
        }
    }
    global.extend(regional);
    global
}

pub fn build_converse_body(
    model: &str,
    messages: &[Message],
    tools: &[ToolSchema],
    max_tokens: Option<u32>,
    temperature: Option<f64>,
    extra_body: Option<&Value>,
) -> Value {
    let (system, messages) = convert_messages_to_bedrock(messages);
    let mut body = json!({
        "messages": messages,
    });
    if !system.is_empty() {
        body["system"] = Value::Array(system);
    }
    let mut inference = Map::new();
    if let Some(max_tokens) = max_tokens {
        inference.insert("maxTokens".to_string(), json!(max_tokens));
    }
    if let Some(temperature) = temperature {
        inference.insert("temperature".to_string(), json!(temperature));
    }
    if !inference.is_empty() {
        body["inferenceConfig"] = Value::Object(inference);
    }
    if !tools.is_empty() && model_supports_bedrock_tool_use(model) {
        body["toolConfig"] = json!({
            "tools": convert_tools_to_bedrock(tools),
        });
    }
    if let Some(fields) = bedrock_additional_model_request_fields(model) {
        body["additionalModelRequestFields"] = fields;
    }
    merge_bedrock_extra_body(&mut body, extra_body);
    body
}

fn merge_bedrock_extra_body(body: &mut Value, extra_body: Option<&Value>) {
    let Some(Value::Object(extra)) = extra_body else {
        return;
    };
    for (key, value) in extra {
        match key.as_str() {
            "strict_api" | "strict_tool_calls" | "provider_strict" => {}
            "additionalModelRequestFields" => {
                if let (Some(target), Some(source)) = (
                    body.get_mut("additionalModelRequestFields")
                        .and_then(Value::as_object_mut),
                    value.as_object(),
                ) {
                    for (field_key, field_value) in source {
                        target.insert(field_key.clone(), field_value.clone());
                    }
                } else {
                    body[key] = value.clone();
                }
            }
            "top_p" | "topP" => {
                set_bedrock_inference_field(body, "topP", value.clone());
            }
            "guardrail_config" | "guardrailConfig" => {
                if !value.as_object().is_some_and(Map::is_empty) && !value.is_null() {
                    body["guardrailConfig"] = value.clone();
                }
            }
            _ => {
                body[key] = value.clone();
            }
        }
    }
}

fn set_bedrock_inference_field(body: &mut Value, key: &str, value: Value) {
    if value.is_null() {
        return;
    }
    if !body.get("inferenceConfig").is_some_and(Value::is_object) {
        body["inferenceConfig"] = json!({});
    }
    if let Some(inference) = body
        .get_mut("inferenceConfig")
        .and_then(Value::as_object_mut)
    {
        inference.insert(key.to_string(), value);
    }
}

fn convert_messages_to_bedrock(messages: &[Message]) -> (Vec<Value>, Vec<Value>) {
    let mut system = Vec::new();
    let mut converted = Vec::new();
    for message in messages {
        match message.role {
            MessageRole::System => {
                if let Some(text) = message
                    .content
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    system.push(json!({"text": text}));
                }
            }
            MessageRole::User => {
                push_bedrock_message(
                    &mut converted,
                    "user",
                    content_blocks_or_placeholder(message.content.as_deref()),
                );
            }
            MessageRole::Assistant => {
                let mut content = optional_content_blocks(message.content.as_deref());
                if let Some(reasoning) = message
                    .reasoning_content
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    content.insert(0, json!({"reasoningContent": {"text": reasoning}}));
                }
                if let Some(tool_calls) = message.tool_calls.as_ref() {
                    for call in tool_calls {
                        let input: Value = serde_json::from_str(&call.function.arguments)
                            .unwrap_or_else(|_| json!({ "arguments": call.function.arguments }));
                        content.push(json!({
                            "toolUse": {
                                "toolUseId": call.id,
                                "name": call.function.name,
                                "input": input,
                            }
                        }));
                    }
                }
                if content.is_empty() {
                    content.push(json!({"text": " "}));
                }
                push_bedrock_message(&mut converted, "assistant", content);
            }
            MessageRole::Tool => {
                push_bedrock_message(
                    &mut converted,
                    "user",
                    vec![json!({
                        "toolResult": {
                            "toolUseId": message.tool_call_id.clone().unwrap_or_default(),
                            "content": content_blocks_or_placeholder(message.content.as_deref()),
                        }
                    })],
                );
            }
        }
    }
    enforce_bedrock_message_boundaries(&mut converted);
    (system, converted)
}

fn push_bedrock_message(messages: &mut Vec<Value>, role: &str, content: Vec<Value>) {
    if let Some(last) = messages.last_mut() {
        if last.get("role").and_then(Value::as_str) == Some(role) {
            if let Some(existing) = last.get_mut("content").and_then(Value::as_array_mut) {
                existing.extend(content);
                return;
            }
        }
    }
    messages.push(json!({
        "role": role,
        "content": content,
    }));
}

fn enforce_bedrock_message_boundaries(messages: &mut Vec<Value>) {
    if messages.is_empty() {
        messages.push(placeholder_user_message());
        return;
    }
    if messages
        .first()
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str)
        != Some("user")
    {
        messages.insert(0, placeholder_user_message());
    }
    if messages
        .last()
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str)
        != Some("user")
    {
        messages.push(placeholder_user_message());
    }
}

fn placeholder_user_message() -> Value {
    json!({
        "role": "user",
        "content": [{"text": " "}],
    })
}

fn content_blocks_or_placeholder(content: Option<&str>) -> Vec<Value> {
    let blocks = optional_content_blocks(content);
    if blocks.is_empty() {
        vec![json!({"text": " "})]
    } else {
        blocks
    }
}

fn optional_content_blocks(content: Option<&str>) -> Vec<Value> {
    match content.map(str::trim).filter(|s| !s.is_empty()) {
        Some(text) => {
            if let Some(parts) = parse_acp_multimodal_parts(text) {
                let blocks = bedrock_blocks_from_multimodal_parts(&parts);
                if !blocks.is_empty() {
                    return blocks;
                }
            }
            vec![json!({"text": text})]
        }
        None => Vec::new(),
    }
}

fn parse_acp_multimodal_parts(content: &str) -> Option<Vec<Value>> {
    let payload = content.trim().strip_prefix(ACP_MULTIMODAL_PREFIX)?;
    let parsed: Value = serde_json::from_str(payload).ok()?;
    let parts = parsed.as_array()?.clone();
    if parts.is_empty() {
        return None;
    }
    parts
        .iter()
        .all(|part| {
            part.as_object()
                .and_then(|obj| obj.get("type"))
                .and_then(Value::as_str)
                .is_some()
        })
        .then_some(parts)
}

fn bedrock_blocks_from_multimodal_parts(parts: &[Value]) -> Vec<Value> {
    let mut blocks = Vec::new();
    for part in parts {
        let Some(obj) = part.as_object() else {
            continue;
        };
        let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "text" => {
                if let Some(text) = obj
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                {
                    blocks.push(json!({"text": text}));
                }
            }
            "image_url" | "input_image" => {
                let url = extract_openai_image_url(obj);
                if let Some(block) = url.and_then(bedrock_image_block_from_openai_url) {
                    blocks.push(block);
                } else if let Some(url) = url.filter(|url| !url.is_empty()) {
                    blocks.push(json!({"text": format!("[Attached image]\nURL: {url}")}));
                }
            }
            _ => {
                if let Some(text) = obj
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                {
                    blocks.push(json!({"text": text}));
                }
            }
        }
    }
    blocks
}

fn extract_openai_image_url(obj: &Map<String, Value>) -> Option<&str> {
    obj.get("image_url")
        .and_then(|v| v.get("url"))
        .and_then(Value::as_str)
        .or_else(|| obj.get("image_url").and_then(Value::as_str))
        .or_else(|| obj.get("url").and_then(Value::as_str))
        .map(str::trim)
        .filter(|url| !url.is_empty())
}

fn bedrock_image_block_from_openai_url(url: &str) -> Option<Value> {
    let data = url.strip_prefix("data:")?;
    let (metadata, bytes) = data.split_once(',')?;
    if !metadata
        .split(';')
        .any(|segment| segment.eq_ignore_ascii_case("base64"))
    {
        return None;
    }
    BASE64_STANDARD.decode(bytes).ok()?;
    let media_type = metadata
        .split(';')
        .next()
        .unwrap_or("image/jpeg")
        .to_ascii_lowercase();
    let format = match media_type.strip_prefix("image/")? {
        "jpg" | "jpeg" => "jpeg",
        "png" => "png",
        "gif" => "gif",
        "webp" => "webp",
        _ => return None,
    };
    Some(json!({
        "image": {
            "format": format,
            "source": {
                "bytes": bytes,
            },
        },
    }))
}

pub fn convert_tools_to_bedrock(tools: &[ToolSchema]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let schema = serde_json::to_value(&tool.parameters)
                .ok()
                .filter(|schema| schema.as_object().is_some_and(|obj| !obj.is_empty()))
                .unwrap_or_else(default_bedrock_tool_schema);
            json!({
                "toolSpec": {
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": {
                        "json": schema,
                    },
                }
            })
        })
        .collect()
}

fn default_bedrock_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
    })
}

include!("bedrock/response_parsing.rs");

fn bedrock_additional_model_request_fields(model: &str) -> Option<Value> {
    let betas = bedrock_anthropic_betas(model)?;
    Some(json!({ "anthropic_beta": betas }))
}

pub fn bedrock_anthropic_betas(model: &str) -> Option<Vec<String>> {
    if !is_bedrock_anthropic_model_id(model) {
        return None;
    }
    Some(vec![
        CONTEXT_1M_BETA.to_string(),
        INTERLEAVED_THINKING_BETA.to_string(),
        FINE_GRAINED_TOOL_STREAMING_BETA.to_string(),
    ])
}

fn bedrock_anthropic_beta_header(model: &str) -> Option<String> {
    bedrock_anthropic_betas(model).map(|betas| betas.join(","))
}

pub fn is_bedrock_anthropic_model_id(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    [
        "anthropic.",
        "us.anthropic.",
        "eu.anthropic.",
        "ap.anthropic.",
        "au.anthropic.",
        "jp.anthropic.",
        "apac.anthropic.",
        "global.anthropic.",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

pub fn get_bedrock_context_length(model: &str) -> u64 {
    let model = normalized_bedrock_model_id_for_lookup(model);
    if model.starts_with("amazon.nova-micro") {
        BEDROCK_NOVA_MICRO_CONTEXT_LENGTH
    } else if model.starts_with("amazon.nova-") {
        BEDROCK_NOVA_PRO_CONTEXT_LENGTH
    } else if model.contains("anthropic.claude") {
        200_000
    } else {
        BEDROCK_DEFAULT_CONTEXT_LENGTH
    }
}

pub fn model_supports_bedrock_tool_use(model: &str) -> bool {
    let model = normalized_bedrock_model_id_for_lookup(model);
    !(model.contains("deepseek.r1")
        || model.contains("deepseek-r1")
        || model.starts_with("stability.")
        || model.contains(".embed")
        || model.contains("embed-"))
}

fn normalized_bedrock_model_id_for_lookup(model: &str) -> String {
    let lower = model.trim().to_ascii_lowercase();
    let model = lower.rsplit('/').next().unwrap_or(lower.as_str());
    for prefix in [
        "global.", "us.", "eu.", "ap.", "au.", "jp.", "apac.", "aws.",
    ] {
        if let Some(stripped) = model.strip_prefix(prefix) {
            return stripped.to_string();
        }
    }
    model.to_string()
}

include!("bedrock/auth_sigv4.rs");

#[cfg(test)]
mod tests;
