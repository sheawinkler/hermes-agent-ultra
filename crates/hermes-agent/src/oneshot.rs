//! Stateless one-shot LLM helpers for UI/runtime chores.
//!
//! A one-shot call runs outside a conversation. It never appends to session
//! history and is intended for small generative tasks such as commit-message
//! drafts, rename suggestions, and summaries.

use std::time::Duration;

use hermes_core::Message;
use hermes_intelligence::auxiliary::{
    AuxiliaryClient, AuxiliaryError, AuxiliaryRequest, AuxiliaryTask,
};
use serde_json::{Map, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OneShotError {
    #[error("unknown one-shot template: {0}")]
    UnknownTemplate(String),
    #[error("one-shot requires a template or instructions/input")]
    EmptyPrompt,
    #[error(transparent)]
    Auxiliary(#[from] AuxiliaryError),
}

#[derive(Debug, Clone)]
pub struct OneShotRequest {
    pub instructions: String,
    pub user_input: String,
    pub template: Option<String>,
    pub variables: Map<String, Value>,
    pub task: String,
    pub max_tokens: u32,
    pub temperature: Option<f64>,
    pub timeout_secs: Option<u64>,
}

impl Default for OneShotRequest {
    fn default() -> Self {
        Self {
            instructions: String::new(),
            user_input: String::new(),
            template: None,
            variables: Map::new(),
            task: "title".to_string(),
            max_tokens: 1024,
            temperature: Some(0.3),
            timeout_secs: Some(60),
        }
    }
}

const COMMIT_MESSAGE_INSTRUCTIONS: &str = "You write git commit messages. Given a diff of staged changes, write ONE concise Conventional Commits message describing what changed and why.\n\
Rules:\n\
- Subject line: type(scope): summary, imperative mood, lower-case, no trailing period, <= 72 characters. Types: feat, fix, refactor, perf, docs, test, build, chore, style, ci.\n\
- Omit the scope if it is not obvious.\n\
- Add a short body only when the change needs explanation; skip it for small or obvious changes.\n\
- Describe the actual change, never restate the diff line by line.\n\
- Return only the commit message text, with no quotes, markdown fences, or preamble.";

pub fn truncate_template_text(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n...(truncated)", text[..end].trim_end())
}

pub fn render_template(
    name: &str,
    variables: &Map<String, Value>,
) -> Result<(String, String), OneShotError> {
    match name.trim() {
        "commit_message" => Ok(render_commit_message_template(variables)),
        other => Err(OneShotError::UnknownTemplate(other.to_string())),
    }
}

fn variable_string(variables: &Map<String, Value>, key: &str) -> String {
    variables
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn render_commit_message_template(variables: &Map<String, Value>) -> (String, String) {
    let diff = truncate_template_text(&variable_string(variables, "diff"), 12_000);
    let recent_commits =
        truncate_template_text(&variable_string(variables, "recent_commits"), 1_500);
    let avoid = truncate_template_text(&variable_string(variables, "avoid"), 1_000);

    let mut parts = Vec::new();
    if !recent_commits.trim().is_empty() {
        parts.push(format!(
            "Recent commit subjects from this repo (match their style/conventions):\n{recent_commits}"
        ));
    }
    parts.push(format!(
        "Diff to describe:\n{}",
        if diff.trim().is_empty() {
            "(no textual diff available)"
        } else {
            diff.as_str()
        }
    ));
    if !avoid.trim().is_empty() {
        parts.push(format!(
            "You already proposed this message and the user wants a different one. Write a new message with different wording and do not repeat it:\n{avoid}"
        ));
    }

    (COMMIT_MESSAGE_INSTRUCTIONS.to_string(), parts.join("\n\n"))
}

pub fn strip_wrapping_code_fence(text: &str) -> String {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }
    let lines = trimmed.lines().collect::<Vec<_>>();
    if lines.len() >= 2
        && lines.first().is_some_and(|line| line.starts_with("```"))
        && lines.last().is_some_and(|line| line.trim() == "```")
    {
        return lines[1..lines.len() - 1].join("\n").trim().to_string();
    }
    trimmed.to_string()
}

pub async fn run_oneshot_with_client(
    client: &AuxiliaryClient,
    mut request: OneShotRequest,
) -> Result<String, OneShotError> {
    if let Some(template) = request
        .template
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let (instructions, user_input) = render_template(template, &request.variables)?;
        request.instructions = instructions;
        request.user_input = user_input;
    }

    if request.instructions.trim().is_empty() && request.user_input.trim().is_empty() {
        return Err(OneShotError::EmptyPrompt);
    }

    let mut messages = Vec::new();
    if !request.instructions.trim().is_empty() {
        messages.push(Message::system(request.instructions));
    }
    messages.push(Message::user(request.user_input));

    let mut aux_request = AuxiliaryRequest::new(AuxiliaryTask::from_str(&request.task), messages)
        .with_max_tokens(request.max_tokens);
    if let Some(temperature) = request.temperature {
        aux_request = aux_request.with_temperature(temperature);
    }
    if let Some(timeout_secs) = request.timeout_secs.filter(|value| *value > 0) {
        aux_request = aux_request.with_timeout(Duration::from_secs(timeout_secs));
    }

    let response = client.call(aux_request).await?;
    let text = response
        .text()
        .or(response.response.message.reasoning_content.as_deref())
        .unwrap_or_default();
    Ok(strip_wrapping_code_fence(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use hermes_core::{
        AgentError, LlmProvider, LlmResponse, MessageRole, StreamChunk, ToolSchema, UsageStats,
    };
    use hermes_intelligence::auxiliary::{AuxiliarySource, ProviderCandidate};
    use std::sync::{Arc, Mutex};

    struct ScriptedProvider {
        response: String,
        calls: Mutex<Vec<Vec<Message>>>,
    }

    #[async_trait]
    impl LlmProvider for ScriptedProvider {
        async fn chat_completion(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            model: Option<&str>,
            _extra_body: Option<&Value>,
        ) -> Result<LlmResponse, AgentError> {
            self.calls.lock().unwrap().push(messages.to_vec());
            Ok(LlmResponse {
                message: Message::assistant(self.response.clone()),
                finish_reason: Some("stop".into()),
                model: model.unwrap_or("scripted").to_string(),
                usage: Some(UsageStats {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    estimated_cost: None,
                }),
            })
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            Box::pin(futures::stream::empty())
        }
    }

    fn scripted_client(provider: Arc<ScriptedProvider>) -> AuxiliaryClient {
        AuxiliaryClient::builder()
            .add_candidate(ProviderCandidate::new(
                AuxiliarySource::Custom,
                "scripted-model",
                provider,
            ))
            .build()
    }

    #[test]
    fn commit_message_template_includes_diff_recent_and_avoid() {
        let mut vars = Map::new();
        vars.insert(
            "diff".into(),
            Value::String("diff --git a/x b/x\n+new".into()),
        );
        vars.insert("recent_commits".into(), Value::String("feat: old".into()));
        vars.insert("avoid".into(), Value::String("fix: old wording".into()));

        let (instructions, user_input) = render_template("commit_message", &vars).unwrap();

        assert!(instructions.contains("Conventional Commits"));
        assert!(user_input.contains("diff --git a/x b/x"));
        assert!(user_input.contains("feat: old"));
        assert!(user_input.contains("fix: old wording"));
    }

    #[test]
    fn strip_wrapping_code_fence_removes_single_outer_fence() {
        assert_eq!(strip_wrapping_code_fence("```text\nhello\n```"), "hello");
        assert_eq!(strip_wrapping_code_fence("plain text"), "plain text");
    }

    #[tokio::test]
    async fn run_oneshot_uses_auxiliary_client_without_session_mutation() {
        let provider = Arc::new(ScriptedProvider {
            response: "```text\nfix(cli): tighten checks\n```".into(),
            calls: Mutex::new(Vec::new()),
        });
        let client = scripted_client(provider.clone());
        let mut vars = Map::new();
        vars.insert("diff".into(), Value::String("+changed".into()));

        let text = run_oneshot_with_client(
            &client,
            OneShotRequest {
                template: Some("commit_message".into()),
                variables: vars,
                ..OneShotRequest::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(text, "fix(cli): tighten checks");
        let calls = provider.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0][0].role, MessageRole::System);
        assert_eq!(calls[0][1].role, MessageRole::User);
    }
}
