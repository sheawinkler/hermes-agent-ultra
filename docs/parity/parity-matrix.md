# Parity Matrix

Generated: `2026-04-24T21:19:39.155201+00:00`

## Scope

- Local ref: `main` (`f6d9afdda504f29d85ba9423c63fcab0007a08af`)
- Upstream ref: `upstream/main` (`93ddff53e339b859e88d1d1be97624212722b7f1`)
- Merge base: `none (history divergence)`

## Summary

| Metric | Value |
| --- | ---: |
| Commits behind local (`upstream` ancestry only) | 298 |
| Commits ahead local (`local` ancestry only) | 302 |
| Upstream commits missing by patch-id (`git cherry local upstream`, `+`) | 286 |
| Upstream commits represented by patch-id (`git cherry local upstream`, `-`) | 0 |
| Local commits unique by patch-id (`git cherry upstream local`, `+`) | 291 |
| Files only in upstream tree | 2512 |
| Files only in local tree | 414 |
| Shared files identical content | 2 |
| Shared files different content | 8 |
| Total files changed (`local` vs `upstream`) | 2935 |
| Insertions (`local` vs `upstream`) | 951792 |
| Deletions (`local` vs `upstream`) | 161602 |

## Top 40 upstream-only buckets

| Bucket | Files |
| --- | ---: |
| `website/docs` | 262 |
| `skills/creative` | 202 |
| `tests/gateway` | 185 |
| `tests/tools` | 165 |
| `tests/hermes_cli` | 140 |
| `ui-tui/packages` | 130 |
| `ui-tui/src` | 120 |
| `optional-skills/mlops` | 81 |
| `web/src` | 72 |
| `skills/mlops` | 66 |
| `skills/productivity` | 66 |
| `skills/research` | 63 |
| `tests/run_agent` | 63 |
| `tests/agent` | 62 |
| `tests/cli` | 44 |
| `optional-skills/creative` | 34 |
| `gateway/platforms` | 32 |
| `plugins/memory` | 31 |
| `environments/benchmarks` | 17 |
| `skills/github` | 16 |
| `optional-skills/research` | 14 |
| `optional-skills/security` | 13 |
| `environments/tool_call_parsers` | 12 |
| `tests/plugins` | 12 |
| `tests/acp` | 11 |
| `tools/environments` | 11 |
| `web/public` | 11 |
| `website/static` | 11 |
| `.github/workflows` | 10 |
| `optional-skills/health` | 9 |
| `skills/red-teaming` | 9 |
| `optional-skills/mcp` | 8 |
| `optional-skills/productivity` | 8 |
| `skills/media` | 8 |
| `tests/cron` | 8 |
| `tests/integration` | 8 |
| `agent/transports` | 7 |
| `optional-skills/devops` | 6 |
| `plugins/image_gen` | 6 |
| `skills/software-development` | 6 |

## Top 40 shared-different buckets

| Bucket | Files |
| --- | ---: |
| `.gitignore` | 1 |
| `AGENTS.md` | 1 |
| `Dockerfile` | 1 |
| `LICENSE` | 1 |
| `README.md` | 1 |
| `flake.nix` | 1 |
| `packaging/homebrew` | 1 |
| `scripts/install.sh` | 1 |

## Top 40 local-only buckets

| Bucket | Files |
| --- | ---: |
| `crates/hermes-tools` | 70 |
| `crates/hermes-gateway` | 45 |
| `crates/hermes-agent` | 44 |
| `crates/hermes-cli` | 39 |
| `crates/hermes-intelligence` | 36 |
| `docs/parity` | 25 |
| `crates/hermes-config` | 18 |
| `crates/hermes-parity-tests` | 15 |
| `crates/hermes-environments` | 13 |
| `crates/hermes-core` | 11 |
| `crates/hermes-eval` | 11 |
| `crates/hermes-cron` | 9 |
| `crates/hermes-acp` | 8 |
| `crates/hermes-mcp` | 7 |
| `crates/hermes-skills` | 7 |
| `crates/hermes-http` | 5 |
| `.github/workflows` | 3 |
| `crates/hermes-auth` | 3 |
| `crates/hermes-telemetry` | 3 |
| `optional-skills/creative` | 3 |
| `crates/hermes-rl` | 2 |
| `.ci/clippy-allowlist.txt` | 1 |
| `Cargo.lock` | 1 |
| `Cargo.toml` | 1 |
| `NOTICE` | 1 |
| `PARITY_PLAN.md` | 1 |
| `README_JA.md` | 1 |
| `README_KO.md` | 1 |
| `README_ZH.md` | 1 |
| `UPSTREAM_ATTRIBUTION.md` | 1 |
| `docs/installer-troubleshooting.md` | 1 |
| `docs/roadmaps` | 1 |
| `docs/upstream-sync.md` | 1 |
| `docs/upstream-webhook-sync.md` | 1 |
| `scripts/check-runtime-placeholders.sh` | 1 |
| `scripts/clippy-warning-gate.sh` | 1 |
| `scripts/cron-upstream-sync.sh` | 1 |
| `scripts/generate-adapter-matrix.py` | 1 |
| `scripts/generate-global-parity-proof.py` | 1 |
| `scripts/generate-homebrew-formula.sh` | 1 |

## Workstream Routing

| Workstream | Issue | Name | Upstream-only | Shared-different | Risk | Effort |
| --- | ---: | --- | ---: | ---: | --- | --- |
| `WS6` | #10 | Tests and CI parity | 769 | 0 | high | XL |
| `WS4` | #8 | Skills parity | 665 | 0 | high | XL |
| `WS5` | #9 | UX parity | 637 | 0 | high | XL |
| `WS8` | #12 | Compatibility and divergence policy | 211 | 8 | medium | L |
| `WS3` | #7 | Tools and adapters parity | 180 | 0 | high | L |
| `WS2` | #6 | Core runtime parity | 50 | 0 | critical | M |
| `WS7` | #11 | Security/secrets/store/webhook parity | 0 | 0 | critical | S |

## Commit Mapping

- Upstream missing by patch-id: `286`
- Upstream represented by patch-id: `0`
- Local unique by patch-id: `291`
- Intentional divergence tracked items: `5` (covered files: `1315`)
- Merge base is absent; patch-id mapping is used as primary commit equivalence signal.

## Intentional Divergence Registry

| ID | Status | Workstream | Matched Files | Summary |
| --- | --- | --- | ---: | --- |
| `ultra-contextlattice-memory-plugin` | approved | WS3 | 2 | Keep ContextLattice native memory plugin and provider discovery in the Rust agent runtime. |
| `ultra-webhook-queue-backends` | approved | WS7 | 4 | Preserve webhook-driven sync queue worker architecture with sqlite, SQS, and Kafka support. |
| `ultra-launchd-webhook-lifecycle` | approved | WS7 | 4 | Preserve launchd-based interactive dev lifecycle management for webhook listener and worker. |
| `rust-skills-catalog-governance` | approved | WS4 | 668 | Track upstream skills and optional-skills catalogs via parity audits while keeping Rust runtime skill loading externalized (no direct Python skill-tree vendoring). |
| `rust-cli-tui-primary-ux-surface` | approved | WS5 | 637 | Treat Rust CLI/TUI and gateway as primary UX surface; upstream web/ui-tui trees are tracked as intentional divergence unless explicitly ported. |


## Notes

- Data is computed directly from git refs in this repository.
- Tree-level parity classification does not require a merge-base.
- Commit representation/missing uses patch-id equivalence from `git cherry`.
- Default behavior fetches latest upstream refs (`git fetch upstream --prune`).
