//! Terminal & gateway formatters for [`InsightsReport`].
//!
//! Mirrors `agent.insights.InsightsEngine.format_terminal` and
//! `format_gateway` line-for-line. The output is plain ASCII / unicode and
//! contains no colour codes — callers can wrap or restyle as they wish.

use chrono::{DateTime, Utc};

use crate::usage_pricing::format_duration_compact;

use super::engine::format_with_commas;
use super::types::{InsightsReport, ToolBreakdownRow};

/// Render an insights report for a CLI / terminal audience.
pub fn format_terminal(report: &InsightsReport) -> String {
    if report.empty {
        let src = match report.source_filter.as_deref() {
            Some(s) => format!(" (source: {s})"),
            None => String::new(),
        };
        return format!("  No sessions found in the last {} days{src}.", report.days);
    }

    let mut lines: Vec<String> = Vec::new();
    let o = &report.overview;
    let days = report.days;

    // Header.
    lines.push(String::new());
    lines.push("  ╔══════════════════════════════════════════════════════════╗".into());
    lines.push("  ║                    📊 Hermes Insights                    ║".into());
    let mut period = format!("Last {days} days");
    if let Some(src) = report.source_filter.as_deref() {
        period.push_str(&format!(" ({src})"));
    }
    let inner_width = 58usize;
    let padding = inner_width.saturating_sub(period.len() + 2);
    let left_pad = padding / 2;
    let right_pad = padding - left_pad;
    lines.push(format!(
        "  ║{} {} {}║",
        " ".repeat(left_pad),
        period,
        " ".repeat(right_pad)
    ));
    lines.push("  ╚══════════════════════════════════════════════════════════╝".into());
    lines.push(String::new());

    // Date range.
    if let (Some(start), Some(end)) = (o.date_range_start, o.date_range_end) {
        let s = format_full_date(start);
        let e = format_full_date(end);
        lines.push(format!("  Period: {s} — {e}"));
        lines.push(String::new());
    }

    // Overview.
    lines.push("  📋 Overview".into());
    lines.push(format!("  {}", "─".repeat(56)));
    lines.push(format!(
        "  Sessions:          {:<12}  Messages:        {}",
        o.total_sessions,
        format_with_commas(o.total_messages)
    ));
    lines.push(format!(
        "  Tool calls:        {:<12}  User messages:   {}",
        format_with_commas(o.total_tool_calls),
        format_with_commas(o.user_messages)
    ));
    lines.push(format!(
        "  Input tokens:      {:<12}  Output tokens:   {}",
        format_with_commas(o.total_input_tokens),
        format_with_commas(o.total_output_tokens)
    ));
    let cache_total = o.total_cache_read_tokens + o.total_cache_write_tokens;
    if cache_total > 0 {
        lines.push(format!(
            "  Cache read:        {:<12}  Cache write:     {}",
            format_with_commas(o.total_cache_read_tokens),
            format_with_commas(o.total_cache_write_tokens)
        ));
    }
    let mut cost_str = format!("${:.2}", o.estimated_cost);
    if !o.models_without_pricing.is_empty() {
        cost_str.push_str(" *");
    }
    lines.push(format!(
        "  Total tokens:      {:<12}  Est. cost:       {cost_str}",
        format_with_commas(o.total_tokens)
    ));
    if o.total_hours > 0.0 {
        lines.push(format!(
            "  Active time:       ~{:<11}  Avg session:     ~{}",
            format_duration_compact(o.total_hours * 3600.0),
            format_duration_compact(o.avg_session_duration)
        ));
    }
    lines.push(format!(
        "  Avg msgs/session:  {:.1}",
        o.avg_messages_per_session
    ));
    lines.push(String::new());

    // Models.
    if !report.models.is_empty() {
        lines.push("  🤖 Models Used".into());
        lines.push(format!("  {}", "─".repeat(56)));
        lines.push(format!(
            "  {:<30} {:>8} {:>12} {:>8}",
            "Model", "Sessions", "Tokens", "Cost"
        ));
        for m in &report.models {
            let mut name = m.model.clone();
            name.truncate(28);
            let cost_cell = if m.has_pricing {
                format!("${:>6.2}", m.cost)
            } else {
                "     N/A".into()
            };
            lines.push(format!(
                "  {:<30} {:>8} {:>12} {}",
                name,
                m.sessions,
                format_with_commas(m.total_tokens),
                cost_cell
            ));
        }
        if !o.models_without_pricing.is_empty() {
            lines.push("  * Cost N/A for custom/self-hosted models".into());
        }
        lines.push(String::new());
    }

    // Platforms.
    let multi_platform = report.platforms.len() > 1
        || report
            .platforms
            .first()
            .map(|p| p.platform != "cli")
            .unwrap_or(false);
    if multi_platform && !report.platforms.is_empty() {
        lines.push("  📱 Platforms".into());
        lines.push(format!("  {}", "─".repeat(56)));
        lines.push(format!(
            "  {:<14} {:>8} {:>10} {:>14}",
            "Platform", "Sessions", "Messages", "Tokens"
        ));
        for p in &report.platforms {
            lines.push(format!(
                "  {:<14} {:>8} {:>10} {:>14}",
                p.platform,
                p.sessions,
                format_with_commas(p.messages),
                format_with_commas(p.total_tokens)
            ));
        }
        lines.push(String::new());
    }

    // Tools (top 15).
    if !report.tools.is_empty() {
        lines.push("  🔧 Top Tools".into());
        lines.push(format!("  {}", "─".repeat(56)));
        lines.push(format!("  {:<28} {:>8} {:>8}", "Tool", "Calls", "%"));
        for t in report.tools.iter().take(15) {
            lines.push(format_tool_row(t));
        }
        if report.tools.len() > 15 {
            lines.push(format!("  ... and {} more tools", report.tools.len() - 15));
        }
        lines.push(String::new());
    }

    // Activity patterns.
    if !report.activity.by_day.is_empty() {
        lines.push("  📅 Activity Patterns".into());
        lines.push(format!("  {}", "─".repeat(56)));

        let day_values: Vec<u64> = report.activity.by_day.iter().map(|d| d.count).collect();
        let bars = bar_chart(&day_values, 15);
        for (i, d) in report.activity.by_day.iter().enumerate() {
            let bar = bars.get(i).cloned().unwrap_or_default();
            lines.push(format!("  {}  {:<15} {}", d.day, bar, d.count));
        }
        lines.push(String::new());

        // Top 5 busiest hours.
        let mut busy_hours = report.activity.by_hour.clone();
        busy_hours.sort_by(|a, b| b.count.cmp(&a.count));
        let busy_hours: Vec<_> = busy_hours
            .into_iter()
            .filter(|h| h.count > 0)
            .take(5)
            .collect();
        if !busy_hours.is_empty() {
            let hour_strs: Vec<String> = busy_hours
                .iter()
                .map(|h| {
                    let ampm = if h.hour < 12 { "AM" } else { "PM" };
                    let display_hr = if h.hour % 12 == 0 { 12 } else { h.hour % 12 };
                    format!("{display_hr}{ampm} ({})", h.count)
                })
                .collect();
            lines.push(format!("  Peak hours: {}", hour_strs.join(", ")));
        }
        if report.activity.active_days > 0 {
            lines.push(format!("  Active days: {}", report.activity.active_days));
        }
        if report.activity.max_streak > 1 {
            lines.push(format!(
                "  Best streak: {} consecutive days",
                report.activity.max_streak
            ));
        }
        lines.push(String::new());
    }

    // Notable sessions.
    if !report.top_sessions.is_empty() {
        lines.push("  🏆 Notable Sessions".into());
        lines.push(format!("  {}", "─".repeat(56)));
        for ts in &report.top_sessions {
            lines.push(format!(
                "  {:<20} {:<18} ({}, {})",
                ts.label, ts.value, ts.date, ts.session_id
            ));
        }
        lines.push(String::new());
    }

    lines.join("\n")
}

/// Render an insights report for a chat / gateway audience (markdown).
pub fn format_gateway(report: &InsightsReport) -> String {
    if report.empty {
        return format!("No sessions found in the last {} days.", report.days);
    }

    let mut lines: Vec<String> = Vec::new();
    let o = &report.overview;
    let days = report.days;

    lines.push(format!("📊 **Hermes Insights** — Last {days} days\n"));
    lines.push(format!(
        "**Sessions:** {} | **Messages:** {} | **Tool calls:** {}",
        o.total_sessions,
        format_with_commas(o.total_messages),
        format_with_commas(o.total_tool_calls)
    ));
    let cache_total = o.total_cache_read_tokens + o.total_cache_write_tokens;
    if cache_total > 0 {
        lines.push(format!(
            "**Tokens:** {} (in: {} / out: {} / cache: {})",
            format_with_commas(o.total_tokens),
            format_with_commas(o.total_input_tokens),
            format_with_commas(o.total_output_tokens),
            format_with_commas(cache_total)
        ));
    } else {
        lines.push(format!(
            "**Tokens:** {} (in: {} / out: {})",
            format_with_commas(o.total_tokens),
            format_with_commas(o.total_input_tokens),
            format_with_commas(o.total_output_tokens)
        ));
    }
    let cost_note = if !o.models_without_pricing.is_empty() {
        " _(excludes custom/self-hosted models)_"
    } else {
        ""
    };
    lines.push(format!(
        "**Est. cost:** ${:.2}{cost_note}",
        o.estimated_cost
    ));
    if o.total_hours > 0.0 {
        lines.push(format!(
            "**Active time:** ~{} | **Avg session:** ~{}",
            format_duration_compact(o.total_hours * 3600.0),
            format_duration_compact(o.avg_session_duration)
        ));
    }
    lines.push(String::new());

    // Models (top 5).
    if !report.models.is_empty() {
        lines.push("**🤖 Models:**".into());
        for m in report.models.iter().take(5) {
            let cost_str = if m.has_pricing {
                format!("${:.2}", m.cost)
            } else {
                "N/A".into()
            };
            let mut name = m.model.clone();
            name.truncate(25);
            lines.push(format!(
                "  {} — {} sessions, {} tokens, {}",
                name,
                m.sessions,
                format_with_commas(m.total_tokens),
                cost_str
            ));
        }
        lines.push(String::new());
    }

    // Platforms (only when multi-platform).
    if report.platforms.len() > 1 {
        lines.push("**📱 Platforms:**".into());
        for p in &report.platforms {
            lines.push(format!(
                "  {} — {} sessions, {} msgs",
                p.platform,
                p.sessions,
                format_with_commas(p.messages)
            ));
        }
        lines.push(String::new());
    }

    // Tools (top 8).
    if !report.tools.is_empty() {
        lines.push("**🔧 Top Tools:**".into());
        for t in report.tools.iter().take(8) {
            lines.push(format!(
                "  {} — {} calls ({:.1}%)",
                t.tool,
                format_with_commas(t.count),
                t.percentage
            ));
        }
        lines.push(String::new());
    }

    // Activity summary.
    if let (Some(busiest_day), Some(busiest_hour)) = (
        report.activity.busiest_day.as_ref(),
        report.activity.busiest_hour.as_ref(),
    ) {
        let ampm = if busiest_hour.hour < 12 { "AM" } else { "PM" };
        let display_hr = if busiest_hour.hour % 12 == 0 {
            12
        } else {
            busiest_hour.hour % 12
        };
        lines.push(format!(
            "**📅 Busiest:** {}s ({} sessions), {}{} ({} sessions)",
            busiest_day.day, busiest_day.count, display_hr, ampm, busiest_hour.count
        ));
        if report.activity.active_days > 0 {
            lines.push(format!("**Active days:** {}", report.activity.active_days));
        }
        if report.activity.max_streak > 1 {
            lines.push(format!(
                "**Best streak:** {} consecutive days",
                report.activity.max_streak
            ));
        }
    }

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn format_full_date(ts: f64) -> String {
    DateTime::<Utc>::from_timestamp(ts as i64, 0)
        .map(|dt| dt.format("%b %d, %Y").to_string())
        .unwrap_or_else(|| "?".into())
}

fn format_tool_row(t: &ToolBreakdownRow) -> String {
    let mut tool = t.tool.clone();
    if tool.len() > 28 {
        tool.truncate(28);
    }
    format!(
        "  {:<28} {:>8} {:>7.1}%",
        tool,
        format_with_commas(t.count),
        t.percentage
    )
}

fn bar_chart(values: &[u64], max_width: usize) -> Vec<String> {
    let peak = *values.iter().max().unwrap_or(&0);
    if peak == 0 {
        return vec![String::new(); values.len()];
    }
    values
        .iter()
        .map(|v| {
            if *v == 0 {
                String::new()
            } else {
                let n = ((*v as f64) / (peak as f64) * (max_width as f64)) as usize;
                "█".repeat(n.max(1))
            }
        })
        .collect()
}
