# Parity Matrix

Generated: `2026-04-21T08:47:57.680275+00:00`

## Scope

- Local ref: `main` (`736cfc0879cd441bd0831a1e9b67ca872a22a6c5`)
- Upstream ref: `upstream/main` (`65c2a6b27f6e3bf4441ed063fd96611eacf0aa88`)
- Merge base: `none (history divergence)`

## Summary

| Metric | Value |
| --- | ---: |
| Commits behind local (`upstream` ancestry only) | 5168 |
| Commits ahead local (`local` ancestry only) | 106 |
| Upstream commits missing by patch-id (`git cherry local upstream`, `+`) | 4462 |
| Upstream commits represented by patch-id (`git cherry local upstream`, `-`) | 4 |
| Local commits unique by patch-id (`git cherry upstream local`, `+`) | 104 |
| Files only in upstream tree | 0 |
| Files only in local tree | 368 |
| Shared files identical content | 2186 |
| Shared files different content | 8 |
| Total files changed (`local` vs `upstream`) | 376 |
| Insertions (`local` vs `upstream`) | 2341 |
| Deletions (`local` vs `upstream`) | 129582 |

## Top 40 upstream-only buckets

| Bucket | Files |
| --- | ---: |
| _(none)_ | 0 |

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
| `docs/parity` | 4 |
| `crates/hermes-auth` | 3 |
| `crates/hermes-telemetry` | 3 |
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
| `WS8` | #12 | Compatibility and divergence policy | 0 | 8 | medium | S |
| `WS2` | #6 | Core runtime parity | 0 | 0 | critical | S |
| `WS3` | #7 | Tools and adapters parity | 0 | 0 | high | S |
| `WS4` | #8 | Skills parity | 0 | 0 | medium | S |
| `WS5` | #9 | UX parity | 0 | 0 | medium | S |
| `WS6` | #10 | Tests and CI parity | 0 | 0 | high | S |
| `WS7` | #11 | Security/secrets/store/webhook parity | 0 | 0 | critical | S |

## Commit Mapping

- Upstream missing by patch-id: `4462`
- Upstream represented by patch-id: `4`
- Local unique by patch-id: `104`
- Intentional divergence tracked items: `4` (covered files: `18`)
- Merge base is absent; patch-id mapping is used as primary commit equivalence signal.

## Intentional Divergence Registry

| ID | Status | Workstream | Matched Files | Summary |
| --- | --- | --- | ---: | --- |
| `ultra-contextlattice-memory-plugin` | approved | WS3 | 2 | Keep ContextLattice native memory plugin and provider discovery in the Rust agent runtime. |
| `ultra-webhook-queue-backends` | approved | WS7 | 4 | Preserve webhook-driven sync queue worker architecture with sqlite, SQS, and Kafka support. |
| `ultra-launchd-webhook-lifecycle` | approved | WS7 | 4 | Preserve launchd-based interactive dev lifecycle management for webhook listener and worker. |
| `ultra-root-branding-and-ops-overrides` | approved | WS8 | 8 | Retain repository-specific branding, local agent policy, and operational installer behavior where root shared files differ from upstream. |


## Notes

- Data is computed directly from git refs in this repository.
- Tree-level parity classification does not require a merge-base.
- Commit representation/missing uses patch-id equivalence from `git cherry`.
- Default behavior fetches latest upstream refs (`git fetch upstream --prune`).
