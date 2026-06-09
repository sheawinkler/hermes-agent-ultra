use hermes_tools::tools::env_passthrough::{
    clear_env_passthrough, get_all_passthrough, is_env_passthrough, register_env_passthrough,
};
use hermes_tools::prepare_child_env;
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn reset() -> MutexGuard<'static, ()> {
    let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env_passthrough();
    guard
}

#[test]
fn registers_env_passthrough_names() {
    let _guard = reset();

    register_env_passthrough(["HERMES_TEST_TENOR_API_KEY", "HERMES_TEST_NOTION_TOKEN"]);

    assert!(is_env_passthrough("HERMES_TEST_TENOR_API_KEY"));
    assert!(is_env_passthrough("HERMES_TEST_NOTION_TOKEN"));
}

#[test]
fn ignores_empty_names() {
    let _guard = reset();

    register_env_passthrough(["", "   ", "\t"]);

    assert!(!is_env_passthrough(""));
}

#[test]
fn clear_removes_registered_names() {
    let _guard = reset();
    register_env_passthrough(["HERMES_TEST_CLEAR_ME"]);

    clear_env_passthrough();

    assert!(!is_env_passthrough("HERMES_TEST_CLEAR_ME"));
}

#[test]
fn rejects_hermes_provider_credentials() {
    let _guard = reset();

    register_env_passthrough(["OPENAI_API_KEY", "ANTHROPIC_TOKEN", "TENOR_API_KEY"]);

    assert!(!is_env_passthrough("OPENAI_API_KEY"));
    assert!(!is_env_passthrough("ANTHROPIC_TOKEN"));
    assert!(is_env_passthrough("TENOR_API_KEY"));
}

#[test]
fn get_all_passthrough_returns_registered_union() {
    let _guard = reset();

    register_env_passthrough([
        "HERMES_TEST_TENOR_API_KEY",
        "HERMES_TEST_NOTION_TOKEN",
        "HERMES_TEST_TENOR_API_KEY",
    ]);

    let all = get_all_passthrough();
    assert!(all.contains("HERMES_TEST_TENOR_API_KEY"));
    assert!(all.contains("HERMES_TEST_NOTION_TOKEN"));
}

#[test]
fn registered_names_are_thread_scoped() {
    let _guard = reset();

    register_env_passthrough(["HERMES_TEST_THREAD_ONLY"]);
    assert!(is_env_passthrough("HERMES_TEST_THREAD_ONLY"));

    let child_sees_parent_value =
        std::thread::spawn(|| is_env_passthrough("HERMES_TEST_THREAD_ONLY"))
            .join()
            .expect("child thread");

    assert!(!child_sees_parent_value);
}

#[test]
fn provider_credential_floor_is_visible_to_callers() {
    let _guard = reset();

    assert!(hermes_tools::tools::env_passthrough::is_hermes_provider_credential(
        "OPENAI_API_KEY"
    ));
    assert!(!hermes_tools::tools::env_passthrough::is_hermes_provider_credential(
        "TENOR_API_KEY"
    ));
}

#[test]
fn registered_names_pass_through_child_env_scrubbing() {
    let _guard = reset();
    register_env_passthrough(["TENOR_API_KEY", "OPENAI_API_KEY"]);

    let source = BTreeMap::from([
        ("PATH".to_string(), "/bin".to_string()),
        ("TENOR_API_KEY".to_string(), "tenor-secret".to_string()),
        ("OPENAI_API_KEY".to_string(), "openai-secret".to_string()),
    ]);

    let child = prepare_child_env(&source, is_env_passthrough, false);

    assert_eq!(child.get("PATH").map(String::as_str), Some("/bin"));
    assert_eq!(
        child.get("TENOR_API_KEY").map(String::as_str),
        Some("tenor-secret")
    );
    assert!(!child.contains_key("OPENAI_API_KEY"));
}
