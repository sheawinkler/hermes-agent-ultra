# SOP: Equity Research Data Layer (UZI HTTP parity)

| 字段 | 值 |
|------|-----|
| Scope | `hermes-trading` A-share fetchers / providers |
| UZI repo | `wbh604/UZI-Skill` → `skills/deep-analysis/scripts/` |
| Rust | `crates/hermes-trading/src/providers/eastmoney_http.rs` |

## 必读 UZI 文件（按优先级）

1. **`lib/data_sources.py`** — A 股 basic/kline fallback 链（push2 → 腾讯 → 新浪 …）
2. **`lib/providers/direct_http_provider.py`** — 腾讯/新浪直连 UA 与解析
3. **`lib/market_router.py`** — symbol 归一化（`.SH`/`.SZ`/港股/美股）
4. **`lib/network_preflight.py`** — 国内域可达性诊断
5. **`pipeline/fetchers/fetch_*.py`** — **仅**字段/schema 参考，不含 transport 防御

## UZI → Rust 映射

| UZI 逻辑 | Rust 模块 | Transport 要求 |
|----------|-----------|----------------|
| `fetch_a_share_basic` push2 直连 | `eastmoney_http::fetch_push2_quote` | `ut` + UA + Referer |
| 腾讯 qt fallback | `eastmoney_http::fetch_tencent_qt` | UA + Referer |
| push2his kline | `eastmoney_http::fetch_push2_klines` | `ut` + Referer |
| push2 fflow | `eastmoney_http::fetch_push2_fflow_klines` | `ut` + Referer |
| `parse_ticker` | `symbol::normalize_symbol` | `.SS`→`.SH` 等 |
| 合并 snapshot | `eastmoney_http::fetch_a_share_snapshot` | push2 → 腾讯 |
| basic 维 fallback | `research/fetchers/dims/basic.rs` | basic 失败 → `QuoteRouter` |

## P1a HTTP Transport Gate（blocking）

在**新增或修改**任何 `research/fetchers/dims/*.rs` 之前：

1. 所有 `push2.eastmoney.com` / `push2his.eastmoney.com` 调用必须经 [`eastmoney_http.rs`](../../crates/hermes-trading/src/providers/eastmoney_http.rs)
2. `cargo test -p hermes-trading` 通过
3. `cargo clippy -p hermes-trading -- -D warnings` 通过
4. 本地可选：`cargo test -p hermes-trading -- --ignored live_`

**禁止**：在 `eastmoney_basic.rs` / `eastmoney_quote.rs` / `eastmoney.rs` / `eastmoney_capital_flow.rs` 中直接拼 push2 URL（应走 `eastmoney_http`）。

## 移植 checklist（每个新 provider）

- [ ] Headers：`User-Agent`（`http::BROWSER_USER_AGENT`）、`Referer`、`ut`
- [ ] Symbol：`normalize_symbol` + `EastmoneyProvider::to_secid`
- [ ] Fallback：push2 失败时有独立 host 备选（至少腾讯 qt）
- [ ] 失败语义：`DimQuality::Partial` 或显式 `error`，不 silent empty
- [ ] 单测：parse fixture + merge/failover 逻辑

## 验证

```bash
cargo build -p hermes-trading
cargo test -p hermes-trading
cargo clippy -p hermes-trading -- -D warnings
cargo fmt -p hermes-trading
```

## P2 未实现（刻意跳过）

新浪 hq、百度估值、baostock、雪球、Playwright — 见 [`EQUITY_RESEARCH_TODO.md`](../../EQUITY_RESEARCH_TODO.md) §4。
