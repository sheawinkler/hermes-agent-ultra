//! Deterministic autonomy evals for outcome loops and memory recall quality.
//!
//! These helpers are deliberately pure: callers provide the turn evidence and
//! the report scores whether the work would satisfy Hermes' runtime contract.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

const PASS_SCORE: u8 = 80;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OutcomeLoopRehearsalInput {
    #[serde(default)]
    pub objective: String,
    #[serde(default)]
    pub plan_steps: Vec<String>,
    #[serde(default)]
    pub tool_calls: Vec<RehearsalToolUse>,
    #[serde(default)]
    pub verification: Vec<String>,
    #[serde(default)]
    pub checkpoints: Vec<String>,
    #[serde(default)]
    pub recovery_events: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RehearsalToolUse {
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvalGate {
    pub id: String,
    pub label: String,
    pub score: u8,
    pub max_score: u8,
    pub passed: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutcomeLoopRehearsalReport {
    pub score: u8,
    pub max_score: u8,
    pub pass: bool,
    pub gates: Vec<EvalGate>,
    pub failed_gates: Vec<String>,
    pub next_action: String,
}

pub fn evaluate_outcome_loop_rehearsal(
    input: OutcomeLoopRehearsalInput,
) -> OutcomeLoopRehearsalReport {
    let mut gates = Vec::new();

    let plan_steps = non_empty(&input.plan_steps);
    gates.push(gate(
        "plan",
        "Objective and execution plan",
        if !input.objective.trim().is_empty() && plan_steps.len() >= 2 {
            20
        } else if !input.objective.trim().is_empty() && !plan_steps.is_empty() {
            12
        } else {
            0
        },
        if !input.objective.trim().is_empty() && plan_steps.len() >= 2 {
            "objective plus at least two concrete plan steps"
        } else if !input.objective.trim().is_empty() {
            "objective present but plan is too shallow"
        } else {
            "missing objective or actionable plan"
        },
    ));

    let useful_tools = input
        .tool_calls
        .iter()
        .filter(|call| !call.tool.trim().is_empty())
        .count();
    let grounded_tools = input
        .tool_calls
        .iter()
        .filter(|call| tool_grounded(call))
        .count();
    gates.push(gate(
        "tool_use",
        "Purposeful tool use with evidence",
        if useful_tools > 0 && grounded_tools == useful_tools {
            20
        } else if useful_tools > 0 {
            12
        } else {
            0
        },
        if useful_tools > 0 && grounded_tools == useful_tools {
            "every tool call has purpose and outcome evidence"
        } else if useful_tools > 0 {
            "tool calls exist but some lack purpose or evidence"
        } else {
            "no tool-backed evidence"
        },
    ));

    let verification = non_empty(&input.verification);
    let verification_grounded = verification
        .iter()
        .any(|item| contains_any(item, VERIFICATION_TERMS));
    gates.push(gate(
        "verification",
        "Deterministic verification",
        if verification_grounded {
            20
        } else if !verification.is_empty() {
            12
        } else {
            0
        },
        if verification_grounded {
            "verification includes deterministic pass/check evidence"
        } else if !verification.is_empty() {
            "verification notes exist but lack deterministic pass/check terms"
        } else {
            "missing verification evidence"
        },
    ));

    let checkpoints = non_empty(&input.checkpoints);
    let checkpoint_grounded = checkpoints
        .iter()
        .any(|item| contains_any(item, CHECKPOINT_TERMS));
    gates.push(gate(
        "checkpoint",
        "Durable checkpoint or handoff",
        if checkpoint_grounded {
            20
        } else if !checkpoints.is_empty() {
            12
        } else {
            0
        },
        if checkpoint_grounded {
            "checkpoint is tied to durable PR/commit/release/ContextLattice evidence"
        } else if !checkpoints.is_empty() {
            "checkpoint exists but is weakly grounded"
        } else {
            "missing durable checkpoint"
        },
    ));

    let failed_tools = input
        .tool_calls
        .iter()
        .filter(|call| contains_any(&call.outcome, FAILURE_TERMS))
        .count();
    let recovery = non_empty(&input.recovery_events);
    gates.push(gate(
        "recovery",
        "Recovery from failures or clean no-failure proof",
        if failed_tools == 0 || !recovery.is_empty() {
            20
        } else {
            0
        },
        if failed_tools == 0 {
            "no failed tool outcomes requiring recovery"
        } else if !recovery.is_empty() {
            "failed tool outcomes have explicit recovery events"
        } else {
            "failed tool outcomes lack recovery evidence"
        },
    ));

    finish_report(gates)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RecallQualityInput {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub recall_items: Vec<RecallQualityItem>,
    #[serde(default)]
    pub outcome: TaskOutcomeEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RecallQualityItem {
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub relevance: f64,
    #[serde(default)]
    pub used_for: Vec<String>,
    #[serde(default)]
    pub outcome_links: Vec<String>,
    #[serde(default)]
    pub provenance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TaskOutcomeEvidence {
    #[serde(default)]
    pub objective: String,
    #[serde(default)]
    pub implemented_items: Vec<String>,
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default)]
    pub checkpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecallQualityReport {
    pub score: u8,
    pub max_score: u8,
    pub pass: bool,
    pub gates: Vec<EvalGate>,
    pub failed_gates: Vec<String>,
    pub next_action: String,
}

pub fn evaluate_recall_quality(input: RecallQualityInput) -> RecallQualityReport {
    let mut gates = Vec::new();
    let items = input
        .recall_items
        .iter()
        .filter(|item| {
            !item.source.trim().is_empty()
                || !item.summary.trim().is_empty()
                || item.relevance > 0.0
        })
        .collect::<Vec<_>>();

    gates.push(gate(
        "recall_available",
        "Recall returned usable items",
        if items.is_empty() { 0 } else { 20 },
        if items.is_empty() {
            "no usable recall items"
        } else {
            "recall items are available"
        },
    ));

    let provenance_count = items
        .iter()
        .filter(|item| !item.source.trim().is_empty() && !item.provenance.trim().is_empty())
        .count();
    gates.push(gate(
        "provenance",
        "Recall has source provenance",
        if !items.is_empty() && provenance_count == items.len() {
            20
        } else if provenance_count > 0 {
            12
        } else {
            0
        },
        if !items.is_empty() && provenance_count == items.len() {
            "every recall item has source and provenance"
        } else if provenance_count > 0 {
            "some recall items have source provenance"
        } else {
            "recall is not source/provenance grounded"
        },
    ));

    let query_terms = terms(&input.query)
        .into_iter()
        .chain(terms(&input.outcome.objective))
        .collect::<BTreeSet<_>>();
    let aligned = items.iter().any(|item| {
        item.relevance >= 0.65
            || !query_terms.is_empty() && !terms(&item.summary).is_disjoint(&query_terms)
    });
    gates.push(gate(
        "objective_alignment",
        "Recall aligns with the objective",
        if aligned { 20 } else { 0 },
        if aligned {
            "recall overlaps the query/objective or carries high relevance"
        } else {
            "recall availability is not enough without objective alignment"
        },
    ));

    let implementation_evidence = !non_empty(&input.outcome.implemented_items).is_empty();
    let implementation_linked = implementation_evidence
        && items.iter().any(|item| {
            item.used_for
                .iter()
                .chain(item.outcome_links.iter())
                .any(|value| contains_any(value, IMPLEMENTATION_TERMS))
        });
    gates.push(gate(
        "implementation_impact",
        "Recall influenced implementation outcome",
        if implementation_linked { 20 } else { 0 },
        if implementation_linked {
            "recall is linked to implemented items"
        } else {
            "recall was not tied to any implementation outcome"
        },
    ));

    let verification_evidence = !non_empty(&input.outcome.checks).is_empty();
    let verification_linked = verification_evidence
        && items.iter().any(|item| {
            item.used_for
                .iter()
                .chain(item.outcome_links.iter())
                .any(|value| contains_any(value, VERIFICATION_TERMS))
        });
    gates.push(gate(
        "verification_impact",
        "Recall influenced verification outcome",
        if verification_linked { 20 } else { 0 },
        if verification_linked {
            "recall is linked to deterministic checks"
        } else {
            "recall was not tied to verification evidence"
        },
    ));

    let outcome = finish_report(gates);
    RecallQualityReport {
        score: outcome.score,
        max_score: outcome.max_score,
        pass: outcome.pass,
        gates: outcome.gates,
        failed_gates: outcome.failed_gates,
        next_action: outcome.next_action,
    }
}

fn finish_report(gates: Vec<EvalGate>) -> OutcomeLoopRehearsalReport {
    let score = gates.iter().map(|gate| gate.score).sum::<u8>();
    let max_score = gates.iter().map(|gate| gate.max_score).sum::<u8>();
    let failed_gates = gates
        .iter()
        .filter(|gate| !gate.passed)
        .map(|gate| gate.id.clone())
        .collect::<Vec<_>>();
    let pass = score >= PASS_SCORE && failed_gates.is_empty();
    let next_action = if pass {
        "promote to implementation/merge gate; continue recording deterministic evidence"
            .to_string()
    } else {
        format!(
            "repair failed eval gates before promotion: {}",
            failed_gates.join(", ")
        )
    };
    OutcomeLoopRehearsalReport {
        score,
        max_score,
        pass,
        gates,
        failed_gates,
        next_action,
    }
}

fn gate(id: &str, label: &str, score: u8, reason: &str) -> EvalGate {
    EvalGate {
        id: id.to_string(),
        label: label.to_string(),
        score,
        max_score: 20,
        passed: score == 20,
        reason: reason.to_string(),
    }
}

fn tool_grounded(call: &RehearsalToolUse) -> bool {
    !call.tool.trim().is_empty()
        && !call.purpose.trim().is_empty()
        && (!call.outcome.trim().is_empty() || !call.evidence.trim().is_empty())
}

fn non_empty(values: &[String]) -> Vec<&str> {
    values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect()
}

fn terms(value: &str) -> BTreeSet<String> {
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(str::trim)
        .filter(|part| part.len() >= 3)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    let lower = value.to_ascii_lowercase();
    needles.iter().any(|needle| lower.contains(needle))
}

const VERIFICATION_TERMS: &[&str] = &[
    "pass", "passed", "green", "ok", "check", "checked", "verified", "test", "cargo", "pytest",
    "ci",
];

const CHECKPOINT_TERMS: &[&str] = &[
    "contextlattice",
    "checkpoint",
    "commit",
    "pr #",
    "pull request",
    "merged",
    "tag",
    "release",
];

const FAILURE_TERMS: &[&str] = &["fail", "failed", "error", "timeout", "timed out", "blocked"];

const IMPLEMENTATION_TERMS: &[&str] = &[
    "implement",
    "implemented",
    "code",
    "patch",
    "rust",
    "provider",
    "release",
    "ci",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_loop_rehearsal_passes_with_recovery_and_checkpoint() {
        let report = evaluate_outcome_loop_rehearsal(OutcomeLoopRehearsalInput {
            objective: "Close provider and release smoke gaps".to_string(),
            plan_steps: vec![
                "Inspect repo state".to_string(),
                "Implement Rust matrix".to_string(),
            ],
            tool_calls: vec![
                RehearsalToolUse {
                    tool: "rg".to_string(),
                    purpose: "find provider auth code".to_string(),
                    outcome: "passed with matching files".to_string(),
                    evidence: "crates/hermes-provider-runtime/src/lib.rs".to_string(),
                },
                RehearsalToolUse {
                    tool: "cargo test".to_string(),
                    purpose: "verify new evals".to_string(),
                    outcome: "failed before import fix".to_string(),
                    evidence: "compiler error captured".to_string(),
                },
            ],
            verification: vec!["cargo test -p hermes-tools ultra_autonomy passed".to_string()],
            checkpoints: vec!["ContextLattice checkpoint recorded for PR #720".to_string()],
            recovery_events: vec!["fixed import and reran test green".to_string()],
        });

        assert!(report.pass);
        assert_eq!(report.score, 100);
    }

    #[test]
    fn outcome_loop_rehearsal_rejects_unverified_work() {
        let report = evaluate_outcome_loop_rehearsal(OutcomeLoopRehearsalInput {
            objective: "Ship change".to_string(),
            plan_steps: vec!["Patch".to_string(), "Report".to_string()],
            tool_calls: vec![RehearsalToolUse {
                tool: "apply_patch".to_string(),
                purpose: "edit code".to_string(),
                outcome: "ok".to_string(),
                evidence: "diff exists".to_string(),
            }],
            ..OutcomeLoopRehearsalInput::default()
        });

        assert!(!report.pass);
        assert!(report.failed_gates.contains(&"verification".to_string()));
        assert!(report.failed_gates.contains(&"checkpoint".to_string()));
    }

    #[test]
    fn recall_quality_requires_outcome_linkage_not_just_items() {
        let report = evaluate_recall_quality(RecallQualityInput {
            query: "provider auth smoke".to_string(),
            recall_items: vec![RecallQualityItem {
                source: "contextlattice_synthesis_pack".to_string(),
                summary: "Provider auth smoke matrix exists".to_string(),
                relevance: 0.95,
                provenance: "runbooks/codex-integration".to_string(),
                ..RecallQualityItem::default()
            }],
            outcome: TaskOutcomeEvidence::default(),
        });

        assert!(!report.pass);
        assert!(report
            .failed_gates
            .contains(&"implementation_impact".to_string()));
        assert!(report
            .failed_gates
            .contains(&"verification_impact".to_string()));
    }

    #[test]
    fn recall_quality_passes_when_synthesis_pack_drives_checked_outcome() {
        let report = evaluate_recall_quality(RecallQualityInput {
            query: "ContextLattice synthesis pack outcome eval".to_string(),
            recall_items: vec![RecallQualityItem {
                source: "contextlattice_synthesis_pack".to_string(),
                summary: "Use synthesis pack as guardrail for outcome eval implementation"
                    .to_string(),
                relevance: 0.91,
                used_for: vec!["implementation".to_string(), "verification".to_string()],
                outcome_links: vec![
                    "implemented Rust recall quality eval".to_string(),
                    "cargo test verification passed".to_string(),
                ],
                provenance: "runbooks/codex-integration".to_string(),
            }],
            outcome: TaskOutcomeEvidence {
                objective: "Implement outcome eval tied to ContextLattice recall".to_string(),
                implemented_items: vec!["Rust recall quality eval".to_string()],
                checks: vec!["cargo test -p hermes-tools ultra_autonomy_evals passed".to_string()],
                checkpoint: "ContextLattice checkpoint".to_string(),
            },
        });

        assert!(report.pass);
        assert_eq!(report.score, 100);
    }
}
