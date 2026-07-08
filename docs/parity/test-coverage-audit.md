# Test Coverage Audit

Generated: `2026-07-08T09:23:41.617774+00:00`

## Gate

- Audit gate: **PASS**
- Critical gaps: `0`
- Advisory gaps: `3`

## Summary

| Metric | Value |
| --- | ---: |
| `tracked_behavior_rows` | 477 |
| `covered_behavior_rows` | 477 |
| `tracked_behavior_coverage_ratio` | 1.0 |
| `rust_test_files` | 423 |
| `rust_test_functions` | 7412 |
| `coverage_manifest_entries` | 467 |
| `coverage_manifest_entries_with_valid_rust_tests` | 467 |
| `missing_rust_test_refs` | 0 |
| `queue_pending` | 0 |
| `queue_total` | 51 |
| `test_intents_total` | 10 |
| `test_intents_mapped` | 10 |

## Coverage Manifests

| Manifest | Entries | Valid Rust-test entries | Referenced Rust tests | Missing refs |
| --- | ---: | ---: | ---: | ---: |
| `docs/parity/python-test-suite-coverage.json` | 219 | 219 | 123 | 0 |
| `docs/parity/hermes-cli-test-coverage.json` | 126 | 126 | 90 | 0 |
| `docs/parity/ui-tui-source-coverage.json` | 122 | 122 | 68 | 0 |

## Test Intent Domains

| Intent | Classification | Evidence files | Direct test evidence |
| --- | --- | ---: | ---: |
| `gateway-platform-behavior` | `direct_rust_test` | 25 | 22 |
| `tool-runtime-behavior` | `direct_rust_test` | 123 | 101 |
| `cli-command-surface` | `direct_rust_test` | 178 | 47 |
| `agent-loop-and-runtime` | `direct_rust_test` | 106 | 71 |
| `acp-protocol-and-transport` | `direct_rust_test` | 21 | 15 |
| `skills-management-contract` | `direct_rust_test` | 69 | 16 |
| `cron-and-scheduler-runtime` | `direct_rust_test` | 71 | 16 |
| `memory-plugin-integration` | `direct_rust_test` | 12 | 12 |
| `environment-lifecycle-contract` | `direct_rust_test` | 14 | 9 |
| `tool-call-parser-contract` | `direct_rust_test` | 3 | 3 |

## Critical Gaps

- none

## Advisory Gaps

- `nonzero_tree_drift`: max_commits_behind remains nonzero in parity matrix
- `nonzero_tree_drift`: max_upstream_patch_missing remains nonzero in parity matrix
- `nonzero_tree_drift`: max_files_only_upstream remains nonzero in parity matrix

## Completed Sigma Harness Moves

- **Coverage trend ledger**: `docs/parity/harness-trend-ledger.json`, `docs/parity/harness-trend-ledger.md`
- **ContextLattice replay evidence index**: `docs/parity/contextlattice-replay-evidence-index.json`, `docs/parity/contextlattice-replay-evidence-index.md`
- **Cross-version harness budget**: `docs/parity/harness-budget.json`, `docs/parity/harness-budget.md`

## Next Sigma Harness Moves

- none
