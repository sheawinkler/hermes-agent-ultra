# Hermes Agent (Rust)

A Rust rewrite of [Hermes Agent](https://github.com/NousResearch/hermes-agent) — the self-improving AI agent by [Nous Research](https://nousresearch.com).

---

## Why Rust? The Real Value

### The Problem with Python AI Agents

Python AI agents today share a dirty secret: they're all **single-user toys**. Run one on a $5 VPS, connect Telegram + Discord + Slack, and watch it fall apart under 10 concurrent conversations. The Python GIL, asyncio's cooperative scheduling, and `Dict[str, Any]` everywhere mean you get:

- **One conversation blocks another.** A slow tool call in one session freezes the event loop for everyone.
- **Memory bloat.** Each conversation carries dictionaries of dictionaries, with no way to know what shape the data actually is. A 50-session gateway easily eats 2GB+ RAM.
- **Silent corruption.** A typo in a key name (`"mesage"` instead of `"message"`) passes through every layer undetected until it hits the LLM API and returns garbage.
- **Deployment friction.** `pip install` with 40+ dependencies, version conflicts, platform-specific wheels (try installing `faster-whisper` on ARM Linux), and a 500MB virtualenv.

These aren't bugs. They're the ceiling of the language.

### What Rust Actually Changes

**1. Single Binary, Zero Dependencies**

```bash
# Python: hope your target has Python 3.11+, pip, venv, and compatible wheels
curl -fsSL install.sh | bash  # 500MB+ installed

# Rust: one 15MB binary, runs anywhere
scp hermes user@server:~/
./hermes
```

This is the single biggest deployment advantage. An AI agent that runs on a Raspberry Pi, a $3/month VPS, an air-gapped server, or a Docker scratch image with nothing else in it. No runtime, no interpreter, no dependency hell. For edge AI, IoT, and enterprise environments where you can't install Python — this is the only path.

**2. True Concurrency, Not Pretend Concurrency**

Python's asyncio is cooperative — one badly-behaved tool call that does CPU work (JSON parsing a 10MB response, regex matching, context compression) blocks everything. Rust's tokio gives you:

- **Real parallel tool execution.** `JoinSet` spawns tool calls across OS threads. A 30-second browser scrape doesn't block a 50ms file read.
- **Lock-free message routing.** The gateway can process incoming messages from 16 platforms simultaneously without a GIL.
- **Predictable latency.** No GC pauses. No surprise 200ms freezes when Python decides to collect garbage mid-stream.

For a multi-user gateway serving 100+ concurrent conversations across platforms, this is the difference between "works" and "works reliably."

**3. The Compiler as Architecture Enforcer**

The Python codebase has a 9,913-line `run_agent.py` and a 7,905-line `gateway/run.py`. These files grew organically because Python has no mechanism to prevent it. Any file can import anything. Any function can mutate any global. The type checker is optional and routinely ignored.

Rust's crate system makes this physically impossible:

```
hermes-core          ← defines traits, owned by nobody
hermes-agent         ← depends on core, cannot see gateway
hermes-gateway       ← depends on core, cannot see agent internals
hermes-tools         ← depends on core, cannot see provider details
```

Circular dependencies? Compile error. Forgotten error handling? Compile error. Wrong message type passed to a tool? Compile error. This isn't discipline — it's physics. The architecture can't degrade over time because the compiler won't let it.

**4. Type Safety Where It Matters Most**

In the Python version, a "message" is `Dict[str, Any]`. A "tool call" is `Dict[str, Any]`. A "config" is `Dict[str, Any]`. When something goes wrong, you get a `KeyError` at runtime, in production, at 3am.

In Rust:

```rust
pub struct Message {
    pub role: MessageRole,        // enum, not string
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub reasoning_content: Option<ReasoningContent>,
    pub cache_control: Option<CacheControl>,
}
```

Every field is typed. Every variant is enumerated. Every error path is handled. The LLM returns unexpected JSON? `serde` catches it at the boundary. A tool handler returns the wrong type? It doesn't compile. This eliminates an entire class of bugs that plague Python agents in production.

**5. Memory Efficiency for Long-Running Agents**

An AI agent isn't a request-response server. It runs for days, weeks, months. It accumulates conversation history, skill files, memory entries, session state. Python's reference-counted GC and dict overhead mean memory grows unpredictably.

Rust's ownership model means:
- Conversation history is freed the moment a session ends. No GC delay.
- Tool results are truncated and dropped immediately after context insertion.
- The entire agent state for 100 concurrent sessions fits in ~50MB, not 2GB.

For a personal AI agent running 24/7 on a cheap VPS, this is the difference between $3/month and $20/month.

---

## Architectural Decisions

### Trait-Based Abstraction

Every integration point is a trait:

| Trait | Purpose | Implementations |
|-------|---------|----------------|
| `LlmProvider` | LLM API calls | OpenAI, Anthropic, OpenRouter, Generic |
| `ToolHandler` | Tool execution | 18 tool types |
| `PlatformAdapter` | Messaging platforms | 16 platforms |
| `TerminalBackend` | Command execution | Local, Docker, SSH, Daytona, Modal, Singularity |
| `MemoryProvider` | Persistent memory | File-based, SQLite |
| `SkillProvider` | Skill management | File store + Hub |

This means you can swap any component without touching the rest. Want to add a new LLM provider? Implement `LlmProvider`. New messaging platform? Implement `PlatformAdapter`. The agent loop doesn't know or care.

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

Every error type converts automatically via `From` traits. The compiler ensures every error path is handled. No more `except Exception: pass`.

### Workspace Structure

```
crates/
├── hermes-core           # Shared types, traits, error types
├── hermes-agent          # Agent loop, providers, context, memory
├── hermes-tools          # Tool registry, dispatch, all tools
├── hermes-gateway        # Message gateway, platform adapters
├── hermes-cli            # CLI binary, TUI, commands
├── hermes-config         # Configuration loading and merging
├── hermes-intelligence   # Prompt building, model routing, usage
├── hermes-skills         # Skill management, store, guard
├── hermes-environments   # Terminal backends
├── hermes-cron           # Cron scheduling
└── hermes-mcp            # Model Context Protocol
```

---

## Competitive Moat

The AI agent space is crowded with Python projects that all hit the same ceiling. The ones that will survive are the ones that can:

1. **Run anywhere** — not just on a developer's MacBook with Python 3.11 and 40 pip packages, but on edge devices, embedded systems, enterprise servers with no internet access, and $3 VPS instances.

2. **Scale to multi-user** — serve a team, a family, or a community from a single process, without each conversation degrading the others.

3. **Stay reliable over months** — no memory leaks, no GC pauses, no silent type errors accumulating in long-running sessions.

4. **Embed into other systems** — a Rust library can be called from C, C++, Python (via PyO3), Node.js (via napi), Go (via CGo), and WASM. A Python agent can only be called from Python.

The Rust rewrite isn't about being faster at LLM API calls (those are I/O bound anyway). It's about building the **infrastructure layer** that makes an AI agent production-grade: deployable, embeddable, reliable, and efficient.

---

## Current Status

Early stage. The architecture and core abstractions are solid. Feature parity with the Python version is ~10%. See [GAP_ANALYSIS.md](./GAP_ANALYSIS.md) for the detailed breakdown.

What works today:
- Agent loop with streaming, interrupt handling, parallel tool execution
- 4 LLM providers (OpenAI, Anthropic, OpenRouter, Generic)
- Tool registry and dispatch framework
- 16 platform adapter structures
- 6 terminal backends
- Skill management with security guard
- Configuration loading and merging
- Session persistence (SQLite)
- Cron scheduling framework
- 524 tests passing

## Building

```bash
cargo build --release
```

## Running

```bash
cargo run --release -p hermes-cli
```

## Testing

```bash
cargo test --workspace
```

## License

MIT — see [LICENSE](LICENSE).

Based on [Hermes Agent](https://github.com/NousResearch/hermes-agent) by [Nous Research](https://nousresearch.com).
