# MCP Runtime

Hermes Agent Ultra supports MCP in both directions:

- MCP client mode: connect Hermes Ultra to external MCP servers and expose their tools to the runtime.
- MCP server mode: run `hermes-ultra mcp serve` so another MCP client can call Hermes Ultra tools over stdio.

The implementation is Rust-native in `crates/hermes-mcp` and is surfaced through `hermes-ultra mcp ...` commands.

## Client Configuration

Add MCP servers with the CLI:

```bash
hermes-ultra mcp add filesystem --command npx --parallel-tools
hermes-ultra mcp add remote-api --url https://example.com/mcp
hermes-ultra mcp list
hermes-ultra mcp test remote-api
```

The CLI keeps `mcp_servers.json` and `config.yaml` synchronized under the Hermes home directory.

Equivalent JSON shape:

```json
{
  "filesystem": {
    "command": "npx",
    "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
    "enabled": true,
    "supports_parallel_tool_calls": true
  },
  "remote-api": {
    "url": "https://example.com/mcp",
    "enabled": true
  }
}
```

Validation rules:

- Every server must have either `url` or `command`.
- `url` must use `http` or `https`.
- If both `url` and `command` are present, HTTP wins and the CLI prints a warning.
- `supports_parallel_tool_calls` is preserved and displayed by `mcp list` and `mcp test`.

## Runtime Behavior

`crates/hermes-mcp` provides:

- stdio and HTTP/SSE transports.
- `initialize`, `tools/list`, `tools/call`, `resources/list`, `resources/read`, `prompts/list`, and `prompts/get` client methods.
- Parallel connection/discovery reporting through `McpManager::connect_all_parallel`.
- Stale transport detection with one reconnect attempt on tool calls.
- Bearer/OAuth authentication providers for remote servers.
- Media block caching for image tool responses.

Parallel discovery gives each configured server an independent connection future, so one slow or broken server does not consume the entire startup budget for other servers.

## Server Mode

Run Hermes Ultra as an MCP stdio server:

```bash
hermes-ultra mcp serve
```

Example MCP client config:

```json
{
  "mcpServers": {
    "hermes-ultra": {
      "command": "hermes-ultra",
      "args": ["mcp", "serve"]
    }
  }
}
```

The server exposes Hermes Ultra tool definitions through MCP `tools/list` and runs tool calls through the same Rust tool registry used by the agent runtime.

## Sentrux Profile

Hermes Ultra includes a convenience profile for the Sentrux MCP backend:

```bash
hermes-ultra mcp sentrux
hermes-ultra mcp sentrux-status
hermes-ultra mcp sentrux-remove
```

This configures the Sentrux MCP command in both JSON and YAML config surfaces and marks it as safe for parallel tool calls.
