# Hermes Agent (Rust)

**[English](./README.md)** | **[中文](./README_ZH.md)** | **[日本語](./README_JA.md)** | **[한국어](./README_KO.md)**

A production-grade Rust rewrite of [Hermes Agent](https://github.com/NousResearch/hermes-agent) — the self-improving AI agent by [Nous Research](https://nousresearch.com).

**84,000+ lines of Rust · 16 crates · 641 tests · 17 platform adapters · 30 tool backends · 8 memory plugins · 6 cross-platform release targets**

---

## Python v2026.4.13 Alignment Status

Baseline target: `NousResearch/hermes-agent@v2026.4.13` (`1af2e18d408a9dcc2c61d6fc1eef5c6667f8e254`).

- Progress: **9 / 13 scoped parity items completed**.
- Completed focus areas: prompt layering/core guidance parity, smart routing basic runtime switching and fallback, memory tool semantics and limits, built-in `MEMORY.md`/`USER.md` snapshot injection, memory lifecycle hooks (`on_memory_write`, `queue_prefetch`, `on_pre_compress`, `on_session_end`, `on_delegation`), `session_search` dual mode with `role_filter` and capped limit.
- Remaining focus areas: automatic current-session lineage exclusion without tool args, full `resolve_turn_route` runtime signature parity fields, and Python-style skill-driven behavior self-improvement loop parity.

### TODO (Parity Tracker)

- [x] Long Memory: built-in memory action/target semantics + char limits.
- [x] Long Memory: memory snapshot prompt injection at session start.
- [x] Long Memory: lifecycle hooks (`on_memory_write`, `on_pre_compress`, `on_session_end`, `on_delegation`).
- [x] Session Search: recent mode (empty query), keyword mode, `role_filter`, `limit <= 5`.
- [x] Session Search: child->parent lineage normalization support (parent session column + resolution).
- [x] Session Search: Python-equivalent per-session LLM summary generation.
- [x] Session Search: hidden/internal source filtering parity.
- [ ] Session Search: auto inject and exclude active session lineage by runtime context.
- [x] Smart Model Selection: per-turn cheap-route and policy recommendation route.
- [x] Smart Model Selection: routed-provider build failure fallback to primary provider.
- [ ] Smart Model Selection: full Python `resolve_turn_route` runtime signature surface (`api_mode`, `command`, `args`, `credential_pool`, `signature`) end-to-end.
- [ ] Self-Evolution: Python-style memory/skills-driven automatic adaptation loop parity.
- [ ] Self-Evolution: parity validation tests vs Python `v2026.4.13` behavior fixtures.

### Capability Status (Requested Checklist)

Status legend: `implemented` = available in current codebase, `partial` = available but not fully equivalent to the requested wording/behavior.

| Capability | Status | Notes |
|---|---|---|
| Interactive CLI + one-shot mode (`crates/hermes-cli`) | implemented | TUI interactive mode + `chat --query` one-shot path are present. |
| Agent loop: streaming + tool execution + context compression | implemented | `run_stream`, parallel tool execution, auto compression are implemented. |
| Prompt caching | partial | System prompt builder cache exists; full cross-request prompt-cache behavior is not fully parity-tracked yet. |
| Providers: Anthropic, OpenAI chat-compatible, OpenAI Responses, OpenRouter-compatible | implemented | Provider adapters exist across `hermes-agent`/`api_bridge`/extras. |
| Built-in tools: files/terminal/patch/memory/web/vision + opt-in code execution | implemented | Toolset includes these categories; code execution is available and should be policy/toolset controlled. |
| Runtime MCP tool discovery from configured stdio/HTTP servers | implemented | MCP client supports stdio/http configs and runtime tools listing. |
| MCP bridge tools for prompts/resources with capability gating | partial | MCP prompt/resource APIs exist; strict capability-gated bridge behavior is not fully marked complete. |
| Local memory snapshots + request-local skill matching/injection | implemented | `MEMORY.md`/`USER.md` snapshot injection and skills prompt orchestration are wired. |
| SQLite-backed session history + resume | implemented | SQLite persistence (`sessions.db`) and session load/resume workflows exist. |
| Multi-model support (OpenAI/Anthropic/OpenRouter) | implemented | Supported in routing/provider stack. |
| Built-in tools count (your list says 26) | implemented | Current Rust repo is already above this (30+ tool backends). |
| TUI: interactive chat, slash commands, tool progress, status bar | implemented | TUI + status bar + extensive slash command handlers are present. |
| Context-aware auto-loading (`AGENTS.md`, `CLAUDE.md`, `MEMORY.md`, `USER.md`) | implemented | Context file loaders + memory snapshot loaders are present. |
| Memory system: SQLite + FTS5 + cross-session persistence | implemented | Session persistence + FTS-backed `session_search` are implemented. |
| Skills system: YAML-based skill creation/management | implemented | Skills tooling and skill store/hub pipeline are present. |
| Personality system: coder/writer/analyst personas | partial | Personality switching is implemented; named personas depend on available personality files. |
| Context compression: automatic + manual | implemented | Auto compression in loop + manual slash command path exist. |
| Sub-agent delegation | partial | `delegate_task` tooling and delegation hooks exist; full autonomous child-agent orchestration is still evolving. |
| Messaging: Telegram/Discord/Slack APIs | implemented | Gateway adapters for these platforms are present. |
| Security: path validation, dangerous command blocking, search-depth limits | partial | Command approval and credential/file guards are present; not all requested guard dimensions are fully parity-tracked. |
| Chinese input / UTF-8 in TUI | implemented | Rust/TUI path handles UTF-8 text input/output normally. |

## Highlights

### Single Binary, Zero Dependencies

One ~16MB binary. No Python, no pip, no virtualenv, no Docker required. Runs on Raspberry Pi, $3/month VPS, air-gapped servers, Docker scratch images.

```bash
scp hermes user@server:~/
./hermes
```

### Self-Evolution Policy Engine

The agent learns from its own execution. A three-layer adaptive system:

- **L1 — Model & Retry Tuning.** Multi-armed bandit selects the best model per task based on historical success rate, latency, and cost. Retry strategy adjusts dynamically based on task complexity.
- **L2 — Long-Task Planning.** Automatically decides parallelism, subtask splitting, and checkpoint intervals for complex prompts.
- **L3 — Prompt & Memory Shaping.** System prompts and memory context are optimized and trimmed per-request based on accumulated feedback.

Policy versioning with canary rollout, hard-gate rollback, and audit logging. The engine improves over time without manual tuning.

### True Concurrency

Rust's tokio runtime gives real parallel execution — not Python's cooperative asyncio. `JoinSet` dispatches tool calls across OS threads. A 30-second browser scrape doesn't block a 50ms file read. The gateway processes messages from 17 platforms simultaneously without a GIL.

### 17 Platform Adapters

Telegram, Discord, Slack, WhatsApp, Signal, Matrix, Mattermost, DingTalk, Feishu, WeCom, Weixin, Email, SMS, BlueBubbles, Home Assistant, Webhook, API Server.

### 30 Tool Backends

File operations, terminal, browser, code execution, web search, vision, image generation, TTS, transcription, memory, messaging, delegation, cron jobs, skills, session search, Home Assistant, RL training, URL safety, OSV vulnerability check, and more.
The built-in `memory` tool follows Python parity semantics: `action=add|replace|remove`, `target=memory|user`, with `old_text` substring matching for replace/remove updates.
Built-in store limits also match Python defaults: `memory` ≈ 2200 chars and `user` ≈ 1375 chars.
The built-in `session_search` now supports Python-style dual mode: recent-session browse when `query` is omitted, and keyword search with optional `role_filter` plus `limit` capped at 5.
`session_search` can run per-session LLM summaries when auxiliary credentials are available (`HERMES_SESSION_SEARCH_SUMMARY_API_KEY` or `OPENAI_API_KEY`; optional base/model overrides).

### 8 Memory Plugins

Mem0, Honcho, Holographic, Hindsight, ByteRover, OpenViking, RetainDB, Supermemory.
Built-in `~/.hermes/memories/MEMORY.md` and `USER.md` snapshots are also injected at session start for prompt-stable long memory context.

### 6 Terminal Backends

Local, Docker, SSH, Daytona, Modal, Singularity.

### MCP (Model Context Protocol) Support

Built-in MCP client and server. Connect to external tool providers or expose Hermes tools to other MCP-compatible agents.

### ACP (Agent Communication Protocol)

Inter-agent communication with session management, event streaming, and permission controls.

---

## Architecture

### 16-Crate Workspace

```
crates/
├── hermes-core           # Shared types, traits, error hierarchy
├── hermes-agent          # Agent loop, LLM providers, context, memory plugins
├── hermes-tools          # Tool registry, dispatch, 30 tool backends
├── hermes-gateway        # Message gateway, 17 platform adapters
├── hermes-cli            # CLI/TUI binary, slash commands
├── hermes-config         # Configuration loading, merging, YAML compat
├── hermes-intelligence   # Self-evolution engine, model routing, prompt building
├── hermes-skills         # Skill management, store, security guard
├── hermes-environments   # Terminal backends (Local/Docker/SSH/Daytona/Modal/Singularity)
├── hermes-cron           # Cron scheduling and persistence
├── hermes-mcp            # Model Context Protocol client/server
├── hermes-acp            # Agent Communication Protocol
├── hermes-rl             # Reinforcement learning runs
├── hermes-http           # HTTP/WebSocket API server
├── hermes-auth           # OAuth token exchange
└── hermes-telemetry      # OpenTelemetry integration
```

### Trait-Based Abstraction

| Trait | Purpose | Implementations |
|-------|---------|----------------|
| `LlmProvider` | LLM API calls | OpenAI, Anthropic, OpenRouter, Generic |
| `ToolHandler` | Tool execution | 30 tool backends |
| `PlatformAdapter` | Messaging platforms | 17 platforms |
| `TerminalBackend` | Command execution | Local, Docker, SSH, Daytona, Modal, Singularity |
| `MemoryProvider` | Persistent memory | 8 memory plugins + file/SQLite |
| `SkillProvider` | Skill management | File store + Hub |

### Error Hierarchy

```
AgentError (top-level)
├── LlmApi(String)
├── ToolExecution(String)      ← auto-converted from ToolError
├── Gateway(String)            ← auto-converted from GatewayError
├── Config(String)             ← auto-converted from ConfigError
├── RateLimited { retry_after_secs }
├── Interrupted { message }
├── ContextTooLong
├── MaxTurnsExceeded
└── Io(String)
```

Every error type converts automatically via `From` traits. The compiler ensures every error path is handled.

---

## Install

Download the latest release binary for your platform:

```bash
# macOS (Apple Silicon)
curl -LO https://github.com/Lumio-Research/hermes-agent-rs/releases/latest/download/hermes-macos-aarch64.tar.gz
tar xzf hermes-macos-aarch64.tar.gz && sudo mv hermes /usr/local/bin/

# macOS (Intel)
curl -LO https://github.com/Lumio-Research/hermes-agent-rs/releases/latest/download/hermes-macos-x86_64.tar.gz
tar xzf hermes-macos-x86_64.tar.gz && sudo mv hermes /usr/local/bin/

# Linux (x86_64)
curl -LO https://github.com/Lumio-Research/hermes-agent-rs/releases/latest/download/hermes-linux-x86_64.tar.gz
tar xzf hermes-linux-x86_64.tar.gz && sudo mv hermes /usr/local/bin/

# Linux (ARM64)
curl -LO https://github.com/Lumio-Research/hermes-agent-rs/releases/latest/download/hermes-linux-aarch64.tar.gz
tar xzf hermes-linux-aarch64.tar.gz && sudo mv hermes /usr/local/bin/

# Linux (musl / Alpine / Docker)
curl -LO https://github.com/Lumio-Research/hermes-agent-rs/releases/latest/download/hermes-linux-x86_64-musl.tar.gz
tar xzf hermes-linux-x86_64-musl.tar.gz && sudo mv hermes /usr/local/bin/

# Windows (x86_64)
# Download hermes-windows-x86_64.zip from the releases page
```

All release binaries: https://github.com/Lumio-Research/hermes-agent-rs/releases

## Building from source

```bash
cargo build --release
# Binary at target/release/hermes
```

## Running

```bash
hermes              # Interactive chat
hermes --help       # All commands
hermes gateway start  # Start multi-platform gateway
hermes doctor       # Check dependencies and config
```

## Testing

```bash
cargo test --workspace   # 641 tests
```

## Contributing

```bash
git config core.hooksPath scripts/git-hooks  # Enable pre-commit fmt check
```

## License

MIT — see [LICENSE](LICENSE).

Based on [Hermes Agent](https://github.com/NousResearch/hermes-agent) by [Nous Research](https://nousresearch.com).
