// ---------------------------------------------------------------------------
// Discord thread participation persistence
// ---------------------------------------------------------------------------

/// Persistent ordered set of Discord threads the bot has participated in.
#[derive(Debug, Clone)]
pub struct DiscordThreadParticipationTracker {
    path: PathBuf,
    threads: VecDeque<String>,
    max_tracked: usize,
    #[cfg(test)]
    fail_next_save: Arc<AtomicBool>,
    #[cfg(test)]
    fail_next_parent_sync: Arc<AtomicBool>,
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
            #[cfg(test)]
            fail_next_save: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            fail_next_parent_sync: Arc::new(AtomicBool::new(false)),
        };
        tracker.load();
        tracker
    }

    pub fn set_max_tracked(&mut self, max_tracked: usize) {
        self.max_tracked = max_tracked.max(1);
        self.enforce_capacity();
    }


    /// Build a durable-write candidate without holding the caller's state
    /// lock during filesystem I/O. A serialized owner commits this candidate
    /// only after `save_staged` succeeds.
    pub(crate) fn stage_mark(&mut self, thread_id: impl Into<String>) -> Option<Self> {
        let thread_id = thread_id.into();
        let thread_id = thread_id.trim();
        if thread_id.is_empty() || self.contains(thread_id) {
            return None;
        }

        let mut staged = self.clone();
        staged.threads.push_back(thread_id.to_string());
        staged.enforce_capacity();
        Some(staged)
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

        let previous = self.threads.clone();
        self.threads.push_back(thread_id.to_string());
        self.enforce_capacity();
        if let Err(error) = self.save() {
            self.threads = previous;
            return Err(error);
        }
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

    #[cfg(test)]
    fn fail_next_save_for_test(&mut self) {
        self.fail_next_save.store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn fail_next_parent_sync_for_test(&mut self) {
        self.fail_next_parent_sync.store(true, Ordering::SeqCst);
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

    pub(crate) fn save_staged(&mut self) -> std::io::Result<()> {
        self.save()
    }

    fn save(&mut self) -> std::io::Result<()> {
        #[cfg(test)]
        if self.fail_next_save.swap(false, Ordering::SeqCst) {
            return Err(std::io::Error::other("injected tracker save failure"));
        }
        #[cfg(test)]
        let fail_parent_sync = self.fail_next_parent_sync.swap(false, Ordering::SeqCst);
        #[cfg(not(test))]
        let fail_parent_sync = false;
        let values: Vec<&str> = self.threads.iter().map(String::as_str).collect();
        let body = serde_json::to_string(&values).expect("thread id list serializes");
        write_discord_tracker_state_atomically(&self.path, &body, fail_parent_sync)
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
    #[cfg(test)]
    fail_next_save: Arc<AtomicBool>,
    #[cfg(test)]
    fail_next_parent_sync: Arc<AtomicBool>,
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
            #[cfg(test)]
            fail_next_save: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            fail_next_parent_sync: Arc::new(AtomicBool::new(false)),
        };
        tracker.load();
        tracker
    }

    pub fn contains(&self, message_id: &str) -> bool {
        let message_id = message_id.trim();
        !message_id.is_empty() && self.ids.iter().any(|existing| existing == message_id)
    }


    /// Build a durable-write candidate without holding the caller's state
    /// lock during filesystem I/O. A serialized owner commits this candidate
    /// only after `save_staged` succeeds.
    pub(crate) fn stage_mark_many(
        &mut self,
        message_ids: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Option<Self> {
        let mut staged = self.clone();
        let mut changed = false;
        for message_id in message_ids {
            let message_id = message_id.as_ref().trim();
            if !message_id.is_empty() && !staged.contains(message_id) {
                staged.ids.push_back(message_id.to_string());
                changed = true;
            }
        }
        if !changed {
            return None;
        }
        staged.enforce_capacity();
        Some(staged)
    }
    pub fn mark_many(
        &mut self,
        message_ids: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> std::io::Result<bool> {
        let previous = self.ids.clone();
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
            if let Err(error) = self.save() {
                self.ids = previous;
                return Err(error);
            }
        }
        Ok(changed)
    }

    pub fn entries(&self) -> Vec<String> {
        self.ids.iter().cloned().collect()
    }

    #[cfg(test)]
    fn fail_next_save_for_test(&mut self) {
        self.fail_next_save.store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn fail_next_parent_sync_for_test(&mut self) {
        self.fail_next_parent_sync.store(true, Ordering::SeqCst);
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


    pub(crate) fn save_staged(&mut self) -> std::io::Result<()> {
        self.save()
    }
    fn save(&mut self) -> std::io::Result<()> {
        #[cfg(test)]
        if self.fail_next_save.swap(false, Ordering::SeqCst) {
            return Err(std::io::Error::other("injected tracker save failure"));
        }
        #[cfg(test)]
        let fail_parent_sync = self.fail_next_parent_sync.swap(false, Ordering::SeqCst);
        #[cfg(not(test))]
        let fail_parent_sync = false;
        let values: Vec<&str> = self.ids.iter().map(String::as_str).collect();
        let body = serde_json::to_string(&values).expect("discord status id list serializes");
        write_discord_tracker_state_atomically(&self.path, &body, fail_parent_sync)
    }
}


/// Open a sibling tracker temporary file without allowing the process umask to
/// turn durable Discord identifiers into group/world-readable state.
fn open_private_discord_tracker_temp(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(not(unix))]
fn preserve_existing_tracker_permissions(
    temporary: &std::fs::File,
    destination: &Path,
) -> std::io::Result<()> {
    // `ReplaceFileW` preserves the existing destination's security descriptor.
    // For platforms using the fallback replace path, retain its permissions
    // explicitly; a new file inherits the selected state directory's ACL.
    if let Ok(metadata) = std::fs::metadata(destination) {
        temporary.set_permissions(metadata.permissions())?;
    }
    Ok(())
}

#[cfg(unix)]
fn preserve_existing_tracker_permissions(
    _temporary: &std::fs::File,
    _destination: &Path,
) -> std::io::Result<()> {
    Ok(())
}

/// Persist the directory entry created by the atomic replacement. File fsync
/// alone does not make a rename survive power loss.
fn sync_discord_tracker_parent(
    directory: &Path,
    #[allow(unused_variables)] fail_parent_sync: bool,
) -> std::io::Result<()> {
    #[cfg(test)]
    if fail_parent_sync {
        return Err(std::io::Error::other(
            "injected Discord tracker parent-directory sync failure",
        ));
    }

    #[cfg(unix)]
    {
        std::fs::File::open(directory)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        // Windows `ReplaceFileW`/`MoveFileExW` uses write-through replacement;
        // preserve that platform's durable replacement contract.
        let _ = directory;
        Ok(())
    }
}

fn write_discord_tracker_state_atomically(
    path: &Path,
    body: &str,
    fail_parent_sync: bool,
) -> std::io::Result<()> {
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        std::fs::create_dir_all(parent)?;
    }
    let directory = parent.unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("discord-tracker");
    let temporary = directory.join(format!(
        ".{file_name}.{}.tmp",
        uuid::Uuid::new_v4()
    ));

    let write_result = (|| {
        let mut file = open_private_discord_tracker_temp(&temporary)?;
        use std::io::Write as _;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        preserve_existing_tracker_permissions(&file, path)?;
        drop(file);
        hermes_config::atomic_replace_file(&temporary, path)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        sync_discord_tracker_parent(directory, fail_parent_sync)
    })();

    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result
}
