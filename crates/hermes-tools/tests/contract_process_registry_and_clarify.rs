use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hermes_core::{ToolError, ToolHandler};
use serde_json::{json, Value};

use hermes_tools::tools::clarify::{ClarifyBackend, ClarifyHandler, MAX_CHOICES};
use hermes_tools::tools::process_registry::ProcessRegistryHandler;

fn parse_json(raw: &str) -> Value {
    serde_json::from_str(raw).expect("valid json")
}

#[derive(Default)]
struct CapturingClarifyBackend {
    question: Mutex<Option<String>>,
    choices: Mutex<Option<Vec<String>>>,
}

#[async_trait]
impl ClarifyBackend for CapturingClarifyBackend {
    async fn ask(&self, question: &str, choices: Option<&[String]>) -> Result<String, ToolError> {
        *self.question.lock().expect("lock question") = Some(question.to_string());
        *self.choices.lock().expect("lock choices") = choices.map(|c| c.to_vec());
        Ok("ok".to_string())
    }
}

#[tokio::test]
async fn process_registry_contract_matches_expected_actions() {
    let handler = ProcessRegistryHandler::default();

    let out = handler
        .execute(json!({"action":"register","name":"proc_1","pid":1001}))
        .await
        .expect("register");
    let parsed = parse_json(&out);
    assert_eq!(parsed["status"], "registered");

    let out = handler
        .execute(json!({"action":"poll","name":"proc_1"}))
        .await
        .expect("poll");
    let parsed = parse_json(&out);
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["entry"]["pid"], 1001);

    let out = handler
        .execute(json!({"action":"list"}))
        .await
        .expect("list");
    let parsed = parse_json(&out);
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["count"], 1);

    let out = handler
        .execute(json!({"action":"deregister","name":"proc_1"}))
        .await
        .expect("deregister");
    let parsed = parse_json(&out);
    assert_eq!(parsed["status"], "removed");

    let out = handler
        .execute(json!({"action":"get","name":"proc_1"}))
        .await
        .expect("get after remove");
    let parsed = parse_json(&out);
    assert_eq!(parsed["status"], "not_found");
}

#[tokio::test]
async fn clarify_contract_trims_question_and_limits_choices() {
    let backend = Arc::new(CapturingClarifyBackend::default());
    let handler = ClarifyHandler::new(backend.clone());
    handler
        .execute(json!({
            "question":"  pick one  ",
            "choices":["a","b","c","d","e","f"]
        }))
        .await
        .expect("clarify call should succeed");

    assert_eq!(
        backend.question.lock().expect("lock question").as_deref(),
        Some("pick one")
    );
    let captured = backend
        .choices
        .lock()
        .expect("lock choices")
        .clone()
        .expect("choices captured");
    assert_eq!(captured.len(), MAX_CHOICES);
    assert_eq!(captured, vec!["a", "b", "c", "d"]);
}

#[tokio::test]
async fn clarify_rejects_empty_question() {
    let handler = ClarifyHandler::new(Arc::new(CapturingClarifyBackend::default()));
    let err = handler
        .execute(json!({"question":"   "}))
        .await
        .expect_err("should reject empty question");
    assert!(err.to_string().contains("cannot be empty"));
}
