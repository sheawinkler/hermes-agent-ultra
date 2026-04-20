//! Historical session usage insights.
//!
//! Port of Python `agent.insights` (790 LoC). The engine consumes session +
//! message rows from a pluggable [`SessionDataSource`] and produces a
//! structured [`InsightsReport`] plus terminal/gateway formatted strings.
//!
//! # Layered design
//!
//! - [`types`]      — strongly-typed input rows and computed report
//! - [`source`]     — `SessionDataSource` trait and `InMemorySessionData`
//! - [`engine`]     — pure compute functions and `SessionInsightsEngine`
//! - [`format`]     — `format_terminal` / `format_gateway` renderers
//!
//! The crate-level [`Insights`](crate::insights::Insights) struct (built on
//! `UsageTracker`) is unrelated and kept for in-memory analytics.

pub mod engine;
pub mod format;
pub mod source;
pub mod types;

pub use engine::SessionInsightsEngine;
pub use format::{format_gateway, format_terminal};
pub use source::{AssistantToolCallRow, InMemorySessionData, SessionDataSource, ToolMessageRow};
pub use types::{
    ActivityPatterns, DayCount, HourCount, InsightsReport, MessageStats, ModelBreakdownRow,
    Overview, PlatformBreakdownRow, SessionRow, ToolBreakdownRow, ToolUsageRow, TopSessionRow,
};

#[cfg(test)]
mod tests {
    use super::*;

    // 2024-01-15 12:00:00 UTC = 1705320000 (Monday).
    const FIXED_NOW: f64 = 1_705_924_800.0; // 2024-01-22 12:00 UTC (Monday)

    fn ts(days_ago: f64, hours: f64) -> f64 {
        FIXED_NOW - days_ago * 86_400.0 + hours * 3600.0
    }

    fn session(
        id: &str,
        source: &str,
        model: &str,
        started_offset_days: f64,
        duration_seconds: f64,
        message_count: u64,
        tool_call_count: u64,
        input: u64,
        output: u64,
    ) -> SessionRow {
        // Derive `billing_provider` from the leading `provider/model` segment
        // when present so `has_known_pricing` (which expects an explicit
        // provider) sees what real callers populate from the SQL row.
        let billing_provider = model
            .split_once('/')
            .map(|(p, _)| p.to_string())
            .or_else(|| Some("unknown".to_string()));
        let start = ts(started_offset_days, 0.0);
        SessionRow {
            id: id.to_string(),
            source: Some(source.to_string()),
            model: Some(model.to_string()),
            started_at: Some(start),
            ended_at: Some(start + duration_seconds),
            message_count: Some(message_count),
            tool_call_count: Some(tool_call_count),
            input_tokens: Some(input),
            output_tokens: Some(output),
            billing_provider,
            ..Default::default()
        }
    }

    fn data_with_two_sessions() -> InMemorySessionData {
        let mut data = InMemorySessionData::new();
        data.add_session(session(
            "abcdef0123456789xxxxxxxx",
            "cli",
            "anthropic/claude-sonnet-4-20250514",
            1.0,
            600.0,
            10,
            3,
            5_000,
            2_500,
        ))
        .add_session(session(
            "0987654321fedcbayyyyyyyy",
            "telegram",
            "openai/gpt-4o",
            3.0,
            1200.0,
            20,
            7,
            12_000,
            6_000,
        ))
        .add_tool_message("abcdef0123456789xxxxxxxx", Some("cli".into()), "shell", 2)
        .add_tool_message(
            "0987654321fedcbayyyyyyyy",
            Some("telegram".into()),
            "shell",
            5,
        )
        .add_tool_message(
            "0987654321fedcbayyyyyyyy",
            Some("telegram".into()),
            "fs_read",
            2,
        )
        .set_message_stats(MessageStats {
            total_messages: 30,
            user_messages: 12,
            assistant_messages: 12,
            tool_messages: 6,
        });
        data
    }

    // ---------- empty / no data ----------

    #[test]
    fn generates_empty_report_when_no_sessions() {
        let data = InMemorySessionData::new();
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(7, None, FIXED_NOW);
        assert!(report.empty);
        assert_eq!(report.days, 7);
        assert!(report.models.is_empty());
        assert!(report.tools.is_empty());

        let term = format_terminal(&report);
        assert!(term.contains("No sessions found"));
    }

    #[test]
    fn generates_empty_report_when_filter_matches_nothing() {
        let data = data_with_two_sessions();
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(30, Some("slack"), FIXED_NOW);
        assert!(report.empty);
        assert_eq!(report.source_filter.as_deref(), Some("slack"));
    }

    // ---------- overview ----------

    #[test]
    fn overview_aggregates_tokens_messages_and_costs() {
        let data = data_with_two_sessions();
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(7, None, FIXED_NOW);

        assert!(!report.empty);
        let o = &report.overview;
        assert_eq!(o.total_sessions, 2);
        assert_eq!(o.total_input_tokens, 17_000);
        assert_eq!(o.total_output_tokens, 8_500);
        assert_eq!(o.total_tokens, 25_500);
        assert_eq!(o.total_messages, 30); // sum of message_count fields
        assert_eq!(o.total_tool_calls, 10);
        assert!(o.estimated_cost > 0.0); // both models have known pricing
        assert!(o.total_hours > 0.0);
        assert!(o.avg_session_duration > 0.0);
        assert!((o.avg_messages_per_session - 15.0).abs() < 1e-6);
        assert!(o.date_range_start.is_some());
        assert!(o.date_range_end.is_some());
        assert_eq!(
            o.date_range_start.unwrap() <= o.date_range_end.unwrap(),
            true
        );
    }

    #[test]
    fn overview_categorises_models_by_pricing_availability() {
        let mut data = InMemorySessionData::new();
        data.add_session(session(
            "id-1",
            "cli",
            "openai/gpt-4o",
            1.0,
            60.0,
            5,
            1,
            100,
            50,
        ))
        .add_session(session(
            "id-2",
            "cli",
            "self-hosted/llama-3-very-custom",
            1.0,
            60.0,
            5,
            1,
            100,
            50,
        ));
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(7, None, FIXED_NOW);
        assert_eq!(report.overview.models_with_pricing, vec!["gpt-4o"]);
        assert_eq!(
            report.overview.models_without_pricing,
            vec!["llama-3-very-custom"]
        );
    }

    // ---------- model / platform breakdown ----------

    #[test]
    fn model_breakdown_sorts_by_total_tokens_descending() {
        let data = data_with_two_sessions();
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(7, None, FIXED_NOW);
        assert_eq!(report.models.len(), 2);
        assert_eq!(report.models[0].model, "gpt-4o");
        assert_eq!(report.models[0].total_tokens, 18_000);
        assert_eq!(report.models[1].model, "claude-sonnet-4-20250514");
        assert_eq!(report.models[1].total_tokens, 7_500);
        assert!(report.models[0].has_pricing);
        assert!(report.models[1].has_pricing);
    }

    #[test]
    fn platform_breakdown_sorts_by_session_count() {
        let mut data = InMemorySessionData::new();
        data.add_session(session(
            "a",
            "telegram",
            "openai/gpt-4o",
            1.0,
            60.0,
            5,
            1,
            100,
            50,
        ))
        .add_session(session(
            "b",
            "telegram",
            "openai/gpt-4o",
            1.0,
            60.0,
            5,
            1,
            100,
            50,
        ))
        .add_session(session(
            "c",
            "cli",
            "openai/gpt-4o",
            1.0,
            60.0,
            5,
            1,
            100,
            50,
        ));
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(7, None, FIXED_NOW);
        assert_eq!(report.platforms.len(), 2);
        assert_eq!(report.platforms[0].platform, "telegram");
        assert_eq!(report.platforms[0].sessions, 2);
        assert_eq!(report.platforms[1].platform, "cli");
        assert_eq!(report.platforms[1].sessions, 1);
    }

    // ---------- tool breakdown ----------

    #[test]
    fn tool_breakdown_uses_tool_messages_when_present() {
        let data = data_with_two_sessions();
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(7, None, FIXED_NOW);
        // shell: 2 (cli) + 5 (telegram) = 7, fs_read: 2 → total 9
        assert_eq!(report.tools.len(), 2);
        assert_eq!(report.tools[0].tool, "shell");
        assert_eq!(report.tools[0].count, 7);
        assert!((report.tools[0].percentage - (7.0 / 9.0 * 100.0)).abs() < 1e-6);
    }

    #[test]
    fn tool_breakdown_falls_back_to_assistant_tool_calls() {
        let mut data = InMemorySessionData::new();
        data.add_session(session(
            "asst-only",
            "cli",
            "openai/gpt-4o",
            1.0,
            60.0,
            5,
            1,
            100,
            50,
        ))
        .add_assistant_tool_call(
            "asst-only",
            Some("cli".into()),
            vec!["fs_write".into(), "shell".into()],
        )
        .add_assistant_tool_call("asst-only", Some("cli".into()), vec!["fs_write".into()]);
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(7, None, FIXED_NOW);
        assert_eq!(report.tools.len(), 2);
        // fs_write appeared twice, shell once → fs_write first
        assert_eq!(report.tools[0].tool, "fs_write");
        assert_eq!(report.tools[0].count, 2);
        assert_eq!(report.tools[1].tool, "shell");
        assert_eq!(report.tools[1].count, 1);
    }

    #[test]
    fn tool_breakdown_takes_max_when_both_sources_have_data() {
        let mut data = InMemorySessionData::new();
        data.add_session(session(
            "s",
            "cli",
            "openai/gpt-4o",
            1.0,
            60.0,
            5,
            1,
            100,
            50,
        ))
        .add_tool_message("s", Some("cli".into()), "shell", 2)
        .add_assistant_tool_call("s", Some("cli".into()), vec!["shell".into()])
        .add_assistant_tool_call("s", Some("cli".into()), vec!["shell".into()])
        .add_assistant_tool_call("s", Some("cli".into()), vec!["shell".into()]);
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(7, None, FIXED_NOW);
        // tool_messages says 2, assistant_tool_calls says 3 → take max = 3
        assert_eq!(report.tools[0].count, 3);
    }

    // ---------- activity patterns ----------

    #[test]
    fn activity_patterns_compute_busiest_day_and_hour() {
        let mut data = InMemorySessionData::new();
        // FIXED_NOW = 2024-01-22 12:00 UTC (Monday).
        // Three sessions on the same Monday, two on Wednesday.
        for i in 0..3 {
            data.add_session(session(
                &format!("mon-{i}"),
                "cli",
                "openai/gpt-4o",
                0.0,
                60.0,
                1,
                0,
                10,
                5,
            ));
        }
        for i in 0..2 {
            data.add_session(session(
                &format!("wed-{i}"),
                "cli",
                "openai/gpt-4o",
                -2.0, // Wednesday
                60.0,
                1,
                0,
                10,
                5,
            ));
        }
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(30, None, FIXED_NOW);
        let busiest_day = report.activity.busiest_day.unwrap();
        assert_eq!(busiest_day.day, "Mon");
        assert_eq!(busiest_day.count, 3);
        assert!(report
            .activity
            .by_day
            .iter()
            .any(|d| d.day == "Wed" && d.count == 2));
        assert!(report.activity.active_days >= 2);
    }

    #[test]
    fn activity_streak_counts_consecutive_dates() {
        let mut data = InMemorySessionData::new();
        // Three consecutive days: yesterday, day before, two days before.
        for i in 0..3 {
            data.add_session(session(
                &format!("d-{i}"),
                "cli",
                "openai/gpt-4o",
                (i + 1) as f64, // 1, 2, 3 days ago
                60.0,
                1,
                0,
                10,
                5,
            ));
        }
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(30, None, FIXED_NOW);
        assert_eq!(report.activity.max_streak, 3);
    }

    // ---------- top sessions ----------

    #[test]
    fn top_sessions_includes_longest_most_messages_tokens_and_tools() {
        let data = data_with_two_sessions();
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(7, None, FIXED_NOW);
        let labels: Vec<&str> = report
            .top_sessions
            .iter()
            .map(|t| t.label.as_str())
            .collect();
        assert!(labels.contains(&"Longest session"));
        assert!(labels.contains(&"Most messages"));
        assert!(labels.contains(&"Most tokens"));
        assert!(labels.contains(&"Most tool calls"));
    }

    #[test]
    fn top_sessions_session_id_is_truncated_to_16_chars() {
        let data = data_with_two_sessions();
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(7, None, FIXED_NOW);
        for ts in &report.top_sessions {
            assert!(ts.session_id.len() <= 16);
        }
    }

    // ---------- formatters ----------

    #[test]
    fn format_terminal_renders_header_and_overview() {
        let data = data_with_two_sessions();
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(7, None, FIXED_NOW);
        let out = format_terminal(&report);
        assert!(out.contains("Hermes Insights"));
        assert!(out.contains("Last 7 days"));
        assert!(out.contains("Overview"));
        assert!(out.contains("Sessions:"));
        assert!(out.contains("Models Used"));
        assert!(out.contains("gpt-4o"));
        assert!(out.contains("Top Tools"));
        assert!(out.contains("shell"));
        assert!(out.contains("Activity Patterns"));
    }

    #[test]
    fn format_terminal_handles_empty_with_filter() {
        let data = InMemorySessionData::new();
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(14, Some("discord"), FIXED_NOW);
        let out = format_terminal(&report);
        assert!(out.contains("No sessions"));
        assert!(out.contains("14 days"));
        assert!(out.contains("discord"));
    }

    #[test]
    fn format_gateway_uses_markdown_and_caps_lists() {
        let data = data_with_two_sessions();
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(7, None, FIXED_NOW);
        let out = format_gateway(&report);
        assert!(out.starts_with("📊 **Hermes Insights**"));
        assert!(out.contains("**Sessions:**"));
        assert!(out.contains("**Tokens:**"));
        assert!(out.contains("**🤖 Models:**"));
        assert!(out.contains("**🔧 Top Tools:**"));
        // Multi-platform present in the fixture.
        assert!(out.contains("**📱 Platforms:**"));
    }

    #[test]
    fn format_gateway_omits_platforms_when_only_one() {
        let mut data = InMemorySessionData::new();
        data.add_session(session(
            "a",
            "cli",
            "openai/gpt-4o",
            1.0,
            60.0,
            5,
            1,
            100,
            50,
        ))
        .add_session(session(
            "b",
            "cli",
            "openai/gpt-4o",
            1.0,
            60.0,
            5,
            1,
            100,
            50,
        ));
        let engine = SessionInsightsEngine::new(&data);
        let report = engine.generate(7, None, FIXED_NOW);
        let out = format_gateway(&report);
        assert!(!out.contains("**📱 Platforms:**"));
    }
}
