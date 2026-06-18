# UZI-Skill → Rust 重写 · 股票研报引擎迁移 TODO

> **更新时间**：2026-06-18
> **总体状态**：P0a/b/c 已完成；P1 取数 **部分完成**（HTTP transport 层 P1a 进行中，见 [`docs/sop/equity_research_data.md`](docs/sop/equity_research_data.md)）
> **目标**：把 UZI-Skill(`wbh604/UZI-Skill`)的 **确定性分析大脑 + 结构化研报** 用纯 Rust 重写进 `hermes-trading`，**不引入 Python 运行时**。
> **不做**：装 Python 包 / Playwright / akshare 作运行时依赖。

---

## 0. 背景与定位

UZI-Skill = `SKILL.md`(LLM playbook) + 一套 Python `scripts/`(22 维 fetcher → 估值模型 → 评分 → 渲染)。
它的**价值主张**是 `输入 symbol → 600KB 机构级研报`，但实现上把"计算"和"取数"混在 Python 里。

Rust 化的价值不是再写一套 Excel 技能，而是：**同一份输入 → 同一份输出（可测、可缓存、可复现）**，与 hermes parity 哲学对齐，与现有 `optional-skills/finance/dcf-model`(LLM+Excel)互补、不重复。

### 与 hermes 现状的对齐（已核实）
- `hermes-trading/src/lib.rs:7` 明确 **0py 约束**（无 Python/PyO3/subprocess）。
- `EastmoneyQuoteProvider` / `eastmoney_http` — push2 **字段映射**已验证；**生产 transport** 需 UA/`ut`/Referer + 腾讯 qt fallback（见 `docs/sop/equity_research_data.md`）。
- `get_quote` / `get_market_data` / `run_backtest` 已在 `TOOLSET_TRADING` 里 → 数据可软喂。
- 落地位置：**`hermes-trading/src/research/` 子模块**，不新建 crate（YAGNI；边界稳 / >3k 行 / 取数与计算耦合明显时再拆）。

---

## 1. 分层迁移判断（实测）

| 层 | UZI 实现 | 体量 | 依赖 | Rust 化 | 阶段 |
|---|---|---|---|---|---|
| ① 估值模型 | `fin_models.py`(DCF/Comps/LBO/3-stmt/Accretion) | 591 行 | 纯数学 | 🟢 移植 | P0a |
| ② 评分引擎 | `pipeline/score_fns.py` | 1314 行 | 纯逻辑 | 🟢 移植 | P0a/b |
| ③ 人格评审(裁决) | `investor_evaluator.py` + 51 personas YAML | 432 行 | 逻辑+数据 | 🟢 移植 | P0b |
| ④ 报告渲染 | `lib/report/*`(SVG/HTML) | 3296 行 | 字符串模板 | 🟡 后置 | P0.5 |
| ⑤ 取数(22 维) | `fetch_*.py` | 22 个 | **15 靠 akshare** | 🔴 增量 | P1/P2 |
| ⑥ 叙事/定性 | SKILL.md 驱动 LLM | — | LLM 推理 | ⚪ 不移植 | — |

### akshare 依赖密度（解释为何取数层是真成本）
```
capital_flow=9  financials=5  fund_holders=5  valuation=4
chain/peers/governance/events/research=2  basic/futures/industry/kline/lhb/materials=1
akshare-free(7): macro moat sentiment similar_stocks trap_signals policy contests
```
→ 15/22 维靠 akshare 的**未文档化端点 + DataFrame 清洗**。装包不解决问题——是**端点维护**问题。

---

## 2. 类型契约（最高优先级 · 锚定 `fin_models.py` 实测字段）

> ⚠️ 核心风险：UZI 的模型**不是纯函数**，隐式假设字段齐全。缺字段时静默兜底（如 `fcf0` 缺 → `revenue×margin` → `market_cap×0.05`），产出**权威感十足的垃圾数字**。
> Rust 必须：**所有输入 `Option<f64>` + 记录哪些是真值/哪些是推算 + 输出显式 `data_confidence` & `missing_fields`**。

### `research/types.rs`
- [ ] `FundamentalsSnapshot`：宽表，**全部 `Option<f64>`** + provenance（每字段来源 quote/web/provider）
  - DCF 消费：`fcf_latest_yi, revenue_latest_yi, net_margin, market_cap_yi, shares_outstanding_yi, total_debt_yi, cash_yi, price`
  - Comps 消费：target{`price, eps, bvps, pe, pb`} + peers[{`pe, pb`}]
  - 3-stmt 消费：`revenue_latest_yi, equity_yi, market_cap_yi, pb`
  - LBO 消费：`ebitda_yi, revenue_latest_yi, net_margin`
- [ ] `DataConfidence { score: f64, present: Vec<String>, missing: Vec<String> }`
  - **`score` 不可手填**：= `present_weighted / required_weighted`，权重对齐 UZI 维度权重
- [ ] `DcfAssumptions`（默认值锚定 UZI 常量：`stage1/2_growth, stage1/2_years, terminal_g, beta, tax, target_debt_ratio=0.30`，`compute_wacc` 默认 `kd_pretax=0.045`）
- [ ] 每个模型返回值携带 `used_fallback: Vec<String>`（哪些输入走了推算路径）

**验收**：契约能表达"DCF 跑通但 60% 输入是推算的"这一状态，agent 不会把半成品当 institutional-grade。

---

## 3. 分期 TODO

### 🔲 P0a — 估值模型核心 + 类型契约
- [ ] `research/mod.rs` + 在 `lib.rs:21` 后挂 `pub mod research;`
- [ ] `research/types.rs`（见 §2）
- [ ] `research/models/dcf.rs` — 2 段 DCF + Gordon 终值 + 5×5 敏感性表 + `_dcf_verdict` 阈值（30/15/-15）
- [ ] `research/models/comps.rs` — 可比公司：PE/PB 分位、隐含价（中位 PE×EPS）
- [ ] `research/models/wacc.rs` — CAPM `k_e = rf + beta·erp`，税后 `k_d`
- [ ] 单测：每个模型 happy-path + 缺字段退化路径
- **验收**：录 UZI `features.json` golden fixture（茅台 / 亏损股 / 小市值 3 个），Rust 输出与 Python **±1% 内一致**（对齐 `hermes-parity-tests`）

### 🔲 P0b — 评分 + 人格规则引擎
- [ ] **先读 1 份 persona YAML 验 schema**，再 port（别假设 `investor_criteria` 结构）
- [ ] `research/scoring/` — 移植 `score_fns.py` 的 quality/valuation/momentum 等核心分（缺维度 → 中性分，**记进 missing**）
- [ ] `research/personas/` — 51 YAML `include_str!` + `serde_yaml`，`investor_evaluator` 规则裁决
- [ ] 输出 `persona votes JSON`（`{id, vote, score, cited_rule}`），**评语交给 LLM**（不移植 narrative）
- **验收**：persona 裁决可复现；同一 fixture 两次运行结果一致

### 🔲 P0c — Tool + Skill 端到端
- [ ] `research/models/three_stmt.rs` + `research/models/lbo.rs`（补全模型集）
- [ ] `analyze_stock` ToolHandler：输入 `symbol` + 可选 `fundamentals` JSON → 输出 `{dcf, comps, scores, personas, data_confidence, missing_dims}`
- [ ] 注册进 `toolset.rs` `TOOLSET_TRADING`（或新 `TOOLSET_RESEARCH`）
- [ ] `skills/finance/equity-research/SKILL.md`：编排 `get_quote → web_search 补基本面 → analyze_stock → LLM 写结论`
- **验收**：`hermes chat` 中 "深度分析 600519.SH" 能端到端跑出结构化 JSON + LLM narrative

### 🔲 P0.5 — 最小 HTML 报告
- [ ] `research/report/` — 表格 + 分数 + DCF 区间，**无 SVG 花活**
- [ ] LLM narrative 嵌入
- **验收**：可读 HTML，但产物契约仍是 JSON（HTML 是表皮）

### 🔲 P1a — HTTP Transport Gate（blocking）

> 在新增/修改 `research/fetchers/dims/*.rs` 前必须完成。SOP：[`docs/sop/equity_research_data.md`](docs/sop/equity_research_data.md)

- [x] `providers/eastmoney_http.rs` — push2 + 腾讯 qt + kline/fflow 统一入口
- [x] `network_preflight.rs` — push2/tencent TCP 诊断日志
- [x] `BasicFetcher` A 股 fallback → `QuoteRouter`
- [ ] 新浪 hq / baidu / baostock（P2）

### 🔲 P1 — 硬数据替换 web_search（取数层）
逆向 4 个高价值 provider（沿用 `eastmoney_quote.rs` 模式，抓原始 JSON 自解析）：
- [ ] `financials`（财务三表）— xueqiu / em_data
- [ ] `valuation`（PE/PB 历史分位）— em_data
- [ ] `capital_flow`（北向/融资融券）— em_data（注意 UZI 这里 9 个 akshare 调用 = 9 端点）
- [ ] `lhb`（龙虎榜）— em_data / akshare_lhb 底层端点
- **验收**：`FundamentalsSnapshot` 这 4 类字段 provenance 从 `web` 变 `provider`，`data_confidence` 显著上升

### 🔲 P2 — 长尾维度（逼近 UZI 完整度）
- [ ] `accretion_dilution`（M&A）+ 分部建模 `segmental`
- [ ] 杀猪盘检测 `trap_signals`（规则部分移植，判断留 LLM）
- [ ] 行业/同行/股东/事件 等剩余维度增量逆向
- [ ] SVG 渲染（`svg_primitives` / `dim_viz` / `institutional`，3296 行机械移植）

---

## 4. 明确不迁移（架构边界）

| 项 | 原因 | 替代 |
|---|---|---|
| akshare / baostock 作运行时 | 端点维护地狱 + 违反 0py | 逐端点逆向（P1）|
| Playwright 源(iwencai/ths_f10/雪球/legulegu/futu) | Rust 无浏览器引擎 | 放弃，或 LLM+web_search |
| ddgs 搜索 | hermes 已有 `web_search`/`web_extract` | 复用现有工具 |
| 66 人格**评语** / 护城河 / 政策叙事 / 杀猪盘**判断** | LLM 推理，确定性化会变差 | SKILL.md + LLM |
| mx_api | 需 `MX_APIKEY`，与无 key 定位冲突 | 不接 |

---

## 5. 风险登记

1. **取数层是 80% 的真成本**，不是模型层。别被"会出 600KB 报告"迷惑——骨架好移植，喂数据靠 akshare 攒的端点逆向。
2. **不是所有"22 维"都该确定性化**。情绪/护城河/叙事是 LLM 判断，移植成 Rust 规则=降质。移植规则，叙事交 LLM。
3. **静默兜底 = 头号坑**。模型缺字段不能 crash 也不能假装权威，必须把"推算路径"和"覆盖率"暴露到输出。
4. **人格 schema 未核实**：P0b 动手前必须先读 YAML，否则 port 一半发现结构不符。

---

## 6. 与现有 skills 分工

| 能力 | 现有 hermes | UZI Rust 化后 |
|---|---|---|
| DCF Excel 模型 | `dcf-model` skill | 不重复（互补：本项目是确定性 pipeline）|
| 回测 / K 线 / 现价 | `trading-research` / `get_quote` | 继续用，被 `analyze_stock` 复用 |
| A 股深度研报 pipeline | 无（靠 web_search 拼） | **新能力** |
| 叙事 / 护城河 / 政策 | LLM | LLM（不移植）|
