//! Clarify tool: ask user questions with choices

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::borrow::Cow;

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};
use tracing::debug;

use std::sync::Arc;

pub const MAX_CHOICES: usize = 4;

// ---------------------------------------------------------------------------
// ClarifyBackend trait
// ---------------------------------------------------------------------------

/// Backend for presenting clarification questions to the user.
#[async_trait]
pub trait ClarifyBackend: Send + Sync {
    /// Ask the user a question and return their answer.
    ///
    /// `session_key` is the gateway chat session (injected as `task_id` on IM
    /// routes) so channel backends can match inbound replies.
    async fn ask(
        &self,
        question: &str,
        choices: Option<&[String]>,
        session_key: Option<&str>,
    ) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// ClarifyHandler
// ---------------------------------------------------------------------------

/// Tool for asking the user clarification questions.
pub struct ClarifyHandler {
    backend: Arc<dyn ClarifyBackend>,
}

impl ClarifyHandler {
    pub fn new(backend: Arc<dyn ClarifyBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for ClarifyHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let question_raw = params
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'question' parameter".into()))?;
        let question = question_raw.trim();
        if question.is_empty() {
            return Err(ToolError::InvalidParams(
                "Parameter 'question' cannot be empty".into(),
            ));
        }

        let choices = match params.get("choices") {
            None | Some(Value::Null) => None,
            Some(Value::Array(arr)) => {
                let normalized: Vec<String> = arr
                    .iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(Cow::Borrowed(s.as_str())),
                        Value::Number(n) => Some(Cow::Owned(n.to_string())),
                        Value::Bool(b) => Some(Cow::Owned(b.to_string())),
                        _ => None,
                    })
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .take(MAX_CHOICES)
                    .collect();
                if normalized.is_empty() {
                    None
                } else {
                    Some(normalized)
                }
            }
            _ => {
                return Err(ToolError::InvalidParams(
                    "Parameter 'choices' must be an array of values".into(),
                ));
            }
        };

        let session_key = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        debug!(
            question = %question,
            choice_count = choices.as_ref().map(|c| c.len()).unwrap_or(0),
            choices = ?choices,
            session_key = ?session_key,
            "clarify tool execute"
        );
        match choices {
            Some(ref c) => {
                self.backend
                    .ask(question, Some(c.as_slice()), session_key)
                    .await
            }
            None => self.backend.ask(question, None, session_key).await,
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "question".into(),
            json!({
                "type": "string",
                "description": "The question to ask the user"
            }),
        );
        props.insert(
            "choices".into(),
            json!({
                "type": "array",
                "description": "Optional list of choices for the user to select from",
                "items": { "type": "string" },
                "maxItems": MAX_CHOICES
            }),
        );

        tool_schema(
            "clarify",
            "Ask the user a clarification question. Optionally provide choices for the user to select from.",
            JsonSchema::object(props, vec!["question".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockClarifyBackend {
        captured_question: Mutex<Option<String>>,
        captured_choices: Mutex<Option<Vec<String>>>,
    }
    #[async_trait]
    impl ClarifyBackend for MockClarifyBackend {
        async fn ask(
            &self,
            question: &str,
            choices: Option<&[String]>,
            _session_key: Option<&str>,
        ) -> Result<String, ToolError> {
            *self.captured_question.lock().expect("lock question") = Some(question.to_string());
            *self.captured_choices.lock().expect("lock choices") = choices.map(|c| c.to_vec());
            Ok(format!("User answered: (question was: {})", question))
        }
    }

    #[tokio::test]
    async fn test_clarify_schema() {
        let handler = ClarifyHandler::new(Arc::new(MockClarifyBackend::default()));
        assert_eq!(handler.schema().name, "clarify");
    }

    #[tokio::test]
    async fn test_clarify_execute() {
        let backend = Arc::new(MockClarifyBackend::default());
        let handler = ClarifyHandler::new(backend.clone());
        let result = handler
            .execute(json!({"question": "Which option?", "choices": ["A", "B"]}))
            .await
            .unwrap();
        assert!(result.contains("Which option?"));
        assert_eq!(
            backend
                .captured_question
                .lock()
                .expect("lock question")
                .as_deref(),
            Some("Which option?")
        );
        assert_eq!(
            backend
                .captured_choices
                .lock()
                .expect("lock choices")
                .clone(),
            Some(vec!["A".to_string(), "B".to_string()])
        );
    }

    #[tokio::test]
    async fn test_empty_question_rejected() {
        let handler = ClarifyHandler::new(Arc::new(MockClarifyBackend::default()));
        let err = handler
            .execute(json!({"question": "   ", "choices": ["A"]}))
            .await
            .expect_err("expected empty question error");
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[tokio::test]
    async fn test_choices_trimmed_and_limited() {
        let backend = Arc::new(MockClarifyBackend::default());
        let handler = ClarifyHandler::new(backend.clone());
        handler
            .execute(json!({
                "question": " Pick one ",
                "choices": ["  a  ", "", "b", 3, true, "c", "d", "e"]
            }))
            .await
            .expect("execute should succeed");

        assert_eq!(
            backend
                .captured_question
                .lock()
                .expect("lock question")
                .as_deref(),
            Some("Pick one")
        );
        assert_eq!(
            backend
                .captured_choices
                .lock()
                .expect("lock choices")
                .clone(),
            Some(vec![
                "a".to_string(),
                "b".to_string(),
                "3".to_string(),
                "true".to_string(),
            ])
        );
    }

    #[tokio::test]
    async fn test_invalid_choices_type_rejected() {
        let handler = ClarifyHandler::new(Arc::new(MockClarifyBackend::default()));
        let err = handler
            .execute(json!({"question": "Q?", "choices": "not-an-array"}))
            .await
            .expect_err("expected invalid choices type");
        assert!(err.to_string().contains("must be an array"));
    }
}
