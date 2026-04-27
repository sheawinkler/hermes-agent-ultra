# Ultra Elite Phase 2 Plan (ELITE-07 .. ELITE-20)

## Objective
Complete ELITE-07 through ELITE-20 with production-grade implementation, deterministic gates, and operator-ready diagnostics.

## Workstream Breakdown

1. ELITE-07 / ELITE-08 / ELITE-09 (Route learning state lifecycle)
- Persist smart-route learning state under `$HERMES_HOME/logs/route-learning.json`.
- Apply deterministic TTL + half-life decay controls:
  - `HERMES_SMART_ROUTING_LEARNING_TTL_SECS`
  - `HERMES_SMART_ROUTING_LEARNING_HALF_LIFE_SECS`
- Add operator CLI for inspect/reset:
  - `hermes route-learning`
  - `hermes route-learning --json`
  - `hermes route-learning reset`

2. ELITE-10 / ELITE-11 (Provenance key lifecycle + strict verification)
- Add key rotation command:
  - `hermes rotate-provenance-key`
- Add strict + machine-readable verify mode:
  - `hermes verify-provenance <artifact> --strict --json`
- Emit deterministic verification `code` values:
  - `ok`, `artifact_read_error`, `signature_read_error`, `signature_parse_error`,
    `key_unavailable`, `artifact_sha256_mismatch`, `signature_mismatch`.

3. ELITE-12 / ELITE-13 / ELITE-14 (Unified gate + chaos regression + severity)
- Add consolidated gate runner:
  - `scripts/run-elite-sync-gate.py`
- Add chaos report comparator:
  - `scripts/compare-adapter-chaos-reports.py`
- Extend chaos harness reporting for per-scenario metrics.
- Add severity classes + threshold gate to red-team runner:
  - `scripts/run-redteam-gate.py --max-severity-allowed <none|info|low|medium|high|critical>`

4. ELITE-15 / ELITE-16 / ELITE-17 (Policy presets + counters + fast-path)
- Add tool policy presets:
  - `HERMES_TOOL_POLICY_PRESET=strict|balanced|dev`
- Preserve override layering:
  - preset defaults -> policy file -> explicit env overrides
- Add runtime policy counters persistence:
  - `$HERMES_HOME/logs/tool-policy-counters.json`
- Optimize registry JSON wrapping fast-path + benchmark guardrail.

5. ELITE-18 / ELITE-19 / ELITE-20 (Doctor/status + one-shot gate + runbook/proof)
- Extend `doctor` snapshot with elite diagnostics block:
  - provenance key status
  - route-learning status
  - tool policy mode/preset/counters
  - elite gate script availability
- Add one-shot command:
  - `hermes elite-check [--json] [--strict]`
- Ship closure runbook + proof artifact map.

## Validation Gates
- Rust:
  - `cargo test -p hermes-agent route_learning -- --nocapture`
  - `cargo test -p hermes-cli cli_parse_ -- --nocapture`
  - `cargo test -p hermes-cli provenance_ -- --nocapture`
  - `cargo test -p hermes-tools tool_policy_hot_path_benchmark_report -- --nocapture`
  - `cargo test -p hermes-tools policy_counters_track_dispatch_outcomes -- --nocapture`
- Python:
  - `python3 -m py_compile scripts/run-redteam-gate.py scripts/run-adapter-chaos-harness.py scripts/run-elite-sync-gate.py scripts/compare-adapter-chaos-reports.py scripts/upstream_webhook_sync.py`

## Operational Commands
- Consolidated local gate:
  - `python3 scripts/run-elite-sync-gate.py --repo-root .`
- Sync pipeline with elite gate:
  - `bash scripts/sync-upstream.sh --elite-gate`
- Webhook worker with elite gate:
  - `python3 scripts/upstream_webhook_sync.py worker --elite-gate`
