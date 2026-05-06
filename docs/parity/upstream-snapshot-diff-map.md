# Upstream Snapshot Diff Map

Generated: `2026-05-06T03:50:28.045654+00:00`

- Compared refs: `main` vs `upstream/main`
- Total diff entries: `3503`
- Status counts: `A=3021`, `D=470`, `M=12`

## Prefix Groups

| Prefix | Total | A | M | D | Classification |
| --- | ---: | ---: | ---: | ---: | --- |
| `tests` | 960 | 960 | 0 | 0 | `needs_rust_implementation_review` |
| `skills` | 514 | 514 | 0 | 0 | `needs_rust_implementation_review` |
| `crates` | 356 | 0 | 0 | 356 | `intentional_divergence_rust_primary` |
| `website` | 339 | 339 | 0 | 0 | `needs_rust_implementation_review` |
| `ui-tui` | 307 | 307 | 0 | 0 | `needs_rust_implementation_review` |
| `optional-skills` | 211 | 203 | 2 | 6 | `needs_rust_implementation_review` |
| `plugins` | 154 | 154 | 0 | 0 | `needs_rust_implementation_review` |
| `web` | 90 | 90 | 0 | 0 | `needs_rust_implementation_review` |
| `tools` | 89 | 89 | 0 | 0 | `needs_rust_implementation_review` |
| `scripts` | 71 | 19 | 1 | 51 | `selective_adopt_review` |
| `hermes_cli` | 67 | 67 | 0 | 0 | `needs_rust_implementation_review` |
| `agent` | 58 | 58 | 0 | 0 | `needs_rust_implementation_review` |
| `gateway` | 58 | 58 | 0 | 0 | `needs_rust_implementation_review` |
| `docs` | 46 | 2 | 0 | 44 | `manual_review_required` |
| `environments` | 43 | 43 | 0 | 0 | `needs_rust_implementation_review` |
| `.github` | 20 | 17 | 0 | 3 | `selective_adopt_review` |
| `nix` | 11 | 11 | 0 | 0 | `selective_adopt_review` |
| `acp_adapter` | 9 | 9 | 0 | 0 | `needs_rust_implementation_review` |
| `locales` | 8 | 8 | 0 | 0 | `manual_review_required` |
| `tui_gateway` | 8 | 8 | 0 | 0 | `needs_rust_implementation_review` |
| `datagen-config-examples` | 4 | 4 | 0 | 0 | `manual_review_required` |
| `cron` | 3 | 3 | 0 | 0 | `needs_rust_implementation_review` |
| `providers` | 3 | 3 | 0 | 0 | `manual_review_required` |
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
| `README.zh-CN.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `README_JA.md` | 1 | 0 | 0 | 1 | `manual_review_required` |
| `README_KO.md` | 1 | 0 | 0 | 1 | `manual_review_required` |
| `README_QUICKSTART.md` | 1 | 0 | 0 | 1 | `manual_review_required` |
| `README_ZH.md` | 1 | 0 | 0 | 1 | `manual_review_required` |
| `RELEASE_v0.10.0.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `RELEASE_v0.11.0.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
| `RELEASE_v0.12.0.md` | 1 | 1 | 0 | 0 | `manual_review_required` |
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
| `infra_surface` | 106 | `A=49, D=54, M=3` |
| `runtime_surface` | 426 | `A=426` |
| `rust_divergence` | 358 | `D=358` |
| `ux_surface` | 1461 | `A=1453, D=6, M=2` |
| `validation_surface` | 1003 | `A=1003` |

## Notes

- `intentional_divergence_rust_primary` marks Rust-first surfaces kept on purpose in this fork.
- `needs_rust_implementation_review` marks upstream product/runtime paths to review for behavior-level parity.
- `selective_adopt_review` marks installer/docs/infra paths that may need partial adoption.
- `manual_review_required` marks uncategorized paths requiring explicit triage.

