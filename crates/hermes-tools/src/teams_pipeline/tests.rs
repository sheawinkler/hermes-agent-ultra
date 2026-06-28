use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

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
            organizer_user_id: None,
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
        Ok(self.transcript.lock().unwrap().clone())
    }

    async fn list_recording_artifacts(
        &self,
        _meeting_ref: &TeamsMeetingRef,
    ) -> TeamsPipelineResult<Vec<MeetingArtifact>> {
        Ok(self.recordings.lock().unwrap().clone())
    }

    async fn download_recording_artifact(
        &self,
        _meeting_ref: &TeamsMeetingRef,
        _recording: &MeetingArtifact,
    ) -> TeamsPipelineResult<Vec<u8>> {
        Ok(self.recording_bytes.lock().unwrap().clone())
    }

    async fn enrich_meeting_with_call_record(
        &self,
        _meeting_ref: &TeamsMeetingRef,
        _call_record_id: Option<&str>,
    ) -> TeamsPipelineResult<Option<MeetingArtifact>> {
        Ok(self.call_record.lock().unwrap().clone())
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
            notion_target: None,
            linear_target: None,
            teams_target: None,
        })
    }
}

struct MockSink;

#[async_trait]
impl TeamsSinkWriter for MockSink {
    async fn write_summary(
        &self,
        sink: TeamsSinkKind,
        _payload: &TeamsMeetingSummaryPayload,
        _config: &Value,
        existing_record: Option<Value>,
    ) -> TeamsPipelineResult<Value> {
        let id_key = match sink {
            TeamsSinkKind::Notion => "page_id",
            TeamsSinkKind::Linear => "issue_id",
            TeamsSinkKind::Teams => "message_id",
        };
        Ok(json!({ id_key: existing_record
            .as_ref()
            .and_then(|v| v.get(id_key))
            .cloned()
            .unwrap_or_else(|| Value::String(format!("{}-1", sink.as_str()))) }))
    }
}

fn artifact(kind: &str, id: &str) -> MeetingArtifact {
    MeetingArtifact {
        artifact_type: kind.into(),
        artifact_id: id.into(),
        display_name: Some(format!("{id}.vtt")),
        content_type: None,
        source_url: None,
        download_url: Some(format!("https://example.com/{id}")),
        created_at: None,
        available_at: None,
        size_bytes: None,
        metadata: Map::new(),
    }
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
    let tmp = tempfile::tempdir().unwrap();
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
    store
        .upsert_sink_record("notion:meeting-1", json!({"page_id": "page-1"}))
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
    assert_eq!(
        reloaded
            .get_sink_record("notion:meeting-1")
            .unwrap()
            .unwrap()
            .get("page_id")
            .unwrap(),
        "page-1"
    );
}

#[tokio::test]
async fn transcript_first_path_persists_state() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(TeamsPipelineStore::new(tmp.path().join("store.json")).unwrap());
    let graph = Arc::new(MockGraph::default());
    *graph.transcript.lock().unwrap() = Some((
        artifact("transcript", "tx-1"),
        "Action: Send draft by Friday.\nDecision: Ship the transcript-first path.\nDetailed transcript content.".into(),
    ));
    *graph.call_record.lock().unwrap() = Some(MeetingArtifact {
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
        .run_notification(json!({
            "id": "notif-1",
            "changeType": "updated",
            "resource": "communications/onlineMeetings/meeting-123",
            "resourceData": {"id": "meeting-123"}
        }))
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
async fn recording_fallback_uses_transcriber_and_sinks() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(TeamsPipelineStore::new(tmp.path().join("store.json")).unwrap());
    let graph = Arc::new(MockGraph::default());
    *graph.recordings.lock().unwrap() = vec![artifact("recording", "rec-1")];
    *graph.recording_bytes.lock().unwrap() = b"recording bytes".to_vec();
    let summarizer = Arc::new(CountingSummarizer {
        count: AtomicUsize::new(0),
    });
    let pipeline = pipeline(
        graph,
        store.clone(),
        summarizer,
        TeamsPipelineConfig {
            notion: Some(json!({"enabled": true, "database_id": "db-1"})),
            teams_delivery: Some(json!({"enabled": true, "channel_id": "channel-1"})),
            ..TeamsPipelineConfig::default()
        },
    )
    .with_notion_writer(Arc::new(MockSink))
    .with_teams_sender(Arc::new(MockSink));

    let job = pipeline
        .run_notification(json!({
            "id": "notif-2",
            "changeType": "updated",
            "resource": "communications/onlineMeetings/meeting-456",
            "resourceData": {"id": "meeting-456"}
        }))
        .await
        .unwrap();

    assert_eq!(job.status, "completed");
    assert_eq!(
        job.selected_artifact_strategy.as_deref(),
        Some("recording_stt_fallback")
    );
    assert_eq!(
        store
            .get_sink_record("notion:meeting-456")
            .unwrap()
            .unwrap()
            .get("page_id")
            .unwrap(),
        "notion-1"
    );
    assert_eq!(
        store
            .get_sink_record("teams:meeting-456")
            .unwrap()
            .unwrap()
            .get("message_id")
            .unwrap(),
        "teams-1"
    );
}

#[tokio::test]
async fn missing_transcript_and_recording_schedules_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(TeamsPipelineStore::new(tmp.path().join("store.json")).unwrap());
    let graph = Arc::new(MockGraph::default());
    let summarizer = Arc::new(CountingSummarizer {
        count: AtomicUsize::new(0),
    });
    let pipeline = pipeline(graph, store, summarizer, TeamsPipelineConfig::default());

    let job = pipeline
        .run_notification(json!({
            "id": "notif-3",
            "changeType": "updated",
            "resource": "communications/onlineMeetings/meeting-789",
            "resourceData": {"id": "meeting-789"}
        }))
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
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(TeamsPipelineStore::new(tmp.path().join("store.json")).unwrap());
    let graph = Arc::new(MockGraph::default());
    *graph.transcript.lock().unwrap() = Some((
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
    let notification = json!({
        "id": "notif-dup",
        "changeType": "updated",
        "resource": "communications/onlineMeetings/meeting-dup",
        "resourceData": {"id": "meeting-dup"}
    });

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
