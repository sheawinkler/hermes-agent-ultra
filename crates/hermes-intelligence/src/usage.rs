//! Usage/pricing tracker — records token usage and computes cost summaries.
//!
//! Requirement 16.3

use std::collections::HashMap;

use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ModelPricing
// ---------------------------------------------------------------------------

/// Pricing information for a model (cost per 1K tokens).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub model: String,
    /// Cost per 1K input tokens in USD.
    pub input_per_1k: f64,
    /// Cost per 1K output tokens in USD.
    pub output_per_1k: f64,
}

// ---------------------------------------------------------------------------
// UsageRecord
// ---------------------------------------------------------------------------

/// A single usage record for a model call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub timestamp: chrono::DateTime<Utc>,
    pub estimated_cost: f64,
}

// ---------------------------------------------------------------------------
// ModelUsage
// ---------------------------------------------------------------------------

/// Aggregated usage for a single model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
    pub request_count: u64,
}

// ---------------------------------------------------------------------------
// UsageSummary
// ---------------------------------------------------------------------------

/// Summary of usage over a time period.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageSummary {
    pub total_tokens: u64,
    pub total_cost: f64,
    pub by_model: HashMap<String, ModelUsage>,
}

// ---------------------------------------------------------------------------
// UsageTracker
// ---------------------------------------------------------------------------

/// Tracks token usage and computes cost estimates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageTracker {
    pub records: Vec<UsageRecord>,
    pub pricing: HashMap<String, ModelPricing>,
}

impl UsageTracker {
    /// Create a new tracker with no records.
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            pricing: HashMap::new(),
        }
    }

    /// Create a tracker pre-loaded with common model pricing.
    pub fn with_default_pricing() -> Self {
        let mut tracker = Self::new();
        tracker.pricing.insert(
            "gpt-4o".into(),
            ModelPricing {
                model: "gpt-4o".into(),
                input_per_1k: 0.0025,
                output_per_1k: 0.01,
            },
        );
        tracker.pricing.insert(
            "gpt-4o-mini".into(),
            ModelPricing {
                model: "gpt-4o-mini".into(),
                input_per_1k: 0.00015,
                output_per_1k: 0.0006,
            },
        );
        tracker.pricing.insert(
            "claude-sonnet-4-20250514".into(),
            ModelPricing {
                model: "claude-sonnet-4-20250514".into(),
                input_per_1k: 0.003,
                output_per_1k: 0.015,
            },
        );
        tracker.pricing.insert(
            "claude-haiku-3-5-20241022".into(),
            ModelPricing {
                model: "claude-haiku-3-5-20241022".into(),
                input_per_1k: 0.0008,
                output_per_1k: 0.004,
            },
        );
        tracker.pricing.insert(
            "gemini-2.0-flash".into(),
            ModelPricing {
                model: "gemini-2.0-flash".into(),
                input_per_1k: 0.0001,
                output_per_1k: 0.0004,
            },
        );
        tracker.pricing.insert(
            "o3".into(),
            ModelPricing {
                model: "o3".into(),
                input_per_1k: 0.01,
                output_per_1k: 0.04,
            },
        );
        tracker
    }

    /// Register pricing for a model.
    pub fn add_pricing(&mut self, pricing: ModelPricing) {
        self.pricing.insert(pricing.model.clone(), pricing);
    }

    /// Estimate cost for a model call given prompt and completion tokens.
    pub fn estimate_cost(&self, model: &str, prompt_tokens: u64, completion_tokens: u64) -> f64 {
        self.pricing
            .get(model)
            .map(|p| {
                (prompt_tokens as f64 / 1000.0) * p.input_per_1k
                    + (completion_tokens as f64 / 1000.0) * p.output_per_1k
            })
            .unwrap_or(0.0)
    }

    /// Track a usage event.
    pub fn track(&mut self, model: &str, prompt_tokens: u64, completion_tokens: u64) {
        let estimated_cost = self.estimate_cost(model, prompt_tokens, completion_tokens);
        self.records.push(UsageRecord {
            model: model.to_string(),
            prompt_tokens,
            completion_tokens,
            timestamp: Utc::now(),
            estimated_cost,
        });
    }

    /// Get a usage summary for all records on a given date.
    pub fn get_usage_summary(&self, date: NaiveDate) -> UsageSummary {
        let mut summary = UsageSummary::default();

        for record in &self.records {
            if record.timestamp.date_naive() == date {
                summary.total_tokens += record.prompt_tokens + record.completion_tokens;
                summary.total_cost += record.estimated_cost;

                let entry = summary.by_model.entry(record.model.clone()).or_default();
                entry.prompt_tokens += record.prompt_tokens;
                entry.completion_tokens += record.completion_tokens;
                entry.cost += record.estimated_cost;
                entry.request_count += 1;
            }
        }

        summary
    }

    /// Get a usage summary for a date range (inclusive).
    pub fn get_usage_summary_range(&self, start: NaiveDate, end: NaiveDate) -> UsageSummary {
        let mut summary = UsageSummary::default();

        for record in &self.records {
            let date = record.timestamp.date_naive();
            if date >= start && date <= end {
                summary.total_tokens += record.prompt_tokens + record.completion_tokens;
                summary.total_cost += record.estimated_cost;

                let entry = summary.by_model.entry(record.model.clone()).or_default();
                entry.prompt_tokens += record.prompt_tokens;
                entry.completion_tokens += record.completion_tokens;
                entry.cost += record.estimated_cost;
                entry.request_count += 1;
            }
        }

        summary
    }

    /// Get the total number of tracked records.
    pub fn record_count(&self) -> usize {
        self.records.len()
    }

    /// Clear all records.
    pub fn clear(&mut self) {
        self.records.clear();
    }
}

impl Default for UsageTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_track_and_estimate() {
        let mut tracker = UsageTracker::with_default_pricing();
        tracker.track("gpt-4o", 1000, 500);
        assert_eq!(tracker.record_count(), 1);

        let cost = tracker.estimate_cost("gpt-4o", 1000, 500);
        let expected = (1000.0 / 1000.0) * 0.0025 + (500.0 / 1000.0) * 0.01;
        assert!((cost - expected).abs() < 1e-8);
    }

    #[test]
    fn test_unknown_model_cost() {
        let tracker = UsageTracker::with_default_pricing();
        let cost = tracker.estimate_cost("unknown-model", 1000, 500);
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn test_daily_summary() {
        let mut tracker = UsageTracker::with_default_pricing();
        tracker.track("gpt-4o", 1000, 500);
        tracker.track("claude-sonnet-4-20250514", 2000, 1000);

        let today = Utc::now().date_naive();
        let summary = tracker.get_usage_summary(today);
        assert_eq!(summary.total_tokens, 4500);
        assert!(summary.total_cost > 0.0);
        assert_eq!(summary.by_model.len(), 2);
    }

    #[test]
    fn test_clear() {
        let mut tracker = UsageTracker::with_default_pricing();
        tracker.track("gpt-4o", 100, 50);
        assert_eq!(tracker.record_count(), 1);
        tracker.clear();
        assert_eq!(tracker.record_count(), 0);
    }
}