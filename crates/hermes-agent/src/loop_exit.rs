//! Exit metadata types for agent loop invocations.
//!
//! Extracted from `agent_loop.rs` to keep the loop logic focused on
//! orchestration.

/// Exit metadata for one `run` / `run_stream` invocation (maps to Python loop fields).
pub(crate) struct LoopExit<'a> {
    pub(crate) turn_exit_reason: &'a str,
    pub(crate) api_calls: u32,
    pub(crate) failed: bool,
    pub(crate) partial: bool,
    pub(crate) finished_naturally: bool,
    pub(crate) interrupted: bool,
}
