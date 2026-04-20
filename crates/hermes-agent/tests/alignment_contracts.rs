//! Phase A — contract scenarios (see `python_alignment` module docs in `hermes-agent`).

use hermes_agent::{
    leading_system_prompt_for_persist, python_alignment, AgentConfig, AgentLoop, SessionPersistence,
};
use hermes_core::{AgentResult, Message};

#[test]
fn hydrate_stored_system_prompt_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![Message::user("hi")];
    sp.persist_session(
        "sid-1",
        &messages,
        Some("gpt-4o"),
        None,
        None,
        Some("STORED_SYSTEM_PROMPT_XYZ"),
    )
    .unwrap();

    let mut cfg = AgentConfig {
        session_id: Some("sid-1".into()),
        ..AgentConfig::default()
    };
    AgentLoop::hydrate_stored_system_prompt_from_hermes_home(&mut cfg, tmp.path()).unwrap();
    assert_eq!(
        cfg.stored_system_prompt.as_deref(),
        Some("STORED_SYSTEM_PROMPT_XYZ")
    );
}

#[test]
fn agent_result_default_serializes_new_fields() {
    let r: AgentResult = AgentResult::default();
    let j = serde_json::to_value(&r).unwrap();
    assert!(
        j.get("interrupted").is_none() || j["interrupted"] == false,
        "{j:?}"
    );
    assert!(j.get("session_cost_usd").is_none() || j["session_cost_usd"].is_null());
}

#[test]
fn strip_budget_tool_message_matches_python_fixture() {
    let mut messages = vec![Message {
        role: hermes_core::MessageRole::Tool,
        content: Some(
            r#"{"result":"ok","_budget_warning":"[BUDGET: Iteration 55/60. 5 iterations left. Start consolidating your work.]"}"#
                .into(),
        ),
        tool_calls: None,
        tool_call_id: Some("t1".into()),
        name: None,
        reasoning_content: None,
        cache_control: None,
    }];
    python_alignment::strip_budget_warnings_from_messages(&mut messages);
    let v: serde_json::Value = serde_json::from_str(messages[0].content.as_ref().unwrap()).unwrap();
    assert!(!v.as_object().unwrap().contains_key("_budget_warning"));
}

#[test]
fn leading_system_prompt_for_persist_joins_prefix() {
    let messages = vec![
        Message::system("A"),
        Message::system("B"),
        Message::user("hi"),
    ];
    assert_eq!(
        leading_system_prompt_for_persist(&messages).as_deref(),
        Some("A\n\nB")
    );
}

#[test]
fn codex_ack_heuristic_matches_workspace_user() {
    assert!(python_alignment::looks_like_codex_intermediate_ack(
        "check files in src/",
        "I'll look into the repository and inspect the codebase for issues.",
        false,
    ));
    assert!(!python_alignment::looks_like_codex_intermediate_ack(
        "check files in src/",
        "I'll look into the repository and inspect the codebase for issues.",
        true,
    ));
}
