# Hermes Agent (Rust)

**[English](./README.md)** | **[中文](./README_ZH.md)** | **[日本語](./README_JA.md)** | **[한국어](./README_KO.md)**

A production-grade Rust rewrite of [Hermes Agent](https://github.com/NousResearch/hermes-agent) — the self-improving AI agent by [Nous Research](https://nousresearch.com).

**84,000+ lines of Rust · 16 crates · 641 tests · 17 platform adapters · 30 tool backends · 8 memory plugins · 6 cross-platform release targets**

---

## Python v2026.4.13 Alignment Status

Baseline target: `NousResearch/hermes-agent@v2026.4.13` (`1af2e18d408a9dcc2c61d6fc1eef5c6667f8e254`).

- Progress: **7 / 13 scoped parity items completed**.
- Completed focus areas: prompt layering/core guidance parity, smart routing basic runtime switching and fallback, memory tool semantics and limits, built-in `MEMORY.md`/`USER.md` snapshot injection, memory lifecycle hooks (`on_memory_write`, `queue_prefetch`, `on_pre_compress`, `on_session_end`, `on_delegation`), `session_search` dual mode with `role_filter` and capped limit.
- Remaining focus areas: full Python-equivalent `session_search` LLM summarization pipeline and hidden-source filtering, automatic current-session lineage exclusion without tool args, full `resolve_turn_route` runtime signature parity fields, and Python-style skill-driven behavior self-improvement loop parity.

### TODO (Parity Tracker)

- [x] Long Memory: built-in memory action/target semantics + char limits.
- [x] Long Memory: memory snapshot prompt injection at session start.
- [x] Long Memory: lifecycle hooks (`on_memory_write`, `on_pre_compress`, `on_session_end`, `on_delegation`).
- [x] Session Search: recent mode (empty query), keyword mode, `role_filter`, `limit <= 5`.
- [x] Session Search: child->parent lineage normalization support (parent session column + resolution).
- [ ] Session Search: Python-equivalent per-session LLM summary generation.
- [ ] Session Search: hidden/internal source filtering parity.
- [ ] Session Search: auto inject and exclude active session lineage by runtime context.
- [x] Smart Model Selection: per-turn cheap-route and policy recommendation route.
- [x] Smart Model Selection: routed-provider build failure fallback to primary provider.
- [ ] Smart Model Selection: full Python `resolve_turn_route` runtime signature surface (`api_mode`, `command`, `args`, `credential_pool`, `signature`) end-to-end.
- [ ] Self-Evolution: Python-style memory/skills-driven automatic adaptation loop parity.
- [ ] Self-Evolution: parity validation tests vs Python `v2026.4.13` behavior fixtures.

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
