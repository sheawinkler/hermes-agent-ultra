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

pub fn validate_bedrock_response(value: &Value) -> bool {
    value.get("output").and_then(|v| v.get("message")).is_some() && value.get("error").is_none()
}

pub fn map_bedrock_finish_reason(reason: Option<&str>) -> Option<String> {
    Some(
        match reason.unwrap_or("end_turn") {
            "end_turn" | "stop_sequence" => "stop",
            "tool_use" => "tool_calls",
            "max_tokens" => "length",
            "content_filtered" | "guardrail_intervened" => "content_filter",
            _ => "stop",
        }
        .to_string(),
    )
}

pub fn parse_bedrock_response(json: &Value, model: &str) -> Result<LlmResponse, AgentError> {
    if let Some(response) = parse_openai_like_response(json, model) {
        return Ok(response);
    }
    if !validate_bedrock_response(json) {
        return Err(AgentError::LlmApi(format!(
            "Invalid Bedrock response shape: {}",
            truncate_json(json, 600)
        )));
    }
    let content_blocks = json
        .get("output")
        .and_then(|v| v.get("message"))
        .and_then(|v| v.get("content"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut text_parts = Vec::new();
    let mut reasoning_parts = Vec::new();
    let mut tool_calls = Vec::new();
    for block in content_blocks {
        if let Some(text) = block.get("text").and_then(Value::as_str) {
            if !text.is_empty() {
                text_parts.push(text.to_string());
            }
        }
        if let Some(text) = block
            .get("reasoningContent")
            .and_then(|v| v.get("text"))
            .and_then(Value::as_str)
        {
            if !text.is_empty() {
                reasoning_parts.push(text.to_string());
            }
        }
        if let Some(tool_use) = block.get("toolUse") {
            let id = tool_use
                .get("toolUseId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let name = tool_use
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let arguments = tool_use
                .get("input")
                .cloned()
                .unwrap_or_else(|| json!({}))
                .to_string();
            tool_calls.push(ToolCall {
                id,
                function: FunctionCall { name, arguments },
                extra_content: None,
            });
        }
    }
    let usage = json.get("usage").map(|usage| UsageStats {
        prompt_tokens: usage
            .get("inputTokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        completion_tokens: usage
            .get("outputTokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        total_tokens: usage
            .get("totalTokens")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                usage
                    .get("inputTokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default()
                    + usage
                        .get("outputTokens")
                        .and_then(Value::as_u64)
                        .unwrap_or_default()
            }),
        estimated_cost: None,
    });
    let finish_reason = if tool_calls.is_empty() {
        map_bedrock_finish_reason(json.get("stopReason").and_then(Value::as_str))
    } else {
        Some("tool_calls".to_string())
    };
    Ok(LlmResponse {
        message: Message {
            role: MessageRole::Assistant,
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
            tool_call_id: None,
            name: None,
            reasoning_content: if reasoning_parts.is_empty() {
                None
            } else {
                Some(reasoning_parts.join("\n"))
            },
            cache_control: None,
        },
        usage,
        model: model.to_string(),
        finish_reason,
    })
}

pub fn parse_bedrock_stream_events(json: &Value, model: &str) -> Result<LlmResponse, AgentError> {
    let events = json
        .get("stream")
        .and_then(Value::as_array)
        .or_else(|| json.as_array())
        .ok_or_else(|| {
            AgentError::LlmApi(format!(
                "Invalid Bedrock ConverseStream shape: {}",
                truncate_json(json, 600)
            ))
        })?;
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tools: BTreeMap<u64, StreamToolAccumulator> = BTreeMap::new();
    let mut stop_reason: Option<String> = None;
    let mut usage: Option<UsageStats> = None;

    for event in events {
        if let Some(start) = event.get("contentBlockStart") {
            let index = stream_content_block_index(start);
            if let Some(tool_use) = start.get("start").and_then(|v| v.get("toolUse")) {
                let entry = tools.entry(index).or_default();
                entry.id = tool_use
                    .get("toolUseId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                entry.name = tool_use
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
        }
        if let Some(delta_event) = event.get("contentBlockDelta") {
            let index = stream_content_block_index(delta_event);
            if let Some(delta) = delta_event.get("delta") {
                if let Some(fragment) = delta.get("text").and_then(Value::as_str) {
                    text.push_str(fragment);
                }
                if let Some(fragment) = delta
                    .get("reasoningContent")
                    .and_then(|v| v.get("text"))
                    .and_then(Value::as_str)
                {
                    reasoning.push_str(fragment);
                }
                if let Some(tool_use) = delta.get("toolUse") {
                    let entry = tools.entry(index).or_default();
                    if let Some(id) = tool_use.get("toolUseId").and_then(Value::as_str) {
                        entry.id = Some(id.to_string());
                    }
                    if let Some(name) = tool_use.get("name").and_then(Value::as_str) {
                        entry.name = Some(name.to_string());
                    }
                    if let Some(input) = tool_use.get("input").and_then(Value::as_str) {
                        entry.input_fragments.push_str(input);
                    } else if let Some(input) = tool_use.get("input") {
                        entry.input_fragments.push_str(&input.to_string());
                    }
                }
            }
        }
        if let Some(message_stop) = event.get("messageStop") {
            stop_reason = message_stop
                .get("stopReason")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if let Some(metadata) = event.get("metadata") {
            if let Some(raw_usage) = metadata.get("usage") {
                usage = Some(parse_bedrock_usage(raw_usage));
            }
        }
    }

    let tool_calls = tools
        .into_values()
        .filter_map(|tool| {
            let name = tool.name?;
            if name.trim().is_empty() {
                return None;
            }
            Some(ToolCall {
                id: tool.id.unwrap_or_default(),
                function: FunctionCall {
                    name,
                    arguments: normalize_tool_input_arguments(&tool.input_fragments),
                },
                extra_content: None,
            })
        })
        .collect::<Vec<_>>();
    let finish_reason = if tool_calls.is_empty() {
        map_bedrock_finish_reason(stop_reason.as_deref())
    } else {
        Some("tool_calls".to_string())
    };
    Ok(LlmResponse {
        message: Message {
            role: MessageRole::Assistant,
            content: if text.is_empty() { None } else { Some(text) },
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
            name: None,
            reasoning_content: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
            cache_control: None,
        },
        usage,
        model: model.to_string(),
        finish_reason,
    })
}

fn take_aws_event_stream_message(
    buffer: &mut Vec<u8>,
) -> Result<Option<AwsEventStreamMessage>, AgentError> {
    if buffer.len() < 12 {
        return Ok(None);
    }
    let total_len = read_be_u32(&buffer[0..4]) as usize;
    let headers_len = read_be_u32(&buffer[4..8]) as usize;
    if total_len < 16 {
        return Err(AgentError::LlmApi(format!(
            "Invalid Bedrock event stream frame length: {total_len}"
        )));
    }
    if total_len > buffer.len() {
        return Ok(None);
    }
    if headers_len > total_len.saturating_sub(16) {
        return Err(AgentError::LlmApi(format!(
            "Invalid Bedrock event stream headers length: {headers_len}"
        )));
    }

    let frame: Vec<u8> = buffer.drain(..total_len).collect();
    let expected_prelude_crc = read_be_u32(&frame[8..12]);
    let actual_prelude_crc = crc32_ieee(&frame[..8]);
    if expected_prelude_crc != actual_prelude_crc {
        return Err(AgentError::LlmApi(
            "Invalid Bedrock event stream prelude checksum".to_string(),
        ));
    }

    let expected_message_crc = read_be_u32(&frame[total_len - 4..total_len]);
    let actual_message_crc = crc32_ieee(&frame[..total_len - 4]);
    if expected_message_crc != actual_message_crc {
        return Err(AgentError::LlmApi(
            "Invalid Bedrock event stream message checksum".to_string(),
        ));
    }

    let headers_start = 12;
    let headers_end = headers_start + headers_len;
    let payload_end = total_len - 4;
    Ok(Some(AwsEventStreamMessage {
        headers: parse_aws_event_stream_headers(&frame[headers_start..headers_end])?,
        payload: frame[headers_end..payload_end].to_vec(),
    }))
}

fn decode_bedrock_event_stream_message(
    message: &AwsEventStreamMessage,
) -> Result<Option<Value>, AgentError> {
    if message.payload.is_empty() {
        return Ok(None);
    }
    let payload: Value = serde_json::from_slice(&message.payload).map_err(|err| {
        AgentError::LlmApi(format!("Bedrock event stream JSON parse failed: {err}"))
    })?;
    let message_type = message.headers.get(":message-type").map(String::as_str);
    let event_type = message.headers.get(":event-type").map(String::as_str);
    if matches!(message_type, Some("exception"))
        || event_type.is_some_and(|event| event.ends_with("Exception"))
        || bedrock_stream_exception_status(event_type).is_some()
        || bedrock_stream_payload_exception_status(&payload).is_some()
    {
        return Err(map_bedrock_error(
            bedrock_stream_exception_status(event_type)
                .or_else(|| bedrock_stream_payload_exception_status(&payload))
                .unwrap_or(500),
            &payload.to_string(),
        ));
    }
    if is_bedrock_stream_event_value(&payload) {
        return Ok(Some(payload));
    }
    if let Some(event_type) = event_type.filter(|event| !event.is_empty()) {
        return Ok(Some(json!({ event_type: payload })));
    }
    Ok(Some(payload))
}

fn bedrock_stream_event_to_chunks(event: &Value) -> Result<Vec<StreamChunk>, AgentError> {
    let mut chunks = Vec::new();
    if let Some(start_event) = event.get("contentBlockStart") {
        if let Some(tool_use) = start_event
            .get("start")
            .and_then(|start| start.get("toolUse"))
        {
            let index = stream_content_block_index(start_event) as u32;
            let id = tool_use
                .get("toolUseId")
                .and_then(Value::as_str)
                .map(str::to_string);
            let name = tool_use
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string);
            chunks.push(StreamChunk {
                delta: Some(StreamDelta {
                    content: None,
                    tool_calls: Some(vec![ToolCallDelta {
                        index,
                        id,
                        function: Some(FunctionCallDelta {
                            name,
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

    if let Some(delta_event) = event.get("contentBlockDelta") {
        if let Some(delta) = delta_event.get("delta") {
            if let Some(text) = delta.get("text").and_then(Value::as_str) {
                chunks.push(StreamChunk {
                    delta: Some(StreamDelta {
                        content: Some(text.to_string()),
                        tool_calls: None,
                        extra: None,
                    }),
                    finish_reason: None,
                    usage: None,
                });
            }
            if let Some(reasoning) = delta
                .get("reasoningContent")
                .and_then(|value| value.get("text"))
                .and_then(Value::as_str)
            {
                chunks.push(StreamChunk {
                    delta: Some(StreamDelta {
                        content: None,
                        tool_calls: None,
                        extra: Some(json!({ "thinking": reasoning })),
                    }),
                    finish_reason: None,
                    usage: None,
                });
            }
            if let Some(tool_use) = delta.get("toolUse") {
                let index = stream_content_block_index(delta_event) as u32;
                let id = tool_use
                    .get("toolUseId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let name = tool_use
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let arguments = tool_use.get("input").map(|input| {
                    input
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| input.to_string())
                });
                chunks.push(StreamChunk {
                    delta: Some(StreamDelta {
                        content: None,
                        tool_calls: Some(vec![ToolCallDelta {
                            index,
                            id,
                            function: Some(FunctionCallDelta { name, arguments }),
                        }]),
                        extra: None,
                    }),
                    finish_reason: None,
                    usage: None,
                });
            }
        }
    }

    if let Some(stop_event) = event.get("messageStop") {
        chunks.push(StreamChunk {
            delta: None,
            finish_reason: map_bedrock_finish_reason(
                stop_event.get("stopReason").and_then(Value::as_str),
            ),
            usage: None,
        });
    }

    if let Some(metadata) = event.get("metadata") {
        if let Some(raw_usage) = metadata.get("usage") {
            chunks.push(StreamChunk {
                delta: None,
                finish_reason: None,
                usage: Some(parse_bedrock_usage(raw_usage)),
            });
        }
    }

    Ok(chunks)
}

fn is_bedrock_stream_event_value(value: &Value) -> bool {
    [
        "messageStart",
        "contentBlockStart",
        "contentBlockDelta",
        "contentBlockStop",
        "messageStop",
        "metadata",
        "internalServerException",
        "modelStreamErrorException",
        "serviceUnavailableException",
        "throttlingException",
        "validationException",
    ]
    .iter()
    .any(|key| value.get(*key).is_some())
}

fn bedrock_stream_exception_status(event_type: Option<&str>) -> Option<u16> {
    match event_type? {
        "validationException" => Some(400),
        "throttlingException" => Some(429),
        "modelTimeoutException" => Some(408),
        "modelStreamErrorException" => Some(424),
        "serviceUnavailableException" => Some(503),
        "internalServerException" => Some(500),
        _ => None,
    }
}

fn bedrock_stream_payload_exception_status(payload: &Value) -> Option<u16> {
    [
        ("validationException", 400),
        ("throttlingException", 429),
        ("modelTimeoutException", 408),
        ("modelStreamErrorException", 424),
        ("serviceUnavailableException", 503),
        ("internalServerException", 500),
    ]
    .iter()
    .find_map(|(key, status)| payload.get(*key).map(|_| *status))
}

fn parse_aws_event_stream_headers(raw: &[u8]) -> Result<HashMap<String, String>, AgentError> {
    let mut headers = HashMap::new();
    let mut offset = 0;
    while offset < raw.len() {
        let name_len = *raw.get(offset).ok_or_else(|| {
            AgentError::LlmApi("Malformed Bedrock event stream header name".to_string())
        })? as usize;
        offset += 1;
        if name_len == 0 || offset + name_len > raw.len() {
            return Err(AgentError::LlmApi(
                "Malformed Bedrock event stream header name".to_string(),
            ));
        }
        let name = std::str::from_utf8(&raw[offset..offset + name_len])
            .map_err(|err| {
                AgentError::LlmApi(format!("Bedrock event stream header name UTF-8: {err}"))
            })?
            .to_string();
        offset += name_len;
        let value_type = *raw.get(offset).ok_or_else(|| {
            AgentError::LlmApi("Malformed Bedrock event stream header value".to_string())
        })?;
        offset += 1;
        match value_type {
            0 => {
                headers.insert(name, "true".to_string());
            }
            1 => {
                headers.insert(name, "false".to_string());
            }
            2 => {
                ensure_header_bytes(raw, offset, 1)?;
                headers.insert(name, i8::from_be_bytes([raw[offset]]).to_string());
                offset += 1;
            }
            3 => {
                ensure_header_bytes(raw, offset, 2)?;
                headers.insert(
                    name,
                    i16::from_be_bytes([raw[offset], raw[offset + 1]]).to_string(),
                );
                offset += 2;
            }
            4 => {
                ensure_header_bytes(raw, offset, 4)?;
                headers.insert(
                    name,
                    i32::from_be_bytes([
                        raw[offset],
                        raw[offset + 1],
                        raw[offset + 2],
                        raw[offset + 3],
                    ])
                    .to_string(),
                );
                offset += 4;
            }
            5 | 8 => {
                ensure_header_bytes(raw, offset, 8)?;
                headers.insert(
                    name,
                    i64::from_be_bytes([
                        raw[offset],
                        raw[offset + 1],
                        raw[offset + 2],
                        raw[offset + 3],
                        raw[offset + 4],
                        raw[offset + 5],
                        raw[offset + 6],
                        raw[offset + 7],
                    ])
                    .to_string(),
                );
                offset += 8;
            }
            6 => {
                let len = read_header_len(raw, &mut offset)?;
                ensure_header_bytes(raw, offset, len)?;
                headers.insert(name, hex::encode(&raw[offset..offset + len]));
                offset += len;
            }
            7 => {
                let len = read_header_len(raw, &mut offset)?;
                ensure_header_bytes(raw, offset, len)?;
                let value = std::str::from_utf8(&raw[offset..offset + len])
                    .map_err(|err| {
                        AgentError::LlmApi(format!("Bedrock event stream header UTF-8: {err}"))
                    })?
                    .to_string();
                headers.insert(name, value);
                offset += len;
            }
            9 => {
                ensure_header_bytes(raw, offset, 16)?;
                headers.insert(name, hex::encode(&raw[offset..offset + 16]));
                offset += 16;
            }
            other => {
                return Err(AgentError::LlmApi(format!(
                    "Unsupported Bedrock event stream header value type: {other}"
                )));
            }
        }
    }
    Ok(headers)
}

fn read_header_len(raw: &[u8], offset: &mut usize) -> Result<usize, AgentError> {
    ensure_header_bytes(raw, *offset, 2)?;
    let len = u16::from_be_bytes([raw[*offset], raw[*offset + 1]]) as usize;
    *offset += 2;
    Ok(len)
}

fn ensure_header_bytes(raw: &[u8], offset: usize, len: usize) -> Result<(), AgentError> {
    if offset + len > raw.len() {
        return Err(AgentError::LlmApi(
            "Malformed Bedrock event stream header value".to_string(),
        ));
    }
    Ok(())
}

fn read_be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn crc32_ieee(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn stream_content_block_index(event: &Value) -> u64 {
    event
        .get("contentBlockIndex")
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

fn parse_bedrock_usage(usage: &Value) -> UsageStats {
    let prompt_tokens = usage
        .get("inputTokens")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let completion_tokens = usage
        .get("outputTokens")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let total_tokens = usage
        .get("totalTokens")
        .and_then(Value::as_u64)
        .unwrap_or(prompt_tokens + completion_tokens);
    UsageStats {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        estimated_cost: None,
    }
}

fn normalize_tool_input_arguments(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return "{}".to_string();
    }
    serde_json::from_str::<Value>(trimmed)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| trimmed.to_string())
}

fn parse_openai_like_response(json: &Value, fallback_model: &str) -> Option<LlmResponse> {
    let choices = json.get("choices")?.as_array()?;
    let choice = choices.first()?;
    let message_obj = choice.get("message")?;
    let content = message_obj
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let tool_calls = message_obj
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let function = tc.get("function")?;
                    Some(ToolCall {
                        id: tc
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        function: FunctionCall {
                            name: function.get("name")?.as_str()?.to_string(),
                            arguments: function
                                .get("arguments")
                                .and_then(Value::as_str)
                                .unwrap_or("{}")
                                .to_string(),
                        },
                        extra_content: None,
                    })
                })
                .collect::<Vec<_>>()
        })
        .filter(|calls| !calls.is_empty());
    let usage = json.get("usage").map(|usage| UsageStats {
        prompt_tokens: usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        completion_tokens: usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        total_tokens: usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        estimated_cost: None,
    });
    Some(LlmResponse {
        message: Message {
            role: MessageRole::Assistant,
            content: Some(content),
            tool_calls,
            tool_call_id: None,
            name: None,
            reasoning_content: message_obj
                .get("reasoning")
                .or_else(|| message_obj.get("reasoning_content"))
                .and_then(Value::as_str)
                .map(str::to_string),
            cache_control: None,
        },
        usage,
        model: json
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(fallback_model)
            .to_string(),
        finish_reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

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

pub fn classify_bedrock_error(message: &str) -> BedrockErrorClass {
    let lower = message.to_ascii_lowercase();
    if lower.contains("input is too long")
        || lower.contains("exceeds the maximum number of input tokens")
        || lower.contains("maximum context length")
        || lower.contains("context length")
        || lower.contains("too many tokens")
    {
        BedrockErrorClass::ContextOverflow
    } else if lower.contains("throttlingexception")
        || lower.contains("too many concurrent requests")
        || lower.contains("too many requests")
        || lower.contains("rate exceeded")
        || lower.contains("rate limit")
    {
        BedrockErrorClass::RateLimit
    } else if lower.contains("modelnotreadyexception")
        || lower.contains("modeltimeoutexception")
        || lower.contains("serviceunavailable")
        || lower.contains("temporarily unavailable")
        || lower.contains("overloaded")
    {
        BedrockErrorClass::Overloaded
    } else {
        BedrockErrorClass::Unknown
    }
}

fn map_bedrock_error(status: u16, body: &str) -> AgentError {
    let lower = body.to_ascii_lowercase();
    if status == 401
        || status == 403
        || lower.contains("unauthorized")
        || lower.contains("accessdenied")
        || lower.contains("invalidsignature")
    {
        AgentError::AuthFailed(format!("Bedrock authorization failed: {body}"))
    } else {
        match classify_bedrock_error(body) {
            BedrockErrorClass::ContextOverflow => AgentError::ContextTooLong,
            BedrockErrorClass::RateLimit => AgentError::RateLimited {
                retry_after_secs: None,
            },
            BedrockErrorClass::Overloaded => {
                AgentError::LlmApi(format!("Bedrock model overloaded: {body}"))
            }
            BedrockErrorClass::Unknown if status == 429 => AgentError::RateLimited {
                retry_after_secs: None,
            },
            BedrockErrorClass::Unknown => {
                AgentError::LlmApi(format!("Bedrock API error {status}: {body}"))
            }
        }
    }
}

fn resolve_bedrock_auth() -> Option<BedrockAuth> {
    std::env::var("AWS_BEARER_TOKEN_BEDROCK")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(BedrockAuth::Bearer)
        .or_else(|| resolve_env_credentials().map(BedrockAuth::SigV4))
        .or_else(|| resolve_shared_credentials().map(BedrockAuth::SigV4))
}

fn resolve_env_credentials() -> Option<AwsCredentials> {
    let access_key_id = std::env::var("AWS_ACCESS_KEY_ID")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())?;
    let secret_access_key = std::env::var("AWS_SECRET_ACCESS_KEY")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())?;
    let session_token = std::env::var("AWS_SESSION_TOKEN")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    Some(AwsCredentials {
        access_key_id,
        secret_access_key,
        session_token,
    })
}

fn resolve_shared_credentials() -> Option<AwsCredentials> {
    let path = aws_shared_credentials_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let profile = aws_profile_name();
    let values = parse_ini_section(&raw, &profile);
    let access_key_id = values.get("aws_access_key_id")?.trim().to_string();
    let secret_access_key = values.get("aws_secret_access_key")?.trim().to_string();
    if access_key_id.is_empty() || secret_access_key.is_empty() {
        return None;
    }
    let session_token = values
        .get("aws_session_token")
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    Some(AwsCredentials {
        access_key_id,
        secret_access_key,
        session_token,
    })
}

fn resolve_region_from_aws_config() -> Option<String> {
    let path = aws_config_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let profile = aws_profile_name();
    parse_ini_section(&raw, &profile)
        .get("region")
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn aws_profile_name() -> String {
    std::env::var("AWS_PROFILE")
        .or_else(|_| std::env::var("AWS_DEFAULT_PROFILE"))
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "default".to_string())
}

fn aws_shared_credentials_path() -> Option<PathBuf> {
    std::env::var("AWS_SHARED_CREDENTIALS_FILE")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".aws").join("credentials")))
}

fn aws_config_path() -> Option<PathBuf> {
    std::env::var("AWS_CONFIG_FILE")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".aws").join("config")))
}

fn parse_ini_section(raw: &str, profile: &str) -> HashMap<String, String> {
    let mut current_matches = false;
    let mut out = HashMap::new();
    let profile_section = if profile == "default" {
        "default".to_string()
    } else {
        format!("profile {profile}")
    };
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let section = trimmed.trim_start_matches('[').trim_end_matches(']').trim();
            current_matches = section == profile || section == profile_section;
            continue;
        }
        if !current_matches {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            out.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    out
}

#[derive(Clone, Copy)]
struct BedrockHeaderRequest<'a> {
    method: &'a str,
    url: &'a str,
    region: &'a str,
    service: &'a str,
    body: &'a [u8],
    anthropic_beta: Option<&'a str>,
    now: DateTime<Utc>,
}

fn bedrock_request_headers(
    request: BedrockHeaderRequest<'_>,
    auth: &BedrockAuth,
) -> Result<BTreeMap<String, String>, AgentError> {
    let mut headers = BTreeMap::new();
    headers.insert("accept".to_string(), "application/json".to_string());
    if request.method != "GET" {
        headers.insert("content-type".to_string(), "application/json".to_string());
    }
    if let Some(beta) = request
        .anthropic_beta
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        headers.insert("anthropic-beta".to_string(), beta.to_string());
    }
    match auth {
        BedrockAuth::Bearer(token) => {
            headers.insert("authorization".to_string(), format!("Bearer {token}"));
            Ok(headers)
        }
        BedrockAuth::SigV4(credentials) => sign_sigv4_headers(request, credentials, headers),
    }
}

fn sign_sigv4_headers(
    request: BedrockHeaderRequest<'_>,
    credentials: &AwsCredentials,
    mut headers: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, AgentError> {
    let url = reqwest::Url::parse(request.url)
        .map_err(|err| AgentError::Config(format!("invalid Bedrock URL for SigV4: {err}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| AgentError::Config("Bedrock SigV4 URL missing host".to_string()))?;
    let amz_date = request.now.format("%Y%m%dT%H%M%SZ").to_string();
    let short_date = request.now.format("%Y%m%d").to_string();
    let payload_hash = hex::encode(Sha256::digest(request.body));

    headers.insert("host".to_string(), host.to_string());
    headers.insert("x-amz-date".to_string(), amz_date.clone());
    headers.insert("x-amz-content-sha256".to_string(), payload_hash.clone());
    if let Some(token) = credentials
        .session_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        headers.insert("x-amz-security-token".to_string(), token.to_string());
    }

    let canonical_headers = headers
        .iter()
        .map(|(key, value)| format!("{}:{}\n", key.to_ascii_lowercase(), collapse_spaces(value)))
        .collect::<String>();
    let signed_headers = headers
        .keys()
        .map(|key| key.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_query = canonical_query_string(&url);
    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        request.method.to_ascii_uppercase(),
        canonical_uri(&url),
        canonical_query,
        canonical_headers,
        signed_headers,
        payload_hash
    );
    let scope = format!(
        "{}/{}/{}/aws4_request",
        short_date,
        normalized_region_or_default(request.region),
        request.service
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        amz_date,
        scope,
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    let signing_key = sigv4_signing_key(
        credentials.secret_access_key.as_bytes(),
        short_date.as_bytes(),
        normalized_region_or_default(request.region).as_bytes(),
        request.service.as_bytes(),
    )?;
    let signature = hmac_sha256_hex(&signing_key, string_to_sign.as_bytes())?;
    headers.insert(
        "authorization".to_string(),
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            credentials.access_key_id, scope, signed_headers, signature
        ),
    );
    Ok(headers)
}

fn sigv4_signing_key(
    secret: &[u8],
    date: &[u8],
    region: &[u8],
    service: &[u8],
) -> Result<Vec<u8>, AgentError> {
    let k_secret = [b"AWS4".as_slice(), secret].concat();
    let k_date = hmac_sha256(&k_secret, date)?;
    let k_region = hmac_sha256(&k_date, region)?;
    let k_service = hmac_sha256(&k_region, service)?;
    hmac_sha256(&k_service, b"aws4_request")
}

fn hmac_sha256(key: &[u8], value: &[u8]) -> Result<Vec<u8>, AgentError> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|err| AgentError::Config(format!("SigV4 HMAC init failed: {err}")))?;
    mac.update(value);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hmac_sha256_hex(key: &[u8], value: &[u8]) -> Result<String, AgentError> {
    Ok(hex::encode(hmac_sha256(key, value)?))
}

fn canonical_uri(url: &reqwest::Url) -> String {
    let path = url.path();
    if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    }
}

fn canonical_query_string(url: &reqwest::Url) -> String {
    let mut pairs = url
        .query_pairs()
        .map(|(key, value)| {
            format!(
                "{}={}",
                percent_encode_query_component(&key),
                percent_encode_query_component(&value)
            )
        })
        .collect::<Vec<_>>();
    pairs.sort();
    pairs.join("&")
}

fn collapse_spaces(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalized_region_or_default(region: &str) -> String {
    let trimmed = region.trim();
    if trimmed.is_empty() {
        BEDROCK_DEFAULT_REGION.to_string()
    } else {
        trimmed.to_string()
    }
}

fn anthropic_inference_profile_prefix(region: &str) -> &'static str {
    let region = normalized_region_or_default(region);
    if region.starts_with("eu-") {
        "eu"
    } else if matches!(
        region.as_str(),
        "ap-southeast-2" | "ap-southeast-4" | "ap-southeast-6"
    ) {
        "au"
    } else if matches!(region.as_str(), "ap-northeast-1" | "ap-northeast-3") {
        "jp"
    } else {
        "us"
    }
}

fn amazon_inference_profile_prefix(region: &str) -> &'static str {
    let region = normalized_region_or_default(region);
    if region.starts_with("eu-") {
        "eu"
    } else {
        "us"
    }
}

fn percent_encode_path_segment(input: &str) -> String {
    percent_encode_bytes(input.as_bytes(), false)
}

fn percent_encode_query_component(input: &str) -> String {
    percent_encode_bytes(input.as_bytes(), true)
}

fn percent_encode_bytes(input: &[u8], encode_tilde: bool) -> String {
    let mut out = String::new();
    for &byte in input {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.')
            || (!encode_tilde && byte == b'~');
        if keep {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn truncate_json(value: &Value, max_chars: usize) -> String {
    let raw = value.to_string();
    if raw.chars().count() <= max_chars {
        raw
    } else {
        raw.chars().take(max_chars).collect::<String>() + "..."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::JsonSchema;
    use std::sync::OnceLock;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::Mutex as AsyncMutex;

    fn encode_event_stream_message(event_type: &str, payload: Value) -> Vec<u8> {
        let payload = serde_json::to_vec(&payload).expect("payload JSON");
        let mut headers = Vec::new();
        push_event_stream_string_header(&mut headers, ":message-type", "event");
        push_event_stream_string_header(&mut headers, ":event-type", event_type);
        push_event_stream_string_header(&mut headers, ":content-type", "application/json");
        let total_len = 16 + headers.len() + payload.len();
        let mut frame = Vec::with_capacity(total_len);
        frame.extend_from_slice(&(total_len as u32).to_be_bytes());
        frame.extend_from_slice(&(headers.len() as u32).to_be_bytes());
        frame.extend_from_slice(&crc32_ieee(&frame[..8]).to_be_bytes());
        frame.extend_from_slice(&headers);
        frame.extend_from_slice(&payload);
        frame.extend_from_slice(&crc32_ieee(&frame).to_be_bytes());
        frame
    }

    fn push_event_stream_string_header(out: &mut Vec<u8>, name: &str, value: &str) {
        out.push(name.len() as u8);
        out.extend_from_slice(name.as_bytes());
        out.push(7);
        out.extend_from_slice(&(value.len() as u16).to_be_bytes());
        out.extend_from_slice(value.as_bytes());
    }

    fn bedrock_env_lock() -> &'static AsyncMutex<()> {
        static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| AsyncMutex::new(()))
    }

    struct ScopedEnv {
        key: &'static str,
        previous: Option<String>,
    }

    impl ScopedEnv {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for ScopedEnv {
        fn drop(&mut self) {
            if let Some(value) = self.previous.as_ref() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn build_converse_body_maps_messages_tools_and_1m_beta() {
        let tools = vec![ToolSchema::new(
            "terminal",
            "Run commands",
            JsonSchema::new("object"),
        )];
        let body = build_converse_body(
            "global.anthropic.claude-opus-4-7",
            &[Message::system("system"), Message::user("hello")],
            &tools,
            Some(8192),
            Some(0.2),
            None,
        );
        assert_eq!(body["system"][0]["text"], "system");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["inferenceConfig"]["maxTokens"], 8192);
        assert_eq!(
            body["toolConfig"]["tools"][0]["toolSpec"]["name"],
            "terminal"
        );
        let betas = body["additionalModelRequestFields"]["anthropic_beta"]
            .as_array()
            .expect("anthropic betas");
        assert!(betas.iter().any(|v| v == CONTEXT_1M_BETA));
    }

    #[test]
    fn build_converse_body_passes_top_p_guardrails_and_strips_unsupported_tools() {
        let tools = vec![ToolSchema::new("test", "Test", JsonSchema::new("object"))];
        let body = build_converse_body(
            "us.deepseek.r1-v1:0",
            &[Message::user("hello")],
            &tools,
            None,
            Some(0.7),
            Some(&json!({
                "top_p": 0.9,
                "guardrail_config": {
                    "guardrailIdentifier": "gr-123",
                    "guardrailVersion": "1"
                }
            })),
        );
        assert_eq!(body["inferenceConfig"]["temperature"], 0.7);
        assert_eq!(body["inferenceConfig"]["topP"], 0.9);
        assert_eq!(body["guardrailConfig"]["guardrailIdentifier"], "gr-123");
        assert!(body.get("toolConfig").is_none());
    }

    #[test]
    fn convert_messages_merges_roles_and_enforces_user_boundaries() {
        let messages = vec![
            Message::user("first"),
            Message::user("second"),
            Message::assistant("part 1"),
            Message::assistant("part 2"),
        ];
        let (_system, converted) = convert_messages_to_bedrock(&messages);
        assert_eq!(converted.first().unwrap()["role"], "user");
        assert_eq!(converted.last().unwrap()["role"], "user");
        let user_messages = converted
            .iter()
            .filter(|message| message["role"] == "user")
            .count();
        let assistant_messages = converted
            .iter()
            .filter(|message| message["role"] == "assistant")
            .count();
        assert_eq!(user_messages, 2);
        assert_eq!(assistant_messages, 1);
        let assistant_text = converted[1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert!(assistant_text.contains(&"part 1"));
        assert!(assistant_text.contains(&"part 2"));
    }

    #[test]
    fn convert_messages_decodes_acp_multimodal_data_url_and_empty_placeholder() {
        let parts = json!([
            {"type": "text", "text": "what is here"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgo="}}
        ]);
        let marker = format!("{ACP_MULTIMODAL_PREFIX}{parts}");
        let (_system, converted) =
            convert_messages_to_bedrock(&[Message::user(marker), Message::user("   ")]);
        let blocks = converted[0]["content"].as_array().expect("content blocks");
        assert!(blocks.iter().any(|block| block["text"] == "what is here"));
        let image = blocks
            .iter()
            .find_map(|block| block.get("image"))
            .expect("image block");
        assert_eq!(image["format"], "png");
        assert_eq!(image["source"]["bytes"], "iVBORw0KGgo=");
        assert!(blocks.iter().any(|block| block["text"] == " "));
    }

    #[test]
    fn convert_tool_schema_defaults_empty_parameters_to_object_schema() {
        let tools = vec![ToolSchema::new(
            "noop",
            "No-op",
            JsonSchema {
                schema_type: None,
                properties: None,
                required: None,
                additional_properties: None,
            },
        )];
        let converted = convert_tools_to_bedrock(&tools);
        assert_eq!(
            converted[0]["toolSpec"]["inputSchema"]["json"],
            json!({"type": "object", "properties": {}})
        );
    }

    #[test]
    fn parse_bedrock_response_preserves_text_tool_reasoning_and_usage() {
        let raw = json!({
            "output": {
                "message": {
                    "role": "assistant",
                    "content": [
                        {"reasoningContent": {"text": "Let me think..."}},
                        {"text": "Answer."},
                        {"toolUse": {
                            "toolUseId": "tool_1",
                            "name": "terminal",
                            "input": {"command": "ls"}
                        }}
                    ]
                }
            },
            "stopReason": "tool_use",
            "usage": {"inputTokens": 10, "outputTokens": 5, "totalTokens": 15}
        });
        let response = parse_bedrock_response(&raw, "anthropic.claude").expect("response");
        assert_eq!(response.message.content.as_deref(), Some("Answer."));
        assert_eq!(
            response.message.reasoning_content.as_deref(),
            Some("Let me think...")
        );
        assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(response.usage.expect("usage").total_tokens, 15);
        let calls = response.message.tool_calls.expect("tool calls");
        assert_eq!(calls[0].function.name, "terminal");
        assert_eq!(calls[0].function.arguments, r#"{"command":"ls"}"#);
    }

    #[test]
    fn parse_bedrock_response_handles_empty_content_and_tool_finish_override() {
        let empty = json!({
            "output": {"message": {"role": "assistant", "content": []}},
            "stopReason": "end_turn",
            "usage": {"inputTokens": 1, "outputTokens": 0}
        });
        let response = parse_bedrock_response(&empty, "anthropic.claude").expect("empty response");
        assert_eq!(response.message.content, None);
        assert_eq!(response.message.tool_calls, None);
        assert_eq!(response.finish_reason.as_deref(), Some("stop"));

        let tool = json!({
            "output": {
                "message": {
                    "role": "assistant",
                    "content": [{"toolUse": {"toolUseId": "c1", "name": "search", "input": {}}}]
                }
            },
            "stopReason": "end_turn",
            "usage": {"inputTokens": 1, "outputTokens": 1}
        });
        let response = parse_bedrock_response(&tool, "anthropic.claude").expect("tool response");
        assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(
            response.message.tool_calls.unwrap()[0].function.arguments,
            "{}"
        );
    }

    #[test]
    fn parse_bedrock_stream_events_collects_text_tool_reasoning_and_usage() {
        let raw = json!({
            "stream": [
                {"messageStart": {"role": "assistant"}},
                {"contentBlockDelta": {"contentBlockIndex": 0, "delta": {"text": "Hello"}}},
                {"contentBlockDelta": {"contentBlockIndex": 0, "delta": {"text": ", world"}}},
                {"contentBlockDelta": {"contentBlockIndex": 1, "delta": {
                    "reasoningContent": {"text": "thinking"}
                }}},
                {"contentBlockStart": {"contentBlockIndex": 2, "start": {
                    "toolUse": {"toolUseId": "call_1", "name": "read_file"}
                }}},
                {"contentBlockDelta": {"contentBlockIndex": 2, "delta": {
                    "toolUse": {"input": "{\"path\":"}
                }}},
                {"contentBlockDelta": {"contentBlockIndex": 2, "delta": {
                    "toolUse": {"input": "\"/tmp/f\"}"}
                }}},
                {"messageStop": {"stopReason": "end_turn"}},
                {"metadata": {"usage": {"inputTokens": 5, "outputTokens": 3}}}
            ]
        });
        let response = parse_bedrock_stream_events(&raw, "anthropic.claude").expect("stream");
        assert_eq!(response.message.content.as_deref(), Some("Hello, world"));
        assert_eq!(
            response.message.reasoning_content.as_deref(),
            Some("thinking")
        );
        assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(response.usage.expect("usage").total_tokens, 8);
        let call = &response.message.tool_calls.unwrap()[0];
        assert_eq!(call.id, "call_1");
        assert_eq!(call.function.name, "read_file");
        assert_eq!(call.function.arguments, r#"{"path":"/tmp/f"}"#);
    }

    #[test]
    fn aws_event_stream_decoder_maps_bedrock_events_to_chunks() {
        let frames = [
            encode_event_stream_message(
                "contentBlockDelta",
                json!({"contentBlockDelta": {"contentBlockIndex": 0, "delta": {"text": "Hello"}}}),
            ),
            encode_event_stream_message(
                "contentBlockDelta",
                json!({"contentBlockDelta": {"contentBlockIndex": 1, "delta": {
                    "reasoningContent": {"text": "thinking"}
                }}}),
            ),
            encode_event_stream_message(
                "contentBlockStart",
                json!({"contentBlockStart": {"contentBlockIndex": 2, "start": {
                    "toolUse": {"toolUseId": "tool_1", "name": "read_file"}
                }}}),
            ),
            encode_event_stream_message(
                "contentBlockDelta",
                json!({"contentBlockDelta": {"contentBlockIndex": 2, "delta": {
                    "toolUse": {"input": "{\"path\":\"/tmp/f\"}"}
                }}}),
            ),
            encode_event_stream_message(
                "metadata",
                json!({"metadata": {"usage": {"inputTokens": 5, "outputTokens": 3}}}),
            ),
            encode_event_stream_message(
                "messageStop",
                json!({"messageStop": {"stopReason": "end_turn"}}),
            ),
        ];
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&frames[0][..frames[0].len() / 2]);
        assert!(take_aws_event_stream_message(&mut buffer)
            .expect("partial frame")
            .is_none());
        buffer.extend_from_slice(&frames[0][frames[0].len() / 2..]);
        for frame in frames.iter().skip(1) {
            buffer.extend_from_slice(frame);
        }

        let mut chunks = Vec::new();
        while let Some(message) =
            take_aws_event_stream_message(&mut buffer).expect("event stream frame")
        {
            let event = decode_bedrock_event_stream_message(&message)
                .expect("bedrock event")
                .expect("nonempty event");
            chunks.extend(bedrock_stream_event_to_chunks(&event).expect("chunks"));
        }

        assert!(buffer.is_empty());
        assert!(chunks.iter().any(|chunk| {
            chunk
                .delta
                .as_ref()
                .and_then(|delta| delta.content.as_deref())
                == Some("Hello")
        }));
        assert!(chunks.iter().any(|chunk| {
            chunk
                .delta
                .as_ref()
                .and_then(|delta| delta.extra.as_ref())
                .and_then(|extra| extra.get("thinking"))
                .and_then(Value::as_str)
                == Some("thinking")
        }));
        let tool_delta = chunks
            .iter()
            .filter_map(|chunk| chunk.delta.as_ref())
            .filter_map(|delta| delta.tool_calls.as_ref())
            .flat_map(|calls| calls.iter())
            .find(|call| {
                call.function.as_ref().and_then(|f| f.name.as_deref()) == Some("read_file")
            })
            .expect("tool start delta");
        assert_eq!(tool_delta.index, 2);
        assert_eq!(tool_delta.id.as_deref(), Some("tool_1"));
        assert!(chunks.iter().any(|chunk| {
            chunk
                .delta
                .as_ref()
                .and_then(|delta| delta.tool_calls.as_ref())
                .and_then(|calls| calls.first())
                .and_then(|call| call.function.as_ref())
                .and_then(|function| function.arguments.as_deref())
                == Some(r#"{"path":"/tmp/f"}"#)
        }));
        assert_eq!(
            chunks
                .iter()
                .find_map(|chunk| chunk.usage.as_ref())
                .unwrap()
                .total_tokens,
            8
        );
        assert_eq!(
            chunks
                .iter()
                .find_map(|chunk| chunk.finish_reason.as_deref()),
            Some("stop")
        );
    }

    #[test]
    fn aws_event_stream_decoder_rejects_bad_crc() {
        let mut frame = encode_event_stream_message("metadata", json!({"metadata": {"usage": {}}}));
        let last = frame.len() - 1;
        frame[last] ^= 0xff;
        let mut buffer = frame;
        let err = take_aws_event_stream_message(&mut buffer).expect_err("CRC failure");
        assert!(matches!(err, AgentError::LlmApi(message) if message.contains("checksum")));
    }

    #[test]
    fn crc32_ieee_matches_standard_check_value() {
        assert_eq!(crc32_ieee(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn aws_event_stream_decoder_maps_payload_exceptions() {
        let mut buffer = encode_event_stream_message(
            "validationException",
            json!({"validationException": {"message": "bad input"}}),
        );
        let message = take_aws_event_stream_message(&mut buffer)
            .expect("event frame")
            .expect("complete frame");
        let err =
            decode_bedrock_event_stream_message(&message).expect_err("stream exception error");
        assert!(matches!(err, AgentError::LlmApi(message) if message.contains("400")));
    }

    #[tokio::test]
    async fn bedrock_chat_completion_stream_uses_converse_stream_transport() {
        let _lock = bedrock_env_lock().lock().await;
        let _token = ScopedEnv::set("AWS_BEARER_TOKEN_BEDROCK", "test-token");
        let body = [
            encode_event_stream_message(
                "contentBlockStart",
                json!({"contentBlockStart": {"contentBlockIndex": 0, "start": {
                    "toolUse": {"toolUseId": "call_1", "name": "read_file"}
                }}}),
            ),
            encode_event_stream_message(
                "contentBlockDelta",
                json!({"contentBlockDelta": {"contentBlockIndex": 0, "delta": {
                    "toolUse": {"input": "{\"path\":\"/tmp/f\"}"}
                }}}),
            ),
            encode_event_stream_message(
                "messageStop",
                json!({"messageStop": {"stopReason": "end_turn"}}),
            ),
            encode_event_stream_message(
                "metadata",
                json!({"metadata": {"usage": {"inputTokens": 2, "outputTokens": 4}}}),
            ),
        ]
        .concat();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock Bedrock");
        let addr = listener.local_addr().expect("mock address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept request");
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                let n = socket.read(&mut buf).await.expect("read request");
                assert!(n > 0, "client closed before headers");
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let header_end = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .expect("request headers")
                + 4;
            let headers = String::from_utf8_lossy(&request[..header_end]).to_string();
            let content_len = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or_default();
            while request.len().saturating_sub(header_end) < content_len {
                let n = socket.read(&mut buf).await.expect("read request body");
                assert!(n > 0, "client closed before body");
                request.extend_from_slice(&buf[..n]);
            }
            assert!(
                headers.starts_with("POST /model/anthropic.claude/converse-stream HTTP/1.1"),
                "unexpected request line: {headers}"
            );
            assert!(
                headers
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case("authorization: Bearer test-token")),
                "missing bearer authorization: {headers}"
            );
            let response_headers = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/vnd.amazon.eventstream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            socket
                .write_all(response_headers.as_bytes())
                .await
                .expect("write response headers");
            socket.write_all(&body).await.expect("write response body");
        });

        let provider = BedrockProvider::new()
            .with_region("us-east-1")
            .with_model("anthropic.claude")
            .with_base_url(format!("http://{addr}"));
        let chunks = provider
            .chat_completion_stream(&[Message::user("hello")], &[], None, None, None, None)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("stream chunks");
        server.await.expect("mock server");

        assert!(chunks.iter().any(|chunk| {
            chunk
                .delta
                .as_ref()
                .and_then(|delta| delta.tool_calls.as_ref())
                .and_then(|calls| calls.first())
                .and_then(|call| call.function.as_ref())
                .and_then(|function| function.name.as_deref())
                == Some("read_file")
        }));
        assert!(chunks.iter().any(|chunk| {
            chunk
                .delta
                .as_ref()
                .and_then(|delta| delta.tool_calls.as_ref())
                .and_then(|calls| calls.first())
                .and_then(|call| call.function.as_ref())
                .and_then(|function| function.arguments.as_deref())
                == Some(r#"{"path":"/tmp/f"}"#)
        }));
        assert_eq!(
            chunks
                .iter()
                .find_map(|chunk| chunk.finish_reason.as_deref()),
            Some("tool_calls")
        );
        assert_eq!(
            chunks
                .iter()
                .find_map(|chunk| chunk.usage.as_ref())
                .unwrap()
                .total_tokens,
            6
        );
    }

    #[test]
    fn finish_reason_mapping_matches_bedrock_transport_contract() {
        assert_eq!(
            map_bedrock_finish_reason(Some("end_turn")).as_deref(),
            Some("stop")
        );
        assert_eq!(
            map_bedrock_finish_reason(Some("stop_sequence")).as_deref(),
            Some("stop")
        );
        assert_eq!(
            map_bedrock_finish_reason(Some("tool_use")).as_deref(),
            Some("tool_calls")
        );
        assert_eq!(
            map_bedrock_finish_reason(Some("max_tokens")).as_deref(),
            Some("length")
        );
        assert_eq!(
            map_bedrock_finish_reason(Some("guardrail_intervened")).as_deref(),
            Some("content_filter")
        );
        assert_eq!(
            map_bedrock_finish_reason(Some("content_filtered")).as_deref(),
            Some("content_filter")
        );
        assert_eq!(
            map_bedrock_finish_reason(Some("unknown")).as_deref(),
            Some("stop")
        );
    }

    #[test]
    fn catalog_parser_accepts_foundation_models_and_inference_profiles() {
        let raw = json!({
            "modelSummaries": [
                {"modelId": "anthropic.claude-3-5-sonnet-20241022-v2:0"}
            ],
            "inferenceProfileSummaries": [
                {"inferenceProfileId": "eu.anthropic.claude-sonnet-4-6"}
            ]
        });
        let ids = parse_bedrock_catalog_model_ids(&raw);
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().any(|id| id.starts_with("eu.anthropic.")));
    }

    #[test]
    fn catalog_parser_filters_unsupported_models_and_sorts_global_profiles_first() {
        let raw = json!({
            "modelSummaries": [
                {
                    "modelId": "old-model",
                    "outputModalities": ["TEXT"],
                    "responseStreamingSupported": true,
                    "modelLifecycle": {"status": "LEGACY"}
                },
                {
                    "modelId": "embed-model",
                    "outputModalities": ["EMBEDDING"],
                    "responseStreamingSupported": false,
                    "modelLifecycle": {"status": "ACTIVE"}
                },
                {
                    "modelId": "anthropic.claude-v2",
                    "outputModalities": ["TEXT"],
                    "responseStreamingSupported": true,
                    "modelLifecycle": {"status": "ACTIVE"}
                }
            ],
            "inferenceProfileSummaries": [
                {"inferenceProfileId": "us.anthropic.claude-v2", "status": "ACTIVE"},
                {"inferenceProfileId": "global.anthropic.claude-v2", "status": "ACTIVE"}
            ]
        });
        let ids = parse_bedrock_catalog_model_ids(&raw);
        assert_eq!(
            ids.first().map(String::as_str),
            Some("global.anthropic.claude-v2")
        );
        assert!(ids.iter().any(|id| id == "anthropic.claude-v2"));
        assert!(!ids.iter().any(|id| id == "old-model"));
        assert!(!ids.iter().any(|id| id == "embed-model"));
    }

    #[test]
    fn bedrock_context_tool_support_and_error_helpers_match_adapter_policy() {
        assert_eq!(
            get_bedrock_context_length("us.anthropic.claude-sonnet-4-6"),
            200_000
        );
        assert_eq!(get_bedrock_context_length("amazon.nova-pro-v1:0"), 300_000);
        assert_eq!(
            get_bedrock_context_length("amazon.nova-micro-v1:0"),
            128_000
        );
        assert_eq!(
            get_bedrock_context_length("unknown.model-v1:0"),
            BEDROCK_DEFAULT_CONTEXT_LENGTH
        );
        assert!(model_supports_bedrock_tool_use(
            "us.anthropic.claude-sonnet-4-6"
        ));
        assert!(model_supports_bedrock_tool_use("deepseek.v3.2"));
        assert!(!model_supports_bedrock_tool_use("us.deepseek.r1-v1:0"));
        assert!(!model_supports_bedrock_tool_use(
            "stability.stable-diffusion-xl"
        ));
        assert!(!model_supports_bedrock_tool_use("cohere.embed-v4"));
        assert_eq!(
            classify_bedrock_error("ValidationException: input is too long").as_str(),
            "context_overflow"
        );
        assert_eq!(
            classify_bedrock_error("Too many concurrent requests").as_str(),
            "rate_limit"
        );
        assert_eq!(
            classify_bedrock_error("ModelTimeoutException").as_str(),
            "overloaded"
        );
        assert_eq!(
            classify_bedrock_error("SomeRandomError").as_str(),
            "unknown"
        );
    }

    #[test]
    fn anthropic_detector_accepts_regional_inference_profile_prefixes() {
        assert!(is_bedrock_anthropic_model_id(
            "au.anthropic.claude-sonnet-4-6"
        ));
        assert!(is_bedrock_anthropic_model_id(
            "jp.anthropic.claude-sonnet-4-6"
        ));
        assert!(is_bedrock_anthropic_model_id(
            "apac.anthropic.claude-sonnet-4-6"
        ));
        assert!(!is_bedrock_anthropic_model_id("us.amazon.nova-pro-v1:0"));
    }

    #[test]
    fn sigv4_headers_include_required_bedrock_fields() {
        let creds = AwsCredentials {
            access_key_id: "AKIDEXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: Some("session".to_string()),
        };
        let auth = BedrockAuth::SigV4(creds);
        let headers = bedrock_request_headers(
            BedrockHeaderRequest {
                method: "POST",
                url: "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude%3A0/converse",
                region: "us-east-1",
                service: "bedrock",
                body: br#"{"messages":[]}"#,
                anthropic_beta: Some(CONTEXT_1M_BETA),
                now: DateTime::parse_from_rfc3339("2026-05-30T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
            &auth,
        )
        .expect("headers");
        assert_eq!(
            headers.get("x-amz-date").map(String::as_str),
            Some("20260530T000000Z")
        );
        assert_eq!(
            headers.get("x-amz-security-token").map(String::as_str),
            Some("session")
        );
        assert!(headers.get("authorization").expect("auth").starts_with(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260530/us-east-1/bedrock/aws4_request"
        ));
        assert_eq!(
            headers.get("anthropic-beta").map(String::as_str),
            Some(CONTEXT_1M_BETA)
        );
    }
}
