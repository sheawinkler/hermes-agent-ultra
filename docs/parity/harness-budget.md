# Fixture-Reference Harness Hygiene Budget

Generated: `2026-07-19T00:53:42.937365+00:00`

## Authority and Scope

- Coverage scope: `fixture_reference_regression`
- Capability-promotion authority: `false`
- Production complete: `false`
- Registry/fixture entries and their referenced Rust tests preserve regression coverage only. They do not establish an implemented product capability.

Budget gate: **FAIL**

| Budget | Actual | Operator | Limit | Status |
| --- | ---: | --- | ---: | --- |
| `queue_pending` | 1067 | `<=` | 0 | FAIL |
| `tracked_behavior_coverage_ratio` | 1.0 | `>=` | 1.0 | PASS |
| `fixture_reference_category_coverage_ratio` | 1.0 | `>=` | 1.0 | PASS |
| `rust_test_functions` | 7412 | `<=` | 8000 | PASS |
| `coverage_manifest_entries` | 467 | `<=` | 600 | PASS |
| `fixture_reference_workflow_replay_steps` | 9 | `<=` | 12 | PASS |
| `fixture_reference_protocol_cases` | 7 | `<=` | 20 | PASS |
| `fixture_reference_fault_scenarios` | 6 | `<=` | 16 | PASS |
