# Parity Matrix

Generated: `2026-05-01T04:50:37.109301+00:00`

## Scope

- Local ref: `main` (`f9c3b022de0e5188ecb275014fd074d9d6760a46`)
- Upstream ref: `upstream/main` (`ec1443b9f106bf0c4e83669d9abea8ecf934fb3d`)
- Merge base: `none (history divergence)`

## Summary

| Metric | Value |
| --- | ---: |
| Commits behind local (`upstream` ancestry only) | 1368 |
| Commits ahead local (`local` ancestry only) | 469 |
| Upstream commits missing by patch-id (`git cherry local upstream`, `+`) | 1309 |
| Upstream commits represented by patch-id (`git cherry local upstream`, `-`) | 0 |
| Local commits unique by patch-id (`git cherry upstream local`, `+`) | 428 |
| Files only in upstream tree | 2850 |
| Files only in local tree | 458 |
| Shared files identical content | 38 |
| Shared files different content | 10 |
| Total files changed (`local` vs `upstream`) | 3319 |
| Insertions (`local` vs `upstream`) | 1103493 |
| Deletions (`local` vs `upstream`) | 203013 |

## Top 40 upstream-only buckets

| Bucket | Files |
| --- | ---: |
| `website/docs` | 283 |
| `skills/creative` | 229 |
| `tests/gateway` | 212 |
| `tests/tools` | 182 |
| `tests/hermes_cli` | 167 |
| `ui-tui/src` | 158 |
| `ui-tui/packages` | 135 |
| `optional-skills/mlops` | 81 |
| `tests/agent` | 79 |
| `tests/run_agent` | 78 |
| `web/src` | 69 |
| `skills/productivity` | 68 |
| `skills/mlops` | 66 |
| `skills/research` | 63 |
| `tests/cli` | 52 |
| `gateway/platforms` | 37 |
| `plugins/memory` | 31 |
| `optional-skills/creative` | 26 |
| `website/static` | 26 |
| `tests/plugins` | 19 |
| `environments/benchmarks` | 17 |
| `plugins/google_meet` | 17 |
| `skills/github` | 16 |
| `optional-skills/research` | 14 |
| `optional-skills/security` | 13 |
| `skills/software-development` | 13 |
| `environments/tool_call_parsers` | 12 |
| `optional-skills/productivity` | 12 |
| `plugins/hermes-achievements` | 12 |
| `tools/environments` | 12 |
| `tests/acp` | 11 |
| `web/public` | 11 |
| `tests/cron` | 10 |
| `tests/stress` | 10 |
| `.github/workflows` | 9 |
| `optional-skills/health` | 9 |
| `skills/red-teaming` | 9 |
| `optional-skills/mcp` | 8 |
| `skills/media` | 8 |
| `tests/integration` | 8 |

## Top 40 shared-different buckets

| Bucket | Files |
| --- | ---: |
| `.gitignore` | 1 |
| `AGENTS.md` | 1 |
| `Dockerfile` | 1 |
| `LICENSE` | 1 |
| `README.md` | 1 |
| `docker-compose.yml` | 1 |
| `docker/entrypoint.sh` | 1 |
| `flake.nix` | 1 |
| `packaging/homebrew` | 1 |
| `scripts/install.sh` | 1 |

## Top 40 local-only buckets

| Bucket | Files |
| --- | ---: |
| `crates/hermes-tools` | 72 |
| `crates/hermes-agent` | 47 |
| `crates/hermes-gateway` | 45 |
| `crates/hermes-cli` | 40 |
| `crates/hermes-intelligence` | 36 |
| `docs/parity` | 30 |
| `crates/hermes-config` | 18 |
| `crates/hermes-eval` | 15 |
| `crates/hermes-parity-tests` | 15 |
| `crates/hermes-environments` | 13 |
| `crates/hermes-core` | 11 |
| `crates/hermes-cron` | 9 |
| `crates/hermes-acp` | 8 |
| `crates/hermes-mcp` | 7 |
| `crates/hermes-skills` | 7 |
| `optional-skills/creative` | 6 |
| `crates/hermes-http` | 5 |
| `docs/roadmaps` | 5 |
| `.github/workflows` | 3 |
| `crates/hermes-auth` | 3 |
| `crates/hermes-telemetry` | 3 |
| `crates/hermes-rl` | 2 |
| `docs/releases` | 2 |
| `.ci/clippy-allowlist.txt` | 1 |
| `Cargo.lock` | 1 |
| `Cargo.toml` | 1 |
| `NOTICE` | 1 |
| `PARITY_PLAN.md` | 1 |
| `README_JA.md` | 1 |
| `README_KO.md` | 1 |
| `README_QUICKSTART.md` | 1 |
| `README_ZH.md` | 1 |
| `UPSTREAM_ATTRIBUTION.md` | 1 |
| `docs/installer-troubleshooting.md` | 1 |
| `docs/upstream-sync.md` | 1 |
| `docs/upstream-webhook-sync.md` | 1 |
| `scripts/check-runtime-placeholders.sh` | 1 |
| `scripts/clippy-warning-gate.sh` | 1 |
| `scripts/compare-adapter-chaos-reports.py` | 1 |
| `scripts/cron-upstream-sync.sh` | 1 |

## Workstream Routing

| Workstream | Issue | Name | Upstream-only | Shared-different | Risk | Effort |
| --- | ---: | --- | ---: | ---: | --- | --- |
| `WS6` | #10 | Tests and CI parity | 919 | 0 | high | XL |
| `WS5` | #9 | UX parity | 716 | 0 | high | XL |
| `WS4` | #8 | Skills parity | 699 | 0 | high | XL |
| `WS8` | #12 | Compatibility and divergence policy | 232 | 10 | medium | L |
| `WS3` | #7 | Tools and adapters parity | 228 | 0 | high | L |
| `WS2` | #6 | Core runtime parity | 56 | 0 | critical | M |
| `WS7` | #11 | Security/secrets/store/webhook parity | 0 | 0 | critical | S |

## Commit Mapping

- Upstream missing by patch-id: `1309`
- Upstream represented by patch-id: `0`
- Local unique by patch-id: `428`
- Intentional divergence tracked items: `5` (covered files: `1430`)
- Merge base is absent; patch-id mapping is used as primary commit equivalence signal.

## Intentional Divergence Registry

| ID | Status | Workstream | Matched Files | Summary |
| --- | --- | --- | ---: | --- |
| `ultra-contextlattice-memory-plugin` | approved | WS3 | 2 | Keep ContextLattice native memory plugin and provider discovery in the Rust agent runtime. |
| `ultra-webhook-queue-backends` | approved | WS7 | 4 | Preserve webhook-driven sync queue worker architecture with sqlite, SQS, and Kafka support. |
| `ultra-launchd-webhook-lifecycle` | approved | WS7 | 4 | Preserve launchd-based interactive dev lifecycle management for webhook listener and worker. |
| `rust-skills-catalog-governance` | approved | WS4 | 705 | Track upstream skills and optional-skills catalogs via parity audits while keeping Rust runtime skill loading externalized (no direct Python skill-tree vendoring). |
| `rust-cli-tui-primary-ux-surface` | approved | WS5 | 715 | Treat Rust CLI/TUI and gateway as primary UX surface; upstream web/ui-tui trees are tracked as intentional divergence unless explicitly ported. |


## Notes

- Data is computed directly from git refs in this repository.
- Tree-level parity classification does not require a merge-base.
- Commit representation/missing uses patch-id equivalence from `git cherry`.
- Default behavior fetches latest upstream refs (`git fetch upstream --prune`).
