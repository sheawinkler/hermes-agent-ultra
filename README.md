# Hermes Agent Ultra

**[English](./README.md)** | **[中文](./README_ZH.md)** | **[日本語](./README_JA.md)** | **[한국어](./README_KO.md)**

```text
H E R M E S   A G E N T   U L T R A
```

Rust-first autonomous agent runtime with upstream Hermes parity and focused Ultra extensions for reliability, observability, and operator control.

## Why This Exists

Hermes Agent Ultra keeps functional parity with `NousResearch/hermes-agent` while intentionally optimizing the runtime for:

- Rust-native execution paths and reduced runtime drift
- deterministic operations and stronger incident debugging
- tighter control over tool and MCP execution boundaries
- practical deployment for operators who want one binary and clear behavior

## What Is Different in Ultra

Key additions beyond baseline parity:

- Replay trace recorder with sensitive-field redaction
- MCP sandbox profiles (`strict`, `balanced`, `relaxed`) plus message size guardrails
- Tool policy engine (`off`, `audit`, `enforce`) with runtime enforcement hooks
- Memory fusion scoring across providers (ContextLattice + external sources)
- Adaptive turn governor (token budget and tool concurrency pressure controls)
- `doctor --deep --snapshot --bundle` operational support artifacts
- Upstream sync workflow with draft-PR mode for controlled parity roll-forward

## Install

### One-line installer

```bash
curl -fsSL https://raw.githubusercontent.com/sheawinkler/hermes-agent-ultra/main/scripts/install.sh | bash
```

Install to a different path:

```bash
curl -fsSL https://raw.githubusercontent.com/sheawinkler/hermes-agent-ultra/main/scripts/install.sh | sudo INSTALL_DIR=/usr/local/bin bash
```

### From source

```bash
cargo install --git https://github.com/sheawinkler/hermes-agent-ultra hermes-cli --locked --bin hermes-agent-ultra --bin hermes
```

## First Run

```bash
hermes setup
```

Setup initializes `~/.hermes-agent-ultra` and can import API keys from legacy `.env` files when present.

## Daily Use

Interactive:

```bash
hermes
```

One-shot query:

```bash
hermes chat --query "summarize this repository"
```

Gateway mode:

```bash
hermes gateway --live
```

Deep health report with support bundle:

```bash
hermes doctor --deep --snapshot --bundle
```

## Context Loading and Memory

Ultra auto-loads high-value context and memory files:

- `SOUL.md` (persona)
- `AGENTS.md`
- `DESIGN.md`
- `.hermes.md` / `HERMES.md`
- `MEMORY.md` / `USER.md`

Subdirectory hint discovery also loads local context files as the agent navigates code paths.

## DESIGN.md Support

Ultra now supports Google's `DESIGN.md` workflow in two ways:

1. Native context-file detection (`DESIGN.md`) in workspace and subdirectory discovery.
2. Optional skill bundle included at:
   - `optional-skills/creative/design-md/SKILL.md`
   - `optional-skills/creative/design-md/templates/starter.md`

The skill is designed for author/lint/diff/export flows with `@google/design.md`.

## Core Capabilities

- Multi-provider LLM routing (OpenAI, Anthropic, OpenRouter-compatible)
- MCP client/server support
- Rich tool runtime (file ops, terminal, browser, memory, delegation, messaging, cron)
- Skills lifecycle (install, update, publish, snapshot)
- Session persistence and search
- Gateway adapters and API surfaces for production integration

## Architecture

Workspace crates are organized by runtime role:

- `hermes-agent`: agent loop, provider calls, memory orchestration
- `hermes-tools`: tool registry and execution backends
- `hermes-mcp`: MCP transport/client/server flows
- `hermes-cli`: CLI, TUI, setup, doctor, operator commands
- `hermes-gateway`: gateway adapters and runtime hooks
- `hermes-skills`: skill hub/store primitives
- `hermes-config`: config model and loading
- `hermes-telemetry`: metrics and instrumentation

See `crates/` for the full workspace.

## Security and Guardrails

Ultra ships with explicit runtime controls:

- credential/path safeguards in file and terminal tools
- prompt-injection scanning for loaded context files
- policy-driven tool execution gates
- MCP capability and sandbox policy boundaries
- bounded output and context shaping protections

## Parity and Upstream Maintenance

This repository tracks upstream Hermes parity and intentionally retains Ultra-only enhancements.

Operational model:

- parity analysis + triage in-repo (`docs/parity/`)
- controlled upstream sync via scripts
- preserved chronology through regular commits on `main`

Primary sync tooling:

- `scripts/sync-upstream.sh`
- `scripts/upstream_webhook_sync.py`

## License, Attribution, and Ownership

- Ultra is distributed under this repository's license and notices.
- Upstream attribution is documented in [UPSTREAM_ATTRIBUTION.md](./UPSTREAM_ATTRIBUTION.md).
- This repo is an independent implementation and product line owned and operated under the `hermes-agent-ultra` project identity.

## Links

- Ultra repo: https://github.com/sheawinkler/hermes-agent-ultra
- Upstream repo: https://github.com/NousResearch/hermes-agent
- Issues: https://github.com/sheawinkler/hermes-agent-ultra/issues
