//! Audited harness capability-inventory snapshots.
//!
//! This tool composes existing Hermes surfaces without replacing them:
//! telemetry gates, verification evidence, skill taps, objective commands,
//! replay controls, and dashboard auth stay owned by their subsystem. Catalog,
//! diagnostic, and checklist presence is reported separately from production
//! completeness.

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

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HarnessMaturity {
    Complete,
    Partial,
    FacadeOnly,
}

impl HarnessMaturity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::FacadeOnly => "facade_only",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct HarnessMaturitySummary {
    pub complete: usize,
    pub partial: usize,
    pub facade_only: usize,
}

impl HarnessMaturitySummary {
    fn is_complete(self) -> bool {
        self.complete > 0 && self.partial == 0 && self.facade_only == 0
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct HarnessItem {
    pub id: u8,
    pub name: &'static str,
    pub maturity: HarnessMaturity,
    pub primary_surface: &'static str,
    pub observed_evidence: &'static str,
    pub missing_evidence: &'static str,
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
            "Report the audited Hermes production-completeness inventory without promoting catalogs, diagnostics, or unit coverage to implemented capabilities.",
            JsonSchema::object(props, vec![]),
        )
    }
}

pub fn harness_cockpit_snapshot(repo_root: Option<&Path>) -> Value {
    let roadmap = harness_items();
    let maturity = harness_maturity_summary(&roadmap);
    json!({
        "status": "inventory",
        "production_complete": maturity.is_complete(),
        "mission": "one_true_rust_first_agent_harness",
        "repo_root": repo_root.map(|path| path.display().to_string()),
        "issue": "https://github.com/sheawinkler/hermes-agent-ultra/issues/702",
        "maturity": maturity,
        "roadmap": roadmap,
        "skills": {
            "status": "recommendations_only",
            "production_complete": false,
            "default_tap": MATTPOCOCK_TAP,
            "mattpocock": mattpocock_recommended_skills(),
            "star_candidates": star_skill_candidates(),
        },
        "proof": proof_snapshot(repo_root),
        "objectives": {
            "status": "partial",
            "production_complete": false,
            "surfaces": objective_surfaces(),
        },
        "autonomy": {
            "status": "inventory",
            "surfaces": autonomy_surfaces(),
        },
        "onboarding": {
            "status": "component_commands",
            "production_complete": false,
            "commands": onboarding_commands(),
        },
        "chaos": {
            "status": "checklist_only",
            "production_complete": false,
            "scenarios": chaos_scenarios(),
        },
    })
}

pub fn harness_cockpit_action_snapshot(
    action: &str,
    repo_root: Option<&Path>,
) -> Result<Value, String> {
    match action.trim().to_ascii_lowercase().as_str() {
        "" | "status" | "all" => Ok(harness_cockpit_snapshot(repo_root)),
        "skills" => Ok(json!({
            "status": "recommendations_only",
            "production_complete": false,
            "default_tap": MATTPOCOCK_TAP,
            "mattpocock": mattpocock_recommended_skills(),
            "star_candidates": star_skill_candidates(),
        })),
        "proof" => Ok(proof_snapshot(repo_root)),
        "roadmap" => {
            let items = harness_items();
            let maturity = harness_maturity_summary(&items);
            Ok(json!({
                "status": "inventory",
                "production_complete": maturity.is_complete(),
                "maturity": maturity,
                "items": items,
            }))
        }
        "chaos" => Ok(json!({
            "status": "checklist_only",
            "production_complete": false,
            "scenarios": chaos_scenarios(),
        })),
        "onboarding" => Ok(json!({
            "status": "component_commands",
            "production_complete": false,
            "commands": onboarding_commands(),
        })),
        "objective" | "objectives" => Ok(json!({
            "status": "partial",
            "production_complete": false,
            "surfaces": objective_surfaces(),
        })),
        "autonomy" => Ok(json!({
            "status": "inventory",
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
    let maturity = harness_maturity_summary(&items);
    let skills = mattpocock_recommended_skills();
    let candidates = star_skill_candidates();
    let mut out = String::new();
    out.push_str("Hermes production-completeness cockpit\n");
    out.push_str("======================================\n");
    out.push_str("status=inventory\n");
    out.push_str(&format!("production_complete={}\n", maturity.is_complete()));
    out.push_str(&format!(
        "maturity=complete:{} partial:{} facade_only:{}\n",
        maturity.complete, maturity.partial, maturity.facade_only
    ));
    out.push_str("issue=https://github.com/sheawinkler/hermes-agent-ultra/issues/702\n");
    if let Some(root) = repo_root {
        out.push_str(&format!("repo_root={}\n", root.display()));
    }
    out.push_str("\nRoadmap surfaces\n");
    for item in &items {
        out.push_str(&format!(
            "{:02}. {} [{}] -> {}; missing: {}\n",
            item.id,
            item.name,
            item.maturity.as_str(),
            item.primary_surface,
            item.missing_evidence
        ));
    }
    out.push_str("\nRecommended skills (catalog evidence only)\n");
    out.push_str(&format!("tap={MATTPOCOCK_TAP}\n"));
    for skill in skills.iter().take(8) {
        out.push_str(&format!("- {} ({})\n", skill.id, skill.why));
    }
    out.push_str("\nStar-derived candidates (not installed capabilities)\n");
    for skill in candidates.iter().take(8) {
        out.push_str(&format!("- {} ({})\n", skill.id, skill.why));
    }
    out.push_str("\nAutonomy surface inventory\n");
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
            maturity: HarnessMaturity::Partial,
            primary_surface: "DEFAULT_SKILL_TAPS + /skills + harness_cockpit.skills",
            observed_evidence: "tap merge/install paths, SkillGuard, and recommendation tests",
            missing_evidence:
                "indexed provenance, quality, compatibility, revocation, runtime install smoke, and eval ledger",
        },
        HarnessItem {
            id: 2,
            name: "Rust dashboard OIDC loader",
            maturity: HarnessMaturity::Partial,
            primary_surface: "hermes-http /auth/oidc/* + request guard",
            observed_evidence: "dashboard routes, request guard, and config/session/JWKS unit coverage",
            missing_evidence:
                "protocol lifecycle integration for discovery, PKCE, state/nonce, rotation, refresh, expiry, revocation, and browser sessions",
        },
        HarnessItem {
            id: 3,
            name: "Proof cockpit",
            maturity: HarnessMaturity::Partial,
            primary_surface: "verification.status + telemetry_snapshot + harness_cockpit.proof",
            observed_evidence: "diagnostic aggregate readers and generated parity evidence",
            missing_evidence:
                "authoritative runtime-event lineage and digests with failure, public smoke, and eval-ledger evidence",
        },
        HarnessItem {
            id: 4,
            name: "Domain model autopilot",
            maturity: HarnessMaturity::FacadeOnly,
            primary_surface: "recommended skill catalog entry",
            observed_evidence: "domain-modeling recommendation metadata only",
            missing_evidence:
                "repository-analysis producer, persisted domain artifact, real consumer, failure handling, smoke, and evaluation",
        },
        HarnessItem {
            id: 5,
            name: "Architecture deepening report",
            maturity: HarnessMaturity::FacadeOnly,
            primary_surface: "recommended skill catalog entry + unrelated /graph diagnostics",
            observed_evidence: "architecture-skill recommendation metadata only",
            missing_evidence:
                "repository-analysis producer, generated architecture report, real consumer, failure handling, smoke, and evaluation",
        },
        HarnessItem {
            id: 6,
            name: "Deterministic replay",
            maturity: HarnessMaturity::Partial,
            primary_surface: "/raw trace export|verify + /studio replay + telemetry replay gate",
            observed_evidence: "replay controls and diagnostic gate snapshots",
            missing_evidence:
                "provider-free exact event/artifact transcript replay, branch lineage, drift detection, crash tests, and eval ledger",
        },
        HarnessItem {
            id: 7,
            name: "Agent black box recorder",
            maturity: HarnessMaturity::Partial,
            primary_surface: "ReplayRecorder + raw trace + verification evidence + tool result storage",
            observed_evidence: "redacted hash-chained JSONL events and trace consumers",
            missing_evidence:
                "typed transactional RunRecord, crash-safe ordering, complete outcomes and lineage, artifact digests, fsync, and restart proof",
        },
        HarnessItem {
            id: 8,
            name: "Skill supply-chain scanner",
            maturity: HarnessMaturity::Partial,
            primary_surface: "hermes-skills SkillGuard + duplicate CLI scanner + hub lock",
            observed_evidence: "guard scans, quality commands, and catalog candidates",
            missing_evidence:
                "one production scanner/index with publisher provenance, signature/revocation policy, behavioral verification, and eval ledger",
        },
        HarnessItem {
            id: 9,
            name: "Capability-aware model router",
            maturity: HarnessMaturity::Partial,
            primary_surface: "hermes_agent::smart_model_routing::resolve_turn_route + duplicate intelligence router",
            observed_evidence: "production heuristic route selection and router unit tests",
            missing_evidence:
                "one consolidated router with fixed holdout, confidence, cost/latency bounds, fallback, override, and drift evaluation",
        },
        HarnessItem {
            id: 10,
            name: "Objective cockpit",
            maturity: HarnessMaturity::Partial,
            primary_surface: "/objective status|verify|ledger|dag|eval + snapshot plane",
            observed_evidence: "objective commands, contracts, and diagnostic snapshots",
            missing_evidence:
                "durable policy-enforced resumable execution with event lineage, crash recovery, public smoke, and evaluation",
        },
        HarnessItem {
            id: 11,
            name: "Chaos simulator for agents",
            maturity: HarnessMaturity::Partial,
            primary_surface: "adapter chaos fixtures + policy simulation + cockpit checklist",
            observed_evidence: "real component failure fixtures and non-executing policy probes",
            missing_evidence:
                "production-path fault injectors, recovery assertions, durable artifacts, public smoke, and evaluation",
        },
        HarnessItem {
            id: 12,
            name: "One-command repo onboarding",
            maturity: HarnessMaturity::Partial,
            primary_surface: "/boot + /walkthrough + project.facts/tree + /skills sync",
            observed_evidence: "individual onboarding commands and component regressions",
            missing_evidence:
                "clean-machine one-command journey with successful recovery paths, user-facing smoke, and evaluation",
        },
        HarnessItem {
            id: 13,
            name: "Team memory with redaction",
            maturity: HarnessMaturity::Partial,
            primary_surface: "memory manager + ContextLattice adapter + verification redaction",
            observed_evidence: "memory storage/retrieval surfaces and redaction helpers",
            missing_evidence:
                "team lifecycle, tenant scope, privacy/retention, concurrent writes, forget/export, audit, public smoke, and evaluation",
        },
        HarnessItem {
            id: 14,
            name: "Cross-agent delegation marketplace",
            maturity: HarnessMaturity::Partial,
            primary_surface: "SubAgentOrchestrator + /swarm + /quorum + delegate_task",
            observed_evidence: "ephemeral delegation and orchestration component tests",
            missing_evidence:
                "durable DAG, isolation, locks, budgets, approvals, crash recovery, retry/cancel semantics, side-effect lineage, and evaluation",
        },
        HarnessItem {
            id: 15,
            name: "Interactive grill-me mode",
            maturity: HarnessMaturity::FacadeOnly,
            primary_surface: "recommended skill catalog entries + generic clarify tool",
            observed_evidence: "grill-me recommendation metadata only",
            missing_evidence:
                "executable adversarial workflow, persisted verdict, production consumer, failure handling, public smoke, and evaluation",
        },
    ]
}

fn harness_maturity_summary(items: &[HarnessItem]) -> HarnessMaturitySummary {
    let mut summary = HarnessMaturitySummary {
        complete: 0,
        partial: 0,
        facade_only: 0,
    };
    for item in items {
        match item.maturity {
            HarnessMaturity::Complete => summary.complete += 1,
            HarnessMaturity::Partial => summary.partial += 1,
            HarnessMaturity::FacadeOnly => summary.facade_only += 1,
        }
    }
    summary
}

pub fn mattpocock_recommended_skills() -> Vec<RecommendedSkill> {
    vec![
        RecommendedSkill {
            id: "domain-modeling",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/engineering/domain-modeling",
            why: "keeps CONTEXT.md, glossary, and ADRs aligned with the real codebase",
            tier: "recommended",
        },
        RecommendedSkill {
            id: "grill-with-docs",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/engineering/grill-with-docs",
            why: "forces sharp doc-grounded questions before expensive work",
            tier: "recommended",
        },
        RecommendedSkill {
            id: "codebase-design",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/engineering/codebase-design",
            why: "gives agents shared vocabulary for depth, seams, adapters, and locality",
            tier: "recommended",
        },
        RecommendedSkill {
            id: "improve-codebase-architecture",
            source: "mattpocock/skills",
            install_ref:
                "github/mattpocock/skills/skills/engineering/improve-codebase-architecture",
            why: "turns architecture pressure into a concrete reviewable report",
            tier: "recommended",
        },
        RecommendedSkill {
            id: "diagnosing-bugs",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/engineering/diagnosing-bugs",
            why: "requires a deterministic feedback loop before fixes",
            tier: "recommended",
        },
        RecommendedSkill {
            id: "tdd",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/engineering/tdd",
            why: "complements Hermes test-first skill with domain and ADR discipline",
            tier: "recommended",
        },
        RecommendedSkill {
            id: "teach",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/productivity/teach",
            why: "turns harness usage into an interactive learning surface",
            tier: "recommended",
        },
        RecommendedSkill {
            id: "grill-me",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/productivity/grill-me",
            why: "makes clarification a deliberate runtime mode instead of ad-hoc questioning",
            tier: "recommended",
        },
        RecommendedSkill {
            id: "handoff",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/productivity/handoff",
            why: "improves resumability and reduces context loss across agents",
            tier: "recommended",
        },
        RecommendedSkill {
            id: "writing-great-skills",
            source: "mattpocock/skills",
            install_ref: "github/mattpocock/skills/skills/productivity/writing-great-skills",
            why: "raises the bar for future Hermes skill authoring",
            tier: "recommended",
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
        "status": "diagnostic",
        "production_complete": false,
        "missing_evidence": "authoritative typed RunRecord lineage and artifact digests",
        "repo_root": repo_root.map(|path| path.display().to_string()),
        "gate_snapshot": repo_root
            .map(telemetry_gate_snapshot)
            .unwrap_or_else(|| json!({"status": "unknown", "reason": "repo_root_not_detected"})),
        "verification": verification_status(None, repo_root),
        "channels": [
            "verification.status RPC",
            "telemetry_snapshot tool",
            "/raw trace verify (verify_run_export)",
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
        json!({"command": "/harness", "purpose": "show the audited production-completeness inventory"}),
        json!({"rpc": "project.facts", "purpose": "structured repo onboarding facts"}),
        json!({"rpc": "project.tree", "purpose": "bounded repo tree for dashboards"}),
    ]
}

fn chaos_scenarios() -> Vec<Value> {
    vec![
        json!({"id": "auth_expiry", "evidence_type": "checklist", "executable_fault_injector": false, "probe": "/auth verify", "expected": "clear stale credential guidance"}),
        json!({"id": "tool_failure", "evidence_type": "checklist", "executable_fault_injector": false, "probe": "/simulate <tool> <params>", "expected": "policy decision without execution"}),
        json!({"id": "dirty_worktree", "evidence_type": "checklist", "executable_fault_injector": false, "probe": "verification.status", "expected": "evidence remains explicit"}),
        json!({"id": "tcc_or_fda_failure", "evidence_type": "checklist", "executable_fault_injector": false, "probe": "/boot", "expected": "macOS access issue separated from chmod fixes"}),
        json!({"id": "rate_limit", "evidence_type": "checklist", "executable_fault_injector": false, "probe": "provider telemetry", "expected": "retry/backoff guidance"}),
        json!({"id": "merge_conflict", "evidence_type": "checklist", "executable_fault_injector": false, "probe": "skill: resolving-merge-conflicts", "expected": "non-destructive resolution path"}),
        json!({"id": "partial_build", "evidence_type": "checklist", "executable_fault_injector": false, "probe": "telemetry gates", "expected": "failed gate appears in proof cockpit"}),
        json!({"id": "interrupted_session", "evidence_type": "checklist", "executable_fault_injector": false, "probe": "/handoff + ContextLattice checkpoint", "expected": "resumable state"}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roadmap_tracks_exact_issue_702_maturity_without_false_promotion() {
        let items = harness_items();
        assert_eq!(items.len(), 15);
        assert_eq!(items.first().unwrap().id, 1);
        assert_eq!(items.last().unwrap().id, 15);
        assert_eq!(
            harness_maturity_summary(&items),
            HarnessMaturitySummary {
                complete: 0,
                partial: 12,
                facade_only: 3,
            }
        );
        assert!(items
            .iter()
            .all(|item| !item.observed_evidence.is_empty() && !item.missing_evidence.is_empty()));
        assert!(items.iter().any(|item| item.name.contains("OIDC")));
        assert!(items.iter().any(|item| item.name.contains("grill-me")));
    }

    #[test]
    fn deterministic_replay_claims_use_cli_export_not_legacy_tool() {
        let replay = harness_items()
            .into_iter()
            .find(|item| item.name == "Deterministic replay")
            .expect("replay item");
        assert!(!replay.primary_surface.contains("replay_trace_control"));
        assert!(replay.primary_surface.contains("/raw trace export|verify"));

        let proof = harness_cockpit_action_snapshot("proof", None).expect("proof");
        let channels = proof["channels"]
            .as_array()
            .expect("channels")
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>();
        assert!(channels
            .iter()
            .all(|channel| !channel.contains("replay_trace_control")));
        assert!(channels
            .iter()
            .any(|channel| channel.contains("/raw trace verify")));
    }

    #[test]
    fn mattpocock_catalog_is_recommendations_not_capability_proof() {
        let skills = mattpocock_recommended_skills();
        let ids: Vec<_> = skills.iter().map(|skill| skill.id).collect();
        assert!(ids.contains(&"teach"));
        assert!(ids.contains(&"domain-modeling"));
        assert!(ids.contains(&"grill-with-docs"));
        assert!(skills.iter().all(|skill| skill.tier == "recommended"));
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
    fn text_renderer_reports_incomplete_maturity_and_structured_tool() {
        let text = render_harness_cockpit_text(None);
        assert!(text.contains("Hermes production-completeness cockpit"));
        assert!(text.contains("production_complete=false"));
        assert!(text.contains("complete:0 partial:12 facade_only:3"));
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

    #[test]
    fn catalog_actions_are_machine_readably_non_authoritative() {
        let roadmap = harness_cockpit_action_snapshot("roadmap", None).expect("roadmap");
        assert_eq!(roadmap["production_complete"], false);
        assert_eq!(roadmap["maturity"]["complete"], 0);
        assert_eq!(roadmap["maturity"]["partial"], 12);
        assert_eq!(roadmap["maturity"]["facade_only"], 3);

        let chaos = harness_cockpit_action_snapshot("chaos", None).expect("chaos");
        assert_eq!(chaos["status"], "checklist_only");
        assert_eq!(chaos["production_complete"], false);
        assert!(chaos["scenarios"]
            .as_array()
            .expect("scenarios")
            .iter()
            .all(|scenario| scenario["executable_fault_injector"] == false));
    }
}
