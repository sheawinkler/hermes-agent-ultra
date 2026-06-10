# Test Coverage Audit

Generated: `2026-06-10T08:53:14.773456+00:00`

## Gate

- Audit gate: **PASS**
- Critical gaps: `0`
- Advisory gaps: `3`

## Summary

| Metric | Value |
| --- | ---: |
| `tracked_behavior_rows` | 384 |
| `covered_behavior_rows` | 384 |
| `tracked_behavior_coverage_ratio` | 1.0 |
| `rust_test_files` | 302 |
| `rust_test_functions` | 3409 |
| `coverage_manifest_entries` | 374 |
| `coverage_manifest_entries_with_valid_rust_tests` | 374 |
| `missing_rust_test_refs` | 0 |
| `queue_pending` | 0 |
| `queue_total` | 5375 |
| `test_intents_total` | 10 |
| `test_intents_mapped` | 10 |

## Coverage Manifests

| Manifest | Entries | Valid Rust-test entries | Referenced Rust tests | Missing refs |
| --- | ---: | ---: | ---: | ---: |
| `docs/parity/python-test-suite-coverage.json` | 174 | 174 | 91 | 0 |
| `docs/parity/hermes-cli-test-coverage.json` | 113 | 113 | 61 | 0 |
| `docs/parity/ui-tui-source-coverage.json` | 87 | 87 | 55 | 0 |

## Test Intent Domains

| Intent | Classification | Evidence files | Direct test evidence |
| --- | --- | ---: | ---: |
| `gateway-platform-behavior` | `direct_rust_test` | 25 | 22 |
| `tool-runtime-behavior` | `direct_rust_test` | 91 | 82 |
| `cli-command-surface` | `direct_rust_test` | 40 | 24 |
| `agent-loop-and-runtime` | `direct_rust_test` | 49 | 45 |
| `acp-protocol-and-transport` | `direct_rust_test` | 9 | 8 |
| `skills-management-contract` | `direct_rust_test` | 10 | 9 |
| `cron-and-scheduler-runtime` | `direct_rust_test` | 9 | 6 |
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

- **User-journey replay corpus**: Record CLI/TUI/gateway task journeys as deterministic fixtures so regressions are caught at the workflow level, not only at unit boundaries.
- **Protocol differential contracts**: Run provider, MCP, ACP, and gateway request/response fixtures through normalized differential checks against upstream intent and Ultra policy.
- **Fault-injection matrix**: Inject network stalls, partial streams, auth expiry, tool failures, and malformed model/tool payloads into the Rust runtime to prove recovery behavior.
- **PTY and TUI golden snapshots**: Exercise real terminal input/output flows and snapshot critical UX states so command ergonomics and status rendering are protected.
- **Coverage trend ledger**: Track behavior coverage over time so new upstream rows or local harness regressions create visible deltas before release prep.
