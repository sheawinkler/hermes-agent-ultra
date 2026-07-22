# Parity Dashboard

_Generated from source artifacts: `2026-07-19T00:53:42.769301+00:00`_

## Snapshot

- Upstream target: `upstream/main` @ `f64e4f4f5768c18a53f44890747653bafcab2796`
- Workstream snapshot generated: `2026-07-07T16:03:18-06:00`
- Parity matrix generated: `2026-07-08T09:23:39.924163+00:00`
- Queue snapshot generated: `2026-07-17T09:13:41.339610+00:00`
- Global gate report snapshot generated: `2026-07-19T00:53:42.769301+00:00`

## Gate Status

- Release gate: **FAIL**
- CI/tree-drift gate: **FAIL**
- Test coverage audit: **PASS**
- Fixture-reference regression matrix: **PASS** (non-authoritative for capability promotion)
- Behavioral similarity diff: **FAIL**
- Deep problem-solving diff: **PASS**
- Release gate failures: max_behavioral_missing_rust_refs (actual=24.0, limit=0); max_queue_pending_commits (actual=1067.0, limit=0); required_workstreams_complete (actual={'GPAR-01': True, 'GPAR-02': True, 'GPAR-03': True, 'GPAR-04': True, 'GPAR-05': True, 'GPAR-06': True, 'GPAR-07': False, 'GPAR-08': True, 'GPAR-09': True, 'GPAR-10': False, 'GPAR-11': True}, limit=all true); source_provenance_and_required_metrics (actual=['source ledger is not valid', 'source ledger reports source or classification errors', 'source ledger fork fetch timestamp is stale or in the future', 'source ledger upstream fetch timestamp is stale or in the future', 'source ledger local_source is missing or malformed', 'current local-source record does not match the canonical source ledger', 'current local tree-state digest does not match the source ledger', 'patch queue truth status is not PASS', 'parity matrix local SHA is stale relative to the canonical source ledger', 'parity matrix upstream SHA is stale relative to the canonical source ledger'], limit=no source/provenance/metric errors)
- CI gate failures: max_files_only_upstream (actual=3439.0, limit=2600); max_behavioral_missing_rust_refs (actual=24.0, limit=0); max_queue_pending_commits (actual=1067.0, limit=100); source_provenance_and_required_metrics (actual=['source ledger is not valid', 'source ledger reports source or classification errors', 'source ledger fork fetch timestamp is stale or in the future', 'source ledger upstream fetch timestamp is stale or in the future', 'source ledger local_source is missing or malformed', 'current local-source record does not match the canonical source ledger', 'current local tree-state digest does not match the source ledger', 'patch queue truth status is not PASS', 'parity matrix local SHA is stale relative to the canonical source ledger', 'parity matrix upstream SHA is stale relative to the canonical source ledger'], limit=no source/provenance/metric errors)
- CI gate warnings: none

## Test Coverage Audit

| Metric | Value |
| --- | ---: |
| Tracked behavior rows | 477 |
| Covered behavior rows | 477 |
| Tracked behavior coverage ratio | 1.0000 |
| Rust test functions | 7412 |
| Missing Rust test refs | 0 |
| Critical gaps | 0 |

## Fixture-Reference Regression Hygiene (Non-Authoritative)

- Coverage scope: `fixture_reference_regression`
- Capability-promotion authority: `false`
- Production complete: `false`
- Registry/fixture entries and their referenced Rust tests preserve regression coverage only. They do not establish an implemented product capability or executable protocol, client, or chaos evidence.

| Fixture-reference hygiene metric | Value |
| --- | ---: |
| Declared fixture categories | 3 |
| Fixture categories passing | 3 |
| Fixture-category coverage ratio | 1.0000 |
| Referenced Rust tests | 12 |
| Unresolved fixture-reference gaps | 0 |
| Missing referenced Rust test refs | 0 |

## Behavioral Similarity Diff

| Metric | Value |
| --- | ---: |
| Total cases | 13 |
| Equal or better cases | 13 |
| Superior cases | 7 |
| Similarity ratio | 1.0000 |
| Regressions | 0 |
| Gaps | 0 |
| Unverified cases | 0 |
| Missing Rust test refs | 24 |

## Deep Problem-Solving Diff

| Metric | Value |
| --- | ---: |
| Total cases | 10 |
| Equal or better cases | 10 |
| Superior cases | 10 |
| Deep problem-solving ratio | 1.0000 |
| Regressions | 0 |
| Gaps | 0 |
| Unverified cases | 0 |
| Missing Rust test refs | 0 |

## Queue Summary

| Metric | Value |
| --- | ---: |
| Total commits in queue | 1067 |
| Pending | 1067 |
| Ported | 0 |
| Superseded | 0 |

## Tree/Patch Drift

| Metric | Value |
| --- | ---: |
| commits_behind | 1 |
| commits_ahead | 1337 |
| upstream_patch_missing | 1 |
| upstream_patch_represented | 0 |
| local_patch_unique | 1071 |
| files_only_upstream | 3439 |
| files_only_local | 1336 |
| files_shared_identical | 1458 |
| files_shared_different | 1280 |

## Workstream States

| State | Count |
| --- | ---: |
| complete | 6 |
| in_progress | 1 |

## Workstream Detail

| WS | Title | State |
| --- | --- | --- |
| WS2 | Core runtime parity | in_progress |
| WS3 | Tools/adapters parity | complete |
| WS4 | Skills parity | complete |
| WS5 | UX parity | complete |
| WS6 | Tests and CI parity | complete |
| WS7 | Security/secrets/store/webhook parity | complete |
| WS8 | Compatibility and divergence policy | complete |

## Source Artifacts

- `docs/parity/parity-matrix.json`
- `docs/parity/workstream-status.json`
- `docs/parity/upstream-missing-queue.json`
- `docs/parity/global-parity-proof.json`
- `docs/parity/test-coverage-audit.json`
- `docs/parity/sota-harness-matrix.json`
- `docs/parity/behavioral-similarity-diff.json`
- `docs/parity/deep-problem-solving-diff.json`

