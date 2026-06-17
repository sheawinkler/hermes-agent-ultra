# Trading Rust 重写 — TODO 进度

> **更新时间**：2026-06-17  
> **总体状态**：P0 ✅ 已完成，P1 核心增强 ✅（含 Hermes memory/session_search/cron 集成）→ Skills 分工 + trading-debate ✅

---

## ✅ 已完成（P0 — MVP 可 demo）

### 工程基础
- [x] `crates/hermes-trading/` crate 创建 + workspace 配置
- [x] 根 `Cargo.toml` 添加 workspace member + dependency
- [x] `hermes-tools/Cargo.toml` 添加 `trading-research` feature（已包含在 `full` 中）

### 数据层
- [x] `MarketDataProvider` trait 定义
- [x] `BinanceProvider` — Binance REST `/api/v3/klines`（Crypto，无需 Key）
- [x] `EastmoneyProvider` — 东方财富 HTTP API（A 股，无需 Key）
- [x] `AutoRouter` — 根据 symbol 格式自动路由数据源

### 回测引擎
- [x] `BacktestEngine` — 模板策略回测框架
- [x] `sma_cross` 策略 — 金叉/死叉，计算 return/drawdown/sharpe/win_rate
- [x] SMA 指标自实现（无 polars/ta-lib 依赖，零外部大库）

### Tool Handler
- [x] `get_market_data` ToolHandler — 返回 OHLCV JSON
- [x] `run_backtest` ToolHandler — 返回 RunCard JSON
- [x] `register/trading.rs` 注册 + feature gate

### Skill & 测试
- [x] `skills/finance/trading-research/SKILL.md`
- [x] Parity fixture + runner（`trading_market_data/ohlcv.json` + `trading_backtest/sma_cross.json`，`cargo test -p hermes-parity-tests` 通过，MockProvider 隔离网络）
- [x] `hermes-trading` 单元测试 54 通过，`hermes-parity-tests` trading fixtures 通过；Clippy 零警告（touched crates）

### P0 验收
- [x] `cargo build -p hermes-cli` 自动包含 trading tools
- [x] `hermes chat` 中可用自然语言触发拉数据 + 回测

---

## 🔲 未完成（P1 — 增强：小完整研究闭环）

**P1 总体验收目标**：
- [x] A-share / HK / US / crypto 四类市场各至少 1 个 symbol 能成功回测。
- [x] 回测结果持久化为 `~/.hermes/trading/runs/{id}/run_card.json`，并可通过 tool 读取复盘。
- [x] 新增 `rsi_revert` 策略模板。
- [x] A 股 T+1 规则生效（`.SZ`/`.SH` 自动启用）。
- [x] 声明式策略框架：JSON 定义策略 + DSL 规则解析 + 运行时注册表 + create_strategy 工具。
- [x] `cargo test -p hermes-trading` 和 `cargo test -p hermes-parity-tests` trading 全部通过。
- [x] `cargo clippy -p hermes-trading -p hermes-parity-tests -- -D warnings` 通过。

### Tools 增强

#### `get_market_data`
- [x] 支持显式 `source` 参数
  - 验收：`source` 可选值为 `auto|binance|eastmoney`，默认 `auto`。
  - 验收：`source=binance` 时只走 BinanceProvider，`source=eastmoney` 时只走 EastmoneyProvider。
  - 验收：新增/更新 parity fixture 覆盖 `source` 参数。
- [x] 支持 HK / US 市场 symbol 格式（至少设计好路由规则）
  - 验收：`HK_00700` 或 `0700.HK` 格式能识别为待接入状态（可 mock）。
  - 验收：非法 market 返回清晰错误。
- [x] 实现 `TRADING_DATA_CACHE` 磁盘缓存
  - 验收：缓存目录为 `{HERMES_HOME}/trading/cache/`。
  - 验收：缓存 key 格式为 `{source}-{symbol}-{interval}-{start}-{end}.json`。
  - 验收：默认缓存有效期 24h；过期后重新请求网络。
  - 验收：同一请求在缓存有效期内只触发一次网络调用（单测验证）。
  - 验收：缓存可手动清空或绕过（`refresh=true`）。

#### `run_backtest`
- [x] 新增 `rsi_revert` 策略模板
  - 验收：默认参数 `rsi_period=14`, `oversold=30`, `overbought=70`。
  - 验收：在 mock 数据上产生至少 1 笔交易。
  - 验收：parity fixture `trading_backtest/sma_cross.json` 含 `btc_rsi_revert_14`。
- [x] A 股 T+1 规则
  - 验收：当日买入信号不成交，下一交易日开盘价成交。
  - 验收：卖出信号当日可成交（A 股 T+1 只限制买入后当日卖出）。
  - 验收：单测覆盖 T+1 与 T+0 的差异。
- [x] Sharpe 改进
  - 验收：使用日频收益率序列（mark-to-market equity）计算年化 Sharpe。
  - 验收：提供 `risk_free_rate` 参数，默认 0.0。

#### `get_backtest_report`（可选）
- [x] 读取 `~/.hermes/trading/runs/{id}/run_card.json`
  - 验收：`{id}` 支持 UUID 或时间戳格式。
  - 验收：文件不存在时返回清晰错误。
  - 验收：返回 JSON 包含 run_card 全部字段。

#### 数据质量与 API 健壮性
- [x] 网络超时与重试
  - 验收：`reqwest` client 配置连接/读取超时（默认 10s / 30s）。
  - 验收：对 Binance / Eastmoney 请求实现指数退避重试（最多 3 次）。
  - 验收：重试失败后返回明确错误，不返回半成品数据。
- [x] API 限流与降级
  - 验收：Binance 429 时识别 `Retry-After` 并等待。
  - 验收：Eastmoney 返回空数据或 403 时返回 `TradingError::InvalidResponse`。
  - 验收：wiremock 单测覆盖 429 场景。
- [x] 数据缺口处理
  - 验收：节假日/停牌导致某日期无数据时不 panic。
  - 验收：返回数据行数小于请求区间时，在结果中标记 `partial: true`。

### Skills

- [x] 更新 `trading-research` SKILL
  - 验收：When to Use 增加 `rsi_revert` 说明。
  - 验收：增加 T+1 规则说明（A-share 回测默认启用）。
  - 验收：增加 `run_card.json` 保存路径说明。
  - 验收：验证示例 prompt 覆盖 `rsi_revert`。
- [x] 新建 `trading-debate` SKILL
  - 验收：路径 `skills/finance/trading-debate/SKILL.md`。
  - 验收：frontmatter `name: trading-debate`。
  - 验收：使用 `delegate_task` 触发 bull/bear 两个子 agent。
  - 验收：输出格式为 pros/cons 结论摘要。
- [x] 更新 `finance/stocks` SKILL（optional）
  - 验收：现货查价走 `get_quote`；本 skill 仅 search/compare/history。
  - 验收：路径 `optional-skills/finance/stocks/`（非 bundled）。
- [x] WeCom 查价路由（`get_quote` 优先）
  - 验收：gateway finance quote hint + `trading-quote` toolset；`web_search` 仅作失败回退。
  - 验收：`stocks` 降回 optional；现货不再依赖 Python / `terminal`。

### Hermes 能力启用

- [x] 多 agent：投委会（SKILL 文档层）
  - 验收：通过 `delegate_task` 并行触发 bull 和 bear agent。
  - 验收：输入包含 symbol、strategy、run_card 摘要。
  - 验收：输出统一格式（如 `{"bull": "...", "bear": "...", "consensus": "..."}`）。
- [x] 记忆：风险偏好、标的池
  - 验收：使用 `memory` tool 存储用户风险等级（保守/稳健/积极）。
  - 验收：使用 `memory` tool 存储用户关注标的列表。
  - 验收：回测前自动读取风险偏好并提示。
- [x] 历史：`session_search` 上次回测结论
  - 验收：通过 `session_search` 找到最近 7 天内包含 `run_backtest` 的会话。
  - 验收：能提取上次回测的 symbol、strategy、total_return_pct。
- [x] 定时：`cronjob` 收盘复盘
  - 验收：支持 `hermes cron` 配置每日收盘后运行预设 symbol 回测。
  - 验收：输出保存到 `~/.hermes/trading/runs/`。

### 工程

- [x] `run_card.json` 持久化到 `~/.hermes/trading/runs/`
  - 验收：每次 `run_backtest` 成功后将 RunCard 写入 `{id}/run_card.json`。
  - 验收：`id` 生成规则明确（建议使用 `{symbol}-{strategy}-{timestamp}` 或 UUID）。
  - 验收：目录不存在时自动创建。
- [x] 声明式策略框架
  - 验收：策略 JSON Schema 定义和验证（`dsl.rs`）。
  - 验收：规则 DSL 解析器支持 4 种操作符（crosses_above、crosses_below、above、below）。
  - 验收：内置指标库（sma、ema、rsi、macd、bollinger）。
  - 验收：`DeclarativeStrategy` 实现 `Strategy` trait。
  - 验收：`StrategyRegistry` 运行时注册表支持内置策略 + 用户策略加载。
  - 验收：`create_strategy` 工具允许在对话中创建策略。
  - 验收：20 + 36 个单元测试全部通过，clippy 零警告。
  - 验收：`trading_backtest` 集成 StrategyRegistry，支持声明式和硬编码双路径。
- [ ] 库拆分（可选）：`hermes-trading` → `hermes-trading-data` + `hermes-trading-backtest`
  - 验收：如执行拆分，`hermes-tools` 依赖保持不变或更清晰。
  - 验收：拆不拆不影响 P1 总体验收。

---

## 🔲 未完成（P2 — 研究台）

**验收目标**：基准对比、券商 CSV 行为分析、因子 IC 子集、MCP 对外暴露。

### 新增 Tools
- [ ] `compare_benchmark` — 策略 vs SPY/CSI300
- [ ] `analyze_trade_journal` — 券商 CSV → 行为统计
- [ ] `run_factor_ic` — 风格因子 IC 子集（toraniko + factors）
- [x] `get_quote` — 轻量现价查询（Rust `hermes-trading`，`trading-quote` toolset，gateway hint）
- [ ] `trading_account_read` — 券商只读账户/持仓（alpacars / ibapi）

### 新增 Skills
- [ ] `trading-journal` — CSV 路径 + analyze_trade_journal
- [ ] `trading-factor` — 何时 run_factor_ic；IC 局限
- [x] `trading-cron` — cronjob 收盘/周报配方
- [ ] 更新 `trading-research` — benchmark、因子、只读账户

### MCP & 离线
- [ ] `hermes mcp serve` 对外暴露 5–8 个 trading tools
- [ ] `hermes-mcp` client 连接 Robinhood MCP
- [ ] `rustdx` 离线 Parquet 导入

---

## 🚧 阻塞项（用户场景，非开发 workaround）

- [x] **内置 Skills 未 sync 到用户 home** — layered runtime + release 打包；见 [`docs/issues/2026-06-17-bundled-skills-never-sync.md`](../issues/2026-06-17-bundled-skills-never-sync.md)

---

## 🔲 未完成（P3+ / Backlog）

### Tools 候选
- [ ] `export_pine` — TradingView Pine Script 导出
- [ ] `walk_forward_validate` — 防过拟合
- [ ] `portfolio_backtest` — 多标的组合回测
- [ ] `correlation_matrix` — 组合相关性
- [ ] `options_price` — 期权理论价（RustQuant）
- [ ] `trading_place_order` — 下单（需 mandate 栈）

### Skills 候选
- [ ] `trading-shadow` — Shadow Account HTML 报告
- [ ] `trading-options` — 期权研究
- [ ] `trading-crypto-perp` — 永续合约/资金费率

---

## ❌ 明确不做（0py 路线图外）

- Alpha Zoo 452 全量 port
- Python 用户策略 `import`
- futu / baostock 官方 SDK
- PDF Shadow（WeasyPrint）
- `read_document` 全格式
- smartmoneyconcepts / pyharmonics
- Vibe React UI 全量重写
- AKTools / Python akshare / mootdx 依赖
- 挂 akshare-mcp 42 tools

---

## 关键文件索引

| 模块 | 路径 |
|------|------|
| Trading 库 | `crates/hermes-trading/src/` |
| 数据提供者 | `crates/hermes-trading/src/providers/` |
| 回测引擎 | `crates/hermes-trading/src/backtest.rs` |
| 策略框架 | `crates/hermes-strategies/src/` |
| 策略 DSL | `crates/hermes-strategies/src/dsl.rs` |
| 策略实现 | `crates/hermes-strategies/src/declarative.rs` |
| 内置策略 | `crates/hermes-strategies/src/builtin.rs` |
| 策略注册表 | `crates/hermes-strategies/src/registry.rs` |
| Tool Handler | `crates/hermes-tools/src/tools/trading_market_data.rs` |
| Tool Handler | `crates/hermes-tools/src/tools/trading_backtest.rs` |
| Tool Handler | `crates/hermes-tools/src/tools/trading_create_strategy.rs` |
| Tool Handler | `crates/hermes-tools/src/tools/trading_strategies.rs` |
| 注册 | `crates/hermes-tools/src/register/trading.rs` |
| Skill | `skills/finance/trading-research/SKILL.md` |
| Skill | `skills/finance/trading-debate/SKILL.md` |
| Skill | `skills/finance/trading-cron/SKILL.md` |
| Skill | `optional-skills/finance/stocks/SKILL.md` |
| Parity | `crates/hermes-parity-tests/fixtures/trading_*/` |
| 路线图 | `docs/roadmaps/VIBE_TRADING_RUST_REWRITE.md` |
