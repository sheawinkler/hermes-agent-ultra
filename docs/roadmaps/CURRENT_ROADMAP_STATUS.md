# Current Roadmap Status

Last reviewed: 2026-07-03

This file is the human roadmap index. The machine-readable source of truth
remains the generated parity artifacts under `docs/parity/`.

## Current Baseline

| Signal | Current state | Evidence |
| --- | ---: | --- |
| Latest release baseline | `v0.21.3` | `docs/releases/v0.21.3.md` |
| Upstream missing queue pending | `0` | `docs/parity/upstream-missing-queue.json` |
| Shared diff pending classification/review | `0 / 0` | `docs/parity/shared-diff-backlog.json` |
| Coverage critical gaps | `0` | `docs/parity/test-coverage-audit.json` |
| SOTA harness critical gaps | `0` | `docs/parity/sota-harness-matrix.json` |
| Released artifact smoke | scripted | `scripts/smoke-release-artifact.sh` |
| Public readiness summary | scripted | `scripts/generate-release-readiness-summary.py` |

The tracked parity backlog is closed at this baseline. That means the current
local governance artifacts have no pending tracked rows; it does not mean
future upstream commits cannot create new drift.

## Closed Workstreams

| Area | Status | Evidence |
| --- | --- | --- |
| WS2-WS8 parity workstreams | Closed | `docs/parity/workstream-status.json` |
| GPAR-01..04 scoped release proof | Closed | `docs/parity/gpar-01-04-proof.json` |
| Global parity governance tickets #19-#28 | Release-gated by local artifacts | `docs/parity/global-parity-proof.json` |
| ELITE-07..28 closure | Closed | `docs/parity/ELITE_07_20_CLOSURE_PROOF.md`, `docs/parity/ELITE_21_28_CLOSURE_PROOF.md` |
| ALPHA-001..061 | Closed by GitHub issue state and local alpha plans | `docs/alpha/HERMES_ULTRA_ALPHA_61_PLAN.md` |
| One-true-harness issue #702 | Closed and runtime-backed | `/harness`, `harness_cockpit`, dashboard OIDC, SOTA harness fixtures |

## Active Governance

| Area | Required state | Local gate |
| --- | --- | --- |
| Release readiness summary | PASS | `python3 scripts/generate-release-readiness-summary.py --repo-root . --check` |
| Released binary smoke | PASS for current tag | `bash scripts/smoke-release-artifact.sh --version v0.21.3` |
| Upstream missing queue | `pending = 0` before release | `python3 scripts/generate-upstream-patch-queue.py --max-commits 0` |
| Global parity proof | Pass release gate locally | `python3 scripts/generate-global-parity-proof.py --check-release` |
| SOTA harness coverage | All domains covered | `python3 scripts/generate-sota-harness-hardening.py --check` |
| Behavioral similarity diff | Outcome similarity `1.0`, no regressions or unverified cases | `python3 scripts/generate-behavioral-similarity-diff.py --check` |
| Deep problem-solving diff | Ratio `1.0`, no regressions, gaps, or unverified cases | `python3 scripts/generate-deep-problem-solving-diff.py --check` |
| Runtime placeholder discipline | Clean | `scripts/check-runtime-placeholders.sh` |
| Rust workspace health | Build/test locally | `cargo build --workspace`, `cargo test --workspace` |
| Cargo build surface | Track compile invalidation roots before crate splits | `scripts/audit-cargo-build-surface.sh` |

## Current Implementation Direction

- Preserve Rust-only runtime surfaces. Upstream Python, Electron, and desktop
  rows are either ported into Rust behavior or explicitly superseded with
  evidence in `docs/parity/queue-overrides.json`.
- Prefer ContextLattice as the memory backbone for handoff evidence. The replay
  evidence index lives at `docs/parity/contextlattice-replay-evidence-index.json`.
- Keep harness growth bounded and reviewable with `docs/parity/harness-budget.json`.
- Use released-artifact smoke evidence, not local-source assumptions, before
  claiming installer or distribution behavior.
- Keep public issue/status truth synchronized with release artifacts. Issue
  #585 is resolved by the `v0.21.3` zero-backlog baseline and should be closed
  after this post-release confidence PR lands.
- Treat stale roadmap prose as non-authoritative when it conflicts with
  generated parity artifacts, release workflow evidence, or closed GitHub issue
  state.

## Next Work

The next implementation item should be created only when one of these becomes
nonzero or failing:

- release-readiness summary failure
- released-artifact smoke failure
- upstream queue pending row
- shared-diff pending classification/review row
- failed SOTA harness budget or missing Rust test ref
- failed Rust workspace check
