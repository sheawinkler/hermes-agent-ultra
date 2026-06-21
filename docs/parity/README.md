# Parity Artifacts

This directory stores generated parity reports against the upstream target
(`upstream/main`).

## Regenerate

From repository root:

```bash
python3 scripts/generate-parity-matrix.py
python3 scripts/generate-workstream-status.py
python3 scripts/generate-test-intent-mapping.py
python3 scripts/generate-test-coverage-audit.py --check
python3 scripts/generate-adapter-matrix.py
python3 scripts/validate-intentional-divergence.py --check --allow-warnings
python3 scripts/generate-upstream-patch-queue.py --max-commits 0
python3 scripts/generate-behavioral-similarity-diff.py --check
python3 scripts/generate-global-parity-proof.py --check-ci
python3 scripts/generate-gpar-01-04-proof.py
python3 scripts/generate-sota-harness-hardening.py --check
python3 scripts/generate-parity-dashboard.py
python3 scripts/run-upstream-surface-coverage-gate.py --upstream-ref upstream/main
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
- `test-coverage-audit.json`: upstream behavior/test-intent coverage audit with Rust test-reference validation
- `test-coverage-audit.md`: human-readable coverage audit, advisory gaps, and next harness moves
- `sota-harness-matrix.json`: release-gated matrix for workflow replay, protocol differential contracts, and fault injection
- `sota-harness-matrix.md`: human-readable SOTA harness domain summary
- `behavioral-similarity-cases.json`: upstream-vs-Ultra outcome comparison case manifest
- `behavioral-similarity-diff.json`: release-gated behavioral similarity and superiority proof
- `behavioral-similarity-diff.md`: human-readable behavior diff summary
- `harness-trend-ledger.json` / `harness-trend-ledger.md`: coverage and SOTA harness trend ledger
- `contextlattice-replay-evidence-index.json` / `contextlattice-replay-evidence-index.md`: ContextLattice replay evidence index for SOTA harness proofs
- `harness-budget.json` / `harness-budget.md`: cross-version review budgets for queue, coverage, and harness growth
- `adapter-feature-matrix.json`: platform adapter + memory plugin matrix
- `adapter-feature-matrix.md`: human-readable adapter matrix
- `divergence-validation.json`: ownership/review freshness and coverage checks for intentional divergences
- `global-parity-thresholds.json`: machine-readable CI/release parity thresholds
  - `ci_thresholds`: tree-drift observability gate
  - `ci_thresholds.special_rules`: scoped exemptions for non-actionable upstream-only drift
  - `release_thresholds`: functional parity gate (GPAR/workstream + divergence/test integrity + behavioral similarity)
- `global-parity-proof.json`: consolidated parity proof with gate results for tickets #19-#28
- `global-parity-proof.md`: human-readable parity proof summary
- `gpar-01-04-proof.json`: scoped proof for GPAR-01..04 queue closure parity release
- `gpar-01-04-proof.md`: human-readable scoped proof for GPAR-01..04
- `PARITY_DASHBOARD.md`: operator-facing dashboard synthesized from parity JSON artifacts
- `shared-different-classification.json`: functional-vs-policy classification for shared-different files
- `upstream-missing-queue.md` / `upstream-missing-queue.json`: auditable upstream missing commit queue
- `intentional-divergence.json`: tracked, approved ultra-only deltas used by the report
- `.sync-reports/upstream-surface-coverage-gate-*.json`: required-surface coverage proof for upstream files under `skills`, `optional-skills`, `plugins`, `tests`, `website`, `ui-tui`, `docs`; reports raw missing files, approved intentional-divergence coverage, and unclassified actionable misses

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
