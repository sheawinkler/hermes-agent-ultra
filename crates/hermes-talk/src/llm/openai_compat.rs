use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{Stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::error::Error as _;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::{ChatMessage, LlmClient, StreamItem, ToolCallDelta, ToolDefinition};
use crate::config::LlmConfig;
use crate::error::{DemoError, Result};

pub struct OpenAiCompatClient {
    http: Client,
    cfg: LlmConfig,
}

impl OpenAiCompatClient {
    pub fn new(cfg: LlmConfig) -> Self {
        let http = Client::builder()
            .pool_max_idle_per_host(2)
            .tcp_keepalive(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self { http, cfg }
    }

    pub async fn warmup(&self) -> Result<()> {
        if !self.cfg.warmup_on_start {
            return Ok(());
        }
        let cancel = CancellationToken::new();
        let messages = [ChatMessage {
            role: "user".to_string(),
            content: "hi".to_string(),
            tool_calls: None,
            tool_call_id: None,
        }];
        let mut stream = self.stream_chat(&messages, None, cancel).await?;
        let _ = stream.next().await;
        info!("llm connection warmed up");
        Ok(())
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ApiMessage<'a>>,
    stream: bool,
    max_tokens: u32,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [ToolDefinition]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    extra_body: Option<ExtraBody>,
    chat_template_kwargs: ChatTemplateKwargs,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_budget_tokens: Option<u32>,
}

#[derive(Serialize)]
struct ChatTemplateKwargs {
    enable_thinking: bool,
}

#[derive(Serialize)]
struct ExtraBody {
    enable_thinking: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_budget: Option<u32>,
    reasoning_format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
}

#[derive(Serialize)]
struct ApiMessage<'a> {
    role: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<&'a [ApiToolCall]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
}

#[derive(Serialize)]
struct ApiToolCall {
    id: String,
    r#type: String,
    function: ApiToolCallFunction,
}

#[derive(Serialize)]
struct ApiToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StreamDelta {
    content: Option<String>,
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<StreamToolCallDelta>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StreamToolCallDelta {
    index: u32,
    #[serde(default)]
    #[allow(dead_code)]
    r#type: Option<String>,
    id: Option<String>,
    function: Option<StreamToolCallFunction>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StreamToolCallFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[async_trait]
impl LlmClient for OpenAiCompatClient {
    async fn stream_chat(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolDefinition]>,
        cancel: CancellationToken,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamItem>> + Send>>> {
        let mut api_messages = vec![ApiMessage {
            role: "system",
            content: Some(&self.cfg.system_prompt),
            tool_calls: None,
            tool_call_id: None,
        }];
        for m in messages {
            api_messages.push(api_message(m));
        }

        let url = format!(
            "{}/chat/completions",
            self.cfg.base_url.trim_end_matches('/')
        );
        let body = ChatRequest {
            model: &self.cfg.model,
            messages: api_messages,
            stream: true,
            max_tokens: self.cfg.max_tokens,
            temperature: self.cfg.temperature,
            tools,
            extra_body: if self.cfg.thinking_enabled {
                Some(ExtraBody {
                    enable_thinking: true,
                    thinking_budget: self.cfg.thinking_budget,
                    reasoning_format: "deepseek".to_string(),
                    budget_tokens: self.cfg.thinking_budget,
                    reasoning_effort: self.cfg.reasoning_effort.clone(),
                })
            } else {
                None
            },
            chat_template_kwargs: ChatTemplateKwargs {
                enable_thinking: self.cfg.thinking_enabled,
            },
            thinking_budget_tokens: if self.cfg.thinking_enabled {
                self.cfg.thinking_budget
            } else {
                None
            },
        };

        let resp = self
            .http
            .post(&url)
            .header("X-Hermes-Channel", "talk")
            .header("X-Hermes-User", &self.cfg.user_id)
            .bearer_auth(&self.cfg.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                let mut msg = e.to_string();
                let mut src = e.source();
                while let Some(s) = src {
                    msg.push_str(" => ");
                    msg.push_str(&s.to_string());
                    src = s.source();
                }
                DemoError::Llm(msg)
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(DemoError::Llm(format!("HTTP {status}: {text}")));
        }

        let byte_stream = resp.bytes_stream();
        let stream = async_stream::stream! {
            let mut buffer = String::new();
            futures_util::pin_mut!(byte_stream);
            while let Some(chunk) = byte_stream.next().await {
                if cancel.is_cancelled() {
                    break;
                }
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        yield Err(DemoError::Llm({
                            let mut msg = e.to_string();
                            let mut src = e.source();
                            while let Some(s) = src {
                                msg.push_str(" => ");
                                msg.push_str(&s.to_string());
                                src = s.source();
                            }
                            msg
                        }));
                        break;
                    }
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(pos) = buffer.find("\n\n") {
                    let frame: String = buffer.drain(..pos + 2).collect();
                    for line in frame.lines() {
                    let line = line.trim();
                    if let Some(data) = line.strip_prefix("data:") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            return;
                        }
                        if let Ok(chunk) = serde_json::from_str::<StreamChunk>(data) {
                            for choice in chunk.choices {
                                let content = choice.delta.content.filter(|t| !t.is_empty());
                                let reasoning_content = choice.delta.reasoning_content.filter(|t| !t.is_empty());
                                let tool_calls: Vec<ToolCallDelta> = choice.delta.tool_calls
                                    .map(|deltas| {
                                        deltas.into_iter().map(|d| ToolCallDelta {
                                            index: d.index,
                                            id: d.id,
                                            function_name: d.function.as_ref().and_then(|f| f.name.clone()),
                                            function_arguments: d.function.as_ref().and_then(|f| f.arguments.clone()),
                                        }).collect()
                                    })
                                    .unwrap_or_default();

                                if content.is_some() || reasoning_content.is_some() || !tool_calls.is_empty() {
                                    yield Ok(StreamItem { content, reasoning_content, tool_calls });
                                }
                            }
                        }
                    }
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

fn api_message(m: &ChatMessage) -> ApiMessage<'_> {
    let role = m.role.as_str();
    match role {
        "tool" => ApiMessage {
            role: "tool",
            content: Some(&m.content),
            tool_calls: None,
            tool_call_id: m.tool_call_id.as_deref(),
        },
        "assistant" if m.tool_calls.is_some() => {
            let tcs: Vec<ApiToolCall> = m
                .tool_calls
                .as_ref()
                .unwrap()
                .iter()
                .map(|tc| ApiToolCall {
                    id: tc.id.clone(),
                    r#type: tc.r#type.clone(),
                    function: ApiToolCallFunction {
                        name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    },
                })
                .collect();
            ApiMessage {
                role: "assistant",
                content: if m.content.is_empty() {
                    None
                } else {
                    Some(m.content.as_str())
                },
                tool_calls: Some(Box::leak(tcs.into_boxed_slice())),
                tool_call_id: None,
            }
        }
        _ => ApiMessage {
            role,
            content: Some(&m.content),
            tool_calls: None,
            tool_call_id: None,
        },
    }
}
