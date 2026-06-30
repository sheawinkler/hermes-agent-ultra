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
- First-class local/self-host backends: Ollama, llama.cpp/llamafile, vLLM, MLX, Apple ANE endpoint, SGLang, TGI, LM Studio, LMDeploy, LocalAI, KoboldCpp, text-generation-webui, TabbyAPI
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
- `Session branching + time-travel`: checkpoint/rollback/replay navigation from the TUI
- `Tool-call simulator`: preview policy allow/deny outcomes before running risky tool invocations
- `Adaptive repo-review budget controls`: tune discovery-loop trimming live (`balanced/aggressive/relaxed/off`)
- `Semantic repo graph`: inspect dependency hubs/edges with inline Mermaid preview
- `Provider QoS router controls`: inspect route learning/health and apply autotune from chat
- `Live session eval harness`: score real saved sessions and gate quality trends from actual usage
- `RTK raw-mode controls`: inspect unwrapped tool payloads when debugging integrations
- `Memory fusion`: ContextLattice + external memory providers with scoring/fusion logic
- `One-true-harness cockpit`: `/harness` and `harness_cockpit` unify skills, proof, OIDC, replay, objectives, onboarding, and chaos probes
- `Advanced sync gates`: differential parity checks, red-team/adversarial gating, elite sync gate
- `Operational tooling`: deep doctor snapshots, replay traces, queue-based upstream webhook sync
- `Rust-only implementation strategy`: parity in Rust first; no direct Python runtime vendoring

## Install

### One-line installer

```bash
curl -fsSL https://raw.githubusercontent.com/sheawinkler/hermes-agent-ultra/main/scripts/install.sh | bash
```

The one-line installer is safe to run on machines that also have upstream
NousResearch Hermes installed as `hermes`: by default it installs
`hermes-agent-ultra` and `hermes-ultra` only, leaving any existing `hermes`
command untouched. In non-interactive `curl | bash` installs, post-install
doctor/auth/setup probes are skipped by default; run setup later with
`hermes-ultra setup`, or opt in during install with:

```bash
curl -fsSL https://raw.githubusercontent.com/sheawinkler/hermes-agent-ultra/main/scripts/install.sh | bash -s -- --setup
```

Custom install path:

```bash
curl -fsSL https://raw.githubusercontent.com/sheawinkler/hermes-agent-ultra/main/scripts/install.sh | sudo INSTALL_DIR=/usr/local/bin bash
```

### From source

```bash
cargo install --git https://github.com/sheawinkler/hermes-agent-ultra hermes-cli --locked --bin hermes-agent-ultra --bin hermes-ultra
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

Interactive mode is single-instance per Hermes home by default (prevents accidental parallel TUI sessions sharing the same state).  
If you intentionally want parallel interactive sessions, run:

```bash
HERMES_ALLOW_PARALLEL_INTERACTIVE=1 hermes-ultra
```

One-shot query:

```bash
hermes-ultra chat --query "summarize this repository"
```

Gateway mode:

```bash
hermes-ultra gateway --live
```

## Skip API-key collection With Nous Portal

Hermes Agent Ultra still supports direct provider and per-tool keys. If you prefer one managed subscription for model access plus hosted tool backends, [Nous Portal](https://portal.nousresearch.com) can cover:

- 300+ models, selectable with `/model <name>`.
- Tool Gateway routing for web search, image generation, text-to-speech, and cloud browser backends.

Fresh install path:

```bash
hermes-ultra setup --portal
```

That starts Nous OAuth setup, sets Nous as the provider, and enables Tool Gateway routing. Inspect the current state with:

```bash
hermes-ultra portal info
```

You can still bring your own keys for individual tools; gateway routing is per backend, not all-or-nothing.

Deep diagnostics bundle:

```bash
hermes-ultra doctor --deep --snapshot --bundle
```

Optional Sentrux MCP profile:

Full MCP guide: [docs/mcp.md](./docs/mcp.md)

```bash
hermes-ultra mcp sentrux
hermes-ultra mcp sentrux-status
```

Key operator commands:

```bash
# Capability diagnostics for current or target model
/model explain
/model why-not --cap tools,reasoning --min-context 200000
/swarm status
/swarm plan graph
/swarm run 4 sequential

# Deterministic trace controls
/raw trace status
/raw trace verify
/raw trace export 200

# Runtime policy packs
/policy list
/policy strict
/policy standard
/policy dev

# Adaptive intelligence-performance autopilot
/ops autopilot status
/ops autopilot run
/ops autopilot recommend
/ops autopilot apply

# OpenHuman-derived P0/P1 operator control-plane
/commands search boot
/boot quick
/boot profile prod
/walkthrough start quick
/walkthrough insights
/integrations status
/integrations repair
/integrations snapshot
/triage eval webhook "secret leak panic outage"
/triage feedback webhook critical "secret leak panic outage"
/subconscious status
/subconscious profile strict
/subconscious run 2 --dry-run
/compress rules recommend
/compress rules autotune apply user

# Session time-travel + simulation
/timetravel list
/timetravel goto <snapshot>
/simulate terminal {"cmd":"ls -la"}

# QoS + eval runtime surfaces
/qos status
/qos health
/ops budget balanced
/ops eval run
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
- `lmstudio` (default `http://127.0.0.1:1234/v1`)
- `lmdeploy` (default `http://127.0.0.1:23333/v1`)
- `localai` (default `http://127.0.0.1:8080/v1`)
- `koboldcpp` (default `http://127.0.0.1:5001/v1`)
- `text-generation-webui` (default `http://127.0.0.1:5000/v1`)
- `tabbyapi` (default `http://127.0.0.1:5000/v1`)

Override endpoint URLs via env vars:

- `OLLAMA_BASE_URL`
- `LLAMA_CPP_BASE_URL`
- `VLLM_BASE_URL`
- `MLX_BASE_URL`
- `APPLE_ANE_BASE_URL`
- `SGLANG_BASE_URL`
- `TGI_BASE_URL`
- `LMSTUDIO_BASE_URL`
- `LMDEPLOY_BASE_URL`
- `LOCALAI_BASE_URL`
- `KOBOLDCPP_BASE_URL`
- `TEXT_GENERATION_WEBUI_BASE_URL`
- `TABBYAPI_BASE_URL`

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

- Default GitHub skill taps include OpenAI, Anthropic, VoltAgent, Matt Pocock,
  Addy Osmani, Google, Obsidian, design-skill, Loopy, GitHub Copilot, gstack,
  and MiniMax roots where those repos expose a clear skill directory.
- Registry-aware installs include:
  - `official/...`
  - `skills.sh/...`
  - `github/...`
  - `lobehub/...`
  - `clawhub/...`
  - `claude-marketplace/...`
- Mandatory skill security scanning runs before install and before use.

Use `/harness` in the TUI or the `harness_cockpit` tool for the curated
one-true-harness index, including Matt Pocock `teach`, `domain-modeling`,
`grill-with-docs`, `codebase-design`, and architecture-deepening workflows.

## Dashboard OIDC

`hermes-http` supports a Rust-native dashboard OIDC mode while preserving
`HERMES_HTTP_API_KEY` bearer access for machine clients.

Minimum OIDC env:

```bash
HERMES_DASHBOARD_AUTH_PROVIDER=oidc
HERMES_DASHBOARD_OIDC_ISSUER=https://issuer.example
HERMES_DASHBOARD_OIDC_CLIENT_ID=hermes-dashboard
HERMES_DASHBOARD_SESSION_SECRET='replace-with-strong-random-secret'
```

Optional env includes `HERMES_DASHBOARD_OIDC_CLIENT_SECRET`,
`HERMES_DASHBOARD_OIDC_REDIRECT_URI`, `HERMES_DASHBOARD_OIDC_SCOPES`,
`HERMES_DASHBOARD_OIDC_ALLOWED_EMAILS`, `HERMES_DASHBOARD_OIDC_ALLOWED_DOMAINS`,
`HERMES_DASHBOARD_COOKIE_SECURE`, and explicit authorization/token/JWKS endpoint
overrides.

OpenHuman runbooks and matrices:
- `docs/implementation/openhuman-p0-p1-runbook.md`
- `docs/implementation/openhuman-p0-p1-surface-matrix.md`
- `docs/implementation/openhuman-p2a-p2b-runbook.md`
- `docs/implementation/openhuman-p2a-p2b-surface-matrix.md`
- `docs/implementation/openhuman-p3-swarms-runbook.md`
- `docs/implementation/openhuman-p3-swarms-surface-matrix.md`

## Security Posture

- Skill content security scanning blocks dangerous patterns and restricted URL targets
- Skill guard modes: `strict` (default), `relaxed` (only blocks destructive `rm` ops), `off`
- Policy-controlled tool execution modes: `off`, `audit`, `simulate`, `enforce`
- Tool policy presets: `strict`, `balanced`, `dev`, `relaxed`
- Sensitive field redaction in traces/log surfaces
- Guardrails for path traversal, unsafe file ops, and runtime boundary violations

Operator runtime overrides (env):

- `HERMES_SKILL_GUARD_MODE=relaxed`
- `HERMES_TOOL_POLICY_PRESET=relaxed`
- `HERMES_MAX_TURNS_UNLIMITED=1` (or set `max_turns: 0` in config/profile)
- `HERMES_FORCE_RUNTIME_AUTH_REFRESH=1`
- `HERMES_AUTH_REFRESH_MAX_RETRIES=6`

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
- Queue summary (`docs/parity/upstream-missing-queue.json`): pending `121`, ported `266`, superseded `5499`
- Parity gates (`docs/parity/global-parity-proof.json`): release `fail`, ci `fail`
- Workstream snapshot (`docs/parity/workstream-status.json`): `upstream/main` @ `55cb4103beba5822303c06b662635e1491ae72f5` (generated `2026-06-13T16:15:14-06:00`)
<!-- END:ULTRA_SYNC_STATUS -->

Note: this repository intentionally tracks parity via queue/gate workflows because upstream and ultra history can diverge materially.

## Contributing

Interested in helping? Start with [CONTRIBUTING.md](./CONTRIBUTING.md) for setup, PR expectations, parity rules, and the no-stub completeness gate.

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
