# Parity Dashboard

_Generated from source artifacts: `2026-06-16T02:12:15.001297+00:00`_

## Snapshot

- Upstream target: `upstream/main` @ `55cb4103beba5822303c06b662635e1491ae72f5`
- Workstream snapshot generated: `2026-06-13T16:15:14-06:00`
- Parity matrix generated: `2026-06-12T11:29:13.798398+00:00`
- Queue snapshot generated: `2026-06-16T02:12:02.309717+00:00`
- Proof snapshot generated: `2026-06-16T02:12:15.001297+00:00`

## Gate Status

- Release gate: **FAIL**
- CI/tree-drift gate: **FAIL**
- Test coverage audit: **PASS**
- SOTA harness matrix: **PASS**
- Release gate failures: max_queue_pending_commits (actual=121.0, limit=0)
- CI gate failures: max_commits_behind (actual=5880.0, limit=5500); max_upstream_patch_missing (actual=5657.0, limit=5000); max_queue_pending_commits (actual=121.0, limit=100)

## Test Coverage Audit

| Metric | Value |
| --- | ---: |
| Tracked behavior rows | 415 |
| Covered behavior rows | 415 |
| Tracked behavior coverage ratio | 1.0000 |
| Rust test functions | 3465 |
| Missing Rust test refs | 0 |
| Critical gaps | 0 |

## SOTA Harness Matrix

| Metric | Value |
| --- | ---: |
| Domains total | 3 |
| Domains passing | 3 |
| Domain coverage ratio | 1.0000 |
| Direct Rust tests | 12 |
| Critical gaps | 0 |
| Missing Rust test refs | 0 |

## Queue Summary

| Metric | Value |
| --- | ---: |
| Total commits in queue | 5956 |
| Pending | 121 |
| Ported | 266 |
| Superseded | 5499 |

## Tree/Patch Drift

| Metric | Value |
| --- | ---: |
| commits_behind | 5880 |
| commits_ahead | 1102 |
| upstream_patch_missing | 5657 |
| upstream_patch_represented | 2 |
| local_patch_unique | 895 |
| files_only_upstream | 2309 |
| files_only_local | 730 |
| files_shared_identical | 1623 |
| files_shared_different | 1108 |

## Workstream States

| State | Count |
| --- | ---: |
| complete | 7 |

## Workstream Detail

| WS | Title | State |
| --- | --- | --- |
| WS2 | Core runtime parity | complete |
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

