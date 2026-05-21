# Parity 模块 SOP（标准操作流程）

每页对应 [`crates/hermes-parity-tests/fixtures/registry.json`](../../crates/hermes-parity-tests/fixtures/registry.json) 中一条 **`status: active`** 的 `id`。

## 何时新增 / 更新

| 事件 | 动作 |
|------|------|
| registry 新增 active 模块 | 新增 `docs/sop/<id>.md`，并在根 [`AGENTS.md`](../../AGENTS.md) 路由表补充一行 |
| 实现路径或验收命令变化 | 只改对应 SOP，**不要**在 `AGENTS.md` 重复细节 |
| 模块仍在 `PARITY_PLAN.md` 但未进 registry | **不写** SOP；先在 `fixtures/` 录 golden 并注册 |

## 页面索引

| `id` | SOP |
|------|-----|
| `anthropic_adapter` | [anthropic_adapter.md](anthropic_adapter.md) |
| `hermes_core_tool_format` | [hermes_core_tool_format.md](hermes_core_tool_format.md) |
| `checkpoint_manager` | [checkpoint_manager.md](checkpoint_manager.md) |
| `model_metadata` | [model_metadata.md](model_metadata.md) |
| `usage_pricing` | [usage_pricing.md](usage_pricing.md) |
| `approval` | [approval.md](approval.md) |
| `v4a_patch` | [v4a_patch.md](v4a_patch.md) |
| `error_classifier` | [error_classifier.md](error_classifier.md) |
| `skills_guard` | [skills_guard.md](skills_guard.md) |
| `run_conversation` | [run_conversation.md](run_conversation.md) |

路线图与 Prompt 长文保留在 [`PARITY_PLAN.md`](../../PARITY_PLAN.md)；本目录只写**可执行**步骤与验证命令。
