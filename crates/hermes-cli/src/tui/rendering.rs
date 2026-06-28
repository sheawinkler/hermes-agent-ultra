// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

const TRANSCRIPT_HARD_WRAP_COLS: u16 = 80;
const TRANSCRIPT_CONTENT_WRAP_COLS: usize = 76;
const OFFSET_ANCHOR_SEARCH_RADIUS: usize = 1200;
const DEFAULT_MAX_ASSISTANT_RENDER_LINES: usize = 260;
const MAX_STREAM_RENDER_LINES: usize = 140;
const DEFAULT_TOOL_OUTPUT_MAX_LINES: usize = 16;
const DEFAULT_TOOL_OUTPUT_MAX_LINE_CHARS: usize = 600;
const DEFAULT_TOOL_OUTPUT_MAX_TOTAL_CHARS: usize = 1_024;

include!("rendering/frame_runtime.rs");
include!("rendering/header_live_details.rs");
include!("rendering/inline_markdown.rs");
include!("rendering/assistant_markdown.rs");
include!("rendering/tool_messages.rs");
include!("rendering/transcript_lines.rs");
include!("rendering/message_window.rs");

include!("rendering/modals_events.rs");
