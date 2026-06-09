//! LLM-backed executor that calls MiniMax (or any OpenAI-compatible API).
//!
//! Used by the standalone test example to give Cherry real responses.

use super::{PipeSession, PromptExecutor, PromptResult, StreamContent, StreamEvent};
use async_trait::async_trait;
use hermes_acp::protocol::StopReason;
use serde_json::Value;

pub struct LlmExecutor {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub client: reqwest::Client,
}

impl LlmExecutor {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        let timeout = std::time::Duration::from_secs(120);
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    "reqwest Client builder failed, falling back to default (timeout preserved)"
                );
                // Re-apply timeout to the fallback client so we never hang indefinitely.
                reqwest::Client::builder()
                    .timeout(timeout)
                    .build()
                    .expect("default reqwest Client must be constructable")
            });
        Self {
            api_key,
            base_url,
            model,
            client,
        }
    }

    /// Load LLM config from `~/.hermes/config.yaml`.
    /// Tries providers in order: custom, minimax, openai, openrouter.
    pub async fn from_hermes_config() -> Option<Self> {
        let home = dirs::home_dir()?;
        let config_path = home.join(".hermes").join("config.yaml");
        let content = tokio::fs::read_to_string(&config_path).await.ok()?;
        let config: serde_yaml::Value = tokio::task::spawn_blocking(move || {
            serde_yaml::from_str::<serde_yaml::Value>(&content)
        })
        .await
        .ok()
        .and_then(|r| r.ok())?;

        let providers = config.get("llm_providers")?;
        for key in &["custom", "minimax", "openai", "openrouter"] {
            if let Some(llm) = providers.get(key) {
                let api_key = match llm.get("api_key").and_then(|v| v.as_str()) {
                    Some(k) if !k.is_empty() => k.to_string(),
                    _ => continue,
                };
                let base_url = match llm.get("base_url").and_then(|v| v.as_str()) {
                    Some(u) if !u.is_empty() => u.to_string(),
                    _ => continue,
                };
                let model = match llm.get("model").and_then(|v| v.as_str()) {
                    Some(m) if !m.is_empty() => m.to_string(),
                    _ => continue,
                };
                tracing::info!(provider = key, "Loaded LLM config");
                return Some(Self::new(api_key, base_url, model));
            }
        }
        None
    }
}

#[async_trait]
impl PromptExecutor for LlmExecutor {
    async fn execute(
        &self,
        _session: &PipeSession,
        prompt_text: &str,
        history: &[Value],
        event_tx: tokio::sync::mpsc::Sender<StreamEvent>,
    ) -> Result<PromptResult, String> {
        let mut messages = Vec::new();
        // Keep a sliding window of recent history to avoid unbounded growth.
        let max_history = 50;
        let start = history.len().saturating_sub(max_history);
        for msg in &history[start..] {
            messages.push(msg.clone());
        }
        messages.push(serde_json::json!({
            "role": "user",
            "content": prompt_text
        }));

        let body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "max_tokens": 4096
        });

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("API error {}: {}", status, text));
        }

        // Parse SSE stream -- single-line data fields only (no multi-line data values).
        let mut stream = response.bytes_stream();
        use futures::StreamExt;
        let mut buffer = String::new();
        let mut assistant_text = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("Stream error: {}", e))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim().to_string();
                buffer = buffer[pos + 1..].to_string();

                if line.is_empty() || line == "data: [DONE]" {
                    continue;
                }

                let Some(data) = line.strip_prefix("data: ") else {
                    continue;
                };
                let Ok(parsed) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                let Some(choices) = parsed.get("choices").and_then(|c| c.as_array()) else {
                    continue;
                };
                for choice in choices {
                    let delta = match choice.get("delta") {
                        Some(d) => d,
                        None => continue,
                    };

                    // MiniMax reasoning/thinking content -> AgentThoughtChunk
                    if let Some(reasoning) = delta
                        .get("reasoning_content")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        && event_tx
                            .send(StreamEvent::AgentThoughtChunk {
                                content: StreamContent::Text {
                                    text: reasoning.to_string(),
                                },
                            })
                            .await
                            .is_err()
                    {
                        return Ok(PromptResult {
                            stop_reason: StopReason::EndTurn,
                            usage: None,
                            assistant_message: if assistant_text.is_empty() {
                                None
                            } else {
                                Some(assistant_text)
                            },
                        });
                    }

                    // Regular response content -> AgentMessageChunk
                    if let Some(content) = delta
                        .get("content")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                    {
                        assistant_text.push_str(content);
                        if event_tx
                            .send(StreamEvent::AgentMessageChunk {
                                content: StreamContent::Text {
                                    text: content.to_string(),
                                },
                            })
                            .await
                            .is_err()
                        {
                            return Ok(PromptResult {
                                stop_reason: StopReason::EndTurn,
                                usage: None,
                                assistant_message: if assistant_text.is_empty() {
                                    None
                                } else {
                                    Some(assistant_text)
                                },
                            });
                        }
                    }
                }
            }
        }

        Ok(PromptResult {
            stop_reason: StopReason::EndTurn,
            usage: None,
            assistant_message: if assistant_text.is_empty() {
                None
            } else {
                Some(assistant_text)
            },
        })
    }
}
