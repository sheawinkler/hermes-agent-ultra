# Parity Matrix

Generated: `2026-04-24T01:29:20.072048+00:00`

## Scope

- Local ref: `main` (`efa221b03ffdd61c75bc27990201441d0b397eb2`)
- Upstream ref: `upstream/main` (`6fdbf2f2d76cf37393e657bf37ceda3d84589200`)
- Merge base: `none (history divergence)`

## Summary

| Metric | Value |
| --- | ---: |
| Commits behind local (`upstream` ancestry only) | 137 |
| Commits ahead local (`local` ancestry only) | 276 |
| Upstream commits missing by patch-id (`git cherry local upstream`, `+`) | 132 |
| Upstream commits represented by patch-id (`git cherry local upstream`, `-`) | 0 |
| Local commits unique by patch-id (`git cherry upstream local`, `+`) | 271 |
| Files only in upstream tree | 2300 |
| Files only in local tree | 407 |
| Shared files identical content | 2 |
| Shared files different content | 8 |
| Total files changed (`local` vs `upstream`) | 2716 |
| Insertions (`local` vs `upstream`) | 885115 |
| Deletions (`local` vs `upstream`) | 154262 |

## Top 40 upstream-only buckets

| Bucket | Files |
| --- | ---: |
| `skills/creative` | 200 |
| `tests/gateway` | 181 |
| `tests/tools` | 158 |
| `tests/hermes_cli` | 131 |
| `ui-tui/packages` | 128 |
| `website/docs` | 127 |
| `ui-tui/src` | 119 |
| `optional-skills/mlops` | 81 |
| `skills/mlops` | 66 |
| `skills/productivity` | 66 |
| `skills/research` | 63 |
| `tests/run_agent` | 59 |
| `tests/agent` | 58 |
| `web/src` | 48 |
| `tests/cli` | 45 |
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
| `website/static` | 11 |
| `.github/workflows` | 10 |
| `optional-skills/health` | 9 |
| `skills/red-teaming` | 9 |
| `optional-skills/mcp` | 8 |
| `optional-skills/productivity` | 8 |
| `tests/integration` | 8 |
| `web/public` | 8 |
| `agent/transports` | 7 |
| `skills/media` | 7 |
| `tests/cron` | 7 |
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
| `crates/hermes-tools` | 69 |
| `crates/hermes-gateway` | 45 |
| `crates/hermes-agent` | 44 |
| `crates/hermes-cli` | 38 |
| `crates/hermes-intelligence` | 36 |
| `docs/parity` | 24 |
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
| `scripts/generate-adapter-matrix.py` | 1 |
| `scripts/generate-global-parity-proof.py` | 1 |
| `scripts/generate-homebrew-formula.sh` | 1 |
| `scripts/generate-parity-matrix.py` | 1 |
| `scripts/generate-test-intent-mapping.py` | 1 |

## Workstream Routing

| Workstream | Issue | Name | Upstream-only | Shared-different | Risk | Effort |
| --- | ---: | --- | ---: | ---: | --- | --- |
| `WS6` | #10 | Tests and CI parity | 740 | 0 | high | XL |
| `WS4` | #8 | Skills parity | 662 | 0 | high | XL |
| `WS5` | #9 | UX parity | 471 | 0 | high | XL |
| `WS8` | #12 | Compatibility and divergence policy | 206 | 8 | medium | L |
| `WS3` | #7 | Tools and adapters parity | 172 | 0 | high | L |
| `WS2` | #6 | Core runtime parity | 49 | 0 | critical | M |
| `WS7` | #11 | Security/secrets/store/webhook parity | 0 | 0 | critical | S |

## Commit Mapping

- Upstream missing by patch-id: `132`
- Upstream represented by patch-id: `0`
- Local unique by patch-id: `271`
- Intentional divergence tracked items: `5` (covered files: `1143`)
- Merge base is absent; patch-id mapping is used as primary commit equivalence signal.

## Intentional Divergence Registry

| ID | Status | Workstream | Matched Files | Summary |
| --- | --- | --- | ---: | --- |
| `ultra-contextlattice-memory-plugin` | approved | WS3 | 2 | Keep ContextLattice native memory plugin and provider discovery in the Rust agent runtime. |
| `ultra-webhook-queue-backends` | approved | WS7 | 4 | Preserve webhook-driven sync queue worker architecture with sqlite, SQS, and Kafka support. |
| `ultra-launchd-webhook-lifecycle` | approved | WS7 | 4 | Preserve launchd-based interactive dev lifecycle management for webhook listener and worker. |
| `rust-skills-catalog-governance` | approved | WS4 | 662 | Track upstream skills and optional-skills catalogs via parity audits while keeping Rust runtime skill loading externalized (no direct Python skill-tree vendoring). |
| `rust-cli-tui-primary-ux-surface` | approved | WS5 | 471 | Treat Rust CLI/TUI and gateway as primary UX surface; upstream web/ui-tui trees are tracked as intentional divergence unless explicitly ported. |


## Notes

- Data is computed directly from git refs in this repository.
- Tree-level parity classification does not require a merge-base.
- Commit representation/missing uses patch-id equivalence from `git cherry`.
- Default behavior fetches latest upstream refs (`git fetch upstream --prune`).
