---
name: equity-research
description: "A-share equity research: DCF, 19-dim scoring, 66-investor panel via analyze_stock. Slash: /equity-research <name|symbol>"
version: 0.1.0
author: Hermes Agent
license: MIT
platforms: [linux, macos, windows]
metadata:
  hermes:
    tags: [Finance, Equity, Research, DCF, Valuation, A-Share]
    category: finance
    related_skills: [trading-research, spot-quote, dcf-model, comps-analysis]
    requires_toolsets: [trading, web]
---

# Equity Research Skill

Pure Rust institutional-style equity research — DCF, comps, 19-dimension scoring,
and 66-investor persona panel. **No Python runtime.**

Complements optional `dcf-model` (Excel) and `trading-research` (OHLCV/backtest).

## When to Use

- User wants **fundamentals, valuation, DCF, investment merit, or peer comparison** on a listed stock (name or code)
- User invokes **`/equity-research`** (optional args: stock name or symbol)
- User wants structured JSON with `data_confidence`, `used_fallback`, persona votes
- A-share pipeline (600519.SH, 000001.SZ, etc.)

## Agent Workflow

No gateway keyword routing — decide from **intent** (valuation vs spot price vs backtest):

1. If the request is **fundamental/valuation research**, call `skill_view(name="equity-research")` when unsure of the workflow.
2. **`resolve_a_share_symbol`** when the user gives a Chinese name or bare 6-digit code.
3. **`analyze_stock(symbol, use_providers=true)`** — always **before** `web_search` for research requests.
4. **`web_search`** only after `analyze_stock`, when `data_confidence` flags gaps.
5. If user typed **`/equity-research …`**, treat the skill as loaded and run the workflow below.

## When NOT to Use

- User wants **only spot price** → `get_quote` + `spot-quote`
- User wants **K-line backtest** → `trading-research`
- User wants **Excel DCF workbook** → optional `dcf-model` skill
- User wants **news only** → `web_search`

## Slash command

- **`/equity-research 山西汾酒`** — loads this skill and runs the workflow on the given stock
- **`/equity-research 600809.SH`** — same, with explicit symbol

## Workflow (mandatory order)

**Symbol format:** A-shares use `.SH` / `.SZ` (e.g. `600519.SH`). Do **not** use Yahoo suffix `.SS` — Hermes normalizes it, but prefer `.SH` in tool calls.

1. **`resolve_a_share_symbol(query)`** — when user gives Chinese name (e.g. `牧原股份`, `山西汾酒`), resolve to canonical symbol
2. **`analyze_stock(symbol, use_providers=true)`** — **next** (before web). Runs 22-dim HTTP fetchers + DCF/scoring/panel; returns `raw_dims`, `data_confidence`, `used_fallback`
   - Only pass manual `fundamentals` / `peers` when providers failed or user supplied research notes
3. **`get_quote(symbol)`** — optional spot check; not a substitute for `analyze_stock` on research requests
4. **`web_search`** — **after** `analyze_stock`, when `data_confidence.score < 0.5` OR `missing_dims` includes macro/policy/moat/chain:
   - supplement revenue, FCF, debt, ROE, peers, industry, policy headlines
   - Chinese queries via bing_cn may return unrelated results ("贵州" tourism when searching for Moutai). Use English queries like `"Kweichow Moutai 600519 market cap"` for financial data.
5. **LLM narrative** — after pasting **`summary_markdown`** from `analyze_stock` (full 19 dims + 66 judges; do not shorten to 9 rows), add conclusion citing:
   - `data_confidence.score` and `missing_dims`
   - `used_fallback` (never hide proxy/Fallback paths)
   - DCF `verdict` + persona `panel_consensus`
6. **Optional `format: "html"`** + `narrative` for one-page readable report (DCF sensitivity, 19-dim scores, gauges)

### Eastmoney API fallback

Tool layer (`get_quote`, `analyze_stock` basic/kline/financials dims) tries **akshare-rs → push2 → Tencent qt** automatically.

If both fail (push2.eastmoney.com unreachable):

1. **`get_market_data(symbol, source="eastmoney")`** — uses push2his endpoint, often works when quote endpoint is blocked. Latest `close` ≈ current price proxy.
2. **Web-extract financial pages** — search English: `"600519.SS stock price"`, `"Kweichow Moutai market cap"`. Check snippets from Investing.com, SimplyWallSt, companiesmarketcap.com, Yahoo Finance.
3. **Extract price from Sina snippet** — search `"贵州茅台" "最新价格"` and check snippet for today's price e.g. `"贵州茅台 1240.00 (-1.25%)"`.
4. **Manually estimate PE** from web-marketcap / web-earnings. Market cap from companiesmarketcap.com, net income from tradingeconomics.com.
5. **Deliver with data-availability warning** — label non-real-time data as estimated. Never claim institutional-grade when live quote was unavailable.

### Rules

- If `data_confidence.score < 0.5`: **do not** claim "institutional-grade" — say data is partial; run `web_search` for gaps before final narrative
- Always surface `used_fallback` fields in the user-facing summary
- Persona **commentary** is LLM-generated; Rust output is `{id, vote, score, cited_rule}` for all **66** investors in `personas.investors`
- **`summary_markdown`** in tool JSON is the canonical chat table — paste verbatim before your narrative
- `use_providers` defaults **true**; set `false` only for quote-only smoke tests
- Prefer `format: "html"` + `narrative` when user asks for 研报 / readable report

## Example

```json
analyze_stock({
  "symbol": "600519.SH",
  "fundamentals": {
    "revenue_latest_yi": 1500,
    "fcf_latest_yi": 600,
    "net_margin": 52,
    "market_cap_yi": 21000,
    "shares_outstanding_yi": 12.56,
    "total_debt_yi": 30,
    "cash_yi": 1500,
    "roe_latest": 30,
    "moat_total": 35
  },
  "peers": [
    {"name": "五粮液", "pe": 18, "pb": 4.2},
    {"name": "泸州老窖", "pe": 16, "pb": 3.8}
  ]
})
```

## Toolsets

- **`trading`** — `resolve_a_share_symbol`, `get_quote`, `analyze_stock`
- **`web`** — `web_search` for fundamentals gap-fill (macro, policy, moat when not in `raw_dims`)
