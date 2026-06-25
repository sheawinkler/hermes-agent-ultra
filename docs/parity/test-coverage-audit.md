# Test Coverage Audit

Generated: `2026-06-25T08:37:52.001151+00:00`

## Gate

- Audit gate: **PASS**
- Critical gaps: `0`
- Advisory gaps: `4`

## Summary

| Metric | Value |
| --- | ---: |
| `tracked_behavior_rows` | 419 |
| `covered_behavior_rows` | 419 |
| `tracked_behavior_coverage_ratio` | 1.0 |
| `rust_test_files` | 315 |
| `rust_test_functions` | 3749 |
| `coverage_manifest_entries` | 409 |
| `coverage_manifest_entries_with_valid_rust_tests` | 409 |
| `missing_rust_test_refs` | 0 |
| `queue_pending` | 207 |
| `queue_total` | 6997 |
| `test_intents_total` | 10 |
| `test_intents_mapped` | 10 |

## Coverage Manifests

| Manifest | Entries | Valid Rust-test entries | Referenced Rust tests | Missing refs |
| --- | ---: | ---: | ---: | ---: |
| `docs/parity/python-test-suite-coverage.json` | 192 | 192 | 91 | 0 |
| `docs/parity/hermes-cli-test-coverage.json` | 116 | 116 | 69 | 0 |
| `docs/parity/ui-tui-source-coverage.json` | 101 | 101 | 55 | 0 |

## Test Intent Domains

| Intent | Classification | Evidence files | Direct test evidence |
| --- | --- | ---: | ---: |
| `gateway-platform-behavior` | `direct_rust_test` | 25 | 22 |
| `tool-runtime-behavior` | `direct_rust_test` | 91 | 82 |
| `cli-command-surface` | `direct_rust_test` | 42 | 25 |
| `agent-loop-and-runtime` | `direct_rust_test` | 52 | 47 |
| `acp-protocol-and-transport` | `direct_rust_test` | 9 | 8 |
| `skills-management-contract` | `direct_rust_test` | 10 | 9 |
| `cron-and-scheduler-runtime` | `direct_rust_test` | 12 | 9 |
| `memory-plugin-integration` | `direct_rust_test` | 12 | 11 |
| `environment-lifecycle-contract` | `direct_rust_test` | 12 | 8 |
| `tool-call-parser-contract` | `direct_rust_test` | 3 | 3 |

## Critical Gaps

- none

## Advisory Gaps

- `upstream_queue_pending`: upstream missing queue has pending rows; queue closure is enforced by global parity thresholds
- `nonzero_tree_drift`: max_commits_behind remains nonzero in parity matrix
- `nonzero_tree_drift`: max_upstream_patch_missing remains nonzero in parity matrix
- `nonzero_tree_drift`: max_files_only_upstream remains nonzero in parity matrix

## Completed Sigma Harness Moves

- **Coverage trend ledger**: `docs/parity/harness-trend-ledger.json`, `docs/parity/harness-trend-ledger.md`
- **ContextLattice replay evidence index**: `docs/parity/contextlattice-replay-evidence-index.json`, `docs/parity/contextlattice-replay-evidence-index.md`
- **Cross-version harness budget**: `docs/parity/harness-budget.json`, `docs/parity/harness-budget.md`

## Next Sigma Harness Moves

- none
