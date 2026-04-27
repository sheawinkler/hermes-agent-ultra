# ULTRA ELITE Phase 3 Implementation Plan (ELITE-21..ELITE-28)

## Scope
Deliver the next elite tranche as production features (no placeholders):

- ELITE-21: differential parity runner
- ELITE-22: SLO auto-rollback hook
- ELITE-23: router health scorer
- ELITE-24: execution sandbox profile hardening
- ELITE-25: deterministic incident pack command
- ELITE-26: eval trend regression gate
- ELITE-27: memory fusion confidence gating + trace
- ELITE-28: operator TUI control plane

## Implementation

### ELITE-21 Differential Parity Runner
- Add `scripts/run-differential-parity-gate.py`.
- Compare local CLI command/action surface against `upstream/main`.
- Include commit behind/ahead metrics and hard gate on missing surface deltas.

### ELITE-22 SLO Auto-Rollback
- Add `scripts/run-slo-auto-rollback.py`.
- Execute arbitrary check command; on failure trigger rollback command.
- Add `--elite-rollback-cmd` wiring in `scripts/sync-upstream.sh`.
- Add `--rollback-cmd` support in `scripts/run-elite-sync-gate.py`.

### ELITE-23 Router Health Scorer
- Add CLI command: `hermes route-health [show|reset] [--json]`.
- Compute route health from learned routing state with tiering:
  - `healthy`, `watch`, `degraded`, `critical`
- Persist report at `$HERMES_HOME/logs/route-health.json`.
- Surface summary in `hermes status` and doctor elite diagnostics.

### ELITE-24 Sandbox Profile Hardening
- Extend `hermes-tools` policy engine with execution sandbox profiles:
  - `strict`, `balanced`, `dev`
- New env: `HERMES_EXECUTION_SANDBOX_PROFILE`.
- Enforce command-channel protections in strict/balanced modes for terminal-like tools.

### ELITE-25 Deterministic Incident Pack
- Add CLI command: `hermes incident-pack [--snapshot <path>] [--output <path>] [--json]`.
- Build deterministic bundle by default (stable tar metadata + deterministic replay manifest timestamp).
- Sign bundle artifact sidecar with provenance key.

### ELITE-26 Eval Trend Gate
- Add `scripts/run-eval-trend-gate.py`.
- Compare baseline/current eval JSON and gate on:
  - pass@1 drop
  - mean task latency increase
  - cost increase
- Wire into `scripts/run-elite-sync-gate.py`.

### ELITE-27 Memory Fusion Confidence
- Add confidence floor env: `HERMES_MEMORY_FUSION_MIN_CONFIDENCE`.
- Filter fused memory candidates below threshold.
- Add optional structured trace:
  - `HERMES_MEMORY_FUSION_TRACE=1`
  - output: `$HERMES_HOME/logs/memory-fusion-trace.jsonl`

### ELITE-28 Operator Control Plane
- Add slash command `/ops` with unified status + controls.
- Include delegated controls:
  - model selection
  - personality selection
  - mouse, yolo, reasoning, verbose, statusbar toggles

## Validation Gates

### Rust
- `cargo test -p hermes-cli cli_parse_route_health_show cli_parse_incident_pack doctor_elite_diagnostics_payload_has_required_sections route_health_tier_marks_failure_streak_critical -- --nocapture`
- `cargo test -p hermes-tools strict_sandbox_profile_blocks_remote_command_channels policy_from_env_uses_preset_defaults -- --nocapture`
- `cargo test -p hermes-agent test_fusion_min_confidence_gate_filters_low_confidence -- --nocapture`

### Python / Shell
- `python3 -m py_compile scripts/run-differential-parity-gate.py scripts/run-eval-trend-gate.py scripts/run-slo-auto-rollback.py scripts/run-elite-sync-gate.py`
- `bash -n scripts/sync-upstream.sh`

### Gate Smoke
- `python3 scripts/run-differential-parity-gate.py --repo-root . --json`
- `python3 scripts/run-eval-trend-gate.py --repo-root . --allow-missing-baseline --json`
- `python3 scripts/run-elite-sync-gate.py --repo-root . --json`
