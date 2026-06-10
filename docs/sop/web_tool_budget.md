# Web tool budget (gateway harness)

Ultra gateway enhancement: per-run web tool quotas, billable-only accounting, and URL dedup. Not a `registry.json` parity module.

## Scope

- **Per user message** = one `AgentLoop::run` / `run_stream` invocation. Counters reset at the start of each run.
- **Not** a session/thread lifetime cap. A new inbound user message gets fresh pools.
- Distinct from **rate limits** (HTTP/provider throttling) and from **token/cost** budgets.

## Environment variables

| Variable | Default | Pool |
|----------|---------|------|
| `HERMES_BROWSER_BUDGET_MAX_CALLS` | `2` | `browser_navigate` |
| `HERMES_WEB_EXTRACT_BUDGET_MAX_CALLS` | `5` | `web_extract` |
| `HERMES_WEB_SEARCH_BUDGET_MAX_CALLS` | `2` | `web_search` |
| `HERMES_WEB_TOOL_BUDGET_MAX_CALLS` | *(unset)* | Optional **aggregate backstop** on billable successes only; does not replace per-tool pools |
| `HERMES_WEB_TOOL_BUDGET_MAX_ATTEMPTS` | `12` | Hard per-message attempted-call safety cap, including failures |
| `HERMES_WEB_TOOL_BUDGET_MAX_CONSECUTIVE_ERRORS` | `2` | Blocks all web tools after N turns where every web call in the turn was non-billable |

Implementation: [`crates/hermes-agent/src/web_tool_budget.rs`](../../crates/hermes-agent/src/web_tool_budget.rs).

## Billing rules

1. **Pre-check** (`apply_web_tool_budget`): allow if the tool’s successful/billable pool `used < max`, aggregate billable backstop (if set) is not exceeded, and attempted safety cap is not exceeded.
2. **Post-execute** (`record_web_tool_results`): increment pool counters only when the result is **not** an error and `is_billable_web_tool_result` (browser timeouts/open failures are not billable).
3. **Query/URL dedup**: same-batch duplicate `web_search` and successful prior search queries are blocked; failed queries get one retry. `web_extract` / `browser_navigate` are blocked when the same normalized URL already has a **successful** `web_extract` in `ctx` messages.

User-facing notices include **「本则用户消息」** so operators know the cap is per message, not the whole chat.

## Gateway defaults

[`gateway_runtime_defaults.rs`](../../crates/hermes-cli/src/gateway_runtime_defaults.rs) no longer forces `HERMES_WEB_TOOL_BUDGET_MAX_CALLS=3`. Tighten via env on the gateway host if needed, for example:

```bash
export HERMES_BROWSER_BUDGET_MAX_CALLS=2
export HERMES_WEB_EXTRACT_BUDGET_MAX_CALLS=5
export HERMES_WEB_SEARCH_BUDGET_MAX_CALLS=2
export HERMES_WEB_TOOL_BUDGET_MAX_ATTEMPTS=12
# optional backstop:
# export HERMES_WEB_TOOL_BUDGET_MAX_CALLS=8
```

## Verification

```bash
cargo build -p hermes-agent
cargo test -p hermes-agent web_tool_budget
cargo clippy -p hermes-agent -- -D warnings
```

Manual (gateway, `RUST_LOG=info`):

1. One message: browser timeout + search + extract×2 → **next** user message can still `web_search`.
2. Same URL second `web_extract` → dedup error (use context), not “quota exhausted”.
3. Logs contain `web_tool_budget block` / `dedup block` with `scope=run`.
