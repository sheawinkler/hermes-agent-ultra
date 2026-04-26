//! Terminal UI using ratatui + crossterm (Requirement 9.1-9.6).
//!
//! Implements the interactive terminal interface with:
//! - Message history rendering (9.1, 9.4)
//! - Input area with slash command auto-completion (9.2)
//! - Ctrl+C interrupt for tool execution (9.3)
//! - Streaming output display (9.5)
//! - Status bar with model/session info (9.6)
//! - Theme/skin engine support (9.8)

use std::io::Stdout;
use std::time::Duration;

use crossterm::event::{Event as CrosstermEvent, KeyEvent};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use tokio::sync::mpsc;

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
        })
    }

    /// Restore the terminal to its original state.
    pub fn restore(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        disable_raw_mode()?;
        self.terminal.backend_mut().execute(LeaveAlternateScreen)?;
        self.terminal.show_cursor()?;
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
        }
    }
}

impl TuiState {
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
            // Ctrl+Enter or Alt+Enter → submit
            KeyCode::Enter
                if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) =>
            {
                // Submit is handled by the caller checking for this combo
                false
            }
            // Plain Enter → insert newline (multi-line editing)
            KeyCode::Enter => {
                self.input.insert(self.cursor_position, '\n');
                self.cursor_position += 1;
                self.selection_anchor = None;
                false
            }
            // Ctrl+A → move to beginning of line
            KeyCode::Char('a') if mods.contains(KeyModifiers::CONTROL) => {
                self.cursor_position = self.line_start();
                self.selection_anchor = None;
                false
            }
            // Ctrl+E → move to end of line
            KeyCode::Char('e') if mods.contains(KeyModifiers::CONTROL) => {
                self.cursor_position = self.line_end();
                self.selection_anchor = None;
                false
            }
            // Ctrl+V → paste from clipboard (best-effort via crossterm)
            KeyCode::Char('v') if mods.contains(KeyModifiers::CONTROL) => {
                // Bracketed paste is delivered as regular Char events in this input loop,
                // so no dedicated Ctrl+V action is required here.
                false
            }
            // Ctrl+R → toggle history search
            KeyCode::Char('r') if mods.contains(KeyModifiers::CONTROL) => {
                self.history_search_active = !self.history_search_active;
                if !self.history_search_active {
                    self.history_search_query.clear();
                }
                false
            }
            KeyCode::Home => {
                self.cursor_position = self.line_start();
                self.selection_anchor = None;
                false
            }
            KeyCode::End => {
                self.cursor_position = self.line_end();
                self.selection_anchor = None;
                false
            }
            // Ctrl/Alt+Left → jump backward by word
            KeyCode::Left
                if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) =>
            {
                self.cursor_position = self.word_start_left(self.cursor_position);
                self.selection_anchor = None;
                false
            }
            // Ctrl/Alt+Right → jump forward by word
            KeyCode::Right
                if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) =>
            {
                self.cursor_position = self.word_end_right(self.cursor_position);
                self.selection_anchor = None;
                false
            }
            // Ctrl+B / Ctrl+F → char-wise navigation
            KeyCode::Char('b') if mods.contains(KeyModifiers::CONTROL) => {
                self.cursor_position = self.prev_char_start(self.cursor_position);
                self.selection_anchor = None;
                false
            }
            KeyCode::Char('f') if mods.contains(KeyModifiers::CONTROL) => {
                self.cursor_position = self.next_char_start(self.cursor_position);
                self.selection_anchor = None;
                false
            }
            // Ctrl+W or Ctrl/Alt+Backspace → delete previous word
            KeyCode::Char('w') if mods.contains(KeyModifiers::CONTROL) => {
                let start = self.word_start_left(self.cursor_position);
                if start < self.cursor_position {
                    self.input.drain(start..self.cursor_position);
                    self.cursor_position = start;
                }
                self.refresh_completions();
                self.selection_anchor = None;
                false
            }
            KeyCode::Backspace
                if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) =>
            {
                let start = self.word_start_left(self.cursor_position);
                if start < self.cursor_position {
                    self.input.drain(start..self.cursor_position);
                    self.cursor_position = start;
                }
                self.refresh_completions();
                self.selection_anchor = None;
                false
            }
            // Ctrl+D/Delete → delete char under cursor
            KeyCode::Delete => {
                if self.cursor_position < self.input.len() {
                    let next = self.next_char_start(self.cursor_position);
                    self.input.drain(self.cursor_position..next);
                    self.refresh_completions();
                }
                self.selection_anchor = None;
                false
            }
            KeyCode::Char('d') if mods.contains(KeyModifiers::CONTROL) => {
                if self.cursor_position < self.input.len() {
                    let next = self.next_char_start(self.cursor_position);
                    self.input.drain(self.cursor_position..next);
                    self.refresh_completions();
                }
                self.selection_anchor = None;
                false
            }
            // Ctrl+U / Ctrl+K → delete to start/end of line
            KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => {
                let start = self.line_start();
                if start < self.cursor_position {
                    self.input.drain(start..self.cursor_position);
                    self.cursor_position = start;
                }
                self.refresh_completions();
                self.selection_anchor = None;
                false
            }
            KeyCode::Char('k') if mods.contains(KeyModifiers::CONTROL) => {
                let end = self.line_end();
                if self.cursor_position < end {
                    self.input.drain(self.cursor_position..end);
                }
                self.refresh_completions();
                self.selection_anchor = None;
                false
            }
            KeyCode::Char(c) => {
                if self.history_search_active {
                    self.history_search_query.push(c);
                    // Search through history
                    if let Some(found) = app
                        .input_history
                        .iter()
                        .rev()
                        .find(|h| h.contains(&self.history_search_query))
                    {
                        self.input = found.clone();
                        self.cursor_position = self.input.len();
                    }
                    return false;
                }
                self.input.insert(self.cursor_position, c);
                self.cursor_position += c.len_utf8();
                self.selection_anchor = None;
                self.refresh_completions();
                false
            }
            KeyCode::Backspace => {
                if self.history_search_active {
                    self.history_search_query.pop();
                    return false;
                }
                if self.cursor_position > 0 {
                    // Find the previous char boundary
                    let prev = self.input[..self.cursor_position]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    self.input.drain(prev..self.cursor_position);
                    self.cursor_position = prev;
                }
                self.selection_anchor = None;
                self.refresh_completions();
                false
            }
            KeyCode::Left => {
                if self.cursor_position > 0 {
                    // Move to previous char boundary
                    self.cursor_position = self.input[..self.cursor_position]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                }
                self.selection_anchor = None;
                false
            }
            KeyCode::Right => {
                if self.cursor_position < self.input.len() {
                    // Move to next char boundary
                    self.cursor_position = self.input[self.cursor_position..]
                        .char_indices()
                        .nth(1)
                        .map(|(i, _)| self.cursor_position + i)
                        .unwrap_or(self.input.len());
                }
                self.selection_anchor = None;
                false
            }
            KeyCode::Up => {
                // In multi-line: move cursor up one line
                if self.input.contains('\n') {
                    if let Some(new_pos) = self.cursor_up() {
                        self.cursor_position = new_pos;
                        return false;
                    }
                }
                // Single-line or at top: browse history
                if let Some(prev) = app.history_prev() {
                    self.input = prev.to_string();
                    self.cursor_position = self.input.len();
                }
                false
            }
            KeyCode::Down => {
                // In multi-line: move cursor down one line
                if self.input.contains('\n') {
                    if let Some(new_pos) = self.cursor_down() {
                        self.cursor_position = new_pos;
                        return false;
                    }
                }
                // Single-line or at bottom: browse history
                if let Some(next) = app.history_next() {
                    self.input = next.to_string();
                    self.cursor_position = self.input.len();
                }
                false
            }
            KeyCode::Tab => {
                // Accept completion
                if let Some(idx) = self.completion_index {
                    if idx < self.completions.len() {
                        self.input = self.completions[idx].clone();
                        self.cursor_position = self.input.len();
                    }
                } else if !self.completions.is_empty() {
                    self.input = self.completions[0].clone();
                    self.cursor_position = self.input.len();
                }
                self.completions.clear();
                self.completion_index = None;
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
            _ => false,
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

    // -----------------------------------------------------------------------
    // Multi-line cursor helpers
    // -----------------------------------------------------------------------

    fn prev_char_start(&self, pos: usize) -> usize {
        if pos == 0 {
            return 0;
        }
        self.input[..pos]
            .char_indices()
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn next_char_start(&self, pos: usize) -> usize {
        if pos >= self.input.len() {
            return self.input.len();
        }
        self.input[pos..]
            .char_indices()
            .nth(1)
            .map(|(i, _)| pos + i)
            .unwrap_or(self.input.len())
    }

    fn is_word_char(c: char) -> bool {
        c.is_alphanumeric() || matches!(c, '_' | '-')
    }

    fn word_start_left(&self, pos: usize) -> usize {
        let mut p = pos.min(self.input.len());
        while p > 0 {
            let prev = self.prev_char_start(p);
            let ch = self.input[prev..p].chars().next().unwrap_or_default();
            if Self::is_word_char(ch) {
                break;
            }
            p = prev;
        }
        while p > 0 {
            let prev = self.prev_char_start(p);
            let ch = self.input[prev..p].chars().next().unwrap_or_default();
            if !Self::is_word_char(ch) {
                break;
            }
            p = prev;
        }
        p
    }

    fn word_end_right(&self, pos: usize) -> usize {
        let mut p = pos.min(self.input.len());
        while p < self.input.len() {
            let next = self.next_char_start(p);
            let ch = self.input[p..next].chars().next().unwrap_or_default();
            if Self::is_word_char(ch) {
                break;
            }
            p = next;
        }
        while p < self.input.len() {
            let next = self.next_char_start(p);
            let ch = self.input[p..next].chars().next().unwrap_or_default();
            if !Self::is_word_char(ch) {
                break;
            }
            p = next;
        }
        p
    }

    /// Get the byte offset of the start of the current line.
    fn line_start(&self) -> usize {
        self.input[..self.cursor_position]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0)
    }

    /// Get the byte offset of the end of the current line.
    fn line_end(&self) -> usize {
        self.input[self.cursor_position..]
            .find('\n')
            .map(|i| self.cursor_position + i)
            .unwrap_or(self.input.len())
    }

    /// Column offset within the current line.
    fn current_column(&self) -> usize {
        self.cursor_position - self.line_start()
    }

    /// Move cursor up one line, returning the new byte offset or None if at top.
    fn cursor_up(&self) -> Option<usize> {
        let line_start = self.line_start();
        if line_start == 0 {
            return None; // already on first line
        }
        let col = self.current_column();
        // Previous line ends at line_start - 1 (the '\n')
        let prev_line_end = line_start - 1;
        let prev_line_start = self.input[..prev_line_end]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let prev_line_len = prev_line_end - prev_line_start;
        Some(prev_line_start + col.min(prev_line_len))
    }

    /// Move cursor down one line, returning the new byte offset or None if at bottom.
    fn cursor_down(&self) -> Option<usize> {
        let line_end = self.line_end();
        if line_end >= self.input.len() {
            return None; // already on last line
        }
        let col = self.current_column();
        let next_line_start = line_end + 1;
        let next_line_end = self.input[next_line_start..]
            .find('\n')
            .map(|i| next_line_start + i)
            .unwrap_or(self.input.len());
        let next_line_len = next_line_end - next_line_start;
        Some(next_line_start + col.min(next_line_len))
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
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render the full TUI frame.
pub fn render(frame: &mut Frame, app: &App, state: &TuiState, theme: &Theme) {
    let resolved = theme.resolved_styles();
    let colors = theme.colors.to_ratatui_colors();

    let size = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(colors.background)),
        size,
    );

    // Layout: header, messages, completions (optional), input, status bar
    let header_height = 2;
    let composer_lines = state.input.matches('\n').count() as u16 + 1;
    let input_height = (composer_lines + 2).clamp(3, 6);
    let completion_rows = state.completions.len().min(6) as u16;
    let completion_height = if state.completions.is_empty() {
        0
    } else {
        completion_rows + 2
    };
    let status_height = 1;

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),     // header
            Constraint::Min(4),                    // messages
            Constraint::Length(completion_height), // completions
            Constraint::Length(input_height),      // input
            Constraint::Length(status_height),     // status
        ])
        .split(size);

    let header_area = vertical[0];
    let messages_area = vertical[1];
    let completions_area = vertical[2];
    let input_area = vertical[3];
    let status_area = vertical[4];

    render_header(frame, app, header_area, &colors);

    // --- Render message history ---
    render_messages(frame, app, state, messages_area, &resolved, &colors);

    // --- Render completions ---
    if !state.completions.is_empty() {
        render_completions(
            frame,
            &state.completions,
            state.completion_index,
            completions_area,
            &colors,
        );
    }

    // --- Render input area ---
    if let Some(pos) = render_input(frame, state, input_area, &colors) {
        frame.set_cursor_position(pos);
    }

    // --- Render status bar ---
    render_status(frame, app, state, status_area, &colors);
}

fn render_header(frame: &mut Frame, app: &App, area: Rect, colors: &crate::theme::RatatuiColors) {
    let session_short = &app.session_id[..8.min(app.session_id.len())];
    let text = Text::from(vec![
        Line::from(vec![
            Span::styled(
                " HERMES ",
                Style::default()
                    .fg(Color::Black)
                    .bg(colors.status_bar_good)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " AGENT ",
                Style::default()
                    .fg(Color::Black)
                    .bg(colors.warning)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " ULTRA ",
                Style::default()
                    .fg(Color::Black)
                    .bg(colors.status_bar_strong)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("   SESSION {session_short}"),
                Style::default()
                    .fg(colors.status_bar_text)
                    .bg(colors.status_bar_bg),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  Ctrl+Enter send",
                Style::default()
                    .fg(colors.status_bar_text)
                    .bg(colors.status_bar_bg),
            ),
            Span::styled(
                "  •  Ctrl/Alt+←/→ word nav",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.status_bar_bg),
            ),
            Span::styled(
                "  •  Ctrl+W delete word",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.status_bar_bg),
            ),
            Span::styled(
                "  •  PgUp/PgDn scroll",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.status_bar_bg),
            ),
        ]),
    ]);
    let title = Paragraph::new(text)
        .block(Block::default().style(Style::default().bg(colors.status_bar_bg)));
    frame.render_widget(title, area);
}

/// Render the message history area.
fn role_visuals(
    role: hermes_core::MessageRole,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
) -> (&'static str, &'static str, Style, Style) {
    match role {
        hermes_core::MessageRole::User => (
            "◆",
            "USER",
            styles.user_input,
            styles.user_input.remove_modifier(Modifier::BOLD),
        ),
        hermes_core::MessageRole::Assistant => (
            "●",
            "HERMES",
            styles.assistant_response,
            styles.assistant_response,
        ),
        hermes_core::MessageRole::System => {
            ("◇", "SYSTEM", styles.system_message, styles.system_message)
        }
        hermes_core::MessageRole::Tool => (
            "◈",
            "TOOL",
            styles.tool_call,
            Style::default().fg(colors.status_bar_text),
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

fn render_assistant_markdown_lines(
    content: &str,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
) -> Vec<Line<'static>> {
    let mut rendered: Vec<Line<'static>> = Vec::new();
    let mut in_code_block = false;
    let mut code_lang = String::new();
    let code_frame_style = Style::default().fg(colors.status_bar_dim);
    let code_text_style = Style::default().fg(colors.status_bar_text);
    let heading_style = Style::default()
        .fg(colors.status_bar_strong)
        .add_modifier(Modifier::BOLD);
    let bullet_style = Style::default()
        .fg(colors.accent)
        .add_modifier(Modifier::BOLD);
    let quote_style = Style::default()
        .fg(colors.status_bar_dim)
        .add_modifier(Modifier::ITALIC);
    let inline_code_style = Style::default()
        .fg(colors.accent)
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
            rendered.push(Line::from(vec![
                Span::styled("    │ ", code_frame_style),
                Span::styled(raw.to_string(), code_text_style),
            ]));
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
                        Style::default().fg(colors.status_bar_dim),
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
                Span::styled(body.to_string(), styles.assistant_response),
            ]));
            continue;
        }

        if let Some((marker, body)) = parse_markdown_numbered_marker(trimmed) {
            rendered.push(Line::from(vec![
                Span::styled(format!("    {marker} "), bullet_style),
                Span::styled(body.to_string(), styles.assistant_response),
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

fn build_transcript_lines(
    messages: &[hermes_core::Message],
    state: &TuiState,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut rendered_messages = 0usize;

    for msg in messages {
        // Hide internal orchestration/system payloads from the chat transcript.
        if matches!(msg.role, hermes_core::MessageRole::System) {
            continue;
        }
        if rendered_messages > 0 {
            lines.push(Line::from(String::new()));
        }
        rendered_messages += 1;
        let (glyph, label, label_style, body_style) = role_visuals(msg.role, styles, colors);
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", glyph),
                label_style.add_modifier(Modifier::BOLD),
            ),
            Span::styled(label.to_string(), label_style.add_modifier(Modifier::BOLD)),
        ]));

        if let Some(content) = msg.content.as_deref() {
            match msg.role {
                hermes_core::MessageRole::Assistant => {
                    lines.extend(render_assistant_markdown_lines(content, styles, colors));
                }
                hermes_core::MessageRole::Tool => {
                    let all_lines: Vec<&str> = content.lines().collect();
                    for line in all_lines.iter().take(8) {
                        lines.push(render_inline_with_code(
                            "    ",
                            line,
                            styles.tool_result,
                            Style::default()
                                .fg(colors.accent)
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                    if all_lines.len() > 8 {
                        lines.push(Line::from(vec![Span::styled(
                            format!("    … {} more lines", all_lines.len() - 8),
                            Style::default().fg(colors.status_bar_dim),
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
            if let Some(reasoning) = msg
                .reasoning_content
                .as_ref()
                .filter(|s| !s.trim().is_empty())
            {
                lines.push(Line::from(vec![Span::styled(
                    "    🤔 reasoning",
                    Style::default().fg(colors.status_bar_dim),
                )]));
                for line in reasoning.lines() {
                    lines.push(Line::from(vec![Span::styled(
                        format!("      {}", line.trim_end()),
                        Style::default()
                            .fg(colors.status_bar_dim)
                            .add_modifier(Modifier::ITALIC),
                    )]));
                }
            }
            if let Some(tool_calls) = msg.tool_calls.as_ref() {
                for tc in tool_calls {
                    let args = serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                        .unwrap_or_else(|_| serde_json::Value::Null);
                    let preview = build_tool_preview_from_value(&tc.function.name, &args, 44)
                        .unwrap_or_default();
                    let emoji = tool_emoji(&tc.function.name);
                    let summary = if preview.is_empty() {
                        format!("{emoji} {}", tc.function.name)
                    } else {
                        format!("{emoji} {} {}", tc.function.name, preview)
                    };
                    lines.push(Line::from(vec![
                        Span::styled("    ↳ ", Style::default().fg(colors.status_bar_dim)),
                        Span::styled(summary, styles.tool_call),
                    ]));
                }
            }
        }
    }

    // Streaming buffer (partial assistant response)
    if !state.stream_buffer.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::from(String::new()));
        }
        lines.push(Line::from(vec![
            Span::styled(
                " ● ",
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
                .add_modifier(Modifier::BOLD),
        )]));
    }

    if lines.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            " Start chatting — your messages and Hermes replies will appear here.",
            Style::default()
                .fg(colors.status_bar_dim)
                .add_modifier(Modifier::ITALIC),
        )]));
    }
    lines
}

fn render_messages(
    frame: &mut Frame,
    app: &App,
    state: &TuiState,
    area: Rect,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
) {
    let lines = build_transcript_lines(&app.messages, state, styles, colors);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Conversation ")
        .border_style(Style::default().fg(colors.status_bar_dim));
    let inner = block.inner(area);
    if inner.width == 0 || inner.height == 0 {
        frame.render_widget(block, area);
        return;
    }

    let viewport_rows = usize::from(inner.height.max(1));
    let max_hidden_from_bottom = lines.len().saturating_sub(viewport_rows);
    let hidden_from_bottom = usize::from(state.scroll_offset).min(max_hidden_from_bottom);
    let end = lines.len().saturating_sub(hidden_from_bottom);
    let start = end.saturating_sub(viewport_rows);
    let mut visible_lines: Vec<Line<'static>> = lines[start..end].to_vec();
    if visible_lines.len() < viewport_rows {
        let pad = viewport_rows - visible_lines.len();
        let mut padded = Vec::with_capacity(viewport_rows);
        padded.extend((0..pad).map(|_| Line::from(String::new())));
        padded.extend(visible_lines);
        visible_lines = padded;
    }

    let paragraph = Paragraph::new(Text::from(visible_lines))
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

/// Render the auto-completion suggestions.
fn render_completions(
    frame: &mut Frame,
    completions: &[String],
    selected: Option<usize>,
    area: Rect,
    colors: &crate::theme::RatatuiColors,
) {
    let inner_rows = usize::from(area.height.saturating_sub(2));
    if inner_rows == 0 {
        return;
    }
    let mut start = 0usize;
    if let Some(sel) = selected {
        if sel >= inner_rows {
            start = sel + 1 - inner_rows;
        }
    }
    let end = (start + inner_rows).min(completions.len());
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
                Style::default().fg(colors.status_bar_dim)
            };
            Line::from(Span::styled(cmd.clone(), style))
        })
        .collect();

    let paragraph = Paragraph::new(Text::from(items))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(colors.status_bar_dim))
                .title(" Slash Completions "),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

/// Render the input area (supports multi-line display with wrapping).
fn render_input(
    frame: &mut Frame,
    state: &TuiState,
    area: Rect,
    colors: &crate::theme::RatatuiColors,
) -> Option<Position> {
    let mode_indicator = match state.mode {
        InputMode::Normal => " NORMAL ",
        InputMode::Insert => " INSERT ",
        InputMode::Command => " CMD ",
    };

    let mode_style = match state.mode {
        InputMode::Normal => Style::default().fg(Color::White).bg(Color::DarkGray),
        InputMode::Insert => Style::default().fg(Color::Black).bg(colors.success),
        InputMode::Command => Style::default().fg(Color::Black).bg(colors.accent),
    };

    let history_prefix = if state.history_search_active {
        format!("(reverse-i-search)`{}': ", state.history_search_query)
    } else {
        String::new()
    };
    let show_placeholder =
        state.input.is_empty() && state.mode == InputMode::Insert && !state.history_search_active;
    let input_text = if show_placeholder {
        "Type a message and press Ctrl+Enter to send".to_string()
    } else {
        format!("{history_prefix}{}", state.input)
    };
    let input_body_style = if show_placeholder {
        Style::default()
            .fg(colors.status_bar_dim)
            .add_modifier(Modifier::ITALIC)
    } else {
        Style::default().fg(colors.foreground)
    };

    // For multi-line, show line count indicator
    let line_count = state.input.matches('\n').count() + 1;
    let line_indicator = if line_count > 1 {
        format!(" L{}", line_count)
    } else {
        String::new()
    };
    let line_indicator_width = line_indicator.chars().count();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Composer ")
        .border_style(Style::default().fg(colors.status_bar_strong));
    let prompt_glyph = "›";
    let paragraph = Paragraph::new(Text::from(vec![Line::from(vec![
        Span::styled(mode_indicator, mode_style),
        Span::styled(line_indicator, Style::default().fg(colors.status_bar_dim)),
        Span::styled(" │ ", Style::default().fg(colors.status_bar_dim)),
        Span::styled(
            prompt_glyph,
            Style::default()
                .fg(colors.status_bar_strong)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(input_text, input_body_style),
    ])]))
    .block(block.clone())
    .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);

    if state.mode == InputMode::Normal {
        return None;
    }

    let inner = block.inner(area);
    if inner.width == 0 || inner.height == 0 {
        return None;
    }

    let mut rendered_before_cursor = String::new();
    rendered_before_cursor.push_str(&history_prefix);
    rendered_before_cursor.push_str(&state.input[..state.cursor_position.min(state.input.len())]);

    let width = inner.width as usize;
    let prefix_width =
        mode_indicator.chars().count() + line_indicator_width + " │ ".chars().count() + 2;
    let mut x = prefix_width;
    let mut y = 0usize;

    for ch in rendered_before_cursor.chars() {
        if ch == '\n' {
            y += 1;
            x = 0;
            continue;
        }
        if x >= width {
            y += 1;
            x = 0;
        }
        x += 1;
    }

    if x >= width {
        y += x / width;
        x %= width;
    }
    if y >= inner.height as usize {
        return None;
    }

    Some(Position {
        x: inner.x + x as u16,
        y: inner.y + y as u16,
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
    let msg_count = app.messages.len();
    let scroll_hint = if state.scroll_offset > 0 {
        format!(" (history +{})", state.scroll_offset)
    } else {
        String::new()
    };

    let status_message_style = status_message_style(&state.status_message, colors);

    let status_bar = Paragraph::new(Line::from(vec![
        Span::styled(
            format!(" {} {} ", processing_indicator, state.mode),
            Style::default()
                .fg(colors.status_bar_strong)
                .bg(colors.status_bar_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "◆ ",
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.status_bar_bg),
        ),
        Span::styled(
            format!("Model: {} ", model),
            Style::default()
                .fg(colors.status_bar_text)
                .bg(colors.status_bar_bg),
        ),
        Span::styled(
            "◆ ",
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.status_bar_bg),
        ),
        Span::styled(
            format!("Session: {} ", session),
            Style::default()
                .fg(colors.status_bar_text)
                .bg(colors.status_bar_bg),
        ),
        Span::styled(
            "◆ ",
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.status_bar_bg),
        ),
        Span::styled(
            format!("Messages: {} ", msg_count),
            Style::default()
                .fg(colors.status_bar_good)
                .bg(colors.status_bar_bg),
        ),
        Span::styled(
            "◆ ",
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.status_bar_bg),
        ),
        Span::styled(
            format!("{}{}", state.status_message, scroll_hint),
            status_message_style,
        ),
    ]));

    frame.render_widget(status_bar, area);
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
    app.set_stream_handle(Some(StreamHandle::from(tui.event_sender())));

    // Spawn crossterm event reader
    let event_sender = tui.event_sender();
    let _event_task = tokio::spawn(async move {
        loop {
            if crossterm::event::poll(Duration::from_millis(100)).unwrap_or(false) {
                if let Ok(event) = crossterm::event::read() {
                    let msg = match event {
                        CrosstermEvent::Key(key) => Some(Event::Key(key)),
                        CrosstermEvent::Resize(w, h) => Some(Event::Resize(w, h)),
                        _ => None,
                    };
                    if let Some(msg) = msg {
                        let _ = event_sender.send(msg);
                    }
                }
            }
        }
    });

    // Main event loop
    while app.running {
        // Render
        let active_theme = tui.theme().clone();
        tui.terminal
            .draw(|f| {
                render(f, &app, &state, &active_theme);
            })
            .map_err(|e| AgentError::Config(e.to_string()))?;

        // Handle events
        tokio::select! {
            event = tui.events.recv() => {
                match event {
                    Some(Event::Key(key)) => {
                        // Handle Ctrl+C interrupt (Requirement 9.3)
                        if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                            && key.code == crossterm::event::KeyCode::Char('c')
                        {
                            if state.processing {
                                // Interrupt current tool execution
                                state.processing = false;
                                state.stream_buffer.clear();
                                state.status_message = "Interrupted".to_string();
                                tui.event_sender().send(Event::Interrupt).ok();
                            } else {
                                // Exit on second Ctrl+C
                                app.running = false;
                                break;
                            }
                        } else {
                            let should_quit = state.handle_key(key, &mut app);
                            if should_quit {
                                app.running = false;
                                break;
                            }

                            // Ctrl+Enter or Alt+Enter submits the input
                            let is_submit = (key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                                || key.modifiers.contains(crossterm::event::KeyModifiers::ALT))
                                && key.code == crossterm::event::KeyCode::Enter;

                            if is_submit {
                                let input = state.input.clone();
                                state.input.clear();
                                state.cursor_position = 0;
                                state.scroll_offset = 0;

                                if !input.is_empty() {
                                    state.processing = true;
                                    state.status_message = "Processing...".to_string();

                                    // Re-render before processing
                                    let active_theme = tui.theme().clone();
                                    tui.terminal
                                        .draw(|f| {
                                            render(f, &app, &state, &active_theme);
                                        })
                                        .map_err(|e| AgentError::Config(e.to_string()))?;

                                    match app.handle_input(&input).await {
                                        Ok(_) => {
                                            state.processing = false;
                                            state.status_message.clear();
                                        }
                                        Err(e) => {
                                            state.processing = false;
                                            state.status_message = format!("Error: {}", e);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Some(Event::Resize(_, _)) => {
                        // Terminal was resized — next render will adapt
                    }
                    Some(Event::Message(msg)) => {
                        state.status_message = msg;
                    }
                    Some(Event::StreamDelta(delta)) => {
                        state.stream_buffer.push_str(&delta);
                    }
                    Some(Event::StreamChunk(chunk)) => {
                        if let Some(delta) = chunk.delta {
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
                            }
                            if let Some(content) = delta.content {
                                if !state.stream_muted {
                                    if state.stream_needs_break {
                                        state.stream_buffer.push_str("\n\n");
                                        state.stream_needs_break = false;
                                    }
                                    state.stream_buffer.push_str(&content);
                                }
                            }
                        }
                    }
                    Some(Event::AgentDone) => {
                        state.processing = false;
                        state.stream_buffer.clear();
                        state.stream_muted = false;
                        state.stream_needs_break = false;
                        state.status_message.clear();
                    }
                    Some(Event::Interrupt) => {
                        state.processing = false;
                        state.stream_buffer.clear();
                        state.stream_muted = false;
                        state.stream_needs_break = false;
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
    fn test_multiline_cursor_helpers() {
        let mut state = TuiState::default();
        state.input = "line1\nline2\nline3".to_string();
        // Cursor at start of line2 (byte 6)
        state.cursor_position = 6;
        assert_eq!(state.line_start(), 6);
        assert_eq!(state.line_end(), 11);
        assert_eq!(state.current_column(), 0);

        // Move up should go to line1
        let up = state.cursor_up();
        assert_eq!(up, Some(0));

        // Cursor at middle of line2
        state.cursor_position = 8; // "li" of line2
        assert_eq!(state.current_column(), 2);
        let down = state.cursor_down();
        assert_eq!(down, Some(14)); // "li" of line3
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
    fn test_event_debug() {
        let event = Event::Message("hello".to_string());
        let debug_str = format!("{:?}", event);
        assert!(debug_str.contains("hello"));
    }

    #[test]
    fn test_stream_handle() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let handle: StreamHandle = tx.into();
        handle.send_delta("test delta");
        handle.send_done();
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
    fn test_word_navigation_helpers() {
        let mut state = TuiState::default();
        state.input = "alpha beta  gamma".to_string();

        assert_eq!(state.word_start_left(state.input.len()), 12); // gamma
        assert_eq!(state.word_start_left(11), 6); // beta
        assert_eq!(state.word_end_right(0), 5); // alpha
        assert_eq!(state.word_end_right(6), 10); // beta
    }

    #[test]
    fn test_ctrl_w_delete_previous_word_behavior() {
        let mut state = TuiState::default();
        state.input = "hello brave new world".to_string();
        state.cursor_position = state.input.len();

        let start = state.word_start_left(state.cursor_position);
        state.input.drain(start..state.cursor_position);
        state.cursor_position = start;
        assert_eq!(state.input, "hello brave new ");
    }

    #[test]
    fn test_transcript_hides_system_messages() {
        let theme = Theme::default_theme();
        let colors = theme.colors.to_ratatui_colors();
        let styles = theme.resolved_styles();
        let state = TuiState::default();
        let messages = vec![
            Message::system("internal system payload"),
            Message::user("reply with 1"),
            Message::assistant("1"),
        ];
        let rendered = build_transcript_lines(&messages, &state, &styles, &colors);
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
        let state = TuiState::default();
        let rendered = build_transcript_lines(&[], &state, &styles, &colors);
        let rendered_text = rendered
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered_text.contains("Start chatting"));
    }
}
