//! Session-scoped usage counters and context-engine transition (Python `reset_session_state`).

use std::collections::HashMap;

use hermes_core::{Message, UsageStats};
use hermes_intelligence::usage_pricing::{
    calculate_cost, CanonicalUsage, CostResult, CostSource, CostStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::debug;

use crate::agent_loop::{AgentConfig, AgentLoop};
use crate::compression::ContextCompressor;

/// Cumulative token/cost counters for the active session (Python `AIAgent.session_*`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionUsageMetrics {
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub api_calls: u32,
    pub estimated_cost_usd: f64,
    pub cost_status: String,
    pub cost_source: String,
}

impl SessionUsageMetrics {
    pub fn reset(&mut self) {
        self.total_tokens = 0;
        self.input_tokens = 0;
        self.output_tokens = 0;
        self.prompt_tokens = 0;
        self.completion_tokens = 0;
        self.cache_read_tokens = 0;
        self.cache_write_tokens = 0;
        self.reasoning_tokens = 0;
        self.api_calls = 0;
        self.estimated_cost_usd = 0.0;
        self.cost_status = "unknown".to_string();
        self.cost_source = "none".to_string();
    }

    /// Accumulate one LLM response (Python `conversation_loop` session_* updates).
    pub fn accumulate_api_call(
        &mut self,
        usage: &UsageStats,
        model: &str,
        provider: Option<&str>,
        base_url: Option<&str>,
    ) {
        self.api_calls = self.api_calls.saturating_add(1);
        self.prompt_tokens = self.prompt_tokens.saturating_add(usage.prompt_tokens);
        self.completion_tokens = self.completion_tokens.saturating_add(usage.completion_tokens);
        self.total_tokens = self.total_tokens.saturating_add(usage.total_tokens);
        self.input_tokens = self.input_tokens.saturating_add(usage.prompt_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.completion_tokens);

        let canonical = CanonicalUsage {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            ..CanonicalUsage::default()
        };
        let cost = calculate_cost(model, &canonical, provider, base_url);
        self.apply_cost_result(&cost);
    }

    fn apply_cost_result(&mut self, cost: &CostResult) {
        if let Some(amount) = cost.amount_usd {
            self.estimated_cost_usd += amount.max(0.0);
        }
        self.cost_status = cost_status_str(&cost.status).to_string();
        self.cost_source = cost_source_str(&cost.source).to_string();
    }
}

fn cost_status_str(status: &CostStatus) -> &'static str {
    match status {
        CostStatus::Actual => "actual",
        CostStatus::Estimated => "estimated",
        CostStatus::Included => "included",
        CostStatus::Unknown => "unknown",
    }
}

fn cost_source_str(source: &CostSource) -> &'static str {
    match source {
        CostSource::OfficialDocsSnapshot => "official_docs_snapshot",
        CostSource::ProviderModelsApi => "provider_models_api",
        CostSource::UserOverride => "user_override",
        CostSource::None => "none",
    }
}

/// TUI/gateway usage payload (Python `tui_gateway.server._get_usage`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionUsageDisplay {
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
    pub prompt: u64,
    pub completion: u64,
    pub total: u64,
    pub calls: u32,
    pub context_used: Option<u64>,
    pub context_max: Option<u64>,
    pub context_percent: Option<u32>,
    pub compressions: u32,
    pub cost_status: String,
    pub cost_usd: Option<f64>,
}

pub fn format_usage_command_text(display: &SessionUsageDisplay) -> String {
    if display.calls == 0 {
        return "(._.) No API calls made yet in this session.".to_string();
    }
    let mut lines = vec![
        "Session token usage".to_string(),
        format!("  Model:       {}", display.model),
        format!("  API calls:   {}", display.calls),
        format!("  Input:       {:>12}", display.input),
    ];
    if display.cache_read > 0 {
        lines.push(format!("  Cache read:  {:>12}", display.cache_read));
    }
    if display.cache_write > 0 {
        lines.push(format!("  Cache write: {:>12}", display.cache_write));
    }
    lines.push(format!("  Output:      {:>12}", display.output));
    lines.push(format!("  Prompt:      {:>12}", display.prompt));
    lines.push(format!("  Completion:  {:>12}", display.completion));
    lines.push(format!("  Total:       {:>12}", display.total));
    if let (Some(used), Some(max), Some(pct)) =
        (display.context_used, display.context_max, display.context_percent)
    {
        lines.push(format!("  Context:     {:>12} / {} ({}%)", used, max, pct));
    }
    if display.compressions > 0 {
        lines.push(format!("  Compressions: {}", display.compressions));
    }
    if let Some(cost) = display.cost_usd {
        lines.push(format!(
            "  Est. cost:   ${:.4} ({})",
            cost, display.cost_status
        ));
    } else {
        lines.push(format!("  Cost status: {}", display.cost_status));
    }
    lines.join("\n")
}

pub fn format_gateway_usage_text(display: &SessionUsageDisplay) -> String {
    if display.calls == 0 {
        return "📊 No API calls made yet in this session.".to_string();
    }
    let mut lines = vec![
        "📊 Session usage".to_string(),
        format!("- model: {}", display.model),
        format!("- input tokens: {}", display.input),
    ];
    if display.cache_read > 0 {
        lines.push(format!("- cache read: {}", display.cache_read));
    }
    if display.cache_write > 0 {
        lines.push(format!("- cache write: {}", display.cache_write));
    }
    lines.push(format!("- output tokens: {}", display.output));
    lines.push(format!("- total tokens: {}", display.total));
    lines.push(format!("- api calls: {}", display.calls));
    if let Some(cost) = display.cost_usd {
        lines.push(format!("- est. cost: ${:.4} ({})", cost, display.cost_status));
    }
    lines.join("\n")
}

/// Optional context-engine session hooks (Python `hasattr` checks on `context_compressor`).
pub trait ContextEngineHost {
    fn context_length(&self) -> Option<u64> {
        None
    }

    fn on_session_end(&mut self, _old_session_id: &str, _previous_messages: &[Message]) {}

    fn on_session_reset(&mut self) {}

    fn on_session_start(&mut self, _session_id: &str, _context: &HashMap<String, Value>) {}

    fn carry_over_new_session_context(&mut self, _old_session_id: &str, _new_session_id: &str) {}
}

impl ContextEngineHost for ContextCompressor {
    fn context_length(&self) -> Option<u64> {
        Some(self.config_context_length())
    }

    fn on_session_reset(&mut self) {
        self.reset_session_state();
    }
}

/// Notify the active context engine about a host session transition.
pub(crate) fn transition_context_engine_session(
    engine: &mut dyn ContextEngineHost,
    config: &AgentConfig,
    old_session_id: Option<&str>,
    new_session_id: Option<&str>,
    previous_messages: Option<&[Message]>,
    carry_over_context: bool,
    reset_engine: bool,
    extra_context: HashMap<String, Value>,
) {
    if let (Some(old_sid), Some(msgs)) = (old_session_id, previous_messages) {
        if !old_sid.is_empty() {
            engine.on_session_end(old_sid, msgs);
        }
    }

    if reset_engine {
        engine.on_session_reset();
    }

    let should_start = old_session_id.is_some()
        || previous_messages.is_some()
        || carry_over_context
        || !extra_context.is_empty();

    let target_session_id = new_session_id
        .or(config.session_id.as_deref())
        .unwrap_or("")
        .trim()
        .to_string();

    if should_start && !target_session_id.is_empty() {
        let platform = config
            .platform
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                std::env::var("HERMES_SESSION_SOURCE")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| "cli".to_string());

        let mut start_context: HashMap<String, Value> = HashMap::from([
            (
                "carry_over_context".to_string(),
                Value::Bool(carry_over_context),
            ),
            (
                "platform".to_string(),
                Value::String(platform),
            ),
            (
                "model".to_string(),
                Value::String(config.model.clone()),
            ),
        ]);
        if let Some(old) = old_session_id.filter(|s| !s.is_empty()) {
            start_context.insert("old_session_id".to_string(), Value::String(old.to_string()));
        }
        if let Some(ctx_len) = engine.context_length() {
            start_context.insert("context_length".to_string(), Value::from(ctx_len));
        }
        if let Some(ref conv_id) = config.gateway_session_key {
            if !conv_id.is_empty() {
                start_context.insert("conversation_id".to_string(), Value::String(conv_id.clone()));
            }
        }
        start_context.extend(extra_context);
        start_context.retain(|_, v| {
            !v.is_null()
                && !(v.is_string() && v.as_str().is_some_and(|s| s.is_empty()))
        });
        engine.on_session_start(&target_session_id, &start_context);
    }

    if carry_over_context {
        if let (Some(old), Some(new)) = (old_session_id, Some(target_session_id.as_str())) {
            if !old.is_empty() && !new.is_empty() {
                engine.carry_over_new_session_context(old, new);
            }
        }
    }
}

impl AgentLoop {
    /// Reset session-scoped token counters and context-engine state (Python `reset_session_state`).
    pub fn reset_session_state(
        &self,
        previous_messages: Option<&[Message]>,
        old_session_id: Option<&str>,
        carry_over_context: bool,
    ) {
        if let Ok(mut metrics) = self.session_usage.lock() {
            metrics.reset();
        }
        if let Ok(mut counters) = self.evolution_counters.lock() {
            counters.user_turn_count = 0;
        }

        let config = self.config();
        let new_session_id = config.session_id.clone();
        if let Ok(mut compressor) = self.context_compressor.try_lock() {
            transition_context_engine_session(
                &mut *compressor,
                &config,
                old_session_id,
                new_session_id.as_deref(),
                previous_messages,
                carry_over_context,
                true,
                HashMap::new(),
            );
        } else {
            debug!("context engine transition skipped: compressor lock busy");
        }
    }

    pub fn session_usage_metrics(&self) -> SessionUsageMetrics {
        self.session_usage
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Record token usage from one LLM HTTP response.
    pub fn record_api_usage(&self, usage: &UsageStats) {
        let config = self.config();
        let model = self.active_model();
        let provider = config.provider.as_deref();
        let runtime = self.primary_runtime_snapshot();
        let base_url = runtime.base_url.as_deref();
        if let Ok(mut metrics) = self.session_usage.lock() {
            metrics.accumulate_api_call(usage, &model, provider, base_url);
        }
        if let Ok(mut compressor) = self.context_compressor.try_lock() {
            compressor.update_from_usage(usage.prompt_tokens);
        }
    }

    /// Usage snapshot for TUI `session.usage` and `/usage` (Python `_get_usage`).
    pub fn session_usage_display(&self) -> SessionUsageDisplay {
        let metrics = self.session_usage_metrics();
        let model = self.active_model();
        let config = self.config();
        let provider = config.provider.as_deref();
        let runtime = self.primary_runtime_snapshot();
        let base_url = runtime.base_url.as_deref();

        let input = if metrics.input_tokens > 0 {
            metrics.input_tokens
        } else {
            metrics.prompt_tokens
        };
        let output = if metrics.output_tokens > 0 {
            metrics.output_tokens
        } else {
            metrics.completion_tokens
        };

        let mut display = SessionUsageDisplay {
            model,
            input,
            output,
            cache_read: metrics.cache_read_tokens,
            cache_write: metrics.cache_write_tokens,
            reasoning: metrics.reasoning_tokens,
            prompt: metrics.prompt_tokens,
            completion: metrics.completion_tokens,
            total: metrics.total_tokens,
            calls: metrics.api_calls,
            cost_status: metrics.cost_status.clone(),
            cost_usd: if metrics.estimated_cost_usd > 0.0 {
                Some(metrics.estimated_cost_usd)
            } else {
                None
            },
            ..SessionUsageDisplay::default()
        };

        if let Ok(compressor) = self.context_compressor.try_lock() {
            let ctx_used = compressor.last_prompt_tokens();
            let ctx_max = compressor.config_context_length();
            display.context_used = Some(ctx_used);
            display.context_max = Some(ctx_max);
            display.compressions = compressor.compression_count() as u32;
            if ctx_max > 0 {
                let pct = ((ctx_used as f64 / ctx_max as f64) * 100.0).round() as u32;
                display.context_percent = Some(pct.min(100));
            }
        }

        if display.cost_usd.is_none() && display.calls > 0 {
            let canonical = CanonicalUsage {
                input_tokens: display.input,
                output_tokens: display.output,
                cache_read_tokens: display.cache_read,
                cache_write_tokens: display.cache_write,
                ..CanonicalUsage::default()
            };
            let cost = calculate_cost(&display.model, &canonical, provider, base_url);
            display.cost_status = cost_status_str(&cost.status).to_string();
            display.cost_usd = cost.amount_usd;
        }

        display
    }

    /// Gateway/TUI JSON shape for `session.usage` RPC.
    pub fn session_usage_json(&self) -> Value {
        let d = self.session_usage_display();
        json!({
            "model": d.model,
            "input": d.input,
            "output": d.output,
            "cache_read": d.cache_read,
            "cache_write": d.cache_write,
            "reasoning": d.reasoning,
            "prompt": d.prompt,
            "completion": d.completion,
            "total": d.total,
            "calls": d.calls,
            "context_used": d.context_used,
            "context_max": d.context_max,
            "context_percent": d.context_percent,
            "compressions": d.compressions,
            "cost_status": d.cost_status,
            "cost_usd": d.cost_usd,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct RecordingEngine {
        events: Vec<String>,
        context_length: u64,
    }

    impl ContextEngineHost for RecordingEngine {
        fn context_length(&self) -> Option<u64> {
            Some(self.context_length)
        }

        fn on_session_end(&mut self, _: &str, _: &[Message]) {
            self.events.push("on_session_end".into());
        }

        fn on_session_reset(&mut self) {
            self.events.push("on_session_reset".into());
        }

        fn on_session_start(&mut self, _: &str, _: &HashMap<String, Value>) {
            self.events.push("on_session_start".into());
        }

        fn carry_over_new_session_context(&mut self, _: &str, _: &str) {
            self.events.push("carry_over".into());
        }
    }

    #[test]
    fn transition_runs_full_lifecycle_in_order() {
        let mut engine = RecordingEngine {
            events: Vec::new(),
            context_length: 200_000,
        };
        let config = AgentConfig::default();
        transition_context_engine_session(
            &mut engine,
            &config,
            Some("old-sid"),
            Some("new-sid"),
            Some(&[Message::user("hi")]),
            true,
            true,
            HashMap::new(),
        );
        assert_eq!(
            engine.events,
            vec![
                "on_session_end",
                "on_session_reset",
                "on_session_start",
                "carry_over"
            ]
        );
    }

    #[test]
    fn transition_default_call_only_resets() {
        let mut engine = RecordingEngine {
            events: Vec::new(),
            context_length: 100_000,
        };
        let config = AgentConfig::default();
        transition_context_engine_session(
            &mut engine,
            &config,
            None,
            None,
            None,
            false,
            true,
            HashMap::new(),
        );
        assert_eq!(engine.events, vec!["on_session_reset"]);
    }

    #[test]
    fn transition_passes_conversation_id_from_gateway_session_key() {
        struct CaptureEngine {
            captured: Option<HashMap<String, Value>>,
        }
        impl ContextEngineHost for CaptureEngine {
            fn context_length(&self) -> Option<u64> {
                Some(200_000)
            }
            fn on_session_start(&mut self, _: &str, ctx: &HashMap<String, Value>) {
                self.captured = Some(ctx.clone());
            }
        }
        let mut engine = CaptureEngine { captured: None };
        let config = AgentConfig {
            gateway_session_key: Some("agent:main:telegram:dm:42".into()),
            platform: Some("telegram".into()),
            ..AgentConfig::default()
        };
        transition_context_engine_session(
            &mut engine,
            &config,
            Some("old-sid"),
            Some("new-sid"),
            Some(&[Message::user("hi")]),
            false,
            true,
            HashMap::new(),
        );
        let ctx = engine.captured.expect("on_session_start");
        assert_eq!(
            ctx.get("conversation_id").and_then(|v| v.as_str()),
            Some("agent:main:telegram:dm:42")
        );
        assert_eq!(
            ctx.get("old_session_id").and_then(|v| v.as_str()),
            Some("old-sid")
        );
        assert_eq!(ctx.get("platform").and_then(|v| v.as_str()), Some("telegram"));
    }

    #[test]
    fn accumulate_api_call_increments_counters() {
        let mut m = SessionUsageMetrics::default();
        let usage = UsageStats {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            estimated_cost: None,
        };
        m.accumulate_api_call(&usage, "gpt-4o", Some("openai"), None);
        assert_eq!(m.api_calls, 1);
        assert_eq!(m.total_tokens, 15);
        m.accumulate_api_call(&usage, "gpt-4o", Some("openai"), None);
        assert_eq!(m.api_calls, 2);
        assert_eq!(m.total_tokens, 30);
    }
}
