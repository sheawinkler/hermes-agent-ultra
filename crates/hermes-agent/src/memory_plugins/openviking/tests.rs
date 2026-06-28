use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration as StdDuration;

struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }

    fn remove(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

#[test]
fn name() {
    let p = OpenVikingMemoryPlugin::new();
    assert_eq!(p.name(), "openviking");
}

#[test]
fn config_file_activates_provider_and_loads_values() {
    let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
    let tmp = tempfile::tempdir().expect("tempdir");
    let _home = EnvGuard::set("HERMES_HOME", tmp.path());
    let _endpoint = EnvGuard::remove("OPENVIKING_ENDPOINT");
    let _api_key = EnvGuard::remove("OPENVIKING_API_KEY");
    let _account = EnvGuard::remove("OPENVIKING_ACCOUNT");
    let _user = EnvGuard::remove("OPENVIKING_USER");
    let _agent = EnvGuard::remove("OPENVIKING_AGENT");
    std::fs::write(
        tmp.path().join("openviking.json"),
        r#"{
            "enabled": true,
            "endpoint": "localhost:1934/",
            "api_key": "ov-secret",
            "api_key_type": "root",
            "account": "acct",
            "user": "operator",
            "agent": "ultra"
        }"#,
    )
    .expect("write config");

    assert!(OpenVikingMemoryPlugin::new().is_available());
    let config = OpenVikingConfig::load(tmp.path().to_str().expect("home"));
    assert_eq!(config.endpoint, "http://localhost:1934");
    assert_eq!(config.api_key, "ov-secret");
    assert_eq!(config.api_key_type, "root");
    assert_eq!(config.account, "acct");
    assert_eq!(config.user, "operator");
    assert_eq!(config.agent, "ultra");
}

#[test]
fn save_config_merges_and_writes_owner_only() {
    let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
    let tmp = tempfile::tempdir().expect("tempdir");
    let _home = EnvGuard::set("HERMES_HOME", tmp.path());
    let path = tmp.path().join("openviking.json");
    std::fs::write(&path, r#"{"agent":"existing"}"#).expect("write existing");

    OpenVikingMemoryPlugin::new()
        .save_config(&json!({
            "enabled": true,
            "endpoint": "https://openviking.example",
            "api_key": "ov-secret"
        }))
        .expect("save config");

    let parsed: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read config")).expect("json");
    assert_eq!(parsed["agent"], "existing");
    assert_eq!(parsed["enabled"], true);
    assert_eq!(parsed["endpoint"], "https://openviking.example");
    assert_eq!(parsed["api_key"], "ov-secret");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn memory_uri_sanitizes_tenant_segments_without_agent_scope() {
    let uri = build_memory_uri("user/name", "agent one", "patterns");
    assert!(uri.starts_with("viking://user/user_name/memories/patterns/mem_"));
    assert!(uri.ends_with(".md"));
    assert!(!uri.contains("user/name"));
    assert!(!uri.contains("agent one"));
}

#[test]
fn memory_subdir_mapping_matches_write_targets_and_categories() {
    assert_eq!(memory_subdir_for_category("entity"), "entities");
    assert_eq!(memory_subdir_for_category("event"), "events");
    assert_eq!(memory_subdir_for_category("case"), "cases");
    assert_eq!(memory_subdir_for_category("pattern"), "patterns");
    assert_eq!(memory_subdir_for_category("unknown"), "preferences");
    assert_eq!(memory_subdir_for_target("memory"), "patterns");
    assert_eq!(memory_subdir_for_target("user"), "preferences");
}

#[test]
fn tool_schemas_include_narrow_forget_tool() {
    let plugin = OpenVikingMemoryPlugin::new();

    let names = plugin
        .get_tool_schemas()
        .into_iter()
        .filter_map(|schema| {
            schema
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect::<Vec<_>>();

    assert!(names.iter().any(|name| name == VIKING_FORGET_TOOL));
}

#[test]
fn validate_forget_memory_uri_accepts_exact_user_memory_files() {
    assert_eq!(
        validate_forget_memory_uri(Some(
            "viking://user/peers/hermes/memories/preferences/mem_abc123.md"
        ))
        .expect("valid"),
        "viking://user/peers/hermes/memories/preferences/mem_abc123.md"
    );
    assert_eq!(
        validate_forget_memory_uri(Some("viking://user/default/memories/profile.md"))
            .expect("valid"),
        "viking://user/default/memories/profile.md"
    );
    assert_eq!(
        validate_forget_memory_uri(Some("viking://user/default/memories/.full.md")).expect("valid"),
        "viking://user/default/memories/.full.md"
    );
}

#[test]
fn validate_forget_memory_uri_rejects_broad_or_non_memory_targets() {
    for uri in [
        "",
        "viking:/user/memories/preferences/mem_abc123.md",
        "viking://resources/project/doc.md",
        "viking://resources/project/memories/mem_abc123.md",
        "viking://agent/hermes/memories/preferences/mem_abc123.md",
        "viking://user/skills/example/SKILL.md",
        "viking://user/sessions/session-1/messages.jsonl",
        "viking://user/memories/preferences/",
        "viking://user/memories/preferences/.overview.md",
        "viking://user/memories/preferences/.abstract.md",
        "viking://user/memories/preferences/.relations.json",
        "viking://user/memories/preferences/mem_abc123.md?recursive=true",
    ] {
        assert!(
            validate_forget_memory_uri(Some(uri)).is_err(),
            "{uri} should be rejected"
        );
    }
}

fn one_shot_openviking_server(body: &'static str) -> (String, mpsc::Receiver<String>) {
    openviking_server(vec![(200, body)])
}

fn openviking_server(responses: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .set_read_timeout(Some(StdDuration::from_secs(2)))
                .expect("timeout");
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).expect("read");
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            tx.send(request).expect("send request");
            let reason = if (200..300).contains(&status) {
                "OK"
            } else {
                "Error"
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).expect("write");
        }
    });
    (format!("http://{addr}"), rx)
}

fn plugin_with_endpoint(endpoint: String) -> OpenVikingMemoryPlugin {
    let plugin = OpenVikingMemoryPlugin::new();
    *plugin.state.lock().unwrap() = Some(VikingState {
        client: Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("client"),
        endpoint,
        api_key: "test-key".to_string(),
        account: "acct".to_string(),
        user: "usr".to_string(),
        agent: "hermes".to_string(),
        session_id: "sid".to_string(),
        turn_count: 0,
    });
    plugin
}

#[test]
fn prefetch_uses_current_query_search_contract() {
    let body = r#"{"result":{"memories":[{"uri":"viking://user/usr/memories/project.md","abstract":"We chose Rust parity.","score":0.91}]}}"#;
    let (endpoint, rx) = openviking_server(vec![(200, body)]);
    let plugin = plugin_with_endpoint(endpoint);

    let out = plugin.prefetch("Rust parity status", "session-7");

    assert!(out.contains("## OpenViking Context"));
    assert!(out.contains("We chose Rust parity."));
    let request = rx.recv_timeout(StdDuration::from_secs(2)).expect("request");
    assert!(request.starts_with("POST /api/v1/search/search "));
    assert!(request.contains("\"query\":\"Rust parity status\""));
    assert!(request.contains("\"limit\":6"));
    assert!(request.contains("\"score_threshold\":0"));
    assert!(request.contains("\"context_type\":\"memory\""));
    assert!(request.contains("\"session_id\":\"session-7\""));
    assert!(!request.contains("top_k"));
}

#[test]
fn prefetch_falls_back_to_find_when_search_endpoint_fails() {
    let body = r#"{"result":{"memories":[{"uri":"viking://user/usr/memories/fallback.md","abstract":"Fallback recall worked.","score":0.88}]}}"#;
    let (endpoint, rx) = openviking_server(vec![(500, r#"{"error":"boom"}"#), (200, body)]);
    let plugin = plugin_with_endpoint(endpoint);

    let out = plugin.prefetch("fallback recall topic", "session-8");

    assert!(out.contains("Fallback recall worked."));
    let first = rx.recv_timeout(StdDuration::from_secs(2)).expect("first");
    let second = rx.recv_timeout(StdDuration::from_secs(2)).expect("second");
    assert!(first.starts_with("POST /api/v1/search/search "));
    assert!(second.starts_with("POST /api/v1/search/find "));
    assert!(second.contains("\"top_k\":6"));
}

#[test]
fn prefetch_reads_l2_content_when_abstract_is_empty() {
    let search = r#"{"result":{"memories":[{"uri":"viking://user/usr/memories/full.md","abstract":"","score":0.92,"level":2}]}}"#;
    let full = r#"{"result":{"content":"Full memory body from L2 read."}}"#;
    let (endpoint, rx) = openviking_server(vec![(200, search), (200, full)]);
    let plugin = plugin_with_endpoint(endpoint);

    let out = plugin.prefetch("full memory body", "session-9");

    assert!(out.contains("Full memory body from L2 read."));
    let search_request = rx.recv_timeout(StdDuration::from_secs(2)).expect("search");
    let read_request = rx.recv_timeout(StdDuration::from_secs(2)).expect("read");
    assert!(search_request.starts_with("POST /api/v1/search/search "));
    assert!(read_request.starts_with("GET /api/v1/content/read?"));
    assert!(read_request.contains("uri=viking%3A%2F%2Fuser%2Fusr%2Fmemories%2Ffull.md"));
}

#[test]
fn viking_read_accepts_batched_uris() {
    let (endpoint, rx) = openviking_server(vec![
        (200, r#"{"content":"first"}"#),
        (200, r#"{"content":"second"}"#),
    ]);
    let plugin = plugin_with_endpoint(endpoint);

    let result: Value = serde_json::from_str(&plugin.handle_tool_call(
        VIKING_READ_TOOL,
        &json!({"uris": ["viking://one.md", "viking://two.md"], "level": "overview"}),
    ))
    .expect("json");

    assert_eq!(result["results"].as_array().expect("results").len(), 2);
    assert_eq!(result["results"][0]["result"]["content"], "first");
    assert_eq!(result["results"][1]["result"]["content"], "second");
    let first = rx.recv_timeout(StdDuration::from_secs(2)).expect("first");
    let second = rx.recv_timeout(StdDuration::from_secs(2)).expect("second");
    assert!(first.starts_with("GET /api/v1/content/overview?"));
    assert!(second.starts_with("GET /api/v1/content/overview?"));
}

#[test]
fn openviking_schema_exposes_recall_policy_knobs() {
    let schema = OpenVikingMemoryPlugin::new()
        .get_config_schema()
        .expect("schema");
    let keys = schema
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|entry| entry.get("key").and_then(Value::as_str))
        .collect::<HashSet<_>>();

    for key in [
        "recall_limit",
        "recall_score_threshold",
        "recall_max_injected_chars",
        "recall_timeout_seconds",
        "recall_request_timeout_seconds",
        "recall_full_read_limit",
        "recall_prefer_abstract",
        "recall_resources",
    ] {
        assert!(keys.contains(key), "missing {key}");
    }
}

#[test]
fn handle_tool_call_forget_deletes_exact_memory_file_uri() {
    let uri = "viking://user/peers/hermes/memories/preferences/mem_abc123.md";
    let body = r#"{"status":"ok","result":{"uri":"viking://user/peers/hermes/memories/preferences/mem_abc123.md","estimated_deleted_count":1}}"#;
    let (endpoint, rx) = one_shot_openviking_server(body);
    let plugin = OpenVikingMemoryPlugin::new();
    *plugin.state.lock().unwrap() = Some(VikingState {
        client: Client::new(),
        endpoint,
        api_key: "test-key".to_string(),
        account: "acct".to_string(),
        user: "usr".to_string(),
        agent: "hermes".to_string(),
        session_id: "sid".to_string(),
        turn_count: 0,
    });

    let result: Value =
        serde_json::from_str(&plugin.handle_tool_call(VIKING_FORGET_TOOL, &json!({"uri": uri})))
            .expect("json");

    assert_eq!(result["status"], "deleted");
    assert_eq!(result["uri"], uri);
    assert_eq!(result["estimated_deleted_count"], 1);
    let request = rx.recv_timeout(StdDuration::from_secs(2)).expect("request");
    assert!(request.starts_with("DELETE /api/v1/fs?"));
    assert!(request.contains("recursive=false"));
    assert!(request
        .to_ascii_lowercase()
        .contains("authorization: bearer test-key"));
}

#[test]
fn extract_current_turn_anchors_on_latest_matching_user_and_assistant() {
    let messages = vec![
        json!({"role": "user", "content": "Please inspect the repository for assemble hooks."}),
        json!({"role": "assistant", "content": "Earlier answer."}),
        json!({"role": "user", "content": "Please inspect the repository for assemble hooks."}),
        json!({
            "role": "assistant",
            "content": "I will search the codebase.",
            "tool_calls": [{
                "id": "call_rg_1",
                "type": "function",
                "function": {
                    "name": "shell_command",
                    "arguments": serde_json::to_string(&json!({"command": "rg assemble"})).unwrap(),
                },
            }],
        }),
        json!({
            "role": "tool",
            "tool_call_id": "call_rg_1",
            "name": "shell_command",
            "content": "agent/context_engine.py: no preassemble hook",
        }),
        json!({"role": "assistant", "content": "The current main does not expose assemble."}),
    ];

    let turn = extract_current_turn_messages(
        &messages,
        "Please inspect the repository for assemble hooks.",
        "The current main does not expose assemble.",
    );

    assert_eq!(turn, messages[2..].to_vec());
}

#[test]
fn extract_current_turn_includes_trailing_tool_result_after_empty_assistant() {
    let messages = vec![
        json!({"role": "user", "content": "Run the check."}),
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_check",
                "type": "function",
                "function": {
                    "name": "terminal",
                    "arguments": serde_json::to_string(&json!({"cmd": "cargo test"})).unwrap(),
                },
            }],
        }),
        json!({
            "role": "tool",
            "tool_call_id": "call_check",
            "name": "terminal",
            "content": "test result: ok",
        }),
    ];

    let turn = extract_current_turn_messages(&messages, "Run the check.", "");

    assert_eq!(turn, messages);
}

#[test]
fn messages_to_openviking_batch_coalesces_tool_results() {
    let turn = vec![
        json!({"role": "user", "content": "Please inspect the repository for assemble hooks."}),
        json!({
            "role": "assistant",
            "content": "I will search the codebase.",
            "tool_calls": [{
                "id": "call_rg_1",
                "type": "function",
                "function": {
                    "name": "shell_command",
                    "arguments": serde_json::to_string(&json!({"command": "rg assemble"})).unwrap(),
                },
            }],
        }),
        json!({
            "role": "tool",
            "tool_call_id": "call_rg_1",
            "name": "shell_command",
            "content": "agent/context_engine.py: no preassemble hook",
        }),
        json!({"role": "assistant", "content": "The current main does not expose assemble."}),
    ];

    let batch = messages_to_openviking_batch(&turn, None);

    let roles = batch
        .iter()
        .filter_map(|message| message.get("role").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(roles, vec!["user", "assistant", "assistant", "assistant"]);
    assert_eq!(
        batch[2]["parts"],
        json!([{
            "type": "tool",
            "tool_id": "call_rg_1",
            "tool_name": "shell_command",
            "tool_input": {"command": "rg assemble"},
            "tool_output": "agent/context_engine.py: no preassemble hook",
            "tool_status": "completed",
        }])
    );
}

#[test]
fn messages_to_openviking_batch_marks_json_tool_error_results() {
    let turn = vec![
        json!({"role": "user", "content": "Check the file."}),
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_read_1",
                "type": "function",
                "function": {
                    "name": "read_file",
                    "arguments": serde_json::to_string(&json!({"path": "missing.md"})).unwrap(),
                },
            }],
        }),
        json!({
            "role": "tool",
            "tool_call_id": "call_read_1",
            "name": "read_file",
            "content": serde_json::to_string(&json!({"error": "File not found", "exit_code": 1})).unwrap(),
        }),
    ];

    let batch = messages_to_openviking_batch(&turn, None);

    assert_eq!(batch[1]["role"], "assistant");
    assert_eq!(batch[1]["parts"][0]["tool_status"], TOOL_STATUS_ERROR);
    assert_eq!(
        batch[1]["parts"][0]["tool_input"],
        json!({"path": "missing.md"})
    );
}

#[test]
fn messages_to_openviking_batch_keeps_pending_tool_call_without_result() {
    let turn = vec![
        json!({"role": "user", "content": "Start a long running check."}),
        json!({
            "role": "assistant",
            "content": "Starting it now.",
            "tool_calls": [{
                "id": "call_long_1",
                "type": "function",
                "function": {
                    "name": "long_check",
                    "arguments": serde_json::to_string(&json!({"target": "repo"})).unwrap(),
                },
            }],
        }),
    ];

    let batch = messages_to_openviking_batch(&turn, None);

    assert_eq!(
        batch[1]["parts"],
        json!([
            {"type": "text", "text": "Starting it now."},
            {
                "type": "tool",
                "tool_id": "call_long_1",
                "tool_name": "long_check",
                "tool_input": {"target": "repo"},
                "tool_status": "pending",
            }
        ])
    );
}

#[test]
fn messages_to_openviking_batch_skips_recall_results_without_reingesting_echoes() {
    let turn = vec![
        json!({"role": "user", "content": "What did we decide about context assembly?"}),
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [
                {
                    "id": "call_recall_1",
                    "type": "function",
                    "function": {
                        "name": VIKING_SEARCH_TOOL,
                        "arguments": serde_json::to_string(&json!({"query": "context assembly decision"})).unwrap(),
                    },
                },
                {
                    "id": "call_shell_1",
                    "type": "function",
                    "function": {
                        "name": "shell_command",
                        "arguments": serde_json::to_string(&json!({"command": "rg preassemble"})).unwrap(),
                    },
                },
            ],
        }),
        json!({
            "role": "tool",
            "tool_call_id": "call_recall_1",
            "name": VIKING_SEARCH_TOOL,
            "content": {"results": [{"uri": "viking://user/hermes/memories/context", "abstract": "Old OpenViking memory content"}]},
        }),
        json!({
            "role": "tool",
            "tool_call_id": "call_shell_1",
            "name": "shell_command",
            "content": "plugins/memory/openviking/__init__.py",
        }),
        json!({"role": "assistant", "content": "We decided to keep sync_turn scoped to ingestion."}),
    ];

    let batch = messages_to_openviking_batch(&turn, None);
    let batch_text = serde_json::to_string(&batch).unwrap();

    assert!(!batch_text.contains(VIKING_SEARCH_TOOL));
    assert!(!batch_text.contains("Old OpenViking memory content"));
    assert!(batch_text.contains("shell_command"));
    assert!(batch_text.contains("plugins/memory/openviking/__init__.py"));
}

#[test]
fn empty_recall_tool_id_does_not_skip_other_empty_id_tool_results() {
    let turn = vec![
        json!({"role": "user", "content": "Run tools."}),
        json!({
            "role": "tool",
            "tool_call_id": "",
            "name": VIKING_SEARCH_TOOL,
            "content": "recalled old memory",
        }),
        json!({
            "role": "tool",
            "tool_call_id": "",
            "name": "shell_command",
            "content": "fresh shell output",
        }),
    ];

    let batch = messages_to_openviking_batch(&turn, None);
    let batch_text = serde_json::to_string(&batch).unwrap();

    assert!(!batch_text.contains("recalled old memory"));
    assert!(batch_text.contains("fresh shell output"));
}

#[test]
fn messages_to_openviking_batch_preserves_responses_text_parts_and_peer_id() {
    let turn = vec![
        json!({"role": "user", "content": [{"type": "input_text", "text": "hello"}]}),
        json!({"role": "assistant", "content": [{"type": "output_text", "text": "answer"}]}),
    ];

    let batch = messages_to_openviking_batch(&turn, Some("hermes"));

    assert_eq!(
        batch,
        vec![
            json!({"role": "user", "parts": [{"type": "text", "text": "hello"}]}),
            json!({"role": "assistant", "parts": [{"type": "text", "text": "answer"}], "peer_id": "hermes"}),
        ]
    );
}

#[test]
fn fallback_turn_batch_preserves_empty_assistant_turn() {
    let batch = fallback_turn_batch("hello", "", "hermes");

    assert_eq!(
        batch,
        vec![
            json!({"role": "user", "parts": [{"type": "text", "text": "hello"}]}),
            json!({"role": "assistant", "parts": [{"type": "text", "text": ""}], "peer_id": "hermes"}),
        ]
    );
}

#[test]
fn rust_flattened_tool_calls_reuse_cached_top_level_arguments() {
    let turn = vec![
        json!({"role": "user", "content": "Run it."}),
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_terminal",
                "name": "terminal",
                "arguments": serde_json::to_string(&json!({"cmd": "pwd"})).unwrap(),
            }],
        }),
        json!({
            "role": "tool",
            "tool_call_id": "call_terminal",
            "name": "terminal",
            "content": "/repo",
        }),
    ];

    let batch = messages_to_openviking_batch(&turn, None);

    assert_eq!(batch[1]["parts"][0]["tool_name"], "terminal");
    assert_eq!(batch[1]["parts"][0]["tool_input"], json!({"cmd": "pwd"}));
    assert_eq!(batch[1]["parts"][0]["tool_status"], TOOL_STATUS_COMPLETED);
}

#[test]
fn object_tool_outputs_are_preserved_as_json_text() {
    let turn = vec![
        json!({"role": "user", "content": "Inspect structured output."}),
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_structured",
                "name": "structured_tool",
                "arguments": "{}",
            }],
        }),
        json!({
            "role": "tool",
            "tool_call_id": "call_structured",
            "name": "structured_tool",
            "content": {"answer": "kept", "success": true},
        }),
    ];

    let batch = messages_to_openviking_batch(&turn, None);

    assert_eq!(
        batch[1]["parts"][0]["tool_output"],
        json!({"answer": "kept", "success": true}).to_string()
    );
    assert_eq!(batch[1]["parts"][0]["tool_status"], TOOL_STATUS_COMPLETED);
}

#[test]
fn headers_include_agent_and_bearer_key() {
    let st = VikingState {
        client: Client::new(),
        endpoint: DEFAULT_ENDPOINT.to_string(),
        api_key: "secret".to_string(),
        account: "acct".to_string(),
        user: "user".to_string(),
        agent: "agent".to_string(),
        session_id: "session".to_string(),
        turn_count: 0,
    };
    let headers = viking_headers(&st);
    assert_eq!(headers["X-OpenViking-Agent"], "agent");
    assert_eq!(headers["X-API-Key"], "secret");
    assert_eq!(headers["Authorization"], "Bearer secret");
}

#[test]
fn content_write_body_uses_user_scoped_create_uri() {
    let st = VikingState {
        client: Client::new(),
        endpoint: DEFAULT_ENDPOINT.to_string(),
        api_key: String::new(),
        account: "acct".to_string(),
        user: "she/a".to_string(),
        agent: "hermes ultra".to_string(),
        session_id: "session".to_string(),
        turn_count: 0,
    };
    let body = content_write_body(&st, "patterns", "fact");
    let uri = body["uri"].as_str().expect("uri");
    assert!(uri.starts_with("viking://user/she_a/memories/patterns/"));
    assert_eq!(body["content"], "fact");
    assert_eq!(body["mode"], "create");
}

#[test]
fn session_switch_updates_session_and_clears_prefetch() {
    let plugin = OpenVikingMemoryPlugin::new();
    *plugin.state.lock().unwrap() = Some(VikingState {
        client: Client::new(),
        endpoint: DEFAULT_ENDPOINT.to_string(),
        api_key: String::new(),
        account: "acct".to_string(),
        user: "user".to_string(),
        agent: "agent".to_string(),
        session_id: "old".to_string(),
        turn_count: 0,
    });
    *plugin.prefetch.lock().unwrap() = "stale".to_string();

    plugin.on_session_switch("new", "old", false);

    let state = plugin.state.lock().unwrap().clone().expect("state");
    assert_eq!(state.session_id, "new");
    assert_eq!(state.turn_count, 0);
    assert!(plugin.prefetch.lock().unwrap().is_empty());
}

#[test]
fn drain_writers_waits_for_all_finished_session_writers() {
    let plugin = OpenVikingMemoryPlugin::new();
    plugin.spawn_session_writer("sid".to_string(), || {});
    plugin.spawn_session_writer("sid".to_string(), || {});

    assert!(drain_writers_for_session(
        &plugin.inflight_writers,
        "sid",
        Duration::from_secs(1)
    ));
    assert!(plugin.inflight_writers.lock().unwrap().get("sid").is_none());
}

#[test]
fn session_end_skips_commit_when_writer_outlives_drain() {
    let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
    let _timeout = EnvGuard::set("OPENVIKING_SESSION_DRAIN_TIMEOUT_MS", "1");
    let plugin = OpenVikingMemoryPlugin::new();
    *plugin.state.lock().unwrap() = Some(VikingState {
        client: Client::new(),
        endpoint: "http://127.0.0.1:9".to_string(),
        api_key: String::new(),
        account: "acct".to_string(),
        user: "user".to_string(),
        agent: "agent".to_string(),
        session_id: "old".to_string(),
        turn_count: 2,
    });
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    plugin.spawn_session_writer("old".to_string(), move || {
        let _ = release_rx.recv();
    });

    plugin.on_session_end(&[]);

    assert_eq!(
        plugin
            .state
            .lock()
            .unwrap()
            .as_ref()
            .expect("state")
            .turn_count,
        2
    );
    release_tx.send(()).expect("release writer");
    assert!(drain_writers_for_session(
        &plugin.inflight_writers,
        "old",
        Duration::from_secs(1)
    ));
}

#[test]
fn session_switch_rotates_without_waiting_for_old_writer() {
    let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
    let _timeout = EnvGuard::set("OPENVIKING_SESSION_DRAIN_TIMEOUT_MS", "1");
    let plugin = OpenVikingMemoryPlugin::new();
    *plugin.state.lock().unwrap() = Some(VikingState {
        client: Client::new(),
        endpoint: "http://127.0.0.1:9".to_string(),
        api_key: String::new(),
        account: "acct".to_string(),
        user: "user".to_string(),
        agent: "agent".to_string(),
        session_id: "old".to_string(),
        turn_count: 2,
    });
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    plugin.spawn_session_writer("old".to_string(), move || {
        let _ = release_rx.recv();
    });
    let start = Instant::now();

    plugin.on_session_switch("new", "old", false);

    assert!(start.elapsed() < Duration::from_millis(100));
    let state = plugin.state.lock().unwrap().clone().expect("state");
    assert_eq!(state.session_id, "new");
    assert_eq!(state.turn_count, 0);
    release_tx.send(()).expect("release writer");
    assert!(drain_writers_for_session(
        &plugin.inflight_writers,
        "old",
        Duration::from_secs(1)
    ));
}

#[test]
fn add_resource_payload_routes_remote_url_as_path() {
    let (body, upload) = add_resource_payload_for_source(
        "https://example.com/doc.md",
        &json!({"reason": "docs", "wait": true}),
    )
    .expect("payload");

    assert_eq!(body["path"], "https://example.com/doc.md");
    assert_eq!(body["reason"], "docs");
    assert_eq!(body["wait"], true);
    assert!(upload.is_none());
}

#[test]
fn add_resource_payload_uploads_existing_local_file_and_file_uri() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sample = tmp.path().join("sample file.md");
    std::fs::write(&sample, "# Local\n").expect("write sample");

    let (body, upload) =
        add_resource_payload_for_source(sample.to_str().expect("sample path"), &json!({}))
            .expect("payload");
    assert_eq!(body["source_name"], "sample file.md");
    assert_eq!(upload.as_deref(), Some(sample.as_path()));

    let uri = format!("file://{}", sample.to_string_lossy().replace(' ', "%20"));
    let (body, upload) = add_resource_payload_for_source(&uri, &json!({"reason": "file uri"}))
        .expect("file uri payload");
    assert_eq!(body["source_name"], "sample file.md");
    assert_eq!(body["reason"], "file uri");
    assert_eq!(upload.as_deref(), Some(sample.as_path()));
}

#[test]
fn add_resource_payload_rejects_missing_local_path_and_to_parent_conflict() {
    let err = add_resource_payload_for_source("./definitely-missing-openviking.md", &json!({}))
        .expect_err("missing local path");
    assert!(err.contains("does not exist"));

    let err = add_resource_payload_for_source(
        "https://example.com/doc.md",
        &json!({"to": "viking://a", "parent": "viking://b"}),
    )
    .expect_err("to parent conflict");
    assert!(err.contains("Cannot specify both"));
}

#[test]
fn add_resource_payload_zips_directory_and_skips_symlinks() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let docs = tmp.path().join("docs");
    std::fs::create_dir_all(docs.join("nested")).expect("mkdir");
    std::fs::write(docs.join("guide.md"), "# Guide\n").expect("write guide");
    std::fs::write(docs.join("nested").join("api.md"), "# API\n").expect("write api");
    #[cfg(unix)]
    std::os::unix::fs::symlink(docs.join("guide.md"), docs.join("guide-link.md")).expect("symlink");

    let (body, upload) =
        add_resource_payload_for_source(docs.to_str().expect("docs path"), &json!({}))
            .expect("payload");
    let zip_path = upload.expect("zip path");
    assert_eq!(body["source_name"], "docs");
    assert!(zip_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("openviking_upload_")));

    let zip_file = std::fs::File::open(&zip_path).expect("open zip");
    let mut archive = zip::ZipArchive::new(zip_file).expect("zip archive");
    let mut names = Vec::new();
    for idx in 0..archive.len() {
        names.push(archive.by_index(idx).expect("zip entry").name().to_string());
    }
    assert!(names.contains(&"guide.md".to_string()));
    assert!(names.contains(&"nested/api.md".to_string()));
    assert!(!names.contains(&"guide-link.md".to_string()));

    std::fs::remove_file(zip_path).expect("cleanup zip");
}
