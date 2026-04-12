//! Clarify tool: ask user questions with choices

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::sync::Arc;

// ---------------------------------------------------------------------------
// ClarifyBackend trait
// ---------------------------------------------------------------------------

/// Backend for presenting clarification questions to the user.
#[async_trait]
pub trait ClarifyBackend: Send + Sync {
    /// Ask the user a question and return their answer.
    async fn ask(&self, question: &str, choices: Option<&[String]>) -> Result<String, ToolError>;
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
        let question = params
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'question' parameter".into()))?;

        let choices: Option<Vec<String>> =
            params.get("choices").and_then(|v| v.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            });

        match choices {
            Some(ref c) => self.backend.ask(question, Some(c.as_slice())).await,
            None => self.backend.ask(question, None).await,
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
                "items": { "type": "string" }
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

    struct MockClarifyBackend;
    #[async_trait]
    impl ClarifyBackend for MockClarifyBackend {
        async fn ask(
            &self,
            question: &str,
            _choices: Option<&[String]>,
        ) -> Result<String, ToolError> {
            Ok(format!("User answered: (question was: {})", question))
        }
    }

    #[tokio::test]
    async fn test_clarify_schema() {
        let handler = ClarifyHandler::new(Arc::new(MockClarifyBackend));
        assert_eq!(handler.schema().name, "clarify");
    }

    #[tokio::test]
    async fn test_clarify_execute() {
        let handler = ClarifyHandler::new(Arc::new(MockClarifyBackend));
        let result = handler
            .execute(json!({"question": "Which option?", "choices": ["A", "B"]}))
            .await
            .unwrap();
        assert!(result.contains("Which option?"));
    }
}
