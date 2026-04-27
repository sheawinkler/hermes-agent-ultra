# Upstream Snapshot Diff Map

Generated: `2026-04-27T07:30:15.500479+00:00`

- Compared refs: `main` vs `upstream/main`
- Total diff entries: `3033`
- Status counts: `A=2603`, `D=420`, `M=10`

## Prefix Groups

| Prefix | Total | A | M | D | Classification |
| --- | ---: | ---: | ---: | ---: | --- |
| `tests` | 794 | 794 | 0 | 0 | `needs_rust_implementation_review` |
| `skills` | 483 | 483 | 0 | 0 | `needs_rust_implementation_review` |
| `crates` | 352 | 0 | 0 | 352 | `intentional_divergence_rust_primary` |
| `ui-tui` | 291 | 291 | 0 | 0 | `needs_rust_implementation_review` |
| `website` | 290 | 290 | 0 | 0 | `needs_rust_implementation_review` |
| `optional-skills` | 192 | 189 | 0 | 3 | `needs_rust_implementation_review` |
| `web` | 92 | 92 | 0 | 0 | `needs_rust_implementation_review` |
| `tools` | 84 | 84 | 0 | 0 | `needs_rust_implementation_review` |
| `hermes_cli` | 58 | 58 | 0 | 0 | `needs_rust_implementation_review` |
| `gateway` | 54 | 54 | 0 | 0 | `needs_rust_implementation_review` |
| `plugins` | 54 | 54 | 0 | 0 | `needs_rust_implementation_review` |
| `agent` | 51 | 51 | 0 | 0 | `needs_rust_implementation_review` |
| `environments` | 43 | 43 | 0 | 0 | `needs_rust_implementation_review` |
| `scripts` | 43 | 18 | 1 | 24 | `selective_adopt_review` |
| `docs` | 29 | 0 | 0 | 29 | `manual_review_required` |
| `.github` | 19 | 16 | 0 | 3 | `selective_adopt_review` |
| `acp_adapter` | 9 | 9 | 0 | 0 | `needs_rust_implementation_review` |
| `nix` | 9 | 9 | 0 | 0 | `selective_adopt_review` |
| `tui_gateway` | 8 | 8 | 0 | 0 | `needs_rust_implementation_review` |
| `datagen-config-examples` | 4 | 4 | 0 | 0 | `manual_review_required` |
| `cron` | 3 | 3 | 0 | 0 | `needs_rust_implementation_review` |
| `.plans` | 2 | 2 | 0 | 0 | `manual_review_required` |
| `acp_registry` | 2 | 2 | 0 | 0 | `manual_review_required` |
| `docker` | 2 | 1 | 1 | 0 | `selective_adopt_review` |
| `packaging` | 2 | 1 | 1 | 0 | `selective_adopt_review` |
| `.ci` | 1 | 0 | 0 | 1 | `manual_review_required` |
| `.dockerignore` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `.env.example` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `.envrc` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `.gitattributes` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `.gitignore` | 1 | 0 | 1 | 0 | `selective_adopt_review` |
| `.gitmodules` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `.mailmap` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `AGENTS.md` | 1 | 0 | 1 | 0 | `selective_adopt_review` |
| `CONTRIBUTING.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `Cargo.lock` | 1 | 0 | 0 | 1 | `intentional_divergence_rust_primary` |
| `Cargo.toml` | 1 | 0 | 0 | 1 | `intentional_divergence_rust_primary` |
| `Dockerfile` | 1 | 0 | 1 | 0 | `selective_adopt_review` |
| `LICENSE` | 1 | 0 | 1 | 0 | `selective_adopt_review` |
| `MANIFEST.in` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `NOTICE` | 1 | 0 | 0 | 1 | `manual_review_required` |
| `PARITY_PLAN.md` | 1 | 0 | 0 | 1 | `manual_review_required` |
| `README.md` | 1 | 0 | 1 | 0 | `selective_adopt_review` |
| `README_JA.md` | 1 | 0 | 0 | 1 | `manual_review_required` |
| `README_KO.md` | 1 | 0 | 0 | 1 | `manual_review_required` |
| `README_ZH.md` | 1 | 0 | 0 | 1 | `manual_review_required` |
| `RELEASE_v0.10.0.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `RELEASE_v0.11.0.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `RELEASE_v0.2.0.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `RELEASE_v0.3.0.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `RELEASE_v0.4.0.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `RELEASE_v0.5.0.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `RELEASE_v0.6.0.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `RELEASE_v0.7.0.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `RELEASE_v0.8.0.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `RELEASE_v0.9.0.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `SECURITY.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `UPSTREAM_ATTRIBUTION.md` | 1 | 0 | 0 | 1 | `manual_review_required` |
| `assets` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `batch_runner.py` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `cli-config.yaml.example` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `cli.py` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `constraints-termux.txt` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `docker-compose.yml` | 1 | 0 | 1 | 0 | `manual_review_required` |
| `flake.lock` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `flake.nix` | 1 | 0 | 1 | 0 | `selective_adopt_review` |
| `hermes` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `hermes-already-has-routines.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `hermes_constants.py` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `hermes_logging.py` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `hermes_state.py` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `hermes_time.py` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `mcp_serve.py` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `mini_swe_runner.py` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `model_tools.py` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `package-lock.json` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `package.json` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `plans` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `pyproject.toml` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `rl_cli.py` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `run_agent.py` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `setup-hermes.sh` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `tinker-atropos` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `toolset_distributions.py` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `toolsets.py` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `trajectory_compressor.py` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `utils.py` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `uv.lock` | 1 | 1 | 0 | 0 | `manual_review_required` |

## Tranche Summary

| Tranche | Total | Status Counts |
| --- | ---: | --- |
| `infra_surface` | 75 | `A=45, D=27, M=3` |
| `runtime_surface` | 301 | `A=301` |
| `rust_divergence` | 354 | `D=354` |
| `ux_surface` | 1348 | `A=1345, D=3` |
| `validation_surface` | 837 | `A=837` |

## Notes

- `intentional_divergence_rust_primary` marks Rust-first surfaces kept on purpose in this fork.
- `needs_rust_implementation_review` marks upstream product/runtime paths to review for behavior-level parity.
- `selective_adopt_review` marks installer/docs/infra paths that may need partial adoption.
- `manual_review_required` marks uncategorized paths requiring explicit triage.

