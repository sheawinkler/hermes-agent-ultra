# Parity Matrix

Generated: `2026-04-21T08:30:13.201957+00:00`

## Scope

- Local ref: `main` (`a23e3bed5d1c02d545b2022d4bee5a212626ec76`)
- Upstream ref: `upstream/main` (`517f5e263953ab92c6076cd888ea755af106d6d4`)
- Merge base: `none (history divergence)`

## Summary

| Metric | Value |
| --- | ---: |
| Commits behind local (`upstream` ancestry only) | 5161 |
| Commits ahead local (`local` ancestry only) | 98 |
| Upstream commits missing by patch-id (`git cherry local upstream`, `+`) | 4455 |
| Upstream commits represented by patch-id (`git cherry local upstream`, `-`) | 4 |
| Local commits unique by patch-id (`git cherry upstream local`, `+`) | 96 |
| Files only in upstream tree | 2184 |
| Files only in local tree | 367 |
| Shared files identical content | 0 |
| Shared files different content | 8 |
| Total files changed (`local` vs `upstream`) | 2560 |
| Insertions (`local` vs `upstream`) | 846879 |
| Deletions (`local` vs `upstream`) | 128261 |

## Top 40 upstream-only buckets

| Bucket | Files |
| --- | ---: |
| `tests/gateway` | 174 |
| `skills/creative` | 165 |
| `tests/tools` | 153 |
| `website/docs` | 127 |
| `ui-tui/packages` | 126 |
| `tests/hermes_cli` | 122 |
| `ui-tui/src` | 103 |
| `optional-skills/mlops` | 81 |
| `skills/productivity` | 66 |
| `skills/mlops` | 65 |
| `skills/research` | 63 |
| `tests/run_agent` | 56 |
| `tests/agent` | 52 |
| `web/src` | 47 |
| `tests/cli` | 45 |
| `optional-skills/creative` | 34 |
| `gateway/platforms` | 32 |
| `plugins/memory` | 31 |
| `environments/benchmarks` | 17 |
| `skills/github` | 16 |
| `optional-skills/research` | 14 |
| `optional-skills/security` | 13 |
| `environments/tool_call_parsers` | 12 |
| `tools/environments` | 11 |
| `website/static` | 11 |
| `.github/workflows` | 10 |
| `tests/acp` | 10 |
| `optional-skills/health` | 9 |
| `skills/red-teaming` | 9 |
| `optional-skills/mcp` | 8 |
| `optional-skills/productivity` | 8 |
| `tests/integration` | 8 |
| `tests/plugins` | 8 |
| `web/public` | 8 |
| `skills/media` | 7 |
| `tests/cron` | 7 |
| `optional-skills/devops` | 6 |
| `skills/software-development` | 6 |
| `tests/skills` | 6 |
| `scripts/whatsapp-bridge` | 5 |

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
| `crates/hermes-tools` | 68 |
| `crates/hermes-gateway` | 45 |
| `crates/hermes-agent` | 43 |
| `crates/hermes-intelligence` | 36 |
| `crates/hermes-cli` | 33 |
| `crates/hermes-config` | 18 |
| `crates/hermes-environments` | 13 |
| `crates/hermes-core` | 11 |
| `crates/hermes-eval` | 11 |
| `crates/hermes-parity-tests` | 11 |
| `crates/hermes-cron` | 9 |
| `crates/hermes-acp` | 8 |
| `crates/hermes-mcp` | 7 |
| `crates/hermes-skills` | 7 |
| `crates/hermes-http` | 5 |
| `crates/hermes-auth` | 3 |
| `crates/hermes-telemetry` | 3 |
| `docs/parity` | 3 |
| `.github/workflows` | 2 |
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
| `docs/upstream-sync.md` | 1 |
| `docs/upstream-webhook-sync.md` | 1 |
| `scripts/check-runtime-placeholders.sh` | 1 |
| `scripts/clippy-warning-gate.sh` | 1 |
| `scripts/cron-upstream-sync.sh` | 1 |
| `scripts/generate-homebrew-formula.sh` | 1 |
| `scripts/generate-parity-matrix.py` | 1 |
| `scripts/git-hooks` | 1 |
| `scripts/install-upstream-sync-cron.sh` | 1 |
| `scripts/install-upstream-webhook-launchd.sh` | 1 |

## Workstream Routing

| Workstream | Issue | Name | Upstream-only | Shared-different | Risk | Effort |
| --- | ---: | --- | ---: | ---: | --- | --- |
| `WS6` | #10 | Tests and CI parity | 703 | 0 | high | XL |
| `WS4` | #8 | Skills parity | 624 | 0 | high | XL |
| `WS5` | #9 | UX parity | 451 | 0 | high | XL |
| `WS8` | #12 | Compatibility and divergence policy | 196 | 8 | medium | L |
| `WS3` | #7 | Tools and adapters parity | 161 | 0 | high | L |
| `WS2` | #6 | Core runtime parity | 49 | 0 | critical | M |
| `WS7` | #11 | Security/secrets/store/webhook parity | 0 | 0 | critical | S |

## Commit Mapping

- Upstream missing by patch-id: `4455`
- Upstream represented by patch-id: `4`
- Local unique by patch-id: `96`
- Intentional divergence tracked items: `3` (covered files: `10`)
- Merge base is absent; patch-id mapping is used as primary commit equivalence signal.

## Intentional Divergence Registry

| ID | Status | Workstream | Matched Files | Summary |
| --- | --- | --- | ---: | --- |
| `ultra-contextlattice-memory-plugin` | approved | WS3 | 2 | Keep ContextLattice native memory plugin and provider discovery in the Rust agent runtime. |
| `ultra-webhook-queue-backends` | approved | WS7 | 4 | Preserve webhook-driven sync queue worker architecture with sqlite, SQS, and Kafka support. |
| `ultra-launchd-webhook-lifecycle` | approved | WS7 | 4 | Preserve launchd-based interactive dev lifecycle management for webhook listener and worker. |


## Notes

- Data is computed directly from git refs in this repository.
- Tree-level parity classification does not require a merge-base.
- Commit representation/missing uses patch-id equivalence from `git cherry`.
- Default behavior fetches latest upstream refs (`git fetch upstream --prune`).
