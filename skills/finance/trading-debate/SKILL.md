---
name: trading-debate
description: Investment-committee bull/bear debate after a backtest run_card.
version: 0.1.0
author: Hermes Agent
license: MIT
platforms: [linux, macos, windows]
metadata:
  hermes:
    tags: [Finance, Debate, Bull-Bear, Trading]
    category: finance
    related_skills: [trading-research, stocks]
    requires_toolsets: [delegation, trading, web, skills]
---

# Trading Debate Skill

Structured **investment-committee** workflow: after a backtest RunCard exists, spawn
parallel bull and bear sub-agents via `delegate_task`, then synthesize a pros/cons verdict.

No new tools — orchestration only (`delegate_task`, optional `web_search`, `get_backtest_report`).

## When to Use

- User asks for **bull/bear debate**, **投委会**, **多空分析**, or **该不该买/卖**
- A **`run_backtest` RunCard** already exists (or `get_backtest_report` can load one)
- User wants a structured verdict beyond raw backtest metrics

## When NOT to Use

- No backtest yet → run **`trading-research`** `run_backtest` first
- User only wants **news** → `web_search`
- User only wants **spot price** → **`stocks`** skill (`quote`) or `web_search`
- User wants to **place orders** → not supported

## Prerequisites

- Parent agent toolsets: **`delegation`**, **`trading`**, **`web`**, **`skills`**
- Build with `trading-research` feature so `get_market_data` / `run_backtest` / `get_backtest_report` are registered
- `delegate_task` depth: parent must be below `max_depth` (default allows one child level)

## Workflow (fixed order)

1. **Obtain RunCard** — if missing:
   - `run_backtest` with user's symbol/strategy, or
   - `get_backtest_report` with a known `id`
2. **Optional context** (parent agent): `web_search` for 1–2 recent headlines / macro notes
3. **Parallel debate** — in the **same assistant turn**, issue **two** `delegate_task` calls:
   - Bull sub-agent (long thesis)
   - Bear sub-agent (risks / avoid thesis)
4. **Synthesize** — parent agent merges sub-agent results into **DebateSummary JSON** (schema below)

### Parallel `delegate_task` (important)

Rust `delegate_task` accepts a single `task` string per call — **not** a `tasks=[...]` batch.
Issue **two separate** `delegate_task` tool calls in one turn; the runtime executes them concurrently.

Sub-agents inherit the parent's tool surface. Ensure parent has `trading` + `web` enabled.
Do **not** rely on `toolset: "trading,web"` (only one toolset string is supported).

## Bull sub-agent template

```
delegate_task({
  "task": "You are the BULL analyst on an investment committee. Argue FOR taking a long position based ONLY on the provided run_card and context. Cite specific metrics (total_return_pct, max_drawdown_pct, sharpe_ratio, trade_count). List 3-5 bullish evidence bullets and acknowledge 1-2 risks you accept.",
  "context": "<paste run_card JSON or summary: symbol, strategy, params, total_return_pct, max_drawdown_pct, sharpe_ratio, trade_count, period, id>\\n<optional web_search headlines>"
})
```

**Rules for bull sub-agent:** Do not invent numbers. Do not call `delegate_task` again.

## Bear sub-agent template

```
delegate_task({
  "task": "You are the BEAR analyst on an investment committee. Argue AGAINST or for CAUTION based ONLY on the provided run_card and context. Stress max_drawdown, overfitting, sample period, trade_count, and data limitations (US/HK OHLCV not supported for backtest). List 3-5 bearish evidence bullets.",
  "context": "<same run_card summary as bull>\\n<optional web_search headlines>"
})
```

**Rules for bear sub-agent:** Do not invent numbers. Do not call `delegate_task` again.

## DebateSummary output (parent agent MUST produce)

After both sub-agents return, the parent agent outputs this JSON shape:

```json
{
  "symbol": "0700.HK",
  "strategy": "sma_cross",
  "run_card_id": "uuid-or-timestamp-id",
  "bull": {
    "thesis": "One-sentence bull case",
    "evidence": ["metric or fact 1", "metric or fact 2"],
    "risks_acknowledged": ["risk bull accepts"]
  },
  "bear": {
    "thesis": "One-sentence bear case",
    "evidence": ["risk or weakness 1", "risk or weakness 2"],
    "risks_acknowledged": ["what could invalidate the bear view"]
  },
  "consensus": "neutral",
  "summary": "2-4 sentence balanced conclusion in the user's language",
  "disclaimer": "Not investment advice. HK/US may use stub mock data."
}
```

`consensus` must be one of: `"bullish"`, `"bearish"`, `"neutral"`.

Optionally `write_file` the summary to `~/.hermes/trading/runs/{id}/debate_summary.json`.

## Critical Rules

- **NEVER fabricate run_card metrics** — context passed to sub-agents must come from real tool output
- Sub-agents must **not** nest `delegate_task` (depth limit)
- If one sub-agent fails, report **partial debate** with the error; do not invent the missing side
- Debate complements backtest — it does not replace `run_backtest`

## Relationship with other skills

| Step | Skill |
|------|-------|
| Backtest | `trading-research` |
| Debate (this skill) | `trading-debate` |
| Spot quote during debate | `stocks` or `web_search` |

## Verification

Ask: "回测 AAPL sma_cross 后开投委会辩论"
Expected: `run_backtest` → two `delegate_task` (bull + bear) in one turn → DebateSummary JSON.

Ask: "基于上次 run_card 多空分析 0700.HK"
Expected: `get_backtest_report` → two `delegate_task` → DebateSummary JSON with stub-data disclaimer.
