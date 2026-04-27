//! Terminal UI using ratatui + crossterm (Requirement 9.1-9.6).
//!
//! Implements the interactive terminal interface with:
//! - Message history rendering (9.1, 9.4)
//! - Input area with slash command auto-completion (9.2)
//! - Ctrl+C immediate exit back to parent terminal (with interrupt signal) (9.3)
//! - Streaming output display (9.5)
//! - Status bar with model/session info (9.6)
//! - Theme/skin engine support (9.8)

use std::io::Stdout;
use std::time::Duration;

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
            // Submit is handled by the caller checking for these combos.
            KeyCode::Enter
                if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) =>
            {
                false
            }
            // Slash commands submit on plain Enter in the run-loop.
            KeyCode::Enter if self.input.starts_with('/') && !self.input.contains('\n') => false,
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
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render the full TUI frame.
pub fn render(frame: &mut Frame, app: &App, state: &TuiState, theme: &Theme) {
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

    // Layout: header, messages, input, status bar
    let header_height = 1;
    let composer_lines = state.input.matches('\n').count() as u16 + 1;
    let input_height = (composer_lines + 2).clamp(3, 6);
    let status_height = 1;

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height), // header
            Constraint::Min(5),                // messages
            Constraint::Length(input_height),  // input
            Constraint::Length(status_height), // status
        ])
        .split(size);

    let header_area = vertical[0];
    let messages_area = vertical[1];
    let input_area = vertical[2];
    let status_area = vertical[3];

    render_header(frame, app, header_area, &colors);

    // --- Render message history ---
    render_messages(frame, app, state, messages_area, &resolved, &colors);

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

    // --- Render status bar ---
    render_status(frame, app, state, status_area, &colors);
}

fn should_render_completions_popup(state: &TuiState) -> bool {
    state.mode != InputMode::Normal
        && state.input.starts_with('/')
        && !state.input.contains('\n')
        && !state.history_search_active
        && !state.completions.is_empty()
}

fn render_header(frame: &mut Frame, app: &App, area: Rect, colors: &crate::theme::RatatuiColors) {
    let session_short = &app.session_id[..8.min(app.session_id.len())];
    let title = format!(
        " HERMES AGENT ULTRA  •  session {}  •  Ctrl+Enter send  •  / for commands",
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
    let code_text_style = Style::default()
        .fg(colors.status_bar_text)
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

fn build_transcript_lines(
    messages: &[hermes_core::Message],
    state: &TuiState,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
    content_width: u16,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut rendered_messages = 0usize;
    let divider = transcript_divider(content_width);

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
                format!(" ╭ {} ", glyph),
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
        lines.push(Line::from(vec![Span::styled(
            divider.clone(),
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background),
        )]));
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
    state: &TuiState,
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
    let lines = build_transcript_lines(&transcript, state, styles, colors, inner.width);

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

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);

    if lines.len() > viewport_rows {
        let mut scrollbar_state = ScrollbarState::new(lines.len())
            .position(start)
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
                format!("  •  L{} ", line_count),
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
        textarea.set_placeholder_text("Type a message and press Ctrl+Enter to send");
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
    let msg_count = app.messages.len();
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
    if !state.status_message.is_empty() || !scroll_hint.is_empty() {
        status_text.push_str(" | ");
        status_text.push_str(&state.status_message);
        status_text.push_str(&scroll_hint);
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

fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
        && key.code == crossterm::event::KeyCode::Char('c')
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
                        // Ctrl+C always exits back to parent terminal. If work is in flight,
                        // emit interrupt first so in-progress tools can stop gracefully.
                        if is_ctrl_c(&key) {
                            if state.processing {
                                tui.event_sender().send(Event::Interrupt).ok();
                            }
                            app.running = false;
                            break;
                        } else {
                            let should_quit = state.handle_key(key, &mut app);
                            if should_quit {
                                app.running = false;
                                break;
                            }

                            // Ctrl+Enter / Alt+Enter submits. For slash commands, Enter submits too.
                            let is_submit_combo = (key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                                || key.modifiers.contains(crossterm::event::KeyModifiers::ALT))
                                && key.code == crossterm::event::KeyCode::Enter;
                            let is_slash_enter = key.code == crossterm::event::KeyCode::Enter
                                && key.modifiers.is_empty()
                                && state.input.starts_with('/')
                                && !state.input.contains('\n');
                            let is_submit = is_submit_combo || is_slash_enter;

                            if is_submit {
                                let input = state.input.clone();
                                state.input.clear();
                                state.cursor_position = 0;
                                state.completions.clear();
                                state.completion_index = None;
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
                                            app.push_ui_assistant(format!("Error: {}", e));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Some(Event::Resize(_, _)) => {
                        // Terminal was resized — next render will adapt
                    }
                    Some(Event::Mouse(mouse)) => {
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
    fn test_is_ctrl_c_detection() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let plain_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(is_ctrl_c(&ctrl_c));
        assert!(!is_ctrl_c(&plain_c));
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
        let rendered = build_transcript_lines(&messages, &state, &styles, &colors, 80);
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
        let rendered = build_transcript_lines(&[], &state, &styles, &colors, 80);
        let rendered_text = rendered
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered_text.contains("Start chatting"));
    }
}
