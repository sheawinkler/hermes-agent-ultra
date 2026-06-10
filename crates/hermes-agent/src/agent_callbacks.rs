//! Agent callbacks for progress reporting during tool execution.
//!
//! Extracted from `agent_loop.rs` to keep the loop logic focused on
//! orchestration while callback plumbing lives in its own module.

use std::sync::Arc;

use serde_json::Value;

/// Callbacks invoked during tool execution for progress reporting.
#[derive(Default)]
pub struct AgentCallbacks {
    /// Called when the LLM is "thinking" (reasoning tokens).
    pub on_thinking: Option<Box<dyn Fn(&str) + Send + Sync>>,
    /// Called when a tool call begins.
    pub on_tool_start: Option<Box<dyn Fn(&str, &Value) + Send + Sync>>,
    /// Called when a tool call finishes.
    pub on_tool_complete: Option<Box<dyn Fn(&str, &str) + Send + Sync>>,
    /// Called for each stream delta.
    pub on_stream_delta: Option<Box<dyn Fn(&str) + Send + Sync>>,
    /// Called after each completed LLM step (full response assembled).
    pub on_step_complete: Option<Box<dyn Fn(u32) + Send + Sync>>,
    /// Called when background memory/skill review completes or fails.
    ///
    /// Payload is a user-friendly summary string suitable for direct UI output.
    pub background_review_callback: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// Called for lifecycle/status notices (context pressure, retries, etc.).
    pub status_callback: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
    /// Interactive Codex exec/patch approval (Python terminal `approval_callback`).
    pub codex_approval_callback: Option<Arc<dyn Fn(&str, &str) -> String + Send + Sync>>,
}
