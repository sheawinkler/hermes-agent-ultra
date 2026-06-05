# AGENTS.md — Hermes Parity 移植操作系统（路由层）

> **薄路由 + 厚 SOP**：本文件定义元规则与通用流程；模块级步骤见 [`docs/sop/`](docs/sop/README.md)。  
> **机器真相**：可测模块以 [`crates/hermes-parity-tests/fixtures/registry.json`](crates/hermes-parity-tests/fixtures/registry.json) 为准；路线图见 [`PARITY_PLAN.md`](PARITY_PLAN.md)。

## 核心元规则

1. **任务分型**：parity 移植 / 修 golden / `hermes-eval` 评测 / 其它 — 仅前两类执行下方「通用移植 SOP」。
2. **禁止猜测**：路径、trait、依赖版本、fixture `op` 名称 — 用仓库搜索或 Read 工具查证，不得臆造。
3. **步进验证**：每一步有且仅有一条验证命令；失败则停止，不得进入下一步。
4. **单模块 PR**：一次改动只覆盖 `registry.json` 中的一个 `id`（或为其新增 fixture 条目）。

## 任务路由

| 人类意图 | 第一步 | 详细 SOP |
|----------|--------|----------|
| 移植某 Python 模块 | 读 `registry.json` 找 `id`；若无条目 → 见 [`PARITY_PLAN.md`](PARITY_PLAN.md)，先录 fixture | [`docs/sop/<id>.md`](docs/sop/README.md) |
| 跑 parity / 验收 | `cargo test -p hermes-parity-tests` | [`crates/hermes-parity-tests/fixtures/README.md`](crates/hermes-parity-tests/fixtures/README.md) |
| 录 Python golden | `python3 scripts/record_fixtures.py` | 同上 |
| 真实 agent 评测 rollout | `cargo build -p hermes-eval --features agent-loop` | 见文末「评测」 |

**当前 active 模块**（与 registry 同步）：`anthropic_adapter`、`hermes_core_tool_format`、`checkpoint_manager`、`model_metadata`、`usage_pricing`、`approval`、`v4a_patch`、`error_classifier`、`skills_guard`、`code_execution_env`、`code_execution_stubs`、`send_message`。

未出现在 registry 的模块（如 `process_registry`）**没有**已激活 SOP；按 `PARITY_PLAN.md` 对应 Week 推进，落地 fixture 后再在 `docs/sop/` 新增一页。

## 通用移植 SOP（所有 `registry.json` active 模块）

```
0. Rust edition 2024
1. 读 registry 条目 → python 路径、rust 位置、fixture_dirs、note
2. 读 docs/sop/<id>.md → 打开列出的 Python / Rust 源文件
3. 实现或修改 Rust（仅 touched crate；API 命名 snake_case，与 Python 一致）
4. 验证编译：cargo build -p <crate>   （失败则只修报错，重复至多 3 次，见防御规则）
5. 验证 parity：cargo test -p hermes-parity-tests
6. 验证风格：cargo clippy -p <crate> -- -D warnings  （仅 touched crate）
7. 提交：parity(<id>): port from python@commit for example port from python@d85f24f  
```

### 编码约定（所有移植）

1. 先读 `C:\\Users\\15059\\hermes-agent` 下对应 Python（registry 标 `N/A` 的模块跳过）。
2. 错误类型用各 crate 已有 `AgentError` / `ToolError`，不新建平行体系。
3. 日志：`tracing::{debug,info,warn,error}`；CLI 面向用户的输出可用 `println!`。
4. 异步：**tokio**，不用 async-std。
5. 新 golden：放在 `crates/hermes-parity-tests/fixtures/<dir>/`，并更新 `scripts/record_fixtures.py`（若适用）与 `registry.json`。
6. `docs\roadmaps\ULTRA_ADDITIONAL_FEATURES_PLAN_2026-04-24.md` Issue #78 / Workstream 7 不属于 registry.json parity，避免移植任务误加 Rust-only 行为。

### 禁止事项

- 不随意改 workspace 成员（新 crate 需动机 + 根 `Cargo.toml`）。
- **依赖**：改 `Cargo.toml` 前必须在根 `Cargo.toml` / workspace 中核对已有版本；禁止重复添加同名 crate。
- 合并前消除**本次引入**的 clippy 警告（全仓 `-D warnings` 为目标）。

## 时间 / 时区

时区分两类时钟（见 `crates/hermes-core/src/time.rs`，Python `hermes_time.py`）：

| Tier | 用途 | API |
|------|------|-----|
| **A 用户墙钟** | system prompt 日期、cron 调度/到期、execute_code `TZ`、会话搜索展示、**cron session id 时间戳** | `hermes_core::now()` / `ensure_aware_*` |
| **B 内部 UTC** | session `last_active`（存储）、token 过期、telemetry、文件名 | `Utc::now()` |

> **注意**：`now_utc()` 返回 UTC，属于 Tier B 语义。cron 调度代码可用 `now_utc()` 作为输入，但时区偏移转换由 `cron_wall_offset_at()` 内部保证，不代表 `now_utc()` 等于 wall clock。

- System prompt 只注入 **date-only** 的 `Conversation started:`，不得放分钟级「当前时间」。
- 配置：`HERMES_TIMEZONE` > `config.yaml` 的 `timezone` > 服务器本地；`HERMES_CRON_TZ` 已 deprecated。

## 防御性规则

- **连续 3 次** `cargo build` 仍失败 → 向人类汇报完整错误与已尝试修改 → **暂停**。
- **parity 失败** → 记录 case `id`、`op`、Rust 实际输出与 fixture `expected`（完整 JSON 或 diff）→ 再改实现；仍不确定则请人确认 golden。
- **禁止**在未读 fixture 的情况下改 `expected` 以「骗过」测试。

## Parity 测试（速查）

```bash
cargo test -p hermes-parity-tests
```

- `fixtures/pending/` 不参与 `run_all_active_fixtures`。
- 无 Python 仓库时，`record_fixtures.py` 仍可输出 checkpoint shadow 目录哈希 golden。

## 评测（非移植任务）

`hermes-eval` 用 [`JsonReporter`](crates/hermes-eval/src/reporter.rs) 写 [`RunRecord`](crates/hermes-eval/src/result.rs) JSON。

真实 Agent rollout：启用 feature **`agent-loop`**，以 [`AgentLoopRollout`](crates/hermes-eval/src/agent_rollout.rs) 作为 [`TaskRollout`](crates/hermes-eval/src/runner.rs)：

```bash
cargo build -p hermes-eval --features agent-loop
```
