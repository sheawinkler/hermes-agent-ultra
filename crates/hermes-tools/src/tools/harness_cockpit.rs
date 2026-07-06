//! One-true-harness cockpit snapshots.
//!
//! This tool intentionally composes existing Hermes surfaces instead of
//! replacing them: telemetry gates, verification evidence, skill taps, objective
//! commands, replay controls, and dashboard auth all stay owned by their
//! subsystem. The cockpit gives users and agents one durable map of the harness.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::repo::detect_repo_root_from_cwd;
use crate::tools::telemetry_snapshot::telemetry_gate_snapshot;
use crate::tools::ultra_autonomy::memory_lifecycle_snapshot;
use crate::tools::ultra_autonomy::resource_admission_plan;
use crate::tools::ultra_autonomy::MemoryProviderSignal;
use crate::tools::ultra_autonomy::ResourceGovernorInput;
use crate::verification_evidence::verification_status;

const TOOL_NAME: &str = "harness_cockpit";

const MATTPOCOCK_TAP: &str = "https://github.com/mattpocock/skills::skills";

#[derive(Debug, Clone, Copy, Serialize)]
pub struct RecommendedSkill {
    pub id: &'static str,
    pub source: &'static str,
    pub install_ref: &'static str,
    pub why: &'static str,
    pub tier: &'static str,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct HarnessItem {
    pub id: u8,
    pub name: &'static str,
    pub status: &'static str,
    pub primary_surface: &'static str,
    pub verification: &'static str,
}

#[derive(Clone, Default)]
pub struct HarnessCockpitHandler;

impl HarnessCockpitHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for HarnessCockpitHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("status")
            .trim()
            .to_ascii_lowercase();
        let repo_root = params
            .get("repo_root")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(detect_repo_root_from_cwd);

        let payload = harness_cockpit_action_snapshot(&action, repo_root.as_deref())
            .map_err(ToolError::InvalidParams)?;

        serde_json::to_string_pretty(&payload)
            .map_err(|err| ToolError::ExecutionFailed(format!("render cockpit: {err}")))
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": ["status", "skills", "proof", "roadmap", "chaos", "onboarding", "objective", "autonomy", "help"],
                "description": "Harness cockpit section. Defaults to status."
            }),
        );
        props.insert(
            "repo_root".into(),
            json!({
                "type": "string",
                "description": "Optional repository root for gate/evidence lookups."
            }),
        );
        tool_schema(
            TOOL_NAME,
            "Report the unified Hermes one-true-harness cockpit: roadmap, proof, skills, objectives, onboarding, and chaos surfaces.",
            JsonSchema::object(props, vec![]),
        )
    }
}

pub fn harness_cockpit_snapshot(repo_root: Option<&Path>) -> Value {
    json!({
        "status": "ok",
        "mission": "one_true_rust_first_agent_harness",
        "repo_root": repo_root.map(|path| path.display().to_string()),
        "issue": "https://github.com/sheawinkler/hermes-agent-ultra/issues/702",
        "roadmap": harness_items(),
        "skills": {
            "default_tap": MATTPOCOCK_TAP,
            "mattpocock": mattpocock_recommended_skills(),
            "star_candidates": star_skill_candidates(),
        },
        "proof": proof_snapshot(repo_root),
        "objectives": objective_surfaces(),
        "autonomy": autonomy_surfaces(),
        "onboarding": onboarding_commands(),
        "chaos": chaos_scenarios(),
    })
}

pub fn harness_cockpit_action_snapshot(
    action: &str,
    repo_root: Option<&Path>,
) -> Result<Value, String> {
    match action.trim().to_ascii_lowercase().as_str() {
        "" | "status" | "all" => Ok(harness_cockpit_snapshot(repo_root)),
        "skills" => Ok(json!({
            "status": "ok",
            "default_tap": MATTPOCOCK_TAP,
            "mattpocock": mattpocock_recommended_skills(),
            "star_candidates": star_skill_candidates(),
        })),
        "proof" => Ok(proof_snapshot(repo_root)),
        "roadmap" => Ok(json!({
            "status": "ok",
            "items": harness_items(),
        })),
        "chaos" => Ok(json!({
            "status": "ok",
            "scenarios": chaos_scenarios(),
        })),
        "onboarding" => Ok(json!({
            "status": "ok",
            "commands": onboarding_commands(),
        })),
        "objective" | "objectives" => Ok(json!({
            "status": "ok",
            "surfaces": objective_surfaces(),
        })),
        "autonomy" => Ok(json!({
            "status": "ok",
            "surfaces": autonomy_surfaces(),
        })),
        "help" => Ok(json!({
            "status": "ok",
            "tool": TOOL_NAME,
            "actions": ["status", "skills", "proof", "roadmap", "chaos", "onboarding", "objective", "autonomy", "help"],
        })),
        other => Err(format!(
            "unknown action '{other}'; expected status|skills|proof|roadmap|chaos|onboarding|objective|autonomy|help"
        )),
    }
}

pub fn render_harness_cockpit_text(repo_root: Option<&Path>) -> String {
    let items = harness_items();
    let skills = mattpocock_recommended_skills();
    let candidates = star_skill_candidates();
    let mut out = String::new();
    out.push_str("Hermes one-true-harness cockpit\n");
    out.push_str("=================================\n");
    out.push_str("status=ok\n");
    out.push_str("issue=https://github.com/sheawinkler/hermes-agent-ultra/issues/702\n");
    if let Some(root) = repo_root {
        out.push_str(&format!("repo_root={}\n", root.display()));
    }
    out.push_str("\nRoadmap surfaces\n");
    for item in &items {
        out.push_str(&format!(
            "{:02}. {} [{}] -> {}\n",
            item.id, item.name, item.status, item.primary_surface
        ));
    }
    out.push_str("\nMatt Pocock recommended skills\n");
    out.push_str(&format!("tap={MATTPOCOCK_TAP}\n"));
    for skill in skills.iter().take(8) {
        out.push_str(&format!("- {} ({})\n", skill.id, skill.why));
    }
    out.push_str("\nStar-derived candidates\n");
    for skill in candidates.iter().take(8) {
        out.push_str(&format!("- {} ({})\n", skill.id, skill.why));
    }
    out.push_str("\nAutonomy surfaces\n");
    for surface in autonomy_surfaces() {
        let name = surface
            .get("surface")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let purpose = surface
            .get("purpose")
            .and_then(Value::as_str)
            .unwrap_or("runtime surface");
        out.push_str(&format!("- {name} ({purpose})\n"));
    }
    out.push_str("\nUse `harness_cockpit` tool action=status for structured JSON.");
    out
}

pub fn harness_items() -> Vec<HarnessItem> {
    vec![
        HarnessItem {
            id: 1,
            name: "Curated SOTA skill tap and recommendation layer",
            status: "implemented",
            primary_surface: "DEFAULT_SKILL_TAPS + /skills + harness_cockpit.skills",
            verification: "default tap regression + curated recommendation tests",
        },
        HarnessItem {
            id: 2,
            name: "Rust dashboard OIDC loader",
            status: "implemented_by_hermes_http_oidc",
            primary_surface: "hermes-http /auth/oidc/* + request guard",
            verification: "OIDC config/session/JWKS unit tests + HTTP auth smoke",
        },
        HarnessItem {
            id: 3,
            name: "Proof cockpit",
            status: "implemented",
            primary_surface: "harness_cockpit.proof + verification.status + telemetry_snapshot",
            verification: "snapshot includes passive terminal evidence and gate summaries",
        },
        HarnessItem {
            id: 4,
            name: "Domain model autopilot",
            status: "implemented_as_curated_workflow",
            primary_surface: "mattpocock/domain-modeling + CONTEXT.md/ADR guidance",
            verification: "skill recommendation indexes domain-modeling and ADR workflow",
        },
        HarnessItem {
            id: 5,
            name: "Architecture deepening report",
            status: "implemented_as_curated_workflow",
            primary_surface: "mattpocock/improve-codebase-architecture + /graph",
            verification: "skill recommendation indexes architecture deepening workflow",
        },
        HarnessItem {
            id: 6,
            name: "Deterministic replay",
            status: "implemented_existing_surface",
            primary_surface: "/studio replay + replay_trace_control + telemetry replay gate",
            verification: "telemetry snapshot reports deterministic replay gate",
        },
        HarnessItem {
            id: 7,
            name: "Agent black box recorder",
            status: "implemented_existing_surface",
            primary_surface: "raw trace, replay trace, verification evidence, tool result storage",
            verification: "cockpit proof section links evidence without private reasoning",
        },
        HarnessItem {
            id: 8,
            name: "Skill supply-chain scanner",
            status: "implemented_existing_surface",
            primary_surface: "hermes-skills guard + skills audit/quality + hub lock",
            verification: "guard scan tests + cockpit indexes scanner candidates",
        },
        HarnessItem {
            id: 9,
            name: "Capability-aware model router",
            status: "implemented_existing_surface",
            primary_surface:
                "smart_model_routing + /model harness/backend + telemetry provider health",
            verification: "model/router tests remain authoritative",
        },
        HarnessItem {
            id: 10,
            name: "Objective cockpit",
            status: "implemented_existing_surface",
            primary_surface: "/objective status|verify|ledger|dag|eval + harness_cockpit.objective",
            verification: "objective command regression tests + cockpit surface map",
        },
        HarnessItem {
            id: 11,
            name: "Chaos simulator for agents",
            status: "implemented_as_runtime_checklist",
            primary_surface: "harness_cockpit.chaos + /simulate + existing failure gates",
            verification: "chaos scenario snapshot test",
        },
        HarnessItem {
            id: 12,
            name: "One-command repo onboarding",
            status: "implemented_existing_surface",
            primary_surface: "/boot + /walkthrough + project.facts/tree + /skills sync",
            verification: "cockpit onboarding commands test",
        },
        HarnessItem {
            id: 13,
            name: "Team memory with redaction",
            status: "implemented_existing_surface",
            primary_surface: "/memory + ContextLattice policy + verification evidence redaction",
            verification: "cockpit proof/objective references avoid secret values",
        },
        HarnessItem {
            id: 14,
            name: "Cross-agent delegation marketplace",
            status: "implemented_existing_surface",
            primary_surface: "/swarm + /quorum + delegate_task + subagent workflows",
            verification: "swarm/quorum command and tool tests",
        },
        HarnessItem {
            id: 15,
            name: "Interactive grill-me mode",
            status: "implemented_as_curated_workflow",
            primary_surface: "mattpocock/grill-me + grill-with-docs + clarify tool",
            verification: "skill recommendation indexes grill-me and grill-with-docs",
        },
    ]
}

pub fn mattpocock_recommended_skills() -> Vec<RecommendedSkill> {
    vec![
        RecommendedSkill {
            id: "domain-modeling",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/engineering/domain-modeling",
            why: "keeps CONTEXT.md, glossary, and ADRs aligned with the real codebase",
            tier: "sota",
        },
        RecommendedSkill {
            id: "grill-with-docs",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/engineering/grill-with-docs",
            why: "forces sharp doc-grounded questions before expensive work",
            tier: "sota",
        },
        RecommendedSkill {
            id: "codebase-design",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/engineering/codebase-design",
            why: "gives agents shared vocabulary for depth, seams, adapters, and locality",
            tier: "sota",
        },
        RecommendedSkill {
            id: "improve-codebase-architecture",
            source: "mattpocock/skills",
            install_ref:
                "github/mattpocock/skills/skills/engineering/improve-codebase-architecture",
            why: "turns architecture pressure into a concrete reviewable report",
            tier: "sota",
        },
        RecommendedSkill {
            id: "diagnosing-bugs",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/engineering/diagnosing-bugs",
            why: "requires a deterministic feedback loop before fixes",
            tier: "sota",
        },
        RecommendedSkill {
            id: "tdd",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/engineering/tdd",
            why: "complements Hermes test-first skill with domain and ADR discipline",
            tier: "sota",
        },
        RecommendedSkill {
            id: "teach",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/productivity/teach",
            why: "turns harness usage into an interactive learning surface",
            tier: "sota",
        },
        RecommendedSkill {
            id: "grill-me",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/productivity/grill-me",
            why: "makes clarification a deliberate runtime mode instead of ad-hoc questioning",
            tier: "sota",
        },
        RecommendedSkill {
            id: "handoff",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/productivity/handoff",
            why: "improves resumability and reduces context loss across agents",
            tier: "strong",
        },
        RecommendedSkill {
            id: "writing-great-skills",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/productivity/writing-great-skills",
            why: "raises the bar for future Hermes skill authoring",
            tier: "strong",
        },
    ]
}

pub fn star_skill_candidates() -> Vec<RecommendedSkill> {
    vec![
        RecommendedSkill {
            id: "addyosmani/agent-skills",
            source: "github-stars",
            install_ref: "https://github.com/addyosmani/agent-skills",
            why: "production-grade engineering skills worth indexing alongside Matt Pocock",
            tier: "evaluate",
        },
        RecommendedSkill {
            id: "github/spec-kit",
            source: "github-stars",
            install_ref: "https://github.com/github/spec-kit",
            why: "spec-driven development pairs well with proof cockpit and objectives",
            tier: "evaluate",
        },
        RecommendedSkill {
            id: "Forward-Future/loopy",
            source: "github-stars",
            install_ref: "https://github.com/Forward-Future/loopy",
            why: "agent loop patterns for repeatable workflow design",
            tier: "evaluate",
        },
        RecommendedSkill {
            id: "NVIDIA/SkillSpector",
            source: "github-stars",
            install_ref: "https://github.com/NVIDIA/SkillSpector",
            why: "security scanner inspiration for skill supply-chain hardening",
            tier: "evaluate",
        },
        RecommendedSkill {
            id: "microsoft/SkillOpt",
            source: "github-stars",
            install_ref: "https://github.com/microsoft/SkillOpt",
            why: "validation-gated optimization of reusable natural-language skills",
            tier: "evaluate",
        },
        RecommendedSkill {
            id: "sentient-agi/EvoSkill",
            source: "github-stars",
            install_ref: "https://github.com/sentient-agi/EvoSkill",
            why: "failed-trajectory skill synthesis for learning loops",
            tier: "evaluate",
        },
        RecommendedSkill {
            id: "bergside/awesome-design-skills",
            source: "github-stars",
            install_ref: "https://github.com/bergside/awesome-design-skills",
            why: "design skill catalog for frontend and artifact generation workflows",
            tier: "evaluate",
        },
        RecommendedSkill {
            id: "google/skills",
            source: "github-stars",
            install_ref: "https://github.com/google/skills",
            why: "Google product and technology skills for broader index coverage",
            tier: "evaluate",
        },
        RecommendedSkill {
            id: "kepano/obsidian-skills",
            source: "github-stars",
            install_ref: "https://github.com/kepano/obsidian-skills",
            why: "open-format note workflows for team memory and knowledge ops",
            tier: "evaluate",
        },
        RecommendedSkill {
            id: "garrytan/gstack",
            source: "github-stars",
            install_ref: "https://github.com/garrytan/gstack",
            why:
                "operator, release, design, and executive workflows already align with default taps",
            tier: "evaluate",
        },
    ]
}

fn proof_snapshot(repo_root: Option<&Path>) -> Value {
    json!({
        "status": "ok",
        "repo_root": repo_root.map(|path| path.display().to_string()),
        "gate_snapshot": repo_root
            .map(telemetry_gate_snapshot)
            .unwrap_or_else(|| json!({"status": "unknown", "reason": "repo_root_not_detected"})),
        "verification": verification_status(None, repo_root),
        "channels": [
            "verification.status RPC",
            "telemetry_snapshot tool",
            "replay_trace_control tool",
            "/studio replay verify",
            "/claims status",
            "ContextLattice checkpoint readback"
        ],
        "privacy": "evidence and summaries only; private reasoning is not exposed",
    })
}

fn objective_surfaces() -> Vec<Value> {
    vec![
        json!({"surface": "/objective status", "purpose": "show active objective contract"}),
        json!({"surface": "/objective verify", "purpose": "compare objective claims with evidence"}),
        json!({"surface": "/objective ledger", "purpose": "show learning/checkpoint events"}),
        json!({"surface": "/objective dag", "purpose": "show dependency graph and blockers"}),
        json!({"surface": "/subgoal", "purpose": "manage concrete child goals"}),
        json!({"surface": "ContextLattice checkpoint", "purpose": "durable resume state"}),
    ]
}

fn autonomy_surfaces() -> Vec<Value> {
    let resource_plan = resource_admission_plan(ResourceGovernorInput {
        cpu_cores: std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
        free_ram_mb: None,
        total_ram_mb: None,
        ram_per_agent_mb: 2048,
        min_free_ram_mb: 1024,
        token_budget_remaining: Some(128_000),
        per_agent_token_reserve: 32_000,
        user_override: None,
    });
    let memory = memory_lifecycle_snapshot(&[
        MemoryProviderSignal {
            provider: "builtin".to_string(),
            available: true,
            score: 1.20,
            confidence: 0.80,
            last_seen_days: 0,
            evidence_count: 1,
        },
        MemoryProviderSignal {
            provider: "contextlattice".to_string(),
            available: true,
            score: 1.25,
            confidence: 0.90,
            last_seen_days: 0,
            evidence_count: 3,
        },
    ]);
    vec![
        json!({"surface": "ultra_autonomy loop_evaluate", "purpose": "detect repeated tool calls, repeated failures, no-action loops, and repeated model text"}),
        json!({"surface": "ultra_autonomy board_*", "purpose": "JSON-backed task boards with dependencies, question state, comments, and token budgets"}),
        json!({"surface": "ultra_autonomy objective_bridge", "purpose": "materialize durable objectives into dependency-tracked board cards"}),
        json!({"surface": "ultra_autonomy resource_plan", "purpose": "resource-aware subagent concurrency admission", "sample": resource_plan}),
        json!({"surface": "ultra_autonomy memory_lifecycle", "purpose": "ContextLattice-first hot/warm/archive memory projection", "sample": memory}),
        json!({"surface": "ultra_autonomy memory_resolve", "purpose": "memory reinforcement/conflict resolution with provenance notes"}),
        json!({"surface": "ultra_autonomy service_plan", "purpose": "one-command service UX contract for hermes-ultra up"}),
        json!({"surface": "ultra_autonomy channel_surface", "purpose": "CLI/dashboard/gateway/Telegram status, skill, and permission surface map"}),
        json!({"surface": "ultra_autonomy events", "purpose": "dashboard/gateway SSE-style event envelopes for board and memory changes"}),
    ]
}

fn onboarding_commands() -> Vec<Value> {
    vec![
        json!({"command": "/boot", "purpose": "check readiness and auth/runtime state"}),
        json!({"command": "/walkthrough", "purpose": "guide first-run behavior"}),
        json!({"command": "/skills sync", "purpose": "hydrate bundled skills"}),
        json!({"command": "/skills quality", "purpose": "score installed skills"}),
        json!({"command": "/harness", "purpose": "show this one-true-harness cockpit"}),
        json!({"rpc": "project.facts", "purpose": "structured repo onboarding facts"}),
        json!({"rpc": "project.tree", "purpose": "bounded repo tree for dashboards"}),
    ]
}

fn chaos_scenarios() -> Vec<Value> {
    vec![
        json!({"id": "auth_expiry", "probe": "/auth verify", "expected": "clear stale credential guidance"}),
        json!({"id": "tool_failure", "probe": "/simulate <tool> <params>", "expected": "policy decision without execution"}),
        json!({"id": "dirty_worktree", "probe": "verification.status", "expected": "evidence remains explicit"}),
        json!({"id": "tcc_or_fda_failure", "probe": "/boot", "expected": "macOS access issue separated from chmod fixes"}),
        json!({"id": "rate_limit", "probe": "provider telemetry", "expected": "retry/backoff guidance"}),
        json!({"id": "merge_conflict", "probe": "skill: resolving-merge-conflicts", "expected": "non-destructive resolution path"}),
        json!({"id": "partial_build", "probe": "telemetry gates", "expected": "failed gate appears in proof cockpit"}),
        json!({"id": "interrupted_session", "probe": "/handoff + ContextLattice checkpoint", "expected": "resumable state"}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roadmap_tracks_all_issue_702_items() {
        let items = harness_items();
        assert_eq!(items.len(), 15);
        assert_eq!(items.first().unwrap().id, 1);
        assert_eq!(items.last().unwrap().id, 15);
        assert!(items.iter().any(|item| item.name.contains("OIDC")));
        assert!(items.iter().any(|item| item.name.contains("grill-me")));
    }

    #[test]
    fn mattpocock_recommendations_include_teach_and_domain_modeling() {
        let ids: Vec<_> = mattpocock_recommended_skills()
            .into_iter()
            .map(|skill| skill.id)
            .collect();
        assert!(ids.contains(&"teach"));
        assert!(ids.contains(&"domain-modeling"));
        assert!(ids.contains(&"grill-with-docs"));
    }

    #[test]
    fn star_candidates_include_security_and_spec_sources() {
        let ids: Vec<_> = star_skill_candidates()
            .into_iter()
            .map(|skill| skill.id)
            .collect();
        assert!(ids.contains(&"NVIDIA/SkillSpector"));
        assert!(ids.contains(&"github/spec-kit"));
        assert!(ids.contains(&"addyosmani/agent-skills"));
    }

    #[test]
    fn text_renderer_mentions_structured_tool() {
        let text = render_harness_cockpit_text(None);
        assert!(text.contains("Hermes one-true-harness cockpit"));
        assert!(text.contains("harness_cockpit"));
        assert!(text.contains("teach"));
        assert!(text.contains("ultra_autonomy"));
    }

    #[test]
    fn autonomy_action_exposes_contextlattice_memory_and_resource_plan() {
        let payload = harness_cockpit_action_snapshot("autonomy", None).expect("autonomy");
        let rendered = serde_json::to_string(&payload).expect("render");
        assert!(rendered.contains("contextlattice"));
        assert!(rendered.contains("resource_plan"));
        assert!(rendered.contains("objective_bridge"));
    }
}
