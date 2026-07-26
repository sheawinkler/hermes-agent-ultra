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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamsPipelineStoreLimits {
    /// Full jobs are retained only while their terminal record is useful for
    /// operator inspection. A compact receipt tombstone survives separately
    /// for the Graph redelivery horizon.
    pub max_jobs: usize,
    pub max_serialized_bytes: usize,
    pub terminal_job_ttl: Duration,
    pub max_notification_tombstones: usize,
    pub notification_tombstone_ttl: Duration,
}

impl Default for TeamsPipelineStoreLimits {
    fn default() -> Self {
        Self {
            max_jobs: 1_024,
            max_serialized_bytes: 8 * 1024 * 1024,
            terminal_job_ttl: Duration::from_secs(30 * 24 * 60 * 60),
            max_notification_tombstones: 8_192,
            notification_tombstone_ttl: Duration::from_secs(7 * 24 * 60 * 60),
        }
    }
}

impl TeamsPipelineStoreLimits {
    fn validate(&self) -> TeamsPipelineResult<()> {
        if self.max_jobs == 0
            || self.max_serialized_bytes == 0
            || self.max_notification_tombstones == 0
            || self.terminal_job_ttl.is_zero()
            || self.notification_tombstone_ttl.is_zero()
        {
            return Err(TeamsPipelineError::Config(
                "Teams pipeline store limits must all be finite and non-zero.".into(),
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
}

#[derive(Debug)]
pub struct TeamsPipelineStore {
    path: PathBuf,
    limits: TeamsPipelineStoreLimits,
    state: Mutex<TeamsPipelineState>,
}

const TEAMS_STORE_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const TEAMS_STORE_LOCK_RETRY: Duration = Duration::from_millis(10);

/// An advisory lock is held for the whole read-modify-write transaction so
/// independently started Hermes processes cannot both reserve the same sink
/// effect. OS locks are released if a process exits unexpectedly.
struct TeamsStoreFileLock {
    file: std::fs::File,
}

impl TeamsStoreFileLock {
    fn acquire(store_path: &Path) -> TeamsPipelineResult<Self> {
        Self::acquire_path(&store_path.with_extension("lock"))
    }

    fn acquire_path(lock_path: &Path) -> TeamsPipelineResult<Self> {
        let deadline = std::time::Instant::now() + TEAMS_STORE_LOCK_TIMEOUT;
        loop {
            match Self::try_acquire_path(lock_path)? {
                Some(lock) => return Ok(lock),
                None if std::time::Instant::now() < deadline => {
                    std::thread::sleep(TEAMS_STORE_LOCK_RETRY);
                }
                None => {
                    return Err(TeamsPipelineError::Store(format!(
                        "Timed out waiting for Teams pipeline store transaction lock {}.",
                        lock_path.display()
                    )));
                }
            }
        }
    }

    fn try_acquire_path(lock_path: &Path) -> TeamsPipelineResult<Option<Self>> {
        let file = open_teams_store_lock_file(lock_path).map_err(|error| {
            TeamsPipelineError::Store(format!(
                "Could not open Teams pipeline store lock {}: {error}",
                lock_path.display()
            ))
        })?;
        match try_lock_teams_store_file(&file) {
            Ok(true) => Ok(Some(Self { file })),
            Ok(false) => Ok(None),
            Err(error) => Err(TeamsPipelineError::Store(format!(
                "Could not lock Teams pipeline store transaction {}: {error}",
                lock_path.display()
            ))),
        }
    }
}

impl Drop for TeamsStoreFileLock {
    fn drop(&mut self) {
        let _ = unlock_teams_store_file(&self.file);
    }
}

#[cfg(unix)]
fn open_teams_store_lock_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_teams_store_lock_file(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
}

#[cfg(unix)]
fn try_lock_teams_store_file(file: &std::fs::File) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;

    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if matches!(error.raw_os_error(), Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(unix)]
fn unlock_teams_store_file(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn try_lock_teams_store_file(file: &std::fs::File) -> std::io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let mut overlapped = OVERLAPPED::default();
    let locked = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    };
    if locked != 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(33) {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(windows)]
fn unlock_teams_store_file(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let mut overlapped = OVERLAPPED::default();
    if unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped) } != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

impl TeamsPipelineStore {
    pub fn new(path: impl Into<PathBuf>) -> TeamsPipelineResult<Self> {
        Self::with_limits(path, TeamsPipelineStoreLimits::default())
    }

    pub fn with_limits(
        path: impl Into<PathBuf>,
        limits: TeamsPipelineStoreLimits,
    ) -> TeamsPipelineResult<Self> {
        limits.validate()?;
        let path = path.into();
        ensure_teams_store_parent(&path)?;
        let _transaction_lock = TeamsStoreFileLock::acquire(&path)?;
        let (state, normalized) = load_teams_pipeline_state(&path, &limits)?;
        if normalized {
            persist_teams_pipeline_state(&path, &state, &limits)?;
        }
        Ok(Self {
            path,
            limits,
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
        self.transact(|state| {
            let removed = state.subscriptions.remove(subscription_id).is_some();
            Ok((removed, removed))
        })
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
        let payload_digest = payload.as_ref().map(notification_payload_digest);
        self.transact(|state| {
            if state.notification_receipts.contains_key(receipt_key) {
                return Ok((false, false));
            }
            state.notification_receipts.insert(
                receipt_key.to_string(),
                compact_notification_receipt(
                    received_at.clone().unwrap_or_else(utc_now_iso),
                    payload_digest.clone(),
                    None,
                    None,
                ),
            );
            Ok((true, true))
        })
    }

    pub fn record_event_timestamp(
        &self,
        event_key: &str,
        timestamp: Option<String>,
    ) -> TeamsPipelineResult<String> {
        let timestamp = timestamp.unwrap_or_else(utc_now_iso);
        self.transact(|state| {
            state
                .event_timestamps
                .insert(event_key.to_string(), Value::String(timestamp.clone()));
            Ok((timestamp.clone(), true))
        })
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

    pub fn delete_job(&self, job_id: &str) -> TeamsPipelineResult<bool> {
        self.transact(|state| {
            let removed = state.jobs.remove(job_id).is_some();
            Ok((removed, removed))
        })
    }

    pub fn get_job(&self, job_id: &str) -> TeamsPipelineResult<Option<Value>> {
        Ok(self.lock_state()?.jobs.get(job_id).cloned())
    }

    pub fn list_jobs(&self) -> TeamsPipelineResult<BTreeMap<String, Value>> {
        Ok(self.lock_state()?.jobs.clone())
    }


    fn create_notification_job(
        &self,
        receipt_key: &str,
        notification: &Value,
        job: TeamsMeetingPipelineJob,
    ) -> TeamsPipelineResult<TeamsMeetingPipelineJob> {
        let receipt_digest = notification_payload_digest(notification);
        self.transact(|state| {
            if let Some(receipt) = state.notification_receipts.get(receipt_key) {
                return Ok((notification_receipt_job(state, receipt)?, false));
            }
            if let Some(existing) = state
                .jobs
                .values()
                .find(|value| value.get("dedupe_key").and_then(Value::as_str) == Some(receipt_key))
                .cloned()
            {
                let existing_job = TeamsMeetingPipelineJob::from_value(existing)?;
                state.notification_receipts.insert(
                    receipt_key.to_string(),
                    compact_notification_receipt(
                        utc_now_iso(),
                        Some(receipt_digest.clone()),
                        Some(existing_job.job_id.clone()),
                        None,
                    ),
                );
                return Ok((existing_job, true));
            }

            let stored = merge_store_record(&BTreeMap::new(), "jobs", &job.job_id, serde_json::to_value(&job)?)?;
            let stored_job = TeamsMeetingPipelineJob::from_value(stored.clone())?;
            state.jobs.insert(job.job_id.clone(), stored);
            state.notification_receipts.insert(
                receipt_key.to_string(),
                compact_notification_receipt(
                    utc_now_iso(),
                    Some(receipt_digest),
                    Some(stored_job.job_id.clone()),
                    None,
                ),
            );
            Ok((stored_job, true))
        })
    }

    fn lock_state(&self) -> TeamsPipelineResult<parking_lot::MutexGuard<'_, TeamsPipelineState>> {
        Ok(self.state.lock())
    }
    fn transact<T>(
        &self,
        operation: impl FnOnce(&mut TeamsPipelineState) -> TeamsPipelineResult<(T, bool)>,
    ) -> TeamsPipelineResult<T> {
        let mut state = self.lock_state()?;
        let _transaction_lock = TeamsStoreFileLock::acquire(&self.path)?;
        let (mut candidate, _) = load_teams_pipeline_state(&self.path, &self.limits)?;
        let (output, changed) = operation(&mut candidate)?;
        let normalized = normalize_teams_pipeline_state(&mut candidate, &self.limits)?;
        if changed || normalized {
            persist_teams_pipeline_state(&self.path, &candidate, &self.limits)?;
        }
        *state = candidate;
        Ok(output)
    }

    fn upsert_record(
        &self,
        bucket: &str,
        key: &str,
        payload: Value,
    ) -> TeamsPipelineResult<Value> {
        self.transact(|state| {
            let records = match bucket {
                "subscriptions" => &mut state.subscriptions,
                "jobs" => &mut state.jobs,
                _ => {
                    return Err(TeamsPipelineError::Store(format!(
                        "unknown store bucket: {bucket}"
                    )))
                }
            };
            let stored = merge_store_record(records, bucket, key, payload)?;
            records.insert(key.to_string(), stored.clone());
            Ok((stored, true))
        })
    }
}

fn load_teams_pipeline_state(
    path: &Path,
    limits: &TeamsPipelineStoreLimits,
) -> TeamsPipelineResult<(TeamsPipelineState, bool)> {
    let destination_exists = inspect_teams_store_destination(path)?;
    if destination_exists {
        validate_private_teams_store_destination(path)?;
    }
    let mut state = if destination_exists {
        let raw = std::fs::read_to_string(path)?;
        if raw.trim().is_empty() {
            return Err(TeamsPipelineError::Store(format!(
                "Teams pipeline store {} is empty rather than valid JSON.",
                path.display()
            )));
        }
        serde_json::from_str(&raw).map_err(|error| {
            TeamsPipelineError::Store(format!(
                "Teams pipeline store {} contains malformed JSON: {error}",
                path.display()
            ))
        })?
    } else {
        TeamsPipelineState::default()
    };
    let normalized = normalize_teams_pipeline_state(&mut state, limits)?;
    Ok((state, normalized))
}

fn merge_store_record(
    records: &BTreeMap<String, Value>,
    bucket: &str,
    key: &str,
    payload: Value,
) -> TeamsPipelineResult<Value> {
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
        _ => {
            return Err(TeamsPipelineError::Store(format!(
                "unknown store bucket: {bucket}"
            )))
        }
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
    Ok(if bucket == "jobs" {
        redact_job_payload_for_store(value)
    } else {
        value
    })
}

fn teams_store_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn ensure_teams_store_parent(path: &Path) -> TeamsPipelineResult<()> {
    let parent = teams_store_parent(path);
    std::fs::create_dir_all(parent)?;
    crate::tts_audio::ensure_safe_private_media_parent(
        &parent.join(".teams-pipeline-store-parent-probe"),
    )
    .map_err(|error| {
        TeamsPipelineError::Store(format!(
            "Teams pipeline store parent {} is not a trusted private-media root: {error}",
            parent.display()
        ))
    })
}

fn inspect_teams_store_destination(path: &Path) -> TeamsPipelineResult<bool> {
    crate::tts_audio::inspect_output_destination(path).map_err(|error| {
        TeamsPipelineError::Store(format!(
            "Teams pipeline store destination {} is invalid: {error}",
            path.display()
        ))
    })
}

fn validate_private_teams_store_destination(path: &Path) -> TeamsPipelineResult<()> {
    crate::tts_audio::validate_private_media_file(path).map_err(|error| {
        TeamsPipelineError::Store(format!(
            "Teams pipeline store destination {} is not owner-only: {error}",
            path.display()
        ))
    })
}

fn persist_teams_pipeline_state(
    path: &Path,
    state: &TeamsPipelineState,
    limits: &TeamsPipelineStoreLimits,
) -> TeamsPipelineResult<()> {
    ensure_teams_store_parent(path)?;
    let destination_exists = inspect_teams_store_destination(path)?;
    if destination_exists {
        validate_private_teams_store_destination(path)?;
    }
    let body = serde_json::to_vec_pretty(state)?;
    if body.len() > limits.max_serialized_bytes {
        return Err(TeamsPipelineError::Store(format!(
            "Teams pipeline store serialization is {} bytes, above the {} byte limit.",
            body.len(),
            limits.max_serialized_bytes
        )));
    }
    let parent = teams_store_parent(path);
    let temporary = parent.join(format!(
        ".teams-pipeline-store-{}.tmp",
        Uuid::new_v4().simple()
    ));
    let result = (|| -> TeamsPipelineResult<()> {
        let mut file = crate::tts_audio::create_private_temp_file(
            &temporary,
            path,
            destination_exists,
        )
        .map_err(|error| {
            TeamsPipelineError::Store(format!(
                "could not create owner-only Teams pipeline store temporary file: {error}"
            ))
        })?;
        maybe_fail_teams_store_write_for_test()?;
        file.write_all(&body)?;
        file.sync_all()?;
        drop(file);
        hermes_config::atomic_replace_file(&temporary, path).map_err(|error| {
            TeamsPipelineError::Store(format!(
                "could not atomically replace Teams pipeline store {}: {error}",
                path.display()
            ))
        })?;
        sync_teams_store_parent(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn sync_teams_store_parent(parent: &Path) -> TeamsPipelineResult<()> {
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_teams_store_parent(_parent: &Path) -> TeamsPipelineResult<()> {
    Ok(())
}

fn notification_payload_digest(payload: &Value) -> String {
    let digest = Sha256::digest(canonical_json(payload).as_bytes());
    format!("sha256:{}", hex::encode(digest))
}

fn compact_notification_receipt(
    received_at: String,
    payload_digest: Option<String>,
    job_id: Option<String>,
    job_tombstone: Option<Value>,
) -> Value {
    let mut receipt = Map::new();
    receipt.insert("received_at".into(), Value::String(received_at));
    if let Some(payload_digest) = payload_digest {
        receipt.insert("payload_digest".into(), Value::String(payload_digest));
    }
    if let Some(job_id) = job_id {
        receipt.insert("job_id".into(), Value::String(job_id));
    }
    if let Some(job_tombstone) = job_tombstone {
        receipt.insert("job_tombstone".into(), job_tombstone);
    }
    Value::Object(receipt)
}

fn notification_receipt_job(
    state: &TeamsPipelineState,
    receipt: &Value,
) -> TeamsPipelineResult<TeamsMeetingPipelineJob> {
    let receipt = receipt.as_object().ok_or_else(|| {
        TeamsPipelineError::Store("Teams notification receipt is not an object.".into())
    })?;
    let job_id = receipt.get("job_id").and_then(value_to_string).ok_or_else(|| {
        TeamsPipelineError::Store(
            "Teams notification receipt has no job id; refusing duplicate replay.".into(),
        )
    })?;
    if let Some(job) = state.jobs.get(&job_id) {
        return TeamsMeetingPipelineJob::from_value(job.clone());
    }
    receipt
        .get("job_tombstone")
        .cloned()
        .ok_or_else(|| {
            TeamsPipelineError::Store(format!(
                "Teams notification receipt for {job_id} has no retained idempotency tombstone."
            ))
        })
        .and_then(TeamsMeetingPipelineJob::from_value)
}

fn redact_job_payload_for_store(mut value: Value) -> Value {
    let Some(job) = value.as_object_mut() else {
        return value;
    };
    for summary_key in ["summary_payload", "summaryPayload"] {
        let Some(summary) = job.get_mut(summary_key).and_then(Value::as_object_mut) else {
            continue;
        };
        let transcript = summary
            .remove("transcript_text")
            .or_else(|| summary.remove("transcriptText"))
            .and_then(|value| value.as_str().map(ToOwned::to_owned));
        if let (Some(transcript), Some(summary_text)) = (
            transcript.as_deref(),
            summary.get("summary").and_then(Value::as_str).map(ToOwned::to_owned),
        ) {
            summary.insert(
                "summary".into(),
                Value::String(redact_summary_against_transcript(&summary_text, transcript)),
            );
        }
        for artifacts_key in ["source_artifacts", "sourceArtifacts"] {
            let Some(artifacts) = summary.get_mut(artifacts_key).and_then(Value::as_array_mut)
            else {
                continue;
            };
            for artifact in artifacts {
                let Some(artifact) = artifact.as_object_mut() else {
                    continue;
                };
                let digest = Sha256::digest(canonical_json(&Value::Object(artifact.clone())).as_bytes());
                artifact.retain(|key, _| {
                    matches!(
                        key.as_str(),
                        "artifact_type"
                            | "artifactType"
                            | "artifact_id"
                            | "id"
                            | "content_type"
                            | "contentType"
                            | "created_at"
                            | "createdDateTime"
                            | "available_at"
                            | "availableDateTime"
                            | "size_bytes"
                            | "size"
                    )
                });
                artifact.insert(
                    "artifact_digest".into(),
                    Value::String(format!("sha256:{}", hex::encode(digest))),
                );
            }
        }
    }
    for meeting_key in ["meeting_ref", "meetingRef"] {
        let Some(meeting) = job.get_mut(meeting_key).and_then(Value::as_object_mut) else {
            continue;
        };
        if let Some(metadata) = meeting.get_mut("metadata").and_then(Value::as_object_mut) {
            metadata.retain(|key, _| {
                matches!(
                    key.as_str(),
                    "call_record_id" | "notification_artifact_id" | "notification_artifact_type"
                )
            });
        }
    }
    value
}

fn redact_summary_against_transcript(summary: &str, transcript: &str) -> String {
    let mut redacted = summary.to_string();
    if !transcript.trim().is_empty() {
        for line in transcript.lines().map(str::trim).filter(|line| line.len() >= 8) {
            redacted = redacted.replace(line, "[redacted]");
        }
        if redacted.trim() == transcript.trim() {
            redacted = "[redacted]".into();
        }
    }
    redacted.chars().take(12_000).collect()
}

fn compact_job_tombstone(value: &Value) -> Value {
    let value = redact_job_payload_for_store(value.clone());
    let Some(job) = value.as_object() else {
        return value;
    };
    let mut compact = Map::new();
    for key in [
        "job_id",
        "event_id",
        "source_event_type",
        "dedupe_key",
        "status",
        "retry_count",
        "created_at",
        "updated_at",
        "meeting_ref",
        "selected_artifact_strategy",
        "summary_payload",
        "error_info",
    ] {
        if let Some(value) = job.get(key) {
            compact.insert(key.into(), value.clone());
        }
    }
    Value::Object(compact)
}

fn normalize_teams_pipeline_state(
    state: &mut TeamsPipelineState,
    limits: &TeamsPipelineStoreLimits,
) -> TeamsPipelineResult<bool> {
    let mut changed = false;
    for job in state.jobs.values_mut() {
        let redacted = redact_job_payload_for_store(job.clone());
        if *job != redacted {
            *job = redacted;
            changed = true;
        }
    }
    for receipt in state.notification_receipts.values_mut() {
        let compact = normalize_notification_receipt(receipt.clone());
        if *receipt != compact {
            *receipt = compact;
            changed = true;
        }
    }

    let now = Utc::now();
    let terminal_cutoff = now
        .checked_sub_signed(ChronoDuration::from_std(limits.terminal_job_ttl).map_err(|error| {
            TeamsPipelineError::Config(format!("invalid Teams terminal job TTL: {error}"))
        })?)
        .ok_or_else(|| TeamsPipelineError::Config("Teams terminal job TTL underflowed.".into()))?;
    let expired_jobs = state
        .jobs
        .iter()
        .filter_map(|(job_id, job)| {
            if job_is_retention_protected(job) {
                return None;
            }
            let timestamp = job
                .get("updated_at")
                .or_else(|| job.get("updatedAt"))
                .and_then(value_to_string)
                .and_then(|value| parse_datetime_utc(&value))?;
            (timestamp <= terminal_cutoff).then(|| job_id.clone())
        })
        .collect::<Vec<_>>();
    for job_id in expired_jobs {
        if let Some(job) = state.jobs.remove(&job_id) {
            retain_job_tombstone(state, &job);
            changed = true;
        }
    }

    let tombstone_cutoff = now
        .checked_sub_signed(
            ChronoDuration::from_std(limits.notification_tombstone_ttl).map_err(|error| {
                TeamsPipelineError::Config(format!("invalid Teams notification tombstone TTL: {error}"))
            })?,
        )
        .ok_or_else(|| {
            TeamsPipelineError::Config("Teams notification tombstone TTL underflowed.".into())
        })?;
    let expired_receipts = state
        .notification_receipts
        .iter()
        .filter_map(|(receipt_key, receipt)| {
            let timestamp = receipt
                .get("received_at")
                .and_then(value_to_string)
                .and_then(|value| parse_datetime_utc(&value))?;
            (timestamp <= tombstone_cutoff).then(|| receipt_key.clone())
        })
        .collect::<Vec<_>>();
    for receipt_key in expired_receipts {
        state.notification_receipts.remove(&receipt_key);
        changed = true;
    }

    if state.jobs.len() > limits.max_jobs {
        return Err(TeamsPipelineError::Store(format!(
            "Teams pipeline store has {} jobs, above the configured {} job limit; active or unexpired jobs were retained.",
            state.jobs.len(),
            limits.max_jobs
        )));
    }
    if state.notification_receipts.len() > limits.max_notification_tombstones {
        return Err(TeamsPipelineError::Store(format!(
            "Teams pipeline store has {} notification tombstones, above the configured {} redelivery-horizon limit.",
            state.notification_receipts.len(),
            limits.max_notification_tombstones
        )));
    }
    let serialized = serde_json::to_vec_pretty(state)?;
    if serialized.len() > limits.max_serialized_bytes {
        return Err(TeamsPipelineError::Store(format!(
            "Teams pipeline store would serialize to {} bytes, above the configured {} byte limit.",
            serialized.len(),
            limits.max_serialized_bytes
        )));
    }
    Ok(changed)
}

fn normalize_notification_receipt(receipt: Value) -> Value {
    let Some(receipt_object) = receipt.as_object() else {
        return receipt;
    };
    let payload_digest = receipt_object
        .get("payload_digest")
        .and_then(value_to_string)
        .or_else(|| receipt_object.get("payload").map(notification_payload_digest));
    compact_notification_receipt(
        receipt_object
            .get("received_at")
            .and_then(value_to_string)
            .unwrap_or_else(utc_now_iso),
        payload_digest,
        receipt_object.get("job_id").and_then(value_to_string),
        receipt_object.get("job_tombstone").cloned(),
    )
}

fn retain_job_tombstone(state: &mut TeamsPipelineState, job: &Value) {
    let Some(job_object) = job.as_object() else {
        return;
    };
    let Some(job_id) = job_object.get("job_id").and_then(value_to_string) else {
        return;
    };
    let receipt_key = state
        .notification_receipts
        .iter()
        .find_map(|(key, receipt)| {
            (receipt.get("job_id").and_then(value_to_string).as_deref() == Some(job_id.as_str()))
                .then(|| key.clone())
        })
        .unwrap_or_else(|| {
            job_object
                .get("event_id")
                .and_then(value_to_string)
                .unwrap_or_else(|| job_id.clone())
        });
    let received_at = state
        .notification_receipts
        .get(&receipt_key)
        .and_then(|receipt| receipt.get("received_at"))
        .and_then(value_to_string)
        .unwrap_or_else(utc_now_iso);
    let payload_digest = state
        .notification_receipts
        .get(&receipt_key)
        .and_then(|receipt| receipt.get("payload_digest"))
        .and_then(value_to_string);
    state.notification_receipts.insert(
        receipt_key,
        compact_notification_receipt(
            received_at,
            payload_digest,
            Some(job_id),
            Some(compact_job_tombstone(job)),
        ),
    );
}

fn job_is_retention_protected(job: &Value) -> bool {
    let status = job
        .get("status")
        .and_then(value_to_string)
        .unwrap_or_default();
    ACTIVE_PIPELINE_STATES.iter().any(|active| *active == status)
        || matches!(status.as_str(), "retry_scheduled" | "retry_required")
}

#[cfg(test)]
static TEAMS_STORE_FAIL_NEXT_WRITE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
fn fail_next_teams_store_write_for_test() {
    TEAMS_STORE_FAIL_NEXT_WRITE.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
fn maybe_fail_teams_store_write_for_test() -> TeamsPipelineResult<()> {
    if TEAMS_STORE_FAIL_NEXT_WRITE.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Err(TeamsPipelineError::Store(
            "injected Teams pipeline store write failure".into(),
        ));
    }
    Ok(())
}

#[cfg(not(test))]
fn maybe_fail_teams_store_write_for_test() -> TeamsPipelineResult<()> {
    Ok(())
}

