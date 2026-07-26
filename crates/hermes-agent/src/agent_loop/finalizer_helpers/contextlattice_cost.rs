fn contextlattice_connect_system_hint(messages: &[Message]) -> Option<String> {
    if !detect_contextlattice_connect_intent(messages) {
        return None;
    }
    Some(
        "[SYSTEM] ContextLattice integration intent detected. Execute this order: \
         (1) If available, inspect local instructions file from `HERMES_CONTEXTLATTICE_INSTRUCTIONS_PATH` \
         (2) call `contextlattice_search` for a direct connectivity probe; \
         (3) if needed call `contextlattice_context_pack` for broader grounding; \
         (4) call `contextlattice_write` to checkpoint what was verified. \
         Never use terminal command `contextlattice` for this workflow."
            .to_string(),
    )
}

fn contextlattice_intelligence_system_hint(
    messages: &[Message],
    tool_schemas: &[ToolSchema],
) -> Option<String> {
    let has_context_tools = tool_schemas.iter().any(|t| {
        matches!(
            t.name.as_str(),
            "contextlattice_search"
                | "contextlattice_context_pack"
                | "contextlattice_write"
                | "memory"
        )
    });
    if !has_context_tools {
        return None;
    }

    let objective_active = objective_guard_policy(messages).0;
    let repo_intent = detect_repo_review_intent(messages);
    let connect_intent = detect_contextlattice_connect_intent(messages);
    if !(objective_active || repo_intent || connect_intent) {
        return None;
    }

    Some(
        "[SYSTEM] ContextLattice-first intelligence policy active.\n\
         1) Start with scoped retrieval (`contextlattice_search`) using project + topic path.\n\
         2) If scoped retrieval is empty/degraded, run one broader retrieval in the same project and compare.\n\
         3) For broad or multi-file tasks, run `contextlattice_context_pack` before deep tool loops.\n\
         4) During long execution, checkpoint durable progress with `contextlattice_write`.\n\
         5) Before final answer, run one scoped readback and report contradictions as `unproven` rather than guessing.\n\
         6) Copy numeric facts verbatim; do not normalize or round unless explicitly requested."
            .to_string(),
    )
}

fn is_contextlattice_shell_invocation(raw_args: &str) -> bool {
    let Ok(args) = serde_json::from_str::<Value>(raw_args) else {
        return false;
    };
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default();
    let lower = command.to_ascii_lowercase();
    lower == "contextlattice" || lower.starts_with("contextlattice ")
}


fn default_model_cost_per_million(model: &str) -> Option<(f64, f64)> {
    let m = model.to_lowercase();
    if m.contains("gpt-4o-mini") || m.contains("4.1-mini") || m.contains("haiku") {
        return Some((0.15, 0.60));
    }
    if m.contains("gpt-4o") || m.contains("4.1") || m.contains("sonnet") {
        return Some((2.5, 10.0));
    }
    if m.contains("o3") {
        return Some((10.0, 40.0));
    }
    None
}


fn estimate_usage_cost_usd(usage: &UsageStats, model: &str, config: &AgentConfig) -> Option<f64> {
    if let Some(v) = usage.estimated_cost {
        return Some(v.max(0.0));
    }
    let (in_pm, out_pm) = match (
        config.prompt_cost_per_million_usd,
        config.completion_cost_per_million_usd,
    ) {
        (Some(i), Some(o)) => (i, o),
        _ => default_model_cost_per_million(model)?,
    };
    let prompt_cost = (usage.prompt_tokens as f64 / 1_000_000.0) * in_pm;
    let completion_cost = (usage.completion_tokens as f64 / 1_000_000.0) * out_pm;
    Some(prompt_cost + completion_cost)
}

/// Merge two UsageStats, summing token counts and keeping the latest cost estimate.
fn merge_usage(existing: Option<UsageStats>, new: &UsageStats) -> UsageStats {
    match existing {
        Some(prev) => UsageStats {
            prompt_tokens: prev.prompt_tokens + new.prompt_tokens,
            completion_tokens: prev.completion_tokens + new.completion_tokens,
            total_tokens: prev.total_tokens + new.total_tokens,
            estimated_cost: match (prev.estimated_cost, new.estimated_cost) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            },
        },
        None => new.clone(),
    }
}
