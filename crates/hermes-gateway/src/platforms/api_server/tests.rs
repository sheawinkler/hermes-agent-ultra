use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn spawn_one_request_server(
    mailbox: Arc<RwLock<ResponseMailbox>>,
    response_store: Arc<RwLock<ResponseStore>>,
    run_cancels: Arc<RwLock<RunCancelRegistry>>,
    run_store: Arc<RwLock<RunStore>>,
    cron_scheduler: Arc<CronScheduler>,
    inbound_tx: Arc<RwLock<Option<mpsc::Sender<ApiInboundRequest>>>>,
    auth_token: Option<String>,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let handle = tokio::spawn(async move {
        let (stream, peer) = listener.accept().await.expect("accept");
        let runtime = ApiServerRuntime {
            mailbox,
            response_store,
            run_cancels,
            run_store,
            cron_scheduler,
            inbound_tx,
            auth_token,
        };
        handle_connection(stream, peer, runtime)
            .await
            .expect("handle connection");
    });
    (addr, handle)
}

async fn read_http_response(mut stream: tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.expect("read response");
    String::from_utf8(bytes).expect("utf8 response")
}

struct ApiTestState {
    mailbox: Arc<RwLock<ResponseMailbox>>,
    response_store: Arc<RwLock<ResponseStore>>,
    run_cancels: Arc<RwLock<RunCancelRegistry>>,
    run_store: Arc<RwLock<RunStore>>,
    cron_scheduler: Arc<CronScheduler>,
    inbound_tx: Arc<RwLock<Option<mpsc::Sender<ApiInboundRequest>>>>,
    _cron_dir: tempfile::TempDir,
}

impl ApiTestState {
    fn new(tx: mpsc::Sender<ApiInboundRequest>) -> Self {
        let cron_dir = tempfile::tempdir().expect("cron tempdir");
        let cron_scheduler = Arc::new(hermes_cron::cron_scheduler_for_data_dir(
            cron_dir.path().join("cron"),
        ));
        Self {
            mailbox: Arc::new(RwLock::new(ResponseMailbox::default())),
            response_store: Arc::new(RwLock::new(ResponseStore::default())),
            run_cancels: Arc::new(RwLock::new(RunCancelRegistry::default())),
            run_store: Arc::new(RwLock::new(RunStore::default())),
            cron_scheduler,
            inbound_tx: Arc::new(RwLock::new(Some(tx))),
            _cron_dir: cron_dir,
        }
    }

    async fn roundtrip(&self, request: String, auth_token: Option<String>) -> String {
        let (addr, handle) = spawn_one_request_server(
            Arc::clone(&self.mailbox),
            Arc::clone(&self.response_store),
            Arc::clone(&self.run_cancels),
            Arc::clone(&self.run_store),
            Arc::clone(&self.cron_scheduler),
            Arc::clone(&self.inbound_tx),
            auth_token,
        )
        .await;

        let mut client = tokio::net::TcpStream::connect(addr).await.expect("connect");
        client
            .write_all(request.as_bytes())
            .await
            .expect("write request");
        client.shutdown().await.expect("shutdown write side");
        let response = read_http_response(client).await;
        handle.await.expect("server task");
        response
    }
}

fn json_body(response: &str) -> serde_json::Value {
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("http body");
    serde_json::from_str(body).expect("json body")
}

fn json_request(method: &str, path: &str, body: serde_json::Value) -> String {
    let body = body.to_string();
    format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
}

fn empty_request(method: &str, path: &str) -> String {
    format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n\r\n")
}

fn empty_request_with_bearer(method: &str, path: &str, token: &str) -> String {
    format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n\r\n")
}

fn throwaway_cron_scheduler() -> Arc<CronScheduler> {
    Arc::new(hermes_cron::cron_scheduler_for_data_dir(
        std::env::temp_dir().join(format!("hermes-api-server-test-{}", uuid::Uuid::new_v4())),
    ))
}

async fn wait_for_run_status(
    run_store: &Arc<RwLock<RunStore>>,
    run_id: &str,
    expected: &str,
) -> RunRecord {
    for _ in 0..50 {
        if let Some(record) = run_store.read().await.get(run_id) {
            if record.status == expected {
                return record;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    run_store
        .read()
        .await
        .get(run_id)
        .expect("run should exist")
}

#[test]
fn parse_content_length_is_case_insensitive() {
    let h = "POST /x HTTP/1.1\r\nHost: localhost\r\nContent-Length: 42\r\n\r\n";
    assert_eq!(parse_content_length(h), 42);
    let h2 = "POST /x HTTP/1.1\r\ncontent-length: 9\r\n\r\n";
    assert_eq!(parse_content_length(h2), 9);
}

#[test]
fn build_prompt_from_messages_preserves_single_user_prompt() {
    let msgs = vec![ChatMessage {
        role: "user".into(),
        content: "final prompt".into(),
    }];
    assert_eq!(
        build_prompt_from_messages(&msgs).as_deref(),
        Some("final prompt")
    );
}

#[test]
fn build_prompt_from_messages_preserves_multi_message_transcript() {
    let msgs = vec![
        ChatMessage {
            role: "system".into(),
            content: "rules".into(),
        },
        ChatMessage {
            role: "assistant".into(),
            content: "hello".into(),
        },
        ChatMessage {
            role: "user".into(),
            content: "final prompt".into(),
        },
    ];
    let rendered = build_prompt_from_messages(&msgs).expect("prompt should exist");
    assert!(rendered.contains("[SYSTEM]\nrules"));
    assert!(rendered.contains("[ASSISTANT]\nhello"));
    assert!(rendered.contains("[USER]\nfinal prompt"));
}

#[test]
fn build_prompt_from_messages_requires_user_message() {
    let msgs = vec![
        ChatMessage {
            role: "system".into(),
            content: "rules".into(),
        },
        ChatMessage {
            role: "assistant".into(),
            content: "hello".into(),
        },
    ];
    assert!(build_prompt_from_messages(&msgs).is_none());
}

#[test]
fn network_accessibility_classifies_ip_binds() {
    assert!(!is_network_accessible("127.0.0.1"));
    assert!(!is_network_accessible("::1"));
    assert!(!is_network_accessible("::ffff:127.0.0.1"));
    assert!(is_network_accessible("0.0.0.0"));
    assert!(is_network_accessible("::"));
    assert!(is_network_accessible("10.0.0.1"));
    assert!(is_network_accessible("::ffff:0.0.0.0"));
}

#[test]
fn network_accessibility_hostname_resolution_is_fail_closed() {
    assert!(!is_network_accessible_with_lookup("localhost", |_| {
        Ok(vec!["127.0.0.1".parse().expect("loopback should parse")])
    }));

    assert!(is_network_accessible_with_lookup(
        "dual-stack.local",
        |_| {
            Ok(vec![
                "127.0.0.1".parse().expect("loopback should parse"),
                "10.0.0.7".parse().expect("private ip should parse"),
            ])
        }
    ));

    assert!(is_network_accessible_with_lookup(
        "nonexistent.invalid",
        |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "resolution failed",
            ))
        }
    ));
}

#[test]
fn bind_guard_requires_token_only_for_network_accessible_hosts() {
    assert!(!requires_auth_token_for_bind("127.0.0.1", None));
    assert!(!requires_auth_token_for_bind("::1", Some(" ")));
    assert!(requires_auth_token_for_bind("0.0.0.0", None));
    assert!(requires_auth_token_for_bind("::", Some("")));
    assert!(!requires_auth_token_for_bind("0.0.0.0", Some("sk-test")));
}

#[test]
fn image_marker_message_with_caption() {
    let marker = image_marker_message("https://cdn.example.com/a.png", Some("Diagram"));
    assert_eq!(
        marker,
        "[image] https://cdn.example.com/a.png | caption=Diagram"
    );
}

#[test]
fn image_marker_message_without_caption() {
    let marker = image_marker_message("https://cdn.example.com/a.png", Some("   "));
    assert_eq!(marker, "[image] https://cdn.example.com/a.png");
}

#[test]
fn parse_stop_run_path_accepts_valid_route() {
    assert_eq!(
        parse_stop_run_path("/v1/runs/run_abc123/stop"),
        Some("run_abc123")
    );
}

#[test]
fn parse_stop_run_path_rejects_invalid_route() {
    assert_eq!(parse_stop_run_path("/v1/runs//stop"), None);
    assert_eq!(parse_stop_run_path("/v1/runs/run_abc123"), None);
    assert_eq!(parse_stop_run_path("/v1/chat/completions"), None);
}

#[test]
fn parse_run_routes_accept_only_expected_subresources() {
    assert_eq!(
        parse_get_run_path("/v1/runs/run_abc123"),
        Some("run_abc123")
    );
    assert_eq!(
        parse_run_events_path("/v1/runs/run_abc123/events"),
        Some("run_abc123")
    );
    assert_eq!(
        parse_run_approval_path("/v1/runs/run_abc123/approval"),
        Some("run_abc123")
    );
    assert_eq!(parse_get_run_path("/v1/runs/run_abc123/events"), None);
    assert_eq!(parse_run_events_path("/v1/runs/run_abc123"), None);
}

#[test]
fn api_boolish_fields_accept_quoted_false() {
    let chat: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
        "model": "hermes-agent",
        "messages": [{"role": "user", "content": "hello"}],
        "stream": "false",
    }))
    .expect("quoted false stream should deserialize");
    assert!(!chat.stream);

    let responses: ResponsesRequest = serde_json::from_value(serde_json::json!({
        "model": "hermes-agent",
        "input": "hello",
        "stream": "false",
        "store": "false",
    }))
    .expect("quoted false stream/store should deserialize");
    assert!(!responses.stream);
    assert!(!responses.store);

    let approval: RunApprovalRequest = serde_json::from_value(serde_json::json!({
        "choice": "once",
        "all": "false",
    }))
    .expect("quoted false all should deserialize");
    assert!(!approval.all);
}

#[test]
fn chat_message_content_arrays_normalize_to_text() {
    let chat: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
        "model": "hermes-agent",
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "first"},
                {"type": "image_url", "image_url": {"url": "https://example.com/img.png"}},
                {"type": "input_text", "text": "second"},
                42,
                true
            ]
        }]
    }))
    .expect("content array should normalize");

    assert_eq!(chat.messages[0].content, "first\nsecond\n42\nTrue");
}

#[test]
fn chat_message_content_normalization_is_bounded() {
    let many = (0..2000)
        .map(|_| serde_json::json!({"type": "text", "text": "x"}))
        .collect::<Vec<_>>();
    let chat: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
        "model": "hermes-agent",
        "messages": [{"role": "user", "content": many}]
    }))
    .expect("large content array should normalize");

    assert_eq!(chat.messages[0].content.matches('x').count(), 1000);

    let huge = "x".repeat(100_000);
    let chat: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
        "model": "hermes-agent",
        "messages": [{"role": "user", "content": huge}]
    }))
    .expect("huge string should normalize");
    assert_eq!(chat.messages[0].content.len(), MAX_CHAT_CONTENT_CHARS);
}

#[test]
fn response_store_deletes_and_evicts_conversation_mappings() {
    let mut store = ResponseStore::new(2);
    let stored = |text: &str| StoredApiResponse {
        response: serde_json::json!({"id": text}),
        conversation_history: vec![ChatMessage {
            role: "assistant".into(),
            content: text.into(),
        }],
    };

    store.put("resp_1", stored("one"));
    store.set_conversation("chat-a", "resp_1");
    assert_eq!(store.get_conversation("chat-a").as_deref(), Some("resp_1"));
    assert!(store.delete("resp_1"));
    assert_eq!(store.get_conversation("chat-a"), None);

    store.put("resp_2", stored("two"));
    store.set_conversation("chat-b", "resp_2");
    store.put("resp_3", stored("three"));
    store.set_conversation("chat-c", "resp_3");
    store.put("resp_4", stored("four"));

    assert!(store.get("resp_2").is_none());
    assert_eq!(store.get_conversation("chat-b"), None);
    assert_eq!(store.get_conversation("chat-c").as_deref(), Some("resp_3"));
}

#[test]
fn api_discovery_bodies_expose_capabilities_toolsets_and_headers() {
    let capabilities = capabilities_response_body();
    assert_eq!(
        capabilities["endpoints"]["skills"],
        serde_json::json!({"method": "GET", "path": "/v1/skills"})
    );
    assert_eq!(
        capabilities["endpoints"]["toolsets"],
        serde_json::json!({"method": "GET", "path": "/v1/toolsets"})
    );

    let toolsets = toolsets_response_body();
    let data = toolsets["data"].as_array().expect("toolset list");
    let api_server = data
        .iter()
        .find(|entry| entry["name"] == "hermes-api-server")
        .expect("hermes-api-server toolset");
    assert_eq!(api_server["enabled"], true);
    assert!(api_server["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .any(|tool| tool == "terminal"));

    let response =
        json_http_response(HTTP_OK, &serde_json::json!({"status": "ok"})).expect("json response");
    assert!(response.contains("X-Content-Type-Options: nosniff\r\n"));
    assert!(response.contains("Referrer-Policy: no-referrer\r\n"));
    assert!(response.contains("X-Frame-Options: DENY\r\n"));
    assert!(response
        .contains("Content-Security-Policy: default-src 'none'; frame-ancestors 'none'\r\n"));
}

#[test]
fn skill_discovery_reads_nested_skill_dirs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let skill_dir = tmp.path().join("creative").join("ascii-art");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "# ASCII Art\n\nGenerate terminal-friendly drawings.\n",
    )
    .expect("write skill");

    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    collect_skill_entries(tmp.path(), tmp.path(), &mut seen, &mut entries);

    assert_eq!(
        entries,
        vec![SkillListEntry {
            name: "ascii-art".into(),
            description: "Generate terminal-friendly drawings.".into(),
            category: "creative".into(),
        }]
    );
}

#[test]
fn responses_body_shape_matches_openai_responses_contract() {
    let body = make_responses_api_body("resp_abc", "hermes-agent", "done", Some("resp_prev"));
    assert_eq!(body["id"], "resp_abc");
    assert_eq!(body["object"], "response");
    assert_eq!(body["previous_response_id"], "resp_prev");
    assert_eq!(body["output"][0]["type"], "message");
    assert_eq!(body["output"][0]["content"][0]["type"], "output_text");
    assert_eq!(body["output"][0]["content"][0]["text"], "done");
}

#[tokio::test]
async fn api_discovery_endpoint_serves_capabilities_with_security_headers() {
    let (tx, _rx) = mpsc::channel(1);
    let (addr, handle) = spawn_one_request_server(
        Arc::new(RwLock::new(ResponseMailbox::default())),
        Arc::new(RwLock::new(ResponseStore::default())),
        Arc::new(RwLock::new(RunCancelRegistry::default())),
        Arc::new(RwLock::new(RunStore::default())),
        throwaway_cron_scheduler(),
        Arc::new(RwLock::new(Some(tx))),
        None,
    )
    .await;

    let mut client = tokio::net::TcpStream::connect(addr).await.expect("connect");
    client
        .write_all(b"GET /v1/capabilities HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .expect("write request");
    client.shutdown().await.expect("shutdown write side");
    let response = read_http_response(client).await;
    handle.await.expect("server task");

    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.contains("X-Content-Type-Options: nosniff\r\n"));
    assert!(response.contains("\"skills\":{\"method\":\"GET\",\"path\":\"/v1/skills\"}"));
    assert!(response.contains("\"toolsets\":{\"method\":\"GET\",\"path\":\"/v1/toolsets\"}"));
}

#[tokio::test]
async fn responses_endpoint_accepts_input_and_quoted_false_without_storing() {
    let mailbox = Arc::new(RwLock::new(ResponseMailbox::default()));
    let response_store = Arc::new(RwLock::new(ResponseStore::default()));
    let run_cancels = Arc::new(RwLock::new(RunCancelRegistry::default()));
    let (tx, mut rx) = mpsc::channel(1);
    let (addr, handle) = spawn_one_request_server(
        Arc::clone(&mailbox),
        Arc::clone(&response_store),
        Arc::clone(&run_cancels),
        Arc::new(RwLock::new(RunStore::default())),
        throwaway_cron_scheduler(),
        Arc::new(RwLock::new(Some(tx))),
        None,
    )
    .await;

    let body = serde_json::json!({
        "model": "hermes-agent",
        "input": "hello",
        "store": "false",
        "stream": "false",
    })
    .to_string();
    let request = format!(
        "POST /v1/responses HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );

    let mut client = tokio::net::TcpStream::connect(addr).await.expect("connect");
    client
        .write_all(request.as_bytes())
        .await
        .expect("write request");

    let inbound = rx.recv().await.expect("inbound request");
    assert_eq!(inbound.model.as_deref(), Some("hermes-agent"));
    assert_eq!(inbound.prompt, "hello");
    let sender = mailbox
        .read()
        .await
        .pending
        .get(&inbound.session_id)
        .cloned()
        .expect("pending response sender");
    sender.send("done".to_string()).await.expect("send reply");

    let response = read_http_response(client).await;
    handle.await.expect("server task");

    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(!response.contains("text/event-stream"));
    assert!(response.contains("\"object\":\"response\""));
    assert!(response.contains("\"text\":\"done\""));
    assert!(response_store.read().await.entries.is_empty());
}

#[tokio::test]
async fn api_jobs_endpoint_crud_filters_and_actions() {
    let (tx, _rx) = mpsc::channel(1);
    let state = ApiTestState::new(tx);

    let create_response = state
        .roundtrip(
            json_request(
                "POST",
                "/api/jobs",
                serde_json::json!({
                    "name": "test-job",
                    "schedule": "*/5 * * * *",
                    "prompt": "do something",
                    "repeat": 2,
                }),
            ),
            None,
        )
        .await;
    assert!(create_response.starts_with("HTTP/1.1 200 OK"));
    let created = json_body(&create_response);
    let job_id = created["job"]["id"].as_str().expect("job id").to_string();
    assert_eq!(created["job"]["name"], "test-job");
    assert_eq!(created["job"]["deliver"], "local");
    assert_eq!(created["job"]["enabled"], true);

    let list_response = state
        .roundtrip(empty_request("GET", "/api/jobs"), None)
        .await;
    let list = json_body(&list_response);
    assert_eq!(list["jobs"].as_array().expect("jobs").len(), 1);

    let update_response = state
        .roundtrip(
            json_request(
                "PATCH",
                &format!("/api/jobs/{job_id}"),
                serde_json::json!({
                    "name": "updated-name",
                    "skill": "browser",
                    "evil_field": "ignored",
                    "__proto__": "ignored",
                }),
            ),
            None,
        )
        .await;
    assert!(update_response.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(json_body(&update_response)["job"]["name"], "updated-name");
    assert_eq!(
        json_body(&update_response)["job"]["skills"],
        serde_json::json!(["browser"])
    );

    let no_valid_update_response = state
        .roundtrip(
            json_request(
                "PATCH",
                &format!("/api/jobs/{job_id}"),
                serde_json::json!({"evil_field": "ignored"}),
            ),
            None,
        )
        .await;
    assert!(no_valid_update_response.starts_with("HTTP/1.1 400 Bad Request"));

    let pause_response = state
        .roundtrip(
            json_request(
                "POST",
                &format!("/api/jobs/{job_id}/pause"),
                serde_json::json!({}),
            ),
            None,
        )
        .await;
    assert!(pause_response.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(json_body(&pause_response)["job"]["enabled"], false);

    let filtered_response = state
        .roundtrip(empty_request("GET", "/api/jobs"), None)
        .await;
    assert!(json_body(&filtered_response)["jobs"]
        .as_array()
        .expect("jobs")
        .is_empty());

    let include_disabled_response = state
        .roundtrip(
            empty_request("GET", "/api/jobs?include_disabled=true"),
            None,
        )
        .await;
    assert_eq!(
        json_body(&include_disabled_response)["jobs"]
            .as_array()
            .expect("jobs")
            .len(),
        1
    );

    let resume_response = state
        .roundtrip(
            json_request(
                "POST",
                &format!("/api/jobs/{job_id}/resume"),
                serde_json::json!({}),
            ),
            None,
        )
        .await;
    assert!(resume_response.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(json_body(&resume_response)["job"]["enabled"], true);

    let delete_response = state
        .roundtrip(
            empty_request("DELETE", &format!("/api/jobs/{job_id}")),
            None,
        )
        .await;
    assert!(delete_response.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(json_body(&delete_response)["ok"], true);

    let missing_response = state
        .roundtrip(empty_request("GET", &format!("/api/jobs/{job_id}")), None)
        .await;
    assert!(missing_response.starts_with("HTTP/1.1 404 Not Found"));
}

#[tokio::test]
async fn api_jobs_endpoint_validates_input_ids_and_auth() {
    let (tx, _rx) = mpsc::channel(1);
    let state = ApiTestState::new(tx);

    let missing_name_response = state
        .roundtrip(
            json_request(
                "POST",
                "/api/jobs",
                serde_json::json!({
                    "schedule": "*/5 * * * *",
                    "prompt": "do something",
                }),
            ),
            None,
        )
        .await;
    assert!(missing_name_response.starts_with("HTTP/1.1 400 Bad Request"));

    let unknown_field_response = state
        .roundtrip(
            json_request(
                "PATCH",
                "/api/jobs/aabbccddeeff",
                serde_json::json!({"evil_field": "ignored"}),
            ),
            None,
        )
        .await;
    assert!(unknown_field_response.starts_with("HTTP/1.1 404 Not Found"));

    let invalid_id_response = state
        .roundtrip(empty_request("GET", "/api/jobs/not-a-valid-hex!"), None)
        .await;
    assert!(invalid_id_response.starts_with("HTTP/1.1 400 Bad Request"));

    let auth_response = state
        .roundtrip(empty_request("GET", "/api/jobs"), Some("sk-secret".into()))
        .await;
    assert!(auth_response.starts_with("HTTP/1.1 401 Unauthorized"));

    let authed_response = state
        .roundtrip(
            empty_request_with_bearer("GET", "/api/jobs", "sk-secret"),
            Some("sk-secret".into()),
        )
        .await;
    assert!(authed_response.starts_with("HTTP/1.1 200 OK"));
}

#[tokio::test]
async fn cron_fire_endpoint_uses_nas_auth_not_generic_api_token() {
    let (tx, _rx) = mpsc::channel(1);
    let state = ApiTestState::new(tx);

    let response = state
        .roundtrip(
            json_request(
                "POST",
                "/api/cron/fire",
                serde_json::json!({"job_id": "job-1"}),
            ),
            Some("api-secret".to_string()),
        )
        .await;
    assert!(response.starts_with("HTTP/1.1 401 Unauthorized"));
    let body = json_body(&response);
    assert_eq!(
        body["error"]["message"].as_str(),
        Some("Missing Chronos bearer token")
    );
}

#[tokio::test]
async fn api_jobs_endpoint_manual_run_updates_job_state() {
    let (tx, _rx) = mpsc::channel(1);
    let state = ApiTestState::new(tx);
    let create_response = state
        .roundtrip(
            json_request(
                "POST",
                "/api/jobs",
                serde_json::json!({
                    "name": "script-job",
                    "schedule": "*/5 * * * *",
                    "prompt": "run script",
                    "script": "printf job-ok",
                    "no_agent": true,
                }),
            ),
            None,
        )
        .await;
    assert!(create_response.starts_with("HTTP/1.1 200 OK"));
    let job_id = json_body(&create_response)["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();

    let run_response = state
        .roundtrip(
            json_request(
                "POST",
                &format!("/api/jobs/{job_id}/run"),
                serde_json::json!({}),
            ),
            None,
        )
        .await;
    assert!(run_response.starts_with("HTTP/1.1 200 OK"));
    let run = json_body(&run_response);
    assert_eq!(run["job"]["id"], job_id);
    assert!(run["job"]["last_run"].is_string());
}

#[tokio::test]
async fn runs_endpoint_starts_completes_status_and_events() {
    let (tx, mut rx) = mpsc::channel(1);
    let state = ApiTestState::new(tx);
    let start_response = state
        .roundtrip(
            json_request(
                "POST",
                "/v1/runs",
                serde_json::json!({
                    "model": "hermes-agent",
                    "input": "hello",
                    "session_id": "space-session",
                }),
            ),
            None,
        )
        .await;

    assert!(start_response.starts_with("HTTP/1.1 202 Accepted"));
    let start = json_body(&start_response);
    assert_eq!(start["status"], "started");
    let run_id = start["run_id"].as_str().expect("run id").to_string();
    assert!(run_id.starts_with("run_"));

    let inbound = rx.recv().await.expect("inbound run");
    assert_eq!(inbound.request_id, run_id);
    assert_eq!(inbound.session_id, "space-session");
    assert_eq!(inbound.prompt, "hello");

    let sender = state
        .mailbox
        .read()
        .await
        .pending
        .get("space-session")
        .cloned()
        .expect("pending run response");
    sender.send("done".to_string()).await.expect("send reply");
    let record = wait_for_run_status(&state.run_store, &run_id, "completed").await;
    assert_eq!(record.output.as_deref(), Some("done"));

    let status_response = state
        .roundtrip(empty_request("GET", &format!("/v1/runs/{run_id}")), None)
        .await;
    assert!(status_response.starts_with("HTTP/1.1 200 OK"));
    let status = json_body(&status_response);
    assert_eq!(status["run_id"], run_id);
    assert_eq!(status["status"], "completed");
    assert_eq!(status["session_id"], "space-session");
    assert_eq!(status["output"], "done");
    assert_eq!(status["last_event"], "run.completed");
    assert!(status["usage"]["total_tokens"].as_u64().unwrap_or_default() > 0);

    let events_response = state
        .roundtrip(
            empty_request("GET", &format!("/v1/runs/{run_id}/events")),
            None,
        )
        .await;
    assert!(events_response.starts_with("HTTP/1.1 200 OK"));
    assert!(events_response.contains("Content-Type: text/event-stream"));
    assert!(events_response.contains("event: run.completed"));
    assert!(events_response.contains("\"output\":\"done\""));
    assert!(events_response.contains("data: [DONE]"));
}

#[tokio::test]
async fn runs_endpoint_rejects_invalid_history_without_allocating_run() {
    let (tx, _rx) = mpsc::channel(1);
    let state = ApiTestState::new(tx);
    let response = state
        .roundtrip(
            json_request(
                "POST",
                "/v1/runs",
                serde_json::json!({
                    "input": "hello",
                    "conversation_history": {"role": "user"},
                }),
            ),
            None,
        )
        .await;

    assert!(response.starts_with("HTTP/1.1 400 Bad Request"));
    assert!(state.run_store.read().await.records.is_empty());
}

#[tokio::test]
async fn runs_stop_marks_active_run_cancelled_and_events_emit_failure() {
    let (tx, mut rx) = mpsc::channel(1);
    let state = ApiTestState::new(tx);
    let start_response = state
        .roundtrip(
            json_request("POST", "/v1/runs", serde_json::json!({"input": "hold"})),
            None,
        )
        .await;
    let run_id = json_body(&start_response)["run_id"]
        .as_str()
        .expect("run id")
        .to_string();
    let inbound = rx.recv().await.expect("inbound run");
    assert_eq!(inbound.request_id, run_id);

    let stop_response = state
        .roundtrip(
            json_request(
                "POST",
                &format!("/v1/runs/{run_id}/stop"),
                serde_json::json!({}),
            ),
            None,
        )
        .await;
    assert!(stop_response.starts_with("HTTP/1.1 200 OK"));
    let stop = json_body(&stop_response);
    assert_eq!(stop["run_id"], run_id);
    assert_eq!(stop["status"], "stopping");

    let record = wait_for_run_status(&state.run_store, &run_id, "cancelled").await;
    assert_eq!(record.last_event.as_deref(), Some("run.failed"));

    let status_response = state
        .roundtrip(empty_request("GET", &format!("/v1/runs/{run_id}")), None)
        .await;
    let status = json_body(&status_response);
    assert_eq!(status["status"], "cancelled");

    let events_response = state
        .roundtrip(
            empty_request("GET", &format!("/v1/runs/{run_id}/events")),
            None,
        )
        .await;
    assert!(events_response.contains("event: run.failed"));
    assert!(events_response.contains("Run stopped"));
}

#[tokio::test]
async fn runs_approval_without_pending_returns_conflict() {
    let (tx, mut rx) = mpsc::channel(1);
    let state = ApiTestState::new(tx);
    let start_response = state
        .roundtrip(
            json_request("POST", "/v1/runs", serde_json::json!({"input": "hello"})),
            None,
        )
        .await;
    let run_id = json_body(&start_response)["run_id"]
        .as_str()
        .expect("run id")
        .to_string();

    let inbound = rx.recv().await.expect("inbound run");
    let sender = state
        .mailbox
        .read()
        .await
        .pending
        .get(&inbound.session_id)
        .cloned()
        .expect("pending run response");
    sender.send("done".to_string()).await.expect("send reply");
    let _record = wait_for_run_status(&state.run_store, &run_id, "completed").await;

    let approval_response = state
        .roundtrip(
            json_request(
                "POST",
                &format!("/v1/runs/{run_id}/approval"),
                serde_json::json!({"choice": "once", "all": "false"}),
            ),
            None,
        )
        .await;
    assert!(approval_response.starts_with("HTTP/1.1 409 Conflict"));
    let body = json_body(&approval_response);
    assert_eq!(body["error"]["code"], "409");
    assert_eq!(body["error"]["type"], "approval_not_pending");
}

#[tokio::test]
async fn runs_endpoints_require_bearer_auth_when_configured() {
    let (tx, _rx) = mpsc::channel(1);
    let state = ApiTestState::new(tx);
    let response = state
        .roundtrip(
            json_request("POST", "/v1/runs", serde_json::json!({"input": "hello"})),
            Some("sk-secret".to_string()),
        )
        .await;
    assert!(response.starts_with("HTTP/1.1 401 Unauthorized"));
    assert!(state.run_store.read().await.records.is_empty());
}
