
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
    pub scroll_offset: usize,
    /// Keep transcript pinned to newest content unless user scrolls away.
    auto_follow_transcript: bool,
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
    /// Last total-token value emitted into activity lane.
    last_usage_total_emitted: Option<u64>,
    /// Monotonic sequence for activity-timeline rows.
    timeline_seq: u64,
    /// Sticky prompt hint shown while scrolling history.
    pub sticky_prompt: String,
    /// Number of queued/running background jobs.
    pub background_jobs_running: usize,
    /// Number of active in-process sub-agents from lineage records.
    pub active_subagents_running: usize,
    /// Whether the right-side live activity lane is open.
    pub activity_lane_open: bool,
    /// Right-side lane mode.
    pub activity_lane_mode: ActivityLaneMode,
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
    /// Current structured phase name for active processing cycle.
    processing_phase: String,
    /// Current structured phase label for active processing cycle.
    processing_phase_label: String,
    /// Current structured phase progress percentage (0..=100).
    processing_phase_progress: u8,
    /// Whether the current cycle used fallback/remediation paths.
    processing_degraded: bool,
    /// Degraded lifecycle notes captured during current cycle.
    degraded_notes: Vec<String>,
    /// Stream finished, waiting for `AgentRunComplete` to commit transcript.
    awaiting_run_complete: bool,
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
            auto_follow_transcript: true,
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
            last_usage_total_emitted: None,
            timeline_seq: 0,
            sticky_prompt: String::new(),
            background_jobs_running: 0,
            active_subagents_running: 0,
            activity_lane_open: true,
            activity_lane_mode: ActivityLaneMode::Live,
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
            processing_phase: String::new(),
            processing_phase_label: String::new(),
            processing_phase_progress: 0,
            processing_degraded: false,
            degraded_notes: Vec::new(),
            awaiting_run_complete: false,
        }
    }
}

impl TuiState {
    fn scroll_history_up(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(usize::from(lines.max(1)));
        self.auto_follow_transcript = false;
    }

    fn scroll_history_down(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(usize::from(lines.max(1)));
        if self.scroll_offset == 0 {
            self.auto_follow_transcript = true;
        }
    }

    fn jump_to_latest(&mut self) {
        self.scroll_offset = 0;
        self.auto_follow_transcript = true;
    }

    fn jump_to_oldest(&mut self) {
        // Render path clamps this to current transcript max hidden rows.
        self.scroll_offset = usize::MAX;
        self.auto_follow_transcript = false;
    }

    fn push_activity(&mut self, text: impl Into<String>) {
        let trimmed = text.into().trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        self.timeline_seq = self.timeline_seq.saturating_add(1);
        self.recent_activity
            .push(format!("{:02}. {}", self.timeline_seq, trimmed));
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
        if crate::commands::reasoning_full_enabled() {
            return;
        }
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
        let _ = hermes_core::credits::yield_current_nous_credits_flash_notice();
        self.processing = true;
        self.awaiting_run_complete = true;
        self.processing_started_at = Some(Instant::now());
        self.last_progress_pulse_at = None;
        self.stream_chunk_count = 0;
        self.stream_char_count = 0;
        self.saw_first_token = false;
        self.processing_degraded = false;
        self.degraded_notes.clear();
        self.stream_buffer.clear();
        self.stream_muted = false;
        self.stream_needs_break = false;
        self.active_tools.clear();
        self.last_usage_total_emitted = None;
        self.timeline_seq = 0;
        self.live_thinking.clear();
        self.processing_phase = "preflight".to_string();
        self.processing_phase_label = "preparing request".to_string();
        self.processing_phase_progress = 0;
        self.push_activity(format!("⟳ dispatching request to {model}"));
    }

    fn mark_blocking_action(&mut self, label: impl AsRef<str>) {
        let label = truncate_chars(label.as_ref().trim(), 100);
        if label.is_empty() {
            return;
        }
        self.processing_phase = "command".to_string();
        self.processing_phase_label = label.clone();
        self.processing_phase_progress = self.processing_phase_progress.max(5);
        self.push_activity(format!("◈ {}", label));
        self.maybe_emit_progress_pulse();
    }

    fn finish_processing_cycle(&mut self, label: &str) {
        if !self.processing {
            return;
        }
        let resolved_label = if self.processing_degraded && label.starts_with('✔') {
            "⚠ completed with fallback in"
        } else {
            label
        };
        let elapsed = self
            .processing_started_at
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or_default();
        self.push_activity(format!(
            "{} {:.2}s • {} chunks • {} chars",
            resolved_label, elapsed, self.stream_chunk_count, self.stream_char_count
        ));
        if self.processing_degraded && !self.degraded_notes.is_empty() {
            self.push_activity(format!(
                "fallback notes: {}",
                truncate_chars(&self.degraded_notes.join(" | "), 220)
            ));
        }
        self.processing = false;
        self.processing_started_at = None;
        self.last_progress_pulse_at = None;
        self.stream_chunk_count = 0;
        self.stream_char_count = 0;
        self.saw_first_token = false;
        self.processing_phase.clear();
        self.processing_phase_label.clear();
        self.processing_phase_progress = 0;
        self.processing_degraded = false;
        self.degraded_notes.clear();
        self.awaiting_run_complete = false;
        self.jump_to_latest();
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
        let phase_state = if self.processing_phase_label.is_empty() {
            "phase: n/a".to_string()
        } else {
            format!(
                "phase: {} ({}%)",
                truncate_chars(&self.processing_phase_label, 64),
                self.processing_phase_progress
            )
        };
        self.push_activity(format!(
            "… working {:.1}s • {} chunks • {} chars • {} • {}",
            elapsed, self.stream_chunk_count, self.stream_char_count, tool_state, phase_state
        ));
        self.last_progress_pulse_at = Some(now);
    }

    fn processing_elapsed(&self) -> Duration {
        self.processing_started_at
            .map(|started| started.elapsed())
            .unwrap_or_default()
    }

    fn processing_stage_label(&self) -> &'static str {
        if !self.processing {
            return "idle";
        }
        if !self.processing_phase_label.is_empty() {
            return "phase-driven";
        }
        if !self.saw_first_token {
            if self.active_tools.is_empty() {
                "awaiting first token"
            } else {
                "running tools (pre-token)"
            }
        } else if self.active_tools.is_empty() {
            "streaming response"
        } else {
            "running tools + streaming"
        }
    }

    fn update_processing_phase(&mut self, phase: &str, label: &str, progress_pct: Option<u8>) {
        if !self.processing {
            return;
        }
        let phase = phase.trim().to_ascii_lowercase();
        let label = label.trim();
        if phase.is_empty() && label.is_empty() && progress_pct.is_none() {
            return;
        }
        if !phase.is_empty() {
            self.processing_phase = phase;
        }
        if !label.is_empty() {
            self.processing_phase_label = truncate_chars(label, 120);
        }
        if let Some(progress) = progress_pct {
            self.processing_phase_progress = progress.min(100);
        }
        let activity_label = if self.processing_phase_label.is_empty() {
            self.processing_phase.as_str()
        } else {
            self.processing_phase_label.as_str()
        };
        self.push_activity(format!(
            "◈ phase {}% • {}",
            self.processing_phase_progress, activity_label
        ));
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
        let is_interactive_question = matches!(modal.kind, PickerKind::InteractiveQuestion { .. });
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
            KeyCode::Backspace if !is_interactive_question => {
                modal.query.pop();
                modal.refresh_filter();
                ModalAction::None
            }
            KeyCode::Char('u')
                if key.modifiers.contains(KeyModifiers::CONTROL) && !is_interactive_question =>
            {
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
                if (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
                    && !is_interactive_question =>
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
                self.scroll_history_up(8);
            }
            KeyCode::PageDown => {
                self.scroll_history_down(8);
            }
            KeyCode::Home => {
                self.jump_to_oldest();
            }
            KeyCode::End => {
                self.jump_to_latest();
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
        if self.handle_insert_control_chord(key) {
            return false;
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
                self.scroll_history_up(8);
                false
            }
            KeyCode::PageDown => {
                self.scroll_history_down(8);
                false
            }
            KeyCode::Home => {
                self.jump_to_oldest();
                false
            }
            KeyCode::End if mods.contains(KeyModifiers::CONTROL) => {
                self.jump_to_latest();
                false
            }
            KeyCode::End => {
                self.jump_to_latest();
                false
            }
            // Fine-grained transcript scroll.
            KeyCode::Up if mods.contains(KeyModifiers::CONTROL) => {
                self.scroll_history_up(1);
                false
            }
            KeyCode::Down if mods.contains(KeyModifiers::CONTROL) => {
                self.scroll_history_down(1);
                false
            }
            // Fallback fine-grained scroll chords when terminals reserve Ctrl+Up/Down.
            KeyCode::Up
                if mods.contains(KeyModifiers::ALT) || mods.contains(KeyModifiers::SHIFT) =>
            {
                self.scroll_history_up(1);
                false
            }
            KeyCode::Down
                if mods.contains(KeyModifiers::ALT) || mods.contains(KeyModifiers::SHIFT) =>
            {
                self.scroll_history_down(1);
                false
            }
            // Force refresh + pin to newest transcript content.
            KeyCode::Char('g') if mods.contains(KeyModifiers::CONTROL) => {
                self.jump_to_latest();
                self.transcript_cache = TranscriptCache::default();
                self.status_message = "Jumped to latest transcript (forced refresh)".to_string();
                false
            }
            // Explicit multiline shortcuts.
            KeyCode::Enter if mods.contains(KeyModifiers::SHIFT) => {
                self.insert_newline_at_cursor();
                self.selection_anchor = None;
                self.refresh_completions_for_app(Some(app));
                false
            }
            KeyCode::Char('j') if mods.contains(KeyModifiers::CONTROL) => {
                self.insert_newline_at_cursor();
                self.selection_anchor = None;
                self.refresh_completions_for_app(Some(app));
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
                self.refresh_completions_for_app(Some(app));
                false
            }
            KeyCode::Down
                if !self.input.contains('\n') && !completion_nav_active && mods.is_empty() =>
            {
                if let Some(next) = app.history_next() {
                    self.input = next.to_string();
                    self.cursor_position = self.input.len();
                }
                self.refresh_completions_for_app(Some(app));
                false
            }
            KeyCode::Esc => {
                if self.history_search_active {
                    self.history_search_active = false;
                    self.history_search_query.clear();
                    return false;
                }
                if !self.input.is_empty() {
                    self.input.clear();
                    self.cursor_position = 0;
                    self.selection_anchor = None;
                }
                self.completions.clear();
                self.completion_index = None;
                if self.scroll_offset > 0 {
                    self.jump_to_latest();
                }
                // Keep insert mode so Esc never appears to "freeze" typing.
                self.mode = InputMode::Insert;
                false
            }
            _ => {
                self.apply_editor_input(key);
                self.selection_anchor = None;
                self.refresh_completions_for_app(Some(app));
                false
            }
        }
    }

    fn handle_insert_control_chord(&mut self, key: KeyEvent) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};
        if !key.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        match key.code {
            KeyCode::Char('l') => {
                self.activity_lane_open = !self.activity_lane_open;
                self.status_message = if self.activity_lane_open {
                    "Activity lane enabled".to_string()
                } else {
                    "Activity lane hidden".to_string()
                };
                true
            }
            KeyCode::Char('o') => {
                self.activity_lane_mode = match self.activity_lane_mode {
                    ActivityLaneMode::Live => ActivityLaneMode::Cockpit,
                    ActivityLaneMode::Cockpit => ActivityLaneMode::Live,
                };
                self.status_message = match self.activity_lane_mode {
                    ActivityLaneMode::Live => "Activity lane mode: live".to_string(),
                    ActivityLaneMode::Cockpit => "Activity lane mode: ops cockpit".to_string(),
                };
                true
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
                true
            }
            KeyCode::Char('t') => {
                self.show_timestamps = !self.show_timestamps;
                self.status_message = if self.show_timestamps {
                    "Timestamps visible".to_string()
                } else {
                    "Timestamps hidden".to_string()
                };
                true
            }
            KeyCode::Char('e') => {
                if self.expanded_tool_cards.insert("__all__".to_string()) {
                    self.status_message = "Expanded tool cards".to_string();
                } else {
                    self.expanded_tool_cards.remove("__all__");
                    self.status_message = "Collapsed tool cards".to_string();
                }
                true
            }
            KeyCode::Left => {
                self.move_cursor_word_left();
                true
            }
            KeyCode::Right => {
                self.move_cursor_word_right();
                true
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
    #[cfg(test)]
    fn update_completions(&mut self) {
        self.update_completions_for_app(None);
    }

    fn update_completions_for_app(&mut self, app: Option<&App>) {
        if self.input.starts_with('/') {
            self.completions = match app {
                Some(app) => commands::autocomplete_contextual_for_app(&self.input, app),
                None => commands::autocomplete_contextual(&self.input),
            };
            self.completion_index = None;
        } else {
            self.completions.clear();
            self.completion_index = None;
        }
    }

    fn refresh_completions(&mut self) {
        self.refresh_completions_for_app(None);
    }

    fn refresh_completions_for_app(&mut self, app: Option<&App>) {
        if self.input.starts_with('/') {
            self.update_completions_for_app(app);
        } else {
            self.completions.clear();
            self.completion_index = None;
        }
    }

    fn clamp_char_boundary(input: &str, cursor_byte: usize) -> usize {
        let mut clamped = cursor_byte.min(input.len());
        while clamped > 0 && !input.is_char_boundary(clamped) {
            clamped = clamped.saturating_sub(1);
        }
        clamped
    }

    fn cursor_row_col(input: &str, cursor_byte: usize) -> (usize, usize) {
        let clamped = Self::clamp_char_boundary(input, cursor_byte);
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

    fn input_line_text(&self) -> Vec<Line<'static>> {
        if self.input.is_empty() {
            vec![Line::from(String::new())]
        } else {
            self.input
                .split('\n')
                .map(|line| Line::from(line.to_string()))
                .collect()
        }
    }

    fn move_cursor_left(&mut self) {
        let at = Self::clamp_char_boundary(&self.input, self.cursor_position);
        self.cursor_position = self.input[..at]
            .char_indices()
            .next_back()
            .map(|(idx, _)| idx)
            .unwrap_or(0);
    }

    fn move_cursor_right(&mut self) {
        let at = Self::clamp_char_boundary(&self.input, self.cursor_position);
        self.cursor_position = self.input[at..]
            .chars()
            .next()
            .map(|ch| at + ch.len_utf8())
            .unwrap_or(at);
    }

    fn move_cursor_vertical(&mut self, delta: isize) {
        let (row, col) = Self::cursor_row_col(&self.input, self.cursor_position);
        let next_row = if delta.is_negative() {
            row.saturating_sub(delta.unsigned_abs())
        } else {
            row.saturating_add(delta as usize)
        };
        self.cursor_position = Self::row_col_to_byte_offset(&self.input, next_row, col);
    }

    fn delete_before_cursor(&mut self) {
        let at = Self::clamp_char_boundary(&self.input, self.cursor_position);
        if let Some((prev, _)) = self.input[..at].char_indices().next_back() {
            self.input.replace_range(prev..at, "");
            self.cursor_position = prev;
        }
    }

    fn delete_after_cursor(&mut self) {
        let at = Self::clamp_char_boundary(&self.input, self.cursor_position);
        if let Some(ch) = self.input[at..].chars().next() {
            self.input.replace_range(at..at + ch.len_utf8(), "");
            self.cursor_position = at;
        }
    }

    fn insert_char_at_cursor(&mut self, ch: char) {
        let at = Self::clamp_char_boundary(&self.input, self.cursor_position);
        self.input.insert(at, ch);
        self.cursor_position = at + ch.len_utf8();
    }

    fn move_cursor_to_line_start(&mut self) {
        let (row, _) = Self::cursor_row_col(&self.input, self.cursor_position);
        self.cursor_position = Self::row_col_to_byte_offset(&self.input, row, 0);
    }

    fn move_cursor_to_line_end(&mut self) {
        let (row, _) = Self::cursor_row_col(&self.input, self.cursor_position);
        self.cursor_position = Self::row_col_to_byte_offset(&self.input, row, usize::MAX);
    }

    fn apply_editor_input(&mut self, key: KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        match key.code {
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.insert_char_at_cursor(ch);
            }
            KeyCode::Backspace => self.delete_before_cursor(),
            KeyCode::Delete => self.delete_after_cursor(),
            KeyCode::Left => self.move_cursor_left(),
            KeyCode::Right => self.move_cursor_right(),
            KeyCode::Up => self.move_cursor_vertical(-1),
            KeyCode::Down => self.move_cursor_vertical(1),
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_cursor_to_line_start();
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_cursor_to_line_end();
            }
            _ => {}
        }
    }

    fn insert_newline_at_cursor(&mut self) {
        let at = Self::clamp_char_boundary(&self.input, self.cursor_position);
        self.input.insert(at, '\n');
        self.cursor_position = at.saturating_add(1);
    }

    fn insert_paste_at_cursor(&mut self, pasted: &str) {
        let normalized = pasted.replace("\r\n", "\n").replace('\r', "\n");
        if normalized.is_empty() {
            return;
        }
        let at = Self::clamp_char_boundary(&self.input, self.cursor_position);
        self.input.insert_str(at, &normalized);
        self.cursor_position = at.saturating_add(normalized.len());
        self.selection_anchor = None;
        self.refresh_completions();
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

