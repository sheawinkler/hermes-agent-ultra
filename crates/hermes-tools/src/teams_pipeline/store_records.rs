impl GraphSubscription {
    pub fn from_value(value: Value) -> TeamsPipelineResult<Self> {
        let record: Self = serde_json::from_value(value)?;
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> TeamsPipelineResult<()> {
        if self.subscription_id.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "GraphSubscription.subscription_id is required.".into(),
            ));
        }
        if self.resource.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "GraphSubscription.resource is required.".into(),
            ));
        }
        if self.change_type.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "GraphSubscription.change_type is required.".into(),
            ));
        }
        if self.notification_url.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "GraphSubscription.notification_url is required.".into(),
            ));
        }
        if self.expiration_datetime.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "GraphSubscription.expiration_datetime is required.".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamsMeetingRef {
    #[serde(default, alias = "id")]
    pub meeting_id: String,
    #[serde(
        default,
        alias = "organizerUserId",
        skip_serializing_if = "Option::is_none"
    )]
    pub organizer_user_id: Option<String>,
    #[serde(default, alias = "joinWebUrl", skip_serializing_if = "Option::is_none")]
    pub join_web_url: Option<String>,
    #[serde(
        default,
        alias = "calendarEventId",
        skip_serializing_if = "Option::is_none"
    )]
    pub calendar_event_id: Option<String>,
    #[serde(default, alias = "threadId", skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, alias = "tenantId", skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_map")]
    pub metadata: Map<String, Value>,
}

impl TeamsMeetingRef {
    pub fn from_value(value: Value) -> TeamsPipelineResult<Self> {
        let record: Self = serde_json::from_value(value)?;
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> TeamsPipelineResult<()> {
        if self.meeting_id.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "TeamsMeetingRef.meeting_id is required.".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeetingArtifact {
    #[serde(default, alias = "artifactType")]
    pub artifact_type: String,
    #[serde(default, alias = "id")]
    pub artifact_id: String,
    #[serde(
        default,
        alias = "displayName",
        alias = "name",
        skip_serializing_if = "Option::is_none"
    )]
    pub display_name: Option<String>,
    #[serde(
        default,
        alias = "contentType",
        skip_serializing_if = "Option::is_none"
    )]
    pub content_type: Option<String>,
    #[serde(
        default,
        alias = "sourceUrl",
        alias = "webUrl",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_url: Option<String>,
    #[serde(
        default,
        alias = "downloadUrl",
        alias = "@microsoft.graph.downloadUrl",
        skip_serializing_if = "Option::is_none"
    )]
    pub download_url: Option<String>,
    #[serde(
        default,
        alias = "createdDateTime",
        skip_serializing_if = "Option::is_none"
    )]
    pub created_at: Option<String>,
    #[serde(
        default,
        alias = "availableDateTime",
        alias = "lastModifiedDateTime",
        skip_serializing_if = "Option::is_none"
    )]
    pub available_at: Option<String>,
    #[serde(default, alias = "size", skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_map")]
    pub metadata: Map<String, Value>,
}

impl MeetingArtifact {
    pub fn from_value(value: Value) -> TeamsPipelineResult<Self> {
        let record: Self = serde_json::from_value(value)?;
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> TeamsPipelineResult<()> {
        if !matches!(
            self.artifact_type.as_str(),
            "transcript" | "recording" | "call_record"
        ) {
            return Err(TeamsPipelineError::Invalid(
                "MeetingArtifact.artifact_type must be transcript, recording, or call_record."
                    .into(),
            ));
        }
        if self.artifact_id.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "MeetingArtifact.artifact_id is required.".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamsMeetingSummaryPayload {
    pub meeting_ref: TeamsMeetingRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, alias = "startTime", skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(default, alias = "endTime", skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<String>,
    #[serde(
        default,
        alias = "transcriptText",
        skip_serializing_if = "Option::is_none"
    )]
    pub transcript_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, alias = "keyDecisions", skip_serializing_if = "Vec::is_empty")]
    pub key_decisions: Vec<String>,
    #[serde(default, alias = "actionItems", skip_serializing_if = "Vec::is_empty")]
    pub action_items: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risks: Vec<String>,
    #[serde(default, alias = "callMetrics", skip_serializing_if = "skip_map")]
    pub call_metrics: Map<String, Value>,
    #[serde(
        default,
        alias = "sourceArtifacts",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub source_artifacts: Vec<MeetingArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(
        default,
        alias = "confidenceNotes",
        skip_serializing_if = "Option::is_none"
    )]
    pub confidence_notes: Option<String>,
    #[serde(
        default,
        alias = "notionTarget",
        skip_serializing_if = "Option::is_none"
    )]
    pub notion_target: Option<String>,
    #[serde(
        default,
        alias = "linearTarget",
        skip_serializing_if = "Option::is_none"
    )]
    pub linear_target: Option<String>,
    #[serde(
        default,
        alias = "teamsTarget",
        skip_serializing_if = "Option::is_none"
    )]
    pub teams_target: Option<String>,
}

impl TeamsMeetingSummaryPayload {
    pub fn from_value(value: Value) -> TeamsPipelineResult<Self> {
        Ok(serde_json::from_value(value)?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamsMeetingPipelineJob {
    #[serde(default, alias = "jobId")]
    pub job_id: String,
    #[serde(default, alias = "eventId")]
    pub event_id: String,
    #[serde(default, alias = "sourceEventType")]
    pub source_event_type: String,
    #[serde(default, alias = "dedupeKey")]
    pub dedupe_key: String,
    #[serde(default)]
    pub status: String,
    #[serde(default, alias = "retryCount")]
    pub retry_count: u32,
    #[serde(default, alias = "createdAt", skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, alias = "updatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, alias = "meetingRef", skip_serializing_if = "Option::is_none")]
    pub meeting_ref: Option<TeamsMeetingRef>,
    #[serde(
        default,
        alias = "selectedArtifactStrategy",
        skip_serializing_if = "Option::is_none"
    )]
    pub selected_artifact_strategy: Option<String>,
    #[serde(
        default,
        alias = "summaryPayload",
        skip_serializing_if = "Option::is_none"
    )]
    pub summary_payload: Option<TeamsMeetingSummaryPayload>,
    #[serde(default, alias = "errorInfo", skip_serializing_if = "skip_map")]
    pub error_info: Map<String, Value>,
}

impl TeamsMeetingPipelineJob {
    pub fn from_value(value: Value) -> TeamsPipelineResult<Self> {
        let record: Self = serde_json::from_value(value)?;
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> TeamsPipelineResult<()> {
        if self.job_id.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "TeamsMeetingPipelineJob.job_id is required.".into(),
            ));
        }
        if self.event_id.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "TeamsMeetingPipelineJob.event_id is required.".into(),
            ));
        }
        if self.source_event_type.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "TeamsMeetingPipelineJob.source_event_type is required.".into(),
            ));
        }
        if self.dedupe_key.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "TeamsMeetingPipelineJob.dedupe_key is required.".into(),
            ));
        }
        if self.status.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "TeamsMeetingPipelineJob.status is required.".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TeamsPipelineState {
    #[serde(default)]
    subscriptions: BTreeMap<String, Value>,
    #[serde(default)]
    notification_receipts: BTreeMap<String, Value>,
    #[serde(default)]
    event_timestamps: BTreeMap<String, Value>,
    #[serde(default)]
    jobs: BTreeMap<String, Value>,
    #[serde(default)]
    sink_records: BTreeMap<String, Value>,
}

#[derive(Debug)]
pub struct TeamsPipelineStore {
    path: PathBuf,
    state: Mutex<TeamsPipelineState>,
}

impl TeamsPipelineStore {
    pub fn new(path: impl Into<PathBuf>) -> TeamsPipelineResult<Self> {
        let path = path.into();
        let state = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            if raw.trim().is_empty() {
                TeamsPipelineState::default()
            } else {
                serde_json::from_str(&raw).unwrap_or_default()
            }
        } else {
            TeamsPipelineState::default()
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn build_notification_receipt_key(notification: &Value) -> String {
        if let Some(id) = object_field(notification, "id").and_then(value_to_string) {
            return format!("id:{id}");
        }
        let canonical = canonical_json(notification);
        let digest = Sha256::digest(canonical.as_bytes());
        format!("sha256:{}", hex::encode(digest))
    }

    pub fn stats(&self) -> TeamsPipelineResult<BTreeMap<String, usize>> {
        let state = self.lock_state()?;
        Ok(BTreeMap::from([
            ("subscriptions".into(), state.subscriptions.len()),
            (
                "notification_receipts".into(),
                state.notification_receipts.len(),
            ),
            ("event_timestamps".into(), state.event_timestamps.len()),
            ("jobs".into(), state.jobs.len()),
            ("sink_records".into(), state.sink_records.len()),
        ]))
    }

    pub fn list_subscriptions(&self) -> TeamsPipelineResult<BTreeMap<String, Value>> {
        Ok(self.lock_state()?.subscriptions.clone())
    }

    pub fn get_subscription(&self, subscription_id: &str) -> TeamsPipelineResult<Option<Value>> {
        Ok(self
            .lock_state()?
            .subscriptions
            .get(subscription_id)
            .cloned())
    }

    pub fn upsert_subscription(
        &self,
        subscription_id: &str,
        payload: Value,
    ) -> TeamsPipelineResult<Value> {
        self.upsert_record("subscriptions", subscription_id, payload)
    }

    pub fn delete_subscription(&self, subscription_id: &str) -> TeamsPipelineResult<bool> {
        let removed = {
            let mut state = self.lock_state()?;
            state.subscriptions.remove(subscription_id).is_some()
        };
        if removed {
            self.persist()?;
        }
        Ok(removed)
    }

    pub fn has_notification_receipt(&self, receipt_key: &str) -> TeamsPipelineResult<bool> {
        Ok(self
            .lock_state()?
            .notification_receipts
            .contains_key(receipt_key))
    }

    pub fn record_notification_receipt(
        &self,
        receipt_key: &str,
        payload: Option<Value>,
        received_at: Option<String>,
    ) -> TeamsPipelineResult<bool> {
        let inserted = {
            let mut state = self.lock_state()?;
            if state.notification_receipts.contains_key(receipt_key) {
                return Ok(false);
            }
            state.notification_receipts.insert(
                receipt_key.to_string(),
                json!({
                    "received_at": received_at.unwrap_or_else(utc_now_iso),
                    "payload": payload
                }),
            );
            true
        };
        self.persist()?;
        Ok(inserted)
    }

    pub fn record_event_timestamp(
        &self,
        event_key: &str,
        timestamp: Option<String>,
    ) -> TeamsPipelineResult<String> {
        let timestamp = timestamp.unwrap_or_else(utc_now_iso);
        {
            let mut state = self.lock_state()?;
            state
                .event_timestamps
                .insert(event_key.to_string(), Value::String(timestamp.clone()));
        }
        self.persist()?;
        Ok(timestamp)
    }

    pub fn get_event_timestamp(&self, event_key: &str) -> TeamsPipelineResult<Option<String>> {
        Ok(self
            .lock_state()?
            .event_timestamps
            .get(event_key)
            .and_then(value_to_string))
    }

    pub fn upsert_job(&self, job_id: &str, payload: Value) -> TeamsPipelineResult<Value> {
        self.upsert_record("jobs", job_id, payload)
    }

    pub fn get_job(&self, job_id: &str) -> TeamsPipelineResult<Option<Value>> {
        Ok(self.lock_state()?.jobs.get(job_id).cloned())
    }

    pub fn list_jobs(&self) -> TeamsPipelineResult<BTreeMap<String, Value>> {
        Ok(self.lock_state()?.jobs.clone())
    }

    pub fn upsert_sink_record(&self, sink_key: &str, payload: Value) -> TeamsPipelineResult<Value> {
        self.upsert_record("sink_records", sink_key, payload)
    }

    pub fn get_sink_record(&self, sink_key: &str) -> TeamsPipelineResult<Option<Value>> {
        Ok(self.lock_state()?.sink_records.get(sink_key).cloned())
    }

    fn lock_state(&self) -> TeamsPipelineResult<std::sync::MutexGuard<'_, TeamsPipelineState>> {
        self.state
            .lock()
            .map_err(|_| TeamsPipelineError::Store("teams pipeline store lock poisoned".into()))
    }

    fn upsert_record(&self, bucket: &str, key: &str, payload: Value) -> TeamsPipelineResult<Value> {
        let stored = {
            let mut state = self.lock_state()?;
            let records = match bucket {
                "subscriptions" => &mut state.subscriptions,
                "jobs" => &mut state.jobs,
                "sink_records" => &mut state.sink_records,
                _ => {
                    return Err(TeamsPipelineError::Store(format!(
                        "unknown store bucket: {bucket}"
                    )))
                }
            };
            let now = utc_now_iso();
            let existing = records
                .get(key)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let mut merged = existing.clone();
            if let Some(object) = payload.as_object() {
                for (field, value) in object {
                    merged.insert(field.clone(), value.clone());
                }
            }
            let id_field = match bucket {
                "subscriptions" => "subscription_id",
                "jobs" => "job_id",
                "sink_records" => "sink_key",
                _ => "id",
            };
            merged.insert(id_field.into(), Value::String(key.to_string()));
            if !merged.contains_key("created_at") {
                let created = existing
                    .get("created_at")
                    .cloned()
                    .unwrap_or_else(|| Value::String(now.clone()));
                merged.insert("created_at".into(), created);
            }
            merged.insert("updated_at".into(), Value::String(now));
            let value = Value::Object(merged);
            records.insert(key.to_string(), value.clone());
            value
        };
        self.persist()?;
        Ok(stored)
    }

    fn persist(&self) -> TeamsPipelineResult<()> {
        let state = self.lock_state()?.clone();
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_vec_pretty(&state)?;
        let tmp = self
            .path
            .with_extension(format!("json.tmp-{}", Uuid::new_v4().simple()));
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

