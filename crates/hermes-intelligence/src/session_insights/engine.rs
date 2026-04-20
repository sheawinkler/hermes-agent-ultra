//! Session insights computation engine.
//!
//! Mirrors `agent.insights.InsightsEngine` (`agent/insights.py`, 790 LoC):
//! pulls historical session/message rows from a [`SessionDataSource`] and
//! computes overview, model/platform/tool breakdowns, activity patterns and
//! "notable session" snapshots.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Datelike, Timelike, Utc};

use crate::usage_pricing::{calculate_cost, has_known_pricing, CanonicalUsage, CostStatus};

use super::source::SessionDataSource;
use super::types::*;

const SECONDS_PER_DAY: f64 = 86_400.0;

/// Computes [`InsightsReport`]s from a [`SessionDataSource`].
pub struct SessionInsightsEngine<'a, D: SessionDataSource + ?Sized> {
    data: &'a D,
}

impl<'a, D: SessionDataSource + ?Sized> SessionInsightsEngine<'a, D> {
    /// Borrow a data source. The engine is stateless beyond this borrow.
    pub fn new(data: &'a D) -> Self {
        Self { data }
    }

    /// Generate a complete report covering the last `days` days.
    ///
    /// `now` is provided explicitly for deterministic testing; production
    /// callers should pass [`Utc::now`].timestamp() as a `f64`.
    pub fn generate(&self, days: u32, source: Option<&str>, now: f64) -> InsightsReport {
        let cutoff = now - (days as f64) * SECONDS_PER_DAY;

        let sessions = self.data.fetch_sessions(cutoff, source);
        let tool_usage = self.data.fetch_tool_usage(cutoff, source);
        let message_stats = self.data.fetch_message_stats(cutoff, source);

        if sessions.is_empty() {
            return InsightsReport {
                days,
                source_filter: source.map(str::to_string),
                empty: true,
                ..Default::default()
            };
        }

        let overview = compute_overview(&sessions, &message_stats);
        let models = compute_model_breakdown(&sessions);
        let platforms = compute_platform_breakdown(&sessions);
        let tools = compute_tool_breakdown(&tool_usage);
        let activity = compute_activity_patterns(&sessions);
        let top_sessions = compute_top_sessions(&sessions);

        InsightsReport {
            days,
            source_filter: source.map(str::to_string),
            empty: false,
            generated_at: Some(now),
            overview,
            models,
            platforms,
            tools,
            activity,
            top_sessions,
        }
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

fn estimate_session_cost(session: &SessionRow) -> (f64, CostStatus) {
    let usage = CanonicalUsage {
        input_tokens: session.input_tokens.unwrap_or(0),
        output_tokens: session.output_tokens.unwrap_or(0),
        cache_read_tokens: session.cache_read_tokens.unwrap_or(0),
        cache_write_tokens: session.cache_write_tokens.unwrap_or(0),
        ..Default::default()
    };
    let result = calculate_cost(
        session.model.as_deref().unwrap_or(""),
        &usage,
        session.billing_provider.as_deref(),
        session.billing_base_url.as_deref(),
    );
    (result.amount_usd.unwrap_or(0.0), result.status)
}

fn display_model_name(model: &str) -> String {
    model.rsplit('/').next().unwrap_or(model).to_string()
}

fn unwrap_or_unknown(model: &str) -> String {
    if model.is_empty() {
        "unknown".to_string()
    } else {
        display_model_name(model)
    }
}

fn cost_status_label(status: &CostStatus) -> &'static str {
    match status {
        CostStatus::Actual => "actual",
        CostStatus::Estimated => "estimated",
        CostStatus::Included => "included",
        CostStatus::Unknown => "unknown",
    }
}

// ---------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------

pub(crate) fn compute_overview(sessions: &[SessionRow], message_stats: &MessageStats) -> Overview {
    let mut total_input = 0u64;
    let mut total_output = 0u64;
    let mut total_cache_read = 0u64;
    let mut total_cache_write = 0u64;
    let mut total_tool_calls = 0u64;
    let mut total_messages = 0u64;
    let mut total_cost = 0.0_f64;
    let mut actual_cost = 0.0_f64;
    let mut models_with_pricing: BTreeSet<String> = BTreeSet::new();
    let mut models_without_pricing: BTreeSet<String> = BTreeSet::new();
    let mut unknown_cost_sessions = 0u64;
    let mut included_cost_sessions = 0u64;

    for session in sessions {
        total_input += session.input_tokens.unwrap_or(0);
        total_output += session.output_tokens.unwrap_or(0);
        total_cache_read += session.cache_read_tokens.unwrap_or(0);
        total_cache_write += session.cache_write_tokens.unwrap_or(0);
        total_tool_calls += session.tool_call_count.unwrap_or(0);
        total_messages += session.message_count.unwrap_or(0);

        let (cost, status) = estimate_session_cost(session);
        total_cost += cost;
        actual_cost += session.actual_cost_usd.unwrap_or(0.0);

        match status {
            CostStatus::Included => included_cost_sessions += 1,
            CostStatus::Unknown => unknown_cost_sessions += 1,
            _ => {}
        }

        let raw_model = session.model.as_deref().unwrap_or("");
        let display = unwrap_or_unknown(raw_model);
        if has_known_pricing(
            raw_model,
            session.billing_provider.as_deref(),
            session.billing_base_url.as_deref(),
        ) {
            models_with_pricing.insert(display);
        } else {
            models_without_pricing.insert(display);
        }
    }

    let total_tokens = total_input + total_output + total_cache_read + total_cache_write;

    let mut durations = Vec::new();
    for s in sessions {
        if let (Some(start), Some(end)) = (s.started_at, s.ended_at) {
            if end > start {
                durations.push(end - start);
            }
        }
    }
    let total_hours = if durations.is_empty() {
        0.0
    } else {
        durations.iter().sum::<f64>() / 3600.0
    };
    let avg_duration = if durations.is_empty() {
        0.0
    } else {
        durations.iter().sum::<f64>() / durations.len() as f64
    };

    let mut started: Vec<f64> = sessions.iter().filter_map(|s| s.started_at).collect();
    started.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let date_range_start = started.first().copied();
    let date_range_end = started.last().copied();

    let n_sessions = sessions.len() as u64;
    let avg_messages = if n_sessions > 0 {
        total_messages as f64 / n_sessions as f64
    } else {
        0.0
    };
    let avg_tokens = if n_sessions > 0 {
        total_tokens as f64 / n_sessions as f64
    } else {
        0.0
    };

    Overview {
        total_sessions: n_sessions,
        total_messages,
        total_tool_calls,
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        total_cache_read_tokens: total_cache_read,
        total_cache_write_tokens: total_cache_write,
        total_tokens,
        estimated_cost: total_cost,
        actual_cost,
        total_hours,
        avg_session_duration: avg_duration,
        avg_messages_per_session: avg_messages,
        avg_tokens_per_session: avg_tokens,
        user_messages: message_stats.user_messages,
        assistant_messages: message_stats.assistant_messages,
        tool_messages: message_stats.tool_messages,
        date_range_start,
        date_range_end,
        models_with_pricing: models_with_pricing.into_iter().collect(),
        models_without_pricing: models_without_pricing.into_iter().collect(),
        unknown_cost_sessions,
        included_cost_sessions,
    }
}

// ---------------------------------------------------------------------------
// Model breakdown
// ---------------------------------------------------------------------------

pub(crate) fn compute_model_breakdown(sessions: &[SessionRow]) -> Vec<ModelBreakdownRow> {
    // BTreeMap keeps insertion-independent ordering for deterministic tests
    // before the explicit sort below.
    let mut model_data: BTreeMap<String, ModelBreakdownRow> = BTreeMap::new();

    for session in sessions {
        let raw_model = session.model.as_deref().unwrap_or("unknown");
        let display = if raw_model.is_empty() {
            "unknown".to_string()
        } else {
            display_model_name(raw_model)
        };

        let entry = model_data
            .entry(display.clone())
            .or_insert_with(|| ModelBreakdownRow {
                model: display.clone(),
                ..Default::default()
            });

        entry.sessions += 1;
        let inp = session.input_tokens.unwrap_or(0);
        let out = session.output_tokens.unwrap_or(0);
        let cr = session.cache_read_tokens.unwrap_or(0);
        let cw = session.cache_write_tokens.unwrap_or(0);
        entry.input_tokens += inp;
        entry.output_tokens += out;
        entry.cache_read_tokens += cr;
        entry.cache_write_tokens += cw;
        entry.total_tokens += inp + out + cr + cw;
        entry.tool_calls += session.tool_call_count.unwrap_or(0);

        let (cost, status) = estimate_session_cost(session);
        entry.cost += cost;
        entry.has_pricing = has_known_pricing(
            raw_model,
            session.billing_provider.as_deref(),
            session.billing_base_url.as_deref(),
        );
        entry.cost_status = cost_status_label(&status).to_string();
    }

    let mut rows: Vec<ModelBreakdownRow> = model_data.into_values().collect();
    rows.sort_by(|a, b| {
        b.total_tokens
            .cmp(&a.total_tokens)
            .then_with(|| b.sessions.cmp(&a.sessions))
            .then_with(|| a.model.cmp(&b.model))
    });
    rows
}

// ---------------------------------------------------------------------------
// Platform breakdown
// ---------------------------------------------------------------------------

pub(crate) fn compute_platform_breakdown(sessions: &[SessionRow]) -> Vec<PlatformBreakdownRow> {
    let mut platform_data: BTreeMap<String, PlatformBreakdownRow> = BTreeMap::new();

    for session in sessions {
        let platform = session
            .source
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let entry = platform_data
            .entry(platform.clone())
            .or_insert_with(|| PlatformBreakdownRow {
                platform: platform.clone(),
                ..Default::default()
            });

        entry.sessions += 1;
        entry.messages += session.message_count.unwrap_or(0);
        let inp = session.input_tokens.unwrap_or(0);
        let out = session.output_tokens.unwrap_or(0);
        let cr = session.cache_read_tokens.unwrap_or(0);
        let cw = session.cache_write_tokens.unwrap_or(0);
        entry.input_tokens += inp;
        entry.output_tokens += out;
        entry.cache_read_tokens += cr;
        entry.cache_write_tokens += cw;
        entry.total_tokens += inp + out + cr + cw;
        entry.tool_calls += session.tool_call_count.unwrap_or(0);
    }

    let mut rows: Vec<PlatformBreakdownRow> = platform_data.into_values().collect();
    rows.sort_by(|a, b| {
        b.sessions
            .cmp(&a.sessions)
            .then_with(|| a.platform.cmp(&b.platform))
    });
    rows
}

// ---------------------------------------------------------------------------
// Tool breakdown
// ---------------------------------------------------------------------------

pub(crate) fn compute_tool_breakdown(usage: &[ToolUsageRow]) -> Vec<ToolBreakdownRow> {
    let total: u64 = usage.iter().map(|t| t.count).sum();
    usage
        .iter()
        .map(|t| ToolBreakdownRow {
            tool: t.tool_name.clone(),
            count: t.count,
            percentage: if total == 0 {
                0.0
            } else {
                (t.count as f64) / (total as f64) * 100.0
            },
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Activity patterns
// ---------------------------------------------------------------------------

const DAY_NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

pub(crate) fn compute_activity_patterns(sessions: &[SessionRow]) -> ActivityPatterns {
    let mut day_counts = HashMap::<u8, u64>::new();
    let mut hour_counts = HashMap::<u8, u64>::new();
    // Use a BTreeMap so date strings stay sorted ascending for the streak
    // calculation without an extra sort step.
    let mut daily_counts = BTreeMap::<String, u64>::new();

    for s in sessions {
        let Some(ts) = s.started_at else { continue };
        let Some(dt) = DateTime::<Utc>::from_timestamp(ts as i64, 0) else {
            continue;
        };
        let weekday = dt.weekday().num_days_from_monday() as u8;
        *day_counts.entry(weekday).or_insert(0) += 1;
        *hour_counts.entry(dt.hour() as u8).or_insert(0) += 1;
        let date_key = dt.format("%Y-%m-%d").to_string();
        *daily_counts.entry(date_key).or_insert(0) += 1;
    }

    let by_day: Vec<DayCount> = (0..7u8)
        .map(|i| DayCount {
            day: DAY_NAMES[i as usize].to_string(),
            count: *day_counts.get(&i).unwrap_or(&0),
        })
        .collect();

    let by_hour: Vec<HourCount> = (0..24u8)
        .map(|i| HourCount {
            hour: i,
            count: *hour_counts.get(&i).unwrap_or(&0),
        })
        .collect();

    let busiest_day = by_day.iter().max_by_key(|d| d.count).cloned();
    let busiest_hour = by_hour.iter().max_by_key(|h| h.count).cloned();

    let active_days = daily_counts.len() as u64;

    // Streak: longest run of consecutive UTC dates with at least one
    // session.
    let mut max_streak: u64 = 0;
    if !daily_counts.is_empty() {
        max_streak = 1;
        let dates: Vec<chrono::NaiveDate> = daily_counts
            .keys()
            .filter_map(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .collect();
        let mut current = 1u64;
        for win in dates.windows(2) {
            let diff = (win[1] - win[0]).num_days();
            if diff == 1 {
                current += 1;
                if current > max_streak {
                    max_streak = current;
                }
            } else {
                current = 1;
            }
        }
    }

    ActivityPatterns {
        by_day,
        by_hour,
        busiest_day,
        busiest_hour,
        active_days,
        max_streak,
    }
}

// ---------------------------------------------------------------------------
// Top sessions
// ---------------------------------------------------------------------------

pub(crate) fn compute_top_sessions(sessions: &[SessionRow]) -> Vec<TopSessionRow> {
    let mut top: Vec<TopSessionRow> = Vec::new();
    if sessions.is_empty() {
        return top;
    }

    // Longest by duration.
    let with_duration: Vec<&SessionRow> = sessions
        .iter()
        .filter(|s| s.started_at.is_some() && s.ended_at.is_some())
        .collect();
    if let Some(longest) = with_duration.iter().max_by(|a, b| {
        let da = a.ended_at.unwrap() - a.started_at.unwrap();
        let db = b.ended_at.unwrap() - b.started_at.unwrap();
        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
    }) {
        let dur = longest.ended_at.unwrap() - longest.started_at.unwrap();
        top.push(TopSessionRow {
            label: "Longest session".into(),
            session_id: short_id(&longest.id),
            value: crate::usage_pricing::format_duration_compact(dur),
            date: format_short_date(longest.started_at.unwrap()),
        });
    }

    // Most messages.
    if let Some(most) = sessions.iter().max_by_key(|s| s.message_count.unwrap_or(0)) {
        let count = most.message_count.unwrap_or(0);
        if count > 0 {
            top.push(TopSessionRow {
                label: "Most messages".into(),
                session_id: short_id(&most.id),
                value: format!("{count} msgs"),
                date: most
                    .started_at
                    .map(format_short_date)
                    .unwrap_or_else(|| "?".into()),
            });
        }
    }

    // Most tokens (input + output, matching Python reference).
    if let Some(most) = sessions
        .iter()
        .max_by_key(|s| s.input_tokens.unwrap_or(0) + s.output_tokens.unwrap_or(0))
    {
        let total = most.input_tokens.unwrap_or(0) + most.output_tokens.unwrap_or(0);
        if total > 0 {
            top.push(TopSessionRow {
                label: "Most tokens".into(),
                session_id: short_id(&most.id),
                value: format!("{} tokens", format_with_commas(total)),
                date: most
                    .started_at
                    .map(format_short_date)
                    .unwrap_or_else(|| "?".into()),
            });
        }
    }

    // Most tool calls.
    if let Some(most) = sessions
        .iter()
        .max_by_key(|s| s.tool_call_count.unwrap_or(0))
    {
        let count = most.tool_call_count.unwrap_or(0);
        if count > 0 {
            top.push(TopSessionRow {
                label: "Most tool calls".into(),
                session_id: short_id(&most.id),
                value: format!("{count} calls"),
                date: most
                    .started_at
                    .map(format_short_date)
                    .unwrap_or_else(|| "?".into()),
            });
        }
    }

    top
}

fn short_id(id: &str) -> String {
    id.chars().take(16).collect()
}

fn format_short_date(ts: f64) -> String {
    DateTime::<Utc>::from_timestamp(ts as i64, 0)
        .map(|dt| dt.format("%b %d").to_string())
        .unwrap_or_else(|| "?".into())
}

/// Format an integer with comma separators (e.g. `1,234,567`).
pub(crate) fn format_with_commas(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}
