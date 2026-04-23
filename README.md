# Hermes Agent (Rust)

**[English](./README.md)** | **[中文](./README_ZH.md)** | **[日本語](./README_JA.md)** | **[한국어](./README_KO.md)**

A production-grade Rust rewrite of [Hermes Agent](https://github.com/NousResearch/hermes-agent) — the self-improving AI agent by [Nous Research](https://nousresearch.com).

**commit-level feature parity with NousResearch**

---

## Python v2026.4.13 Alignment Status

Baseline target: `NousResearch/hermes-agent@v2026.4.13` (`1af2e18d408a9dcc2c61d6fc1eef5c6667f8e254`).

- Progress: **13 / 13 scoped parity items completed**.
- Completed focus areas: prompt layering/core guidance parity, Python-shaped `resolve_turn_route` / cheap-route pipeline and runtime snapshots (`api_mode`, primary `acp_command`/`acp_args`, credential pool, `TurnRouteSignature`) across HTTP and subprocess-backed providers, smart routing runtime switching and fallback, memory tool semantics and limits, built-in `MEMORY.md`/`USER.md` snapshot injection, memory lifecycle hooks (`on_memory_write`, `queue_prefetch`, `on_pre_compress`, `on_session_end`, `on_delegation`), `session_search` dual mode with `role_filter` and capped limit, memory/skill nudge counters + optional background review (Python review prompts; gated by `background_review_enabled`), and fixture-style parity tests for self-evolution cadence.
- Remaining focus areas: capability-level enhancements outside this 13-item parity tracker.

### TODO (Parity Tracker)

- [x] Long Memory: built-in memory action/target semantics + char limits.
- [x] Long Memory: memory snapshot prompt injection at session start.
- [x] Long Memory: lifecycle hooks (`on_memory_write`, `on_pre_compress`, `on_session_end`, `on_delegation`).
- [x] Session Search: recent mode (empty query), keyword mode, `role_filter`, `limit <= 5`.
- [x] Session Search: child->parent lineage normalization support (parent session column + resolution).
- [x] Session Search: Python-equivalent per-session LLM summary generation.
- [x] Session Search: hidden/internal source filtering parity.
- [x] Session Search: auto inject and exclude active session lineage by runtime context.
- [x] Smart Model Selection: per-turn cheap-route and policy recommendation route.
- [x] Smart Model Selection: routed-provider build failure fallback to primary provider.
- [x] Smart Model Selection: Python-shaped `resolve_turn_route` + runtime snapshot fields (`api_mode`, `command`/`args`, `credential_pool`, `signature`) for HTTP-based providers.
- [x] Smart Model Selection: subprocess / external-process inference runtimes (Python `resolve_runtime_provider` extras mapped for `openai-codex` / `qwen-oauth` / `copilot-acp`, including auth-store/runtime metadata parity).
- [x] Self-Evolution: Python-style memory/skill nudge cadence + optional background review pass (same review prompts as Python `v2026.4.13`; off by default).
- [x] Self-Evolution: parity validation tests vs Python `v2026.4.13` behavior fixtures.
- [x] Sub-agent actual execution lifecycle: in-process `SubAgentOrchestrator` (`crates/hermes-agent/src/sub_agent_orchestrator.rs`) handles `spawn / timeout / cancel / resume-via-lineage` instead of signal-only envelope; child runs in its own `tokio::spawn` task (breaking async recursion), parent-to-child cancellation via `InterruptController`, wall-clock timeout, and per-run lineage JSON persisted to `$HERMES_HOME/subagents/<id>.json` at start/complete/fail/timeout/cancel boundaries.
- [x] OAuth provider metadata source: unified `provider config centre` (`llm.<provider>.oauth_token_url` / `oauth_client_id` on `LlmProviderConfig` and `RuntimeProviderConfig`); `oauth_refresh_config` prefers config-centre values and keeps env vars (`HERMES_<PROVIDER>_OAUTH_TOKEN_URL` / `_OAUTH_CLIENT_ID`) as a strict fallback for backward compatibility.

### Capability Status (Requested Checklist)

Status legend: `implemented` = available in current codebase, `partial` = available but not fully equivalent to the requested wording/behavior.

| Capability | Status | Notes |
|---|---|---|
| Interactive CLI + one-shot mode (`crates/hermes-cli`) | implemented | TUI interactive mode + `chat --query` one-shot path are present. |
| Agent loop: streaming + tool execution + context compression | implemented | `run_stream`, parallel tool execution, auto compression are implemented. |
| Prompt caching | implemented | Anthropic-aware cache markers (persistent system + ephemeral recent turns) applied in `messages_for_api_call`; `prompt_cache_hits`/`prompt_cache_misses` counters in telemetry. |
| Providers: Anthropic, OpenAI chat-compatible, OpenAI Responses, OpenRouter-compatible | implemented | Provider adapters exist across `hermes-agent`/`api_bridge`/extras. |
| Built-in tools: files/terminal/patch/memory/web/vision + opt-in code execution | implemented | Toolset includes these categories; code execution is available and should be policy/toolset controlled. |
| Runtime MCP tool discovery from configured stdio/HTTP servers | implemented | MCP client supports stdio/http configs and runtime tools listing. |
| MCP bridge tools for prompts/resources with capability gating | implemented | `McpCapabilityPolicy` gates tool invoke / prompt read / resource read; `prompts/get` endpoint added; `Forbidden` error variant with JSON-RPC code -32600. |
| Local memory snapshots + request-local skill matching/injection | implemented | `MEMORY.md`/`USER.md` snapshot injection and skills prompt orchestration are wired. |
| SQLite-backed session history + resume | implemented | SQLite persistence (`sessions.db`) and session load/resume workflows exist. |
| Multi-model support (OpenAI/Anthropic/OpenRouter) | implemented | Supported in routing/provider stack. |
| Built-in tools count (your list says 26) | implemented | Current Rust repo is already above this (30+ tool backends). |
| TUI: interactive chat, slash commands, tool progress, status bar | implemented | TUI + status bar + extensive slash command handlers are present. |
| Context-aware auto-loading (`AGENTS.md`, `CLAUDE.md`, `MEMORY.md`, `USER.md`) | implemented | Context file loaders + memory snapshot loaders are present. |
| Memory system: SQLite + FTS5 + cross-session persistence | implemented | Session persistence + FTS-backed `session_search` are implemented. |
| Skills system: YAML-based skill creation/management | implemented | Skills tooling and skill store/hub pipeline are present. |
| Personality system: coder/writer/analyst personas | implemented | Built-in `coder`/`writer`/`analyst` persona constants + user-file override; fallback to default identity with warning; snapshot tests validate prompt deltas; WebSocket now parses personality from JSON payloads. |
| Context compression: automatic + manual | implemented | Auto compression in loop + manual slash command path exist. |
| Sub-agent delegation | implemented | `delegate_task` tool with Signal/RPC backends **and in-process `SubAgentOrchestrator`** (actual child `AgentLoop` spawn / wall-clock timeout / cooperative cancel / lineage persistence under `$HERMES_HOME/subagents/`); depth enforcement (`max_depth` default 4); parent budget propagation in delegation envelope; `max_concurrent_delegates` cap; delegation lineage via `on_delegation` memory hook. |
| Messaging: Telegram/Discord/Slack APIs | implemented | Gateway adapters for these platforms are present. |
| Security: path validation, dangerous command blocking, search-depth limits | implemented | `ApprovalManager` wired into `TerminalHandler` (deny/confirm/approve); `CredentialGuard` wired into `ReadFileHandler`/`WriteFileHandler`; search depth capped at 12 levels in `LocalSearchBackend`. |
| Chinese input / UTF-8 in TUI | implemented | Rust/TUI path handles UTF-8 text input/output normally. |

### Partial -> Implemented Execution Checklist

All 5 previously-partial capabilities have been promoted to `implemented`.

#### 1) Prompt caching — DONE

- [x] Anthropic-aware `cache_control` markers applied in `messages_for_api_call` (persistent system + last 4 ephemeral user/assistant turns).
- [x] `prompt_cache_hits` / `prompt_cache_misses` counters added to `hermes-telemetry` and exposed in Prometheus `/metrics`.
- [x] Cache markers gated on provider being Anthropic/Claude (no-op for other providers).

#### 2) MCP bridge tools for prompts/resources with capability gating — DONE

- [x] `McpCapabilityPolicy` struct with `allow_tool_invoke`, `allow_prompt_read`, `allow_resource_read` flags.
- [x] Deny-by-default gating in `handle_tools_call`, `handle_resources_read`, `handle_prompts_get` — returns `McpError::Forbidden`.
- [x] `prompts/get` endpoint implemented in `McpServer`.
- [x] `Forbidden` error variant mapped to JSON-RPC code -32600.

#### 3) Personality system: coder/writer/analyst personas — DONE

- [x] Built-in `coder`, `writer`, `analyst` persona constants compiled into binary.
- [x] User-file override via `~/.hermes/personalities/<name>.md`.
- [x] Runtime switch in CLI + HTTP REST + messaging gateways; WebSocket now parses JSON `personality` field.
- [x] Fallback: unknown slug → warn + default identity; whitespace-containing value → inline personality.
- [x] Snapshot tests validate prompt deltas per persona.

#### 4) Sub-agent delegation — DONE

- [x] `SignalDelegationBackend` enforces `max_depth` (default 4); returns `ToolError` when depth exceeded.
- [x] Delegation envelope includes `child_depth`, `max_depth`, `parent_budget_remaining_usd`.
- [x] `max_concurrent_delegates` cap in `AgentLoop`.
- [x] `on_delegation` memory hook fires for delegation lineage tracking.
- [x] **In-process execution lifecycle** via `SubAgentOrchestrator` (attach with `AgentLoop::with_sub_agent_orchestrator`): spawns a child `AgentLoop` on its own `tokio::spawn` task, applies a wall-clock timeout (`DEFAULT_SUB_AGENT_TIMEOUT_SECS`), propagates parent interrupts via `InterruptController`, and persists `SubAgentLineage` JSON (`started / completed / failed / timeout / cancelled`) under `$HERMES_HOME/subagents/<sub_agent_id>.json`. Return value is structured JSON with `sub_agent_id`, `status`, `total_turns`, `usage`, and `depth`/`max_depth`. Unit tests cover timeout path, cancel path, and child-config clamping.

#### 5) Security: path validation, dangerous command blocking, search-depth limits — DONE

- [x] `ApprovalManager` wired into `TerminalHandler` — denied commands return `ToolError`; confirmation commands auto-approved with warning log.
- [x] `CredentialGuard` wired into `ReadFileHandler` and `WriteFileHandler` — protected paths and secret-containing content blocked.
- [x] `LocalSearchBackend` search depth capped at `MAX_SEARCH_DEPTH` (12) for both content and file-name searches.

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
Matrix E2EE status: encryption-state detection and key APIs (`/keys/query`, `/keys/claim`) are implemented, encrypted timeline events support in-process native Olm/Megolm decrypt when `HERMES_MATRIX_NATIVE_DECRYPT=1` (optional device override: `HERMES_MATRIX_DEVICE_ID`), and the external decrypt bridge remains available as fallback (`HERMES_MATRIX_DECRYPT_FFI_COMMAND`, optional args/timeouts via `HERMES_MATRIX_DECRYPT_FFI_ARGS` / `HERMES_MATRIX_DECRYPT_FFI_TIMEOUT_MS`).

### 30 Tool Backends

File operations, terminal, browser, code execution, web search, vision, image generation, TTS, transcription, memory, messaging, delegation, cron jobs, skills, session search, Home Assistant, RL training, URL safety, OSV vulnerability check, and more.
The built-in `memory` tool follows Python parity semantics: `action=add|replace|remove`, `target=memory|user`, with `old_text` substring matching for replace/remove updates.
Built-in store limits also match Python defaults: `memory` ≈ 2200 chars and `user` ≈ 1375 chars.
The built-in `session_search` now supports Python-style dual mode: recent-session browse when `query` is omitted, and keyword search with optional `role_filter` plus `limit` capped at 5.
`session_search` can run per-session LLM summaries when auxiliary credentials are available (`HERMES_SESSION_SEARCH_SUMMARY_API_KEY`, `HERMES_OPENAI_API_KEY`, or legacy `OPENAI_API_KEY`; optional base/model overrides).

### 8 Memory Plugins

Mem0, Honcho, Holographic, Hindsight, ByteRover, OpenViking, RetainDB, Supermemory.
Built-in `~/.hermes/memories/MEMORY.md` and `USER.md` snapshots are also injected at session start for prompt-stable long memory context.

### 6 Terminal Backends

Local, Docker, SSH, Daytona, Modal, Singularity.

### MCP (Model Context Protocol) Support

Built-in MCP client and server. Connect to external tool providers or expose Hermes tools to other MCP-compatible agents.

### ACP (Agent Communication Protocol)

Inter-agent communication with session management, event streaming, and permission controls.
- `hermes acp start` runs a stdio JSON-RPC ACP server.
- ACP `prompt` execution uses the same Rust `AgentLoop` + provider/tool stack as the CLI runtime.
- Session-level ACP config updates (`session/set_model`, `session/set_mode`, `session/set_config`) persist in session state.

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

**One-line installer** (auto-detects OS/CPU, downloads the latest release, installs to `~/.local/bin` by default):

```bash
curl -fsSL https://raw.githubusercontent.com/sheawinkler/hermes-agent-ultra/main/scripts/install.sh | bash
```

Use another directory (example: system path):

```bash
curl -fsSL https://raw.githubusercontent.com/sheawinkler/hermes-agent-ultra/main/scripts/install.sh | sudo INSTALL_DIR=/usr/local/bin bash
```

**From source with Cargo** (if you have the Rust toolchain):

```bash
cargo install --git https://github.com/sheawinkler/hermes-agent-ultra hermes-cli --locked --bin hermes-agent-ultra --bin hermes
```

The script lives at [`scripts/install.sh`](scripts/install.sh) if you prefer to review it before running.
It does **not** rewrite your shell profile automatically.

If `hermes-agent-ultra` is still not found after install, add the install directory to PATH:

```bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc
```

Installer troubleshooting (macOS permissions, PATH, and safe chmod scope):
- [`docs/installer-troubleshooting.md`](docs/installer-troubleshooting.md)

After install, run `hermes-agent-ultra setup` (or `hermes setup`) to initialize `~/.hermes-agent-ultra`.
Setup can import
missing API keys from legacy Python/OpenClaw `.env` files into
`~/.hermes-agent-ultra/.env`.

Encrypted secret vault (recommended for provider keys):

```bash
hermes-agent-ultra secrets set openai
hermes-agent-ultra secrets list
hermes-agent-ultra secrets get openai
```

The runtime auto-hydrates provider env vars from the vault when env vars are unset.

---

Manual download (latest release binary for your platform):

```bash
# macOS (Apple Silicon)
curl -LO https://github.com/sheawinkler/hermes-agent-ultra/releases/latest/download/hermes-macos-aarch64.tar.gz
tar xzf hermes-macos-aarch64.tar.gz && sudo mv hermes /usr/local/bin/

# macOS (Intel)
curl -LO https://github.com/sheawinkler/hermes-agent-ultra/releases/latest/download/hermes-macos-x86_64.tar.gz
tar xzf hermes-macos-x86_64.tar.gz && sudo mv hermes /usr/local/bin/

# Linux (x86_64)
curl -LO https://github.com/sheawinkler/hermes-agent-ultra/releases/latest/download/hermes-linux-x86_64.tar.gz
tar xzf hermes-linux-x86_64.tar.gz && sudo mv hermes /usr/local/bin/

# Linux (ARM64)
curl -LO https://github.com/sheawinkler/hermes-agent-ultra/releases/latest/download/hermes-linux-aarch64.tar.gz
tar xzf hermes-linux-aarch64.tar.gz && sudo mv hermes /usr/local/bin/

# Linux (musl / Alpine / Docker)
curl -LO https://github.com/sheawinkler/hermes-agent-ultra/releases/latest/download/hermes-linux-x86_64-musl.tar.gz
tar xzf hermes-linux-x86_64-musl.tar.gz && sudo mv hermes /usr/local/bin/

# Windows (x86_64)
# Download hermes-windows-x86_64.zip from the releases page
```

All release binaries: https://github.com/sheawinkler/hermes-agent-ultra/releases

## Building from source

```bash
cargo build --release
# Binary at target/release/hermes
# Canonical binary at target/release/hermes-agent-ultra (legacy alias: target/release/hermes)
```

## Running

```bash
hermes-agent-ultra              # Interactive chat
hermes-agent-ultra --help       # All commands
hermes-agent-ultra gateway start  # Start multi-platform gateway
hermes-agent-ultra doctor       # Check dependencies and config
# Legacy alias still works:
hermes --help
```

## Testing

```bash
cargo test --workspace   # 641 tests
bash scripts/check-runtime-placeholders.sh
```

## Contributing

```bash
git config core.hooksPath scripts/git-hooks  # Enable pre-commit fmt check
```

## Upstream Sync Automation

Use the built-in sync scripts to keep this fork current with upstream:
- [docs/upstream-sync.md](docs/upstream-sync.md)

## Licensing and Attribution

This repository is maintained as a fork of
[Lumio-Research/hermes-agent-rs](https://github.com/Lumio-Research/hermes-agent-rs).

- Fork-authored contributions are licensed under MIT; see [LICENSE](LICENSE).
- Upstream-authored portions remain under upstream ownership/terms.
- As of 2026-04-20, upstream metadata did not publish a root `LICENSE` file, so
  this fork does not claim to relicense upstream material.

Attribution and provenance details:
- [NOTICE](NOTICE)
- [UPSTREAM_ATTRIBUTION.md](UPSTREAM_ATTRIBUTION.md)
