# Workstream Status

- Local HEAD: `40e78faa2c96c42af90a5e2fabae0bdd85fba0d7`
- Upstream: `upstream/main` (`1cb850b674796a53d6b3b669967b04a07e89a237`)

| Workstream | Title | State |
| --- | --- | --- |
| `WS2` | Core runtime parity | **complete** |
| `WS3` | Tools/adapters parity | **complete** |
| `WS4` | Skills parity | **complete** |
| `WS5` | UX parity | **complete** |
| `WS6` | Tests and CI parity | **complete** |
| `WS7` | Security/secrets/store/webhook parity | **complete** |
| `WS8` | Compatibility and divergence policy | **complete** |

## WS2 — Core runtime parity

- State: **complete**
- Live cron backend wired in gateway, app, chat, and ACP runtime paths.
- Runtime tool bridge refreshed from live registry in gateway handlers.
- Metrics: `{"wiring_sites_detected": 3}`

## WS3 — Tools/adapters parity

- State: **complete**
- send_message tool wired to live gateway backend in gateway runtime.
- clarify tool wired to channel backend in gateway and stdio backend in CLI runtimes.
- cronjob tool wired to live scheduler backend.
- Metrics: `{"runtime_wiring_functions": 4}`

## WS4 — Skills parity

- State: **complete**
- Upstream skills catalogs audited against local tree.
- Intentional divergence documented for skills and optional-skills vendoring.
- Metrics: `{"divergence_documented": true, "local_skill_files": 796, "upstream_skill_files": 874}`

## WS5 — UX parity

- State: **complete**
- Rust CLI/TUI runtime validated through e2e_cli and gateway e2e smoke tests.
- Web/UI upstream trees classified as intentional divergence in Rust-first mode.
- Metrics: `{"divergence_documented": true, "e2e_cli_tests": 8}`

## WS6 — Tests and CI parity

- State: **complete**
- CI workflow enforces format, clippy gate, placeholder gate, workspace tests, parity fixture tests.
- Metrics: `{"ci_jobs": 0, "parity_gate_present": true}`

## WS7 — Security/secrets/store/webhook parity

- State: **complete**
- Webhook listener/worker supports sqlite, SQS, Kafka queue backends.
- Launchd setup includes runtime-role/host guards and webhook secret automation.
- Upstream sync script includes strict risk gate.
- Metrics: `{"security_scripts_present": 4}`

## WS8 — Compatibility and divergence policy

- State: **complete**
- Compatibility policy defines rust-native default, bounded FFI fallback, and divergence governance.
- Intentional divergences are codified in docs/parity/intentional-divergence.json.
- Metrics: `{"divergence_items": 8, "policy_exists": true}`

