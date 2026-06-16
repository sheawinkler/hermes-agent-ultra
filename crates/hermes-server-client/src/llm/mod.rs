//! Remote LLM provider backed by Flowy OpenAI-compatible `/claw/v1` API.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use hermes_agent::provider::GenericProvider;
use hermes_config::ServerConfig;
use hermes_core::{AgentError, LlmProvider, LlmResponse, Message, StreamChunk, ToolSchema};
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::warn;

use crate::error::ServerClientError;
use crate::flowy::FlowyApiClient;
use crate::session::ServerSession;

/// Remote LLM gateway using JWT from [`ServerSession`] against Flowy `/v1/chat/completions`.
pub struct ServerLlmProvider {
    config: ServerConfig,
    session: ServerSession,
    api: FlowyApiClient,
    chat_session_id: Arc<Mutex<Option<String>>>,
}

impl ServerLlmProvider {
    pub fn new(
        config: ServerConfig,
        hermes_home: impl AsRef<std::path::Path>,
    ) -> Result<Self, ServerClientError> {
        if !config.enabled {
            return Err(ServerClientError::Disabled);
        }
        if !config.api_ready() {
            return Err(ServerClientError::MissingBaseUrl);
        }
        let api = FlowyApiClient::new(&config)?;
        Ok(Self {
            config: config.clone(),
            session: ServerSession::from_config(&config, hermes_home),
            api,
            chat_session_id: Arc::new(Mutex::new(None)),
        })
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn api(&self) -> &FlowyApiClient {
        &self.api
    }

    pub fn session(&self) -> &ServerSession {
        &self.session
    }

    async fn ensure_chat_session(&self) {
        let mut guard = self.chat_session_id.lock().await;
        if guard.is_some() {
            return;
        }
        let session_id = format!("hermes-{}", uuid::Uuid::new_v4());
        match self
            .api
            .report_chat_session(&self.session, &session_id)
            .await
        {
            Ok(resp) if resp.stored => {
                *guard = Some(session_id);
            }
            Ok(_) => {
                warn!("chat session report returned stored=false; continuing anyway");
                *guard = Some(session_id);
            }
            Err(err) => {
                warn!(error = %err, "chat session report failed; continuing with LLM call");
            }
        }
    }

    async fn build_inner(&self) -> Result<GenericProvider, ServerClientError> {
        let token = self
            .session
            .access_token()
            .await?
            .filter(|t| !t.is_empty())
            .ok_or_else(|| ServerClientError::AuthRequired("not logged in to remote server".into()))?;

        let base = self.config.effective_llm_base_url();
        let model = self.config.effective_default_llm_model();

        Ok(GenericProvider::new(base, token.clone(), model)
            .with_header("token", token)
            .with_request_timeout_seconds(
                self.config.llm.request_timeout_seconds.max(1) as f64,
            ))
    }

    async fn resolve_model(&self, model: Option<&str>) -> Result<String, ServerClientError> {
        Ok(model
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.config.effective_default_llm_model()))
    }
}

#[async_trait]
impl LlmProvider for ServerLlmProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.ensure_chat_session().await;
        let inner = self.build_inner().await?;
        let effective_model = self.resolve_model(model).await?;
        inner
            .chat_completion(
                messages,
                tools,
                max_tokens,
                temperature,
                Some(effective_model.as_str()),
                extra_body,
            )
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
        use async_stream::stream;
        use futures::StreamExt;

        let this = self.clone();
        let messages = messages.to_vec();
        let tools = tools.to_vec();
        let model = model.map(str::to_string);
        let extra_body = extra_body.cloned();

        Box::pin(stream! {
            this.ensure_chat_session().await;
            let inner = match this.build_inner().await {
                Ok(inner) => inner,
                Err(err) => {
                    yield Err(err.into());
                    return;
                }
            };
            let effective_model = match this.resolve_model(model.as_deref()).await {
                Ok(model) => model,
                Err(err) => {
                    yield Err(err.into());
                    return;
                }
            };
            let mut chunks = inner.chat_completion_stream(
                &messages,
                &tools,
                max_tokens,
                temperature,
                Some(effective_model.as_str()),
                extra_body.as_ref(),
            );
            while let Some(chunk) = chunks.next().await {
                yield chunk;
            }
        })
    }
}

impl ServerLlmProvider {
    fn clone_for_task(&self) -> Self {
        Self {
            config: self.config.clone(),
            session: self.session.clone(),
            api: FlowyApiClient::new(&self.config).expect("flowy client"),
            chat_session_id: Arc::clone(&self.chat_session_id),
        }
    }
}

impl Clone for ServerLlmProvider {
    fn clone(&self) -> Self {
        self.clone_for_task()
    }
}

impl From<ServerClientError> for AgentError {
    fn from(value: ServerClientError) -> Self {
        match value {
            ServerClientError::Agent(e) => e,
            ServerClientError::AuthRequired(msg) => AgentError::Config(format!("auth required: {msg}")),
            other => AgentError::Config(other.to_string()),
        }
    }
}
