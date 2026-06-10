# Web tool budget (gateway harness)

Ultra gateway enhancement: per-run web tool quotas, billable-only accounting, and URL dedup. Not a `registry.json` parity module.

## Scope

- **Per user message** = one `AgentLoop::run` / `run_stream` invocation. Counters reset at the start of each run.
- **Not** a session/thread lifetime cap. A new inbound user message gets fresh pools.
- Distinct from **rate limits** (HTTP/provider throttling) and from **token/cost** budgets.

## Dual-pool semantics (web_research + multi-task)

When `agent.web_research` is enabled and the user message decomposes into multiple `ResearchTask`s:

| Layer | Role |
|-------|------|
| **Task pool (primary)** | Each task has its own `max_search` / `max_extract` from `task_profiles`. Enforced by `apply_task_policy` in [`web_research.rs`](../../crates/hermes-agent/src/web_research.rs). |
| **Message fuse (secondary)** | `apply_web_tool_budget` runs in `BudgetMode::TaskPrimary`: skips per-pool `search_max` / `extract_max` caps and only enforces `max_attempt_total`, optional `aggregate_max`, and `max_consecutive_errors`. |
| **Global pools (no tasks)** | When web_research does not decompose tasks, `BudgetMode::Global` applies full per-tool pools as before. |

Dynamic limits: when tasks are present, `limits.search_max` / `limits.extract_max` = `sum(task.max_*)` clamped to `message_caps.max_total_search` / `max_total_extract` (defaults **10** / **5**), not the small env default alone.

Web tools stop only when all tasks are `Verified` or `Exhausted`, or a message fuse triggers — not when the evaluator is satisfied on one sub-question while others remain pending.

## Environment variables

| Variable | Default | Pool |
|----------|---------|------|
| `HERMES_BROWSER_BUDGET_MAX_CALLS` | `2` | `browser_navigate` |
| `HERMES_WEB_EXTRACT_BUDGET_MAX_CALLS` | `5` | `web_extract` |
| `HERMES_WEB_SEARCH_BUDGET_MAX_CALLS` | `2` | `web_search` per-tool cap in **Global** mode; with multi-task web_research, treat as message ceiling fallback via `message_caps` (not “2 searches per sub-question”) |
| `HERMES_WEB_TOOL_BUDGET_MAX_CALLS` | *(unset)* | Optional **aggregate backstop** on billable successes only; does not replace per-tool pools |
| `HERMES_WEB_TOOL_BUDGET_MAX_ATTEMPTS` | `12` | Hard per-message attempted-call safety cap, including failures |
| `HERMES_WEB_TOOL_BUDGET_MAX_CONSECUTIVE_ERRORS` | `2` | Blocks all web tools after N turns where every web call in the turn was non-billable |

Implementation: [`crates/hermes-agent/src/web_tool_budget.rs`](../../crates/hermes-agent/src/web_tool_budget.rs).

## Billing rules

1. **Pre-check** (`apply_web_tool_budget`): in Global mode, allow if the tool’s successful/billable pool `used < max`, aggregate billable backstop (if set) is not exceeded, and attempted safety cap is not exceeded. In TaskPrimary mode, per-pool search/extract caps are skipped; task policy owns those limits.
2. **Post-execute** (`record_web_tool_results`): increment pool counters only when the result is **not** an error and `is_billable_web_tool_result` (browser timeouts/open failures are not billable).
3. **Query/URL dedup**: same-batch duplicate `web_search` and successful prior search queries are blocked; failed queries get one retry. `web_extract` / `browser_navigate` are blocked when the same normalized URL already has a **successful** `web_extract` in `ctx` messages.

User-facing notices include **「本则用户消息」** so operators know the cap is per message, not the whole chat.

## Gateway defaults

[`gateway_runtime_defaults.rs`](../../crates/hermes-cli/src/gateway_runtime_defaults.rs) no longer forces `HERMES_WEB_TOOL_BUDGET_MAX_CALLS=3`. For multi-intent web_research on gateway hosts, align env with `message_caps` so the fuse does not truncate parallel first-round searches:

```bash
export HERMES_BROWSER_BUDGET_MAX_CALLS=2
export HERMES_WEB_EXTRACT_BUDGET_MAX_CALLS=5
export HERMES_WEB_SEARCH_BUDGET_MAX_CALLS=10
export HERMES_WEB_TOOL_BUDGET_MAX_ATTEMPTS=16
# optional aggregate backstop:
# export HERMES_WEB_TOOL_BUDGET_MAX_CALLS=16
```

Single-intent messages still respect task profiles and planner budgets; raising `HERMES_WEB_SEARCH_BUDGET_MAX_CALLS` mainly prevents the legacy `2` default from blocking N parallel first-round searches when task sums exceed it.

## Verification

```bash
cargo build -p hermes-agent
cargo test -p hermes-agent web_research
cargo test -p hermes-agent web_tool_budget
cargo clippy -p hermes-agent -- -D warnings
```

Manual (gateway, `RUST_LOG=info`):

1. Dual-intent message (e.g. weather + numeric fact): turn 1 parallel `web_search` per task; turn 2+ `site:` / `web_extract` still allowed until tasks terminal.
2. One message: browser timeout + search + extract×2 → **next** user message can still `web_search`.
3. Same URL second `web_extract` → dedup error (use context), not “quota exhausted”.
4. Logs contain `web_research task search recorded` / `task policy block` with `task_id`, `status`, and `web_tool_budget block` with `mode=TaskPrimary` when applicable.

## Phase 3–4: evidence quality and decomposition

| Capability | Behavior |
|------------|----------|
| **Numeric signal** | `has_numeric_signal` requires at least one digit in accepted snippet text |
| **Entity scope** | `matches_entity_scope` requires task `entities` / `focus_text` terms in accepted text |
| **Search-snippet-first** | Default `search_snippet_first: true`. `web_extract` is blocked while `search_attempts < max_search` only for **today-scoped** tasks whose focus text has **no digits** (structural signal, not domain keywords). Other tasks may `web_extract` URLs from search when snippets omit needed figures. |
| **Task typing** | Rule-based decompose uses structural signals: digits/year → `targeted_numeric_fact`; `today` time scope → `realtime_weather`; else `simple_lookup`. Optional `llm_decomposer_enabled` for semantic split. |
| **Browser opt-in** | `browser_navigate` is disabled unless the inbound user message explicitly requests browser automation (e.g. `browser_navigate`, 用浏览器打开). No automatic escalation after `web_extract` failure. |
| **Clause split** | `decompose_research_tasks` splits on `，。；?` so each sub-question gets its own `focus_text` |
| **LLM decomposer** | Set `agent.web_research.llm_decomposer_enabled: true` for auxiliary task split; falls back to rules on failure |
| **Parallel cap** | `max_parallel_web_calls` (default **3**) trims excess parallel web tools in one turn |
| **Profiles** | `targeted_numeric_fact` default **4** search / **2** extract (was 6/3) |

```yaml
agent:
  web_research:
    llm_decomposer_enabled: false
    max_parallel_web_calls: 3
    search_snippet_first: true
```

Chinese queries still benefit from DDGS meta-search (international + sogou/bing_cn) configured in `hermes-tools`; no extra gateway env is required for phase 4.
