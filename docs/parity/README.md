# Parity Artifacts

This directory stores generated parity reports against the upstream target
(`upstream/main`).

## Regenerate

From repository root:

```bash
python3 scripts/generate-parity-matrix.py
python3 scripts/generate-workstream-status.py
python3 scripts/generate-test-intent-mapping.py
python3 scripts/generate-adapter-matrix.py
python3 scripts/validate-intentional-divergence.py --check --allow-warnings
python3 scripts/generate-upstream-patch-queue.py --max-commits 0
python3 scripts/generate-global-parity-proof.py --check-ci
```

By default this command fetches upstream directly from GitHub
(`git fetch upstream --prune`) before computing metrics.

## Outputs

- `parity-matrix.json`: machine-readable summary, commit mapping, workstream routing
- `parity-matrix.md`: human-readable report for planning/review
- `workstream-status.json`: WS2-WS8 completion status with auditable metrics
- `workstream-status.md`: human-readable WS2-WS8 status report
- `test-intent-mapping.json`: upstream test-intent domain coverage mapped to Rust evidence
- `test-intent-mapping.md`: human-readable intent mapping table
- `adapter-feature-matrix.json`: platform adapter + memory plugin matrix
- `adapter-feature-matrix.md`: human-readable adapter matrix
- `divergence-validation.json`: ownership/review freshness and coverage checks for intentional divergences
- `global-parity-thresholds.json`: machine-readable CI/release parity thresholds
  - `ci_thresholds`: tree-drift observability gate
  - `release_thresholds`: functional parity gate (GPAR/workstream + divergence/test integrity)
- `global-parity-proof.json`: consolidated parity proof with gate results for tickets #19-#28
- `global-parity-proof.md`: human-readable parity proof summary
- `shared-different-classification.json`: functional-vs-policy classification for shared-different files
- `upstream-missing-queue.md` / `upstream-missing-queue.json`: auditable upstream missing commit queue
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
