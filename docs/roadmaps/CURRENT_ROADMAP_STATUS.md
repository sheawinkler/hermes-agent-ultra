# Current Roadmap Status

Last reviewed: 2026-06-22

This file is the human roadmap index. The machine-readable source of truth
remains the generated parity artifacts under `docs/parity/`.

## Closed Workstreams

| Area | Status | Evidence |
| --- | --- | --- |
| WS2-WS8 parity workstreams | Closed | `docs/parity/workstream-status.json` |
| GPAR-01..04 scoped release proof | Closed | `docs/parity/gpar-01-04-proof.json` |
| Global parity governance tickets #19-#28 | Release-gated by local artifacts | `docs/parity/global-parity-proof.json` |
| ELITE-07..28 closure | Closed | `docs/parity/ELITE_07_20_CLOSURE_PROOF.md`, `docs/parity/ELITE_21_28_CLOSURE_PROOF.md` |
| ALPHA-001..061 | Closed by GitHub issue state and local alpha plans | `docs/alpha/HERMES_ULTRA_ALPHA_61_PLAN.md` |

## Active Governance

| Area | Status | Local gate |
| --- | --- | --- |
| Upstream missing queue | Must stay at `pending = 0` before release | `python3 scripts/generate-upstream-patch-queue.py --max-commits 0` |
| Global parity proof | Must pass release gate locally | `python3 scripts/generate-global-parity-proof.py --check-release` |
| SOTA harness coverage | Must keep all domains covered | `python3 scripts/generate-sota-harness-hardening.py --check` |
| Behavioral similarity diff | Must keep outcome similarity at 1.0 with no regressions or unverified cases | `python3 scripts/generate-behavioral-similarity-diff.py --check` |
| Deep problem-solving diff | Must keep deep problem-solving ratio at 1.0 with no regressions, gaps, or unverified cases | `python3 scripts/generate-deep-problem-solving-diff.py --check` |
| Runtime placeholder discipline | Must stay clean | `scripts/check-runtime-placeholders.sh` |
| Rust workspace health | Must build and test locally | `cargo build --workspace`, `cargo test --workspace` |
| Cargo build surface | Track compile invalidation roots before crate splits | `scripts/audit-cargo-build-surface.sh` |

## Current Implementation Direction

- Preserve Rust-only runtime surfaces. Upstream Python, Electron, and desktop
  rows are either ported into Rust behavior or explicitly superseded with
  evidence in `docs/parity/queue-overrides.json`.
- Prefer ContextLattice as the memory backbone for handoff evidence. The
  replay evidence index lives at
  `docs/parity/contextlattice-replay-evidence-index.json`.
- Keep harness growth bounded and reviewable with
  `docs/parity/harness-budget.json`.
- Reduce Rust build latency by splitting provider/auth routing, app runtime,
  CLI UI, and broad gateway adapter feature surfaces according to
  `docs/roadmaps/CARGO_BUILD_SURFACE_CRATIFICATION_2026-06-22.md`.
- Treat stale roadmap prose as non-authoritative when it conflicts with
  generated parity artifacts or closed GitHub issue state.

## Next Work

The next work item is whatever creates a nonzero pending queue row, failed
release gate, failed harness budget, or failed Rust workspace check after the
local regeneration commands run.
