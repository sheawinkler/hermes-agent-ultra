# Hermes Ultra Alpha (61 Elements)

Source: `plans/alpha/hermes-ultra-alpha-61-elements.json`

## Runtime Surfaces

- CLI: `/objective ...` manages Objective OS contracts, policies, ledgers, DAGs, and eval trends.
- CLI: `/mission ...` manages Mission Control loops, queue recovery/replay, and private trading/autoresearch boards.
- Tools: `objective_snapshot` and `mission_snapshot` expose the persisted alpha runtime state as read-only structured JSON for agents.
- Tests: `crates/hermes-cli/src/alpha_runtime.rs` covers alpha state mutation/rendering, and `crates/hermes-tools/src/tools/alpha_snapshot.rs` covers read-only agent snapshots.

| ID | Area | Title | Priority | Trading-sensitive |
| --- | --- | --- | --- | --- |
| ALPHA-001 | Objective OS | Persistent objective contract engine | P0 | no |
| ALPHA-002 | Objective OS | Utility-function parser with hard constraints | P0 | no |
| ALPHA-003 | Objective OS | Multi-horizon mission planner (intra/day/week) | P0 | no |
| ALPHA-004 | Objective OS | Evidence-to-action promotion gate | P0 | no |
| ALPHA-005 | Objective OS | Continuous capital allocator framework | P0 | yes |
| ALPHA-006 | Objective OS | Portfolio-level risk governor kernel | P0 | yes |
| ALPHA-007 | Objective OS | Counterfactual decision journal | P1 | no |
| ALPHA-008 | Objective OS | Objective confidence + certainty calibration | P1 | no |
| ALPHA-009 | Subagents | Subagent registry with role profiles | P0 | no |
| ALPHA-010 | Subagents | Subagent budget policies (time/tool/token) | P0 | no |
| ALPHA-011 | Subagents | Deterministic run graph + lineage IDs | P0 | no |
| ALPHA-012 | Subagents | Durable worker checkpoint and resume | P0 | no |
| ALPHA-013 | Subagents | Cross-agent contradiction detector | P1 | no |
| ALPHA-014 | Subagents | Escalation ladder for blocked agents | P1 | no |
| ALPHA-015 | Subagents | Subagent skill affinity matcher | P1 | no |
| ALPHA-016 | ContextLattice | Mandatory preflight orchestrator handshake | P0 | no |
| ALPHA-017 | ContextLattice | Auto context-pack at mission start | P0 | no |
| ALPHA-018 | ContextLattice | Retrieval degradation-aware planning | P0 | no |
| ALPHA-019 | ContextLattice | Checkpoint write policy by lifecycle phase | P0 | no |
| ALPHA-020 | ContextLattice | Scoped readback verification before finalization | P0 | no |
| ALPHA-021 | ContextLattice | Shared topic taxonomy and namespace contracts | P1 | no |
| ALPHA-022 | ContextLattice | Memory conflict resolution and provenance merge | P1 | no |
| ALPHA-023 | Loop Runtime | Continuous loop supervisor service | P0 | no |
| ALPHA-024 | Loop Runtime | Loop DSL for declarative runbooks | P1 | no |
| ALPHA-025 | Loop Runtime | Durable event queue with exactly-once-ish replay | P0 | no |
| ALPHA-026 | Loop Runtime | Crash-safe recovery + orphan cleanup | P0 | no |
| ALPHA-027 | Loop Runtime | Hot config reload with schema validation | P1 | no |
| ALPHA-028 | Loop Runtime | Loop health scoring and SLO monitors | P1 | no |
| ALPHA-029 | Loop Runtime | Channel-aware alert routing | P1 | no |
| ALPHA-030 | Trading Objective | Wallet growth KPI engine (0.2 -> 1000 SOL) | P0 | yes |
| ALPHA-031 | Trading Objective | Drawdown and ruin-probability circuit breaker | P0 | yes |
| ALPHA-032 | Trading Objective | Volatility-aware position sizing policy | P0 | yes |
| ALPHA-033 | Trading Objective | Strategy ensemble reweighting allocator | P0 | yes |
| ALPHA-034 | Trading Objective | PnL decomposition (signal/execution/cost) | P0 | yes |
| ALPHA-035 | Trading Objective | Slippage/impact model with adaptive guardrails | P1 | yes |
| ALPHA-036 | Trading Objective | Regime classifier for policy switching | P1 | yes |
| ALPHA-037 | Trading Objective | Live-shadow canary promotion pipeline | P1 | yes |
| ALPHA-038 | Kraken Loop | Kraken telemetry collector (latency/fills/rejects) | P0 | yes |
| ALPHA-039 | Kraken Loop | Execution quality monitor (spread/impact) | P0 | yes |
| ALPHA-040 | Kraken Loop | Anomaly detector with rollback hooks | P0 | yes |
| ALPHA-041 | Kraken Loop | Fee/funding drag tracker | P1 | yes |
| ALPHA-042 | Kraken Loop | Exchange incident classifier + failover policy | P1 | yes |
| ALPHA-043 | Kraken Loop | Automated postmortem packet generator | P1 | yes |
| ALPHA-044 | algotraderv2_rust Loop | Repo drift sentinel for behavior changes | P0 | yes |
| ALPHA-045 | algotraderv2_rust Loop | run_context auditor and invariants checker | P0 | yes |
| ALPHA-046 | algotraderv2_rust Loop | Env/config provenance gate | P0 | yes |
| ALPHA-047 | algotraderv2_rust Loop | Replay canary harness on fresh telemetry | P1 | yes |
| ALPHA-048 | algotraderv2_rust Loop | Patch recommendation ranker | P1 | yes |
| ALPHA-049 | algotraderv2_rust Loop | Automated remediation runbook executor | P1 | yes |
| ALPHA-050 | Autoresearch | Research source ingestion pipeline | P0 | yes |
| ALPHA-051 | Autoresearch | Hypothesis generator with novelty filter | P0 | yes |
| ALPHA-052 | Autoresearch | Experiment spec compiler | P0 | yes |
| ALPHA-053 | Autoresearch | Backtest matrix orchestrator | P0 | yes |
| ALPHA-054 | Autoresearch | Walk-forward + leakage defense suite | P0 | yes |
| ALPHA-055 | Autoresearch | Meta-analysis and strategy ranking | P1 | yes |
| ALPHA-056 | Autoresearch | Promotion-to-canary recommender | P1 | yes |
| ALPHA-057 | Provider Intelligence | Capability-based model router | P0 | no |
| ALPHA-058 | Provider Intelligence | Reasoning-level policy by task criticality | P0 | no |
| ALPHA-059 | Provider Intelligence | Provider health/cost/latency arbitration | P0 | no |
| ALPHA-060 | Provider Intelligence | OAuth/session validity sentinel + refresh hints | P1 | no |
| ALPHA-061 | Operator UX | Mission Control board (multi-loop live state) | P0 | no |
