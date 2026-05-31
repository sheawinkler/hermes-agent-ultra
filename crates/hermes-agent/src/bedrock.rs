//! AWS Bedrock Converse provider and catalog helpers.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::{DateTime, Utc};
use futures::stream::BoxStream;
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use hermes_core::{
    AgentError, FunctionCall, LlmProvider, LlmResponse, Message, MessageRole, StreamChunk,
    StreamDelta, ToolCall, ToolSchema, UsageStats,
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
        for (key, value) in bedrock_request_headers(
            "POST",
            url.as_str(),
            &self.region,
            "bedrock",
            &body_bytes,
            &auth,
            bedrock_anthropic_beta_header(effective_model).as_deref(),
            Utc::now(),
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
        Box::pin(async_stream::stream! {
            match provider
                .chat_completion(
                    &messages,
                    &tools,
                    max_tokens,
                    temperature,
                    model.as_deref(),
                    extra_body.as_ref(),
                )
                .await
            {
                Ok(response) => {
                    if let Some(content) = response.message.content.clone().filter(|s| !s.is_empty()) {
                        yield Ok(StreamChunk {
                            delta: Some(StreamDelta {
                                content: Some(content),
                                tool_calls: None,
                                extra: None,
                            }),
                            finish_reason: None,
                            usage: None,
                        });
                    }
                    yield Ok(StreamChunk {
                        delta: None,
                        finish_reason: response.finish_reason,
                        usage: response.usage,
                    });
                }
                Err(err) => yield Err(err),
            }
        })
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
    let headers =
        match bedrock_request_headers("GET", url, region, "bedrock", b"", auth, None, Utc::now()) {
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

fn bedrock_request_headers(
    method: &str,
    url: &str,
    region: &str,
    service: &str,
    body: &[u8],
    auth: &BedrockAuth,
    anthropic_beta: Option<&str>,
    now: DateTime<Utc>,
) -> Result<BTreeMap<String, String>, AgentError> {
    let mut headers = BTreeMap::new();
    headers.insert("accept".to_string(), "application/json".to_string());
    if method != "GET" {
        headers.insert("content-type".to_string(), "application/json".to_string());
    }
    if let Some(beta) = anthropic_beta.map(str::trim).filter(|s| !s.is_empty()) {
        headers.insert("anthropic-beta".to_string(), beta.to_string());
    }
    match auth {
        BedrockAuth::Bearer(token) => {
            headers.insert("authorization".to_string(), format!("Bearer {token}"));
            Ok(headers)
        }
        BedrockAuth::SigV4(credentials) => sign_sigv4_headers(
            method,
            url,
            region,
            service,
            body,
            credentials,
            headers,
            now,
        ),
    }
}

fn sign_sigv4_headers(
    method: &str,
    url: &str,
    region: &str,
    service: &str,
    body: &[u8],
    credentials: &AwsCredentials,
    mut headers: BTreeMap<String, String>,
    now: DateTime<Utc>,
) -> Result<BTreeMap<String, String>, AgentError> {
    let url = reqwest::Url::parse(url)
        .map_err(|err| AgentError::Config(format!("invalid Bedrock URL for SigV4: {err}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| AgentError::Config("Bedrock SigV4 URL missing host".to_string()))?;
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let short_date = now.format("%Y%m%d").to_string();
    let payload_hash = hex::encode(Sha256::digest(body));

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
        method.to_ascii_uppercase(),
        canonical_uri(&url),
        canonical_query,
        canonical_headers,
        signed_headers,
        payload_hash
    );
    let scope = format!(
        "{}/{}/{}/aws4_request",
        short_date,
        normalized_region_or_default(region),
        service
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
        normalized_region_or_default(region).as_bytes(),
        service.as_bytes(),
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
            "POST",
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude%3A0/converse",
            "us-east-1",
            "bedrock",
            br#"{"messages":[]}"#,
            &auth,
            Some(CONTEXT_1M_BETA),
            DateTime::parse_from_rfc3339("2026-05-30T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
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
