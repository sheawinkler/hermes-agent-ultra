
// ---------------------------------------------------------------------------
// Event
// ---------------------------------------------------------------------------

/// Events that the TUI can process.
#[derive(Debug, Clone)]
pub enum Event {
    /// A keyboard key was pressed.
    Key(KeyEvent),
    /// The terminal was resized.
    Resize(u16, u16),
    /// An asynchronous message (e.g. from agent streaming).
    Message(String),
    /// Agent produced a streaming delta.
    StreamDelta(String),
    /// Agent produced a full stream chunk (including control metadata).
    StreamChunk(StreamChunk),
    /// Agent finished processing.
    AgentDone,
    /// Background agent run completed.
    AgentRunComplete {
        result: Result<AgentResult, String>,
        elapsed_secs: f64,
    },
    /// Background app-owned run completed. Used for slash commands and
    /// managed quorum/swarm turns that must mutate App state while keeping the
    /// render loop responsive.
    ManagedAppRunComplete {
        result: Result<Box<App>, String>,
        elapsed_secs: f64,
    },
    /// Interrupt signal (Ctrl+C).
    Interrupt,
    /// External shutdown signal (SIGINT/SIGTERM/SIGHUP).
    Shutdown,
    /// Mouse interaction.
    Mouse(MouseEvent),
    /// Terminal bracketed paste payload.
    Paste(String),
}

// ---------------------------------------------------------------------------
// InputMode
// ---------------------------------------------------------------------------

/// Current input mode for the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// Normal mode: keys are interpreted as commands.
    Normal,
    /// Insert mode: keys are inserted into the input buffer.
    Insert,
    /// Command mode: entering a slash command with auto-completion.
    Command,
}

impl std::fmt::Display for InputMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InputMode::Normal => write!(f, "NORMAL"),
            InputMode::Insert => write!(f, "INSERT"),
            InputMode::Command => write!(f, "COMMAND"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewDensity {
    Compact,
    Detailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityLaneMode {
    Live,
    Cockpit,
}

#[derive(Debug, Clone)]
enum PickerKind {
    ModelProvider,
    ModelForProvider { provider: String },
    Personality,
    Skin,
    InteractiveQuestion { prompt: String },
}

#[derive(Debug, Clone)]
struct PickerItem {
    label: String,
    detail: String,
    value: String,
}

#[derive(Debug, Clone)]
struct PickerModal {
    kind: PickerKind,
    title: String,
    query: String,
    items: Vec<PickerItem>,
    filtered_indices: Vec<usize>,
    selected_filtered: usize,
    page_size: usize,
    allow_multi: bool,
    selected_values: HashSet<String>,
}

impl PickerModal {
    fn new(kind: PickerKind, title: impl Into<String>, items: Vec<PickerItem>) -> Self {
        let mut this = Self {
            kind,
            title: title.into(),
            query: String::new(),
            items,
            filtered_indices: Vec::new(),
            selected_filtered: 0,
            page_size: 10,
            allow_multi: false,
            selected_values: HashSet::new(),
        };
        this.refresh_filter();
        this
    }

    fn refresh_filter(&mut self) {
        let needle = self.query.trim().to_ascii_lowercase();
        if needle.is_empty() {
            self.filtered_indices = (0..self.items.len()).collect();
        } else {
            let mut ranked: Vec<(usize, i32)> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(idx, item)| {
                    let label = item.label.to_ascii_lowercase();
                    let detail = item.detail.to_ascii_lowercase();
                    if label == needle {
                        return Some((idx, 1200));
                    }
                    if label.starts_with(&needle) {
                        return Some((
                            idx,
                            1000 - (label.len().saturating_sub(needle.len()) as i32),
                        ));
                    }
                    if label.contains(&needle) {
                        return Some((idx, 850));
                    }
                    if detail.contains(&needle) {
                        return Some((idx, 700));
                    }
                    let subseq = fuzzy_subsequence_score(&needle, &label);
                    if subseq > 0 {
                        return Some((idx, 500 + subseq));
                    }
                    None
                })
                .collect();
            ranked.sort_by(|(a_idx, a_score), (b_idx, b_score)| {
                b_score
                    .cmp(a_score)
                    .then_with(|| self.items[*a_idx].label.cmp(&self.items[*b_idx].label))
            });
            self.filtered_indices = ranked.into_iter().map(|(idx, _)| idx).collect();
        }
        if self.filtered_indices.is_empty() {
            self.selected_filtered = 0;
        } else if self.selected_filtered >= self.filtered_indices.len() {
            self.selected_filtered = self.filtered_indices.len() - 1;
        }
    }

    fn selected_item(&self) -> Option<&PickerItem> {
        let idx = self.filtered_indices.get(self.selected_filtered).copied()?;
        self.items.get(idx)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.filtered_indices.is_empty() {
            self.selected_filtered = 0;
            return;
        }
        let len = self.filtered_indices.len() as isize;
        let mut next = self.selected_filtered as isize + delta;
        while next < 0 {
            next += len;
        }
        next %= len;
        self.selected_filtered = next as usize;
    }

    fn page_move(&mut self, pages: isize) {
        let step = self.page_size.max(1) as isize;
        self.move_selection(pages * step);
    }

    fn visible_window(&self) -> (usize, usize) {
        if self.filtered_indices.is_empty() {
            return (0, 0);
        }
        let rows = self.page_size.max(1);
        let mut start = 0usize;
        if self.selected_filtered >= rows {
            start = self.selected_filtered + 1 - rows;
        }
        let end = (start + rows).min(self.filtered_indices.len());
        (start, end)
    }

    fn toggle_selected(&mut self) {
        if !self.allow_multi {
            return;
        }
        if let Some(value) = self.selected_item().map(|item| item.value.clone()) {
            if !self.selected_values.insert(value.clone()) {
                self.selected_values.remove(&value);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct TranscriptCache {
    fingerprint: u64,
    width: u16,
    lines: Vec<Line<'static>>,
    visual_rows: usize,
    total_messages: usize,
    rendered_messages: usize,
    message_fingerprints: Vec<u64>,
    show_timestamps: bool,
    view_density: ViewDensity,
    had_streaming: bool,
}

impl Default for TranscriptCache {
    fn default() -> Self {
        Self {
            fingerprint: 0,
            width: 0,
            lines: Vec::new(),
            visual_rows: 1,
            total_messages: 0,
            rendered_messages: 0,
            message_fingerprints: Vec::new(),
            show_timestamps: false,
            view_density: ViewDensity::Detailed,
            had_streaming: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModalAction {
    None,
    Close,
    Confirm,
    DisconnectProvider,
}

// ---------------------------------------------------------------------------
// Tui
// ---------------------------------------------------------------------------

/// The terminal UI wrapper.
///
/// Owns the ratatui Terminal and provides methods for rendering,
/// event handling, and theme management.
pub struct Tui {
    /// The ratatui terminal backend.
    pub terminal: ratatui::Terminal<CrosstermBackend<Stdout>>,
    /// Channel receiver for control/UI events (keys, mouse, resize, app control).
    pub events: mpsc::UnboundedReceiver<Event>,
    /// Channel receiver for high-volume stream events (tokens/chunks).
    pub stream_events: mpsc::UnboundedReceiver<Event>,
    /// Channel sender for control/UI events.
    event_sender: mpsc::UnboundedSender<Event>,
    /// Channel sender for stream events.
    stream_sender: mpsc::UnboundedSender<Event>,
    /// The active color theme.
    theme: Theme,
    /// Whether terminal cleanup has already run.
    restored: bool,
    /// Whether mouse capture is currently enabled on the terminal backend.
    mouse_capture_enabled: bool,
}

impl Tui {
    /// Create a new Tui instance, initializing the terminal.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        stdout.execute(EnableBracketedPaste)?;
        stdout.execute(EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = ratatui::Terminal::new(backend)?;
        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        let (stream_sender, stream_receiver) = mpsc::unbounded_channel();
        let requested_theme =
            std::env::var("HERMES_THEME").unwrap_or_else(|_| "ultra-sunburst".to_string());
        Ok(Self {
            terminal,
            events: event_receiver,
            stream_events: stream_receiver,
            event_sender,
            stream_sender,
            theme: crate::skin_engine::resolve_theme(requested_theme.as_str()),
            restored: false,
            mouse_capture_enabled: false,
        })
    }

    pub fn set_mouse_capture(&mut self, enabled: bool) -> Result<(), Box<dyn std::error::Error>> {
        if self.mouse_capture_enabled == enabled {
            return Ok(());
        }
        if enabled {
            self.terminal.backend_mut().execute(EnableMouseCapture)?;
        } else {
            self.terminal.backend_mut().execute(DisableMouseCapture)?;
        }
        self.mouse_capture_enabled = enabled;
        Ok(())
    }

    /// Restore the terminal to its original state.
    pub fn restore(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.restored {
            return Ok(());
        }
        disable_raw_mode()?;
        if self.mouse_capture_enabled {
            self.terminal.backend_mut().execute(DisableMouseCapture)?;
            self.mouse_capture_enabled = false;
        }
        self.terminal.backend_mut().execute(DisableBracketedPaste)?;
        self.terminal.backend_mut().execute(LeaveAlternateScreen)?;
        self.terminal.show_cursor()?;
        let mut stdout = std::io::stdout();
        let _ = stdout.execute(ResetColor);
        let _ = stdout.flush();
        self.restored = true;
        Ok(())
    }

    /// Get a sender for injecting events (used by async tasks).
    pub fn event_sender(&self) -> mpsc::UnboundedSender<Event> {
        self.event_sender.clone()
    }

    /// Get a sender for injecting high-volume stream events.
    pub fn stream_sender(&self) -> mpsc::UnboundedSender<Event> {
        self.stream_sender.clone()
    }

    /// Set the active theme.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    /// Get a reference to the current theme.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        if self.restored {
            return;
        }
        let _ = disable_raw_mode();
        let mut stdout = std::io::stdout();
        let _ = stdout.execute(DisableBracketedPaste);
        let _ = stdout.execute(DisableMouseCapture);
        let _ = stdout.execute(LeaveAlternateScreen);
        let _ = stdout.execute(ResetColor);
        let _ = stdout.execute(Show);
        let _ = stdout.flush();
        self.restored = true;
    }
}

async fn abort_and_join_task(task: &mut Option<JoinHandle<()>>) {
    if let Some(handle) = task.take() {
        handle.abort();
        let _ = handle.await;
    }
}
