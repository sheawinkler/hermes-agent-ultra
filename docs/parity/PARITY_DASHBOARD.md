# Parity Dashboard

_Generated from source artifacts: `2026-05-01T04:50:41.704227+00:00`_

## Snapshot

- Upstream target: `upstream/main` @ `ec1443b9f106bf0c4e83669d9abea8ecf934fb3d`
- Workstream snapshot generated: `2026-04-30T22:34:30-06:00`
- Parity matrix generated: `2026-05-01T04:50:37.109301+00:00`
- Queue snapshot generated: `2026-05-01T04:50:41.634533+00:00`
- Proof snapshot generated: `2026-05-01T04:50:41.704227+00:00`

## Gate Status

- Release gate: **FAIL**
- CI/tree-drift gate: **FAIL**
- Release gate failures: max_queue_pending_commits (actual=195.0, limit=0); required_workstreams_complete (actual={'GPAR-01': True, 'GPAR-02': True, 'GPAR-03': True, 'GPAR-04': True, 'GPAR-05': True, 'GPAR-06': False, 'GPAR-07': True, 'GPAR-08': True, 'GPAR-09': True}, limit=all true)
- CI gate failures: max_files_only_upstream (actual=2850.0, limit=2600); max_queue_pending_commits (actual=195.0, limit=100)

## Queue Summary

| Metric | Value |
| --- | ---: |
| Total commits in queue | 1309 |
| Pending | 195 |
| Ported | 49 |
| Superseded | 1065 |

## Tree/Patch Drift

| Metric | Value |
| --- | ---: |
| commits_behind | 1368 |
| commits_ahead | 469 |
| upstream_patch_missing | 1309 |
| upstream_patch_represented | 0 |
| local_patch_unique | 428 |
| files_only_upstream | 2850 |
| files_only_local | 458 |
| files_shared_identical | 38 |
| files_shared_different | 10 |

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

