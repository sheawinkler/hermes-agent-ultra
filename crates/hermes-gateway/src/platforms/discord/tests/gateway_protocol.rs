use std::sync::{Arc, atomic::{AtomicUsize, Ordering as AtomicOrdering}};

use tokio::time::{sleep, Duration, Instant};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};
use crate::gateway::GatewayConfig;
use crate::session::{SessionConfig, SessionManager};
use crate::Gateway;

#[derive(Clone)]
struct DiscordLogCapture(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for DiscordLogCapture {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| std::io::Error::other("Discord log capture lock poisoned"))?
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct RateLimitOnce {
    attempts: AtomicUsize,
}

impl Respond for RateLimitOnce {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        if self.attempts.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "0")
                .set_body_json(serde_json::json!({ "retry_after": 0.0 }))
        } else {
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "after-rate-limit",
                "channel_id": "channel-1"
            }))
        }
    }
}

struct DelayedRateLimitOnce {
    attempts: AtomicUsize,
}

impl Respond for DelayedRateLimitOnce {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        if self.attempts.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "0.1")
                .set_body_json(serde_json::json!({ "retry_after": 0.1 }))
        } else {
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "after-quiesce",
                "channel_id": "pending"
            }))
        }
    }
}

struct MediaFallbackSequence {
    requests: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    attempts: AtomicUsize,
}

impl Respond for MediaFallbackSequence {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body = serde_json::from_slice(&request.body)
            .expect("Discord media fallback requests use JSON");
        self.requests
            .lock()
            .expect("media fallback capture lock")
            .push(body);
        if self.attempts.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
            ResponseTemplate::new(400).set_body_string("embed content exceeds limit")
        } else {
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "fallback-message",
                "channel_id": "channel-1"
            }))
        }
    }
}

async fn attach_test_durable_ingress(adapter: &DiscordAdapter) {
    let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(4);
    let (journal_tx, mut journal_rx) = tokio::sync::mpsc::channel(4);
    adapter
        .set_durable_inbound_sender(inbound_tx, journal_tx)
        .await;
    tokio::spawn(async move {
        let _inbound_rx = inbound_rx;
        while let Some(event) = journal_rx.recv().await {
            if let DiscordIngressJournalEvent::Reserve(mut reservation) = event {
                reservation.acknowledge_durable(DiscordIngressReservationOutcome::Recorded);
            }
        }
    });
}

#[test]
fn media_methods_accept_metadata_contract() {
    let adapter = DiscordAdapter::new(test_config()).unwrap();
    let metadata = DiscordSendMetadata::with_thread_id("thread-1");

    let image_file = adapter.send_image_file(
        "channel-1",
        "/tmp/missing-image.png",
        Some("caption"),
        Some(&metadata),
    );
    drop(image_file);

    let image = adapter.send_image(
        "channel-1",
        "https://example.com/image.png",
        Some("caption"),
        Some(&metadata),
    );
    drop(image);

    let voice = adapter.send_voice(
        "channel-1",
        "/tmp/missing-audio.ogg",
        Some("caption"),
        Some(&metadata),
    );
    drop(voice);
}

#[test]
fn media_preflight_logs_redact_attachment_parent_paths() {
    let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
    let writer = DiscordLogCapture(Arc::clone(&captured));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .without_time()
        .with_writer(move || writer.clone())
        .finish();
    let secret_path = "/private/tenant-secret/hidden-parent/attachment.pdf";

    tracing::subscriber::with_default(subscriber, || {
        let missing = std::io::Error::new(std::io::ErrorKind::NotFound, "fixture missing");
        let _ = discord_local_media_preflight_failure(secret_path, &missing);
        let _ = discord_oversized_media_preflight_failure(
            secret_path,
            DISCORD_UPLOAD_FILE_LIMIT_BYTES + 1,
        );
    });

    let logs = String::from_utf8(captured.lock().expect("capture lock").clone())
        .expect("tracing output is UTF-8");
    assert!(
        !logs.contains("/private/tenant-secret/hidden-parent"),
        "media preflight logs must not expose the absolute attachment parent path: {logs}"
    );
    assert!(
        logs.contains("attachment.pdf"),
        "media preflight logs should retain only the safe attachment basename"
    );
}

#[test]
fn discord_reactions_target_the_thread_channel() {
    let adapter = DiscordAdapter::new(test_config()).expect("adapter");
    assert_eq!(
        PlatformAdapter::reaction_target_chat_id(&adapter, "parent", Some("thread")),
        "thread"
    );
    assert_eq!(
        PlatformAdapter::reaction_target_chat_id(&adapter, "parent", None),
        "parent"
    );
}

#[tokio::test]
async fn reactions_percent_encode_raw_keycap_emoji_in_wire_path() {
    let server = MockServer::start().await;
    let encoded_path =
        "/channels/channel-1/messages/message-1/reactions/%23%EF%B8%8F%E2%83%A3/@me";
    Mock::given(method("PUT"))
        .and(path(encoded_path))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(encoded_path))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new(cfg).expect("adapter");

    PlatformAdapter::add_reaction(&adapter, "channel-1", "message-1", "#️⃣")
        .await
        .expect("encoded add reaction");
    PlatformAdapter::remove_reaction(&adapter, "channel-1", "message-1", "#️⃣")
        .await
        .expect("encoded remove reaction");
}

#[tokio::test]
async fn discord_liveness_probe_uses_rest_users_me_and_bot_auth() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/@me"))
        .and(header("Authorization", "Bot test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "bot-self",
            "username": "Hermes"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    probe_discord_rest_liveness(&client, "test-token", &server.uri())
        .await
        .expect("liveness probe should succeed");
}

#[tokio::test]
async fn discord_liveness_stalled_response_is_bounded_and_marks_unhealthy() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/@me"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(1))
                .set_body_json(serde_json::json!({ "id": "bot-self" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (_stop_tx, stop) = tokio::sync::watch::channel(false);
    tokio::time::timeout(
        Duration::from_millis(250),
        run_discord_liveness_probe_loop_with_timeout(
            reqwest::Client::new(),
            "test-token".to_string(),
            server.uri(),
            Duration::from_millis(1),
            1,
            std::sync::Arc::clone(&failed),
            stop,
            Duration::from_millis(20),
        ),
    )
    .await
    .expect("stalled liveness probe should time out and count as a failure");
    assert!(failed.load(AtomicOrdering::SeqCst));
}



#[tokio::test]
async fn already_stopped_outbound_cancels_stalled_response_body_without_waiting_for_deadline() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stalled Discord REST fixture");
    let address = listener.local_addr().expect("fixture address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("body request");
        let mut request = [0_u8; 4096];
        let _ = socket.read(&mut request).await.expect("read request");
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\n",
            )
            .await
            .expect("write response headers");
        sleep(Duration::from_secs(1)).await;
    });

    let mut cfg = test_config();
    cfg.api_base_url = Some(format!("http://{address}"));
    let adapter = DiscordAdapter::new(cfg).expect("adapter");
    let response = adapter
        .client
        .get(format!("http://{address}/stalled-body"))
        .send()
        .await
        .expect("receive response headers");
    adapter.gateway_driver.outbound_stop_tx.send_replace(true);

    let error = tokio::time::timeout(
        Duration::from_millis(100),
        adapter.response_text_with_deadline(
            response,
            tokio::time::Instant::now() + Duration::from_secs(5),
            "Discord stalled body test",
        ),
    )
    .await
    .expect("an already-stopped adapter must not wait for the body deadline")
    .expect_err("stalled response body must be cancelled");
    assert!(error.to_string().contains("adapter is stopping"));
    server.abort();
}

#[tokio::test]
async fn accepted_thread_create_with_stalled_body_and_failed_recovery_is_ambiguous() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stalled Discord REST fixture");
    let address = listener.local_addr().expect("fixture address");
    let server = tokio::spawn(async move {
        let (mut created, _) = listener.accept().await.expect("create request");
        let mut request = [0_u8; 4096];
        let _ = created.read(&mut request).await.expect("read create request");
        created
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\n",
            )
            .await
            .expect("write accepted create headers");

        let (mut recovery, _) = listener.accept().await.expect("recovery request");
        let _ = recovery
            .read(&mut request)
            .await
            .expect("read recovery request");
        recovery
            .write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
            )
            .await
            .expect("write failed recovery response");
    });

    let mut cfg = test_config();
    cfg.api_base_url = Some(format!("http://{address}"));
    let adapter = DiscordAdapter::new(cfg).expect("adapter");
    let failure = adapter
        .create_thread_classified_until(
            "parent-1",
            "message-1",
            "Hermes",
            None,
            tokio::time::Instant::now() + Duration::from_millis(50),
        )
        .await
        .expect_err("accepted create without a body and failed recovery is ambiguous");
    assert!(matches!(
        failure,
        DiscordThreadCreationFailure::Ambiguous(_)
    ));
    server.await.expect("stalled REST fixture exits");
}

#[tokio::test]
async fn rate_limited_thread_create_is_retryable_without_speculative_recovery() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/parent-1/messages/message-1/threads"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "20")
                .set_body_json(serde_json::json!({ "retry_after": 20.0 })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/channels/message-1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new(cfg).expect("adapter");
    let failure = adapter
        .create_thread_classified_until(
            "parent-1",
            "message-1",
            "Hermes",
            None,
            tokio::time::Instant::now() + Duration::from_millis(50),
        )
        .await
        .expect_err("429 is proven to have no accepted thread creation");
    assert!(matches!(
        failure,
        DiscordThreadCreationFailure::Retryable(GatewayError::RateLimited { .. })
    ));
    server.verify().await;
}
#[tokio::test]
async fn discord_send_retries_a_429_and_preserves_the_accepted_message_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/channel-1/messages"))
        .respond_with(RateLimitOnce {
            attempts: AtomicUsize::new(0),
        })
        .expect(2)
        .mount(&server)
        .await;
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new(cfg).expect("adapter");

    assert_eq!(
        adapter
            .send_text("channel-1", "retry me")
            .await
            .expect("429 should be retried"),
        vec!["after-rate-limit"]
    );
}

#[tokio::test]
async fn discord_partial_delivery_retains_fractional_server_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/channel-1/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .set_body_json(serde_json::json!({ "retry_after": 20.125 })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new(cfg).expect("adapter");

    let error = adapter
        .send_text("channel-1", "preserve the provider retry hint")
        .await
        .expect_err("retry beyond the delivery deadline becomes a partial record");
    let GatewayError::PartialDelivery(partial) = error else {
        panic!("expected a durable Discord partial delivery");
    };
    assert!(partial.retryable);
    assert_eq!(
        partial.retry_after,
        Some(Duration::from_millis(20_125)),
        "the durable continuation must retain Discord's fractional retry hint"
    );
}

#[tokio::test]
async fn adapter_stop_interrupts_a_global_rate_limit_wait_before_a_second_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/channel-1/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "120")
                .set_body_json(serde_json::json!({
                    "retry_after": 120.0,
                    "global": true
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = Arc::new(DiscordAdapter::new(cfg).expect("adapter"));

    // The first request publishes Discord's account-wide 429 barrier, then
    // returns because the provider delay exceeds its own delivery deadline.
    adapter
        .send_text("channel-1", "establish global barrier")
        .await
        .expect_err("long global 429 cannot complete this delivery");
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("first request inspection")
            .len(),
        1
    );

    let wait_started = adapter.global_rate_limit.observe_next_wait();
    let sending = {
        let adapter = Arc::clone(&adapter);
        tokio::spawn(async move { adapter.send_text("channel-1", "wait for stop").await })
    };
    tokio::time::timeout(Duration::from_secs(1), wait_started)
        .await
        .expect("second send reaches the global barrier")
        .expect("barrier wait observer remains installed");
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("blocked request inspection")
            .len(),
        1,
        "the second send must be blocked before it can issue another POST"
    );

    PlatformAdapter::stop(adapter.as_ref())
        .await
        .expect("adapter stop");
    let err = tokio::time::timeout(Duration::from_secs(1), sending)
        .await
        .expect("rate-limit wait must be interruptible")
        .expect("send task joins")
        .expect_err("stopped adapter must cancel the delayed retry");
    assert!(err.to_string().contains("adapter is stopping"));
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("post-stop request inspection")
            .len(),
        1,
        "cancellation at the barrier must not issue a second request"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adapter_stop_drains_pre_stop_tracker_commit_before_taking_writer_slots() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/pre-stop-thread/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "accepted-before-stop",
            "channel_id": "pre-stop-thread"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state_dir = tempfile::tempdir().expect("isolated Discord tracker state");
    let participation_path = state_dir.path().join("threads.json");
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = Arc::new(
        DiscordAdapter::new_with_trackers(
            cfg,
            DiscordThreadParticipationTracker::from_path(&participation_path, 8),
            DiscordNonConversationalMessageTracker::from_path(
                state_dir.path().join("non-conversational.json"),
                8,
            ),
        )
        .expect("adapter with isolated trackers"),
    );

    // Keep the writer's snapshot clone blocked after Discord has accepted the
    // POST. This makes the accepted REST effect and its durable tracker commit
    // a deterministic stop race rather than a timing-dependent sleep.
    let tracker_guard = adapter
        .thread_participation
        .lock()
        .expect("thread tracker lock");
    let sending = {
        let adapter = Arc::clone(&adapter);
        tokio::spawn(async move {
            let metadata = DiscordSendMetadata::with_thread_id("pre-stop-thread");
            adapter
                .send_text_with_metadata("parent", "accepted before stop", Some(&metadata))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if adapter
                .thread_participation_writer
                .lock()
                .expect("thread writer lock")
                .is_some()
            {
                return;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("accepted POST reaches the blocked tracker writer");
    assert!(
        !sending.is_finished(),
        "the accepted send must still await durable tracker state"
    );

    let mut stopping = {
        let adapter = Arc::clone(&adapter);
        tokio::spawn(async move { PlatformAdapter::stop(adapter.as_ref()).await })
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let accepting = adapter
                .accepted_state_admission
                .state
                .lock()
                .expect("accepted-state admission lock")
                .accepting;
            if !accepting {
                return;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("stop closes accepted-state admission");
    let late_error = adapter
        .send_text("parent", "must not become a last stop accept")
        .await
        .expect_err("a send admitted after stop begins must be rejected");
    assert!(
        late_error.to_string().contains("adapter is stopping"),
        "late send must fail at the admission fence: {late_error}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut stopping)
            .await
            .is_err(),
        "stop must wait for the accepted send's durable tracker commit"
    );

    drop(tracker_guard);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), sending)
            .await
            .expect("accepted send drains after tracker lock releases")
            .expect("accepted send task joins")
            .expect("accepted send commits tracker state"),
        vec!["accepted-before-stop"]
    );
    tokio::time::timeout(Duration::from_secs(1), stopping)
        .await
        .expect("stop drains admitted tracker state")
        .expect("stop task joins")
        .expect("stop succeeds");
    assert!(
        DiscordThreadParticipationTracker::from_path(&participation_path, 8)
            .contains("pre-stop-thread"),
        "the accepted state must be durable before stop returns"
    );
    assert!(
        adapter
            .thread_participation_writer
            .lock()
            .expect("thread writer lock")
            .is_none(),
        "stop must join the late-created tracker writer"
    );
    assert!(
        adapter
            .non_conversational_writer
            .lock()
            .expect("non-conversational writer lock")
            .is_none(),
        "stop must leave no alternate tracker writer alive"
    );
    server.verify().await;
}

#[tokio::test]
async fn inbound_quiesce_keeps_active_and_rate_limited_outbound_delivery_live() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/active/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(120))
                .set_body_json(serde_json::json!({
                    "id": "active-after-quiesce",
                    "channel_id": "active"
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/channels/pending/messages"))
        .respond_with(DelayedRateLimitOnce {
            attempts: AtomicUsize::new(0),
        })
        .expect(2)
        .mount(&server)
        .await;

    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = Arc::new(DiscordAdapter::new(cfg).expect("adapter"));
    let active = {
        let adapter = Arc::clone(&adapter);
        tokio::spawn(async move { adapter.send_text("active", "active delivery").await })
    };
    let pending = {
        let adapter = Arc::clone(&adapter);
        tokio::spawn(async move { adapter.send_text("pending", "pending delivery").await })
    };
    for _ in 0..50 {
        if server
            .received_requests()
            .await
            .expect("request inspection")
            .len()
            >= 2
        {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }

    adapter
        .quiesce_inbound()
        .await
        .expect("socket intake quiesces without cancelling owned delivery");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), active)
            .await
            .expect("active route drains")
            .expect("active route task joins")
            .expect("active route succeeds"),
        vec!["active-after-quiesce"]
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .expect("rate-limited route drains")
            .expect("rate-limited route task joins")
            .expect("rate-limited route succeeds"),
        vec!["after-quiesce"]
    );
    PlatformAdapter::stop(adapter.as_ref())
        .await
        .expect("final adapter shutdown");
}

#[tokio::test]
async fn discord_liveness_failure_marks_adapter_offline_for_reconnect_watcher() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/@me"))
        .respond_with(ResponseTemplate::new(500).set_body_string("proxy wedged"))
        .mount(&server)
        .await;

    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    cfg.liveness_interval_seconds = 0.01;
    cfg.liveness_failure_threshold = 2;
    let adapter = DiscordAdapter::new(cfg).expect("adapter");
    attach_test_durable_ingress(&adapter).await;
    adapter.start().await.expect("start");
    assert!(adapter.is_running());

    let deadline = Instant::now() + Duration::from_secs(2);
    while adapter.is_running() && Instant::now() < deadline {
        sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !adapter.is_running(),
        "liveness failures should mark adapter offline for platform_reconnect_watcher"
    );
    adapter.stop().await.expect("stop");
}

#[tokio::test]
async fn failed_adapter_start_recloses_rest_and_a_later_durable_start_is_clean() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/channel-1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "unexpected",
            "channel_id": "channel-1"
        })))
        .expect(0)
        .mount(&server)
        .await;

    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    cfg.liveness_interval_seconds = 0.0;
    let adapter = DiscordAdapter::new(cfg).expect("adapter");

    let (inbound_tx, _inbound_rx) = tokio::sync::mpsc::channel(1);
    let (journal_tx, journal_rx) = tokio::sync::mpsc::channel(1);
    drop(journal_rx);
    adapter
        .set_durable_inbound_sender(inbound_tx, journal_tx)
        .await;

    let error = adapter
        .start()
        .await
        .expect_err("a closed durable journal must fail startup");
    assert!(error
        .to_string()
        .contains("durable ingress journal receiver is unavailable"));
    assert!(!adapter.is_running());
    assert!(
        *adapter.gateway_driver.stop_receiver().borrow(),
        "failed startup must re-close outbound REST"
    );

    let error = adapter
        .send_text("channel-1", "must not reach Discord")
        .await
        .expect_err("a failed startup must reject direct REST sends");
    assert!(error.to_string().contains("adapter is stopping"));

    attach_test_durable_ingress(&adapter).await;
    adapter
        .start()
        .await
        .expect("a new durable receiver allows a clean retry");
    assert!(adapter.is_running());
    adapter.stop().await.expect("clean retry stops");
    server.verify().await;
}

#[tokio::test]
async fn discord_liveness_probe_disabled_by_zero_knob() {
    let mut cfg = test_config();
    cfg.liveness_interval_seconds = 0.0;
    cfg.liveness_failure_threshold = 1;
    let adapter = DiscordAdapter::new(cfg).expect("adapter");
    attach_test_durable_ingress(&adapter).await;
    adapter.start().await.expect("start");
    assert!(adapter.is_running());
    assert!(adapter
        .liveness_task
        .lock()
        .expect("liveness lock")
        .is_none());
    adapter.stop().await.expect("stop");
}

#[test]
fn discord_liveness_interval_rejects_nonrepresentable_or_nonpositive_config_before_start() {
    assert_eq!(
        validate_discord_liveness_interval(0.0).expect("zero disables"),
        None
    );
    assert!(
        validate_discord_liveness_interval(0.01)
            .expect("small positive duration is valid")
            .is_some()
    );

    for seconds in [f64::NAN, -1.0, f64::INFINITY, 1e300] {
        let mut cfg = test_config();
        cfg.liveness_interval_seconds = seconds;
        let construction = std::panic::catch_unwind(|| DiscordAdapter::new(cfg));
        assert!(
            construction.is_ok(),
            "invalid interval {seconds:?} must return an error rather than panic"
        );
        assert!(
            construction.unwrap().is_err(),
            "invalid interval {seconds:?} must fail before any adapter/socket start"
        );
    }
}

#[tokio::test]
async fn dropping_adapter_aborts_its_liveness_task() {
    let mut cfg = test_config();
    cfg.liveness_interval_seconds = 60.0;
    cfg.liveness_failure_threshold = 1;
    let adapter = DiscordAdapter::new(cfg).expect("adapter");
    adapter.base.mark_running();
    adapter.start_liveness_probe();
    let abort = adapter
        .liveness_task
        .lock()
        .expect("liveness lock")
        .as_ref()
        .expect("liveness task")
        .abort_handle();

    drop(adapter);
    tokio::time::timeout(Duration::from_secs(1), async {
        while !abort.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("adapter drop aborts liveness task");
}


#[tokio::test]
async fn platform_image_options_route_native_images_to_the_thread() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/thread-image/messages"))
        .and(header("Authorization", "Bot test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "image-message",
            "channel_id": "thread-image"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state_dir = tempfile::tempdir().expect("isolated Discord state");
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new_with_trackers(
        cfg,
        DiscordThreadParticipationTracker::from_path(
            state_dir.path().join("threads.json"),
            DiscordThreadParticipationTracker::DEFAULT_MAX_TRACKED,
        ),
        DiscordNonConversationalMessageTracker::from_path(
            state_dir.path().join("non_conversational.json"),
            DiscordNonConversationalMessageTracker::DEFAULT_MAX_TRACKED,
        ),
    )
    .expect("adapter with isolated trackers");
    PlatformAdapter::send_image_url_with_options(
        &adapter,
        "parent-channel",
        "https://example.test/image.png",
        Some("caption"),
        SendMessageOptions::threaded(Some("thread-image")),
    )
    .await
    .expect("thread-targeted image delivery");
}

#[tokio::test]
async fn gateway_threaded_status_posts_to_discord_thread_not_parent_channel() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/thread-status/messages"))
        .and(body_json(serde_json::json!({ "content": "working" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "status-message",
            "channel_id": "thread-status"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/channels/thread-status/messages"))
        .and(body_json(serde_json::json!({ "content": "final" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "final-message",
            "channel_id": "thread-status"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/channels/parent-status/messages"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let state_dir = tempfile::tempdir().expect("isolated Discord state");
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = Arc::new(
        DiscordAdapter::new_with_state_root(cfg, state_dir.path())
            .expect("Discord adapter with isolated state"),
    );
    let gateway = Gateway::with_defaults(
        Arc::new(SessionManager::new(SessionConfig::default())),
        GatewayConfig::default(),
    );
    gateway
        .register_adapter("discord", adapter.clone())
        .await;

    gateway
        .send_or_update_status_threaded(
            "discord",
            "parent-status",
            "lifecycle",
            "working",
            None,
            Some("thread-status"),
        )
        .await
        .expect("threaded status delivery");
    assert!(
        !adapter.thread_participation_contains("thread-status"),
        "lifecycle status must not establish conversational thread participation"
    );
    adapter
        .send_text_with_metadata(
            "parent-status",
            "final",
            Some(&DiscordSendMetadata::with_thread_id("thread-status")),
        )
        .await
        .expect("real final reply");
    assert!(
        adapter.thread_participation_contains("thread-status"),
        "a real final reply establishes conversational thread participation"
    );
    server.verify().await;
}

#[tokio::test]
async fn rejected_oversized_discord_embed_falls_back_once_to_the_image_url() {
    let server = MockServer::start().await;
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/channels/channel-1/messages"))
        .respond_with(MediaFallbackSequence {
            requests: Arc::clone(&requests),
            attempts: AtomicUsize::new(0),
        })
        .expect(2)
        .mount(&server)
        .await;

    let state_dir = tempfile::tempdir().expect("isolated Discord state");
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = Arc::new(
        DiscordAdapter::new_with_state_root(cfg, state_dir.path())
            .expect("Discord adapter with isolated state"),
    );
    let gateway = Gateway::with_defaults(
        Arc::new(SessionManager::new(SessionConfig::default())),
        GatewayConfig::default(),
    );
    gateway
        .register_adapter("discord", adapter)
        .await;

    let image_url = "https://images.example.test/graph.png";
    let oversized_caption = "x".repeat(MAX_MESSAGE_LENGTH + 1);
    gateway
        .send_message(
            "discord",
            "channel-1",
            &format!("![{oversized_caption}]({image_url})"),
            None,
        )
        .await
        .expect("definitively rejected embed falls back safely");

    server.verify().await;
    let requests = requests.lock().expect("media fallback capture lock");
    assert_eq!(requests.len(), 2, "one rejected embed and one fallback post");
    assert_eq!(
        requests[0]["embeds"][0]["image"]["url"],
        serde_json::Value::String(image_url.to_string())
    );
    assert_eq!(
        requests[0]["content"],
        serde_json::Value::String(oversized_caption),
        "the initial native embed carries the full caption"
    );
    assert_eq!(
        requests[1]["content"],
        serde_json::Value::String(image_url.to_string()),
        "the fallback drops the oversized caption so it stays a single safe post"
    );
    assert!(
        requests[1].get("embeds").is_none(),
        "the fallback is plain text and cannot repeat the rejected embed effect"
    );
}


#[test]
fn discord_media_remote_rejection_only_classifies_conclusive_pre_effect_statuses() {
    assert!(matches!(
        discord_media_http_status_error(reqwest::StatusCode::BAD_REQUEST, "invalid embed".into()),
        GatewayError::MediaRemoteRejected { status: 400 }
    ));
    assert!(matches!(
        discord_media_http_status_error(reqwest::StatusCode::REQUEST_TIMEOUT, "late".into()),
        GatewayError::SendFailed(_)
    ));
    assert!(matches!(
        discord_media_http_status_error(reqwest::StatusCode::TOO_MANY_REQUESTS, "rate limit".into()),
        GatewayError::RateLimited { .. }
    ));
    assert!(matches!(
        discord_media_http_status_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "wedged".into()),
        GatewayError::ConnectionFailed(_)
    ));
}
#[tokio::test]
async fn malformed_accepted_file_upload_is_quarantined_without_a_duplicate_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/media-1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{not json"))
        .expect(1)
        .mount(&server)
        .await;

    let state_dir = tempfile::tempdir().expect("isolated Discord state");
    let file_path = state_dir.path().join("image.bin");
    std::fs::write(&file_path, b"image-bytes").expect("write fixture attachment");
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new_with_trackers(
        cfg,
        DiscordThreadParticipationTracker::from_path(
            state_dir.path().join("threads.json"),
            DiscordThreadParticipationTracker::DEFAULT_MAX_TRACKED,
        ),
        DiscordNonConversationalMessageTracker::from_path(
            state_dir.path().join("non_conversational.json"),
            DiscordNonConversationalMessageTracker::DEFAULT_MAX_TRACKED,
        ),
    )
    .expect("adapter with isolated trackers");

    let error = adapter
        .send_image_file("media-1", file_path.to_str().unwrap(), Some("caption"), None)
        .await
        .expect_err("an accepted malformed response is not safely retryable");
    let GatewayError::PartialDelivery(partial) = error else {
        panic!("expected a durable ambiguous media delivery");
    };
    assert_eq!(partial.accepted_chunks, 1);
    assert!(!partial.retryable);
    assert!(partial.remaining_text.is_empty());

    let resumed = PlatformAdapter::resume_partial_delivery(&adapter, &partial)
        .await
        .expect_err("ambiguous media delivery must remain quarantined");
    assert!(matches!(resumed, GatewayError::PartialDelivery(_)));
    server.verify().await;
}

#[tokio::test]
async fn stalled_accepted_embed_body_is_quarantined_before_delivery_deadline() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stalled response listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept embed request");
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await.expect("read embed request");
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\n",
            )
            .await
            .expect("write response headers");
        sleep(Duration::from_millis(150)).await;
    });

    let state_dir = tempfile::tempdir().expect("isolated Discord state");
    let mut cfg = test_config();
    cfg.api_base_url = Some(format!("http://{address}"));
    let adapter = DiscordAdapter::new_with_trackers(
        cfg,
        DiscordThreadParticipationTracker::from_path(
            state_dir.path().join("threads.json"),
            DiscordThreadParticipationTracker::DEFAULT_MAX_TRACKED,
        ),
        DiscordNonConversationalMessageTracker::from_path(
            state_dir.path().join("non_conversational.json"),
            DiscordNonConversationalMessageTracker::DEFAULT_MAX_TRACKED,
        ),
    )
    .expect("adapter with isolated trackers");
    let embeds = vec![DiscordEmbed::new()];

    let error = adapter
        .send_embed_with_metadata_until(
            "media-embed",
            Some("caption"),
            &embeds,
            None,
            Instant::now() + Duration::from_millis(50),
        )
        .await
        .expect_err("stalled accepted body must not wait past delivery budget");
    let GatewayError::PartialDelivery(partial) = error else {
        panic!("expected a durable ambiguous embed delivery");
    };
    assert_eq!(partial.accepted_chunks, 1);
    assert!(!partial.retryable);
    server.await.expect("stalled response fixture exits");
}

#[tokio::test]
async fn oversized_upload_is_rejected_before_a_request_or_full_file_read() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/media-large/messages"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let state_dir = tempfile::tempdir().expect("isolated Discord state");
    let file_path = state_dir.path().join("too-large.bin");
    std::fs::File::create(&file_path)
        .expect("create sparse attachment")
        .set_len(DISCORD_UPLOAD_FILE_LIMIT_BYTES + 1)
        .expect("grow sparse attachment metadata");
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new(cfg).expect("adapter");

    let error = adapter
        .upload_file("media-large", file_path.to_str().unwrap(), None)
        .await
        .expect_err("oversized metadata must fail before multipart construction");
    assert!(error.to_string().contains("exceeds"));
    server.verify().await;
}

#[tokio::test]
async fn file_upload_retries_a_429_with_a_reusable_bounded_attachment_payload() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/media-retry/messages"))
        .respond_with(RateLimitOnce {
            attempts: AtomicUsize::new(0),
        })
        .expect(2)
        .mount(&server)
        .await;

    let state_dir = tempfile::tempdir().expect("isolated Discord state");
    let file_path = state_dir.path().join("retry.bin");
    std::fs::write(&file_path, b"retryable attachment").expect("write fixture attachment");
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new(cfg).expect("adapter");

    assert_eq!(
        adapter
            .upload_file("media-retry", file_path.to_str().unwrap(), None)
            .await
            .expect("429 upload should rebuild multipart from retained bytes"),
        "after-rate-limit"
    );
    server.verify().await;
}

#[tokio::test]
async fn multipart_upload_transfer_can_outlive_its_rate_admission_deadline() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/media-slow/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(80))
                .set_body_json(serde_json::json!({
                    "id": "slow-upload",
                    "channel_id": "media-slow"
                })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let state_dir = tempfile::tempdir().expect("isolated Discord state");
    let file_path = state_dir.path().join("slow.bin");
    std::fs::write(&file_path, b"small multipart attachment").expect("write attachment");
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new_with_state_root(cfg, state_dir.path())
        .expect("adapter with isolated state");
    let now = tokio::time::Instant::now();

    assert_eq!(
        adapter
            .upload_file_with_metadata_until(
                "media-slow",
                file_path.to_str().expect("UTF-8 test path"),
                None,
                None,
                now + Duration::from_millis(20),
                now + Duration::from_millis(500),
            )
            .await
            .expect("multipart transfer uses its longer delivery deadline"),
        "slow-upload"
    );
    server.verify().await;
}
#[tokio::test]
async fn successful_thread_send_records_participation_but_failed_send_does_not() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/thread-ok/messages"))
        .and(header("Authorization", "Bot test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "message-ok",
            "channel_id": "thread-ok"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/channels/thread-fail/messages"))
        .and(header("Authorization", "Bot test-token"))
        .respond_with(ResponseTemplate::new(500).set_body_string("send rejected"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/channels/thread-malformed/messages"))
        .and(header("Authorization", "Bot test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{not json"))
        .expect(1)
        .mount(&server)
        .await;

    let state_dir = tempfile::tempdir().expect("isolated Discord state");
    let participation_path = state_dir.path().join("threads.json");
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new_with_trackers(
        cfg,
        DiscordThreadParticipationTracker::from_path(
            &participation_path,
            DiscordThreadParticipationTracker::DEFAULT_MAX_TRACKED,
        ),
        DiscordNonConversationalMessageTracker::from_path(
            state_dir.path().join("non_conversational.json"),
            DiscordNonConversationalMessageTracker::DEFAULT_MAX_TRACKED,
        ),
    )
    .expect("adapter with isolated trackers");

    let success = DiscordSendMetadata::with_thread_id("thread-ok");
    assert_eq!(
        adapter
            .send_text_with_metadata("parent", "sent", Some(&success))
            .await
            .expect("successful send"),
        vec!["message-ok"]
    );
    assert!(adapter.thread_participation_contains("thread-ok"));

    let failure = DiscordSendMetadata::with_thread_id("thread-fail");
    assert!(
        adapter
            .send_text_with_metadata("parent", "rejected", Some(&failure))
            .await
            .is_err()
    );
    assert!(!adapter.thread_participation_contains("thread-fail"));

    let malformed = DiscordSendMetadata::with_thread_id("thread-malformed");
    assert_eq!(
        adapter
            .send_text_with_metadata("parent", "accepted but malformed", Some(&malformed))
            .await
            .expect("a 2xx send must not be retried merely because its body lacks an ID"),
        Vec::<String>::new()
    );
    assert!(adapter.thread_participation_contains("thread-malformed"));

    let persisted = DiscordThreadParticipationTracker::from_path(
        participation_path,
        DiscordThreadParticipationTracker::DEFAULT_MAX_TRACKED,
    );
    assert!(persisted.contains("thread-ok"));
    assert!(!persisted.contains("thread-fail"));
}

#[tokio::test]
async fn accepted_status_send_with_undurable_tracker_state_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/thread-status/messages"))
        .and(header("Authorization", "Bot test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "accepted-status",
            "channel_id": "thread-status"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state_dir = tempfile::tempdir().expect("isolated Discord state");
    let non_conversational_path = state_dir.path().join("non-conversational.json");
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new_with_trackers(
        cfg,
        DiscordThreadParticipationTracker::from_path(state_dir.path().join("threads.json"), 8),
        DiscordNonConversationalMessageTracker::from_path(&non_conversational_path, 8),
    )
    .expect("adapter with isolated trackers");
    adapter
        .non_conversational_messages
        .lock()
        .expect("tracker lock")
        .fail_next_save_for_test();

    let metadata =
        DiscordSendMetadata::with_thread_id("thread-status").with_non_conversational(true);
    let error = adapter
        .send_text_with_metadata("parent", "accepted exactly once", Some(&metadata))
        .await
        .expect_err("an accepted send with failed durable state must remain explicit");

    match error {
        GatewayError::PartialDelivery(partial) => {
            assert_eq!(partial.accepted_message_ids, vec!["accepted-status"]);
            assert_eq!(partial.accepted_chunks, 1);
            assert_eq!(partial.next_chunk_index, 1);
            assert!(partial.remaining_text.is_empty());
            assert!(partial.retryable);
            assert!(
                partial.repair_required.is_some(),
                "accepted state repair must remain attached to the retained delivery record"
            );
        }
        other => panic!("expected accepted-but-undurable partial, got {other:?}"),
    }
    assert!(
        !adapter.non_conversational_message_contains("accepted-status"),
        "the in-memory tracker must not claim a durable update after its writer fails"
    );
    assert!(
        adapter
            .non_conversational_repair_pending
            .load(std::sync::atomic::Ordering::SeqCst),
        "the failed durable boundary must remain visible for shutdown reporting"
    );
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("request inspection")
            .len(),
        1,
        "the accepted POST must not be retried after a tracker persistence failure"
    );
    PlatformAdapter::stop(&adapter)
        .await
        .expect("shutdown retries the exact failed tracker snapshot");
    assert!(
        DiscordNonConversationalMessageTracker::from_path(&non_conversational_path, 8)
            .contains("accepted-status"),
        "shutdown repair must persist the accepted status ID from the failed staged snapshot"
    );
    assert!(
        !adapter
            .non_conversational_repair_pending
            .load(std::sync::atomic::Ordering::SeqCst),
        "only a successful fsync/replace may clear the pending repair flag"
    );
}
#[tokio::test]
async fn multichunk_tracker_failure_retains_only_the_exact_unsent_suffix() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/thread-status/messages"))
        .and(header("Authorization", "Bot test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "first-accepted",
            "channel_id": "thread-status"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state_dir = tempfile::tempdir().expect("isolated Discord state");
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new_with_trackers(
        cfg,
        DiscordThreadParticipationTracker::from_path(state_dir.path().join("threads.json"), 8),
        DiscordNonConversationalMessageTracker::from_path(
            state_dir.path().join("non-conversational.json"),
            8,
        ),
    )
    .expect("adapter with isolated trackers");
    adapter
        .thread_participation
        .lock()
        .expect("tracker lock")
        .fail_next_save_for_test();

    let suffix = "b".repeat(64);
    let content = format!("{}{}", "a".repeat(MAX_MESSAGE_LENGTH), suffix);
    let metadata = DiscordSendMetadata::with_thread_id("thread-status");
    let error = adapter
        .send_text_with_metadata("parent", &content, Some(&metadata))
        .await
        .expect_err("failed accepted-state persistence must retain the remaining suffix");

    match error {
        GatewayError::PartialDelivery(partial) => {
            assert_eq!(partial.accepted_message_ids, vec!["first-accepted"]);
            assert_eq!(partial.accepted_chunks, 1);
            assert_eq!(partial.next_chunk_index, 1);
            assert_eq!(partial.remaining_text, suffix);
            assert_eq!(partial.thread_id.as_deref(), Some("thread-status"));
            assert!(!partial.non_conversational);
            assert!(partial.retryable);
            assert!(
                partial
                    .repair_required
                    .as_deref()
                    .is_some_and(|repair| repair.contains("thread participation")),
                "the partial must retain the local-state repair requirement"
            );
        }
        other => panic!("expected retained partial delivery, got {other:?}"),
    }
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("request inspection")
            .len(),
        1,
        "the accepted prefix must never be retried while state repair is pending"
    );
}

#[tokio::test]
async fn accepted_ingress_liveness_failure_is_typed_retryable_before_thread_post() {
    let server = MockServer::start().await;
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new(cfg).expect("adapter");
    adapter
        .liveness_failed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let incoming = crate::gateway::IncomingMessage {
        platform: "discord".to_string(),
        chat_id: "parent-channel".to_string(),
        user_id: "user-1".to_string(),
        text: "please create a thread".to_string(),
        message_id: Some("source-message".to_string()),
        thread_id: None,
        is_dm: false,
    };

    match adapter.prepare_accepted_ingress(&incoming, false).await {
        Err(crate::gateway::AcceptedIngressPreparationError::Retryable {
            error: GatewayError::ConnectionFailed(message),
            retry_after: None,
        }) => assert!(message.contains("liveness")),
        other => panic!("expected typed pre-POST retryability, got {other:?}"),
    }
    assert!(
        server
            .received_requests()
            .await
            .expect("request inspection")
            .is_empty(),
        "a liveness rejection must not issue a create-thread POST"
    );
}

#[tokio::test]
async fn successful_forum_post_records_thread_participation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/forum-1/threads"))
        .and(header("Authorization", "Bot test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "forum-thread-1",
            "message": {
                "id": "forum-starter-1",
                "channel_id": "forum-thread-1"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state_dir = tempfile::tempdir().expect("isolated Discord state");
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new_with_trackers(
        cfg,
        DiscordThreadParticipationTracker::from_path(
            state_dir.path().join("threads.json"),
            DiscordThreadParticipationTracker::DEFAULT_MAX_TRACKED,
        ),
        DiscordNonConversationalMessageTracker::from_path(
            state_dir.path().join("non_conversational.json"),
            DiscordNonConversationalMessageTracker::DEFAULT_MAX_TRACKED,
        ),
    )
    .expect("adapter with isolated trackers");

    let outcome = adapter
        .send_forum_post("forum-1", "starter", None)
        .await
        .expect("forum thread creation");
    assert_eq!(outcome.thread_id, "forum-thread-1");
    assert_eq!(outcome.message_id, "forum-starter-1");
    assert!(adapter.thread_participation_contains("forum-thread-1"));
}

#[tokio::test]
async fn gateway_bot_rest_bootstrap_configures_identify_budget() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gateway/bot"))
        .and(header("Authorization", "Bot test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "url": "wss://gateway.discord.test",
            "session_start_limit": {
                "total": 1_000,
                "remaining": 37,
                "reset_after": 60_000,
                "max_concurrency": 1
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state_dir = tempfile::tempdir().expect("isolated Discord state");
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new_with_trackers(
        cfg,
        DiscordThreadParticipationTracker::from_path(
            state_dir.path().join("threads.json"),
            DiscordThreadParticipationTracker::DEFAULT_MAX_TRACKED,
        ),
        DiscordNonConversationalMessageTracker::from_path(
            state_dir.path().join("non_conversational.json"),
            DiscordNonConversationalMessageTracker::DEFAULT_MAX_TRACKED,
        ),
    )
    .expect("adapter with isolated trackers");

    adapter.configure_gateway_identify_session_start_limit().await;

    let pacer = adapter.gateway_driver.identify_pacer.lock().await;
    assert_eq!(pacer.total, Some(1_000));
    assert_eq!(pacer.remaining, Some(37));
    assert!(pacer.reset_at.is_some_and(|deadline| deadline > tokio::time::Instant::now()));
}

#[tokio::test]
async fn unknown_thread_parent_resolves_from_rest_and_invalid_channel_fails_closed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/channels/thread-api"))
        .and(header("Authorization", "Bot test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "thread-api",
            "guild_id": "guild-api",
            "type": 11,
            "parent_id": "parent-api"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/channels/thread-missing"))
        .respond_with(ResponseTemplate::new(404).set_body_string("unknown channel"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/channels/thread-wrong-guild"))
        .and(header("Authorization", "Bot test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "thread-wrong-guild",
            "guild_id": "different-guild",
            "type": 11,
            "parent_id": "parent-api"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state_dir = tempfile::tempdir().expect("isolated Discord state");
    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new_with_trackers(
        cfg,
        DiscordThreadParticipationTracker::from_path(
            state_dir.path().join("threads.json"),
            DiscordThreadParticipationTracker::DEFAULT_MAX_TRACKED,
        ),
        DiscordNonConversationalMessageTracker::from_path(
            state_dir.path().join("non_conversational.json"),
            DiscordNonConversationalMessageTracker::DEFAULT_MAX_TRACKED,
        ),
    )
    .expect("adapter with isolated trackers");
    let worker = adapter.gateway_driver.worker();

    let mut resolved = DiscordAdapter::parse_message_create(&serde_json::json!({
        "id": "message-api",
        "channel_id": "thread-api",
        "channel_type": 11,
        "guild_id": "guild-api",
        "content": "resolve me",
        "author": { "id": "human-api", "username": "human", "bot": false }
    }))
    .expect("thread message parses");
    worker
        .resolve_thread_parent_from_rest(&mut resolved)
        .await
        .expect("authoritative channel lookup succeeds");
    assert_eq!(resolved.thread_id.as_deref(), Some("thread-api"));
    assert_eq!(resolved.parent_channel_id.as_deref(), Some("parent-api"));

    let mut rejected = DiscordAdapter::parse_message_create(&serde_json::json!({
        "id": "message-missing",
        "channel_id": "thread-missing",
        "channel_type": 11,
        "guild_id": "guild-api",
        "content": "reject me",
        "author": { "id": "human-api", "username": "human", "bot": false }
    }))
    .expect("unknown thread message parses");
    let error = worker
        .resolve_thread_parent_from_rest(&mut rejected)
        .await
        .expect_err("404 channel lookup rejects the journal entry");
    assert!(matches!(error, DiscordThreadLookupError::Rejected { .. }));
    assert!(rejected.parent_channel_id.is_none());

    let mut cross_guild = DiscordAdapter::parse_message_create(&serde_json::json!({
        "id": "message-cross-guild",
        "channel_id": "thread-wrong-guild",
        "channel_type": 11,
        "guild_id": "guild-api",
        "content": "reject cross-guild context",
        "author": { "id": "human-api", "username": "human", "bot": false }
    }))
    .expect("thread message parses");
    let error = worker
        .resolve_thread_parent_from_rest(&mut cross_guild)
        .await
        .expect_err("a REST response from another guild must not resolve ingress");
    assert!(matches!(error, DiscordThreadLookupError::Rejected { .. }));
    assert!(cross_guild.parent_channel_id.is_none());
}

#[tokio::test]
async fn incomplete_successful_channel_lookups_preserve_journaled_ingress() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/channels/thread-malformed"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{not json"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/channels/thread-truncated"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "{{\"id\":\"{}\"}}",
            "x".repeat(DISCORD_REST_JSON_BODY_LIMIT_BYTES + 64)
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/channels/thread-no-guild"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "thread-no-guild",
            "type": 11,
            "parent_id": "parent"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/channels/thread-no-type"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "thread-no-type",
            "guild_id": "guild-api",
            "parent_id": "parent"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/channels/thread-no-parent"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "thread-no-parent",
            "guild_id": "guild-api",
            "type": 11
        })))
        .mount(&server)
        .await;

    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new(cfg).expect("adapter");
    let worker = adapter.gateway_driver.worker();
    for channel_id in [
        "thread-malformed",
        "thread-truncated",
        "thread-no-guild",
        "thread-no-type",
        "thread-no-parent",
    ] {
        let mut message = DiscordAdapter::parse_message_create(&serde_json::json!({
            "id": format!("message-{channel_id}"),
            "channel_id": channel_id,
            "channel_type": 11,
            "guild_id": "guild-api",
            "content": "preserve me",
            "author": { "id": "human-api", "username": "human", "bot": false }
        }))
        .expect("thread message parses");

        let error = worker
            .resolve_thread_parent_from_rest(&mut message)
            .await
            .expect_err("incomplete 2xx lookup must retain ingress");
        assert!(
            matches!(error, DiscordThreadLookupError::PreserveAndStop { .. }),
            "{channel_id} must not terminally reject a journaled message"
        );
        assert!(message.parent_channel_id.is_none());
    }
}

#[tokio::test]
async fn stalled_channel_lookup_body_preserves_journaled_ingress_without_wedging() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stalled channel fixture");
    let address = listener.local_addr().expect("fixture address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("lookup request");
        let mut request = [0_u8; 4096];
        let _ = socket.read(&mut request).await.expect("read lookup request");
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\n",
            )
            .await
            .expect("write lookup headers");
        sleep(Duration::from_secs(1)).await;
    });

    let mut cfg = test_config();
    cfg.api_base_url = Some(format!("http://{address}"));
    let adapter = DiscordAdapter::new(cfg).expect("adapter");
    let worker = adapter.gateway_driver.worker();
    let mut message = DiscordAdapter::parse_message_create(&serde_json::json!({
        "id": "message-stalled",
        "channel_id": "thread-stalled",
        "channel_type": 11,
        "guild_id": "guild-api",
        "content": "preserve me",
        "author": { "id": "human-api", "username": "human", "bot": false }
    }))
    .expect("thread message parses");

    let error = tokio::time::timeout(
        Duration::from_millis(250),
        worker.resolve_thread_parent_from_rest_until(
            &mut message,
            tokio::time::Instant::now() + Duration::from_millis(30),
        ),
    )
    .await
    .expect("stalled lookup body is bounded")
    .expect_err("stalled lookup must retain rather than discard ingress");
    assert!(matches!(
        error,
        DiscordThreadLookupError::PreserveAndStop { .. }
    ));
    assert!(message.parent_channel_id.is_none());
    server.abort();
}

#[tokio::test]
async fn discord_error_response_text_is_bounded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/too-large-error"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_string("x".repeat(DISCORD_REST_ERROR_BODY_LIMIT_BYTES + 64)),
        )
        .mount(&server)
        .await;

    let resp = reqwest::Client::new()
        .get(format!("{}/too-large-error", server.uri()))
        .send()
        .await
        .expect("response");

    let text = discord_response_text_limited(resp).await;

    assert!(text.ends_with("... [truncated]"));
    assert!(text.len() <= DISCORD_REST_ERROR_BODY_LIMIT_BYTES + "... [truncated]".len());
}

#[tokio::test]
async fn discord_json_response_rejects_oversized_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/too-large-json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "{{\"id\":\"{}\"}}",
            "x".repeat(DISCORD_REST_JSON_BODY_LIMIT_BYTES + 64)
        )))
        .mount(&server)
        .await;

    let resp = reqwest::Client::new()
        .get(format!("{}/too-large-json", server.uri()))
        .send()
        .await
        .expect("response");

    let err = discord_response_json_limited::<DiscordMessage>(resp)
        .await
        .expect_err("oversized JSON should fail closed");

    assert!(err.to_string().contains("Discord API JSON response exceeds"));
}

#[tokio::test]
async fn discord_rename_thread_uses_bounded_utf16_title() {
    let server = MockServer::start().await;
    let title = "😀".repeat(90);
    let expected = truncate_discord_utf16_with_suffix(&title, 100, "...");
    assert!(discord_utf16_len(&expected) <= 100);

    Mock::given(method("PATCH"))
        .and(path("/channels/thread-1"))
        .and(header("Authorization", "Bot test-token"))
        .and(body_json(serde_json::json!({ "name": expected })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "thread-1",
            "name": "renamed"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = DiscordAdapter::new(cfg).expect("adapter");

    adapter
        .rename_thread_channel("thread-1", &title)
        .await
        .expect("thread rename should succeed");
}

#[test]
fn split_message_short() {
    let chunks = split_message("hello", 2000);
    assert_eq!(chunks, vec!["hello"]);
}

#[test]
fn split_message_long() {
    let text = "a".repeat(3000);
    let chunks = split_message(&text, 2000);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].len(), 2000);
    assert_eq!(chunks[1].len(), 1000);
}

#[test]
fn split_message_long_unicode_is_char_boundary_safe() {
    let text = "é".repeat(2001);
    let chunks = split_message(&text, 2000);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].chars().count(), 2000);
    assert_eq!(chunks[1], "é");
}

#[test]
fn split_message_respects_discord_utf16_limit_for_astral_characters() {
    let text = "😀".repeat(1001);
    let chunks = split_message(&text, 2000);
    assert_eq!(chunks, vec!["😀".repeat(1000), "😀".to_string()]);
    assert!(chunks
        .iter()
        .all(|chunk| chunk.chars().map(char::len_utf16).sum::<usize>() <= 2000));
    assert_eq!(chunks.concat(), text);
}

#[test]
fn gateway_payload_identify() {
    let adapter = DiscordAdapter::new(test_config()).unwrap();
    let payload = adapter.build_identify_payload();
    assert_eq!(payload.op, opcodes::IDENTIFY);
    assert!(payload.d.is_some());
    assert_ne!(
        payload.d.unwrap()["intents"].as_u64().unwrap() & (1 << 12),
        0
    );
}

#[test]
fn gateway_payload_heartbeat() {
    let payload = DiscordAdapter::build_heartbeat_payload(Some(42));
    assert_eq!(payload.op, opcodes::HEARTBEAT);
    assert_eq!(payload.d, Some(serde_json::Value::Number(42.into())));
}

#[test]
fn parse_message_create_event() {
    let data = serde_json::json!({
        "id": "msg123",
        "channel_id": "ch456",
        "content": "hello world",
        "type": 19,
        "mentions": [
            { "id": "bot-self", "username": "Hermes" }
        ],
        "attachments": [
            {
                "filename": "current.txt",
                "url": "https://cdn.discordapp.com/current.txt",
                "content_type": "text/plain",
                "size": 11
            }
        ],
        "message_reference": { "message_id": "origin-1" },
        "referenced_message": {
            "content": "original message",
            "attachments": [
                {
                    "filename": "image.png",
                    "url": "https://cdn.discordapp.com/attachments/image.png",
                    "content_type": "image/png",
                    "size": 1234
                }
            ]
        },
        "author": {
            "id": "user789",
            "username": "testuser",
            "bot": false
        }
    });

    let msg = DiscordAdapter::parse_message_create(&data).unwrap();
    assert_eq!(msg.channel_id, "ch456");
    assert_eq!(msg.message_id, "msg123");
    assert_eq!(msg.content, "hello world");
    assert_eq!(msg.user_id, Some("user789".into()));
    assert_eq!(msg.username, Some("testuser".into()));
    assert!(!msg.is_bot);
    assert_eq!(msg.message_type, 19);
    assert!(msg.mentions_user("bot-self"));
    assert_eq!(msg.reply_to_message_id.as_deref(), Some("origin-1"));
    assert_eq!(msg.reply_to_text.as_deref(), Some("original message"));
    assert_eq!(msg.attachments.len(), 1);
    assert_eq!(msg.attachments[0].filename, "current.txt");
    assert_eq!(msg.reply_to_attachments.len(), 1);
    assert_eq!(msg.reply_to_attachments[0].filename, "image.png");
    assert_eq!(
        msg.reply_to_attachments[0].content_type.as_deref(),
        Some("image/png")
    );
    let routed = msg.to_gateway_message().expect("attributable message");
    assert!(routed.text.starts_with("hello world"));
    assert!(routed.text.contains("[Discord reply context]"));
    assert!(routed
        .text
        .contains("[Referenced Discord attachment: image.png]"));
    assert!(routed.text.contains("[Discord attachment: current.txt]"));
    assert!(routed
        .text
        .find("Referenced Discord attachment")
        .unwrap()
        < routed.text.find("Discord attachment: current.txt").unwrap());
}

#[test]
fn attachment_only_message_routes_nonempty_current_attachment_context() {
    let data = serde_json::json!({
        "id": "attachment-only",
        "channel_id": "channel",
        "content": "",
        "attachments": [{
            "filename": "report.pdf",
            "url": "https://cdn.discordapp.com/report.pdf",
            "content_type": "application/pdf",
            "size": 42
        }],
        "author": { "id": "human", "bot": false }
    });

    let message = DiscordAdapter::parse_message_create(&data).unwrap();
    let routed = message.to_gateway_message().expect("attributable message");
    assert_eq!(
        routed.text,
        "[Discord attachment: report.pdf] (application/pdf, 42 bytes)\nhttps://cdn.discordapp.com/report.pdf"
    );
}

#[test]
fn referenced_attachment_is_not_attributed_to_current_sender() {
    let data = serde_json::json!({
        "id": "reply-only",
        "channel_id": "channel",
        "content": "please inspect",
        "type": 19,
        "message_reference": { "message_id": "origin" },
        "referenced_message": {
            "content": "",
            "attachments": [{
                "filename": "prior.png",
                "url": "https://cdn.discordapp.com/prior.png",
                "content_type": "image/png"
            }]
        },
        "author": { "id": "human", "bot": false }
    });

    let message = DiscordAdapter::parse_message_create(&data).unwrap();
    assert!(message.attachments.is_empty());
    assert_eq!(message.reply_to_attachments.len(), 1);
    let routed = message.to_gateway_message().expect("attributable message");
    assert!(routed
        .text
        .contains("[Referenced Discord attachment: prior.png]"));
    assert!(!routed.text.contains("\n\n[Discord attachment: prior.png]"));
}

#[test]
fn parse_message_create_bot() {
    let data = serde_json::json!({
        "id": "msg1",
        "channel_id": "ch1",
        "content": "bot msg",
        "author": { "id": "bot1", "username": "mybot", "bot": true }
    });

    let msg = DiscordAdapter::parse_message_create(&data).unwrap();
    assert!(msg.is_bot);
}

#[test]
fn parse_message_create_webhook_provenance() {
    let mut data = serde_json::json!({
        "id": "msg-webhook",
        "channel_id": "ch1",
        "author": { "id": "sender", "username": "sender", "bot": false },
        "webhook_id": null
    });
    assert!(!DiscordAdapter::parse_message_create(&data).unwrap().is_bot);

    data["webhook_id"] = serde_json::Value::String("webhook-1".to_string());
    assert!(DiscordAdapter::parse_message_create(&data).unwrap().is_bot);
}

#[test]
fn parse_message_create_preserves_direct_thread_parent_context() {
    let data = serde_json::json!({
        "id": "thread-message",
        "channel_id": "thread-1",
        "parent_id": "parent-1",
        "author": { "id": "human-1", "username": "human", "bot": false }
    });

    let message = DiscordAdapter::parse_message_create(&data).unwrap();
    assert_eq!(message.parent_channel_id.as_deref(), Some("parent-1"));
    assert_eq!(message.thread_id.as_deref(), Some("thread-1"));
    assert!(!message.is_dm);
    let incoming = message.to_gateway_message().unwrap();
    assert_eq!(incoming.chat_id, "parent-1");
    assert_eq!(incoming.thread_id.as_deref(), Some("thread-1"));
}

#[test]
fn parse_message_create_known_guild_channel_without_guild_id_is_not_dm() {
    let data = serde_json::json!({
        "id": "message-1",
        "channel_id": "channel-1",
        "channel_type": 0,
        "author": { "id": "human-1", "username": "human", "bot": false }
    });

    assert!(!DiscordAdapter::parse_message_create(&data).unwrap().is_dm);
}

#[test]
fn parse_message_create_rejects_dm_channel_type_with_guild_authority() {
    let data = serde_json::json!({
        "id": "contradictory-message",
        "channel_id": "dm-channel",
        "channel_type": 1,
        "guild_id": "guild-1",
        "author": { "id": "human-1", "username": "human", "bot": false }
    });

    assert!(
        DiscordAdapter::parse_message_create(&data).is_none(),
        "contradictory DM/guild payload must not reach delivery"
    );
}

// -- GatewaySession tests -----------------------------------------------

#[test]
fn session_handles_hello() {
    let mut session = GatewaySession::new();
    let payload = GatewayPayload {
        op: opcodes::HELLO,
        d: Some(serde_json::json!({ "heartbeat_interval": 41250 })),
        s: None,
        t: None,
    };

    let actions = session.handle_gateway_event(&payload);
    assert_eq!(session.heartbeat_interval_ms, Some(41250));
    assert_eq!(actions, vec![GatewayAction::SendIdentify]);
}

#[test]
fn session_rejects_missing_or_zero_hello_heartbeat_before_identify() {
    for data in [
        serde_json::json!({}),
        serde_json::json!({ "heartbeat_interval": 0 }),
        serde_json::json!({ "heartbeat_interval": "1000" }),
    ] {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: opcodes::HELLO,
            d: Some(data),
            s: None,
            t: None,
        };

        assert_eq!(
            session.handle_gateway_event(&payload),
            vec![GatewayAction::Reconnect]
        );
        assert_eq!(session.heartbeat_interval_ms, None);
    }
}

#[test]
fn session_handles_hello_with_resume() {
    let mut session = GatewaySession::new();
    session.session_id = Some("sess123".into());
    session.sequence = Some(42);
    session.commit_sequence(Some(42));

    let payload = GatewayPayload {
        op: opcodes::HELLO,
        d: Some(serde_json::json!({ "heartbeat_interval": 30000 })),
        s: None,
        t: None,
    };

    let actions = session.handle_gateway_event(&payload);
    assert_eq!(actions, vec![GatewayAction::SendResume]);
}

#[test]
fn session_handles_heartbeat_ack_only_for_pending_heartbeat() {
    let mut session = GatewaySession::new();
    session.heartbeat_sent();
    assert!(session.heartbeat_pending);

    let payload = GatewayPayload {
        op: opcodes::HEARTBEAT_ACK,
        d: None,
        s: None,
        t: None,
    };

    session.handle_gateway_event(&payload);
    assert!(!session.heartbeat_pending);

    session.handle_gateway_event(&payload);
    assert!(!session.heartbeat_pending);
}

#[test]
fn session_handles_reconnect() {
    let mut session = GatewaySession::new();
    let payload = GatewayPayload {
        op: opcodes::RECONNECT,
        d: None,
        s: None,
        t: None,
    };

    let actions = session.handle_gateway_event(&payload);
    assert_eq!(actions, vec![GatewayAction::Reconnect]);
}

#[test]
fn session_handles_invalid_session_resumable() {
    let mut session = GatewaySession::new();
    session.session_id = Some("sess".into());
    session.sequence = Some(10);

    let payload = GatewayPayload {
        op: opcodes::INVALID_SESSION,
        d: Some(serde_json::Value::Bool(true)),
        s: None,
        t: None,
    };

    let actions = session.handle_gateway_event(&payload);
    assert_eq!(actions, vec![GatewayAction::InvalidSession(true)]);
    assert!(session.session_id.is_some());
}

#[test]
fn session_handles_invalid_session_not_resumable() {
    let mut session = GatewaySession::new();
    session.session_id = Some("sess".into());
    session.sequence = Some(10);

    let payload = GatewayPayload {
        op: opcodes::INVALID_SESSION,
        d: Some(serde_json::Value::Bool(false)),
        s: None,
        t: None,
    };

    let actions = session.handle_gateway_event(&payload);
    assert_eq!(actions, vec![GatewayAction::InvalidSession(false)]);
    assert!(session.session_id.is_none());
    assert!(session.sequence.is_none());
}

#[test]
fn session_handles_ready_dispatch() {
    let mut session = GatewaySession::new();
    let payload = GatewayPayload {
        op: opcodes::DISPATCH,
        d: Some(serde_json::json!({
            "session_id": "abc123",
            "resume_gateway_url": "wss://resume.discord.gg",
            "user": { "id": "12345", "username": "testbot" }
        })),
        s: Some(1),
        t: Some("READY".into()),
    };

    let actions = session.handle_gateway_event(&payload);
    assert_eq!(session.session_id, Some("abc123".into()));
    assert_eq!(
        session.resume_gateway_url,
        Some("wss://resume.discord.gg/?v=10&encoding=json".into())
    );
    assert_eq!(session.sequence, Some(1));
    assert_eq!(session.client_user_id.as_deref(), Some("12345"));
    assert!(session.identified);

    assert_eq!(actions.len(), 1);
    match &actions[0] {
        GatewayAction::Dispatch(name, _) => assert_eq!(name, "READY"),
        other => panic!("expected Dispatch, got {:?}", other),
    }
}

#[test]
fn session_rejects_ready_without_authoritative_bot_identity() {
    let mut session = GatewaySession::new();
    let payload = GatewayPayload {
        op: opcodes::DISPATCH,
        d: Some(serde_json::json!({
            "session_id": "abc123",
            "resume_gateway_url": "wss://resume.discord.gg",
            "user": { "username": "testbot" }
        })),
        s: Some(1),
        t: Some("READY".into()),
    };

    assert_eq!(
        session.handle_gateway_event(&payload),
        vec![GatewayAction::Reconnect]
    );
    assert!(!session.identified);
    assert!(session.session_id.is_none());
    assert!(session.client_user_id.is_none());
}

#[test]
fn session_tracks_sequence() {
    let mut session = GatewaySession::new();
    let payload = GatewayPayload {
        op: opcodes::DISPATCH,
        d: Some(serde_json::json!({})),
        s: Some(42),
        t: Some("GUILD_CREATE".into()),
    };

    session.handle_gateway_event(&payload);
    assert_eq!(session.sequence, Some(42));
}

#[test]
fn session_reconnects_at_next_interval_without_heartbeat_ack() {
    let mut session = GatewaySession::new();
    assert_eq!(session.heartbeat_tick(), HeartbeatTick::Send);

    session.heartbeat_sent();
    assert!(session.heartbeat_pending);
    assert_eq!(session.heartbeat_tick(), HeartbeatTick::Reconnect);
}

#[test]
fn session_new_connection_clears_heartbeat_without_losing_resume_state() {
    let mut session = GatewaySession::new();
    session.session_id = Some("session".to_string());
    session.sequence = Some(9);
    session.commit_sequence(Some(9));
    session.heartbeat_interval_ms = Some(42_000);
    session.heartbeat_sent();

    session.begin_connection();

    assert!(session.can_resume());
    assert!(!session.heartbeat_pending);
    assert_eq!(session.heartbeat_interval_ms, None);
}

#[test]
fn session_reset() {
    let mut session = GatewaySession::new();
    session.session_id = Some("s".into());
    session.sequence = Some(99);
    session.heartbeat_interval_ms = Some(5000);
    session.identified = true;

    session.reset();
    assert!(session.session_id.is_none());
    assert!(session.sequence.is_none());
    assert!(session.heartbeat_interval_ms.is_none());
    assert!(!session.identified);
    assert!(!session.heartbeat_pending);
}

#[test]
fn session_heartbeat_request_is_immediate_without_extending_pending_state() {
    let mut session = GatewaySession::new();
    let payload = GatewayPayload {
        op: opcodes::HEARTBEAT,
        d: None,
        s: None,
        t: None,
    };

    assert_eq!(
        session.handle_gateway_event(&payload),
        vec![GatewayAction::SendHeartbeatNow]
    );
    session.heartbeat_sent();
    assert_eq!(session.heartbeat_tick(), HeartbeatTick::Reconnect);
    assert_eq!(
        session.handle_gateway_event(&payload),
        vec![GatewayAction::SendHeartbeatNow]
    );
    session.heartbeat_sent();
    assert_eq!(session.heartbeat_tick(), HeartbeatTick::Reconnect);
}

// -- Event parsing tests ------------------------------------------------

// -- Dispatch routing tests ---------------------------------------------

#[test]
fn dispatch_routes_message_create() {
    let data = serde_json::json!({
        "id": "m1",
        "channel_id": "c1",
        "content": "hi",
        "author": { "id": "u1", "username": "a", "bot": false }
    });

    let evt = DiscordAdapter::parse_dispatch("MESSAGE_CREATE", &data);
    assert!(matches!(evt, Some(DispatchEvent::MessageCreate(_))));
}


#[test]
fn dispatch_unknown_event_returns_none() {
    let data = serde_json::json!({});
    let evt = DiscordAdapter::parse_dispatch("UNKNOWN_EVENT", &data);
    assert!(evt.is_none());
}

// -- Embed builder tests ------------------------------------------------

#[test]
fn embed_builder() {
    let embed = DiscordEmbed::new()
        .with_title("Test Embed")
        .with_description("A description")
        .with_color(0xFF5733)
        .with_footer("footer text")
        .with_timestamp("2026-01-01T00:00:00Z")
        .add_field("Field 1", "Value 1", true)
        .add_field("Field 2", "Value 2", false);

    assert_eq!(embed.title, Some("Test Embed".into()));
    assert_eq!(embed.description, Some("A description".into()));
    assert_eq!(embed.color, Some(0xFF5733));
    assert_eq!(embed.footer.as_ref().unwrap().text, "footer text");
    assert_eq!(embed.timestamp, Some("2026-01-01T00:00:00Z".into()));
    assert_eq!(embed.fields.len(), 2);
    assert_eq!(embed.fields[0].name, "Field 1");
    assert_eq!(embed.fields[0].inline, Some(true));
    assert_eq!(embed.fields[1].inline, Some(false));
}

#[test]
fn embed_serialization() {
    let embed = DiscordEmbed::new().with_title("Hello").with_color(0x00FF00);

    let json = serde_json::to_value(&embed).unwrap();
    assert_eq!(json["title"], "Hello");
    assert_eq!(json["color"], 0x00FF00);
    assert!(json.get("description").is_none());
    assert!(json.get("footer").is_none());
}


// -- Emoji encoding tests -----------------------------------------------

#[test]
fn encode_emoji_unicode() {
    let encoded = encode_emoji("\u{1f44d}");
    assert_eq!(encoded, "%F0%9F%91%8D");
}

#[test]
fn encode_emoji_custom() {
    let encoded = encode_emoji("custom_emote:12345");
    assert_eq!(encoded, "custom_emote:12345");
}

// -- Default trait impls ------------------------------------------------

#[test]
fn gateway_session_default() {
    let session = GatewaySession::default();
    assert!(session.sequence.is_none());
    assert!(session.session_id.is_none());
    assert!(!session.identified);
    assert!(!session.heartbeat_pending);
}

#[test]
fn embed_default() {
    let embed = DiscordEmbed::default();
    assert!(embed.title.is_none());
    assert!(embed.fields.is_empty());
}

struct GlobalRateLimitOnce {
    attempts: AtomicUsize,
}

impl Respond for GlobalRateLimitOnce {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        if self.attempts.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
            ResponseTemplate::new(429)
                // The JSON body is Discord's precise source of truth. A zero
                // header proves that outbound admission does not truncate it.
                .insert_header("Retry-After", "0")
                .set_body_json(serde_json::json!({
                    "global": true,
                    "retry_after": 0.5
                }))
        } else {
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "after-global-rate-limit",
                "channel_id": "first"
            }))
        }
    }
}

#[tokio::test]
async fn global_429_blocks_a_concurrent_rest_operation_until_its_precise_pause_expires() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/first/messages"))
        .respond_with(GlobalRateLimitOnce {
            attempts: AtomicUsize::new(0),
        })
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/channels/second/messages/message-1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/channels/lookup-channel"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "lookup-channel",
            "guild_id": "guild-1",
            "type": 0
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    let adapter = std::sync::Arc::new(DiscordAdapter::new(cfg).expect("adapter"));
    let first = {
        let adapter = std::sync::Arc::clone(&adapter);
        tokio::spawn(async move { adapter.send_text("first", "first operation").await })
    };

    let barrier_deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let not_before = *adapter.global_rate_limit.not_before.borrow();
        if not_before.is_some_and(|not_before| not_before > Instant::now()) {
            break;
        }
        assert!(
            Instant::now() < barrier_deadline,
            "global 429 did not publish an adapter-wide pause"
        );
        tokio::task::yield_now().await;
    }

    let second = {
        let adapter = std::sync::Arc::clone(&adapter);
        tokio::spawn(async move { adapter.edit_text("second", "message-1", "second operation").await })
    };
    let lookup = {
        let worker = adapter.gateway_driver.worker();
        tokio::spawn(async move {
            let mut message = DiscordAdapter::parse_message_create(&serde_json::json!({
                "id": "lookup-message",
                "channel_id": "lookup-channel",
                "guild_id": "guild-1",
                "content": "lookup",
                "author": { "id": "human-1", "username": "human", "bot": false }
            }))
            .expect("worker lookup message parses");
            worker.resolve_thread_parent_from_rest(&mut message).await
        })
    };
    sleep(Duration::from_millis(100)).await;
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("request inspection")
            .len(),
        1,
        "a concurrent REST operation must not bypass a live Discord global pause"
    );

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), first)
            .await
            .expect("first operation completes")
            .expect("first task joins")
            .expect("first operation succeeds"),
        vec!["after-global-rate-limit"]
    );
    tokio::time::timeout(Duration::from_secs(2), second)
        .await
        .expect("second operation completes")
        .expect("second task joins")
        .expect("second operation succeeds");
    tokio::time::timeout(Duration::from_secs(2), lookup)
        .await
        .expect("worker lookup completes")
        .expect("worker lookup task joins")
        .expect("worker lookup respects and clears the shared global barrier");
}

#[tokio::test]
async fn liveness_global_429_sets_the_shared_barrier_without_marking_the_adapter_unhealthy() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/@me"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "0")
                .set_body_json(serde_json::json!({
                    "global": true,
                    "retry_after": 0.5
                })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let global_rate_limit = std::sync::Arc::new(DiscordGlobalRateLimitBarrier::default());
    let (stop_tx, stop) = tokio::sync::watch::channel(false);
    let liveness = {
        let failed = std::sync::Arc::clone(&failed);
        let global_rate_limit = std::sync::Arc::clone(&global_rate_limit);
        let api_base_url = server.uri();
        tokio::spawn(async move {
            run_discord_liveness_probe_loop_with_timeout_and_global_rate_limit(
                reqwest::Client::new(),
                "test-token".to_string(),
                api_base_url,
                Duration::from_millis(1),
                1,
                failed,
                stop,
                global_rate_limit,
                Duration::from_millis(250),
            )
            .await;
        })
    };

    for _ in 0..50 {
        if server
            .received_requests()
            .await
            .expect("request inspection")
            .len()
            == 1
        {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("request inspection")
            .len(),
        1,
        "liveness should issue its first probe"
    );
    let barrier_deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if (*global_rate_limit.not_before.borrow())
            .is_some_and(|not_before| not_before > Instant::now())
        {
            break;
        }
        assert!(
            Instant::now() < barrier_deadline,
            "global liveness 429 did not publish the shared pause"
        );
        tokio::task::yield_now().await;
    }
    sleep(Duration::from_millis(75)).await;
    assert!(
        !failed.load(AtomicOrdering::SeqCst),
        "an authenticated 429 is reachable Discord REST, not a liveness failure"
    );
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("request inspection")
            .len(),
        1,
        "the next liveness probe must honor the shared global pause"
    );

    stop_tx.send(true).expect("liveness stop receiver remains live");
    tokio::time::timeout(Duration::from_secs(1), liveness)
        .await
        .expect("global barrier wait yields to stop")
        .expect("liveness task joins");
    assert!(!failed.load(AtomicOrdering::SeqCst));
}

#[tokio::test]
async fn liveness_local_429_waits_for_its_fractional_retry_hint_without_marking_unhealthy() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/@me"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "0")
                .set_body_json(serde_json::json!({
                    "global": false,
                    "retry_after": 0.5
                })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let global_rate_limit = std::sync::Arc::new(DiscordGlobalRateLimitBarrier::default());
    let (stop_tx, stop) = tokio::sync::watch::channel(false);
    let liveness = {
        let failed = std::sync::Arc::clone(&failed);
        let global_rate_limit = std::sync::Arc::clone(&global_rate_limit);
        let api_base_url = server.uri();
        tokio::spawn(async move {
            run_discord_liveness_probe_loop_with_timeout_and_global_rate_limit(
                reqwest::Client::new(),
                "test-token".to_string(),
                api_base_url,
                Duration::from_millis(1),
                1,
                failed,
                stop,
                global_rate_limit,
                Duration::from_millis(250),
            )
            .await;
        })
    };

    for _ in 0..50 {
        if server
            .received_requests()
            .await
            .expect("request inspection")
            .len()
            == 1
        {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    sleep(Duration::from_millis(75)).await;
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("request inspection")
            .len(),
        1,
        "liveness must keep a route-local 429's precise retry delay"
    );
    assert!(
        (*global_rate_limit.not_before.borrow()).is_none(),
        "a non-global 429 must not block unrelated Discord REST routes"
    );
    assert!(
        !failed.load(AtomicOrdering::SeqCst),
        "a route-local 429 is still a reachable liveness response"
    );

    stop_tx.send(true).expect("liveness stop receiver remains live");
    tokio::time::timeout(Duration::from_secs(1), liveness)
        .await
        .expect("local retry wait yields to stop")
        .expect("liveness task joins");
}
