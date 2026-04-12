//! Usage insights — higher-level analytics over usage data.
//!
//! Requirement 16.5

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{Datelike, Duration, NaiveDate, Utc};

use crate::usage::{ModelUsage, UsageSummary, UsageTracker};

// ---------------------------------------------------------------------------
// Insights
// ---------------------------------------------------------------------------

/// Provides analytics and insights over tracked usage data.
pub struct Insights {
    pub usage_tracker: Mutex<UsageTracker>,
}

impl Insights {
    /// Create a new Insights instance wrapping a UsageTracker.
    pub fn new(tracker: UsageTracker) -> Self {
        Self {
            usage_tracker: Mutex::new(tracker),
        }
    }

    /// Create an Insights instance with default pricing.
    pub fn with_default_pricing() -> Self {
        Self::new(UsageTracker::with_default_pricing())
    }

    /// Get a usage summary for a specific day.
    pub fn daily_summary(&self, date: NaiveDate) -> UsageSummary {
        let tracker = self.usage_tracker.lock().unwrap();
        tracker.get_usage_summary(date)
    }

    /// Get a usage summary for the week starting on `week_start` (Monday).
    pub fn weekly_summary(&self, week_start: NaiveDate) -> UsageSummary {
        let tracker = self.usage_tracker.lock().unwrap();
        let week_end = week_start + Duration::days(6);
        tracker.get_usage_summary_range(week_start, week_end)
    }

    /// Get the top N models by total cost.
    pub fn top_models(&self, limit: usize) -> Vec<(String, ModelUsage)> {
        let tracker = self.usage_tracker.lock().unwrap();

        let mut model_totals: HashMap<String, ModelUsage> = HashMap::new();
        for record in &tracker.records {
            let entry = model_totals.entry(record.model.clone()).or_default();
            entry.prompt_tokens += record.prompt_tokens;
            entry.completion_tokens += record.completion_tokens;
            entry.cost += record.estimated_cost;
            entry.request_count += 1;
        }

        let mut results: Vec<(String, ModelUsage)> = model_totals.into_iter().collect();
        results.sort_by(|a, b| b.1.cost.partial_cmp(&a.1.cost).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        results
    }

    /// Get daily cost trend over the last `days` days.
    pub fn cost_trend(&self, days: u32) -> Vec<(NaiveDate, f64)> {
        let tracker = self.usage_tracker.lock().unwrap();
        let today = Utc::now().date_naive();

        let mut trend = Vec::new();
        for i in (0..days).rev() {
            let date = today - Duration::days(i as i64);
            let summary = tracker.get_usage_summary(date);
            trend.push((date, summary.total_cost));
        }
        trend
    }

    /// Get daily total token trend over the last `days` days.
    pub fn token_trend(&self, days: u32) -> Vec<(NaiveDate, u64)> {
        let tracker = self.usage_tracker.lock().unwrap();
        let today = Utc::now().date_naive();

        let mut trend = Vec::new();
        for i in (0..days).rev() {
            let date = today - Duration::days(i as i64);
            let summary = tracker.get_usage_summary(date);
            trend.push((date, summary.total_tokens));
        }
        trend
    }

    /// Record a usage event (delegates to the inner tracker).
    pub fn track(&self, model: &str, prompt_tokens: u64, completion_tokens: u64) {
        let mut tracker = self.usage_tracker.lock().unwrap();
        tracker.track(model, prompt_tokens, completion_tokens);
    }

    /// Get the total number of tracked records.
    pub fn record_count(&self) -> usize {
        let tracker = self.usage_tracker.lock().unwrap();
        tracker.record_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daily_summary() {
        let insights = Insights::with_default_pricing();
        insights.track("gpt-4o", 1000, 500);
        insights.track("gpt-4o", 2000, 1000);

        let today = Utc::now().date_naive();
        let summary = insights.daily_summary(today);
        assert_eq!(summary.total_tokens, 4500);
        assert!(summary.total_cost > 0.0);
    }

    #[test]
    fn test_top_models() {
        let insights = Insights::with_default_pricing();
        insights.track("gpt-4o", 10000, 5000);
        insights.track("gemini-2.0-flash", 1000, 500);

        let top = insights.top_models(2);
        assert_eq!(top.len(), 2);
        // gpt-4o should be more expensive
        assert_eq!(top[0].0, "gpt-4o");
    }

    #[test]
    fn test_cost_trend() {
        let insights = Insights::with_default_pricing();
        insights.track("gpt-4o", 1000, 500);

        let trend = insights.cost_trend(7);
        assert_eq!(trend.len(), 7);
        // Today should have a cost > 0
        let today = Utc::now().date_naive();
        let today_entry = trend.iter().find(|(d, _)| *d == today);
        assert!(today_entry.map(|(_, c)| *c > 0.0).unwrap_or(false));
    }

    #[test]
    fn test_token_trend() {
        let insights = Insights::with_default_pricing();
        insights.track("gpt-4o", 1000, 500);

        let trend = insights.token_trend(7);
        assert_eq!(trend.len(), 7);
    }

    #[test]
    fn test_weekly_summary() {
        let insights = Insights::with_default_pricing();
        insights.track("gpt-4o", 1000, 500);

        let today = Utc::now().date_naive();
        let week_start = today - Duration::days(today.weekday().num_days_from_monday() as i64);
        let summary = insights.weekly_summary(week_start);
        assert!(summary.total_tokens > 0);
    }
}