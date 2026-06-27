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
use std::io::{Stdout, Write};
use std::time::{Duration, Instant};

use chrono::Local;
use crossterm::cursor::Show;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as CrosstermEvent, KeyEvent, MouseEvent,
};
use crossterm::style::ResetColor;
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
use tokio::task::JoinHandle;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use hermes_auth::FileTokenStore;
use hermes_core::{AgentError, AgentResult, Message, StreamChunk};

use crate::app::App;
use crate::commands;
use crate::theme::Theme;
use hermes_cli_ui::tool_preview::{build_tool_preview_from_value, tool_emoji};

// Keep these includes in original item order. This first split preserves the
// existing module namespace while separating state, rendering, run-loop, stream,
// and test surfaces for maintainable follow-up refactors.
include!("tui/core.rs");
include!("tui/state.rs");
include!("tui/rendering.rs");
include!("tui/run_loop.rs");
include!("tui/stream.rs");
include!("tui/tests.rs");
