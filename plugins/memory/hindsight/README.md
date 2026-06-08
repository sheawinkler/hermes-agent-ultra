# Hindsight Memory Provider

Rust-native long-term memory integration for Hindsight, with retain, recall, and
reflect support over the Hindsight HTTP API.

Hermes Agent Ultra prefers ContextLattice as the primary memory backbone.
Hindsight is an optional provider for deployments that explicitly choose a
Hindsight cloud or self-hosted memory bank.

## Supported Modes

- **Cloud:** Hindsight Cloud API. Requires `HINDSIGHT_API_KEY`.
- **Local External:** A running Hindsight-compatible service reachable over HTTP,
  such as a Docker or self-hosted instance.

The Rust runtime does not launch or manage a Python embedded Hindsight daemon.
Legacy `local` and `local_embedded` config values are accepted for compatibility
and normalized to `local_external`; provide `api_url` or `HINDSIGHT_API_URL` for
that service.

## Setup

```bash
hermes memory setup
```

Manual cloud configuration:

```bash
hermes config set memory.provider hindsight
echo "HINDSIGHT_API_KEY=your-key" >> ~/.hermes-agent-ultra/.env
```

Manual local-external configuration:

```json
{
  "mode": "local_external",
  "api_url": "http://localhost:8888",
  "bank_id": "hermes"
}
```

## Config

Config file: `$HERMES_HOME/hindsight/config.json`; by default this is
`~/.hermes-agent-ultra/hindsight/config.json`.

### Connection

| Key | Default | Description |
|-----|---------|-------------|
| `mode` | `cloud` | `cloud` or `local_external`; legacy `local`/`local_embedded` values normalize to `local_external` |
| `api_url` | cloud API URL | Hindsight HTTP API endpoint |
| `api_key` | env | Optional API key; `apiKey` is also accepted for compatibility |
| `hindsight_timeout` | `120` | HTTP timeout in seconds; `timeout` and `HINDSIGHT_TIMEOUT` are also accepted |

### Memory Bank

| Key | Default | Description |
|-----|---------|-------------|
| `bank_id` | `hermes` | Static memory bank fallback |
| `bank_id_template` | empty | Optional dynamic bank template with `{profile}`, `{workspace}`, `{platform}`, `{user}`, and `{session}` placeholders |

### Recall

| Key | Default | Description |
|-----|---------|-------------|
| `recall_budget` | `mid` | Recall thoroughness: `low`, `mid`, or `high` |
| `recall_prefetch_method` | `recall` | Auto-prefetch method: `recall` or `reflect` |
| `recall_max_tokens` | `4096` | Maximum tokens requested for recall results |
| `recall_max_input_chars` | `800` | Maximum input query length for auto-recall |
| `recall_prompt_preamble` | empty | Custom preamble for recalled memories injected into context |
| `recall_types` | `observation` | Fact types surfaced by recall; comma-separated string or JSON list |
| `auto_recall` | `true` | Automatically recall memories before each turn |

### Retain

| Key | Default | Description |
|-----|---------|-------------|
| `auto_retain` | `true` | Automatically retain conversation turns |
| `retain_async` | `true` | Process retain asynchronously on the Hindsight server |
| `retain_every_n_turns` | `1` | Retain every N turns |
| `retain_context` | `conversation between Hermes Agent and the User` | Context label for retained memories |

Automatically retained turns are serialized as structured JSON and sent with a
per-initialization `document_id` so resumed sessions append to a fresh process
document instead of overwriting prior retained content.

### Integration

| Key | Default | Description |
|-----|---------|-------------|
| `memory_mode` | `hybrid` | `hybrid`, `context`, or `tools` |

`hybrid` injects automatic recall context and exposes tools. `context` injects
automatic recall context without tools. `tools` exposes tools without automatic
context injection.

## Tools

Available in `hybrid` and `tools` memory modes:

| Tool | Description |
|------|-------------|
| `hindsight_retain` | Store a memory with optional `context` |
| `hindsight_recall` | Multi-strategy memory search |
| `hindsight_reflect` | Cross-memory synthesis |

## Environment Variables

| Variable | Description |
|----------|-------------|
| `HINDSIGHT_API_KEY` | API key for Hindsight Cloud or protected local services |
| `HINDSIGHT_API_URL` | Override API endpoint |
| `HINDSIGHT_BANK_ID` | Override bank name |
| `HINDSIGHT_BANK_ID_TEMPLATE` | Dynamic bank template |
| `HINDSIGHT_BUDGET` | Override recall budget |
| `HINDSIGHT_MODE` | Override mode (`cloud` or `local_external`; legacy local values normalize to `local_external`) |
| `HINDSIGHT_TIMEOUT` | HTTP timeout in seconds |
