// ---------------------------------------------------------------------------
// Discord thread participation persistence
// ---------------------------------------------------------------------------

/// Persistent ordered set of Discord threads the bot has participated in.
#[derive(Debug, Clone)]
pub struct DiscordThreadParticipationTracker {
    path: PathBuf,
    threads: VecDeque<String>,
    max_tracked: usize,
}

impl DiscordThreadParticipationTracker {
    pub const DEFAULT_MAX_TRACKED: usize = 2048;

    pub fn new(platform: &str) -> Self {
        let filename = format!("{}_threads.json", platform.trim());
        Self::from_path(
            hermes_config::hermes_home().join(filename),
            Self::DEFAULT_MAX_TRACKED,
        )
    }

    pub fn from_path(path: impl Into<PathBuf>, max_tracked: usize) -> Self {
        let path = path.into();
        let mut tracker = Self {
            path,
            threads: VecDeque::new(),
            max_tracked: max_tracked.max(1),
        };
        tracker.load();
        tracker
    }

    pub fn set_max_tracked(&mut self, max_tracked: usize) {
        self.max_tracked = max_tracked.max(1);
        self.enforce_capacity();
    }

    pub fn contains(&self, thread_id: &str) -> bool {
        let thread_id = thread_id.trim();
        !thread_id.is_empty() && self.threads.iter().any(|existing| existing == thread_id)
    }

    pub fn mark(&mut self, thread_id: impl Into<String>) -> std::io::Result<bool> {
        let thread_id = thread_id.into();
        let thread_id = thread_id.trim();
        if thread_id.is_empty() || self.contains(thread_id) {
            return Ok(false);
        }

        self.threads.push_back(thread_id.to_string());
        self.enforce_capacity();
        self.save()?;
        Ok(true)
    }

    pub fn len(&self) -> usize {
        self.threads.len()
    }

    pub fn is_empty(&self) -> bool {
        self.threads.is_empty()
    }

    pub fn entries(&self) -> Vec<String> {
        self.threads.iter().cloned().collect()
    }

    fn load(&mut self) {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return;
        };
        let Ok(values) = serde_json::from_str::<Vec<String>>(&raw) else {
            return;
        };

        let mut seen = BTreeSet::new();
        for value in values {
            let trimmed = value.trim();
            if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
                self.threads.push_back(trimmed.to_string());
            }
        }
        self.enforce_capacity();
    }

    fn enforce_capacity(&mut self) {
        while self.threads.len() > self.max_tracked {
            self.threads.pop_front();
        }
    }

    fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let values: Vec<&str> = self.threads.iter().map(String::as_str).collect();
        let body = serde_json::to_string(&values).expect("thread id list serializes");
        std::fs::write(&self.path, body)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub type ThreadParticipationTracker = DiscordThreadParticipationTracker;

// ---------------------------------------------------------------------------
// Discord non-conversational message persistence
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DiscordNonConversationalMessageTracker {
    path: PathBuf,
    ids: VecDeque<String>,
    max_tracked: usize,
}

impl DiscordNonConversationalMessageTracker {
    pub const DEFAULT_MAX_TRACKED: usize = 2000;

    pub fn new(platform: &str) -> Self {
        let platform = platform.trim();
        let filename = if platform.is_empty() || platform.eq_ignore_ascii_case("discord") {
            DISCORD_NONCONVERSATIONAL_STATE_FILENAME.to_string()
        } else {
            format!("{}_{}", platform, DISCORD_NONCONVERSATIONAL_STATE_FILENAME)
        };
        Self::from_path(
            hermes_config::hermes_home().join("gateway").join(filename),
            Self::DEFAULT_MAX_TRACKED,
        )
    }

    pub fn from_path(path: impl Into<PathBuf>, max_tracked: usize) -> Self {
        let path = path.into();
        let mut tracker = Self {
            path,
            ids: VecDeque::new(),
            max_tracked: max_tracked.max(1),
        };
        tracker.load();
        tracker
    }

    pub fn contains(&self, message_id: &str) -> bool {
        let message_id = message_id.trim();
        !message_id.is_empty() && self.ids.iter().any(|existing| existing == message_id)
    }

    pub fn mark_many(
        &mut self,
        message_ids: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> std::io::Result<bool> {
        let mut changed = false;
        for message_id in message_ids {
            let message_id = message_id.as_ref().trim();
            if !message_id.is_empty() && !self.contains(message_id) {
                self.ids.push_back(message_id.to_string());
                changed = true;
            }
        }
        if changed {
            self.enforce_capacity();
            self.save()?;
        }
        Ok(changed)
    }

    pub fn entries(&self) -> Vec<String> {
        self.ids.iter().cloned().collect()
    }

    fn load(&mut self) {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return;
        };
        let Ok(values) = serde_json::from_str::<Vec<String>>(&raw) else {
            return;
        };
        let mut seen = BTreeSet::new();
        for value in values {
            let trimmed = value.trim();
            if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
                self.ids.push_back(trimmed.to_string());
            }
        }
        self.enforce_capacity();
    }

    fn enforce_capacity(&mut self) {
        while self.ids.len() > self.max_tracked {
            self.ids.pop_front();
        }
    }

    fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let values: Vec<&str> = self.ids.iter().map(String::as_str).collect();
        let body = serde_json::to_string(&values).expect("discord status id list serializes");
        std::fs::write(&self.path, body)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

