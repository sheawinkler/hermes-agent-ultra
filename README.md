# Hermes Agent Ultra

**[English](./README.md)** | **[中文](./README_ZH.md)** | **[日本語](./README_JA.md)** | **[한국어](./README_KO.md)**

```text
██   ██ ███████ ██████  ███    ███ ███████ ███████
██   ██ ██      ██   ██ ████  ████ ██      ██
███████ █████   ██████  ██ ████ ██ █████   ███████
██   ██ ██      ██   ██ ██  ██  ██ ██           ██
██   ██ ███████ ██   ██ ██      ██ ███████ ███████

        A G E N T   U L T R A
```

Rust-first autonomous agent runtime with functional parity goals against `NousResearch/hermes-agent`, plus an Ultra reliability, security, and operator-control layer.

## What You Get

- Fully Rust-native core runtime (agent loop, tools, gateway, skills, CLI/TUI)
- Multi-provider inference routing and OAuth-capable provider flows
- First-class local/self-host backends: Ollama, llama.cpp, vLLM, MLX, Apple ANE endpoint, SGLang, TGI
- Tool runtime with policy enforcement, MCP integration, cron, and memory backends
- Parity upkeep system for upstream drift triage and controlled roll-forward
- Production operations surface (`doctor`, replay traces, sync gates, parity artifacts)

## Why Ultra Exists

`NousResearch/hermes-agent` is the canonical upstream product surface.  
Hermes Agent Ultra keeps that surface in scope while focusing on:

- deterministic Rust execution paths
- explicit safety and policy controls
- better observability and incident debugging
- easier operator workflows for long-running local and gateway sessions

## Differentiation vs Upstream

Ultra keeps parity work separate from intentional extensions.

- `Runtime policy engine`: enforce/audit/simulate tool policy decisions at runtime
- `RTK raw-mode controls`: inspect unwrapped tool payloads when debugging integrations
- `Memory fusion`: ContextLattice + external memory providers with scoring/fusion logic
- `Advanced sync gates`: differential parity checks, red-team/adversarial gating, elite sync gate
- `Operational tooling`: deep doctor snapshots, replay traces, queue-based upstream webhook sync
- `Rust-only implementation strategy`: parity in Rust first; no direct Python runtime vendoring

## Install

### One-line installer

```bash
curl -fsSL https://raw.githubusercontent.com/sheawinkler/hermes-agent-ultra/main/scripts/install.sh | bash
```

Custom install path:

```bash
curl -fsSL https://raw.githubusercontent.com/sheawinkler/hermes-agent-ultra/main/scripts/install.sh | sudo INSTALL_DIR=/usr/local/bin bash
```

### From source

```bash
cargo install --git https://github.com/sheawinkler/hermes-agent-ultra hermes-cli --locked --bin hermes-agent-ultra --bin hermes-ultra --bin hermes
```

## Quick Start

Need a shorter path? See [README_QUICKSTART.md](./README_QUICKSTART.md).

Setup:

```bash
hermes-ultra setup
```

Interactive session:

```bash
hermes-ultra
```

One-shot query:

```bash
hermes-ultra chat --query "summarize this repository"
```

Gateway mode:

```bash
hermes-ultra gateway --live
```

Deep diagnostics bundle:

```bash
hermes-ultra doctor --deep --snapshot --bundle
```

Optional Sentrux MCP profile:

```bash
hermes-ultra mcp sentrux
hermes-ultra mcp sentrux-status
```

Key operator commands:

```bash
# Capability diagnostics for current or target model
/model explain
/model why-not --cap tools,reasoning --min-context 200000

# Deterministic trace controls
/raw trace status
/raw trace verify
/raw trace export 200

# Runtime policy packs
/policy list
/policy strict
/policy standard
/policy dev
```

## Local Backends

`hermes-ultra setup` now includes local/self-host provider options with no mandatory API key:

- `ollama-local` (default `http://127.0.0.1:11434/v1`)
- `llama-cpp` (default `http://127.0.0.1:8080/v1`)
- `vllm` (default `http://127.0.0.1:8000/v1`)
- `mlx` (default `http://127.0.0.1:8080/v1`)
- `apple-ane` (default `http://127.0.0.1:8081/v1`)
- `sglang` (default `http://127.0.0.1:30000/v1`)
- `tgi` (default `http://127.0.0.1:8082/v1`)

Override endpoint URLs via env vars:

- `OLLAMA_BASE_URL`
- `LLAMA_CPP_BASE_URL`
- `VLLM_BASE_URL`
- `MLX_BASE_URL`
- `APPLE_ANE_BASE_URL`
- `SGLANG_BASE_URL`
- `TGI_BASE_URL`

Detailed guide: [docs/local-backends.md](./docs/local-backends.md)

## Built-In Context + Memory Behavior

Ultra auto-loads high-value project and persona context:

- `SOUL.md`
- `AGENTS.md`
- `DESIGN.md`
- `.hermes.md` / `HERMES.md`
- `MEMORY.md` / `USER.md`

Subdirectory discovery is enabled so context follows the code path being edited.

## Skills and Registry Surface

Skills commands support multi-registry search/install and local tap flows.

- Registry-aware installs include:
  - `official/...`
  - `skills.sh/...`
  - `github/...`
  - `lobehub/...`
  - `clawhub/...`
  - `claude-marketplace/...`
- Mandatory skill security scanning runs before install and before use.

## Security Posture

- Skill content security scanning blocks dangerous patterns and restricted URL targets
- Policy-controlled tool execution modes: `off`, `audit`, `simulate`, `enforce`
- Sensitive field redaction in traces/log surfaces
- Guardrails for path traversal, unsafe file ops, and runtime boundary violations

## Upstream Sync and Parity Upkeep

Ultra uses controlled sync workflows, not blind merges.

- Upstream source of truth: `NousResearch/hermes-agent`
- Fetch/sync tooling:
  - `scripts/sync-upstream.sh`
  - `scripts/upstream_webhook_sync.py`
- Parity artifacts:
  - `docs/parity/`
  - `.sync-reports/`
<!-- BEGIN:ULTRA_SYNC_STATUS -->
### Live Upstream Sync Status (auto-generated)

- Generated at: `20260504-053352`
- Source report: [`upstream-sync-20260504-053352.txt`](./.sync-reports/upstream-sync-20260504-053352.txt)
- Sync timestamp (`timestamp_utc`): `20260504-053352`
- `origin/main` at sync: `1861c5dcfb8cad8dcddb5f15c1a5a8c34c7f1ce2`
- `upstream/main` at sync: `95f395027f72c69f06bddcecb08da53cfd10c440`
- Pending commits captured in report: `1512`
- Queue summary (`docs/parity/upstream-missing-queue.json`): pending `138`, ported `51`, superseded `1258`
- Parity gates (`docs/parity/global-parity-proof.json`): release `fail`, ci `fail`
- Workstream snapshot (`docs/parity/workstream-status.json`): `upstream/main` @ `95f395027f72c69f06bddcecb08da53cfd10c440` (generated `2026-05-03T17:47:41-06:00`)
<!-- END:ULTRA_SYNC_STATUS -->

Note: this repository intentionally tracks parity via queue/gate workflows because upstream and ultra history can diverge materially.

## Official References and Attribution

Canonical/official upstream references:

- Upstream (official): https://github.com/NousResearch/hermes-agent
- Ultra (this repository): https://github.com/sheawinkler/hermes-agent-ultra
- Ultra fork archive (historical): https://github.com/sheawinkler/hermes-agent-rs-fork

Integrated ecosystem references used in Ultra workflows:

- OpenAI skills repository: https://github.com/openai/skills
- Anthropic skills repository: https://github.com/anthropics/skills
- VoltAgent skills aggregation: https://github.com/VoltAgent/awesome-agent-skills
- Ratatui (TUI foundation): https://github.com/ratatui/ratatui
- tui-textarea (composer/editor behavior): https://github.com/rhysd/tui-textarea

Additional ownership, provenance, and credit notes are maintained in [UPSTREAM_ATTRIBUTION.md](./UPSTREAM_ATTRIBUTION.md).

## Architecture Map

Primary Rust workspace crates:

- `crates/hermes-agent`: agent loop, memory orchestration, provider control
- `crates/hermes-tools`: tool registry and execution backends
- `crates/hermes-cli`: CLI/TUI, setup, model/personality switching, operator commands
- `crates/hermes-gateway`: gateway adapters and live runtime paths
- `crates/hermes-skills`: skill storage, guardrails, hub and registry pathways
- `crates/hermes-mcp`: MCP transport/client/server support
- `crates/hermes-config`: config model and runtime loading
- `crates/hermes-telemetry`: tracing and metrics surfaces

## License

Distributed under this repository's license and notices.  
See [LICENSE](./LICENSE), [NOTICE](./NOTICE), and [UPSTREAM_ATTRIBUTION.md](./UPSTREAM_ATTRIBUTION.md).
