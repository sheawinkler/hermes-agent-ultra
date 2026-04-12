# Hermes Agent (Rust)

A Rust rewrite of [Hermes Agent](https://github.com/NousResearch/hermes-agent) — the self-improving AI agent by [Nous Research](https://nousresearch.com).

## Architecture

The project is organized as a Cargo workspace with 11 crates:

| Crate | Description |
|-------|-------------|
| `hermes-core` | Shared types, traits (`LlmProvider`, `ToolHandler`, `PlatformAdapter`, `TerminalBackend`, `MemoryProvider`), error types, and tool call parser |
| `hermes-agent` | Agent loop engine — orchestrates LLM calls, tool execution, context management, interrupt handling, and session persistence |
| `hermes-tools` | Tool registry, dispatch, toolset management, and all tool implementations (file, web, terminal, browser, memory, etc.) |
| `hermes-gateway` | Message gateway with platform adapters (Telegram, Discord, Slack, WhatsApp, Signal, Matrix, and more) |
| `hermes-cli` | CLI binary with TUI (ratatui), command parsing (clap), and interactive session management |
| `hermes-config` | Configuration loading, merging, and validation (YAML + JSON) |
| `hermes-intelligence` | Prompt building, smart model routing, error classification, usage tracking, and redaction |
| `hermes-skills` | Skill management, file-based skill store, version tracking, and security guard |
| `hermes-environments` | Terminal backends — Local, Docker, SSH, Daytona, Modal, Singularity |
| `hermes-cron` | Cron job scheduling with persistence and delivery |
| `hermes-mcp` | Model Context Protocol client/server with stdio transport |

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

MIT
