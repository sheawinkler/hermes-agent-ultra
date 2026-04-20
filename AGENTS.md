# Hermes Rust Parity Rules

移植与 parity 相关改动请遵循以下约定（与 `PARITY_PLAN.md` Week 0 一致）。

## 移植任务通用约定

1. 任何移植任务必须先读对应的 Python 源文件（`research/hermes-agent`）以及相关 Rust crate 目录结构。
2. 对外 API 命名保持与 Python 侧一致（`snake_case`）。
3. 错误类型优先使用各 crate 内已有的 `AgentError` / `ToolError`，避免随意新建平行错误体系。
4. 日志使用 `tracing::{debug,info,warn,error}`，避免无门控的 `println!`（CLI 用户输出除外）。
5. 异步代码使用 **tokio**，不要使用 async-std。
6. 可对照的行为应通过 `crates/hermes-parity-tests/fixtures/<module>/*.json` 提供 golden；新增用例时同步更新 `scripts/record_fixtures.py`（若适用）。
7. 单个 PR 尽量只移植一个模块；commit message 建议：`parity(<module>): port from python …`

## 禁止事项

- 不要随意改动 workspace 成员结构（新增 crate 需有明确动机并更新根 `Cargo.toml`）。
- 新增顶层依赖须与根 `Cargo.toml` 已有版本策略一致。
- 合并前应尽量消除新增 `clippy` 警告（全仓 `-D warnings` 为目标，当前 CI 可能仍允许存量警告）。

## Parity 测试

```bash
cargo test -p hermes-parity-tests
```

- 模块状态见 `crates/hermes-parity-tests/fixtures/registry.json`。
- `fixtures/pending/` 中的内容默认不参与 `run_all_active_fixtures`。

Python 侧对照录制：

```bash
python3 scripts/record_fixtures.py
```

无 Python 仓库时仍会输出 **checkpoint 目录哈希**（与 `checkpoint_manager`  shadow 目录命名算法一致）。

## 评测结果落盘

`hermes-eval` 使用 [`JsonReporter`](crates/hermes-eval/src/reporter.rs) 将 [`RunRecord`](crates/hermes-eval/src/result.rs) 写成 JSON；基线对比 / Parquet 可在其上扩展。

### 真实 Agent rollout（非 Noop）

构建与 `hermes-cli` 相同的 [`hermes_agent::AgentLoop`](crates/hermes-agent)，启用 crate feature **`agent-loop`** 后使用 [`AgentLoopRollout`](crates/hermes-eval/src/agent_rollout.rs) 作为 [`TaskRollout`](crates/hermes-eval/src/runner.rs) 传入 [`Runner::run`](crates/hermes-eval/src/runner.rs)：

`cargo build -p hermes-eval --features agent-loop`
