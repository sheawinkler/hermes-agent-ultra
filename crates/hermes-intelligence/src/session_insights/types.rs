//! Data types for the session insights engine.
//!
//! These mirror the dictionaries `agent.insights.InsightsEngine` returns
//! from `generate()`. Strong typing replaces Python's `Dict[str, Any]` so
//! formatters and downstream consumers don't have to defensively probe for
//! optional keys.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Raw input rows
// ---------------------------------------------------------------------------

/// One row from the `sessions` SQL table.
///
/// Field names follow the Python schema (`agent/insights.py::_SESSION_COLS`)
/// so that future SQLite-backed [`super::SessionDataSource`]
/// implementations can map directly.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionRow {
    pub id: String,
    pub source: Option<String>,
    pub model: Option<String>,
    /// Unix epoch seconds.
    pub started_at: Option<f64>,
    pub ended_at: Option<f64>,
    pub message_count: Option<u64>,
    pub tool_call_count: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub billing_provider: Option<String>,
    pub billing_base_url: Option<String>,
    pub billing_mode: Option<String>,
    pub estimated_cost_usd: Option<f64>,
    pub actual_cost_usd: Option<f64>,
    pub cost_status: Option<String>,
    pub cost_source: Option<String>,
}

/// Aggregate counts from the `messages` table (`InsightsEngine._get_message_stats`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageStats {
    pub total_messages: u64,
    pub user_messages: u64,
    pub assistant_messages: u64,
    pub tool_messages: u64,
}

/// One row of (tool_name, count) for tool usage ranking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUsageRow {
    pub tool_name: String,
    pub count: u64,
}

// ---------------------------------------------------------------------------
// Computed report
// ---------------------------------------------------------------------------

/// Top-level report returned by [`super::SessionInsightsEngine::generate`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InsightsReport {
    pub days: u32,
    pub source_filter: Option<String>,
    pub empty: bool,
    /// Unix epoch seconds; only set when `empty == false`.
    pub generated_at: Option<f64>,
    pub overview: Overview,
    pub models: Vec<ModelBreakdownRow>,
    pub platforms: Vec<PlatformBreakdownRow>,
    pub tools: Vec<ToolBreakdownRow>,
    pub activity: ActivityPatterns,
    pub top_sessions: Vec<TopSessionRow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Overview {
    pub total_sessions: u64,
    pub total_messages: u64,
    pub total_tool_calls: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_write_tokens: u64,
    pub total_tokens: u64,
    pub estimated_cost: f64,
    pub actual_cost: f64,
    pub total_hours: f64,
    pub avg_session_duration: f64,
    pub avg_messages_per_session: f64,
    pub avg_tokens_per_session: f64,
    pub user_messages: u64,
    pub assistant_messages: u64,
    pub tool_messages: u64,
    /// Earliest `started_at` across the window.
    pub date_range_start: Option<f64>,
    pub date_range_end: Option<f64>,
    pub models_with_pricing: Vec<String>,
    pub models_without_pricing: Vec<String>,
    pub unknown_cost_sessions: u64,
    pub included_cost_sessions: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelBreakdownRow {
    pub model: String,
    pub sessions: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub tool_calls: u64,
    pub cost: f64,
    pub has_pricing: bool,
    pub cost_status: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlatformBreakdownRow {
    pub platform: String,
    pub sessions: u64,
    pub messages: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub tool_calls: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolBreakdownRow {
    pub tool: String,
    pub count: u64,
    pub percentage: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActivityPatterns {
    pub by_day: Vec<DayCount>,
    pub by_hour: Vec<HourCount>,
    pub busiest_day: Option<DayCount>,
    pub busiest_hour: Option<HourCount>,
    pub active_days: u64,
    pub max_streak: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DayCount {
    /// Three-letter day name: `Mon`..`Sun`.
    pub day: String,
    pub count: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HourCount {
    /// 0..23.
    pub hour: u8,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopSessionRow {
    pub label: String,
    pub session_id: String,
    pub value: String,
    pub date: String,
}
