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
use ratatui::layout::{Constraint, Direction, Layout, Rect};
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
        Ok(Self {
            terminal,
            events: event_receiver,
            event_sender,
            theme: Theme::default_theme(),
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
    /// Scroll offset for the message history.
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
                // Check for slash command auto-completion
                if self.input.starts_with('/') {
                    self.update_completions();
                }
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
                if self.input.starts_with('/') {
                    self.update_completions();
                } else {
                    self.completions.clear();
                    self.completion_index = None;
                }
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

    // -----------------------------------------------------------------------
    // Multi-line cursor helpers
    // -----------------------------------------------------------------------

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
pub fn render(frame: &mut Frame, app: &App, state: &TuiState) {
    let theme = &Theme::default_theme(); // In real use, get from Tui
    let resolved = theme.resolved_styles();
    let colors = theme.colors.to_ratatui_colors();

    let size = frame.area();

    // Layout: messages (top), input (middle), completions (optional), status bar (bottom)
    let input_height = 3;
    let completion_height = if state.completions.is_empty() {
        0
    } else {
        state.completions.len() as u16 + 2
    };
    let status_height = 1;

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),                    // messages
            Constraint::Length(completion_height), // completions
            Constraint::Length(input_height),      // input
            Constraint::Length(status_height),     // status
        ])
        .split(size);

    let messages_area = vertical[0];
    let completions_area = vertical[1];
    let input_area = vertical[2];
    let status_area = vertical[3];

    // --- Render message history ---
    render_messages(frame, app, state, messages_area, &resolved, &colors);

    // --- Render completions ---
    if !state.completions.is_empty() {
        render_completions(
            frame,
            &state.completions,
            state.completion_index,
            completions_area,
        );
    }

    // --- Render input area ---
    render_input(frame, state, input_area, &colors);

    // --- Render status bar ---
    render_status(frame, app, state, status_area, &colors);
}

/// Render the message history area.
fn render_messages(
    frame: &mut Frame,
    app: &App,
    state: &TuiState,
    area: Rect,
    styles: &crate::theme::ResolvedStyles,
    _colors: &crate::theme::RatatuiColors,
) {
    let mut lines: Vec<Line> = Vec::new();

    for msg in &app.messages {
        match msg.role {
            hermes_core::MessageRole::User => {
                if let Some(ref content) = msg.content {
                    for line in content.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("You: {}", line),
                            styles.user_input,
                        )));
                    }
                }
            }
            hermes_core::MessageRole::Assistant => {
                if let Some(ref content) = msg.content {
                    for line in content.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("Assistant: {}", line),
                            styles.assistant_response,
                        )));
                    }
                }
                // Show tool calls if present
                if let Some(ref tool_calls) = msg.tool_calls {
                    for tc in tool_calls {
                        let args =
                            serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                                .unwrap_or_else(|_| serde_json::Value::Null);
                        let preview = build_tool_preview_from_value(&tc.function.name, &args, 40)
                            .unwrap_or_default();
                        let emoji = tool_emoji(&tc.function.name);
                        let label = if preview.is_empty() {
                            format!("  [Tool: {emoji} {}]", tc.function.name)
                        } else {
                            format!("  [Tool: {emoji} {} {preview}]", tc.function.name)
                        };
                        lines.push(Line::from(Span::styled(label, styles.tool_call)));
                    }
                }
            }
            hermes_core::MessageRole::Tool => {
                if let Some(ref content) = msg.content {
                    let preview: String = content.chars().take(200).collect();
                    lines.push(Line::from(Span::styled(
                        format!("  Tool result: {}", preview),
                        styles.tool_result,
                    )));
                }
            }
            hermes_core::MessageRole::System => {
                if let Some(ref content) = msg.content {
                    for line in content.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("[System] {}", line),
                            styles.system_message,
                        )));
                    }
                }
            }
        }
    }

    // Streaming buffer (partial assistant response)
    if !state.stream_buffer.is_empty() {
        for line in state.stream_buffer.lines() {
            lines.push(Line::from(Span::styled(
                format!("Assistant: {}", line),
                styles.assistant_response,
            )));
        }
    }

    let paragraph = Paragraph::new(Text::from(lines))
        .block(Block::default().borders(Borders::NONE))
        .wrap(Wrap { trim: false })
        .scroll((state.scroll_offset, 0));

    frame.render_widget(paragraph, area);
}

/// Render the auto-completion suggestions.
fn render_completions(
    frame: &mut Frame,
    completions: &[String],
    selected: Option<usize>,
    area: Rect,
) {
    let items: Vec<Line> = completions
        .iter()
        .enumerate()
        .map(|(i, cmd)| {
            let style = if selected == Some(i) {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Line::from(Span::styled(cmd.clone(), style))
        })
        .collect();

    let paragraph =
        Paragraph::new(Text::from(items)).block(Block::default().borders(Borders::NONE));
    frame.render_widget(paragraph, area);
}

/// Render the input area (supports multi-line display with wrapping).
fn render_input(
    frame: &mut Frame,
    state: &TuiState,
    area: Rect,
    colors: &crate::theme::RatatuiColors,
) {
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

    let input_text = if state.input.is_empty() && state.mode == InputMode::Insert {
        if state.history_search_active {
            format!("(reverse-i-search)`{}': ", state.history_search_query)
        } else {
            "Type a message (Enter=newline, Ctrl+Enter=send)...".to_string()
        }
    } else if state.history_search_active {
        format!(
            "(reverse-i-search)`{}': {}",
            state.history_search_query, state.input
        )
    } else {
        state.input.clone()
    };

    // For multi-line, show line count indicator
    let line_count = state.input.matches('\n').count() + 1;
    let line_indicator = if line_count > 1 {
        format!(" L{}", line_count)
    } else {
        String::new()
    };

    let paragraph = Paragraph::new(Text::from(vec![Line::from(vec![
        Span::styled(mode_indicator, mode_style),
        Span::styled(line_indicator, Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::raw(input_text),
    ])]))
    .block(Block::default().borders(Borders::BOTTOM))
    .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
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

    let status_text = format!(
        " {} {} │ Model: {} │ Session: {} │ Messages: {} │ {}",
        processing_indicator, state.mode, model, session, msg_count, state.status_message,
    );

    let status_bar = Paragraph::new(Line::from(Span::styled(
        status_text,
        Style::default().fg(colors.foreground).bg(colors.primary),
    )));

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
        tui.terminal
            .draw(|f| {
                render(f, &app, &state);
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

                                if !input.is_empty() {
                                    state.processing = true;
                                    state.status_message = "Processing...".to_string();

                                    // Re-render before processing
                                    tui.terminal.draw(|f| {
                                        render(f, &app, &state);
                                    }).map_err(|e| AgentError::Config(e.to_string()))?;

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
}
