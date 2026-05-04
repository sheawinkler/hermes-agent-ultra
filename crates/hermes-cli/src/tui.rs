//! Terminal UI using ratatui + crossterm (Requirement 9.1-9.6).
//!
//! Implements the interactive terminal interface with:
//! - Message history rendering (9.1, 9.4)
//! - Input area with slash command auto-completion (9.2)
//! - Ctrl+C immediate exit back to parent terminal (with interrupt signal) (9.3)
//! - Streaming output display (9.5)
//! - Status bar with model/session info (9.6)
//! - Theme/skin engine support (9.8)

use std::collections::{HashMap, HashSet};
use std::io::Stdout;
use std::time::{Duration, Instant};

use chrono::Local;
use crossterm::cursor::Show;
use crossterm::event::{Event as CrosstermEvent, KeyEvent, MouseEvent};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::Frame;
use tokio::sync::mpsc;
use tui_textarea::{CursorMove, TextArea};

use hermes_auth::FileTokenStore;
use hermes_core::{AgentError, StreamChunk};

use crate::app::App;
use crate::commands;
use crate::theme::Theme;
use crate::tool_preview::{build_tool_preview_from_value, tool_emoji};

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
    /// Interrupt signal (Ctrl+C).
    Interrupt,
    /// Mouse interaction.
    Mouse(MouseEvent),
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

#[derive(Debug, Clone)]
enum PickerKind {
    ModelProvider,
    ModelForProvider { provider: String },
    Personality,
    Skin,
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
    /// Channel receiver for async events.
    pub events: mpsc::UnboundedReceiver<Event>,
    /// Channel sender for async events.
    event_sender: mpsc::UnboundedSender<Event>,
    /// The active color theme.
    theme: Theme,
    /// Whether terminal cleanup has already run.
    restored: bool,
}

impl Tui {
    /// Create a new Tui instance, initializing the terminal.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = ratatui::Terminal::new(backend)?;
        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        let requested_theme =
            std::env::var("HERMES_THEME").unwrap_or_else(|_| "ultra-neon".to_string());
        Ok(Self {
            terminal,
            events: event_receiver,
            event_sender,
            theme: crate::skin_engine::resolve_theme(requested_theme.as_str()),
            restored: false,
        })
    }

    /// Restore the terminal to its original state.
    pub fn restore(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.restored {
            return Ok(());
        }
        disable_raw_mode()?;
        self.terminal.backend_mut().execute(LeaveAlternateScreen)?;
        self.terminal.show_cursor()?;
        self.restored = true;
        Ok(())
    }

    /// Get a sender for injecting events (used by async tasks).
    pub fn event_sender(&self) -> mpsc::UnboundedSender<Event> {
        self.event_sender.clone()
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
        let _ = stdout.execute(LeaveAlternateScreen);
        let _ = stdout.execute(Show);
        self.restored = true;
    }
}

// ---------------------------------------------------------------------------
// TuiState — holds the mutable state of the TUI between frames
// ---------------------------------------------------------------------------

/// Mutable state for the TUI rendering loop.
pub struct TuiState {
    /// Current input mode.
    pub mode: InputMode,
    /// Current input buffer (supports multi-line).
    pub input: String,
    /// Cursor position within the input buffer (byte offset).
    pub cursor_position: usize,
    /// Auto-completion suggestions (populated in Command mode).
    pub completions: Vec<String>,
    /// Currently selected completion index (if any).
    pub completion_index: Option<usize>,
    /// Scroll offset from newest transcript content (0 = newest).
    pub scroll_offset: u16,
    /// Whether the agent is currently processing.
    pub processing: bool,
    /// Buffer for streaming agent output.
    pub stream_buffer: String,
    /// Whether post-response deltas are currently muted.
    pub stream_muted: bool,
    /// Whether the next visible token should be prefixed by a paragraph break.
    pub stream_needs_break: bool,
    /// Status message shown in the status bar.
    pub status_message: String,
    /// Selection anchor for text selection (byte offset, None if no selection).
    pub selection_anchor: Option<usize>,
    /// Message history index for browsing previous messages.
    pub message_browse_index: Option<usize>,
    /// Whether we are in history search mode (Ctrl+R).
    pub history_search_active: bool,
    /// Current history search query.
    pub history_search_query: String,
    /// Spinner frame counter for tool execution indicator.
    pub spinner_frame: usize,
    /// Tool output sections with fold state (tool_name, output, is_expanded).
    pub tool_outputs: Vec<ToolOutputSection>,
    /// Recent lifecycle/activity rows (newest at end).
    pub recent_activity: Vec<String>,
    /// Active tool names currently running.
    pub active_tools: Vec<String>,
    /// Live thinking preview accumulated during stream.
    pub live_thinking: String,
    /// Last known token usage (prompt, completion, total).
    pub last_usage: Option<(u64, u64, u64)>,
    /// Sticky prompt hint shown while scrolling history.
    pub sticky_prompt: String,
    /// Number of queued/running background jobs.
    pub background_jobs_running: usize,
    /// Whether the right-side live activity lane is open.
    pub activity_lane_open: bool,
    /// Whether transcript headers show timestamp labels.
    pub show_timestamps: bool,
    /// Transcript density mode.
    pub view_density: ViewDensity,
    /// Active picker modal state.
    modal: Option<PickerModal>,
    /// Cached transcript render to reduce full rebuild churn.
    transcript_cache: TranscriptCache,
    /// Expand state for tool cards by transcript key.
    expanded_tool_cards: HashSet<String>,
    /// Stable timestamp labels keyed by message fingerprint.
    message_time_labels: HashMap<u64, String>,
    /// Animation frame index for companion pet rendering.
    pet_frame: usize,
    /// When the current processing cycle started.
    processing_started_at: Option<Instant>,
    /// Last time we emitted a progress heartbeat row.
    last_progress_pulse_at: Option<Instant>,
    /// Count of streaming chunks seen in current cycle.
    stream_chunk_count: usize,
    /// Count of visible streaming chars seen in current cycle.
    stream_char_count: usize,
    /// Whether first response token has been observed in this cycle.
    saw_first_token: bool,
}

/// A section of tool output that can be folded/expanded.
#[derive(Debug, Clone)]
pub struct ToolOutputSection {
    /// Name of the tool that produced this output.
    pub tool_name: String,
    /// Full output text.
    pub output: String,
    /// Whether the section is expanded (showing full output).
    pub is_expanded: bool,
    /// Number of preview lines to show when collapsed.
    pub preview_lines: usize,
}

impl ToolOutputSection {
    pub fn new(tool_name: String, output: String) -> Self {
        Self {
            tool_name,
            output,
            is_expanded: false,
            preview_lines: 3,
        }
    }

    /// Get the display text (collapsed or expanded).
    pub fn display_text(&self) -> String {
        if self.is_expanded {
            self.output.clone()
        } else {
            let lines: Vec<&str> = self.output.lines().take(self.preview_lines).collect();
            let total_lines = self.output.lines().count();
            let mut text = lines.join("\n");
            if total_lines > self.preview_lines {
                text.push_str(&format!(
                    "\n  ... ({} more lines, press Enter to expand)",
                    total_lines - self.preview_lines
                ));
            }
            text
        }
    }
}

impl Default for TuiState {
    fn default() -> Self {
        Self {
            mode: InputMode::Insert,
            input: String::new(),
            cursor_position: 0,
            completions: Vec::new(),
            completion_index: None,
            scroll_offset: 0,
            processing: false,
            stream_buffer: String::new(),
            stream_muted: false,
            stream_needs_break: false,
            status_message: String::new(),
            selection_anchor: None,
            message_browse_index: None,
            history_search_active: false,
            history_search_query: String::new(),
            spinner_frame: 0,
            tool_outputs: Vec::new(),
            recent_activity: Vec::new(),
            active_tools: Vec::new(),
            live_thinking: String::new(),
            last_usage: None,
            sticky_prompt: String::new(),
            background_jobs_running: 0,
            activity_lane_open: true,
            show_timestamps: false,
            view_density: ViewDensity::Detailed,
            modal: None,
            transcript_cache: TranscriptCache::default(),
            expanded_tool_cards: HashSet::new(),
            message_time_labels: HashMap::new(),
            pet_frame: 0,
            processing_started_at: None,
            last_progress_pulse_at: None,
            stream_chunk_count: 0,
            stream_char_count: 0,
            saw_first_token: false,
        }
    }
}

impl TuiState {
    fn push_activity(&mut self, text: impl Into<String>) {
        let trimmed = text.into().trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        self.recent_activity.push(trimmed);
        const MAX_EVENTS: usize = 16;
        if self.recent_activity.len() > MAX_EVENTS {
            let remove = self.recent_activity.len() - MAX_EVENTS;
            self.recent_activity.drain(0..remove);
        }
    }

    fn append_live_thinking(&mut self, chunk: &str) {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            return;
        }
        if !self.live_thinking.is_empty() {
            self.live_thinking.push(' ');
        }
        self.live_thinking.push_str(chunk);
        const MAX_CHARS: usize = 260;
        if self.live_thinking.chars().count() > MAX_CHARS {
            let tail: String = self
                .live_thinking
                .chars()
                .rev()
                .take(MAX_CHARS.saturating_sub(1))
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            self.live_thinking = format!("…{}", tail);
        }
    }

    fn begin_processing_cycle(&mut self, model: &str) {
        self.processing = true;
        self.processing_started_at = Some(Instant::now());
        self.last_progress_pulse_at = None;
        self.stream_chunk_count = 0;
        self.stream_char_count = 0;
        self.saw_first_token = false;
        self.active_tools.clear();
        self.live_thinking.clear();
        self.push_activity(format!("⟳ dispatching request to {model}"));
    }

    fn finish_processing_cycle(&mut self, label: &str) {
        if !self.processing {
            return;
        }
        let elapsed = self
            .processing_started_at
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or_default();
        self.push_activity(format!(
            "{} {:.2}s • {} chunks • {} chars",
            label, elapsed, self.stream_chunk_count, self.stream_char_count
        ));
        self.processing = false;
        self.processing_started_at = None;
        self.last_progress_pulse_at = None;
        self.stream_chunk_count = 0;
        self.stream_char_count = 0;
        self.saw_first_token = false;
    }

    fn maybe_emit_progress_pulse(&mut self) {
        if !self.processing {
            return;
        }
        let now = Instant::now();
        let should_emit = self
            .last_progress_pulse_at
            .map(|t| now.duration_since(t) >= Duration::from_millis(1250))
            .unwrap_or(true);
        if !should_emit {
            return;
        }
        let elapsed = self
            .processing_started_at
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or_default();
        let tool_state = if self.active_tools.is_empty() {
            "no active tools".to_string()
        } else {
            format!("{} active tool(s)", self.active_tools.len())
        };
        self.push_activity(format!(
            "… working {:.1}s • {} chunks • {} chars • {}",
            elapsed, self.stream_chunk_count, self.stream_char_count, tool_state
        ));
        self.last_progress_pulse_at = Some(now);
    }

    fn refresh_sticky_prompt(&mut self, app: &App) {
        if self.scroll_offset == 0 {
            self.sticky_prompt.clear();
            return;
        }
        let transcript = app.transcript_messages();
        let prompt = transcript
            .iter()
            .rev()
            .find(|m| m.role == hermes_core::MessageRole::User)
            .and_then(|m| m.content.as_deref())
            .unwrap_or("")
            .trim();
        self.sticky_prompt = if prompt.is_empty() {
            String::new()
        } else {
            truncate_chars(prompt, 120)
        };
    }

    fn open_modal(&mut self, modal: PickerModal) {
        self.modal = Some(modal);
        self.mode = InputMode::Insert;
    }

    fn close_modal(&mut self) {
        self.modal = None;
    }

    fn modal_active(&self) -> bool {
        self.modal.is_some()
    }

    fn handle_modal_key(&mut self, key: KeyEvent) -> ModalAction {
        use crossterm::event::{KeyCode, KeyModifiers};
        let Some(modal) = self.modal.as_mut() else {
            return ModalAction::None;
        };
        match key.code {
            KeyCode::Esc => ModalAction::Close,
            KeyCode::Enter => ModalAction::Confirm,
            KeyCode::Up => {
                modal.move_selection(-1);
                ModalAction::None
            }
            KeyCode::Down => {
                modal.move_selection(1);
                ModalAction::None
            }
            KeyCode::PageUp => {
                modal.page_move(-1);
                ModalAction::None
            }
            KeyCode::PageDown => {
                modal.page_move(1);
                ModalAction::None
            }
            KeyCode::Home => {
                modal.selected_filtered = 0;
                ModalAction::None
            }
            KeyCode::End => {
                if !modal.filtered_indices.is_empty() {
                    modal.selected_filtered = modal.filtered_indices.len() - 1;
                }
                ModalAction::None
            }
            KeyCode::Char(' ') => {
                modal.toggle_selected();
                ModalAction::None
            }
            KeyCode::Backspace => {
                modal.query.pop();
                modal.refresh_filter();
                ModalAction::None
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                modal.query.clear();
                modal.refresh_filter();
                ModalAction::None
            }
            KeyCode::Char('d')
                if key.modifiers.is_empty()
                    && modal.query.trim().is_empty()
                    && matches!(modal.kind, PickerKind::ModelProvider) =>
            {
                ModalAction::DisconnectProvider
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty()
                    && modal.query.trim().is_empty()
                    && ch.is_ascii_digit() =>
            {
                let nth = if ch == '0' {
                    10usize
                } else {
                    ch.to_digit(10).unwrap_or(0) as usize
                };
                if nth >= 1 && nth <= modal.filtered_indices.len() {
                    modal.selected_filtered = nth - 1;
                    ModalAction::Confirm
                } else {
                    ModalAction::None
                }
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                modal.query.push(ch);
                modal.refresh_filter();
                ModalAction::None
            }
            _ => ModalAction::None,
        }
    }

    /// Handle a key event and return whether the app should quit.
    pub fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> bool {
        match self.mode {
            InputMode::Normal => self.handle_normal_key(key, app),
            InputMode::Insert => self.handle_insert_key(key, app),
            InputMode::Command => self.handle_command_key(key, app),
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent, _app: &mut App) -> bool {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(8);
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(8);
            }
            KeyCode::End => {
                self.scroll_offset = 0;
            }
            KeyCode::Char('i') => {
                self.mode = InputMode::Insert;
            }
            KeyCode::Char(':') => {
                self.mode = InputMode::Command;
                self.input.clear();
                self.cursor_position = 0;
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                return true; // quit
            }
            _ => {}
        }
        false
    }

    fn handle_insert_key(&mut self, key: KeyEvent, app: &mut App) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mods = key.modifiers;
        if mods.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('l') => {
                    self.activity_lane_open = !self.activity_lane_open;
                    self.status_message = if self.activity_lane_open {
                        "Activity lane enabled".to_string()
                    } else {
                        "Activity lane hidden".to_string()
                    };
                    return false;
                }
                KeyCode::Char('d') => {
                    self.view_density = match self.view_density {
                        ViewDensity::Compact => ViewDensity::Detailed,
                        ViewDensity::Detailed => ViewDensity::Compact,
                    };
                    self.status_message = match self.view_density {
                        ViewDensity::Compact => "Compact transcript mode".to_string(),
                        ViewDensity::Detailed => "Detailed transcript mode".to_string(),
                    };
                    return false;
                }
                KeyCode::Char('t') => {
                    self.show_timestamps = !self.show_timestamps;
                    self.status_message = if self.show_timestamps {
                        "Timestamps visible".to_string()
                    } else {
                        "Timestamps hidden".to_string()
                    };
                    return false;
                }
                KeyCode::Char('e') => {
                    if self.expanded_tool_cards.insert("__all__".to_string()) {
                        self.status_message = "Expanded tool cards".to_string();
                    } else {
                        self.expanded_tool_cards.remove("__all__");
                        self.status_message = "Collapsed tool cards".to_string();
                    }
                    return false;
                }
                KeyCode::Left => {
                    self.move_cursor_word_left();
                    return false;
                }
                KeyCode::Right => {
                    self.move_cursor_word_right();
                    return false;
                }
                _ => {}
            }
        }
        let completion_nav_active = self.input.starts_with('/')
            && !self.completions.is_empty()
            && !self.history_search_active;

        if completion_nav_active && mods.is_empty() {
            match key.code {
                KeyCode::Up => {
                    self.move_completion_selection(-1);
                    return false;
                }
                KeyCode::Down => {
                    self.move_completion_selection(1);
                    return false;
                }
                KeyCode::PageUp => {
                    self.move_completion_selection(-6);
                    return false;
                }
                KeyCode::PageDown => {
                    self.move_completion_selection(6);
                    return false;
                }
                KeyCode::Home => {
                    self.completion_index = Some(0);
                    return false;
                }
                KeyCode::End => {
                    if !self.completions.is_empty() {
                        self.completion_index = Some(self.completions.len() - 1);
                    }
                    return false;
                }
                _ => {}
            }
        }

        match key.code {
            // Scroll transcript without leaving insert mode.
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(8);
                false
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(8);
                false
            }
            // Fine-grained transcript scroll.
            KeyCode::Up if mods.contains(KeyModifiers::CONTROL) => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
                false
            }
            KeyCode::Down if mods.contains(KeyModifiers::CONTROL) => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
                false
            }
            // Jump back to newest transcript content.
            KeyCode::End if mods.contains(KeyModifiers::CONTROL) => {
                self.scroll_offset = 0;
                false
            }
            // Explicit multiline shortcuts.
            KeyCode::Enter if mods.contains(KeyModifiers::SHIFT) => {
                self.insert_newline_at_cursor();
                self.selection_anchor = None;
                self.refresh_completions();
                false
            }
            KeyCode::Char('j') if mods.contains(KeyModifiers::CONTROL) => {
                self.insert_newline_at_cursor();
                self.selection_anchor = None;
                self.refresh_completions();
                false
            }
            // Submit shortcuts are handled in the run-loop after key handling.
            _ if is_submit_shortcut(&key, &self.input) => false,
            KeyCode::Tab => {
                // Accept completion
                self.accept_completion();
                self.completions.clear();
                self.completion_index = None;
                false
            }
            // Ctrl+R toggles reverse-search across message input history.
            KeyCode::Char('r') if mods.contains(KeyModifiers::CONTROL) => {
                self.history_search_active = !self.history_search_active;
                if !self.history_search_active {
                    self.history_search_query.clear();
                }
                false
            }
            KeyCode::Char(c) if self.history_search_active => {
                self.history_search_query.push(c);
                if let Some(found) = app
                    .input_history
                    .iter()
                    .rev()
                    .find(|h| h.contains(&self.history_search_query))
                {
                    self.input = found.clone();
                    self.cursor_position = self.input.len();
                }
                false
            }
            KeyCode::Backspace if self.history_search_active => {
                self.history_search_query.pop();
                false
            }
            // On single-line inputs without completion menus, Up/Down browse previous prompts.
            KeyCode::Up
                if !self.input.contains('\n') && !completion_nav_active && mods.is_empty() =>
            {
                if let Some(prev) = app.history_prev() {
                    self.input = prev.to_string();
                    self.cursor_position = self.input.len();
                }
                self.refresh_completions();
                false
            }
            KeyCode::Down
                if !self.input.contains('\n') && !completion_nav_active && mods.is_empty() =>
            {
                if let Some(next) = app.history_next() {
                    self.input = next.to_string();
                    self.cursor_position = self.input.len();
                }
                self.refresh_completions();
                false
            }
            KeyCode::Esc => {
                if self.history_search_active {
                    self.history_search_active = false;
                    self.history_search_query.clear();
                    return false;
                }
                self.mode = InputMode::Normal;
                false
            }
            _ => {
                self.apply_textarea_input(key);
                self.selection_anchor = None;
                self.refresh_completions();
                false
            }
        }
    }

    fn handle_command_key(&mut self, key: KeyEvent, _app: &mut App) -> bool {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Enter => {
                let input = std::mem::take(&mut self.input);
                self.cursor_position = 0;
                self.mode = InputMode::Insert;
                self.completions.clear();
                self.completion_index = None;
                let _ = input; // Processed outside
                false
            }
            KeyCode::Esc => {
                self.mode = InputMode::Insert;
                self.input.clear();
                self.cursor_position = 0;
                self.completions.clear();
                self.completion_index = None;
                false
            }
            KeyCode::Tab => {
                // Cycle through completions
                if !self.completions.is_empty() {
                    let idx = self
                        .completion_index
                        .map(|i| (i + 1) % self.completions.len())
                        .unwrap_or(0);
                    self.completion_index = Some(idx);
                    self.input = self.completions[idx].clone();
                    self.cursor_position = self.input.len();
                }
                false
            }
            _ => {
                // Delegate to insert handler for typing
                self.handle_insert_key(key, _app)
            }
        }
    }

    /// Update auto-completion suggestions based on current input.
    fn update_completions(&mut self) {
        if self.input.starts_with('/') {
            self.completions = commands::autocomplete(&self.input)
                .into_iter()
                .map(String::from)
                .collect();
            self.completion_index = None;
        } else {
            self.completions.clear();
            self.completion_index = None;
        }
    }

    fn refresh_completions(&mut self) {
        if self.input.starts_with('/') {
            self.update_completions();
        } else {
            self.completions.clear();
            self.completion_index = None;
        }
    }

    fn cursor_row_col(input: &str, cursor_byte: usize) -> (usize, usize) {
        let clamped = cursor_byte.min(input.len());
        let before = &input[..clamped];
        let row = before.bytes().filter(|b| *b == b'\n').count();
        let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let col = input[line_start..clamped].chars().count();
        (row, col)
    }

    fn row_col_to_byte_offset(input: &str, row: usize, col: usize) -> usize {
        let mut current_row = 0usize;
        let mut line_start = 0usize;
        for (idx, ch) in input.char_indices() {
            if current_row == row {
                break;
            }
            if ch == '\n' {
                current_row += 1;
                line_start = idx + ch.len_utf8();
            }
        }
        if current_row < row {
            line_start = input.len();
        }
        let line_end = input[line_start..]
            .find('\n')
            .map(|i| line_start + i)
            .unwrap_or(input.len());
        let mut byte = line_start;
        for (taken, (idx, ch)) in input[line_start..line_end].char_indices().enumerate() {
            if taken == col {
                return line_start + idx;
            }
            byte = line_start + idx + ch.len_utf8();
        }
        byte.min(line_end)
    }

    fn textarea_from_input(&self) -> TextArea<'static> {
        let lines: Vec<String> = if self.input.is_empty() {
            vec![String::new()]
        } else {
            self.input.split('\n').map(ToString::to_string).collect()
        };
        let mut textarea = TextArea::from(lines);
        let (row, col) = Self::cursor_row_col(&self.input, self.cursor_position);
        let row_u16 = row.min(u16::MAX as usize) as u16;
        let col_u16 = col.min(u16::MAX as usize) as u16;
        textarea.move_cursor(CursorMove::Jump(row_u16, col_u16));
        textarea
    }

    fn sync_from_textarea(&mut self, textarea: &TextArea<'_>) {
        self.input = textarea.lines().join("\n");
        let (row, col) = textarea.cursor();
        self.cursor_position = Self::row_col_to_byte_offset(&self.input, row, col);
    }

    fn apply_textarea_input(&mut self, key: KeyEvent) {
        let mut textarea = self.textarea_from_input();
        let _ = textarea.input(key);
        self.sync_from_textarea(&textarea);
    }

    fn insert_newline_at_cursor(&mut self) {
        let at = self.cursor_position.min(self.input.len());
        self.input.insert(at, '\n');
        self.cursor_position = at.saturating_add(1);
    }

    fn move_cursor_word_left(&mut self) {
        if self.cursor_position == 0 || self.input.is_empty() {
            self.cursor_position = 0;
            return;
        }
        let chars: Vec<(usize, char)> = self.input.char_indices().collect();
        let mut idx = chars
            .iter()
            .position(|(byte, _)| *byte >= self.cursor_position)
            .unwrap_or(chars.len());
        if idx > 0 && chars[idx - 1].1.is_whitespace() {
            while idx > 0 && chars[idx - 1].1.is_whitespace() {
                idx -= 1;
            }
        }
        while idx > 0 && !chars[idx - 1].1.is_whitespace() {
            idx -= 1;
        }
        self.cursor_position = chars.get(idx).map(|(b, _)| *b).unwrap_or(0);
    }

    fn move_cursor_word_right(&mut self) {
        if self.input.is_empty() {
            self.cursor_position = 0;
            return;
        }
        let chars: Vec<(usize, char)> = self.input.char_indices().collect();
        let mut idx = chars
            .iter()
            .position(|(byte, _)| *byte > self.cursor_position)
            .unwrap_or(chars.len());
        while idx < chars.len() && chars[idx].1.is_whitespace() {
            idx += 1;
        }
        while idx < chars.len() && !chars[idx].1.is_whitespace() {
            idx += 1;
        }
        self.cursor_position = if idx >= chars.len() {
            self.input.len()
        } else {
            chars[idx].0
        };
    }

    fn move_completion_selection(&mut self, delta: isize) {
        if self.completions.is_empty() {
            self.completion_index = None;
            return;
        }
        let len = self.completions.len() as isize;
        let current = self.completion_index.unwrap_or(0) as isize;
        let mut next = current + delta;
        while next < 0 {
            next += len;
        }
        next %= len;
        self.completion_index = Some(next as usize);
    }

    fn accept_completion(&mut self) {
        if let Some(idx) = self.completion_index {
            if idx < self.completions.len() {
                self.input = self.completions[idx].clone();
                self.cursor_position = self.input.len();
                return;
            }
        }
        if let Some(first) = self.completions.first() {
            self.input = first.clone();
            self.cursor_position = self.input.len();
        }
    }

    /// Get the spinner character for the current frame.
    pub fn spinner_char(&self) -> char {
        const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        SPINNER[self.spinner_frame % SPINNER.len()]
    }

    /// Advance the spinner frame.
    pub fn tick_spinner(&mut self) {
        self.spinner_frame = self.spinner_frame.wrapping_add(1);
    }

    pub fn tick_pet(&mut self) {
        self.pet_frame = self.pet_frame.wrapping_add(1);
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render the full TUI frame.
pub fn render(frame: &mut Frame, app: &App, state: &mut TuiState, theme: &Theme) {
    let resolved = theme.resolved_styles();
    let colors = theme.colors.to_ratatui_colors();

    let size = frame.area();
    if size.width == 0 || size.height == 0 {
        return;
    }
    frame.render_widget(
        Block::default().style(Style::default().bg(colors.background)),
        size,
    );

    // Layout: header, body, input, status bar
    let header_height = 1;
    let composer_lines = state.input.matches('\n').count() as u16 + 1;
    let input_height = (composer_lines + 2).clamp(3, 12);
    let status_height = 1;

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height), // header
            Constraint::Min(5),                // body
            Constraint::Length(input_height),  // input
            Constraint::Length(status_height), // status
        ])
        .split(size);

    let header_area = vertical[0];
    let body_area = vertical[1];
    let input_area = vertical[2];
    let status_area = vertical[3];

    let details_enabled = state.activity_lane_open && body_area.width >= 86;
    let body_split = if details_enabled {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(20), Constraint::Length(38)])
            .split(body_area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(20)])
            .split(body_area)
    };
    let messages_area = body_split[0];
    let details_area = if details_enabled {
        Some(body_split[1])
    } else {
        None
    };

    render_header(frame, app, header_area, &colors);

    // --- Render message history ---
    render_messages(frame, app, state, messages_area, &resolved, &colors);

    if let Some(details_area) = details_area {
        render_live_details(frame, state, details_area, &colors);
    }

    // --- Render input area ---
    if let Some(pos) = render_input(frame, state, input_area, &colors) {
        frame.set_cursor_position(pos);
    }

    // --- Render completions as popup above composer ---
    if should_render_completions_popup(state) {
        render_completions_popup(
            frame,
            &state.completions,
            state.completion_index,
            messages_area,
            input_area,
            &colors,
        );
    }

    if let Some(modal) = state.modal.as_ref() {
        render_picker_modal(frame, modal, &colors);
    }

    // --- Render status bar ---
    render_status(frame, app, state, status_area, &colors);
}

fn should_render_completions_popup(state: &TuiState) -> bool {
    state.mode != InputMode::Normal
        && !state.processing
        && state.modal.is_none()
        && state.input.starts_with('/')
        && !state.input.contains('\n')
        && !state.history_search_active
        && !state.completions.is_empty()
}

fn render_header(frame: &mut Frame, app: &App, area: Rect, colors: &crate::theme::RatatuiColors) {
    let session_short = &app.session_id[..8.min(app.session_id.len())];
    let title = format!(
        " HERMES AGENT ULTRA  •  session {}  •  Enter send  •  Shift+Enter/Ctrl+J newline  •  / commands  •  Ctrl+L lane  •  Ctrl+D density  •  Ctrl+T timestamps",
        session_short
    );
    let text = Text::from(vec![Line::from(vec![Span::styled(
        truncate_chars(&title, area.width.saturating_sub(1) as usize),
        Style::default()
            .fg(colors.status_bar_text)
            .bg(colors.status_bar_bg)
            .add_modifier(Modifier::BOLD),
    )])]);
    let title = Paragraph::new(text)
        .block(Block::default().style(Style::default().bg(colors.status_bar_bg)));
    frame.render_widget(title, area);
}

fn render_live_details(
    frame: &mut Frame,
    state: &TuiState,
    area: Rect,
    colors: &crate::theme::RatatuiColors,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Activity Lane ")
        .style(Style::default().bg(colors.background))
        .border_style(Style::default().fg(colors.status_bar_dim));
    let mut rows: Vec<Line<'static>> = Vec::new();

    if !state.active_tools.is_empty() {
        rows.push(Line::from(vec![
            Span::styled(
                " tools: ",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            ),
            Span::styled(
                truncate_chars(&state.active_tools.join(", "), 120),
                Style::default()
                    .fg(colors.status_bar_strong)
                    .bg(colors.background)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    } else if state.processing {
        rows.push(Line::from(vec![Span::styled(
            " tools: awaiting tool events…",
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background),
        )]));
    }

    if !state.live_thinking.is_empty() {
        rows.push(Line::from(vec![
            Span::styled(
                " thinking: ",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            ),
            Span::styled(
                truncate_chars(&state.live_thinking, 140),
                Style::default().fg(colors.accent).bg(colors.background),
            ),
        ]));
    }

    if let Some((prompt, completion, total)) = state.last_usage {
        rows.push(Line::from(vec![
            Span::styled(
                " usage: ",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            ),
            Span::styled(
                format!("in={} out={} total={}", prompt, completion, total),
                Style::default()
                    .fg(colors.status_bar_text)
                    .bg(colors.background),
            ),
        ]));
    }

    let recent_cap = area.height.saturating_sub(rows.len() as u16 + 3) as usize;
    for event in state
        .recent_activity
        .iter()
        .rev()
        .take(recent_cap.max(2))
        .rev()
    {
        rows.push(Line::from(vec![
            Span::styled(
                " • ",
                Style::default().fg(colors.accent).bg(colors.background),
            ),
            Span::styled(
                truncate_chars(event, area.width.saturating_sub(8) as usize),
                Style::default()
                    .fg(colors.status_bar_text)
                    .bg(colors.background),
            ),
        ]));
    }

    if rows.is_empty() {
        rows.push(Line::from(vec![Span::styled(
            " waiting for activity…",
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background)
                .add_modifier(Modifier::ITALIC),
        )]));
    }
    rows.push(Line::from(vec![Span::styled(
        " Ctrl+L toggle lane",
        Style::default()
            .fg(colors.status_bar_dim)
            .bg(colors.background)
            .add_modifier(Modifier::ITALIC),
    )]));

    let paragraph = Paragraph::new(Text::from(rows))
        .block(block)
        .wrap(Wrap { trim: true });
    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

/// Render the message history area.
fn role_visuals(
    role: hermes_core::MessageRole,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
) -> (&'static str, &'static str, Style, Style) {
    let role_bg = colors.background;
    match role {
        hermes_core::MessageRole::User => (
            "◆",
            "USER",
            styles.user_input.bg(role_bg),
            styles
                .user_input
                .remove_modifier(Modifier::BOLD)
                .bg(role_bg),
        ),
        hermes_core::MessageRole::Assistant => (
            "●",
            "HERMES",
            styles.assistant_response.bg(role_bg),
            styles.assistant_response.bg(role_bg),
        ),
        hermes_core::MessageRole::System => (
            "◇",
            "SYSTEM",
            styles.system_message.bg(role_bg),
            styles.system_message.bg(role_bg),
        ),
        hermes_core::MessageRole::Tool => (
            "◈",
            "TOOL",
            styles.tool_call.bg(role_bg),
            Style::default().fg(colors.status_bar_text).bg(role_bg),
        ),
    }
}

fn render_inline_with_code(
    prefix: &str,
    text: &str,
    base_style: Style,
    code_style: Style,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    if !prefix.is_empty() {
        spans.push(Span::styled(prefix.to_string(), base_style));
    }

    let mut in_code = false;
    let mut current = String::new();
    for ch in text.chars() {
        if ch == '`' {
            if !current.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(&mut current),
                    if in_code { code_style } else { base_style },
                ));
            }
            in_code = !in_code;
            continue;
        }
        current.push(ch);
    }
    if !current.is_empty() {
        spans.push(Span::styled(
            current,
            if in_code { code_style } else { base_style },
        ));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base_style));
    }
    Line::from(spans)
}

fn parse_markdown_numbered_marker(line: &str) -> Option<(&str, &str)> {
    let digits = line
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit())
        .last()
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(0);
    if digits == 0 {
        return None;
    }
    let rest = &line[digits..];
    if let Some(tail) = rest.strip_prefix(". ") {
        return Some((&line[..digits + 1], tail));
    }
    if let Some(tail) = rest.strip_prefix(") ") {
        return Some((&line[..digits + 1], tail));
    }
    None
}

fn keyword_set_for_lang(lang: &str) -> &'static [&'static str] {
    match lang.trim().to_ascii_lowercase().as_str() {
        "rust" | "rs" => &[
            "fn", "let", "mut", "pub", "impl", "struct", "enum", "match", "if", "else", "for",
            "while", "loop", "return", "async", "await", "use", "mod", "trait", "where",
        ],
        "python" | "py" => &[
            "def", "class", "if", "elif", "else", "for", "while", "return", "import", "from",
            "with", "as", "try", "except", "finally", "lambda", "yield", "async", "await",
        ],
        "javascript" | "js" | "typescript" | "ts" => &[
            "function", "const", "let", "var", "if", "else", "for", "while", "return", "class",
            "import", "export", "await", "async", "switch", "case", "break", "new",
        ],
        "json" => &[],
        "bash" | "sh" | "zsh" => &[
            "if", "then", "else", "fi", "for", "do", "done", "case", "esac", "function", "echo",
            "export",
        ],
        _ => &[],
    }
}

fn render_highlighted_code_line(
    line: &str,
    lang: &str,
    colors: &crate::theme::RatatuiColors,
) -> Line<'static> {
    let default_style = Style::default()
        .fg(colors.status_bar_text)
        .bg(colors.background);
    let keyword_style = Style::default()
        .fg(colors.accent)
        .bg(colors.background)
        .add_modifier(Modifier::BOLD);
    let string_style = Style::default()
        .fg(colors.status_bar_warn)
        .bg(colors.background);
    let number_style = Style::default()
        .fg(colors.status_bar_good)
        .bg(colors.background);
    let punctuation_style = Style::default()
        .fg(colors.status_bar_dim)
        .bg(colors.background);
    let mut spans: Vec<Span<'static>> = vec![Span::styled(
        "    │ ",
        Style::default()
            .fg(colors.status_bar_dim)
            .bg(colors.background),
    )];
    let keywords = keyword_set_for_lang(lang);
    let mut token = String::new();
    let mut in_string = false;
    let mut quote_char = '\0';
    let flush_token =
        |spans: &mut Vec<Span<'static>>, token: &mut String, style: Style, keywords: &[&str]| {
            if token.is_empty() {
                return;
            }
            let tok = std::mem::take(token);
            let tok_style = if keywords.iter().any(|kw| kw.eq_ignore_ascii_case(&tok)) {
                style
            } else if tok.chars().all(|ch| ch.is_ascii_digit()) {
                Style::default()
                    .fg(Color::Cyan)
                    .bg(style.bg.unwrap_or(Color::Reset))
            } else {
                default_style
            };
            spans.push(Span::styled(tok, tok_style));
        };

    for ch in line.chars() {
        if in_string {
            token.push(ch);
            if ch == quote_char {
                spans.push(Span::styled(std::mem::take(&mut token), string_style));
                in_string = false;
                quote_char = '\0';
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            flush_token(&mut spans, &mut token, keyword_style, keywords);
            in_string = true;
            quote_char = ch;
            token.push(ch);
            continue;
        }
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            token.push(ch);
            continue;
        }
        flush_token(&mut spans, &mut token, keyword_style, keywords);
        if ch.is_ascii_digit() {
            spans.push(Span::styled(ch.to_string(), number_style));
        } else if ch.is_whitespace() {
            spans.push(Span::styled(ch.to_string(), default_style));
        } else {
            spans.push(Span::styled(ch.to_string(), punctuation_style));
        }
    }
    flush_token(&mut spans, &mut token, keyword_style, keywords);
    if in_string && !token.is_empty() {
        spans.push(Span::styled(token, string_style));
    }
    Line::from(spans)
}

fn parse_table_cells(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }
    let cells: Vec<String> = trimmed
        .split('|')
        .map(str::trim)
        .filter(|cell| !cell.is_empty())
        .map(ToString::to_string)
        .collect();
    if cells.len() < 2 {
        return None;
    }
    Some(cells)
}

fn content_width_for_table_row(cells: usize, min_per_cell: usize) -> usize {
    cells.saturating_mul(min_per_cell).max(8)
}

fn message_fingerprint(msg: &hermes_core::Message) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    let role_tag = match msg.role {
        hermes_core::MessageRole::System => "system",
        hermes_core::MessageRole::User => "user",
        hermes_core::MessageRole::Assistant => "assistant",
        hermes_core::MessageRole::Tool => "tool",
    };
    role_tag.hash(&mut hasher);
    msg.content.hash(&mut hasher);
    msg.tool_call_id.hash(&mut hasher);
    msg.reasoning_content.hash(&mut hasher);
    if let Some(calls) = msg.tool_calls.as_ref() {
        for tc in calls {
            tc.id.hash(&mut hasher);
            tc.function.name.hash(&mut hasher);
            tc.function.arguments.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn transcript_fingerprint(messages: &[hermes_core::Message], state: &TuiState, width: u16) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    width.hash(&mut hasher);
    state.stream_buffer.hash(&mut hasher);
    state.show_timestamps.hash(&mut hasher);
    state.view_density.hash(&mut hasher);
    for msg in messages {
        message_fingerprint(msg).hash(&mut hasher);
    }
    hasher.finish()
}

fn transcript_message_fingerprints(messages: &[hermes_core::Message]) -> Vec<u64> {
    messages.iter().map(message_fingerprint).collect()
}

fn count_renderable_messages(messages: &[hermes_core::Message]) -> usize {
    messages
        .iter()
        .filter(|msg| !matches!(msg.role, hermes_core::MessageRole::System))
        .count()
}

fn render_assistant_markdown_lines(
    content: &str,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
) -> Vec<Line<'static>> {
    let mut rendered: Vec<Line<'static>> = Vec::new();
    let mut in_code_block = false;
    let mut code_lang = String::new();
    let code_frame_style = Style::default()
        .fg(colors.status_bar_dim)
        .bg(colors.background);
    let heading_style = Style::default()
        .fg(colors.status_bar_strong)
        .bg(colors.background)
        .add_modifier(Modifier::BOLD);
    let bullet_style = Style::default()
        .fg(colors.accent)
        .bg(colors.background)
        .add_modifier(Modifier::BOLD);
    let quote_style = Style::default()
        .fg(colors.status_bar_dim)
        .bg(colors.background)
        .add_modifier(Modifier::ITALIC);
    let inline_code_style = Style::default()
        .fg(colors.accent)
        .bg(colors.background)
        .add_modifier(Modifier::BOLD);

    for raw in content.lines() {
        let trimmed = raw.trim_start();
        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if is_fence {
            if in_code_block {
                rendered.push(Line::from(vec![Span::styled(
                    "    └─ end code",
                    code_frame_style,
                )]));
                in_code_block = false;
                code_lang.clear();
            } else {
                in_code_block = true;
                code_lang = trimmed
                    .trim_start_matches('`')
                    .trim_start_matches('~')
                    .trim()
                    .to_string();
                let label = if code_lang.is_empty() {
                    "    ┌─ code".to_string()
                } else {
                    format!("    ┌─ code ({})", code_lang)
                };
                rendered.push(Line::from(vec![Span::styled(label, code_frame_style)]));
            }
            continue;
        }

        if in_code_block {
            rendered.push(render_highlighted_code_line(raw, &code_lang, colors));
            continue;
        }

        if trimmed.is_empty() {
            rendered.push(Line::from(String::new()));
            continue;
        }

        let heading_level = trimmed.chars().take_while(|ch| *ch == '#').count();
        if (1..=6).contains(&heading_level) {
            let rest = trimmed[heading_level..].trim_start();
            if !rest.is_empty() {
                rendered.push(Line::from(vec![
                    Span::styled(
                        format!("    {} ", "#".repeat(heading_level)),
                        Style::default()
                            .fg(colors.status_bar_dim)
                            .bg(colors.background),
                    ),
                    Span::styled(rest.to_string(), heading_style),
                ]));
                continue;
            }
        }

        if let Some(quote) = trimmed.strip_prefix('>').map(str::trim_start) {
            rendered.push(render_inline_with_code(
                "    ▎ ",
                quote,
                quote_style,
                inline_code_style,
            ));
            continue;
        }

        if let Some(body) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            rendered.push(Line::from(vec![
                Span::styled("    • ", bullet_style),
                Span::styled(
                    body.to_string(),
                    styles.assistant_response.bg(colors.background),
                ),
            ]));
            continue;
        }

        if let Some(cells) = parse_table_cells(trimmed) {
            let separator = cells
                .iter()
                .all(|cell| cell.chars().all(|ch| ch == '-' || ch == ':'));
            if separator {
                rendered.push(Line::from(vec![Span::styled(
                    format!(
                        "    ├{}┤",
                        "─".repeat(content_width_for_table_row(cells.len(), 16))
                    ),
                    Style::default()
                        .fg(colors.status_bar_dim)
                        .bg(colors.background),
                )]));
                continue;
            }
            let mut row_spans: Vec<Span<'static>> = vec![Span::styled(
                "    │ ",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            )];
            for (idx, cell) in cells.iter().enumerate() {
                if idx > 0 {
                    row_spans.push(Span::styled(
                        " │ ",
                        Style::default()
                            .fg(colors.status_bar_dim)
                            .bg(colors.background),
                    ));
                }
                row_spans.push(Span::styled(
                    truncate_chars(cell, 24),
                    Style::default()
                        .fg(colors.status_bar_text)
                        .bg(colors.background),
                ));
            }
            row_spans.push(Span::styled(
                " │",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            ));
            rendered.push(Line::from(row_spans));
            continue;
        }

        if let Some((marker, body)) = parse_markdown_numbered_marker(trimmed) {
            rendered.push(Line::from(vec![
                Span::styled(format!("    {marker} "), bullet_style),
                Span::styled(
                    body.to_string(),
                    styles.assistant_response.bg(colors.background),
                ),
            ]));
            continue;
        }

        rendered.push(render_inline_with_code(
            "    ",
            trimmed,
            styles.assistant_response,
            inline_code_style,
        ));
    }

    if in_code_block {
        rendered.push(Line::from(vec![Span::styled(
            "    └─ end code",
            code_frame_style,
        )]));
    }
    rendered
}

fn value_to_display_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    return serde_json::to_string_pretty(&parsed)
                        .unwrap_or_else(|_| raw.to_string());
                }
            }
            raw.to_string()
        }
        _ => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    }
}

fn push_block(lines: &mut Vec<String>, header: &str, value: &serde_json::Value) {
    let rendered = value_to_display_text(value);
    if rendered.trim().is_empty() {
        return;
    }
    lines.push(format!("[{header}]"));
    for line in rendered.lines() {
        lines.push(line.to_string());
    }
}

fn format_tool_message_lines(content: &str) -> Vec<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return vec![String::new()];
    }

    let parsed = match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => v,
        Err(_) => {
            return content
                .lines()
                .map(std::string::ToString::to_string)
                .collect();
        }
    };

    if let Some(obj) = parsed.as_object() {
        let mut lines: Vec<String> = Vec::new();

        if let Some(w) = obj.get("_budget_warning").and_then(|v| v.as_str()) {
            lines.push(format!("⚠ {}", w.trim()));
        }

        for key in ["result", "error", "stdout", "stderr", "message"] {
            if let Some(value) = obj.get(key) {
                push_block(&mut lines, key, value);
            }
        }

        let mut extras = serde_json::Map::new();
        for (k, v) in obj.iter() {
            if k == "_budget_warning"
                || k == "result"
                || k == "error"
                || k == "stdout"
                || k == "stderr"
                || k == "message"
            {
                continue;
            }
            extras.insert(k.clone(), v.clone());
        }
        if !extras.is_empty() {
            push_block(&mut lines, "meta", &serde_json::Value::Object(extras));
        }
        if !lines.is_empty() {
            return lines;
        }
    }

    serde_json::to_string_pretty(&parsed)
        .map(|s| s.lines().map(std::string::ToString::to_string).collect())
        .unwrap_or_else(|_| {
            content
                .lines()
                .map(std::string::ToString::to_string)
                .collect()
        })
}

fn append_transcript_message_lines(
    lines: &mut Vec<Line<'static>>,
    msg: &hermes_core::Message,
    msg_idx: usize,
    rendered_messages: &mut usize,
    state: &mut TuiState,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
    divider: &str,
) {
    // Hide internal orchestration/system payloads from the chat transcript.
    if matches!(msg.role, hermes_core::MessageRole::System) {
        return;
    }
    if *rendered_messages > 0 && matches!(state.view_density, ViewDensity::Detailed) {
        lines.push(Line::from(String::new()));
    }
    *rendered_messages += 1;
    let (glyph, label, label_style, body_style) = role_visuals(msg.role, styles, colors);
    let stamp = if state.show_timestamps {
        let fp = message_fingerprint(msg);
        state
            .message_time_labels
            .entry(fp)
            .or_insert_with(|| Local::now().format("%H:%M:%S").to_string())
            .clone()
    } else {
        String::new()
    };
    let label_text = if stamp.is_empty() {
        label.to_string()
    } else {
        format!("{label}  {stamp}")
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!(" ╭ {} ", glyph),
            label_style.add_modifier(Modifier::BOLD),
        ),
        Span::styled(label_text, label_style.add_modifier(Modifier::BOLD)),
    ]));

    if let Some(content) = msg.content.as_deref() {
        match msg.role {
            hermes_core::MessageRole::Assistant => {
                lines.extend(render_assistant_markdown_lines(content, styles, colors));
            }
            hermes_core::MessageRole::Tool => {
                let card_key = format!("tool:{msg_idx}");
                let expanded = state.expanded_tool_cards.contains(&card_key)
                    || state.expanded_tool_cards.contains("__all__")
                    || matches!(state.view_density, ViewDensity::Detailed);
                let all_lines = format_tool_message_lines(content);
                let shown = if expanded { 32 } else { 5 };
                lines.push(Line::from(vec![Span::styled(
                    format!(
                        "    [tool card: {} | {} lines | Ctrl+E toggles]",
                        if expanded { "expanded" } else { "collapsed" },
                        all_lines.len()
                    ),
                    Style::default()
                        .fg(colors.status_bar_dim)
                        .bg(colors.background),
                )]));
                for line in all_lines.iter().take(shown) {
                    lines.push(render_inline_with_code(
                        "    ",
                        line,
                        styles.tool_result,
                        Style::default()
                            .fg(colors.accent)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                if all_lines.len() > shown {
                    lines.push(Line::from(vec![Span::styled(
                        format!("    … {} more lines", all_lines.len() - shown),
                        Style::default()
                            .fg(colors.status_bar_dim)
                            .bg(colors.background),
                    )]));
                }
            }
            _ => {
                for line in content.lines() {
                    lines.push(render_inline_with_code(
                        "    ",
                        line,
                        body_style,
                        Style::default()
                            .fg(colors.accent)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
            }
        }
    }

    if msg.role == hermes_core::MessageRole::Assistant {
        if matches!(state.view_density, ViewDensity::Detailed) {
            if let Some(reasoning) = msg
                .reasoning_content
                .as_ref()
                .filter(|s| !s.trim().is_empty())
            {
                lines.push(Line::from(vec![Span::styled(
                    "    🤔 reasoning",
                    Style::default()
                        .fg(colors.status_bar_dim)
                        .bg(colors.background),
                )]));
                for line in reasoning.lines() {
                    lines.push(Line::from(vec![Span::styled(
                        format!("      {}", line.trim_end()),
                        Style::default()
                            .fg(colors.status_bar_dim)
                            .bg(colors.background)
                            .add_modifier(Modifier::ITALIC),
                    )]));
                }
            }
        }
        if let Some(tool_calls) = msg.tool_calls.as_ref() {
            for tc in tool_calls {
                let args = serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                    .unwrap_or_else(|_| serde_json::Value::Null);
                let preview =
                    build_tool_preview_from_value(&tc.function.name, &args, 44).unwrap_or_default();
                let emoji = tool_emoji(&tc.function.name);
                let summary = if preview.is_empty() {
                    format!("{emoji} {}", tc.function.name)
                } else {
                    format!("{emoji} {} {}", tc.function.name, preview)
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        "    ↳ ",
                        Style::default()
                            .fg(colors.status_bar_dim)
                            .bg(colors.background),
                    ),
                    Span::styled(summary, styles.tool_call),
                ]));
            }
        }
    }
    if matches!(state.view_density, ViewDensity::Detailed) {
        lines.push(Line::from(vec![Span::styled(
            divider.to_string(),
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background),
        )]));
    }
}

fn build_transcript_lines(
    messages: &[hermes_core::Message],
    state: &mut TuiState,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
    content_width: u16,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut rendered_messages = 0usize;
    let divider = transcript_divider(content_width);

    for (msg_idx, msg) in messages.iter().enumerate() {
        append_transcript_message_lines(
            &mut lines,
            msg,
            msg_idx,
            &mut rendered_messages,
            state,
            styles,
            colors,
            &divider,
        );
    }

    // Streaming buffer (partial assistant response)
    if !state.stream_buffer.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::from(String::new()));
        }
        lines.push(Line::from(vec![
            Span::styled(
                " ╭ ● ",
                styles.assistant_response.add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "HERMES (streaming)",
                styles.assistant_response.add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.extend(render_assistant_markdown_lines(
            &state.stream_buffer,
            styles,
            colors,
        ));
        lines.push(Line::from(vec![Span::styled(
            "    ▌",
            Style::default()
                .fg(colors.accent)
                .bg(colors.background)
                .add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::from(vec![Span::styled(
            divider,
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background),
        )]));
    }

    if lines.is_empty() {
        let neon = Style::default()
            .fg(colors.status_bar_strong)
            .bg(colors.background)
            .add_modifier(Modifier::BOLD);
        let dim = Style::default()
            .fg(colors.status_bar_dim)
            .bg(colors.background);
        let accent = Style::default().fg(colors.accent).bg(colors.background);
        let hero = [
            " ╔══════════════════════════════════════════════════════════════════╗",
            " ║                                                                  ║",
            " ║   ██╗  ██╗███████╗██████╗ ███╗   ███╗███████╗███████╗           ║",
            " ║   ██║  ██║██╔════╝██╔══██╗████╗ ████║██╔════╝██╔════╝           ║",
            " ║   ███████║█████╗  ██████╔╝██╔████╔██║█████╗  ███████╗           ║",
            " ║   ██╔══██║██╔══╝  ██╔══██╗██║╚██╔╝██║██╔══╝  ╚════██║           ║",
            " ║   ██║  ██║███████╗██║  ██║██║ ╚═╝ ██║███████╗███████║           ║",
            " ║   ╚═╝  ╚═╝╚══════╝╚═╝  ╚═╝╚═╝     ╚═╝╚══════╝╚══════╝           ║",
            " ║                                                                  ║",
            " ║        AGENT ULTRA • RETRO NEON OPS • READY FOR EXECUTION       ║",
            " ║                                                                  ║",
            " ╚══════════════════════════════════════════════════════════════════╝",
        ];
        for (idx, row) in hero.iter().enumerate() {
            let style = if idx == 0 || idx == hero.len() - 1 {
                accent
            } else if row.contains("AGENT ULTRA") {
                neon
            } else {
                dim
            };
            lines.push(Line::from(vec![Span::styled((*row).to_string(), style)]));
        }
        lines.push(Line::from(String::new()));
        lines.push(Line::from(vec![Span::styled(
            " Start chatting — your messages and Hermes replies will appear here.",
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background)
                .add_modifier(Modifier::ITALIC),
        )]));
    }
    lines
}

fn render_messages(
    frame: &mut Frame,
    app: &App,
    state: &mut TuiState,
    area: Rect,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
) {
    let title = if state.scroll_offset > 0 {
        format!(" Conversation (+{}) ", state.scroll_offset)
    } else {
        " Conversation ".to_string()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().bg(colors.background))
        .border_style(Style::default().fg(colors.status_bar_dim));
    let inner = block.inner(area);
    if inner.width == 0 || inner.height == 0 {
        frame.render_widget(Clear, area);
        frame.render_widget(block, area);
        return;
    }
    let transcript = app.transcript_messages();
    let viewport_rows = usize::from(inner.height.max(1));
    let fingerprint = transcript_fingerprint(&transcript, state, inner.width);
    let message_fingerprints = transcript_message_fingerprints(&transcript);
    if state.transcript_cache.fingerprint != fingerprint
        || state.transcript_cache.width != inner.width
    {
        let cache = &state.transcript_cache;
        let can_incremental_append = !cache.had_streaming
            && state.stream_buffer.is_empty()
            && cache.width == inner.width
            && cache.total_messages > 0
            && transcript.len() > cache.total_messages
            && cache.show_timestamps == state.show_timestamps
            && cache.view_density == state.view_density
            && cache.message_fingerprints.len() == cache.total_messages
            && message_fingerprints.starts_with(&cache.message_fingerprints);

        if can_incremental_append {
            let start_idx = state.transcript_cache.total_messages;
            let mut lines = std::mem::take(&mut state.transcript_cache.lines);
            let mut rendered_messages = state.transcript_cache.rendered_messages;
            let divider = transcript_divider(inner.width);
            for (msg_idx, msg) in transcript.iter().enumerate().skip(start_idx) {
                append_transcript_message_lines(
                    &mut lines,
                    msg,
                    msg_idx,
                    &mut rendered_messages,
                    state,
                    styles,
                    colors,
                    &divider,
                );
            }
            state.transcript_cache = TranscriptCache {
                fingerprint,
                width: inner.width,
                total_messages: transcript.len(),
                rendered_messages,
                message_fingerprints,
                show_timestamps: state.show_timestamps,
                view_density: state.view_density,
                had_streaming: false,
                lines,
            };
        } else {
            let prev_width = state.transcript_cache.width;
            let prev_len = state.transcript_cache.lines.len();
            let prev_anchor_line = if prev_width != 0
                && prev_width != inner.width
                && state.scroll_offset > 0
                && prev_len > 0
            {
                let old_view_rows = viewport_rows.min(prev_len.max(1));
                let max_hidden = prev_len.saturating_sub(old_view_rows);
                let hidden = usize::from(state.scroll_offset).min(max_hidden);
                let old_end = prev_len.saturating_sub(hidden);
                let old_start = old_end.saturating_sub(old_view_rows);
                state
                    .transcript_cache
                    .lines
                    .get(old_start)
                    .map(Line::to_string)
            } else {
                None
            };

            let new_lines = build_transcript_lines(&transcript, state, styles, colors, inner.width);
            if let Some(anchor_text) = prev_anchor_line {
                if let Some(new_idx) = new_lines
                    .iter()
                    .position(|line| line.to_string() == anchor_text)
                {
                    let new_len = new_lines.len();
                    let visible = viewport_rows.min(new_len.max(1));
                    let new_hidden = new_len.saturating_sub((new_idx + visible).min(new_len));
                    state.scroll_offset = new_hidden.min(u16::MAX as usize) as u16;
                }
            }
            state.transcript_cache = TranscriptCache {
                fingerprint,
                width: inner.width,
                total_messages: transcript.len(),
                rendered_messages: count_renderable_messages(&transcript),
                message_fingerprints,
                show_timestamps: state.show_timestamps,
                view_density: state.view_density,
                had_streaming: !state.stream_buffer.is_empty(),
                lines: new_lines,
            };
        }
    }
    let lines = &state.transcript_cache.lines;
    let text = Text::from(lines.clone());
    let total_visual_rows = approximate_visual_rows(lines, inner.width);
    let max_hidden_from_bottom = total_visual_rows.saturating_sub(viewport_rows);
    let hidden_from_bottom = usize::from(state.scroll_offset).min(max_hidden_from_bottom);
    if usize::from(state.scroll_offset) != hidden_from_bottom {
        state.scroll_offset = hidden_from_bottom.min(u16::MAX as usize) as u16;
    }
    let top_visual_row = total_visual_rows.saturating_sub(viewport_rows + hidden_from_bottom);

    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((top_visual_row as u16, 0));

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);

    if total_visual_rows > viewport_rows {
        let mut scrollbar_state = ScrollbarState::new(total_visual_rows)
            .position(top_visual_row)
            .viewport_content_length(viewport_rows);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .track_symbol(Some("│"))
            .thumb_symbol("█")
            .begin_symbol(None)
            .end_symbol(None)
            .track_style(
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            )
            .thumb_style(
                Style::default()
                    .fg(colors.status_bar_strong)
                    .bg(colors.background),
            );
        frame.render_stateful_widget(scrollbar, inner, &mut scrollbar_state);
    }
}

fn approximate_visual_rows(lines: &[Line<'static>], wrap_width: u16) -> usize {
    let width = usize::from(wrap_width.max(1));
    lines
        .iter()
        .map(|line| {
            let chars = line.to_string().chars().count().max(1);
            ((chars - 1) / width) + 1
        })
        .sum::<usize>()
        .max(1)
}

/// Render slash-command completions as a popup over the conversation panel.
fn render_completions_popup(
    frame: &mut Frame,
    completions: &[String],
    selected: Option<usize>,
    messages_area: Rect,
    input_area: Rect,
    colors: &crate::theme::RatatuiColors,
) {
    if completions.is_empty() {
        return;
    }
    let max_inner_rows = 10usize;
    let visible_rows = completions.len().min(max_inner_rows).max(1);
    let mut start = 0usize;
    if let Some(sel) = selected {
        if sel >= visible_rows {
            start = sel + 1 - visible_rows;
        }
    }
    let end = (start + visible_rows).min(completions.len());
    let max_item_width = completions[start..end]
        .iter()
        .map(|c| {
            let desc = crate::commands::help_for(c).unwrap_or("");
            if desc.is_empty() {
                c.chars().count()
            } else {
                format!("{c} — {desc}").chars().count()
            }
        })
        .max()
        .unwrap_or(0);
    let popup_max_width = messages_area.width.saturating_sub(2).max(1);
    let popup_min_width = 36u16.min(popup_max_width);
    let popup_width = (max_item_width as u16 + 8).clamp(popup_min_width, popup_max_width);
    let popup_height = (end.saturating_sub(start) as u16 + 2).max(3);
    if popup_width == 0 || popup_height == 0 {
        return;
    }
    let right_bound = messages_area.x + messages_area.width.saturating_sub(1);
    let mut x = input_area.x.saturating_add(1);
    if x + popup_width > right_bound {
        x = right_bound.saturating_sub(popup_width);
    }
    let min_y = messages_area.y.saturating_add(1);
    let y = input_area.y.saturating_sub(popup_height).max(min_y);
    let popup = Rect {
        x,
        y,
        width: popup_width,
        height: popup_height,
    };

    let inner_width = popup_width.saturating_sub(4) as usize;
    let items: Vec<Line<'static>> = completions
        .iter()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(|(i, cmd)| {
            let style = if selected == Some(i) {
                Style::default()
                    .fg(Color::Black)
                    .bg(colors.status_bar_strong)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(colors.status_bar_text)
                    .bg(colors.status_bar_bg)
            };
            let desc = crate::commands::help_for(cmd).unwrap_or("");
            let text = if desc.is_empty() {
                cmd.to_string()
            } else {
                format!("{:<18} {}", cmd, desc)
            };
            Line::from(Span::styled(truncate_chars(&text, inner_width), style))
        })
        .collect();

    let title = if completions.len() > visible_rows {
        format!(
            " Slash Commands ({}/{}) ↑↓ scroll Tab accept ",
            end,
            completions.len()
        )
    } else {
        " Slash Commands ".to_string()
    };

    let paragraph = Paragraph::new(Text::from(items))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .style(Style::default().bg(colors.status_bar_bg))
                .border_style(Style::default().fg(colors.status_bar_strong))
                .title(title),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(Clear, popup);
    frame.render_widget(paragraph, popup);

    if completions.len() > visible_rows {
        let inner = Rect {
            x: popup.x.saturating_add(1),
            y: popup.y.saturating_add(1),
            width: popup.width.saturating_sub(2),
            height: popup.height.saturating_sub(2),
        };
        let mut scrollbar_state = ScrollbarState::new(completions.len())
            .position(start)
            .viewport_content_length(visible_rows);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .track_symbol(Some("│"))
            .thumb_symbol("█")
            .begin_symbol(None)
            .end_symbol(None)
            .track_style(
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.status_bar_bg),
            )
            .thumb_style(
                Style::default()
                    .fg(colors.status_bar_strong)
                    .bg(colors.status_bar_bg),
            );
        frame.render_stateful_widget(scrollbar, inner, &mut scrollbar_state);
    }
}

fn render_picker_modal(
    frame: &mut Frame,
    modal: &PickerModal,
    colors: &crate::theme::RatatuiColors,
) {
    let area = frame.area();
    if area.width < 20 || area.height < 8 {
        return;
    }
    let width = (area.width.saturating_sub(6)).min(110).max(48);
    let height = (area.height.saturating_sub(4)).min(22).max(10);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", modal.title))
        .style(Style::default().bg(colors.status_bar_bg))
        .border_style(Style::default().fg(colors.status_bar_strong));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let footer_height = 2u16;
    let query_height = 1u16;
    let rows_height = inner.height.saturating_sub(footer_height + query_height);
    let rows_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: rows_height,
    };
    let query_area = Rect {
        x: inner.x,
        y: inner.y + rows_height,
        width: inner.width,
        height: query_height,
    };
    let footer_area = Rect {
        x: inner.x,
        y: inner.y + rows_height + query_height,
        width: inner.width,
        height: footer_height,
    };

    let (start, end) = modal.visible_window();
    let items: Vec<Line<'static>> = if modal.filtered_indices.is_empty() {
        vec![Line::from(vec![Span::styled(
            "No matches for current search query.",
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.status_bar_bg)
                .add_modifier(Modifier::ITALIC),
        )])]
    } else {
        modal
            .filtered_indices
            .iter()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start))
            .map(|(filtered_idx, item_idx)| {
                let item = &modal.items[*item_idx];
                let selected = filtered_idx == modal.selected_filtered;
                let selected_marker = if selected { "▶" } else { " " };
                let absolute_number = filtered_idx + 1;
                let multi_marker = if modal.allow_multi {
                    if modal.selected_values.contains(&item.value) {
                        "■ "
                    } else {
                        "□ "
                    }
                } else {
                    ""
                };
                let row_style = if selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(colors.status_bar_strong)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(colors.status_bar_text)
                        .bg(colors.status_bar_bg)
                };
                let detail_style = if selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(colors.status_bar_strong)
                } else {
                    Style::default()
                        .fg(colors.status_bar_dim)
                        .bg(colors.status_bar_bg)
                };
                let text = format!(
                    "{selected_marker} {:>3}. {multi_marker}{}",
                    absolute_number, item.label
                );
                let available = rows_area.width.saturating_sub(2) as usize;
                let primary = truncate_chars(&text, available);
                if item.detail.is_empty() {
                    Line::from(vec![Span::styled(primary, row_style)])
                } else {
                    let detail = truncate_chars(
                        &format!("  {}", item.detail),
                        rows_area.width.saturating_sub(2) as usize,
                    );
                    Line::from(vec![
                        Span::styled(primary, row_style),
                        Span::styled("  ", row_style),
                        Span::styled(detail, detail_style),
                    ])
                }
            })
            .collect()
    };
    frame.render_widget(
        Paragraph::new(Text::from(items))
            .style(Style::default().bg(colors.status_bar_bg))
            .wrap(Wrap { trim: true }),
        rows_area,
    );

    let query_line = format!(
        "Search: {}",
        if modal.query.is_empty() {
            "(type to filter)"
        } else {
            modal.query.as_str()
        }
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            truncate_chars(&query_line, query_area.width as usize),
            Style::default()
                .fg(colors.accent)
                .bg(colors.status_bar_bg)
                .add_modifier(Modifier::BOLD),
        )])),
        query_area,
    );

    let footer = if modal.allow_multi {
        "↑↓ move • PgUp/PgDn page • Space toggle • Enter confirm • Esc close"
    } else if matches!(modal.kind, PickerKind::ModelProvider) {
        "↑↓ move • 1-9/0 quick-pick • d disconnect • Enter select • Esc close"
    } else {
        "↑↓ move • PgUp/PgDn page • 1-9/0 quick-pick • Enter select • Esc close"
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            truncate_chars(footer, footer_area.width as usize),
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.status_bar_bg),
        )])),
        footer_area,
    );
}

/// Render the input area (supports multi-line display with wrapping).
fn render_input(
    frame: &mut Frame,
    state: &TuiState,
    area: Rect,
    colors: &crate::theme::RatatuiColors,
) -> Option<Position> {
    let mode_label = match state.mode {
        InputMode::Normal => "NORMAL",
        InputMode::Insert => "INSERT",
        InputMode::Command => "COMMAND",
    };
    let mode_color = match state.mode {
        InputMode::Normal => colors.status_bar_dim,
        InputMode::Insert => colors.status_bar_good,
        InputMode::Command => colors.accent,
    };
    let line_count = state.input.matches('\n').count() + 1;

    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![
            Span::styled(" Message  •  ", Style::default().fg(colors.status_bar_dim)),
            Span::styled(
                mode_label.to_string(),
                Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  •  L{}  •  Ctrl+←/→ word-jump ", line_count),
                Style::default().fg(colors.status_bar_dim),
            ),
        ]))
        .style(Style::default().bg(colors.background))
        .border_style(Style::default().fg(colors.status_bar_strong));
    if state.history_search_active {
        block = block.title_bottom(Line::from(Span::styled(
            format!(
                " reverse-i-search: `{}` (Ctrl+R to exit) ",
                state.history_search_query
            ),
            Style::default()
                .fg(colors.status_bar_warn)
                .bg(colors.background)
                .add_modifier(Modifier::ITALIC),
        )));
    }

    let mut textarea = state.textarea_from_input();
    textarea.set_block(block.clone());
    textarea.set_style(Style::default().fg(colors.foreground).bg(colors.background));
    textarea.set_cursor_style(
        Style::default()
            .fg(Color::Black)
            .bg(colors.status_bar_strong)
            .add_modifier(Modifier::BOLD),
    );
    textarea.set_cursor_line_style(Style::default().bg(colors.background));
    if state.input.is_empty() && state.mode == InputMode::Insert && !state.history_search_active {
        textarea.set_placeholder_text(
            "Type a message (Enter sends, Shift+Enter/Ctrl+J inserts newline)",
        );
        textarea.set_placeholder_style(
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background)
                .add_modifier(Modifier::ITALIC),
        );
    } else {
        textarea.set_placeholder_text("");
    }

    frame.render_widget(Clear, area);
    frame.render_widget(&textarea, area);

    if state.mode == InputMode::Normal {
        return None;
    }

    let inner = block.inner(area);
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let (row, col) = textarea.cursor();
    Some(Position {
        x: inner.x + (col as u16).min(inner.width.saturating_sub(1)),
        y: inner.y + (row as u16).min(inner.height.saturating_sub(1)),
    })
}

/// Render the status bar at the bottom of the screen.
fn status_message_style(message: &str, colors: &crate::theme::RatatuiColors) -> Style {
    let lower = message.to_ascii_lowercase();
    if lower.contains("error") {
        Style::default()
            .fg(colors.status_bar_critical)
            .bg(colors.status_bar_bg)
    } else if lower.contains("warn") {
        Style::default()
            .fg(colors.status_bar_warn)
            .bg(colors.status_bar_bg)
    } else {
        Style::default()
            .fg(colors.status_bar_text)
            .bg(colors.status_bar_bg)
    }
}

/// Render the status bar at the bottom of the screen.
fn render_status(
    frame: &mut Frame,
    app: &App,
    state: &TuiState,
    area: Rect,
    colors: &crate::theme::RatatuiColors,
) {
    let processing_indicator = if state.processing {
        format!("{}", state.spinner_char())
    } else {
        "✓".to_string()
    };
    let model = &app.current_model;
    let session = &app.session_id[..8.min(app.session_id.len())];
    let msg_count = state
        .transcript_cache
        .total_messages
        .max(app.messages.len());
    let scroll_hint = if state.scroll_offset > 0 {
        format!(" (history +{})", state.scroll_offset)
    } else {
        String::new()
    };

    let base = Style::default()
        .fg(colors.status_bar_text)
        .bg(colors.status_bar_bg);

    let mut status_text = format!(
        "{} {} | {} | {} msgs | {}",
        processing_indicator, state.mode, model, msg_count, session
    );
    status_text.push_str(match state.view_density {
        ViewDensity::Compact => " | compact",
        ViewDensity::Detailed => " | detailed",
    });
    if state.show_timestamps {
        status_text.push_str(" | ts:on");
    }
    if state.activity_lane_open {
        status_text.push_str(" | lane:on");
    } else {
        status_text.push_str(" | lane:off");
    }
    if state.background_jobs_running > 0 {
        status_text.push_str(&format!(" | bg:{}", state.background_jobs_running));
    }
    status_text.push_str(if app.mouse_enabled() {
        " | mouse:on"
    } else {
        " | mouse:off"
    });
    if !state.sticky_prompt.is_empty() {
        status_text.push_str(&format!(
            " | ↳ {}",
            truncate_chars(&state.sticky_prompt, 40)
        ));
    }
    if !state.status_message.is_empty() || !scroll_hint.is_empty() {
        status_text.push_str(" | ");
        status_text.push_str(&state.status_message);
        status_text.push_str(&scroll_hint);
    }
    if let Some(frame_token) =
        pet_frame_token(app.pet_settings(), state.pet_frame, state.processing)
    {
        if matches!(app.pet_settings().dock, crate::app::PetDock::Left) {
            status_text = format!("{frame_token} | {status_text}");
        } else {
            status_text.push_str(&format!(" | {frame_token}"));
        }
    }
    let clipped = truncate_chars(&status_text, area.width.saturating_sub(1) as usize);
    let line_style = if state.status_message.is_empty() {
        base
    } else {
        status_message_style(&state.status_message, colors).bg(colors.status_bar_bg)
    };
    let status_bar = Paragraph::new(Line::from(Span::styled(clipped, line_style)))
        .block(Block::default().style(Style::default().bg(colors.status_bar_bg)));
    frame.render_widget(status_bar, area);
}

fn transcript_divider(content_width: u16) -> String {
    let width = usize::from(content_width.max(12));
    let rule = "─".repeat(width.saturating_sub(3).max(8));
    format!(" ╰{}", rule)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    if max_chars <= 1 {
        return "…".to_string();
    }
    let take = max_chars.saturating_sub(1);
    let mut out: String = text.chars().take(take).collect();
    out.push('…');
    out
}

fn fuzzy_subsequence_score(needle: &str, haystack: &str) -> i32 {
    if needle.is_empty() || haystack.is_empty() {
        return 0;
    }
    let mut score = 0i32;
    let mut idx = 0usize;
    let chars: Vec<char> = haystack.chars().collect();
    for ch in needle.chars() {
        let mut found = false;
        while idx < chars.len() {
            if chars[idx] == ch {
                score += 2;
                if idx > 0 && chars[idx - 1] == '-' {
                    score += 1;
                }
                idx += 1;
                found = true;
                break;
            }
            idx += 1;
        }
        if !found {
            return 0;
        }
    }
    score
}

fn pet_frame_token(
    settings: &crate::app::PetSettings,
    frame: usize,
    processing: bool,
) -> Option<String> {
    if !settings.enabled {
        return None;
    }
    let effective_mood = if processing && settings.mood != "sleepy" {
        "working"
    } else {
        settings.mood.as_str()
    };
    let frames: [&str; 2] = match (settings.species.as_str(), effective_mood) {
        ("boba", "sleepy") => ["(-_- )z", "(-_- )Z"],
        ("boba", "working") => ["(>_< )", "(<_< )"],
        ("boba", "hyped") => ["(o_o)!", "(!o_o)"],
        ("boba", "chill") => ["(u_u )", "(u_U )"],
        ("bytecat", "sleepy") => ["= -.-=z", "= -.-=Z"],
        ("bytecat", "working") => ["=^x^=", "=^_^="],
        ("bytecat", "hyped") => ["=^o^=!", "=^O^=!"],
        ("bytecat", "chill") => ["=^.^=~", "=^.-=~"],
        ("otter", "sleepy") => ["(>< )z", "(>< )Z"],
        ("otter", "working") => ["(>> )~", "(<< )~"],
        ("otter", "hyped") => ["(OO )~", "(oo )~"],
        ("otter", "chill") => ["(~~ )~", "(~_ )~"],
        ("fox", "sleepy") => ["{-- }z", "{-- }Z"],
        ("fox", "working") => ["{^x }", "{x^ }"],
        ("fox", "hyped") => ["{^^ }!", "{oo }!"],
        ("fox", "chill") => ["{.. }", "{._ }"],
        ("owl", "sleepy") => ["(v_v)z", "(v_v)Z"],
        ("owl", "working") => ["(O_O)", "(0_0)"],
        ("owl", "hyped") => ["(O0O)!", "(0O0)!"],
        ("owl", "chill") => ["(o_o)", "(o_O)"],
        ("capy", "sleepy") => ["(._.)z", "(._.)Z"],
        ("capy", "working") => ["(>_.)", "(._<)"],
        ("capy", "hyped") => ["(o_.)!", "(._o)!"],
        ("capy", "chill") => ["(._.)~", "(.._)~"],
        ("boba", _) => ["(o_o )", "(O_O )"],
        ("bytecat", _) => ["=^.^=", "=^o^="],
        ("otter", _) => ["(>< )~", "(~>< )"],
        ("fox", _) => ["{^.^}", "{^o^}"],
        ("owl", _) => ["(OvO)", "(oVo)"],
        ("capy", _) => ["(._.)", "(o_.)"],
        _ => ["(o_o )", "(O_O )"],
    };
    Some(frames[frame % frames.len()].to_string())
}

fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.modifiers
        .contains(crossterm::event::KeyModifiers::CONTROL)
        && key.code == crossterm::event::KeyCode::Char('c')
}

fn is_submit_shortcut(key: &KeyEvent, input: &str) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    let mods = key.modifiers;

    if key.code == KeyCode::Enter {
        if mods.contains(KeyModifiers::SHIFT) {
            return false;
        }
        if mods.is_empty()
            || mods.contains(KeyModifiers::CONTROL)
            || mods.contains(KeyModifiers::ALT)
        {
            // Slash commands stay single-line and are submitted with Enter.
            if input.starts_with('/') {
                return !input.contains('\n');
            }
            return true;
        }
    }

    // Fallback for terminals that encode Ctrl+Enter as Ctrl+M.
    key.code == KeyCode::Char('m') && mods.contains(KeyModifiers::CONTROL)
}

fn parse_slash_parts(input: &str) -> Option<(String, Vec<String>)> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let mut iter = trimmed.split_whitespace();
    let cmd = iter.next()?.to_string();
    let args = iter.map(ToString::to_string).collect::<Vec<_>>();
    Some((cmd, args))
}

fn provider_env_key_hints(provider: &str) -> &'static [&'static str] {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai" => &["HERMES_OPENAI_API_KEY", "OPENAI_API_KEY"],
        "openai-codex" | "codex" => &["HERMES_OPENAI_CODEX_API_KEY"],
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "nous" => &["NOUS_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "gemini" | "google" => &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
        "google-gemini-cli" => &["HERMES_GEMINI_OAUTH_API_KEY"],
        "qwen" => &["DASHSCOPE_API_KEY", "QWEN_API_KEY"],
        "qwen-oauth" => &["HERMES_QWEN_OAUTH_API_KEY", "DASHSCOPE_API_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "kimi" | "moonshot" | "kimi-coding" | "kimi-coding-cn" => &["KIMI_API_KEY"],
        "ollama-local" => &["OLLAMA_LOCAL_API_KEY", "OLLAMA_API_KEY"],
        "llama-cpp" => &["LLAMA_CPP_API_KEY"],
        "vllm" => &["VLLM_API_KEY"],
        "mlx" => &["MLX_API_KEY"],
        "apple-ane" => &["APPLE_ANE_API_KEY"],
        "sglang" => &["SGLANG_API_KEY"],
        "tgi" => &["TGI_API_KEY"],
        "zai" => &["ZAI_API_KEY"],
        "minimax" | "minimax-cn" => &["MINIMAX_API_KEY"],
        "stepfun" => &["HERMES_STEPFUN_API_KEY", "STEPFUN_API_KEY"],
        _ => &[],
    }
}

async fn load_token_store_providers() -> HashSet<String> {
    let path = hermes_config::paths::hermes_home()
        .join("auth")
        .join("tokens.json");
    let Ok(store) = FileTokenStore::new(path).await else {
        return HashSet::new();
    };
    store
        .list_providers()
        .await
        .into_iter()
        .map(|provider| provider.to_ascii_lowercase())
        .collect()
}

fn provider_auth_detail(provider: &str, token_store_providers: &HashSet<String>) -> String {
    let normalized = provider.trim().to_ascii_lowercase();
    let mut sources: Vec<String> = Vec::new();
    for key in provider_env_key_hints(&normalized) {
        if std::env::var(key)
            .ok()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
        {
            sources.push(format!("env:{key}"));
            break;
        }
    }
    if token_store_providers.contains(&normalized) {
        sources.push("vault".to_string());
    }
    if crate::auth::read_provider_auth_state(&normalized)
        .ok()
        .flatten()
        .is_some()
    {
        sources.push("oauth".to_string());
    }

    if sources.is_empty() {
        let setup_hint = provider_env_key_hints(&normalized)
            .first()
            .copied()
            .unwrap_or("API_KEY");
        format!("auth:missing (setup: /auth {normalized} or {setup_hint})")
    } else {
        format!("auth:{}", sources.join("+"))
    }
}

async fn disconnect_provider_credentials(provider: &str) -> Result<(bool, bool), AgentError> {
    let normalized = provider.trim().to_ascii_lowercase();
    let path = hermes_config::paths::hermes_home()
        .join("auth")
        .join("tokens.json");
    let mut removed_vault = false;
    if let Ok(store) = FileTokenStore::new(path).await {
        removed_vault = store
            .list_providers()
            .await
            .into_iter()
            .any(|p| p.eq_ignore_ascii_case(&normalized));
        let _ = store.remove(&normalized).await;
    }
    let removed_oauth = crate::auth::clear_provider_auth_state(&normalized).unwrap_or(false);
    Ok((removed_vault, removed_oauth))
}

async fn open_model_provider_modal(state: &mut TuiState, app: &App) {
    let providers = crate::model_switch::curated_provider_slugs();
    let entries = crate::model_switch::provider_catalog_entries(&providers, 4).await;
    let token_store_providers = load_token_store_providers().await;
    let mut items: Vec<PickerItem> = Vec::new();
    for provider in providers {
        let entry = entries
            .iter()
            .find(|entry| entry.provider.eq_ignore_ascii_case(provider));
        let auth_detail = provider_auth_detail(provider, &token_store_providers);
        let detail = if let Some(entry) = entry {
            if entry.models.is_empty() {
                format!("{} models • {}", entry.total_models, auth_detail)
            } else {
                format!(
                    "{} models • {} • {}",
                    entry.total_models,
                    entry.models.join(", "),
                    auth_detail
                )
            }
        } else {
            format!("catalog unavailable • {}", auth_detail)
        };
        items.push(PickerItem {
            label: provider.to_string(),
            detail,
            value: provider.to_string(),
        });
    }
    let mut modal = PickerModal::new(PickerKind::ModelProvider, "Select Provider", items);
    let (current_provider, _) = app
        .current_model
        .split_once(':')
        .unwrap_or(("openai", app.current_model.as_str()));
    if let Some(idx) = modal.filtered_indices.iter().position(|item_idx| {
        modal.items[*item_idx]
            .value
            .eq_ignore_ascii_case(current_provider)
    }) {
        modal.selected_filtered = idx;
    }
    state.open_modal(modal);
}

async fn open_provider_model_modal(state: &mut TuiState, app: &App, provider: &str) {
    let models = crate::model_switch::provider_model_ids(provider).await;
    if models.is_empty() {
        state.status_message = format!("No models available for provider `{provider}`");
        return;
    }
    let mut items = Vec::with_capacity(models.len());
    for model in models {
        items.push(PickerItem {
            label: model.clone(),
            detail: format!("{provider}:{model}"),
            value: model,
        });
    }
    let mut modal = PickerModal::new(
        PickerKind::ModelForProvider {
            provider: provider.to_string(),
        },
        format!("Select {provider} model"),
        items,
    );
    let (_, current_model_id) = app
        .current_model
        .split_once(':')
        .unwrap_or(("openai", app.current_model.as_str()));
    if let Some(idx) = modal.filtered_indices.iter().position(|item_idx| {
        modal.items[*item_idx]
            .value
            .eq_ignore_ascii_case(current_model_id)
    }) {
        modal.selected_filtered = idx;
    }
    state.open_modal(modal);
}

fn open_personality_modal(state: &mut TuiState, app: &App) {
    let descriptions = hermes_agent::builtin_personality_descriptions();
    let mut items = Vec::with_capacity(descriptions.len());
    for (name, detail) in descriptions {
        items.push(PickerItem {
            label: (*name).to_string(),
            detail: (*detail).to_string(),
            value: (*name).to_string(),
        });
    }
    let mut modal = PickerModal::new(PickerKind::Personality, "Select Personality", items);
    if let Some(current) = app.current_personality.as_deref() {
        if let Some(idx) = modal
            .filtered_indices
            .iter()
            .position(|item_idx| modal.items[*item_idx].value.eq_ignore_ascii_case(current))
        {
            modal.selected_filtered = idx;
        }
    }
    state.open_modal(modal);
}

fn open_skin_modal(state: &mut TuiState) {
    let mut items = Vec::with_capacity(crate::skin_engine::BUILTIN_SKINS.len());
    for (name, detail) in crate::skin_engine::BUILTIN_SKINS {
        items.push(PickerItem {
            label: (*name).to_string(),
            detail: (*detail).to_string(),
            value: (*name).to_string(),
        });
    }
    let mut modal = PickerModal::new(PickerKind::Skin, "Select Skin", items);
    let active = std::env::var("HERMES_THEME").unwrap_or_else(|_| "ultra-neon".to_string());
    let active_canonical = crate::skin_engine::canonical_skin_name(&active).unwrap_or("ultra-neon");
    if let Some(idx) = modal.filtered_indices.iter().position(|item_idx| {
        modal.items[*item_idx]
            .value
            .eq_ignore_ascii_case(active_canonical)
    }) {
        modal.selected_filtered = idx;
    }
    state.open_modal(modal);
}

async fn process_modal_disconnect(state: &mut TuiState, app: &mut App) -> Result<(), AgentError> {
    let Some(modal) = state.modal.clone() else {
        return Ok(());
    };
    let Some(item) = modal.selected_item().cloned() else {
        state.status_message = "No provider selected".to_string();
        return Ok(());
    };
    if !matches!(modal.kind, PickerKind::ModelProvider) {
        state.status_message = "Disconnect is only supported in provider picker".to_string();
        return Ok(());
    }
    let provider = item.value.trim().to_ascii_lowercase();
    match disconnect_provider_credentials(&provider).await {
        Ok((removed_vault, removed_oauth)) => {
            if removed_vault || removed_oauth {
                state.status_message = format!(
                    "Disconnected `{provider}` (vault={}, oauth={})",
                    removed_vault, removed_oauth
                );
                app.push_ui_assistant(format!(
                    "Disconnected provider `{}` (vault={}, oauth={}).",
                    provider, removed_vault, removed_oauth
                ));
            } else {
                state.status_message =
                    format!("No stored credential found for `{provider}` to disconnect");
            }
            open_model_provider_modal(state, app).await;
        }
        Err(err) => {
            state.status_message = format!("Disconnect failed for `{provider}`: {err}");
        }
    }
    Ok(())
}

async fn process_modal_confirm(state: &mut TuiState, app: &mut App) -> Result<(), AgentError> {
    let Some(modal) = state.modal.clone() else {
        return Ok(());
    };
    let Some(item) = modal.selected_item().cloned() else {
        state.status_message = "Nothing selected".to_string();
        return Ok(());
    };
    match modal.kind {
        PickerKind::ModelProvider => {
            open_provider_model_modal(state, app, &item.value).await;
            state.status_message = format!("Provider selected: {}", item.value);
        }
        PickerKind::ModelForProvider { provider } => {
            let provider_model = format!("{provider}:{}", item.value.trim());
            app.switch_model(&provider_model);
            app.push_ui_assistant(format!("Model switched to: {}", provider_model));
            state.close_modal();
            state.status_message = format!("Switched model to {}", provider_model);
        }
        PickerKind::Personality => {
            app.switch_personality(item.value.as_str());
            app.push_ui_assistant(format!("Switched personality to `{}`.", item.value));
            state.close_modal();
            state.status_message = format!("Personality: {}", item.value);
        }
        PickerKind::Skin => {
            let skin = crate::skin_engine::canonical_skin_name(item.value.as_str())
                .unwrap_or("ultra-neon")
                .to_string();
            std::env::set_var("HERMES_THEME", &skin);
            app.request_theme_change(&skin);
            app.push_ui_assistant(format!("Switched skin to `{}`.", skin));
            state.close_modal();
            state.status_message = format!("Skin: {}", skin);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Main TUI run loop
// ---------------------------------------------------------------------------

/// Run the interactive TUI with the given App.
///
/// This is the main entry point for the interactive TUI mode.
/// It sets up the terminal, renders frames, and handles events.
pub async fn run(mut app: App) -> Result<(), AgentError> {
    let mut tui = Tui::new().map_err(|e| AgentError::Config(e.to_string()))?;
    let mut state = TuiState::default();
    let mut last_jobs_refresh = Instant::now()
        .checked_sub(Duration::from_secs(2))
        .unwrap_or_else(Instant::now);
    let mut last_pet_tick = Instant::now();
    app.set_stream_handle(Some(StreamHandle::from(tui.event_sender())));

    // Spawn crossterm event reader
    let event_sender = tui.event_sender();
    let _event_task = tokio::spawn(async move {
        loop {
            if crate::checklist::embedded_picker_active() {
                tokio::time::sleep(Duration::from_millis(16)).await;
                continue;
            }
            if crossterm::event::poll(Duration::from_millis(100)).unwrap_or(false) {
                if let Ok(event) = crossterm::event::read() {
                    let msg = match event {
                        CrosstermEvent::Key(key) => Some(Event::Key(key)),
                        CrosstermEvent::Resize(w, h) => Some(Event::Resize(w, h)),
                        CrosstermEvent::Mouse(mouse) => Some(Event::Mouse(mouse)),
                        _ => None,
                    };
                    if let Some(msg) = msg {
                        let _ = event_sender.send(msg);
                    }
                }
            }
        }
    });

    let mut frame_tick = tokio::time::interval(Duration::from_millis(60));
    frame_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut needs_redraw = true;

    // Main event loop
    while app.running {
        if let Some(theme_name) = app.take_pending_theme_change() {
            let applied = crate::skin_engine::resolve_theme(&theme_name);
            tui.set_theme(applied);
            needs_redraw = true;
        }

        if needs_redraw {
            state.refresh_sticky_prompt(&app);
            let active_theme = tui.theme().clone();
            tui.terminal
                .draw(|f| {
                    render(f, &app, &mut state, &active_theme);
                })
                .map_err(|e| AgentError::Config(e.to_string()))?;
            needs_redraw = false;
        }

        tokio::select! {
            _ = frame_tick.tick() => {
                let previous_jobs = state.background_jobs_running;
                if last_jobs_refresh.elapsed() >= Duration::from_secs(1) {
                    state.background_jobs_running = app.running_background_job_count();
                    last_jobs_refresh = Instant::now();
                }
                if state.processing {
                    state.tick_spinner();
                    state.maybe_emit_progress_pulse();
                    needs_redraw = true;
                }
                if app.pet_settings().enabled
                    && last_pet_tick.elapsed()
                        >= Duration::from_millis(app.pet_settings().tick_ms.clamp(120, 2000))
                {
                    state.tick_pet();
                    last_pet_tick = Instant::now();
                    needs_redraw = true;
                }
                if previous_jobs != state.background_jobs_running {
                    needs_redraw = true;
                }
            }
            event = tui.events.recv() => {
                match event {
                    Some(Event::Key(key)) => {
                        // Ctrl+C always exits back to parent terminal. If work is in flight,
                        // emit interrupt first so in-progress tools can stop gracefully.
                        if is_ctrl_c(&key) {
                            if state.processing {
                                tui.event_sender().send(Event::Interrupt).ok();
                            }
                            app.running = false;
                            break;
                        }

                        if state.modal_active() {
                            match state.handle_modal_key(key) {
                                ModalAction::Close => {
                                    state.close_modal();
                                    state.status_message = "Picker closed".to_string();
                                }
                                ModalAction::Confirm => {
                                    process_modal_confirm(&mut state, &mut app).await?;
                                }
                                ModalAction::DisconnectProvider => {
                                    process_modal_disconnect(&mut state, &mut app).await?;
                                }
                                ModalAction::None => {}
                            }
                            needs_redraw = true;
                            continue;
                        }

                        let should_quit = state.handle_key(key, &mut app);
                        if should_quit {
                            app.running = false;
                            break;
                        }

                        let is_submit = is_submit_shortcut(&key, &state.input);

                        if is_submit {
                            let input = state.input.clone();
                            state.input.clear();
                            state.cursor_position = 0;
                            state.completions.clear();
                            state.completion_index = None;
                            state.scroll_offset = 0;

                            if !input.is_empty() {
                                let mut handled_by_tui = false;
                                if let Some((cmd, args)) = parse_slash_parts(&input) {
                                    if cmd.eq_ignore_ascii_case("/model") {
                                        if args.is_empty() || (args.len() == 1 && args[0].eq_ignore_ascii_case("list")) {
                                            open_model_provider_modal(&mut state, &app).await;
                                            state.status_message = "Choose provider, then model".to_string();
                                            handled_by_tui = true;
                                        } else if args.len() == 1 {
                                            let providers = crate::model_switch::curated_provider_slugs();
                                            if providers.iter().any(|p| p.eq_ignore_ascii_case(&args[0])) {
                                                open_provider_model_modal(&mut state, &app, &args[0].to_ascii_lowercase()).await;
                                                state.status_message = format!("Choose {} model", args[0].to_ascii_lowercase());
                                                handled_by_tui = true;
                                            }
                                        }
                                    } else if cmd.eq_ignore_ascii_case("/personality")
                                        && (args.is_empty() || (args.len() == 1 && args[0].eq_ignore_ascii_case("list")))
                                    {
                                        open_personality_modal(&mut state, &app);
                                        state.status_message = "Choose personality".to_string();
                                        handled_by_tui = true;
                                    } else if (cmd.eq_ignore_ascii_case("/skin")
                                        || cmd.eq_ignore_ascii_case("/skins"))
                                        && (args.is_empty()
                                            || (args.len() == 1
                                                && (args[0].eq_ignore_ascii_case("list")
                                                    || args[0].eq_ignore_ascii_case("status")
                                                    || args[0].eq_ignore_ascii_case("show"))))
                                    {
                                        open_skin_modal(&mut state);
                                        state.status_message = "Choose skin".to_string();
                                        handled_by_tui = true;
                                    } else if cmd.eq_ignore_ascii_case("/toolcards")
                                        && args.first().is_some_and(|a| a.eq_ignore_ascii_case("export"))
                                    {
                                        let export_path = hermes_config::hermes_home().join("logs/toolcards-export.txt");
                                        let mut out = String::new();
                                        for msg in app.transcript_messages().iter().filter(|m| m.role == hermes_core::MessageRole::Tool) {
                                            if let Some(content) = msg.content.as_deref() {
                                                out.push_str(content);
                                                out.push_str("\n\n---\n\n");
                                            }
                                        }
                                        if let Err(err) = std::fs::write(&export_path, out) {
                                            state.status_message = format!("Export failed: {}", err);
                                        } else {
                                            state.status_message = format!("Exported tool cards to {}", export_path.display());
                                            app.push_ui_assistant(format!("Exported tool cards to `{}`.", export_path.display()));
                                        }
                                        handled_by_tui = true;
                                    }
                                }

                                if !handled_by_tui {
                                    state.begin_processing_cycle(&app.current_model);
                                    state.status_message = "Processing...".to_string();
                                    match app.handle_input(&input).await {
                                        Ok(_) => {
                                            state.finish_processing_cycle("✔ completed in");
                                            state.status_message.clear();
                                        }
                                        Err(e) => {
                                            state.finish_processing_cycle("✖ failed after");
                                            state.status_message = format!("Error: {}", e);
                                            state.push_activity(format!("✖ {}", e));
                                            app.push_ui_assistant(format!("Error: {}", e));
                                        }
                                    }
                                }
                            }
                        }
                        needs_redraw = true;
                    }
                    Some(Event::Resize(_, _)) => {
                        needs_redraw = true;
                    }
                    Some(Event::Mouse(mouse)) => {
                        if !app.mouse_enabled() {
                            continue;
                        }
                        use crossterm::event::MouseEventKind;
                        match mouse.kind {
                            MouseEventKind::ScrollUp => {
                                state.scroll_offset = state.scroll_offset.saturating_add(3);
                            }
                            MouseEventKind::ScrollDown => {
                                state.scroll_offset = state.scroll_offset.saturating_sub(3);
                            }
                            _ => {}
                        }
                        needs_redraw = true;
                    }
                    Some(Event::Message(msg)) => {
                        state.status_message = msg;
                        needs_redraw = true;
                    }
                    Some(Event::StreamDelta(delta)) => {
                        state.stream_buffer.push_str(&delta);
                        needs_redraw = true;
                    }
                    Some(Event::StreamChunk(chunk)) => {
                        if let Some(delta) = chunk.delta {
                            let has_stream_payload = delta
                                .content
                                .as_ref()
                                .is_some_and(|text| !text.is_empty())
                                || delta
                                    .tool_calls
                                    .as_ref()
                                    .is_some_and(|calls| !calls.is_empty());
                            if has_stream_payload {
                                state.stream_chunk_count = state.stream_chunk_count.saturating_add(1);
                            }
                            if let Some(extra) = delta.extra.as_ref() {
                                if let Some(control) = extra.get("control").and_then(|v| v.as_str()) {
                                    if control == "mute_post_response" {
                                        state.stream_muted = extra
                                            .get("enabled")
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false);
                                    } else if control == "stream_break" {
                                        state.stream_needs_break = true;
                                    }
                                }
                                if let Some(ui_event) = extra.get("ui_event").and_then(|v| v.as_str()) {
                                    match ui_event {
                                        "tool_start" => {
                                            let tool = extra
                                                .get("tool")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("tool")
                                                .trim()
                                                .to_string();
                                            if !tool.is_empty()
                                                && !state.active_tools.iter().any(|t| t == &tool)
                                            {
                                                state.active_tools.push(tool.clone());
                                            }
                                            let args_preview = extra
                                                .get("args_preview")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .trim();
                                            if args_preview.is_empty() {
                                                state.push_activity(format!("▶ {}", tool));
                                            } else {
                                                state.push_activity(format!("▶ {} {}", tool, args_preview));
                                            }
                                        }
                                        "tool_complete" => {
                                            let tool = extra
                                                .get("tool")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("tool")
                                                .trim()
                                                .to_string();
                                            if let Some(idx) =
                                                state.active_tools.iter().position(|t| t == &tool)
                                            {
                                                state.active_tools.remove(idx);
                                            }
                                            let result_preview = extra
                                                .get("result_preview")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .trim();
                                            if result_preview.is_empty() {
                                                state.push_activity(format!("✓ {}", tool));
                                            } else {
                                                state.push_activity(format!(
                                                    "✓ {} {}",
                                                    tool, result_preview
                                                ));
                                            }
                                        }
                                        "status" => {
                                            let event_type = extra
                                                .get("event_type")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("status")
                                                .trim();
                                            let message = extra
                                                .get("message")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .trim();
                                            if !message.is_empty() {
                                                state.push_activity(format!(
                                                    "[{}] {}",
                                                    event_type, message
                                                ));
                                            }
                                        }
                                        "lifecycle" => {
                                            let message = extra
                                                .get("message")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .trim();
                                            if !message.is_empty() {
                                                state.push_activity(format!("⟡ {}", message));
                                            }
                                        }
                                        "thinking" => {
                                            if let Some(text) =
                                                extra.get("text").and_then(|v| v.as_str())
                                            {
                                                state.append_live_thinking(text);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                if let Some(thinking) = extra.get("thinking").and_then(|v| v.as_str()) {
                                    state.append_live_thinking(thinking);
                                }
                            }
                            if let Some(content) = delta.content {
                                if !state.stream_muted {
                                    if state.stream_needs_break {
                                        state.stream_buffer.push_str("\n\n");
                                        state.stream_needs_break = false;
                                    }
                                    state.stream_buffer.push_str(&content);
                                    state.stream_char_count =
                                        state.stream_char_count.saturating_add(content.chars().count());
                                    if !state.saw_first_token {
                                        state.saw_first_token = true;
                                        let first_token_ms = state
                                            .processing_started_at
                                            .map(|t| t.elapsed().as_millis())
                                            .unwrap_or_default();
                                        state.push_activity(format!(
                                            "↧ first token in {}ms",
                                            first_token_ms
                                        ));
                                    }
                                }
                            }
                        }
                        if let Some(usage) = chunk.usage {
                            state.last_usage =
                                Some((usage.prompt_tokens, usage.completion_tokens, usage.total_tokens));
                        }
                        needs_redraw = true;
                    }
                    Some(Event::AgentDone) => {
                        state.finish_processing_cycle("✔ completed in");
                        state.stream_buffer.clear();
                        state.stream_muted = false;
                        state.stream_needs_break = false;
                        state.active_tools.clear();
                        state.status_message.clear();
                        needs_redraw = true;
                    }
                    Some(Event::Interrupt) => {
                        state.finish_processing_cycle("⏹ interrupted after");
                        state.stream_buffer.clear();
                        state.stream_muted = false;
                        state.stream_needs_break = false;
                        state.active_tools.clear();
                        needs_redraw = true;
                    }
                    None => {
                        // Channel closed
                        break;
                    }
                }
            }
        }
    }

    // Restore terminal
    tui.restore()
        .map_err(|e| AgentError::Config(e.to_string()))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Streaming support (Requirement 9.5)
// ---------------------------------------------------------------------------

/// A handle for sending streaming deltas to the TUI.
///
/// Clone this and pass it to the agent loop's streaming callback.
/// The TUI will accumulate deltas and display them in real time.
#[derive(Clone)]
pub struct StreamHandle {
    sender: mpsc::UnboundedSender<Event>,
}

impl StreamHandle {
    /// Send a streaming text delta to the TUI.
    pub fn send_delta(&self, text: &str) {
        let _ = self.sender.send(Event::StreamDelta(text.to_string()));
    }

    /// Send a full streaming chunk to the TUI event loop.
    pub fn send_chunk(&self, chunk: StreamChunk) {
        let _ = self.sender.send(Event::StreamChunk(chunk));
    }

    /// Signal that the agent has finished.
    pub fn send_done(&self) {
        let _ = self.sender.send(Event::AgentDone);
    }
}

impl From<mpsc::UnboundedSender<Event>> for StreamHandle {
    fn from(sender: mpsc::UnboundedSender<Event>) -> Self {
        Self { sender }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::Message;

    #[test]
    fn test_input_mode_display() {
        assert_eq!(InputMode::Normal.to_string(), "NORMAL");
        assert_eq!(InputMode::Insert.to_string(), "INSERT");
        assert_eq!(InputMode::Command.to_string(), "COMMAND");
    }

    #[test]
    fn test_tui_state_default() {
        let state = TuiState::default();
        assert_eq!(state.mode, InputMode::Insert);
        assert!(state.input.is_empty());
        assert_eq!(state.cursor_position, 0);
        assert!(state.completions.is_empty());
        assert!(!state.processing);
        assert!(state.selection_anchor.is_none());
        assert!(!state.history_search_active);
    }

    #[test]
    fn test_spinner_char() {
        let mut state = TuiState::default();
        let c1 = state.spinner_char();
        state.tick_spinner();
        let c2 = state.spinner_char();
        assert_ne!(c1, c2);
    }

    #[test]
    fn test_tool_output_section() {
        let section = ToolOutputSection::new(
            "test_tool".to_string(),
            "line1\nline2\nline3\nline4\nline5".to_string(),
        );
        assert!(!section.is_expanded);
        let display = section.display_text();
        assert!(display.contains("line1"));
        assert!(display.contains("more lines"));
    }

    #[test]
    fn test_tui_state_completions_update() {
        let mut state = TuiState::default();
        state.input = "/mod".to_string();
        state.update_completions();
        assert!(state.completions.contains(&"/model".to_string()));
    }

    #[test]
    fn test_completion_popup_hidden_when_slash_deleted() {
        let mut state = TuiState::default();
        state.input = "/model".to_string();
        state.update_completions();
        assert!(should_render_completions_popup(&state));

        state.input.clear();
        state.refresh_completions();
        assert!(!should_render_completions_popup(&state));
        assert!(state.completions.is_empty());
    }

    #[test]
    fn test_completion_popup_hidden_when_modal_or_processing_active() {
        let mut state = TuiState::default();
        state.input = "/model".to_string();
        state.update_completions();
        assert!(should_render_completions_popup(&state));

        state.processing = true;
        assert!(!should_render_completions_popup(&state));
        state.processing = false;

        state.modal = Some(PickerModal::new(
            PickerKind::Personality,
            "personality",
            vec![PickerItem {
                label: "default".to_string(),
                detail: String::new(),
                value: "default".to_string(),
            }],
        ));
        assert!(!should_render_completions_popup(&state));
    }

    #[test]
    fn test_open_skin_modal_populates_builtin_skin_items() {
        let mut state = TuiState::default();
        open_skin_modal(&mut state);
        let modal = state.modal.as_ref().expect("skin modal");
        assert!(matches!(modal.kind, PickerKind::Skin));
        assert!(modal.items.iter().any(|item| item.value == "ultra-neon"));
        assert!(modal.items.iter().any(|item| item.value == "neon-glow"));
        assert!(modal
            .items
            .iter()
            .any(|item| item.value == "hyper-ultra-hyper-saturated"));
    }

    #[test]
    fn test_event_debug() {
        let event = Event::Message("hello".to_string());
        let debug_str = format!("{:?}", event);
        assert!(debug_str.contains("hello"));
    }

    #[test]
    fn test_activity_ring_buffer_caps_size() {
        let mut state = TuiState::default();
        for i in 0..30 {
            state.push_activity(format!("event-{i}"));
        }
        assert_eq!(state.recent_activity.len(), 16);
        assert_eq!(
            state.recent_activity.first().map(String::as_str),
            Some("event-14")
        );
        assert_eq!(
            state.recent_activity.last().map(String::as_str),
            Some("event-29")
        );
    }

    #[test]
    fn test_append_live_thinking_is_capped() {
        let mut state = TuiState::default();
        let long = "x".repeat(400);
        state.append_live_thinking(&long);
        assert!(state.live_thinking.chars().count() <= 260);
        assert!(state.live_thinking.starts_with('…'));
    }

    #[test]
    fn test_processing_cycle_tracks_and_resets_stats() {
        let mut state = TuiState::default();
        state.begin_processing_cycle("nous:test-model");
        assert!(state.processing);
        assert_eq!(state.stream_chunk_count, 0);
        assert_eq!(state.stream_char_count, 0);
        assert!(state.processing_started_at.is_some());
        assert!(state
            .recent_activity
            .last()
            .is_some_and(|line| line.contains("dispatching request")));

        state.stream_chunk_count = 7;
        state.stream_char_count = 1234;
        state.finish_processing_cycle("✔ completed in");

        assert!(!state.processing);
        assert_eq!(state.stream_chunk_count, 0);
        assert_eq!(state.stream_char_count, 0);
        assert!(state.processing_started_at.is_none());
        assert!(state
            .recent_activity
            .last()
            .is_some_and(|line| line.contains("✔ completed in")));
    }

    #[test]
    fn test_progress_pulse_emits_activity_row() {
        let mut state = TuiState::default();
        state.begin_processing_cycle("nous:test-model");
        state.processing_started_at = Some(Instant::now() - Duration::from_secs(2));
        state.last_progress_pulse_at = None;
        let before = state.recent_activity.len();
        state.maybe_emit_progress_pulse();
        assert!(state.recent_activity.len() > before);
        assert!(state
            .recent_activity
            .last()
            .is_some_and(|line| line.contains("working")));
    }

    #[test]
    fn test_stream_handle() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let handle: StreamHandle = tx.into();
        handle.send_delta("test delta");
        handle.send_done();
    }

    #[test]
    fn test_is_ctrl_c_detection() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let plain_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(is_ctrl_c(&ctrl_c));
        assert!(!is_ctrl_c(&plain_c));
    }

    #[test]
    fn test_submit_shortcuts_are_detected() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let plain_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let ctrl_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        let ctrl_m = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL);

        assert!(is_submit_shortcut(&plain_enter, "hello"));
        assert!(is_submit_shortcut(&ctrl_enter, "hello"));
        assert!(is_submit_shortcut(&alt_enter, "hello"));
        assert!(is_submit_shortcut(&ctrl_m, "hello"));
    }

    #[test]
    fn test_submit_shortcuts_exclude_newline_shortcuts() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        let ctrl_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);

        assert!(!is_submit_shortcut(&shift_enter, "hello"));
        assert!(!is_submit_shortcut(&ctrl_j, "hello"));
    }

    #[test]
    fn test_submit_shortcut_rejects_multiline_slash_commands() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let plain_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(!is_submit_shortcut(&plain_enter, "/model\nlist"));
    }

    #[test]
    fn test_insert_newline_at_cursor_updates_input_and_cursor() {
        let mut state = TuiState::default();
        state.input = "hello".to_string();
        state.cursor_position = 5;
        state.insert_newline_at_cursor();
        assert_eq!(state.input, "hello\n");
        assert_eq!(state.cursor_position, 6);
    }

    #[test]
    fn test_status_message_style_critical_for_error() {
        let colors = Theme::default_theme().colors.to_ratatui_colors();
        let style = status_message_style("Error: boom", &colors);
        assert_eq!(style.fg, Some(colors.status_bar_critical));
        assert_eq!(style.bg, Some(colors.status_bar_bg));
    }

    #[test]
    fn test_status_message_style_warn_for_warning() {
        let colors = Theme::default_theme().colors.to_ratatui_colors();
        let style = status_message_style("Warning: retrying", &colors);
        assert_eq!(style.fg, Some(colors.status_bar_warn));
        assert_eq!(style.bg, Some(colors.status_bar_bg));
    }

    #[test]
    fn test_pet_frame_token_hidden_when_disabled() {
        let settings = crate::app::PetSettings {
            enabled: false,
            ..crate::app::PetSettings::default()
        };
        assert!(pet_frame_token(&settings, 0, false).is_none());
    }

    #[test]
    fn test_pet_frame_token_returns_species_specific_frame() {
        let settings = crate::app::PetSettings {
            enabled: true,
            species: "fox".to_string(),
            mood: "ready".to_string(),
            dock: crate::app::PetDock::Right,
            tick_ms: 400,
        };
        let frame0 = pet_frame_token(&settings, 0, false).expect("frame");
        let frame1 = pet_frame_token(&settings, 1, false).expect("frame");
        assert_ne!(frame0, frame1);
        assert!(frame0.contains('{'));
    }

    #[test]
    fn test_transcript_hides_system_messages() {
        let theme = Theme::default_theme();
        let colors = theme.colors.to_ratatui_colors();
        let styles = theme.resolved_styles();
        let mut state = TuiState::default();
        let messages = vec![
            Message::system("internal system payload"),
            Message::user("reply with 1"),
            Message::assistant("1"),
        ];
        let rendered = build_transcript_lines(&messages, &mut state, &styles, &colors, 80);
        let rendered_text = rendered
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!rendered_text.contains("SYSTEM"));
        assert!(!rendered_text.contains("internal system payload"));
        assert!(rendered_text.contains("USER"));
        assert!(rendered_text.contains("HERMES"));
        assert!(rendered_text.contains("reply with 1"));
        assert!(rendered_text.contains("1"));
    }

    #[test]
    fn test_transcript_placeholder_shows_when_empty() {
        let theme = Theme::default_theme();
        let colors = theme.colors.to_ratatui_colors();
        let styles = theme.resolved_styles();
        let mut state = TuiState::default();
        let rendered = build_transcript_lines(&[], &mut state, &styles, &colors, 80);
        let rendered_text = rendered
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered_text.contains("Start chatting"));
    }

    #[test]
    fn test_count_renderable_messages_ignores_system() {
        let messages = vec![
            Message::system("hidden"),
            Message::user("u"),
            Message::assistant("a"),
        ];
        assert_eq!(count_renderable_messages(&messages), 2);
    }

    #[test]
    fn test_format_tool_message_lines_parses_json_payload() {
        let payload = r#"{"result":"line1\nline2","_budget_warning":"[BUDGET WARNING: Iteration 40/50.]","error":"boom"}"#;
        let lines = format_tool_message_lines(payload);
        let joined = lines.join("\n");
        assert!(joined.contains("⚠ [BUDGET WARNING"));
        assert!(joined.contains("[result]"));
        assert!(joined.contains("line1"));
        assert!(joined.contains("[error]"));
        assert!(joined.contains("boom"));
    }

    #[test]
    fn test_approximate_visual_rows_wraps_long_lines() {
        let lines = vec![Line::from("x".repeat(120))];
        assert_eq!(approximate_visual_rows(&lines, 40), 3);
        assert_eq!(approximate_visual_rows(&lines, 80), 2);
    }

    #[test]
    fn test_append_message_renderer_matches_full_builder() {
        let theme = Theme::default_theme();
        let colors = theme.colors.to_ratatui_colors();
        let styles = theme.resolved_styles();
        let messages = vec![Message::user("hello"), Message::assistant("world")];

        let mut full_state = TuiState::default();
        let full = build_transcript_lines(&messages, &mut full_state, &styles, &colors, 80);

        let mut inc_state = TuiState::default();
        let divider = transcript_divider(80);
        let mut lines = Vec::new();
        let mut rendered = 0usize;
        for (idx, msg) in messages.iter().enumerate() {
            append_transcript_message_lines(
                &mut lines,
                msg,
                idx,
                &mut rendered,
                &mut inc_state,
                &styles,
                &colors,
                &divider,
            );
        }

        let as_text =
            |v: &[Line<'static>]| -> Vec<String> { v.iter().map(Line::to_string).collect() };
        assert_eq!(as_text(&full), as_text(&lines));
    }
}
