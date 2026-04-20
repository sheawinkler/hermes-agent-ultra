//! Pluggable data source for the session insights engine.
//!
//! Production code wires this to a SQLite-backed [`SessionDataSource`];
//! tests use [`InMemorySessionData`].

use serde::{Deserialize, Serialize};

use super::types::{MessageStats, SessionRow, ToolUsageRow};

/// One assistant message contributing tool calls (used by the in-memory
/// backend to mirror the Python `tool_calls JSON on assistant messages`
/// fallback).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantToolCallRow {
    pub session_id: String,
    pub source: Option<String>,
    /// Names of the tools the assistant called in this message.
    pub tool_names: Vec<String>,
}

/// Fetches the raw rows the insights engine needs.
///
/// Implementations are expected to be cheap and synchronous (the underlying
/// store is local SQLite or in-memory). Returning `Vec` rather than an
/// iterator keeps the engine portable and matches the Python reference.
pub trait SessionDataSource {
    /// Sessions whose `started_at >= cutoff`, optionally filtered by source.
    fn fetch_sessions(&self, cutoff: f64, source: Option<&str>) -> Vec<SessionRow>;

    /// Tool usage counts derived from `tool_name` on `tool` role messages
    /// **plus** `tool_calls` JSON on `assistant` role messages, deduplicated
    /// by taking the max per tool.
    fn fetch_tool_usage(&self, cutoff: f64, source: Option<&str>) -> Vec<ToolUsageRow>;

    /// Aggregate counts from the messages table.
    fn fetch_message_stats(&self, cutoff: f64, source: Option<&str>) -> MessageStats;
}

// ---------------------------------------------------------------------------
// In-memory backend (production-ready for tests; usable for non-SQLite
// frontends that already hold session data in process).
// ---------------------------------------------------------------------------

/// In-memory [`SessionDataSource`] implementation.
///
/// Sessions and per-tool counts are stored verbatim. A separate vector of
/// assistant tool-call rows lets us replicate the Python "two-source merge"
/// behaviour deterministically.
#[derive(Debug, Default, Clone)]
pub struct InMemorySessionData {
    pub sessions: Vec<SessionRow>,
    /// Tool counts attributed via the explicit `tool_name` column on
    /// tool-role messages (`(session_id, tool_name) -> count`).
    pub tool_messages: Vec<ToolMessageRow>,
    /// Assistant messages that emitted tool calls.
    pub assistant_tool_calls: Vec<AssistantToolCallRow>,
    pub message_counts: MessageStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMessageRow {
    pub session_id: String,
    pub source: Option<String>,
    pub tool_name: String,
    pub count: u64,
}

impl InMemorySessionData {
    /// Builder helper.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a session row.
    pub fn add_session(&mut self, row: SessionRow) -> &mut Self {
        self.sessions.push(row);
        self
    }

    /// Add an explicit `tool` role message contribution.
    pub fn add_tool_message(
        &mut self,
        session_id: impl Into<String>,
        source: Option<String>,
        tool_name: impl Into<String>,
        count: u64,
    ) -> &mut Self {
        self.tool_messages.push(ToolMessageRow {
            session_id: session_id.into(),
            source,
            tool_name: tool_name.into(),
            count,
        });
        self
    }

    /// Add an assistant message contributing one or more tool calls.
    pub fn add_assistant_tool_call(
        &mut self,
        session_id: impl Into<String>,
        source: Option<String>,
        tool_names: Vec<String>,
    ) -> &mut Self {
        self.assistant_tool_calls.push(AssistantToolCallRow {
            session_id: session_id.into(),
            source,
            tool_names,
        });
        self
    }

    /// Set aggregate message stats directly.
    pub fn set_message_stats(&mut self, stats: MessageStats) -> &mut Self {
        self.message_counts = stats;
        self
    }
}

fn passes_filter(session_source: Option<&str>, filter: Option<&str>) -> bool {
    match filter {
        None => true,
        Some(want) => session_source == Some(want),
    }
}

impl SessionDataSource for InMemorySessionData {
    fn fetch_sessions(&self, cutoff: f64, source: Option<&str>) -> Vec<SessionRow> {
        let mut rows: Vec<SessionRow> = self
            .sessions
            .iter()
            .filter(|s| s.started_at.unwrap_or(0.0) >= cutoff)
            .filter(|s| passes_filter(s.source.as_deref(), source))
            .cloned()
            .collect();
        rows.sort_by(|a, b| {
            b.started_at
                .unwrap_or(0.0)
                .partial_cmp(&a.started_at.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        rows
    }

    fn fetch_tool_usage(&self, cutoff: f64, source: Option<&str>) -> Vec<ToolUsageRow> {
        // Build the set of session ids that match the time + source filter so
        // that tool/assistant rows referencing them are accepted.
        let qualifying_session_ids: std::collections::HashSet<&String> = self
            .sessions
            .iter()
            .filter(|s| s.started_at.unwrap_or(0.0) >= cutoff)
            .filter(|s| passes_filter(s.source.as_deref(), source))
            .map(|s| &s.id)
            .collect();

        // Source 1: explicit tool_name on tool messages.
        let mut tool_counts: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for row in &self.tool_messages {
            if !qualifying_session_ids.contains(&row.session_id) {
                continue;
            }
            *tool_counts.entry(row.tool_name.clone()).or_insert(0) += row.count;
        }

        // Source 2: tool_calls on assistant messages.
        let mut tool_calls_counts: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for row in &self.assistant_tool_calls {
            if !qualifying_session_ids.contains(&row.session_id) {
                continue;
            }
            for name in &row.tool_names {
                *tool_calls_counts.entry(name.clone()).or_insert(0) += 1;
            }
        }

        // Merge per Python semantics: when both sources have data, take the
        // max per tool to avoid double-counting overlap.
        let merged: std::collections::HashMap<String, u64> = if tool_counts.is_empty() {
            tool_calls_counts
        } else if tool_calls_counts.is_empty() {
            tool_counts
        } else {
            let mut out = tool_counts.clone();
            for (k, v) in tool_calls_counts {
                let entry = out.entry(k).or_insert(0);
                if v > *entry {
                    *entry = v;
                }
            }
            out
        };

        let mut rows: Vec<ToolUsageRow> = merged
            .into_iter()
            .map(|(tool_name, count)| ToolUsageRow { tool_name, count })
            .collect();
        rows.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.tool_name.cmp(&b.tool_name))
        });
        rows
    }

    fn fetch_message_stats(&self, _cutoff: f64, _source: Option<&str>) -> MessageStats {
        // For in-memory tests the caller pre-supplies aggregate stats; this
        // matches the Python COUNT(*) / SUM(CASE) pattern at row-fetch
        // granularity which we don't otherwise simulate here.
        self.message_counts.clone()
    }
}
