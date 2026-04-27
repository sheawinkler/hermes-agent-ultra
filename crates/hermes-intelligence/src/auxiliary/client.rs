//! Main entry point: [`AuxiliaryClient`].
//!
//! `AuxiliaryClient` owns:
//!
//! * a base [`ProviderChain`] — the auto-detect ordering for the host
//! * an [`AuxiliaryConfig`] — per-task overrides loaded from the user's
//!   config file
//! * a registry of explicitly-named providers that can be selected via
//!   `provider="openrouter"` etc.
//!
//! Every call resolves an effective task setting, builds a task-specific
//! chain (filtering for vision when needed, promoting an explicit provider
//! when set), then walks the chain executing the request and falling back
//! on payment / connection errors.
//!
//! No HTTP code lives here — the client only orchestrates around the
//! [`hermes_core::LlmProvider`] trait.

use std::collections::HashMap;
use std::time::Duration;

use hermes_core::{LlmResponse, Message, ToolSchema};
use serde_json::Value;
use tokio::time::timeout;

use super::candidate::{ProviderCandidate, ProviderChain};
use super::config::{
    resolve_task_settings, AuxiliaryConfig, ExplicitOverrides, ResolvedTaskSettings,
};
use super::error::{
    is_unsupported_parameter_error, is_unsupported_temperature_error, should_fallback,
    AuxiliaryError, AuxiliaryResult,
};
use super::task::AuxiliaryTask;

// ---------------------------------------------------------------------------
// AuxiliaryRequest / Response
// ---------------------------------------------------------------------------

/// Inputs for a single auxiliary call.
#[derive(Debug, Clone, Default)]
pub struct AuxiliaryRequest {
    pub task: Option<AuxiliaryTask>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub timeout: Option<Duration>,
    pub extra_body: Option<Value>,
}

impl AuxiliaryRequest {
    pub fn new(task: AuxiliaryTask, messages: Vec<Message>) -> Self {
        Self {
            task: Some(task),
            messages,
            ..Default::default()
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    pub fn with_temperature(mut self, t: f64) -> Self {
        self.temperature = Some(t);
        self
    }

    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    pub fn with_timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }
}

/// Result of a successful auxiliary call.
#[derive(Debug, Clone)]
pub struct AuxiliaryResponse {
    pub provider_label: String,
    pub model: String,
    pub response: LlmResponse,
}

impl AuxiliaryResponse {
    /// Convenience accessor — extracts the assistant text content if any.
    pub fn text(&self) -> Option<&str> {
        self.response.message.content.as_deref()
    }
}

// ---------------------------------------------------------------------------
// AuxiliaryClient
// ---------------------------------------------------------------------------

/// Routes auxiliary LLM calls to the cheapest available provider with
/// payment / connection error fallback.
pub struct AuxiliaryClient {
    /// The provider chain in auto-detect order. The chain is consulted when
    /// `provider` resolves to `"auto"`.
    base_chain: ProviderChain,
    /// Map from explicit provider label → candidate. Used when the resolved
    /// provider is anything other than `"auto"`.
    by_label: HashMap<String, ProviderCandidate>,
    config: AuxiliaryConfig,
}

fn remove_extra_body_param(extra_body: Option<Value>, key: &str) -> Option<Value> {
    match extra_body {
        Some(Value::Object(mut map)) => {
            map.remove(key);
            Some(Value::Object(map))
        }
        other => other,
    }
}

fn with_max_completion_tokens(extra_body: Option<Value>, max_tokens: u32) -> Option<Value> {
    match extra_body {
        Some(Value::Object(mut map)) => {
            map.remove("max_tokens");
            map.insert(
                "max_completion_tokens".to_string(),
                Value::Number(serde_json::Number::from(max_tokens)),
            );
            Some(Value::Object(map))
        }
        Some(other) => Some(other),
        None => {
            let mut map = serde_json::Map::new();
            map.insert(
                "max_completion_tokens".to_string(),
                Value::Number(serde_json::Number::from(max_tokens)),
            );
            Some(Value::Object(map))
        }
    }
}

fn should_retry_max_tokens(err: &hermes_core::AgentError) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("max_tokens")
        || msg.contains("unsupported_parameter")
        || is_unsupported_parameter_error(err, "max_tokens")
}

impl AuxiliaryClient {
    pub fn builder() -> AuxiliaryClientBuilder {
        AuxiliaryClientBuilder::default()
    }

    /// Number of providers in the auto chain.
    pub fn chain_len(&self) -> usize {
        self.base_chain.len()
    }

    /// Provider labels in auto-detect order.
    pub fn chain_labels(&self) -> Vec<String> {
        self.base_chain.labels()
    }

    /// Override the loaded config (mostly useful for tests).
    pub fn set_config(&mut self, config: AuxiliaryConfig) {
        self.config = config;
    }

    pub fn config(&self) -> &AuxiliaryConfig {
        &self.config
    }

    /// Execute one auxiliary call, walking the resolved chain on retryable
    /// errors. The first error that is *not* a payment / connection failure
    /// short-circuits the chain and is returned to the caller.
    pub async fn call(&self, request: AuxiliaryRequest) -> AuxiliaryResult<AuxiliaryResponse> {
        if request.messages.is_empty() {
            return Err(AuxiliaryError::InvalidRequest(
                "messages must not be empty".into(),
            ));
        }

        let task = request
            .task
            .clone()
            .unwrap_or_else(|| AuxiliaryTask::Custom("call".into()));

        let explicit = ExplicitOverrides {
            provider: request.provider.clone(),
            model: request.model.clone(),
            base_url: request.base_url.clone(),
            api_key: request.api_key.clone(),
            timeout: request.timeout,
        };
        let settings = resolve_task_settings(&task, &explicit, &self.config);

        let chain = self.build_task_chain(&task, &settings);
        if chain.is_empty() {
            return Err(AuxiliaryError::NoProviderAvailable {
                tried: self.chain_labels(),
            });
        }

        let temperature = request.temperature.or_else(|| task.default_temperature());
        let max_tokens = request.max_tokens.or_else(|| task.default_max_tokens());
        let extra_body = request.extra_body.clone();

        let mut errors: Vec<(String, String)> = Vec::new();

        for candidate in chain.iter() {
            let model = settings
                .model
                .clone()
                .unwrap_or_else(|| candidate.default_model.clone());

            let provider = candidate.provider.clone();
            let label = candidate.label();
            let messages = request.messages.clone();
            let tools = request.tools.clone();
            let model_call = model.clone();
            let mut attempt_temperature = temperature;
            let mut attempt_max_tokens = max_tokens;
            let mut attempt_extra_body = extra_body.clone();

            loop {
                let provider_call = provider.clone();
                let messages_call = messages.clone();
                let tools_call = tools.clone();
                let model_call_attempt = model_call.clone();
                let extra_body_call = attempt_extra_body.clone();
                let call_fut = async move {
                    provider_call
                        .chat_completion(
                            &messages_call,
                            &tools_call,
                            attempt_max_tokens,
                            attempt_temperature,
                            Some(&model_call_attempt),
                            extra_body_call.as_ref(),
                        )
                        .await
                };

                let outcome = timeout(settings.timeout, call_fut).await;

                match outcome {
                    Ok(Ok(response)) => {
                        tracing::debug!(
                            "auxiliary {}: succeeded via {} ({})",
                            task.as_key(),
                            label,
                            model
                        );
                        return Ok(AuxiliaryResponse {
                            provider_label: label,
                            model,
                            response,
                        });
                    }
                    Ok(Err(err)) => {
                        if attempt_temperature.is_some() && is_unsupported_temperature_error(&err) {
                            tracing::info!(
                                "auxiliary {}: provider {} rejected temperature; retrying without it",
                                task.as_key(),
                                label
                            );
                            attempt_temperature = None;
                            attempt_extra_body =
                                remove_extra_body_param(attempt_extra_body, "temperature");
                            continue;
                        }

                        if let Some(mt) = attempt_max_tokens {
                            if should_retry_max_tokens(&err) {
                                tracing::info!(
                                    "auxiliary {}: provider {} rejected max_tokens; retrying with max_completion_tokens",
                                    task.as_key(),
                                    label
                                );
                                attempt_max_tokens = None;
                                attempt_extra_body =
                                    with_max_completion_tokens(attempt_extra_body, mt);
                                continue;
                            }
                        }

                        if should_fallback(&err) {
                            tracing::info!(
                                "auxiliary {}: payment/connection error on {} ({}), trying next",
                                task.as_key(),
                                label,
                                err
                            );
                            errors.push((label.clone(), err.to_string()));
                            break;
                        }
                        // Non-retryable: short-circuit.
                        return Err(AuxiliaryError::Llm {
                            provider: label,
                            source: err,
                        });
                    }
                    Err(_elapsed) => {
                        tracing::info!(
                            "auxiliary {}: provider {} timed out after {:?}, trying next",
                            task.as_key(),
                            label,
                            settings.timeout
                        );
                        errors.push((
                            label.clone(),
                            format!("timeout after {:?}", settings.timeout),
                        ));
                        break;
                    }
                }
            }
        }

        Err(AuxiliaryError::all_providers_failed(errors))
    }

    fn build_task_chain(
        &self,
        task: &AuxiliaryTask,
        settings: &ResolvedTaskSettings,
    ) -> ProviderChain {
        let mut chain = if settings.provider == "auto" {
            self.base_chain.clone()
        } else {
            // Explicit provider — only that single candidate (still wrapped
            // as a chain so the rest of the code is uniform).
            let mut single = ProviderChain::new();
            if let Some(c) = self.by_label.get(&settings.provider) {
                single.push(c.clone());
            }
            single
        };

        if task.requires_vision() {
            chain = chain.vision_only();
        }

        chain
    }
}

// ---------------------------------------------------------------------------
// AuxiliaryClientBuilder
// ---------------------------------------------------------------------------

/// Builder for [`AuxiliaryClient`] — the binary layer is responsible for
/// wiring concrete `Arc<dyn LlmProvider>` instances since the intelligence
/// crate must not depend on `hermes-agent` (cycle).
#[derive(Default)]
pub struct AuxiliaryClientBuilder {
    chain: ProviderChain,
    config: AuxiliaryConfig,
}

impl AuxiliaryClientBuilder {
    /// Append a provider candidate to the auto-detect chain. Order matters —
    /// the first candidate added is tried first.
    pub fn add_candidate(mut self, candidate: ProviderCandidate) -> Self {
        self.chain.push(candidate);
        self
    }

    pub fn extend_candidates(
        mut self,
        candidates: impl IntoIterator<Item = ProviderCandidate>,
    ) -> Self {
        self.chain.extend(candidates);
        self
    }

    pub fn config(mut self, config: AuxiliaryConfig) -> Self {
        self.config = config;
        self
    }

    pub fn build(self) -> AuxiliaryClient {
        let mut by_label = HashMap::new();
        for c in self.chain.iter() {
            by_label.insert(c.label(), c.clone());
        }
        AuxiliaryClient {
            base_chain: self.chain,
            by_label,
            config: self.config,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auxiliary::candidate::AuxiliarySource;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use hermes_core::{
        AgentError, LlmProvider, LlmResponse, Message, MessageRole, StreamChunk, ToolSchema,
        UsageStats,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[derive(Debug, Clone)]
    struct RecordedCall {
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        extra_body: Option<serde_json::Value>,
    }

    /// Test provider whose behaviour is driven by a stack of canned outcomes.
    struct ScriptedProvider {
        label: String,
        outcomes: std::sync::Mutex<Vec<Outcome>>,
        calls: AtomicUsize,
        recorded_calls: std::sync::Mutex<Vec<RecordedCall>>,
    }

    enum Outcome {
        Ok(String),
        Err(String),
    }

    impl ScriptedProvider {
        fn new(label: &str, outcomes: Vec<Outcome>) -> Arc<Self> {
            Arc::new(Self {
                label: label.into(),
                outcomes: std::sync::Mutex::new(outcomes),
                calls: AtomicUsize::new(0),
                recorded_calls: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }

        fn recorded_calls(&self) -> Vec<RecordedCall> {
            self.recorded_calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LlmProvider for ScriptedProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            max_tokens: Option<u32>,
            temperature: Option<f64>,
            model: Option<&str>,
            extra_body: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.recorded_calls.lock().unwrap().push(RecordedCall {
                max_tokens,
                temperature,
                extra_body: extra_body.cloned(),
            });
            let mut outcomes = self.outcomes.lock().unwrap();
            let outcome = if outcomes.is_empty() {
                Outcome::Err("no scripted outcome remaining".into())
            } else {
                outcomes.remove(0)
            };
            match outcome {
                Outcome::Ok(text) => Ok(LlmResponse {
                    message: Message {
                        role: MessageRole::Assistant,
                        content: Some(text),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                        reasoning_content: None,
                        cache_control: None,
                    },
                    finish_reason: Some("stop".into()),
                    model: model.unwrap_or("test").to_string(),
                    usage: Some(UsageStats {
                        prompt_tokens: 1,
                        completion_tokens: 1,
                        total_tokens: 2,
                        estimated_cost: None,
                    }),
                }),
                Outcome::Err(msg) => Err(AgentError::LlmApi(format!("{}: {}", self.label, msg))),
            }
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            Box::pin(futures::stream::empty())
        }
    }

    fn user_msg(text: &str) -> Message {
        Message {
            role: MessageRole::User,
            content: Some(text.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
        }
    }

    #[tokio::test]
    async fn first_provider_wins_short_circuits() {
        let p1 = ScriptedProvider::new("openrouter", vec![Outcome::Ok("hi".into())]);
        let p2 = ScriptedProvider::new("anthropic", vec![Outcome::Ok("never".into())]);

        let client = AuxiliaryClient::builder()
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::OpenRouter,
                "or-model",
                p1.clone(),
            ))
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::Anthropic,
                "claude-haiku",
                p2.clone(),
            ))
            .build();

        let resp = client
            .call(AuxiliaryRequest::new(
                AuxiliaryTask::Title,
                vec![user_msg("hello")],
            ))
            .await
            .unwrap();

        assert_eq!(resp.provider_label, "openrouter");
        assert_eq!(resp.text(), Some("hi"));
        assert_eq!(p1.call_count(), 1);
        assert_eq!(p2.call_count(), 0);
    }

    #[tokio::test]
    async fn payment_error_falls_back_to_next_provider() {
        let p1 = ScriptedProvider::new(
            "openrouter",
            vec![Outcome::Err("HTTP 402: insufficient funds".into())],
        );
        let p2 = ScriptedProvider::new("anthropic", vec![Outcome::Ok("rescued".into())]);

        let client = AuxiliaryClient::builder()
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::OpenRouter,
                "or-model",
                p1.clone(),
            ))
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::Anthropic,
                "claude-haiku",
                p2.clone(),
            ))
            .build();

        let resp = client
            .call(AuxiliaryRequest::new(
                AuxiliaryTask::Compression,
                vec![user_msg("compress me")],
            ))
            .await
            .unwrap();

        assert_eq!(resp.provider_label, "anthropic");
        assert_eq!(resp.text(), Some("rescued"));
        assert_eq!(p1.call_count(), 1);
        assert_eq!(p2.call_count(), 1);
    }

    #[tokio::test]
    async fn non_retryable_error_short_circuits() {
        let p1 = ScriptedProvider::new("openrouter", vec![Outcome::Err("invalid api key".into())]);
        let p2 = ScriptedProvider::new("anthropic", vec![Outcome::Ok("never".into())]);

        let client = AuxiliaryClient::builder()
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::OpenRouter,
                "or-model",
                p1.clone(),
            ))
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::Anthropic,
                "claude-haiku",
                p2.clone(),
            ))
            .build();

        let err = client
            .call(AuxiliaryRequest::new(
                AuxiliaryTask::Title,
                vec![user_msg("h")],
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, AuxiliaryError::Llm { .. }));
        assert_eq!(p1.call_count(), 1);
        assert_eq!(p2.call_count(), 0);
    }

    #[tokio::test]
    async fn all_providers_payment_failed() {
        let p1 = ScriptedProvider::new("openrouter", vec![Outcome::Err("402 credits".into())]);
        let p2 = ScriptedProvider::new("anthropic", vec![Outcome::Err("402 billing".into())]);

        let client = AuxiliaryClient::builder()
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::OpenRouter,
                "or-model",
                p1.clone(),
            ))
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::Anthropic,
                "claude",
                p2.clone(),
            ))
            .build();

        let err = client
            .call(AuxiliaryRequest::new(
                AuxiliaryTask::Title,
                vec![user_msg("h")],
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, AuxiliaryError::AllProvidersFailed { .. }));
    }

    #[tokio::test]
    async fn explicit_provider_skips_chain() {
        let p1 = ScriptedProvider::new("openrouter", vec![Outcome::Ok("never".into())]);
        let p2 = ScriptedProvider::new("anthropic", vec![Outcome::Ok("explicit-win".into())]);

        let client = AuxiliaryClient::builder()
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::OpenRouter,
                "or",
                p1.clone(),
            ))
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::Anthropic,
                "claude",
                p2.clone(),
            ))
            .build();

        let req = AuxiliaryRequest::new(AuxiliaryTask::Title, vec![user_msg("h")])
            .with_provider("anthropic");
        let resp = client.call(req).await.unwrap();
        assert_eq!(resp.provider_label, "anthropic");
        assert_eq!(p1.call_count(), 0);
        assert_eq!(p2.call_count(), 1);
    }

    #[tokio::test]
    async fn vision_task_filters_chain() {
        let p1 = ScriptedProvider::new("openrouter", vec![Outcome::Ok("vision-ok".into())]);
        let p2 = ScriptedProvider::new("kimi", vec![Outcome::Ok("never".into())]);

        let client = AuxiliaryClient::builder()
            .add_candidate(
                ProviderCandidate::new(
                    AuxiliarySource::DirectKey("kimi".into()),
                    "kimi-model",
                    p2.clone(),
                )
                .with_supports_vision(false),
            )
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::OpenRouter,
                "or-vision-model",
                p1.clone(),
            ))
            .build();

        let resp = client
            .call(AuxiliaryRequest::new(
                AuxiliaryTask::Vision,
                vec![user_msg("describe")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.provider_label, "openrouter");
        assert_eq!(p2.call_count(), 0);
    }

    #[tokio::test]
    async fn empty_messages_rejected() {
        let p1 = ScriptedProvider::new("openrouter", vec![Outcome::Ok("never".into())]);
        let client = AuxiliaryClient::builder()
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::OpenRouter,
                "or",
                p1,
            ))
            .build();
        let err = client
            .call(AuxiliaryRequest::new(AuxiliaryTask::Title, vec![]))
            .await
            .unwrap_err();
        assert!(matches!(err, AuxiliaryError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn empty_chain_returns_no_provider_available() {
        let client = AuxiliaryClient::builder().build();
        let err = client
            .call(AuxiliaryRequest::new(
                AuxiliaryTask::Title,
                vec![user_msg("h")],
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, AuxiliaryError::NoProviderAvailable { .. }));
    }

    #[tokio::test]
    async fn unsupported_temperature_retries_same_provider_without_temperature() {
        let p1 = ScriptedProvider::new(
            "openrouter",
            vec![
                Outcome::Err("HTTP 400: Unsupported parameter: temperature".into()),
                Outcome::Ok("rescued".into()),
            ],
        );
        let p2 = ScriptedProvider::new("anthropic", vec![Outcome::Ok("never".into())]);

        let client = AuxiliaryClient::builder()
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::OpenRouter,
                "or-model",
                p1.clone(),
            ))
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::Anthropic,
                "claude-haiku",
                p2.clone(),
            ))
            .build();

        let resp = client
            .call(
                AuxiliaryRequest::new(AuxiliaryTask::Compression, vec![user_msg("compress me")])
                    .with_temperature(0.3),
            )
            .await
            .unwrap();

        assert_eq!(resp.provider_label, "openrouter");
        assert_eq!(resp.text(), Some("rescued"));
        assert_eq!(p1.call_count(), 2);
        assert_eq!(p2.call_count(), 0);

        let calls = p1.recorded_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].temperature, Some(0.3));
        assert_eq!(calls[1].temperature, None);
    }

    #[tokio::test]
    async fn unsupported_max_tokens_retries_with_max_completion_tokens() {
        let p1 = ScriptedProvider::new(
            "openrouter",
            vec![
                Outcome::Err("Unknown parameter: max_tokens".into()),
                Outcome::Ok("rescued".into()),
            ],
        );

        let client = AuxiliaryClient::builder()
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::OpenRouter,
                "or-model",
                p1.clone(),
            ))
            .build();

        let resp = client
            .call(
                AuxiliaryRequest::new(AuxiliaryTask::SessionSearch, vec![user_msg("find this")])
                    .with_max_tokens(512),
            )
            .await
            .unwrap();

        assert_eq!(resp.provider_label, "openrouter");
        assert_eq!(resp.text(), Some("rescued"));

        let calls = p1.recorded_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].max_tokens, Some(512));
        assert_eq!(calls[1].max_tokens, None);
        let retry_extra_body = calls[1].extra_body.as_ref().expect("retry extra body");
        assert_eq!(
            retry_extra_body
                .get("max_completion_tokens")
                .and_then(|v| v.as_u64()),
            Some(512)
        );
    }

    #[tokio::test]
    async fn max_tokens_retry_not_triggered_when_max_tokens_absent() {
        let p1 = ScriptedProvider::new(
            "openrouter",
            vec![Outcome::Err(
                "HTTP 400: Unsupported parameter: max_tokens".into(),
            )],
        );

        let client = AuxiliaryClient::builder()
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::OpenRouter,
                "or-model",
                p1.clone(),
            ))
            .build();

        let err = client
            .call(AuxiliaryRequest::new(
                AuxiliaryTask::Custom("custom".into()),
                vec![user_msg("x")],
            ))
            .await
            .unwrap_err();

        assert!(matches!(err, AuxiliaryError::Llm { .. }));
        assert_eq!(p1.call_count(), 1);
    }

    #[tokio::test]
    async fn temperature_retry_not_triggered_without_temperature_in_request() {
        let p1 = ScriptedProvider::new(
            "openrouter",
            vec![Outcome::Err(
                "HTTP 400: Unsupported parameter: temperature".into(),
            )],
        );

        let client = AuxiliaryClient::builder()
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::OpenRouter,
                "or-model",
                p1.clone(),
            ))
            .build();

        let err = client
            .call(AuxiliaryRequest::new(
                AuxiliaryTask::Custom("custom".into()),
                vec![user_msg("x")],
            ))
            .await
            .unwrap_err();

        assert!(matches!(err, AuxiliaryError::Llm { .. }));
        assert_eq!(p1.call_count(), 1);
    }

    #[tokio::test]
    async fn timeout_falls_back_to_next() {
        struct Stalls;
        #[async_trait]
        impl LlmProvider for Stalls {
            async fn chat_completion(
                &self,
                _m: &[Message],
                _t: &[ToolSchema],
                _x: Option<u32>,
                _temp: Option<f64>,
                _model: Option<&str>,
                _eb: Option<&serde_json::Value>,
            ) -> Result<LlmResponse, AgentError> {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Err(AgentError::LlmApi("never".into()))
            }
            fn chat_completion_stream(
                &self,
                _m: &[Message],
                _t: &[ToolSchema],
                _x: Option<u32>,
                _temp: Option<f64>,
                _model: Option<&str>,
                _eb: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                Box::pin(futures::stream::empty())
            }
        }
        let p2 = ScriptedProvider::new("anthropic", vec![Outcome::Ok("rescued".into())]);

        let client = AuxiliaryClient::builder()
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::OpenRouter,
                "or",
                Arc::new(Stalls),
            ))
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::Anthropic,
                "claude",
                p2.clone(),
            ))
            .build();

        let req = AuxiliaryRequest::new(AuxiliaryTask::Title, vec![user_msg("h")])
            .with_timeout(Duration::from_millis(40));
        let resp = client.call(req).await.unwrap();
        assert_eq!(resp.provider_label, "anthropic");
    }
}
