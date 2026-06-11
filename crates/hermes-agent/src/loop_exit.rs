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
    pub(crate) plan_pending: Option<String>,
    pub(crate) plan_phase: Option<String>,
}

impl<'a> LoopExit<'a> {
    pub(crate) fn base(
        turn_exit_reason: &'a str,
        api_calls: u32,
        failed: bool,
        partial: bool,
        finished_naturally: bool,
        interrupted: bool,
    ) -> Self {
        Self {
            turn_exit_reason,
            api_calls,
            failed,
            partial,
            finished_naturally,
            interrupted,
            plan_pending: None,
            plan_phase: None,
        }
    }
}
