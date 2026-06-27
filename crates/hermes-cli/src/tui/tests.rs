#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env_lock;
    use hermes_core::Message;
    use unicode_width::UnicodeWidthStr;

    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        test_env_lock::lock()
    }

    struct ReasoningFullGuard;

    impl ReasoningFullGuard {
        fn set(enabled: bool) -> Self {
            crate::commands::set_reasoning_full(enabled);
            Self
        }
    }

    impl Drop for ReasoningFullGuard {
        fn drop(&mut self) {
            crate::commands::set_reasoning_full(false);
        }
    }

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
        assert_eq!(state.active_subagents_running, 0);
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
    fn test_submit_shortcut_accepts_raw_cr_lf_fallbacks() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let raw_cr = KeyEvent::new(KeyCode::Char('\r'), KeyModifiers::empty());
        let raw_lf = KeyEvent::new(KeyCode::Char('\n'), KeyModifiers::empty());

        assert!(is_submit_shortcut(&raw_cr, "/quit"));
        assert!(is_submit_shortcut(&raw_lf, "/quit"));
    }

    #[test]
    fn test_ctrl_c_accepts_raw_etx_fallback() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let raw_etx = KeyEvent::new(KeyCode::Char('\u{3}'), KeyModifiers::empty());
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

        assert!(is_ctrl_c(&raw_etx));
        assert!(is_ctrl_c(&ctrl_c));
    }

    #[test]
    fn test_managed_route_when_quorum_armed() {
        let messages: Vec<Message> = Vec::new();
        assert!(should_route_prompt_via_managed_agent(true, &messages));
    }

    #[test]
    fn test_managed_route_when_quorum_hint_present() {
        let messages = vec![Message::system(
            "[QUORUM_MODE] Quorum reasoning is enabled for multi-voter fanout",
        )];
        assert!(should_route_prompt_via_managed_agent(false, &messages));
    }

    #[test]
    fn test_background_route_without_quorum_state() {
        let messages = vec![Message::system("normal system message")];
        assert!(!should_route_prompt_via_managed_agent(false, &messages));
    }

    #[test]
    fn test_background_completion_events_clear_task_handle() {
        let event = Event::AgentRunComplete {
            result: Err("stopped".to_string()),
            elapsed_secs: 1.0,
        };
        assert!(stream_event_completes_background_task(&event));
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
        assert!(state
            .recent_activity
            .first()
            .is_some_and(|line| line.ends_with("event-14")));
        assert!(state
            .recent_activity
            .last()
            .is_some_and(|line| line.ends_with("event-29")));
    }

    #[test]
    fn test_fit_status_line_pads_and_respects_display_width() {
        let fitted = fit_status_line("ok", 6);
        assert_eq!(UnicodeWidthStr::width(fitted.as_str()), 6);
        assert!(fitted.starts_with("ok"));

        let wide = fit_status_line("界abc", 4);
        assert_eq!(UnicodeWidthStr::width(wide.as_str()), 4);
        assert!(wide.starts_with('界'));
    }

    #[test]
    fn test_append_live_thinking_is_capped() {
        let _guard = env_test_lock();
        let _reasoning_guard = ReasoningFullGuard::set(false);
        let mut state = TuiState::default();
        let long = "x".repeat(400);
        state.append_live_thinking(&long);
        assert!(state.live_thinking.chars().count() <= 260);
        assert!(state.live_thinking.starts_with('…'));
    }

    #[test]
    fn test_append_live_thinking_keeps_full_when_reasoning_full_enabled() {
        let _guard = env_test_lock();
        let _reasoning_guard = ReasoningFullGuard::set(true);
        let mut state = TuiState::default();
        let long = "x".repeat(400);
        state.append_live_thinking(&long);
        assert_eq!(state.live_thinking, long);
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
    fn test_processing_cycle_yields_flash_credit_notice() {
        hermes_core::credits::clear_last_nous_credits_state();
        hermes_core::credits::capture_nous_credits_from_pairs([
            ("x-nous-credits-version", "1"),
            ("x-nous-credits-remaining-micros", "12000000"),
            ("x-nous-credits-remaining-usd", "12.00"),
            ("x-nous-credits-subscription-micros", "5000000"),
            ("x-nous-credits-subscription-usd", "5.00"),
            ("x-nous-credits-subscription-limit-micros", "10000000"),
            ("x-nous-credits-subscription-limit-usd", "10.00"),
            ("x-nous-credits-rollover-micros", "0"),
            ("x-nous-credits-purchased-micros", "7000000"),
            ("x-nous-credits-purchased-usd", "7.00"),
            ("x-nous-credits-denominator-kind", "subscription_cap"),
            ("x-nous-credits-paid-access", "true"),
            ("x-nous-credits-as-of-ms", "1710000000000"),
        ])
        .expect("credits captured");
        assert_eq!(
            hermes_core::credits::last_nous_credits_notice_line().as_deref(),
            Some("credits: 50% used - run /usage")
        );

        let mut state = TuiState::default();
        state.begin_processing_cycle("nous:test-model");

        assert_eq!(hermes_core::credits::last_nous_credits_notice_line(), None);
        hermes_core::credits::clear_last_nous_credits_state();
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
    fn test_processing_stage_labels() {
        let mut state = TuiState::default();
        assert_eq!(state.processing_stage_label(), "idle");
        state.begin_processing_cycle("nous:test-model");
        assert_eq!(state.processing_stage_label(), "phase-driven");
        state.processing_phase_label.clear();
        state.active_tools.push("terminal".to_string());
        assert_eq!(state.processing_stage_label(), "running tools (pre-token)");
        state.saw_first_token = true;
        assert_eq!(state.processing_stage_label(), "running tools + streaming");
        state.active_tools.clear();
        assert_eq!(state.processing_stage_label(), "streaming response");
    }

    #[test]
    fn test_phase_updates_set_progress_and_activity() {
        let mut state = TuiState::default();
        state.begin_processing_cycle("nous:test-model");
        state.update_processing_phase("retrieval", "collecting evidence", Some(42));
        assert_eq!(state.processing_phase, "retrieval");
        assert_eq!(state.processing_phase_label, "collecting evidence");
        assert_eq!(state.processing_phase_progress, 42);
        assert!(state
            .recent_activity
            .last()
            .is_some_and(|line| line.contains("phase 42%")));
    }

    #[test]
    fn test_animated_processing_bar_width_and_motion() {
        let bar_a = animated_processing_bar(0, 12);
        let bar_b = animated_processing_bar(4, 12);
        assert_eq!(bar_a.chars().count(), 12);
        assert_eq!(bar_b.chars().count(), 12);
        assert_ne!(bar_a, bar_b);
    }

    #[test]
    fn test_find_anchor_line_index_prefers_near_expected_window() {
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from("dup anchor"));
        for idx in 1..2500 {
            lines.push(Line::from(format!("line-{idx}")));
        }
        lines.push(Line::from("dup anchor"));
        let idx = find_anchor_line_index(&lines, "dup anchor", 2499).expect("anchor index");
        assert_eq!(idx, 2500);
    }

    #[test]
    fn test_find_anchor_line_index_falls_back_to_global_search() {
        let lines = vec![Line::from("alpha"), Line::from("beta"), Line::from("gamma")];
        let idx = find_anchor_line_index(&lines, "gamma", 0).expect("anchor index");
        assert_eq!(idx, 2);
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
        let ctrl_upper_c = KeyEvent::new(KeyCode::Char('C'), KeyModifiers::CONTROL);
        let raw_etx = KeyEvent::new(KeyCode::Char('\u{3}'), KeyModifiers::NONE);
        let plain_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(is_ctrl_c(&ctrl_c));
        assert!(is_ctrl_c(&ctrl_upper_c));
        assert!(is_ctrl_c(&raw_etx));
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
        assert!(is_submit_shortcut(&plain_enter, "/model\nlist"));
    }

    #[test]
    fn test_bracketed_paste_inserts_multiline_text_without_submit_shortcut() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut state = TuiState::default();
        state.input = "before  after".to_string();
        state.cursor_position = "before ".len();

        state.insert_paste_at_cursor("line1\r\nline2\rline3");

        assert_eq!(state.input, "before line1\nline2\nline3 after");
        assert_eq!(state.cursor_position, "before line1\nline2\nline3".len());

        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        assert!(!is_submit_shortcut(&shift_enter, &state.input));
    }

    #[test]
    fn test_insert_control_chords_toggle_ui_state_without_text_mutation() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut state = TuiState::default();
        state.input = "keep me".to_string();
        state.cursor_position = state.input.len();

        assert!(state
            .handle_insert_control_chord(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL)));
        assert!(!state.activity_lane_open);
        assert_eq!(state.status_message, "Activity lane hidden");

        assert!(state
            .handle_insert_control_chord(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)));
        assert_eq!(state.activity_lane_mode, ActivityLaneMode::Cockpit);
        assert_eq!(state.status_message, "Activity lane mode: ops cockpit");

        assert!(state
            .handle_insert_control_chord(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)));
        assert_eq!(state.view_density, ViewDensity::Compact);
        assert_eq!(state.status_message, "Compact transcript mode");

        assert!(state
            .handle_insert_control_chord(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)));
        assert!(state.show_timestamps);
        assert_eq!(state.status_message, "Timestamps visible");

        assert!(state
            .handle_insert_control_chord(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL)));
        assert!(state.expanded_tool_cards.contains("__all__"));
        assert_eq!(state.input, "keep me");
    }

    #[test]
    fn test_insert_control_word_navigation_respects_unicode_boundaries() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut state = TuiState::default();
        state.input = "alpha beta éclair".to_string();
        state.cursor_position = state.input.len();

        assert!(
            state.handle_insert_control_chord(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL))
        );
        assert_eq!(&state.input[state.cursor_position..], "éclair");

        assert!(
            state.handle_insert_control_chord(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL))
        );
        assert_eq!(state.cursor_position, state.input.len());
    }

    #[test]
    fn test_parse_interactive_question_request_pipe_syntax() {
        let request = parse_interactive_question_request(
            "/ask Proceed with deploy? | yes (recommended)::ship now | no::pause and inspect",
        )
        .expect("parse request");
        assert_eq!(request.prompt, "Proceed with deploy?");
        assert_eq!(request.options.len(), 2);
        assert_eq!(request.options[0].label, "yes (recommended)");
        assert_eq!(request.options[0].detail, "ship now");
        assert_eq!(request.options[1].label, "no");
    }

    #[test]
    fn test_parse_interactive_question_request_multiline_syntax() {
        let request = parse_interactive_question_request(
            "/question\nWhat path should we take?\n- continue implementation\n- pause for diagnosis",
        )
        .expect("parse request");
        assert_eq!(request.prompt, "What path should we take?");
        assert_eq!(request.options.len(), 2);
        assert_eq!(request.options[0].label, "continue implementation");
    }

    #[test]
    fn test_parse_interactive_question_request_requires_two_options() {
        let err = parse_interactive_question_request("/ask choose one | only-one-option")
            .expect_err("expected parse error");
        assert!(err.contains("at least 2 options"));
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
    fn test_insert_newline_at_cursor_clamps_non_char_boundary() {
        let mut state = TuiState::default();
        state.input = "éx".to_string();
        state.cursor_position = 1; // interior byte of 'é'
        state.insert_newline_at_cursor();
        assert_eq!(state.input, "\néx");
        assert_eq!(state.cursor_position, 1);
    }

    #[test]
    fn test_cursor_row_col_clamps_non_char_boundary() {
        assert_eq!(TuiState::cursor_row_col("éx", 1), (0, 0));
        assert_eq!(TuiState::cursor_row_col("éx", 2), (0, 1));
    }

    #[test]
    fn test_native_editor_input_inserts_and_deletes_at_cursor() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut state = TuiState::default();
        state.input = "helo".to_string();
        state.cursor_position = 2;

        state.apply_editor_input(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        assert_eq!(state.input, "hello");
        assert_eq!(state.cursor_position, 3);

        state.apply_editor_input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(state.input, "helo");
        assert_eq!(state.cursor_position, 2);

        state.apply_editor_input(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(state.input, "heo");
        assert_eq!(state.cursor_position, 2);
    }

    #[test]
    fn test_native_editor_cursor_navigation_respects_multiline_unicode() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut state = TuiState::default();
        state.input = "ab\nécd\nxy".to_string();
        state.cursor_position = TuiState::row_col_to_byte_offset(&state.input, 1, 2);

        state.apply_editor_input(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(
            TuiState::cursor_row_col(&state.input, state.cursor_position),
            (0, 2)
        );

        state.apply_editor_input(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            TuiState::cursor_row_col(&state.input, state.cursor_position),
            (1, 2)
        );

        state.apply_editor_input(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(
            TuiState::cursor_row_col(&state.input, state.cursor_position),
            (1, 0)
        );

        state.apply_editor_input(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(
            TuiState::cursor_row_col(&state.input, state.cursor_position),
            (1, 3)
        );
    }

    #[test]
    fn test_scroll_history_offset_not_capped_to_u16() {
        let mut state = TuiState::default();
        state.scroll_offset = (u16::MAX as usize).saturating_sub(1);
        state.scroll_history_up(8);
        assert_eq!(state.scroll_offset, (u16::MAX as usize).saturating_add(7));
        assert!(!state.auto_follow_transcript);
    }

    #[test]
    fn test_jump_to_oldest_sets_unbounded_offset() {
        let mut state = TuiState::default();
        state.jump_to_oldest();
        assert_eq!(state.scroll_offset, usize::MAX);
        assert!(!state.auto_follow_transcript);
    }

    #[test]
    fn test_project_transcript_window_virtualizes_large_offsets() {
        let lines: Vec<Line<'static>> = (0..100_000)
            .map(|idx| Line::from(format!("line-{idx}")))
            .collect();
        let (window, local_scroll) = project_transcript_window(&lines, 80, 70_000, 30);
        assert!(!window.is_empty());
        assert_eq!(local_scroll, 0);
        assert_eq!(window[0].to_string(), "line-70000");
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
        let _guard = env_test_lock();
        let payload = r#"{"result":"line1\nline2","_budget_warning":"[BUDGET WARNING: Iteration 40/50.]","error":"boom"}"#;
        let lines = format_tool_message_lines(payload);
        let joined = lines.join("\n");
        assert!(joined.contains("[BUDGET WARNING"));
        assert!(joined.contains("[result]"));
        assert!(joined.contains("line1"));
        assert!(joined.contains("[error]"));
        assert!(joined.contains("boom"));
    }

    #[test]
    fn test_format_tool_message_lines_adds_policy_remediation_block() {
        let _guard = env_test_lock();
        let payload = r#"{
  "error":"Blocked by tool policy: tool params matched deny pattern '(?i)api[_-]?key'",
  "policy":{"code":"params_pattern_denied","mode":"enforce"}
}"#;
        let lines = format_tool_message_lines(payload);
        let joined = lines.join("\n");
        assert!(joined.contains("[remediation]"));
        assert!(joined.contains("Remove secret-like parameter names"));
    }

    #[test]
    fn test_format_tool_message_lines_truncates_large_payload() {
        let _guard = env_test_lock();
        let cap = max_tool_output_lines();
        let long = (0..(cap + 40))
            .map(|idx| format!("row-{idx}-{}", "x".repeat(120)))
            .collect::<Vec<_>>()
            .join("\\n");
        let payload = format!(r#"{{"result":"{}"}}"#, long);
        let lines = format_tool_message_lines(&payload);
        let joined = lines.join("\n");
        assert!(joined.contains("tool output truncated"));
        assert!(lines.len() <= cap + 8);
    }

    #[test]
    fn test_format_tool_message_lines_keeps_verbose_preview_small() {
        let _guard = env_test_lock();
        let long = (0..80)
            .map(|idx| format!("row-{idx}-{}", "x".repeat(120)))
            .collect::<Vec<_>>()
            .join("\\n");
        let payload = format!(r#"{{"result":"{}"}}"#, long);
        let lines = format_tool_message_lines(&payload);
        let joined = lines.join("\n");

        assert!(joined.contains("tool output truncated"));
        assert!(
            joined.chars().count() < 2_000,
            "tool preview should stay small enough for retained TUI transcript lines"
        );
    }

    #[test]
    fn test_approximate_visual_rows_wraps_long_lines() {
        let lines = vec![Line::from("x".repeat(120))];
        assert_eq!(approximate_visual_rows(&lines, 40), 3);
        assert_eq!(approximate_visual_rows(&lines, 80), 2);
    }

    #[test]
    fn test_transcript_wrap_width_caps_at_80() {
        assert_eq!(transcript_wrap_width(12), 12);
        assert_eq!(transcript_wrap_width(80), 80);
        assert_eq!(transcript_wrap_width(140), 80);
    }

    #[test]
    fn test_hard_wrap_segments_prefers_word_boundaries() {
        let wrapped = hard_wrap_segments("context lattice integration is core", 12);
        assert_eq!(
            wrapped,
            vec![
                "context".to_string(),
                "lattice".to_string(),
                "integration".to_string(),
                "is core".to_string(),
            ]
        );
    }

    #[test]
    fn test_hard_wrap_segments_splits_overlong_token() {
        let wrapped = hard_wrap_segments("supercalifragilisticexpialidocious", 8);
        assert_eq!(
            wrapped,
            vec![
                "supercal".to_string(),
                "ifragili".to_string(),
                "sticexpi".to_string(),
                "alidocio".to_string(),
                "us".to_string(),
            ]
        );
    }

    #[test]
    fn test_stream_chunk_has_progress_for_extra_only_events() {
        let chunk = StreamChunk {
            delta: Some(hermes_core::StreamDelta {
                content: None,
                tool_calls: None,
                extra: Some(serde_json::json!({
                    "ui_event": "lifecycle",
                    "message": "dispatching request"
                })),
            }),
            finish_reason: None,
            usage: None,
        };
        assert!(stream_chunk_has_progress(&chunk));
    }

    #[test]
    fn test_stream_lane_budget_defaults_balanced() {
        let (cap, budget) = stream_lane_budget_from("advisory", "balanced", false, 0);
        assert_eq!(cap, 96);
        assert_eq!(budget, Duration::from_millis(6));
    }

    #[test]
    fn test_stream_lane_budget_throughput_profile_expands() {
        let (cap, budget) = stream_lane_budget_from("advisory", "throughput", false, 0);
        assert!(cap >= 320);
        assert!(budget >= Duration::from_millis(16));
    }

    #[test]
    fn test_stream_lane_budget_off_mode_uses_baseline() {
        let (cap, budget) = stream_lane_budget_from("off", "throughput", true, 200);
        assert_eq!(cap, 96);
        assert_eq!(budget, Duration::from_millis(6));
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

    #[test]
    fn test_transcript_fingerprint_tracks_toolcard_expand_state() {
        let messages = vec![Message::tool_result("call-1", "{}")];
        let mut state_a = TuiState::default();
        let mut state_b = TuiState::default();
        state_b.expanded_tool_cards.insert("__all__".to_string());

        let fp_a = transcript_fingerprint(&messages, &state_a, 80);
        let fp_b = transcript_fingerprint(&messages, &state_b, 80);
        assert_ne!(fp_a, fp_b);

        // keep borrow-checker happy and ensure states are still usable
        state_a.stream_buffer.clear();
        state_b.stream_buffer.clear();
    }

    #[test]
    fn test_default_language_sanitizer_strips_non_ascii() {
        let raw = "to=functions.memory 大安快些 json ... But I can in one message as seen in logs.";
        let sanitized = sanitize_line_to_default_language_ascii(raw, true).expect("sanitized");
        assert!(!sanitized.contains('大'));
        assert!(!sanitized.contains('安'));
        assert!(sanitized.contains("to=functions.memory"));
        assert!(sanitized.contains("But I can in one message as seen in logs."));
    }

    #[test]
    fn test_render_assistant_markdown_lines_enforces_default_language_gate() {
        let theme = Theme::default_theme();
        let colors = theme.colors.to_ratatui_colors();
        let styles = theme.resolved_styles();
        let content = "to=functions.memory 天安中彩樣\nregular line 大家好";
        let lines = render_assistant_markdown_lines(content, &styles, &colors);
        let joined = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("internal orchestration scaffold hidden"));
        assert!(!joined.contains('天'));
        assert!(!joined.contains('大'));
        assert!(joined.contains("regular line"));
    }

    #[test]
    fn test_scaffold_detector_matches_embedded_tool_tags() {
        let line = "random prefix <tool_use><name>terminal</name></tool_use> suffix";
        assert!(looks_like_internal_scaffold_line(line));
    }

    #[test]
    fn test_scaffold_detector_matches_escaped_tool_tags() {
        let line = "noise \\u003ctool_use\\u003e <argument name=\"skill\">x</argument>";
        assert!(looks_like_internal_scaffold_line(line));
    }

    #[test]
    fn test_collapse_render_lines_adds_notice() {
        let theme = Theme::default_theme();
        let colors = theme.colors.to_ratatui_colors();
        let input: Vec<Line<'static>> = (0..40)
            .map(|idx| Line::from(format!("line-{idx}")))
            .collect();
        let collapsed = collapse_render_lines_with_notice(input, 12, &colors);
        let joined = collapsed
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(collapsed.len() <= 13);
        assert!(joined.contains("transcript compressed for readability"));
    }

    #[test]
    fn test_tail_render_lines_keeps_latest_rows() {
        let theme = Theme::default_theme();
        let colors = theme.colors.to_ratatui_colors();
        let input: Vec<Line<'static>> = (0..20)
            .map(|idx| Line::from(format!("tail-{idx}")))
            .collect();
        let tailed = tail_render_lines_with_notice(input, 5, &colors);
        let joined = tailed
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(tailed.len() <= 6);
        assert!(joined.contains("tail-19"));
        assert!(joined.contains("live stream trimmed"));
    }
}
