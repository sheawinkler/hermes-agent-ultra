# Parity Artifacts

This directory stores generated parity reports against the upstream target
(`upstream/main`).

## Regenerate

From repository root:

```bash
python3 scripts/generate-parity-matrix.py
python3 scripts/generate-workstream-status.py
```

By default this command fetches upstream directly from GitHub
(`git fetch upstream --prune`) before computing metrics.

## Outputs

- `parity-matrix.json`: machine-readable summary, commit mapping, workstream routing
- `parity-matrix.md`: human-readable report for planning/review
- `workstream-status.json`: WS2-WS8 completion status with auditable metrics
- `workstream-status.md`: human-readable WS2-WS8 status report
- `intentional-divergence.json`: tracked, approved ultra-only deltas used by the report

## What The Matrix Includes

- Tree-level path classification that works even when branch histories diverge:
  - files only in upstream
  - files only in local
  - shared files with different content
  - shared files with identical content
- Patch-equivalent commit mapping via `git cherry`:
  - upstream commits missing vs represented
  - local unique commits
- Workstream routing to parity tickets:
  - WS2-WS8 issue mapping, risk level, and effort size.
