//! Swarm runtime planning primitives for Hermes.
//!
//! Keeps orchestration metadata and capability checks isolated from the
//! CLI/UI layer so command surfaces can stay thin.

use serde::{Deserialize, Serialize};

/// Supported orchestration shapes for swarm execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmExecutionMode {
    Concurrent,
    Sequential,
    Graph,
}

impl SwarmExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Concurrent => "concurrent",
            Self::Sequential => "sequential",
            Self::Graph => "graph",
        }
    }
}

/// Runtime capability snapshot for swarm orchestration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmRuntimeStatus {
    pub feature_enabled: bool,
    pub engine: &'static str,
    pub supported_modes: Vec<SwarmExecutionMode>,
    pub notes: Vec<String>,
}

/// Deterministic plan produced before dispatching a swarm execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmExecutionPlan {
    pub mode: SwarmExecutionMode,
    pub voters: usize,
    pub required_success: usize,
    pub models: Vec<String>,
    pub objective: Option<String>,
    pub pass_cap: usize,
    pub constraints: Vec<String>,
}

/// Return runtime status for swarm support compiled into this binary.
pub fn swarm_runtime_status() -> SwarmRuntimeStatus {
    let mut notes = Vec::new();
    let feature_enabled = cfg!(feature = "swarms");
    if feature_enabled {
        let _markers = linked_swarms_type_markers();
        notes.push("swarms-rs feature compiled and linked".to_string());
        notes.push("use Hermes guardrails for tool policy/objective before dispatch".to_string());
    } else {
        notes.push("swarms-rs feature not compiled; using quorum-only runtime".to_string());
        notes.push("rebuild with `--features swarms` to enable native swarms engine".to_string());
    }
    SwarmRuntimeStatus {
        feature_enabled,
        engine: if feature_enabled {
            "swarms-rs"
        } else {
            "quorum-fallback"
        },
        supported_modes: vec![
            SwarmExecutionMode::Concurrent,
            SwarmExecutionMode::Sequential,
            SwarmExecutionMode::Graph,
        ],
        notes,
    }
}

/// Build a deterministic swarm plan from runtime policy state.
pub fn build_swarm_execution_plan(
    mode: SwarmExecutionMode,
    voters: usize,
    models: Vec<String>,
    objective: Option<String>,
    pass_cap: usize,
) -> SwarmExecutionPlan {
    let voters = voters.max(1);
    let pass_cap = pass_cap.clamp(1, 8);
    let required_success = if voters <= 2 {
        voters
    } else {
        (voters / 2) + 1
    };
    let constraints = vec![
        "enforce objective contract + anti-scheming prelude".to_string(),
        "persist voter artifacts + synthesis provenance".to_string(),
        "block publish when required_success threshold is not met".to_string(),
        "respect token/cost caps via pass_cap and bounded voter count".to_string(),
    ];
    SwarmExecutionPlan {
        mode,
        voters,
        required_success,
        models,
        objective,
        pass_cap,
        constraints,
    }
}

#[cfg(feature = "swarms")]
fn linked_swarms_type_markers() -> Vec<&'static str> {
    vec![
        std::any::type_name::<swarms::structs::concurrent_workflow::ConcurrentWorkflow>(),
        std::any::type_name::<swarms::structs::swarms_router::SwarmType>(),
    ]
}

#[cfg(not(feature = "swarms"))]
fn linked_swarms_type_markers() -> Vec<&'static str> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_success_uses_majority_when_voters_gt_two() {
        let plan = build_swarm_execution_plan(SwarmExecutionMode::Concurrent, 5, vec![], None, 99);
        assert_eq!(plan.required_success, 3);
        assert_eq!(plan.pass_cap, 8);
    }
}
