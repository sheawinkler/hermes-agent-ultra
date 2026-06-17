---
name: trading-cron
description: Schedule recurring A-share/crypto backtests and close-of-day reviews via cronjob.
version: 0.1.0
author: Hermes Agent
license: MIT
platforms: [linux, macos, windows]
metadata:
  hermes:
    tags: [Finance, Cron, Backtest, Trading, Scheduled]
    category: finance
    related_skills: [trading-research]
    requires_toolsets: [cronjob, trading, skills]
---

# Trading Cron Skill

Schedule **autonomous** backtest reviews with the built-in `cronjob` tool. Cron sessions
run in a **fresh context** — prompts must be self-contained (include symbols, strategies,
and where to read watchlists).

No new tools — orchestration only (`cronjob` + `trading-research` tools on each tick).

## When to Use

- User asks for **daily / weekly close review**, **定时回测**, **收盘复盘**, or **cron 跑策略**
- User wants recurring `run_backtest` on a fixed schedule
- User asks to **list / pause / remove** an existing trading cron job

## When NOT to Use

- One-off backtest now → **`trading-research`** `run_backtest` directly
- US/HK historical backtest → not supported; cron cannot fix missing OHLCV
- Apple Reminders / phone notifications → `apple-reminders` skill
- Simple "remind me in 5 minutes" → `cronjob` with relative `5m` (any skill)

## Prerequisites

- Parent toolsets: **`cronjob`**, **`trading`**, **`skills`**
- Build with `trading-research` feature
- Cron `task` must name symbols explicitly OR instruct the agent to read
  `Trading watchlist:` from injected MEMORY.md

## Workflow

1. **Clarify** symbols (A-share `.SZ`/`.SH` or crypto `XXX-YYY`), strategy (`sma_cross` / `rsi_revert`), and wall-clock time.
2. **Optional** — persist watchlist via `memory` (see **`trading-research`**):
   `memory(action="add", target="memory", content="Trading watchlist: 000001.SZ, BTC-USDT")`
3. **`cronjob` create** — self-contained `task` + `skills: ["trading-research"]`.
4. Confirm `next_run_display` from the tool response to the user.
5. **Manage** — `action="list"` before `remove` / `pause` / `update` (never guess job ids).

## Schedule recipes

| Market | User intent | Schedule hint |
|--------|-------------|---------------|
| A-share close | 工作日收盘后复盘 | `0 7 * * 1-5` (15:00 Asia/Shanghai ≈ 07:00 UTC) — adjust for your `HERMES_TIMEZONE` |
| Crypto daily | 每天 UTC 0 点 | `0 0 * * *` or `every 24h` |
| Weekly summary | 每周一开盘前 | `0 1 * * 1` |

Prefer **ISO timestamps with explicit offset** when the user gives an absolute local time.
For "every weekday at 3pm local", convert to UTC cron or ISO — do not guess from chat history.

## Create template (A-share close review)

```json
{
  "action": "create",
  "name": "a-share-close-sma",
  "schedule": "0 7 * * 1-5",
  "skills": ["trading-research"],
  "enabled_toolsets": ["trading", "memory", "session_search", "skills"],
  "task": "Close-of-day review. For each symbol in MEMORY.md line 'Trading watchlist:' (default 000001.SZ if missing), call run_backtest with strategy sma_cross and params {\"short_window\":20,\"long_window\":50}. Report each RunCard id, symbol, total_return_pct, max_drawdown_pct, sharpe_ratio. Results persist under ~/.hermes/trading/runs/. Do not schedule new cron jobs."
}
```

## Create template (crypto daily RSI)

```json
{
  "action": "create",
  "name": "btc-daily-rsi",
  "schedule": "0 0 * * *",
  "skills": ["trading-research"],
  "enabled_toolsets": ["trading", "skills"],
  "task": "Daily crypto review: run_backtest symbol BTC-USDT strategy rsi_revert. Summarize total_return_pct, max_drawdown_pct, trade_count, and run id. If total_return_pct < -5, note elevated drawdown in the opening line."
}
```

## CLI equivalent

Users may also manage jobs via `hermes cron list|add|edit` — same on-disk store as `cronjob`.
Prefer `cronjob` tool when operating inside an agent turn.

## Critical rules

- Cron `task` must be **self-contained** (no "same as above" / "the symbols we discussed").
- Set `enabled_toolsets` to include **`trading`** so `run_backtest` is available in the cron session.
- Attach `skills: ["trading-research"]` so the cron agent follows backtest conventions.
- **Never** nest cron creation inside a cron-run session.
- Quote `next_run_display` exactly when telling the user when the job fires.

## Verification

Ask: "每个交易日收盘后帮我回测 000001.SZ 的 SMA 策略"
Expected: `cronjob` `action=create` with weekday schedule, `skills=["trading-research"]`,
`enabled_toolsets` including `trading`, task names symbol + strategy explicitly.

Ask: "取消定时回测"
Expected: `cronjob` `action=list`, then `action=remove` with the matched job id.
