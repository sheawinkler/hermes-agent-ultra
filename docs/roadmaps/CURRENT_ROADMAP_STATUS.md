# Current Roadmap Status

Last reviewed: 2026-07-17

This file is the human roadmap index. The canonical active program is
`docs/roadmaps/ULTRA_EXECUTION_ROADMAP_2026-07-16.md`.

Generated parity artifacts under `docs/parity/` are quarantined as release
truth until P0-A lands and regenerates them from a fresh, fail-closed source
ledger. Missing, stale, malformed, or incomplete provenance must produce
`FAIL`, never an inferred pass.

## Current Baseline

| Signal | Current state | Evidence |
| --- | --- | --- |
| Fork baseline | `c002f8c4` (`main`, PR #720 merge) | repository history |
| Fetched upstream baseline | `0f102fa4` | canonical P0-A source ledger |
| Release baseline | `v0.22.0` | `docs/releases/v0.22.0.md` |
| Fork/upstream history | divergent; no trusted merge-base in available history | P0-A source-ledger audit |
| Issue #702 audit | `0 complete / 12 partial / 3 facade-only` | active execution roadmap |
| Upstream queue/readiness | fail-closed; local byte-level worktree digest still pending | P0-A |
| Working program | P0 provenance, scoped grants, queue/resource integrity, Discord ingress, verified local recovery | `handoffs/PROGRAM.md` |

Raw ahead/behind counts, issue state, catalog entries, fixture labels, generated
`status` fields, and prose evaluators are not capability or parity proof.

## P0 Execution State

| ID | State | Remaining gate |
| --- | --- | --- |
| P0-A | Complete-history source ledger and fail-closed queue/proof/readiness implemented | hash canonical index plus actual tracked bytes, detect hidden/racy changes, resample in every consumer, and pass clean generate-to-immediate-verify |
| P0-B | Versioned platform+DM-chat+user+operation grants implemented in isolation | integration review, directory-sync/atomic-replace durability, focused restart/failure/platform tests |
| P0-C | Transactional sender-provenance queue, ingress deduplication, reset/error/abort recovery, and opt-in resource ceilings implemented | rerun focused P0 suite after P0-B/Discord integration |
| P0-D | Verified unmatched upgrades recovered without branch replay | six focused redaction/session test groups passed; rerun after later CLI/config integration |
| P0-E | Native Discord Gateway v10 ingress implemented in isolation | loss-safe bounded backpressure, real thread context/participation, policy defaults, integration review, focused protocol tests |

No release-readiness PASS is authoritative until the remaining gates close and
the released artifact is smoke-tested.

## Existing Components to Extend

- `ReplayRecorder` is the session capture spine.
- `hermes_eval::RunRecord` and `JsonReporter` own benchmark artifacts.
- `ToolPolicyEngine` owns tool policy and simulation.
- `SkillGuard` owns skill scanning and content hashes.
- `SubAgentOrchestrator` owns real child-agent execution.
- `PluginManager` and the gateway hook registry own hook dispatch.
- `harness_cockpit` remains an evidence index; it does not own subsystem
  implementations.

Parallel replacements for those components are prohibited.

## Dependency Order

1. P0: fail-closed truth, scoped authorization, queue integrity, verified local recovery.
2. P1: Session RunRecord v1, durable outbox, typed runtime modes and standards OIDC.
3. P2: enforced code-execution sandbox, package trust, unified privacy.
4. P3-P4: deterministic replay, real evaluations, measured promotion and rollback gates.
5. P5-P7: durable hooks, governed team DAG, background work, memory/routing, interoperability and observability.
6. P8-P10: Rust control plane, remote workers/enterprise governance, gated self-improvement.

Every capability requires a production callsite, real consumer, failure-path
test, public smoke, eval-ledger record, and rollback. See the canonical roadmap
for exact work and gates.

Repo-local `handoffs/PROGRAM.md` is the durable packet index. Each packet records
its baseline SHA, pre-edit gap/provenance verdict, intentional files, focused
checks, and remaining risks. ContextLattice stores compact decision checkpoints;
neither handoffs nor ContextLattice substitute for runtime proof.

## Upstream and Model Policy

- Re-fetch both remotes and regenerate the source ledger for every release
  candidate.
- Classify only exact, patch-equivalent, tree-equivalent, intentionally
  superseded, intentionally ported, deferred-with-owner, or rejected-with-
  evidence changes.
- Do not infer parity from commit counts or a prior disposition.
- Kimi K3 paid inference probing is deferred by operator direction. Catalog or
  profile parity may not be presented as live runtime proof.

## Next Gate

Complete P0-A byte-level local provenance, integrate and verify the reviewed P0-B
and P0-E isolated deltas, then rerun the complete focused P0 scenario suite.
Only after that may the queue, global proof, readiness summary, released-artifact
smoke evidence, and capability status be regenerated as candidate release truth.
