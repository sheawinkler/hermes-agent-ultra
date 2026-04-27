# ELITE-07..20 Closure Proof

## Scope
- Repository: `sheawinkler/hermes-agent-ultra`
- Issues: `#92` .. `#105`
- Workstream objective: complete ELITE-07 through ELITE-20 with full implementation and reproducible validation.

## Issue-to-Implementation Map

1. ELITE-07 `#92`
- Route-learning persistence at `$HERMES_HOME/logs/route-learning.json`.
- Corruption-safe load fallback implemented.

2. ELITE-08 `#93`
- Deterministic TTL/decay controls:
  - `HERMES_SMART_ROUTING_LEARNING_TTL_SECS`
  - `HERMES_SMART_ROUTING_LEARNING_HALF_LIFE_SECS`

3. ELITE-09 `#94`
- CLI inspect/reset:
  - `hermes route-learning`
  - `hermes route-learning --json`
  - `hermes route-learning reset`

4. ELITE-10 `#95`
- `hermes rotate-provenance-key` archives prior key and rotates active key.

5. ELITE-11 `#96`
- Strict verification + deterministic reason codes:
  - `hermes verify-provenance <path> --strict --json`

6. ELITE-12 `#97`
- Unified gate script:
  - `scripts/run-elite-sync-gate.py`
- Optional sync/webhook wiring for elite gate.

7. ELITE-13 `#98`
- Chaos regression comparator:
  - `scripts/compare-adapter-chaos-reports.py`
- Chaos harness enriched with per-scenario run metrics.

8. ELITE-14 `#99`
- Red-team severity classes and threshold gate:
  - `scripts/run-redteam-gate.py --max-severity-allowed ...`

9. ELITE-15 `#100`
- Tool policy profile presets:
  - `HERMES_TOOL_POLICY_PRESET=strict|balanced|dev`
- Override layering preserved.

10. ELITE-16 `#101`
- Policy counters persisted and visible via status/doctor.

11. ELITE-17 `#102`
- Registry JSON/result fast-path optimization + benchmark guardrail.

12. ELITE-18 `#103`
- Doctor elite diagnostics block (provenance, route-learning, policy counters, elite gate availability).

13. ELITE-19 `#104`
- One-shot operator command:
  - `hermes elite-check [--json] [--strict]`

14. ELITE-20 `#105`
- Phase-2 plan + closure proof docs and reproducible command set.

## Validation Evidence (Commands)

```bash
cargo test -p hermes-agent route_learning -- --nocapture
cargo test -p hermes-cli cli_parse_ -- --nocapture
cargo test -p hermes-cli provenance_ -- --nocapture
cargo test -p hermes-tools tool_policy_hot_path_benchmark_report -- --nocapture
cargo test -p hermes-tools policy_counters_track_dispatch_outcomes -- --nocapture
python3 -m py_compile scripts/run-redteam-gate.py scripts/run-adapter-chaos-harness.py scripts/run-elite-sync-gate.py scripts/compare-adapter-chaos-reports.py scripts/upstream_webhook_sync.py
```

## Operational Acceptance

```bash
hermes route-learning --json
hermes rotate-provenance-key --json
hermes verify-provenance ~/.hermes-agent-ultra/snapshots/doctor-*.json --strict --json
hermes elite-check --strict --json
bash scripts/sync-upstream.sh --elite-gate
```
