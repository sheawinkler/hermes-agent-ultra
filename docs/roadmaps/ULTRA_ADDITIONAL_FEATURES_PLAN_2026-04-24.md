# Hermes-Agent-Ultra Roadmap: Improved Functionality from NousResearch Hermes-Agent via Additional Features

Date: 2026-04-24
Owner: @sheawinkler
Scope: Rust-first implementation across `crates/hermes-agent`, `crates/hermes-tools`, `crates/hermes-mcp`, `crates/hermes-cli`, and `scripts/`.

## Objective
Implement eight production enhancements that preserve upstream functional parity while adding stronger reliability, security, observability, and autonomous upkeep behavior.

## Workstream 1: Deterministic Replay + Incident Packs
- Add replay event schema and recorder for LLM/tool lifecycle events.
- Record turn-level events (model route, usage, tool calls, tool outputs, errors, interrupts) to NDJSON under state root.
- Add deterministic redaction for sensitive fields in replay artifacts.
- Add incident pack generation that bundles:
  - replay traces
  - debug report
  - recent runtime logs
  - gateway/service status snapshot
- Integrate with CLI debug/doctor flows.

Acceptance:
- Replay files created when enabled and readable as valid NDJSON.
- Incident pack archive generated with required sections.
- Unit tests for replay writer + redaction + pack assembly.

## Workstream 2: Zero-Copy Hot Path Pass
- Replace high-copy JSON decode paths with byte-slice decode where possible.
- Apply to MCP stdio receive/server receive and HTTP transport response decode.
- Add bounded message-size checks before decode for predictable memory usage.

Acceptance:
- Target decode paths use `serde_json::from_slice`.
- Behavior unchanged in existing transport tests.
- New tests cover oversized message rejection.

## Workstream 3: MCP Hard Sandbox Profiles
- Implement sandbox profiles (`strict`, `balanced`, `relaxed`) for stdio MCP subprocesses.
- Enforce profile guardrails:
  - command allowlist checks
  - sensitive env stripping
  - optional fixed working directory
  - explicit message-size caps
- Log policy decisions and rejection reasons.

Acceptance:
- Strict profile blocks disallowed commands.
- Sensitive env keys are stripped from child process env.
- Tests validate profile behavior and caps.

## Workstream 4: Memory Fusion Scoring (ContextLattice + Other Backends)
- Replace simple prefetch concatenation with scored fusion in `MemoryManager`.
- Add provider weights, lexical overlap scoring, and source confidence metadata.
- Limit injected context blocks by rank to reduce noise.

Acceptance:
- Fused memory context includes source and score metadata.
- Weighted ranking is deterministic for same inputs.
- Tests verify ranking and cap behavior.

## Workstream 5: Autonomous Parity Updater (Draft PR path)
- Extend webhook/worker automation to optionally open draft parity PRs.
- Include drift artifacts, parity queue summaries, and risk labels in PR body.
- Keep issue creation flow as fallback when PR open fails.

Acceptance:
- New worker flags support draft PR mode.
- Dry-run preserves no-op behavior.
- Generated PR body includes drift classification + test guidance.

## Workstream 6: Policy Engine for Tool Execution
- Add centralized tool policy preflight before tool handler execution.
- Policy file supports:
  - deny list by tool name
  - regex deny patterns for argument payloads
  - mode (`enforce` vs `audit`)
- Return structured denial errors in enforce mode.

Acceptance:
- Tool dispatch is blocked when policy denies in enforce mode.
- Audit mode logs but allows execution.
- Unit tests for deny/audit and pattern matching.

## Workstream 7: Performance Governor
- Add adaptive governor for LLM and tool execution pressure.
- Track latency/error rolling window.
- Adjust:
  - effective max tokens for LLM calls
  - effective parallel tool concurrency per turn
- Emit status telemetry for governor adjustments.

Acceptance:
- Governor reacts to degraded latency/error conditions.
- Concurrency cap and max-token adjustment both applied.
- Unit tests for adjustment logic.

## Workstream 8: Operational Control Plane (`doctor --deep` + snapshots)
- Extend `doctor` with deep diagnostics and optional snapshot export.
- Add health snapshot JSON artifacts under state root.
- Integrate support bundle generation command path.

Acceptance:
- `doctor --deep` outputs extended checks.
- `doctor --snapshot` writes machine-readable health artifact.
- Tests validate snapshot generation.

## Delivery Order
1. WS2 + WS3 foundation in MCP transport
2. WS6 policy engine in tool registry
3. WS7 governor + WS1 replay in agent loop
4. WS4 memory fusion in memory manager
5. WS8 doctor/control-plane
6. WS5 parity updater draft PR integration
7. Final docs/tests + parity artifact refresh

## Verification Matrix
- `cargo test -p hermes-mcp`
- `cargo test -p hermes-tools`
- `cargo test -p hermes-agent`
- `cargo test -p hermes-cli`
- `python3 scripts/generate-upstream-patch-queue.py --max-commits 0 --no-fetch`
- `python3 scripts/generate-workstream-status.py`
- `python3 scripts/generate-global-parity-proof.py --check-ci --check-release`

## Rollout Notes
- All new runtime behaviors are gated by env/profile defaults to preserve compatibility.
- Fail-closed controls apply only in explicit `strict/enforce` modes.
- Upstream parity tracking remains active; additions are ultra-only enhancements on top.
