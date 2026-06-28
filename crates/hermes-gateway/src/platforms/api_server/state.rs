#[derive(Debug, Clone)]
pub struct ApiInboundRequest {
    pub request_id: String,
    pub session_id: String,
    pub user_id: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub personality: Option<String>,
    pub prompt: String,
}

#[derive(Clone)]
struct ApiServerRuntime {
    mailbox: Arc<RwLock<ResponseMailbox>>,
    response_store: Arc<RwLock<ResponseStore>>,
    run_cancels: Arc<RwLock<RunCancelRegistry>>,
    run_store: Arc<RwLock<RunStore>>,
    cron_scheduler: Arc<CronScheduler>,
    inbound_tx: Arc<RwLock<Option<mpsc::Sender<ApiInboundRequest>>>>,
    auth_token: Option<String>,
}

// ---------------------------------------------------------------------------
// Pending response mailbox
// ---------------------------------------------------------------------------

/// Holds pending responses that will be sent back to HTTP callers.
#[derive(Default)]
struct ResponseMailbox {
    pending: HashMap<String, mpsc::Sender<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredApiResponse {
    response: serde_json::Value,
    conversation_history: Vec<ChatMessage>,
}

#[derive(Debug)]
struct ResponseStore {
    max_size: usize,
    entries: HashMap<String, StoredApiResponse>,
    lru: VecDeque<String>,
    conversation_to_response: HashMap<String, String>,
}

impl ResponseStore {
    fn new(max_size: usize) -> Self {
        Self {
            max_size,
            entries: HashMap::new(),
            lru: VecDeque::new(),
            conversation_to_response: HashMap::new(),
        }
    }

    fn put(&mut self, id: impl Into<String>, response: StoredApiResponse) {
        let id = id.into();
        self.entries.insert(id.clone(), response);
        self.touch(&id);
        self.evict_if_needed();
    }

    fn get(&mut self, id: &str) -> Option<StoredApiResponse> {
        if self.entries.contains_key(id) {
            self.touch(id);
        }
        self.entries.get(id).cloned()
    }

    fn delete(&mut self, id: &str) -> bool {
        let existed = self.entries.remove(id).is_some();
        if existed {
            self.lru.retain(|entry| entry != id);
            self.conversation_to_response
                .retain(|_, response_id| response_id != id);
        }
        existed
    }

    fn set_conversation(
        &mut self,
        conversation: impl Into<String>,
        response_id: impl Into<String>,
    ) {
        self.conversation_to_response
            .insert(conversation.into(), response_id.into());
    }

    fn get_conversation(&mut self, conversation: &str) -> Option<String> {
        let response_id = self.conversation_to_response.get(conversation)?.clone();
        if self.entries.contains_key(&response_id) {
            self.touch(&response_id);
            Some(response_id)
        } else {
            self.conversation_to_response.remove(conversation);
            None
        }
    }

    fn touch(&mut self, id: &str) {
        self.lru.retain(|entry| entry != id);
        self.lru.push_back(id.to_string());
    }

    fn evict_if_needed(&mut self) {
        while self.entries.len() > self.max_size {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
            self.conversation_to_response
                .retain(|_, response_id| response_id != &oldest);
        }
    }
}

impl Default for ResponseStore {
    fn default() -> Self {
        Self::new(1024)
    }
}

#[derive(Default)]
struct RunCancelRegistry {
    pending: HashMap<String, Arc<Notify>>,
}

#[derive(Debug, Clone)]
struct RunRecord {
    run_id: String,
    session_id: String,
    user_id: String,
    model: Option<String>,
    provider: Option<String>,
    personality: Option<String>,
    status: String,
    output: Option<String>,
    usage: UsageInfo,
    last_event: Option<String>,
    events: Vec<serde_json::Value>,
    created_at: i64,
    completed_at: Option<i64>,
}

impl RunRecord {
    fn new(
        run_id: String,
        session_id: String,
        user_id: String,
        model: Option<String>,
        provider: Option<String>,
        personality: Option<String>,
    ) -> Self {
        let mut record = Self {
            run_id,
            session_id,
            user_id,
            model,
            provider,
            personality,
            status: "queued".to_string(),
            output: None,
            usage: UsageInfo {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            },
            last_event: None,
            events: Vec::new(),
            created_at: chrono::Utc::now().timestamp(),
            completed_at: None,
        };
        record.push_event("run.queued", None);
        record
    }

    fn is_active(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "running" | "stopping")
    }

    fn is_terminal(&self) -> bool {
        matches!(self.status.as_str(), "completed" | "cancelled" | "failed")
    }

    fn push_event(&mut self, event_type: &str, extra: Option<serde_json::Value>) {
        self.last_event = Some(event_type.to_string());
        let mut event = serde_json::json!({
            "type": event_type,
            "run_id": self.run_id,
            "status": self.status,
            "created_at": chrono::Utc::now().timestamp(),
        });
        if let Some(extra) = extra {
            if let (Some(target), Some(source)) = (event.as_object_mut(), extra.as_object()) {
                for (key, value) in source {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
        self.events.push(event);
    }
}

#[derive(Default)]
struct RunStore {
    records: HashMap<String, RunRecord>,
    notifiers: HashMap<String, Arc<Notify>>,
}

impl RunStore {
    fn insert(&mut self, record: RunRecord) {
        let run_id = record.run_id.clone();
        self.notifiers
            .insert(run_id.clone(), Arc::new(Notify::new()));
        self.records.insert(run_id, record);
    }

    fn get(&self, run_id: &str) -> Option<RunRecord> {
        self.records.get(run_id).cloned()
    }

    fn notifier(&self, run_id: &str) -> Option<Arc<Notify>> {
        self.notifiers.get(run_id).cloned()
    }

    fn update<F>(&mut self, run_id: &str, f: F) -> Option<RunRecord>
    where
        F: FnOnce(&mut RunRecord),
    {
        let record = self.records.get_mut(run_id)?;
        f(record);
        if let Some(notifier) = self.notifiers.get(run_id) {
            notifier.notify_waiters();
        }
        Some(record.clone())
    }
}
