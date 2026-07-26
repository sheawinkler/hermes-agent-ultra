# Test Coverage Audit

Generated: `2026-07-26T11:08:36.943260+00:00`

## Gate

- Audit gate: **PASS**
- Critical gaps: `0`
- Advisory gaps: `4`

## Summary

| Metric | Value |
| --- | ---: |
| `tracked_behavior_rows` | 477 |
| `covered_behavior_rows` | 477 |
| `tracked_behavior_coverage_ratio` | 1.0 |
| `rust_test_files` | 470 |
| `rust_test_functions` | 8996 |
| `coverage_manifest_entries` | 467 |
| `coverage_manifest_entries_with_valid_rust_tests` | 467 |
| `missing_rust_test_refs` | 0 |
| `queue_pending` | 2501 |
| `queue_total` | 3113 |
| `test_intents_total` | 10 |
| `test_intents_mapped` | 10 |

## Coverage Manifests

| Manifest | Entries | Valid Rust-test entries | Referenced Rust tests | Missing refs |
| --- | ---: | ---: | ---: | ---: |
| `docs/parity/python-test-suite-coverage.json` | 219 | 219 | 123 | 0 |
| `docs/parity/hermes-cli-test-coverage.json` | 126 | 126 | 90 | 0 |
| `docs/parity/ui-tui-source-coverage.json` | 122 | 122 | 67 | 0 |

## Test Intent Domains

| Intent | Classification | Evidence files | Direct test evidence |
| --- | --- | ---: | ---: |
| `gateway-platform-behavior` | `direct_rust_test` | 25 | 22 |
| `tool-runtime-behavior` | `direct_rust_test` | 123 | 101 |
| `cli-command-surface` | `direct_rust_test` | 181 | 54 |
| `agent-loop-and-runtime` | `direct_rust_test` | 136 | 91 |
| `acp-protocol-and-transport` | `direct_rust_test` | 21 | 15 |
| `skills-management-contract` | `direct_rust_test` | 72 | 21 |
| `cron-and-scheduler-runtime` | `direct_rust_test` | 73 | 20 |
| `memory-plugin-integration` | `direct_rust_test` | 13 | 13 |
| `environment-lifecycle-contract` | `direct_rust_test` | 15 | 10 |
| `tool-call-parser-contract` | `direct_rust_test` | 3 | 3 |

## Critical Gaps

- none

## Advisory Gaps

- `upstream_queue_pending`: upstream missing queue has pending rows; queue closure is enforced by global parity thresholds
- `nonzero_tree_drift`: max_commits_behind remains nonzero in parity matrix
- `nonzero_tree_drift`: max_upstream_patch_missing remains nonzero in parity matrix
- `nonzero_tree_drift`: max_files_only_upstream remains nonzero in parity matrix

## Completed Harness Moves

- **Coverage trend ledger**: `docs/parity/harness-trend-ledger.json`, `docs/parity/harness-trend-ledger.md`
- **ContextLattice replay evidence index**: `docs/parity/contextlattice-replay-evidence-index.json`, `docs/parity/contextlattice-replay-evidence-index.md`
- **Cross-version fixture-reference hygiene budget**: `docs/parity/harness-budget.json`, `docs/parity/harness-budget.md`

## Next Harness Moves

- none
