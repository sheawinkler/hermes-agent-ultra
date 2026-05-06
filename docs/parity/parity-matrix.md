# Parity Matrix

Generated: `2026-05-06T03:50:27.949342+00:00`

## Scope

- Local ref: `main` (`bbc111a6e5ac41035e570eac179a2b95360baedf`)
- Upstream ref: `upstream/main` (`f27fcb6a82b8487174ca941c15e7a5887371eede`)
- Merge base: `none (history divergence)`

## Summary

| Metric | Value |
| --- | ---: |
| Commits behind local (`upstream` ancestry only) | 1896 |
| Commits ahead local (`local` ancestry only) | 569 |
| Upstream commits missing by patch-id (`git cherry local upstream`, `+`) | 1827 |
| Upstream commits represented by patch-id (`git cherry local upstream`, `-`) | 1 |
| Local commits unique by patch-id (`git cherry upstream local`, `+`) | 501 |
| Files only in upstream tree | 3020 |
| Files only in local tree | 470 |
| Shared files identical content | 38 |
| Shared files different content | 12 |
| Total files changed (`local` vs `upstream`) | 3503 |
| Insertions (`local` vs `upstream`) | 1161418 |
| Deletions (`local` vs `upstream`) | 221138 |

## Top 40 upstream-only buckets

| Bucket | Files |
| --- | ---: |
| `website/docs` | 293 |
| `skills/creative` | 229 |
| `tests/gateway` | 224 |
| `tests/tools` | 186 |
| `tests/hermes_cli` | 178 |
| `ui-tui/src` | 160 |
| `ui-tui/packages` | 136 |
| `tests/agent` | 86 |
| `optional-skills/mlops` | 81 |
| `tests/run_agent` | 81 |
| `web/src` | 70 |
| `skills/productivity` | 68 |
| `skills/mlops` | 66 |
| `skills/research` | 63 |
| `plugins/model-providers` | 57 |
| `tests/cli` | 52 |
| `optional-skills/creative` | 46 |
| `gateway/platforms` | 38 |
| `plugins/memory` | 31 |
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
| `tests/cron` | 12 |
| `tools/environments` | 12 |
| `tests/acp` | 11 |
| `web/public` | 11 |
| `.github/workflows` | 10 |
| `tests/stress` | 10 |
| `optional-skills/health` | 9 |
| `skills/red-teaming` | 9 |
| `optional-skills/mcp` | 8 |
| `skills/media` | 8 |

## Top 40 shared-different buckets

| Bucket | Files |
| --- | ---: |
| `optional-skills/blockchain` | 2 |
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
| `docs/parity` | 32 |
| `crates/hermes-config` | 18 |
| `crates/hermes-eval` | 15 |
| `crates/hermes-parity-tests` | 15 |
| `crates/hermes-environments` | 13 |
| `crates/hermes-core` | 11 |
| `crates/hermes-cron` | 9 |
| `crates/hermes-acp` | 8 |
| `crates/hermes-mcp` | 7 |
| `crates/hermes-skills` | 7 |
| `docs/roadmaps` | 6 |
| `optional-skills/creative` | 6 |
| `crates/hermes-http` | 5 |
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
| `docs/local-backends.md` | 1 |
| `docs/upstream-sync.md` | 1 |
| `docs/upstream-webhook-sync.md` | 1 |
| `scripts/audit_background_queue.py` | 1 |
| `scripts/check-runtime-placeholders.sh` | 1 |
| `scripts/clippy-warning-gate.sh` | 1 |

## Workstream Routing

| Workstream | Issue | Name | Upstream-only | Shared-different | Risk | Effort |
| --- | ---: | --- | ---: | ---: | --- | --- |
| `WS6` | #10 | Tests and CI parity | 970 | 0 | high | XL |
| `WS5` | #9 | UX parity | 738 | 0 | high | XL |
| `WS4` | #8 | Skills parity | 717 | 2 | high | XL |
| `WS3` | #7 | Tools and adapters parity | 286 | 0 | high | L |
| `WS8` | #12 | Compatibility and divergence policy | 251 | 10 | medium | L |
| `WS2` | #6 | Core runtime parity | 58 | 0 | critical | M |
| `WS7` | #11 | Security/secrets/store/webhook parity | 0 | 0 | critical | S |

## Commit Mapping

- Upstream missing by patch-id: `1827`
- Upstream represented by patch-id: `1`
- Local unique by patch-id: `501`
- Intentional divergence tracked items: `5` (covered files: `1471`)
- Merge base is absent; patch-id mapping is used as primary commit equivalence signal.

## Intentional Divergence Registry

| ID | Status | Workstream | Matched Files | Summary |
| --- | --- | --- | ---: | --- |
| `ultra-contextlattice-memory-plugin` | approved | WS3 | 2 | Keep ContextLattice native memory plugin and provider discovery in the Rust agent runtime. |
| `ultra-webhook-queue-backends` | approved | WS7 | 4 | Preserve webhook-driven sync queue worker architecture with sqlite, SQS, and Kafka support. |
| `ultra-launchd-webhook-lifecycle` | approved | WS7 | 4 | Preserve launchd-based interactive dev lifecycle management for webhook listener and worker. |
| `rust-skills-catalog-governance` | approved | WS4 | 725 | Track upstream skills and optional-skills catalogs via parity audits while keeping Rust runtime skill loading externalized (no direct Python skill-tree vendoring). |
| `rust-cli-tui-primary-ux-surface` | approved | WS5 | 736 | Treat Rust CLI/TUI and gateway as primary UX surface; upstream web/ui-tui trees are tracked as intentional divergence unless explicitly ported. |


## Notes

- Data is computed directly from git refs in this repository.
- Tree-level parity classification does not require a merge-base.
- Commit representation/missing uses patch-id equivalence from `git cherry`.
- Default behavior fetches latest upstream refs (`git fetch upstream --prune`).
