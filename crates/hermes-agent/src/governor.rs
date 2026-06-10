//! Performance governor — runtime feedback loop that throttles concurrency,
//! token budgets, and provider routing when latency or error rates degrade.

use std::collections::VecDeque;

use crate::agent_config::AgentConfig;
use crate::context::ContextManager;

// ---------------------------------------------------------------------------
// Governor types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct TurnGovernor {
    pub(crate) max_tokens: Option<u32>,
    pub(crate) tool_concurrency: usize,
    pub(crate) pressure: f64,
    pub(crate) latency_degraded: bool,
    pub(crate) error_degraded: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct GovernorRuntimeState {
    pub(crate) avg_llm_latency_ms: Option<f64>,
    pub(crate) avg_tool_error_rate: f64,
    pub(crate) consecutive_error_turns: u32,
}

// ---------------------------------------------------------------------------
// Environment-read helpers
// ---------------------------------------------------------------------------

pub(crate) fn governor_enabled() -> bool {
    std::env::var("HERMES_PERFORMANCE_GOVERNOR")
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off"
            )
        })
        .unwrap_or(true)
}

pub(crate) fn governor_tool_concurrency_base() -> usize {
    std::env::var("HERMES_TOOL_CALL_MAX_CONCURRENCY")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(8)
}

pub(crate) fn governor_window_size() -> usize {
    std::env::var("HERMES_PERF_GOV_WINDOW")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(8)
}

pub(crate) fn governor_latency_warn_ms() -> f64 {
    std::env::var("HERMES_PERF_GOV_LATENCY_WARN_MS")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(3500.0)
}

pub(crate) fn governor_latency_critical_ms() -> f64 {
    std::env::var("HERMES_PERF_GOV_LATENCY_CRITICAL_MS")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(6500.0)
}

pub(crate) fn governor_error_warn_rate() -> f64 {
    std::env::var("HERMES_PERF_GOV_ERROR_WARN_RATE")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| (0.0..=1.0).contains(v))
        .unwrap_or(0.20)
}

pub(crate) fn governor_error_critical_rate() -> f64 {
    std::env::var("HERMES_PERF_GOV_ERROR_CRITICAL_RATE")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| (0.0..=1.0).contains(v))
        .unwrap_or(0.50)
}

pub(crate) fn governor_tool_loop_guard_enabled() -> bool {
    std::env::var("HERMES_TOOL_LOOP_GUARD_ENABLED")
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off"
            )
        })
        .unwrap_or(true)
}

pub(crate) fn governor_tool_loop_guard_max_consecutive_error_turns() -> u32 {
    std::env::var("HERMES_TOOL_LOOP_GUARD_MAX_CONSEC_ERROR_TURNS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(4)
}

pub(crate) fn governor_tool_loop_guard_min_failed_calls() -> u32 {
    std::env::var("HERMES_TOOL_LOOP_GUARD_MIN_FAILED_CALLS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(1)
}

// ---------------------------------------------------------------------------
// Tool-loop guard
// ---------------------------------------------------------------------------

pub(crate) fn should_trip_tool_loop_guard(
    consecutive_error_turns: u32,
    turn_tool_count: usize,
    turn_tool_error_count: u32,
) -> bool {
    if !governor_tool_loop_guard_enabled() {
        return false;
    }
    if turn_tool_count == 0 {
        return false;
    }
    if turn_tool_error_count < governor_tool_loop_guard_min_failed_calls() {
        return false;
    }
    if turn_tool_error_count != turn_tool_count as u32 {
        return false;
    }
    consecutive_error_turns >= governor_tool_loop_guard_max_consecutive_error_turns()
}

// ---------------------------------------------------------------------------
// Runtime state computation
// ---------------------------------------------------------------------------

fn avg_u64(window: &VecDeque<u64>) -> Option<f64> {
    if window.is_empty() {
        return None;
    }
    Some(window.iter().copied().map(|v| v as f64).sum::<f64>() / window.len() as f64)
}

fn avg_f64(window: &VecDeque<f64>) -> f64 {
    if window.is_empty() {
        return 0.0;
    }
    window.iter().copied().sum::<f64>() / window.len() as f64
}

pub(crate) fn governor_runtime_state(
    llm_latency_window: &VecDeque<u64>,
    tool_error_window: &VecDeque<f64>,
    consecutive_error_turns: u32,
) -> GovernorRuntimeState {
    GovernorRuntimeState {
        avg_llm_latency_ms: avg_u64(llm_latency_window),
        avg_tool_error_rate: avg_f64(tool_error_window),
        consecutive_error_turns,
    }
}

/// Whether to force a reliability-guard runtime route this turn.
///
/// Tool-error degradation needs sustained failures (`consecutive_error_turns >= 2`).
/// Latency degradation needs multiple samples so one slow LLM call cannot hop providers.
pub(crate) fn should_apply_turn_reliability_guard(
    runtime: &GovernorRuntimeState,
    llm_governor: &TurnGovernor,
    llm_latency_window_len: usize,
) -> bool {
    if !llm_governor.error_degraded && !llm_governor.latency_degraded {
        return false;
    }
    if runtime.consecutive_error_turns >= 2 {
        return true;
    }
    llm_governor.latency_degraded
        && llm_latency_window_len >= 2
        && runtime
            .avg_llm_latency_ms
            .map(|v| v >= governor_latency_warn_ms())
            .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Per-turn governor
// ---------------------------------------------------------------------------

pub(crate) fn governor_for_turn(
    config: &AgentConfig,
    ctx: &ContextManager,
    requested_tools: usize,
    runtime: Option<&GovernorRuntimeState>,
) -> TurnGovernor {
    let threshold = ((ctx.max_context_chars().max(1) as f64) * 0.8).max(1.0);
    let mut pressure = (ctx.total_chars() as f64 / threshold).max(0.0);
    let enabled = governor_enabled();
    let mut latency_degraded = false;
    let mut error_degraded = false;

    if enabled {
        if let Some(runtime) = runtime {
            if let Some(lat_ms) = runtime.avg_llm_latency_ms {
                if lat_ms >= governor_latency_critical_ms() {
                    pressure = pressure.max(0.97);
                    latency_degraded = true;
                } else if lat_ms >= governor_latency_warn_ms() {
                    pressure = pressure.max(0.88);
                    latency_degraded = true;
                }
            }
            if runtime.avg_tool_error_rate >= governor_error_critical_rate()
                || runtime.consecutive_error_turns >= 3
            {
                pressure = pressure.max(0.97);
                error_degraded = true;
            } else if runtime.avg_tool_error_rate >= governor_error_warn_rate()
                || runtime.consecutive_error_turns >= 1
            {
                pressure = pressure.max(0.88);
                error_degraded = true;
            }
        }
    }

    let max_tokens = if enabled {
        config.max_tokens.map(|base| {
            if pressure >= 0.95 {
                base.saturating_div(4).max(64)
            } else if pressure >= 0.85 {
                base.saturating_div(2).max(128)
            } else {
                base
            }
        })
    } else {
        config.max_tokens
    };

    let base_concurrency = governor_tool_concurrency_base();
    let mut tool_concurrency = if enabled {
        if pressure >= 0.95 {
            base_concurrency.min(2)
        } else if pressure >= 0.85 {
            base_concurrency.min(4)
        } else {
            base_concurrency
        }
    } else {
        base_concurrency
    };
    if requested_tools > 0 {
        tool_concurrency = tool_concurrency.min(requested_tools).max(1);
    }

    TurnGovernor {
        max_tokens,
        tool_concurrency,
        pressure,
        latency_degraded,
        error_degraded,
    }
}
