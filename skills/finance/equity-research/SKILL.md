---
name: equity-research
description: "A-share stock analysis (分析一下/值不值得买/深度研报). DCF + scoring + 66-investor panel via analyze_stock. Slash: /equity-research 山西汾酒"
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

- User asks to **分析 / 分析一下 / 值不值得买 / 怎么样** a stock (by name or code)
- User asks for **深度分析 / 研报 / DCF 估值 / 投资委员会** on a stock
- User invokes **`/equity-research`** (optional args: stock name or symbol)
- User wants structured JSON with `data_confidence`, `used_fallback`, persona votes
- A-share fundamentals + valuation pipeline (600519.SH, 000001.SZ, etc.)

## Agent Workflow

No gateway keyword routing — **you** decide whether this skill applies:

1. Read `<available_skills>` or call `skills_list` when the user asks to **analyze** a stock.
2. If the request is **fundamental/valuation research** (not spot price, not backtest), call `skill_view(name="equity-research")` and follow this skill.
3. Resolve Chinese name → symbol when needed (`山西汾酒` → `600809.SH`) via `get_quote` or `web_search`, then **`analyze_stock`**.
4. If user typed **`/equity-research …`**, treat the skill as already loaded and execute the workflow below.

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

1. **`get_quote(symbol)`** — live price + PE (A-share via Eastmoney push2, Tencent qt fallback)
   - If Eastmoney quote API fails → note as blocked, continue to fallback section below
2. **`web_search`** — supplement fundamentals if `data_confidence` will be low:
   revenue, FCF, debt, ROE, peers, industry
   - Chinese queries via bing_cn may return unrelated results ("贵州" tourism when searching for Moutai). Use English queries like `"Kweichow Moutai 600519 market cap"` for financial data.
3. **`analyze_stock`** — pass enriched `fundamentals` JSON + optional `peers` array
4. **LLM narrative** — write conclusion citing:
   - `data_confidence.score` and `missing_dims`
   - `used_fallback` (never hide proxy/Fallback paths)
   - DCF `verdict` + persona `panel_consensus`

### Eastmoney API fallback

Tool layer (`get_quote`, `analyze_stock` basic dim) tries **push2 → Tencent qt** automatically.

If both fail (push2.eastmoney.com unreachable):

1. **`get_market_data(symbol, source="eastmoney")`** — uses push2his endpoint, often works when quote endpoint is blocked. Latest `close` ≈ current price proxy.
2. **Web-extract financial pages** — search English: `"600519.SS stock price"`, `"Kweichow Moutai market cap"`. Check snippets from Investing.com, SimplyWallSt, companiesmarketcap.com, Yahoo Finance.
3. **Extract price from Sina snippet** — search `"贵州茅台" "最新价格"` and check snippet for today's price e.g. `"贵州茅台 1240.00 (-1.25%)"`.
4. **Manually estimate PE** from web-marketcap / web-earnings. Market cap from companiesmarketcap.com, net income from tradingeconomics.com.
5. **Deliver with data-availability warning** — label non-real-time data as estimated. Never claim institutional-grade when live quote was unavailable.

### Rules

- If `data_confidence.score < 0.5`: **do not** claim "institutional-grade" — say data is partial
- Always surface `used_fallback` fields in the user-facing summary
- Persona **commentary** is LLM-generated; Rust output is `{id, vote, score, cited_rule}` only
- Optional: `use_providers: true` for A-share hard data (Eastmoney financials/valuation/LHB)
- Optional: `format: "html"` + `narrative` for readable report

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

- **`trading`** — `get_quote`, `analyze_stock`
- **`web`** — `web_search` for fundamentals gap-fill
