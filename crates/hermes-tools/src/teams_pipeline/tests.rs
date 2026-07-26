use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

struct EnvVarGuard {
    key: &'static str,
    original: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var(key).ok();
        unsafe { std::env::set_var(key, value) };
        Self { key, original }
    }

    fn remove(key: &'static str) -> Self {
        let original = std::env::var(key).ok();
        unsafe { std::env::remove_var(key) };
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.original.as_deref() {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

/// This is not Cargo's TMPDIR. The root-owned, sticky system directory is
/// permitted by the product's private-media policy; the build TMPDIR may be
/// intentionally group-writable and must remain rejected.
#[cfg(unix)]
fn trusted_tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;

    let root = PathBuf::from("/private/tmp").join("hermes-teams-pipeline-tests");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    tempfile::Builder::new()
        .prefix("run-")
        .tempdir_in(root)
        .unwrap()
}

#[cfg(not(unix))]
fn trusted_tempdir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

struct OrderedTranscriptResponder {
    calls: AtomicUsize,
}

impl Respond for OrderedTranscriptResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let text = match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => "first bounded chunk",
            1 => "second bounded chunk",
            _ => "unexpected extra chunk",
        };
        ResponseTemplate::new(200).set_body_json(json!({ "text": text }))
    }
}

struct EndlessNextLinkResponder {
    calls: AtomicUsize,
}

impl Respond for EndlessNextLinkResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let next = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        ResponseTemplate::new(200).set_body_json(json!({
            "value": [{"id": next}],
            "@odata.nextLink": format!("/page/{next}")
        }))
    }
}

#[cfg(unix)]
fn write_descendant_holding_ffmpeg(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(
        path,
        "#!/bin/sh\nfor output do :; done\nprintf 'partial audio' > \"$output\"\nprintf '%s' \"$$\" > \"$HERMES_TEST_FFMPEG_PARENT_PID\"\nsh -c 'trap \"\" TERM; printf \"%s\" \"$$\" > \"$HERMES_TEST_FFMPEG_CHILD_PID\"; while :; do printf \"descendant stderr\\\\n\" >&2; done' &\nwhile :; do printf 'parent stderr\\n' >&2; done\n",
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
async fn wait_for_test_pid(path: &std::path::Path) -> i32 {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Ok(value) = std::fs::read_to_string(path) {
                if let Ok(pid) = value.trim().parse() {
                    return pid;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("ffmpeg wrapper did not publish its pid")
}

#[cfg(unix)]
async fn assert_test_pid_exited(pid: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if unsafe { libc::kill(pid, 0) } != 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("ffmpeg process tree member remained alive");
}

#[derive(Default)]
struct MockGraph {
    transcript: Mutex<Option<(MeetingArtifact, String)>>,
    recordings: Mutex<Vec<MeetingArtifact>>,
    recording_bytes: Mutex<Vec<u8>>,
    call_record: Mutex<Option<MeetingArtifact>>,
}

#[async_trait]
impl TeamsGraphBackend for MockGraph {
    async fn resolve_meeting_reference(
        &self,
        meeting_id: Option<&str>,
        organizer_user_id: Option<&str>,
        _join_web_url: Option<&str>,
        tenant_id: Option<&str>,
    ) -> TeamsPipelineResult<TeamsMeetingRef> {
        let mut metadata = Map::new();
        metadata.insert("subject".into(), Value::String("Weekly Sync".into()));
        metadata.insert(
            "participants".into(),
            json!([{"displayName": "Ada"}, {"identity": {"user": {"displayName": "Grace"}}}]),
        );
        Ok(TeamsMeetingRef {
            meeting_id: meeting_id.unwrap_or("meeting-1").into(),
            organizer_user_id: organizer_user_id.map(ToOwned::to_owned),
            join_web_url: None,
            calendar_event_id: None,
            thread_id: None,
            tenant_id: tenant_id.map(ToOwned::to_owned),
            metadata,
        })
    }

    async fn fetch_preferred_transcript_text(
        &self,
        _meeting_ref: &TeamsMeetingRef,
    ) -> TeamsPipelineResult<Option<(MeetingArtifact, String)>> {
        Ok(self.transcript.lock().clone())
    }

    async fn list_recording_artifacts(
        &self,
        _meeting_ref: &TeamsMeetingRef,
    ) -> TeamsPipelineResult<Vec<MeetingArtifact>> {
        Ok(self.recordings.lock().clone())
    }

    async fn download_recording_artifact(
        &self,
        _meeting_ref: &TeamsMeetingRef,
        _recording: &MeetingArtifact,
    ) -> TeamsPipelineResult<Vec<u8>> {
        Ok(self.recording_bytes.lock().clone())
    }

    async fn enrich_meeting_with_call_record(
        &self,
        _meeting_ref: &TeamsMeetingRef,
        _call_record_id: Option<&str>,
    ) -> TeamsPipelineResult<Option<MeetingArtifact>> {
        Ok(self.call_record.lock().clone())
    }
}

struct CappedStreamingGraph {
    inner: MockGraph,
    client: MicrosoftGraphClient,
    max_recording_bytes: usize,
}

#[async_trait]
impl TeamsGraphBackend for CappedStreamingGraph {
    async fn resolve_meeting_reference(
        &self,
        meeting_id: Option<&str>,
        organizer_user_id: Option<&str>,
        join_web_url: Option<&str>,
        tenant_id: Option<&str>,
    ) -> TeamsPipelineResult<TeamsMeetingRef> {
        self.inner
            .resolve_meeting_reference(meeting_id, organizer_user_id, join_web_url, tenant_id)
            .await
    }

    async fn fetch_preferred_transcript_text(
        &self,
        meeting_ref: &TeamsMeetingRef,
    ) -> TeamsPipelineResult<Option<(MeetingArtifact, String)>> {
        self.inner
            .fetch_preferred_transcript_text(meeting_ref)
            .await
    }

    async fn list_recording_artifacts(
        &self,
        meeting_ref: &TeamsMeetingRef,
    ) -> TeamsPipelineResult<Vec<MeetingArtifact>> {
        self.inner.list_recording_artifacts(meeting_ref).await
    }

    async fn download_recording_artifact(
        &self,
        _meeting_ref: &TeamsMeetingRef,
        recording: &MeetingArtifact,
    ) -> TeamsPipelineResult<Vec<u8>> {
        self.client
            .download_bytes(
                recording
                    .download_url
                    .as_deref()
                    .expect("test recording has a download URL"),
                self.max_recording_bytes,
                "recording",
            )
            .await
    }

    async fn enrich_meeting_with_call_record(
        &self,
        meeting_ref: &TeamsMeetingRef,
        call_record_id: Option<&str>,
    ) -> TeamsPipelineResult<Option<MeetingArtifact>> {
        self.inner
            .enrich_meeting_with_call_record(meeting_ref, call_record_id)
            .await
    }
}

struct CountingTranscriber {
    calls: AtomicUsize,
}

#[async_trait]
impl TeamsTranscriber for CountingTranscriber {
    async fn transcribe_recording(
        &self,
        _recording: &MeetingArtifact,
        _bytes: &[u8],
        _config: &TeamsPipelineConfig,
    ) -> TeamsPipelineResult<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok("unexpected transcription".into())
    }
}

struct MockTranscriber;

#[async_trait]
impl TeamsTranscriber for MockTranscriber {
    async fn transcribe_recording(
        &self,
        _recording: &MeetingArtifact,
        _bytes: &[u8],
        _config: &TeamsPipelineConfig,
    ) -> TeamsPipelineResult<String> {
        Ok("Action: Follow up with Legal.\nRisk: Budget approval pending.".into())
    }
}

struct CountingSummarizer {
    count: AtomicUsize,
}

#[async_trait]
impl TeamsSummarizer for CountingSummarizer {
    async fn summarize(
        &self,
        resolved_meeting: &TeamsMeetingRef,
        transcript_text: &str,
        artifacts: &[MeetingArtifact],
        _config: &TeamsPipelineConfig,
    ) -> TeamsPipelineResult<TeamsMeetingSummaryPayload> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(TeamsMeetingSummaryPayload {
            meeting_ref: resolved_meeting.clone(),
            title: Some("Weekly Sync".into()),
            start_time: None,
            end_time: None,
            participants: collect_participants(resolved_meeting),
            transcript_text: Some(transcript_text.into()),
            summary: Some("Short summary".into()),
            key_decisions: vec!["Ship it.".into()],
            action_items: vec!["Send draft.".into()],
            risks: vec!["Timeline risk.".into()],
            call_metrics: collect_call_metrics(artifacts),
            source_artifacts: artifacts.to_vec(),
            confidence: Some("high".into()),
            confidence_notes: Some("Transcript available.".into()),
        })
    }
}

struct CapturingSummarizer {
    count: AtomicUsize,
    transcripts: Mutex<Vec<String>>,
}

#[async_trait]
impl TeamsSummarizer for CapturingSummarizer {
    async fn summarize(
        &self,
        resolved_meeting: &TeamsMeetingRef,
        transcript_text: &str,
        artifacts: &[MeetingArtifact],
        _config: &TeamsPipelineConfig,
    ) -> TeamsPipelineResult<TeamsMeetingSummaryPayload> {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.transcripts.lock().push(transcript_text.into());
        Ok(TeamsMeetingSummaryPayload {
            meeting_ref: resolved_meeting.clone(),
            title: Some("Weekly Sync".into()),
            start_time: None,
            end_time: None,
            participants: collect_participants(resolved_meeting),
            transcript_text: Some(transcript_text.into()),
            summary: Some("Short summary".into()),
            key_decisions: vec!["Ship it.".into()],
            action_items: vec!["Send draft.".into()],
            risks: vec!["Timeline risk.".into()],
            call_metrics: collect_call_metrics(artifacts),
            source_artifacts: artifacts.to_vec(),
            confidence: Some("high".into()),
            confidence_notes: Some("Transcript available.".into()),
        })
    }
}

fn artifact(kind: &str, id: &str) -> MeetingArtifact {
    MeetingArtifact {
        artifact_type: kind.into(),
        artifact_id: id.into(),
        display_name: Some(format!("{id}.vtt")),
        content_type: (kind == "transcript").then(|| "text/plain".into()),
        source_url: None,
        download_url: Some(format!("https://example.com/{id}")),
        created_at: None,
        available_at: None,
        size_bytes: None,
        metadata: Map::new(),
    }
}

const TEST_ORGANIZER_USER_ID: &str = "11111111-1111-1111-1111-111111111111";

fn scoped_notification(
    notification_id: &str,
    meeting_id: &str,
    artifact_type: &str,
    artifact_id: &str,
) -> Value {
    let collection = match artifact_type {
        "transcript" => "transcripts",
        "recording" => "recordings",
        _ => panic!("unsupported test artifact type"),
    };
    json!({
        "id": notification_id,
        "changeType": "updated",
        "subscriptionId": "subscription-1",
        "tenantId": "tenant-1",
        "resource": format!(
            "users/{TEST_ORGANIZER_USER_ID}/onlineMeetings/{meeting_id}/{collection}/{artifact_id}"
        ),
        "resourceData": {
            "id": artifact_id,
            "meetingId": meeting_id,
            "organizerUserId": TEST_ORGANIZER_USER_ID,
            "tenantId": "tenant-1",
        },
    })
}

#[cfg(windows)]
#[test]
fn windows_private_teams_paths_use_protected_owner_only_dacls() {
    let root = trusted_tempdir();
    let run_dir = create_private_teams_run_dir_sync(root.path()).unwrap();
    assert!(
        windows_private_path::has_protected_owner_only_dacl(run_dir.path()).unwrap(),
        "Teams run directory did not receive the protected owner-only DACL"
    );

    let segment_dir = run_dir.path().join("audio-segments");
    create_private_teams_directory_sync(&segment_dir).unwrap();
    assert!(
        windows_private_path::has_protected_owner_only_dacl(&segment_dir).unwrap(),
        "Teams segment directory did not receive the protected owner-only DACL"
    );

    let segment = segment_dir.join("segment-0000.mp3");
    std::fs::write(&segment, b"segment").unwrap();
    windows_private_path::set_owner_only_dacl(&segment).unwrap();
    assert!(
        windows_private_path::has_protected_owner_only_dacl(&segment).unwrap(),
        "Teams segment file did not receive the protected owner-only DACL"
    );

    let recording = run_dir.path().join("recording.mp4");
    drop(crate::tts_audio::create_private_temp_file(&recording, &recording, false).unwrap());
    assert!(
        windows_private_path::has_protected_owner_only_dacl(&recording).unwrap(),
        "Teams recording file did not receive the protected owner-only DACL"
    );
    run_dir.close().unwrap();
}

#[tokio::test]
async fn production_transcriber_authorizes_custom_teams_temp_root() {
    let _env_lock = hermes_config::managed_gateway::test_lock::lock();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .and(header("authorization", "Bearer teams-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "text": "Teams transcript"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let _managed = EnvVarGuard::set("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "0");
    let _key = EnvVarGuard::set("OPENAI_API_KEY", "teams-key");
    let _base = EnvVarGuard::set("STT_OPENAI_BASE_URL", &server.uri());
    let temp = trusted_tempdir();
    let mut recording = artifact("recording", "trusted-root");
    recording.display_name = Some("trusted-root.wav".into());
    let config = TeamsPipelineConfig {
        tmp_dir: Some(temp.path().to_path_buf()),
        ffmpeg_extract_audio: false,
        stt_model: Some("gpt-4o-transcribe".into()),
        ..TeamsPipelineConfig::default()
    };

    let transcript = TranscriptionToolTeamsTranscriber
        .transcribe_recording(&recording, b"teams audio fixture", &config)
        .await
        .unwrap();

    assert_eq!(transcript, "Teams transcript");
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(body.contains("name=\"model\""));
    assert!(body.contains("gpt-4o-transcribe"));
}

#[tokio::test]
async fn production_transcriber_cleans_custom_temp_root_on_failure() {
    let _env_lock = hermes_config::managed_gateway::test_lock::lock();
    let _managed = EnvVarGuard::set("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "0");
    let _voice = EnvVarGuard::remove("VOICE_TOOLS_OPENAI_KEY");
    let _hermes_openai = EnvVarGuard::remove("HERMES_OPENAI_API_KEY");
    let _openai = EnvVarGuard::remove("OPENAI_API_KEY");
    let _groq = EnvVarGuard::remove("GROQ_API_KEY");
    let _mistral = EnvVarGuard::remove("MISTRAL_API_KEY");
    let temp = trusted_tempdir();
    let mut recording = artifact("recording", "cleanup");
    recording.display_name = Some("cleanup.wav".into());
    let config = TeamsPipelineConfig {
        tmp_dir: Some(temp.path().to_path_buf()),
        ffmpeg_extract_audio: false,
        ..TeamsPipelineConfig::default()
    };

    let error = TranscriptionToolTeamsTranscriber
        .transcribe_recording(&recording, b"teams audio fixture", &config)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("configured STT provider"));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn chunked_graph_recording_over_limit_stops_before_stt_or_temp_persistence() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const DOWNLOAD_LIMIT: usize = 8 * 1024;
    const CHUNK_BYTES: usize = 1024;
    const CHUNK_COUNT: usize = 16;

    let _env_lock = hermes_config::managed_gateway::test_lock::lock();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let chunks_sent = Arc::new(AtomicUsize::new(0));
    let request_headers = Arc::new(Mutex::new(Vec::new()));
    let server_chunks_sent = chunks_sent.clone();
    let server_request_headers = request_headers.clone();
    let server = tokio::spawn(async move {
        let (mut connection, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = connection.read(&mut buffer).await.unwrap();
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        *server_request_headers.lock() = request;
        if connection
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await
            .is_err()
        {
            return;
        }
        let chunk = [b'x'; CHUNK_BYTES];
        for _ in 0..CHUNK_COUNT {
            if connection.write_all(b"400\r\n").await.is_err()
                || connection.write_all(&chunk).await.is_err()
                || connection.write_all(b"\r\n").await.is_err()
            {
                return;
            }
            server_chunks_sent.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let _ = connection.write_all(b"0\r\n\r\n").await;
    });
    let _token = EnvVarGuard::set("MSGRAPH_ACCESS_TOKEN", "test-token");
    let graph_client = MicrosoftGraphClient::new(
        MicrosoftGraphTokenProvider::from_env().unwrap(),
        format!("http://{address}"),
    );
    let mut recording = artifact("recording", "chunked-limit");
    recording.display_name = Some("chunked-limit.mp4".into());
    recording.download_url = Some(format!("http://{address}/recording"));
    let inner = MockGraph::default();
    *inner.recordings.lock() = vec![recording];
    let graph = Arc::new(CappedStreamingGraph {
        inner,
        client: graph_client,
        max_recording_bytes: DOWNLOAD_LIMIT,
    });
    let workspace = trusted_tempdir();
    let run_root = trusted_tempdir();
    let store = Arc::new(TeamsPipelineStore::new(workspace.path().join("store.json")).unwrap());
    let transcriber = Arc::new(CountingTranscriber {
        calls: AtomicUsize::new(0),
    });
    let config = TeamsPipelineConfig {
        transcript_preferred: false,
        tmp_dir: Some(run_root.path().to_path_buf()),
        ..TeamsPipelineConfig::default()
    };
    let pipeline =
        TeamsMeetingPipeline::new(graph, store, config).with_transcriber(transcriber.clone());

    let job = pipeline
        .run_notification(scoped_notification(
            "chunked-recording-notification",
            "meeting-chunked",
            "recording",
            "chunked-limit",
        ))
        .await
        .unwrap();

    assert_eq!(job.status, "retry_scheduled");
    assert!(job
        .error_info
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .contains(&format!(
            "recording response exceeds the {DOWNLOAD_LIMIT} byte limit"
        )));
    assert_eq!(transcriber.calls.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read_dir(run_root.path()).unwrap().count(), 0);
    server.await.unwrap();
    assert!(String::from_utf8_lossy(&request_headers.lock())
        .to_ascii_lowercase()
        .contains("authorization: bearer test-token"));
    assert!(
        chunks_sent.load(Ordering::SeqCst) < CHUNK_COUNT,
        "the capped client continued reading the entire chunked recording"
    );
}

#[tokio::test]
async fn graph_download_rejects_cross_origin_without_sending_bearer() {
    let _env_lock = hermes_config::managed_gateway::test_lock::lock();
    let graph = MockServer::start().await;
    let destination = MockServer::start().await;
    let _token = EnvVarGuard::set("MSGRAPH_ACCESS_TOKEN", "test-token");
    let client = MicrosoftGraphClient::new(
        MicrosoftGraphTokenProvider::from_env().unwrap(),
        graph.uri(),
    );

    let error = client
        .download_bytes(
            &format!("{}/recording", destination.uri()),
            1024,
            "recording",
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("untrusted external"));
    assert!(destination.received_requests().await.unwrap().is_empty());

    let next_link_error = client
        .get_json(&format!("{}/next", destination.uri()), &[])
        .await
        .unwrap_err();
    assert!(next_link_error
        .to_string()
        .contains("outside the configured Graph origin"));
    assert!(destination.received_requests().await.unwrap().is_empty());

    let same_origin = client.build_download_target("/recording").unwrap();
    assert!(same_origin.attach_bearer);
    let pre_authenticated = client
        .build_download_target("https://tenant.sharepoint.com/download?tempauth=signed")
        .unwrap();
    assert!(!pre_authenticated.attach_bearer);
}

#[tokio::test]
async fn graph_pagination_rejects_same_origin_next_link_cycle_before_partial_results() {
    let _env_lock = hermes_config::managed_gateway::test_lock::lock();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [{"id": "first"}],
            "@odata.nextLink": "/cycle"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let _token = EnvVarGuard::set("MSGRAPH_ACCESS_TOKEN", "test-token");
    let client = MicrosoftGraphClient::new(
        MicrosoftGraphTokenProvider::from_env().unwrap(),
        server.uri(),
    );

    let error = client.collect_paginated("/cycle").await.unwrap_err();

    assert!(error.to_string().contains("repeated an @odata.nextLink"));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer test-token")
    );
}

#[tokio::test]
async fn graph_pagination_caps_unique_same_origin_next_links() {
    let _env_lock = hermes_config::managed_gateway::test_lock::lock();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(EndlessNextLinkResponder {
            calls: AtomicUsize::new(0),
        })
        .expect(MAX_TEAMS_GRAPH_PAGINATION_PAGES as u64)
        .mount(&server)
        .await;
    let _token = EnvVarGuard::set("MSGRAPH_ACCESS_TOKEN", "test-token");
    let client = MicrosoftGraphClient::new(
        MicrosoftGraphTokenProvider::from_env().unwrap(),
        server.uri(),
    );

    let error = client.collect_paginated("/page/0").await.unwrap_err();

    assert!(error
        .to_string()
        .contains(&format!("{} page limit", MAX_TEAMS_GRAPH_PAGINATION_PAGES)));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), MAX_TEAMS_GRAPH_PAGINATION_PAGES);
    assert!(requests.iter().all(|request| {
        request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            == Some("Bearer test-token")
    }));
}

#[tokio::test]
async fn graph_transcript_fetch_uses_scoped_paths_and_normalizes_microsoft_vtt() {
    let _env_lock = hermes_config::managed_gateway::test_lock::lock();
    let server = MockServer::start().await;
    let meeting_id = "meeting-vtt";
    let transcript_id = "transcript-vtt";
    let base_path = format!("/users/{TEST_ORGANIZER_USER_ID}/onlineMeetings/{meeting_id}");
    let transcripts_path = format!("{base_path}/transcripts");
    let content_path = format!("{transcripts_path}/{transcript_id}/content");
    Mock::given(method("GET"))
        .and(path(transcripts_path.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [{
                "id": transcript_id,
                "status": "available",
                "createdDateTime": "2026-07-18T00:00:00Z"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(content_path.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "\u{feff}WEBVTT\r\nKind: captions\r\nLanguage: en-US\r\n\r\nNOTE internal metadata\r\nthis must not reach the summary\r\n\r\nSTYLE\r\n::cue { color: lime; }\r\n\r\nREGION\r\nid: bottom\r\n\r\ncue-1\r\n00:00:01.000 --> 00:00:03.500 line:0% position:50%\r\n<v Ada Lovelace><c.green>Hello &amp; welcome &lt;team&gt;.</c></v>\r\n\r\n00:00:04.000 --> 00:00:07.000 align:start\r\n<v Grace Hopper>Second <i>ordered</i> line &#x26; more.</v>\r\n",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let _token = EnvVarGuard::set("MSGRAPH_ACCESS_TOKEN", "test-token");
    let client = MicrosoftGraphClient::new(
        MicrosoftGraphTokenProvider::from_env().unwrap(),
        server.uri(),
    );
    let backend = MicrosoftGraphTeamsBackend::new(client);
    let meeting_ref = TeamsMeetingRef {
        meeting_id: meeting_id.into(),
        organizer_user_id: Some(TEST_ORGANIZER_USER_ID.into()),
        join_web_url: None,
        calendar_event_id: None,
        thread_id: None,
        tenant_id: Some("tenant-1".into()),
        metadata: Map::new(),
    };

    let (transcript, text) = backend
        .fetch_preferred_transcript_text(&meeting_ref)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        transcript.content_type, None,
        "missing metadata defaults to VTT"
    );
    assert_eq!(
        text,
        "Ada Lovelace: Hello & welcome <team>.\nGrace Hopper: Second ordered line & more."
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].url.path(), transcripts_path);
    assert_eq!(requests[1].url.path(), content_path);
    assert!(requests.iter().all(|request| {
        request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            == Some("Bearer test-token")
    }));
}

#[test]
fn transcript_normalization_rejects_malformed_vtt_and_allows_only_explicit_plain_text() {
    let mut transcript = artifact("transcript", "parser-fixture");
    transcript.content_type = None;
    for fixture in [
        "WEBVTT\n\ncue-id\nnot-a-timestamp\ntext",
        "WEBVTT\n\nNOTE only metadata\nnot a cue",
        "WEBVTT\n\n00:00:02.000 --> 00:00:01.000\nbackwards",
    ] {
        let error = normalize_transcript_content(&transcript, fixture.as_bytes()).unwrap_err();
        assert!(matches!(error, TeamsPipelineError::Invalid(_)));
    }
    let error = normalize_transcript_content(&transcript, b"plain text without VTT").unwrap_err();
    assert!(matches!(error, TeamsPipelineError::Invalid(message) if message.contains("WEBVTT")));

    transcript.content_type = Some("text/plain; charset=utf-8".into());
    assert_eq!(
        normalize_transcript_content(&transcript, b"Plain <literal> &amp; text").unwrap(),
        "Plain <literal> & text"
    );
}

#[tokio::test]
async fn production_transcriber_cleans_private_temp_dir_when_aborted() {
    let _env_lock = hermes_config::managed_gateway::test_lock::lock();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(30))
                .set_body_string("late transcript"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let _managed = EnvVarGuard::set("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "0");
    let _key = EnvVarGuard::set("OPENAI_API_KEY", "teams-key");
    let _base = EnvVarGuard::set("STT_OPENAI_BASE_URL", &server.uri());
    let temp = trusted_tempdir();
    let temp_path = temp.path().to_path_buf();
    let mut recording = artifact("recording", "cancelled");
    recording.display_name = Some("cancelled.wav".into());
    let config = TeamsPipelineConfig {
        tmp_dir: Some(temp_path.clone()),
        ffmpeg_extract_audio: false,
        ..TeamsPipelineConfig::default()
    };
    let task = tokio::spawn(async move {
        TranscriptionToolTeamsTranscriber
            .transcribe_recording(&recording, b"teams audio fixture", &config)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if !server.received_requests().await.unwrap().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let run_dirs = std::fs::read_dir(&temp_path)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(run_dirs.len(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            std::fs::metadata(&run_dirs[0])
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let recording_path = std::fs::read_dir(&run_dirs[0])
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            std::fs::metadata(recording_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(std::fs::read_dir(&temp_path).unwrap().count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn production_transcriber_segments_oversized_direct_audio_before_ordered_stt_uploads() {
    use std::os::unix::fs::PermissionsExt;

    let _env_lock = hermes_config::managed_gateway::test_lock::lock();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .and(header("authorization", "Bearer teams-key"))
        .respond_with(OrderedTranscriptResponder {
            calls: AtomicUsize::new(0),
        })
        .expect(2)
        .mount(&server)
        .await;
    let workspace = trusted_tempdir();
    let run_root = trusted_tempdir();
    let bin = workspace.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ffmpeg = bin.join("ffmpeg");
    let args_path = workspace.path().join("ffmpeg.args");
    std::fs::write(
        &ffmpeg,
        "#!/bin/sh\nfor output do :; done\nprintf '%s\\n' \"$@\" > \"$HERMES_TEST_FFMPEG_ARGS\"\nout_dir=${output%/*}\nprintf 'first compressed chunk' > \"$out_dir/segment-0000.mp3\"\nprintf 'second compressed chunk' > \"$out_dir/segment-0001.mp3\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{inherited_path}", bin.display());
    let _path = EnvVarGuard::set("PATH", &path);
    let _args = EnvVarGuard::set(
        "HERMES_TEST_FFMPEG_ARGS",
        args_path.to_string_lossy().as_ref(),
    );
    let _managed = EnvVarGuard::set("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "0");
    let _key = EnvVarGuard::set("OPENAI_API_KEY", "teams-key");
    let _base = EnvVarGuard::set("STT_OPENAI_BASE_URL", &server.uri());
    let mut recording = artifact("recording", "oversized-nine-minute");
    recording.display_name = Some("oversized-nine-minute.mp3".into());
    let config = TeamsPipelineConfig {
        transcript_preferred: false,
        tmp_dir: Some(run_root.path().to_path_buf()),
        // Oversized direct audio must still normalize and segment even when
        // direct extraction was explicitly disabled.
        ffmpeg_extract_audio: false,
        stt_model: Some("gpt-4o-mini-transcribe".into()),
        ..TeamsPipelineConfig::default()
    };
    let graph = Arc::new(MockGraph::default());
    *graph.recordings.lock() = vec![recording];
    *graph.recording_bytes.lock() = vec![b'a'; MAX_TEAMS_STT_UPLOAD_BYTES as usize + 1];
    let store = Arc::new(TeamsPipelineStore::new(workspace.path().join("store.json")).unwrap());
    let summarizer = Arc::new(CapturingSummarizer {
        count: AtomicUsize::new(0),
        transcripts: Mutex::new(Vec::new()),
    });
    let pipeline =
        TeamsMeetingPipeline::new(graph, store, config).with_summarizer(summarizer.clone());
    let job = pipeline
        .run_notification(scoped_notification(
            "oversized-nine-minute-notification",
            "meeting-oversized",
            "recording",
            "oversized-nine-minute",
        ))
        .await
        .unwrap();

    assert_eq!(job.status, "completed");
    assert_eq!(
        summarizer.transcripts.lock().as_slice(),
        ["first bounded chunk\nsecond bounded chunk"]
    );
    assert_eq!(summarizer.count.load(Ordering::SeqCst), 1);
    assert!(MAX_TEAMS_STT_UPLOAD_BYTES < 25_000_000);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert!((request.body.len() as u64) < MAX_TEAMS_STT_UPLOAD_BYTES);
        assert!(String::from_utf8_lossy(&request.body).contains("filename=\"audio.mp3\""));
    }
    let args = std::fs::read_to_string(args_path).unwrap();
    let args = args.lines().collect::<Vec<_>>();
    let has_pair = |flag, value| {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(has_pair("-ac", "1"));
    assert!(has_pair("-c:a", "libmp3lame"));
    assert!(has_pair("-minrate", "64k"));
    assert!(has_pair("-maxrate", "64k"));
    assert!(has_pair("-bufsize", "64k"));
    assert!(has_pair("-f", "segment"));
    assert!(has_pair("-segment_time", "300"));
    assert_eq!(std::fs::read_dir(run_root.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn production_transcriber_does_not_upload_partial_segments_when_ffmpeg_fails() {
    use std::os::unix::fs::PermissionsExt;

    let _env_lock = hermes_config::managed_gateway::test_lock::lock();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "text": "must not be returned"
        })))
        .mount(&server)
        .await;
    let workspace = trusted_tempdir();
    let run_root = trusted_tempdir();
    let bin = workspace.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ffmpeg = bin.join("ffmpeg");
    std::fs::write(
        &ffmpeg,
        "#!/bin/sh\nfor output do :; done\nout_dir=${output%/*}\nprintf 'partial compressed chunk' > \"$out_dir/segment-0000.mp3\"\nprintf 'fixture failure\\n' >&2\nexit 7\n",
    )
    .unwrap();
    std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{inherited_path}", bin.display());
    let _path = EnvVarGuard::set("PATH", &path);
    let _managed = EnvVarGuard::set("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "0");
    let _key = EnvVarGuard::set("OPENAI_API_KEY", "teams-key");
    let _base = EnvVarGuard::set("STT_OPENAI_BASE_URL", &server.uri());
    let mut recording = artifact("recording", "partial");
    recording.display_name = Some("partial.mp4".into());
    let config = TeamsPipelineConfig {
        tmp_dir: Some(run_root.path().to_path_buf()),
        ..TeamsPipelineConfig::default()
    };

    let error = TranscriptionToolTeamsTranscriber
        .transcribe_recording(&recording, b"video fixture", &config)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("fixture failure"));
    assert!(server.received_requests().await.unwrap().is_empty());
    assert_eq!(std::fs::read_dir(run_root.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn ffmpeg_timeout_terminates_full_process_tree_and_cleans_segments() {
    let _env_lock = hermes_config::managed_gateway::test_lock::lock();
    let workspace = trusted_tempdir();
    let run_root = trusted_tempdir();
    let bin = workspace.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ffmpeg = bin.join("ffmpeg");
    let parent_pid_path = workspace.path().join("ffmpeg-parent.pid");
    let child_pid_path = workspace.path().join("ffmpeg-child.pid");
    write_descendant_holding_ffmpeg(&ffmpeg);
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{inherited_path}", bin.display());
    let _path = EnvVarGuard::set("PATH", &path);
    let _parent_pid = EnvVarGuard::set(
        "HERMES_TEST_FFMPEG_PARENT_PID",
        parent_pid_path.to_string_lossy().as_ref(),
    );
    let _child_pid = EnvVarGuard::set(
        "HERMES_TEST_FFMPEG_CHILD_PID",
        child_pid_path.to_string_lossy().as_ref(),
    );
    let run_dir = tempfile::tempdir_in(run_root.path()).unwrap();
    let recording = run_dir.path().join("recording.aac");
    let segment_dir = run_dir.path().join("segments");
    std::fs::write(&recording, b"aac fixture").unwrap();

    let error = prepare_audio_paths_with_timeout(
        &recording,
        &segment_dir,
        true,
        std::time::Duration::from_millis(300),
    )
    .await
    .unwrap_err();

    assert!(error.to_string().contains("timed out"));
    let parent_pid = wait_for_test_pid(&parent_pid_path).await;
    let child_pid = wait_for_test_pid(&child_pid_path).await;
    assert_test_pid_exited(parent_pid).await;
    assert_test_pid_exited(child_pid).await;
    run_dir.close().unwrap();
    assert_eq!(std::fs::read_dir(run_root.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn ffmpeg_output_monitor_terminates_tree_when_segment_count_exceeds_bound() {
    use std::os::unix::fs::PermissionsExt;

    let _env_lock = hermes_config::managed_gateway::test_lock::lock();
    let workspace = trusted_tempdir();
    let run_root = trusted_tempdir();
    let bin = workspace.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ffmpeg = bin.join("ffmpeg");
    let parent_pid_path = workspace.path().join("ffmpeg-parent.pid");
    let child_pid_path = workspace.path().join("ffmpeg-child.pid");
    std::fs::write(
        &ffmpeg,
        "#!/bin/sh\nfor output do :; done\nout_dir=${output%/*}\nprintf '%s' \"$$\" > \"$HERMES_TEST_FFMPEG_PARENT_PID\"\nsh -c 'trap \"\" TERM; printf \"%s\" \"$$\" > \"$HERMES_TEST_FFMPEG_CHILD_PID\"; while :; do printf \"descendant stderr\\\\n\" >&2; done' &\nwhile [ ! -s \"$HERMES_TEST_FFMPEG_CHILD_PID\" ]; do :; done\ni=0\nwhile [ \"$i\" -le 128 ]; do printf x > \"$out_dir/segment-$(printf '%04d' \"$i\").mp3\"; i=$((i + 1)); done\nwhile :; do printf 'parent stderr\\n' >&2; done\n",
    )
    .unwrap();
    std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{inherited_path}", bin.display());
    let _path = EnvVarGuard::set("PATH", &path);
    let _parent_pid = EnvVarGuard::set(
        "HERMES_TEST_FFMPEG_PARENT_PID",
        parent_pid_path.to_string_lossy().as_ref(),
    );
    let _child_pid = EnvVarGuard::set(
        "HERMES_TEST_FFMPEG_CHILD_PID",
        child_pid_path.to_string_lossy().as_ref(),
    );
    let run_dir = tempfile::tempdir_in(run_root.path()).unwrap();
    let recording = run_dir.path().join("recording.aac");
    let segment_dir = run_dir.path().join("segments");
    std::fs::write(&recording, b"aac fixture").unwrap();

    let error = prepare_audio_paths_with_timeout(
        &recording,
        &segment_dir,
        true,
        std::time::Duration::from_secs(3),
    )
    .await
    .unwrap_err();

    assert!(error.to_string().contains("bounded output limit"));
    let parent_pid = wait_for_test_pid(&parent_pid_path).await;
    let child_pid = wait_for_test_pid(&child_pid_path).await;
    assert_test_pid_exited(parent_pid).await;
    assert_test_pid_exited(child_pid).await;
    run_dir.close().unwrap();
    assert_eq!(std::fs::read_dir(run_root.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn production_transcriber_cancellation_terminates_full_process_tree_and_cleans_segments() {
    let _env_lock = hermes_config::managed_gateway::test_lock::lock();
    let workspace = trusted_tempdir();
    let run_root = trusted_tempdir();
    let bin = workspace.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ffmpeg = bin.join("ffmpeg");
    let parent_pid_path = workspace.path().join("ffmpeg-parent.pid");
    let child_pid_path = workspace.path().join("ffmpeg-child.pid");
    write_descendant_holding_ffmpeg(&ffmpeg);
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{inherited_path}", bin.display());
    let _path = EnvVarGuard::set("PATH", &path);
    let _parent_pid = EnvVarGuard::set(
        "HERMES_TEST_FFMPEG_PARENT_PID",
        parent_pid_path.to_string_lossy().as_ref(),
    );
    let _child_pid = EnvVarGuard::set(
        "HERMES_TEST_FFMPEG_CHILD_PID",
        child_pid_path.to_string_lossy().as_ref(),
    );
    let mut recording = artifact("recording", "cancel-tree");
    recording.display_name = Some("cancel-tree.mp4".into());
    let config = TeamsPipelineConfig {
        tmp_dir: Some(run_root.path().to_path_buf()),
        ..TeamsPipelineConfig::default()
    };
    let task = tokio::spawn(async move {
        TranscriptionToolTeamsTranscriber
            .transcribe_recording(&recording, b"video fixture", &config)
            .await
    });
    let parent_pid = wait_for_test_pid(&parent_pid_path).await;
    let child_pid = wait_for_test_pid(&child_pid_path).await;

    task.abort();
    let join_error = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("cancelled transcription did not return promptly")
        .unwrap_err();
    assert!(join_error.is_cancelled());
    assert_test_pid_exited(parent_pid).await;
    assert_test_pid_exited(child_pid).await;
    assert_eq!(std::fs::read_dir(run_root.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn ffmpeg_segments_are_private_bounded_and_cleaned_after_failure() {
    use std::os::unix::fs::PermissionsExt;

    let _env_lock = hermes_config::managed_gateway::test_lock::lock();
    let temp = trusted_tempdir();
    let bin = temp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ffmpeg = bin.join("ffmpeg");
    std::fs::write(
        &ffmpeg,
        "#!/bin/sh\nfor output do :; done\nout_dir=${output%/*}\nprintf 'converted MP3' > \"$out_dir/segment-0000.mp3\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{inherited_path}", bin.display());
    let _path = EnvVarGuard::set("PATH", &path);
    let recording = temp.path().join("recording.mp4");
    let segment_dir = temp.path().join("segments");
    std::fs::write(&recording, b"video fixture").unwrap();

    let audio = prepare_audio_paths_with_timeout(
        &recording,
        &segment_dir,
        true,
        std::time::Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_eq!(audio.len(), 1);
    assert_eq!(std::fs::read(&audio[0]).unwrap(), b"converted MP3");
    assert_eq!(
        std::fs::metadata(&audio[0]).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::remove_dir_all(&segment_dir).unwrap();

    std::fs::write(
        &ffmpeg,
        "#!/bin/sh\nfor output do :; done\nout_dir=${output%/*}\nprintf 'partial MP3' > \"$out_dir/segment-0000.mp3\"\nprintf 'fixture failure\\n' >&2\nexit 7\n",
    )
    .unwrap();
    let failure_dir = temp.path().join("failure-segments");
    let error = prepare_audio_paths_with_timeout(
        &recording,
        &failure_dir,
        true,
        std::time::Duration::from_secs(1),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("fixture failure"));
    std::fs::remove_dir_all(&failure_dir).unwrap();
}

#[test]
fn safe_file_name_strips_path_components_and_rejects_dot_only_names() {
    assert_eq!(safe_file_name("../../secret.mp4"), "secret.mp4");
    assert_eq!(safe_file_name(r"..\secret.mp4"), "secret.mp4");
    assert_eq!(
        safe_file_name("Meeting Recording?.mp4"),
        "Meeting_Recording_.mp4"
    );
    assert_eq!(safe_file_name("."), "recording.mp4");
    assert_eq!(safe_file_name(".."), "recording.mp4");
    assert_eq!(safe_file_name(""), "recording.mp4");
    assert_eq!(safe_file_name(r"///\\"), "recording.mp4");
    assert_eq!(safe_file_name("capture.exe"), "capture.mp4");
    assert_eq!(safe_file_name("CON.wav"), "recording.wav");
    assert_eq!(safe_file_name("NUL.mp3"), "recording.mp3");
    assert_eq!(safe_file_name("COM1.m4a"), "recording.m4a");
    assert_eq!(safe_file_name("lpt9.flac"), "recording.flac");
    assert_eq!(safe_file_name("CONIN$.ogg"), "recording.ogg");
    for (unsafe_name, expected) in [
        ("cOn.WaV", "recording.wav"),
        ("PRN...", "recording.mp4"),
        ("AUX...m4a", "recording.m4a"),
        ("COM9...mp3", "recording.mp3"),
        ("LPT1...ogg", "recording.ogg"),
        ("CONOUT$.webm", "recording.webm"),
        ("NUL ", "recording.mp4"),
        (" COM1... ", "recording.mp4"),
        ("LPT1 .ogg", "LPT1_.ogg"),
    ] {
        assert_eq!(safe_file_name(unsafe_name), expected, "{unsafe_name}");
    }
    let long_name = format!(
        "{}.MP3",
        "x".repeat(MAX_SAFE_TEAMS_RECORDING_STEM_BYTES + 1)
    );
    let safe_long_name = safe_file_name(&long_name);
    assert_eq!(
        safe_long_name.len(),
        MAX_SAFE_TEAMS_RECORDING_STEM_BYTES + ".mp3".len()
    );
    assert!(safe_long_name.ends_with(".mp3"));
}

#[test]
fn transcript_chunk_join_is_ordered_and_has_a_cumulative_bound() {
    let mut transcript = String::new();
    append_teams_transcript_chunk(&mut transcript, "first").unwrap();
    append_teams_transcript_chunk(&mut transcript, "second").unwrap();
    assert_eq!(transcript, "first\nsecond");

    let before = transcript.clone();
    let oversized = "x".repeat(MAX_TEAMS_TRANSCRIPT_BYTES);
    let error = append_teams_transcript_chunk(&mut transcript, &oversized).unwrap_err();
    assert!(matches!(error, TeamsPipelineError::Retryable(_)));
    assert_eq!(
        transcript, before,
        "an over-limit chunk must not publish a partial transcript"
    );
}

fn pipeline(
    graph: Arc<MockGraph>,
    store: Arc<TeamsPipelineStore>,
    summarizer: Arc<CountingSummarizer>,
    config: TeamsPipelineConfig,
) -> TeamsMeetingPipeline {
    TeamsMeetingPipeline::new(graph, store, config)
        .with_transcriber(Arc::new(MockTranscriber))
        .with_summarizer(summarizer)
}

#[test]
fn notification_receipt_hash_matches_canonical_shape() {
    let notification = json!({"b": 2, "a": 1});
    let key = TeamsPipelineStore::build_notification_receipt_key(&notification);
    let expected = Sha256::digest(br#"{"a":1,"b":2}"#);
    assert_eq!(key, format!("sha256:{}", hex::encode(expected)));
    assert_eq!(
        TeamsPipelineStore::build_notification_receipt_key(&json!({"id": "n1"})),
        "id:n1"
    );
}

#[test]
fn store_persists_state() {
    let tmp = trusted_tempdir();
    let path = tmp.path().join("teams_pipeline_store.json");
    let store = TeamsPipelineStore::new(&path).unwrap();
    store
        .upsert_subscription(
            "sub-1",
            json!({"client_state": "abc", "resource": "communications/onlineMeetings"}),
        )
        .unwrap();
    store
        .record_event_timestamp("evt-1", Some("2026-05-03T19:30:00Z".into()))
        .unwrap();
    store
        .upsert_job("job-1", json!({"status": "received", "event_id": "evt-1"}))
        .unwrap();

    let reloaded = TeamsPipelineStore::new(&path).unwrap();
    assert_eq!(
        reloaded
            .get_subscription("sub-1")
            .unwrap()
            .unwrap()
            .get("subscription_id")
            .unwrap(),
        "sub-1"
    );
    assert_eq!(
        reloaded.get_event_timestamp("evt-1").unwrap().as_deref(),
        Some("2026-05-03T19:30:00Z")
    );
    assert_eq!(
        reloaded
            .get_job("job-1")
            .unwrap()
            .unwrap()
            .get("status")
            .unwrap(),
        "received"
    );
}

#[tokio::test]
async fn transcript_first_path_persists_state() {
    let tmp = trusted_tempdir();
    let store = Arc::new(TeamsPipelineStore::new(tmp.path().join("store.json")).unwrap());
    let graph = Arc::new(MockGraph::default());
    *graph.transcript.lock() = Some((
        artifact("transcript", "tx-1"),
        "Action: Send draft by Friday.\nDecision: Ship the transcript-first path.\nDetailed transcript content.".into(),
    ));
    *graph.call_record.lock() = Some(MeetingArtifact {
        artifact_type: "call_record".into(),
        artifact_id: "call-1".into(),
        display_name: Some("call_record".into()),
        content_type: None,
        source_url: None,
        download_url: None,
        created_at: None,
        available_at: None,
        size_bytes: None,
        metadata: Map::from_iter([("metrics".into(), json!({"participant_count": 4}))]),
    });
    let summarizer = Arc::new(CountingSummarizer {
        count: AtomicUsize::new(0),
    });
    let pipeline = pipeline(
        graph,
        store.clone(),
        summarizer.clone(),
        TeamsPipelineConfig {
            transcript_min_chars: 20,
            ..TeamsPipelineConfig::default()
        },
    );

    let job = pipeline
        .run_notification(scoped_notification(
            "notif-1",
            "meeting-123",
            "transcript",
            "tx-1",
        ))
        .await
        .unwrap();

    assert_eq!(job.status, "completed");
    assert_eq!(
        job.selected_artifact_strategy.as_deref(),
        Some("transcript_first")
    );
    assert_eq!(
        job.summary_payload.unwrap().summary.as_deref(),
        Some("Short summary")
    );
    assert_eq!(
        store
            .get_job(&job.job_id)
            .unwrap()
            .unwrap()
            .get("status")
            .unwrap(),
        "completed"
    );
    assert_eq!(summarizer.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn recording_fallback_uses_transcriber_without_external_sink_side_effects() {
    let tmp = trusted_tempdir();
    let store = Arc::new(TeamsPipelineStore::new(tmp.path().join("store.json")).unwrap());
    let graph = Arc::new(MockGraph::default());
    *graph.recordings.lock() = vec![artifact("recording", "rec-1")];
    *graph.recording_bytes.lock() = b"recording bytes".to_vec();
    let pipeline = pipeline(
        graph,
        store,
        Arc::new(CountingSummarizer {
            count: AtomicUsize::new(0),
        }),
        TeamsPipelineConfig::default(),
    );

    let job = pipeline
        .run_notification(scoped_notification(
            "notif-2",
            "meeting-456",
            "recording",
            "rec-1",
        ))
        .await
        .unwrap();

    assert_eq!(job.status, "completed");
    assert_eq!(
        job.selected_artifact_strategy.as_deref(),
        Some("recording_stt_fallback")
    );
}

#[tokio::test]
async fn missing_transcript_and_recording_schedules_retry() {
    let tmp = trusted_tempdir();
    let store = Arc::new(TeamsPipelineStore::new(tmp.path().join("store.json")).unwrap());
    let graph = Arc::new(MockGraph::default());
    let summarizer = Arc::new(CountingSummarizer {
        count: AtomicUsize::new(0),
    });
    let pipeline = pipeline(graph, store, summarizer, TeamsPipelineConfig::default());

    let job = pipeline
        .run_notification(scoped_notification(
            "notif-3",
            "meeting-789",
            "recording",
            "recording-unavailable",
        ))
        .await
        .unwrap();

    assert_eq!(job.status, "retry_scheduled");
    assert_eq!(job.error_info.get("retryable"), Some(&Value::Bool(true)));
    assert!(job
        .error_info
        .get("message")
        .and_then(Value::as_str)
        .unwrap()
        .contains("Recording unavailable"));
}

#[tokio::test]
async fn duplicate_notification_reuses_completed_job() {
    let tmp = trusted_tempdir();
    let store = Arc::new(TeamsPipelineStore::new(tmp.path().join("store.json")).unwrap());
    let graph = Arc::new(MockGraph::default());
    *graph.transcript.lock() = Some((
        artifact("transcript", "tx-dup"),
        "Decision: Keep duplicate notifications idempotent.\nAction: Verify cached job reuse."
            .into(),
    ));
    let summarizer = Arc::new(CountingSummarizer {
        count: AtomicUsize::new(0),
    });
    let pipeline = pipeline(
        graph,
        store.clone(),
        summarizer.clone(),
        TeamsPipelineConfig {
            transcript_min_chars: 20,
            ..TeamsPipelineConfig::default()
        },
    );
    let notification = scoped_notification("notif-dup", "meeting-dup", "transcript", "tx-dup");

    let first = pipeline
        .run_notification(notification.clone())
        .await
        .unwrap();
    let second = pipeline.run_notification(notification).await.unwrap();

    assert_eq!(first.status, "completed");
    assert_eq!(second.status, "completed");
    assert_eq!(first.job_id, second.job_id);
    assert_eq!(summarizer.count.load(Ordering::SeqCst), 1);
    assert_eq!(store.list_jobs().unwrap().len(), 1);
}

#[test]
fn summary_helpers_match_python_shape() {
    let parsed = parse_summary_json(
        r#"prefix {"summary":"ok","key_decisions":["Decision: yes"],"action_items":["Action: go"],"risks":["Risk: x"],"confidence":"high","confidence_notes":"clean"} suffix"#,
    )
    .unwrap();
    assert_eq!(parsed.summary, "ok");
    assert_eq!(parsed.key_decisions, vec!["Decision: yes"]);
    assert_eq!(parsed.confidence, "high");

    let meeting = TeamsMeetingRef {
        meeting_id: "m1".into(),
        organizer_user_id: None,
        join_web_url: None,
        calendar_event_id: None,
        thread_id: None,
        tenant_id: None,
        metadata: Map::from_iter([("subject".into(), Value::String("Demo".into()))]),
    };
    let prompt = build_summary_prompt(&meeting, "hello", &[artifact("transcript", "tx")]);
    assert!(prompt.contains("Meeting ID: m1"));
    assert!(prompt.contains("Title: Demo"));
}

#[test]
fn resource_meeting_id_extraction_and_change_type_defaults() {
    assert_eq!(
        extract_meeting_id_from_resource("communications/onlineMeetings/meeting-1/transcripts")
            .as_deref(),
        Some("meeting-1")
    );
    assert_eq!(
        default_change_type_for_resource("communications/onlineMeetings/getAllTranscripts"),
        "created"
    );
    assert_eq!(
        default_change_type_for_resource("communications/onlineMeetings/meeting-1"),
        "updated"
    );
}

#[test]
fn rust_pipeline_config_rejects_external_sink_claims() {
    let error = TeamsPipelineConfig::from_value(Some(json!({
        "notion": {"enabled": true},
        "linear": {"enabled": true},
        "teams_delivery": {"enabled": true},
    })))
    .unwrap_err();
    assert!(matches!(
        error,
        TeamsPipelineError::Config(message)
            if message.contains("notion, linear, teams_delivery")
                && message.contains("unsupported in this Rust release")
                && message.contains("not packaged or verified")
    ));
}
