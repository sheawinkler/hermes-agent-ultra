# Full Queue Triage Groups

Generated: `2026-04-21T22:29:36.991784+00:00`

- Triage tag: `triage-full-2026-04-21`
- Queue total commits: `4536`
- New superseded in this pass: `199`
- Pending grouped in this pass: `4175`

## Disposition Counts

- `pending`: `4175`
- `ported`: `12`
- `superseded`: `349`

## Priority Order

1. `WG4-gateway-memory`
2. `WG7-runtime-backfill`
3. `WG1-tests-ci`
4. `WG2-skills`
5. `WG5-env-parsers-bench`
6. `WG6-packaging-install-docs`
7. `WG3-ux-cli-web`

## Groups

### WG7-runtime-backfill — Runtime backfill parity
- target_ticket: `26`
- pending_commits: `1377`
- top_paths: `hermes_cli:516`, `tools:384`, `run_agent.py:240`, `cli.py:212`, `gateway:194`, `agent:181`, `scripts:108`, `cron:40`, `honcho_integration:28`, `nix:28`, `pyproject.toml:26`, `model_tools.py:17`, `environments:17`, `landingpage:17`, `cli-config.yaml.example:16`, `toolsets.py:15`, `configs:13`, `.env.example:12`, `hermes_state.py:12`, `AGENTS.md:11`
- sample_shas: `153cd5bb44ef`, `abe925e21260`, `137ce05324d0`, `07501bef14bf`, `fc792a4be927`, `389ac5e017ed`, `a291cc99cf70`, `f23856df8ef2`, `ed010752dd1f`, `3099a2f53c85`, `84718d183abb`, `586b0a7047ea`

### WG2-skills — Skills parity
- target_ticket: `21`
- pending_commits: `191`
- top_paths: `skills:552`, `optional-skills:212`, `tests:33`, `website:24`, `hermes_cli:21`, `tools:19`, `agent:8`, `gateway:7`, `cli.py:6`, `docs:5`, `CONTRIBUTING.md:5`, `plugins:5`, `AGENTS.md:4`, `README.md:3`, `cli-config.yaml.example:3`, `.env.example:2`, `run_agent.py:2`, `model_tools.py:2`, `toolsets.py:2`, `scripts:2`
- sample_shas: `8fb44608bfe4`, `14e59706b732`, `24c241d29b3a`, `3dfc0a9679d6`, `757d012ab5fc`, `740dd928f769`, `f1311ad3dee4`, `9cc2cf324168`, `6c86c7c4a96e`, `cb92fbe749fb`, `7a4241e4065e`, `669e4d02975f`

### WG4-gateway-memory — Gateway/plugin-memory parity
- target_ticket: `23`
- pending_commits: `499`
- top_paths: `gateway:774`, `tests:291`, `hermes_cli:102`, `plugins:95`, `tools:72`, `agent:46`, `website:41`, `cli.py:17`, `run_agent.py:14`, `scripts:14`, `cron:12`, `toolsets.py:10`, `pyproject.toml:10`, `model_tools.py:5`, `acp_adapter:5`, `optional-skills:5`, `skills:4`, `requirements.txt:3`, `.env.example:3`, `uv.lock:3`
- sample_shas: `ada0b4f131ba`, `f5be6177b231`, `5404a8fcd8a5`, `69aa35a51c3d`, `3191a9ba11d4`, `748fd3db8858`, `ecb430effeca`, `ededaaa87410`, `92447141d95b`, `674a6f96d36d`, `b2172c4b2e80`, `fbb1923fad18`

### WG5-env-parsers-bench — Environments/parsers/benchmarks parity
- target_ticket: `24`
- pending_commits: `59`
- top_paths: `tools:141`, `environments:36`, `tests:25`, `gateway:12`, `agent:9`, `run_agent.py:3`, `cli.py:3`, `hermes_cli:3`, `README.md:2`, `cli-config.yaml.example:1`, `docs:1`, `cron:1`, `pyproject.toml:1`, `skills:1`
- sample_shas: `1b7bc299f373`, `9123cfb5dd4d`, `90af34bc8336`, `a1838271285a`, `b6d7e222c1f6`, `240f33a06fd4`, `f7677ed275e9`, `f14ff3e0417b`, `fb7df099e0fd`, `0ea6c343259a`, `ee7fde653149`, `e36c8cd49a34`

### WG6-packaging-install-docs — Packaging/install/docs parity
- target_ticket: `25`
- pending_commits: `105`
- top_paths: `scripts:56`, `hermes_cli:49`, `.github:49`, `README.md:34`, `landingpage:33`, `tools:27`, `gateway:20`, `docs:18`, `nix:17`, `Dockerfile:14`, `AGENTS.md:10`, `cli.py:9`, `cli-config.yaml.example:7`, `toolsets.py:6`, `run_agent.py:6`, `tests:6`, `model_tools.py:5`, `pyproject.toml:5`, `setup-hermes.sh:4`, `.env.example:4`
- sample_shas: `eb49936a60aa`, `60812ae0418d`, `061fa7090720`, `9e85408c7bfd`, `440c244cac71`, `59cb0cecb214`, `4d5f29c74ca9`, `90e521112876`, `c007b9e5bd19`, `cfef34f7a61f`, `77a3dda59d3a`, `5c4c0c0cbaf4`

### WG1-tests-ci — Tests/CI parity
- target_ticket: `20`
- pending_commits: `1603`
- top_paths: `tests:2571`, `hermes_cli:863`, `tools:495`, `gateway:408`, `agent:350`, `run_agent.py:298`, `cli.py:216`, `website:98`, `cron:60`, `cli-config.yaml.example:40`, `scripts:35`, `acp_adapter:34`, `honcho_integration:26`, `pyproject.toml:25`, `hermes_state.py:23`, `model_tools.py:20`, `.env.example:14`, `AGENTS.md:13`, `toolsets.py:12`, `docs:12`
- sample_shas: `783acd712d6a`, `70dd3a16dccd`, `cbff1b818c30`, `d8a369e19405`, `8fedbf87d92e`, `8fc28c34ce96`, `609b19b63086`, `ce175d73722d`, `e63986b53487`, `47f16505d2e0`, `91bdb9eb2d8e`, `74c662b63a8c`

### WG3-ux-cli-web — UX/CLI/TUI/Web parity
- target_ticket: `22`
- pending_commits: `341`
- top_paths: `ui-tui:929`, `website:384`, `hermes_cli:204`, `web:165`, `tests:113`, `gateway:73`, `tools:53`, `tui_gateway:49`, `agent:29`, `cli.py:22`, `cli-config.yaml.example:19`, `AGENTS.md:15`, `run_agent.py:15`, `nix:13`, `cron:10`, `toolsets.py:9`, `scripts:8`, `plugins:8`, `pyproject.toml:7`, `landingpage:7`
- sample_shas: `ada3713e777c`, `d7d10b14cd51`, `363633e2bafc`, `32636ecf8a75`, `5ce2c47d603a`, `8481fdcf08b0`, `2dbbedc05a7f`, `388dd4789c45`, `55a21fe37b36`, `4be783446af8`, `a23bcb81ceb5`, `2b8856865339`

