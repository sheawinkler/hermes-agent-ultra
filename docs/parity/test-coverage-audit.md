# Test Coverage Audit

Generated: `2026-06-11T21:26:21.505744+00:00`

## Gate

- Audit gate: **PASS**
- Critical gaps: `0`
- Advisory gaps: `3`

## Summary

| Metric | Value |
| --- | ---: |
| `tracked_behavior_rows` | 415 |
| `covered_behavior_rows` | 415 |
| `tracked_behavior_coverage_ratio` | 1.0 |
| `rust_test_files` | 308 |
| `rust_test_functions` | 3465 |
| `coverage_manifest_entries` | 405 |
| `coverage_manifest_entries_with_valid_rust_tests` | 405 |
| `missing_rust_test_refs` | 0 |
| `queue_pending` | 0 |
| `queue_total` | 5593 |
| `test_intents_total` | 10 |
| `test_intents_mapped` | 10 |

## Coverage Manifests

| Manifest | Entries | Valid Rust-test entries | Referenced Rust tests | Missing refs |
| --- | ---: | ---: | ---: | ---: |
| `docs/parity/python-test-suite-coverage.json` | 192 | 192 | 91 | 0 |
| `docs/parity/hermes-cli-test-coverage.json` | 114 | 114 | 65 | 0 |
| `docs/parity/ui-tui-source-coverage.json` | 99 | 99 | 55 | 0 |

## Test Intent Domains

| Intent | Classification | Evidence files | Direct test evidence |
| --- | --- | ---: | ---: |
| `gateway-platform-behavior` | `direct_rust_test` | 25 | 22 |
| `tool-runtime-behavior` | `direct_rust_test` | 91 | 82 |
| `cli-command-surface` | `direct_rust_test` | 40 | 25 |
| `agent-loop-and-runtime` | `direct_rust_test` | 50 | 46 |
| `acp-protocol-and-transport` | `direct_rust_test` | 9 | 8 |
| `skills-management-contract` | `direct_rust_test` | 10 | 9 |
| `cron-and-scheduler-runtime` | `direct_rust_test` | 10 | 7 |
| `memory-plugin-integration` | `direct_rust_test` | 12 | 11 |
| `environment-lifecycle-contract` | `direct_rust_test` | 12 | 8 |
| `tool-call-parser-contract` | `direct_rust_test` | 3 | 3 |

## Critical Gaps

- none

## Advisory Gaps

- `nonzero_tree_drift`: max_commits_behind remains nonzero in parity matrix
- `nonzero_tree_drift`: max_upstream_patch_missing remains nonzero in parity matrix
- `nonzero_tree_drift`: max_files_only_upstream remains nonzero in parity matrix

## Next Sigma Harness Moves

- **Coverage trend ledger**: Track behavior coverage over time so new upstream rows or local harness regressions create visible deltas before release prep.
- **ContextLattice replay evidence index**: Index passing and failing replay artifacts into ContextLattice so agents can retrieve exact harness evidence instead of rediscovering it from scratch.
- **Cross-version harness budget**: Record runtime and fixture-count budgets across releases so SOTA harness growth stays deterministic, bounded, and reviewable.
