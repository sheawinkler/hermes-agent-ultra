# Python → Rust 移植经验总结

> 本文档记录 `hermes-agent-ultra` 项目将 Python 工具实现移植为 Rust 的通用经验、约束与最佳实践。
> 以 `todo` 工具为核心案例，适用于所有 `registry.json` active 模块的移植工作。

---

## 一、核心移植原则

### 1.1 外部契约优先

移植的**首要目标**是对外行为与 Python 一致，而非复刻内部实现。

| 维度 | 要求 |
|------|------|
| 工具名 | 与 Python 完全一致（如 `todo`，不得改名） |
| 参数名/类型 | 与 Python schema 对齐（如 `todos`, `merge`，不得新增必填参数） |
| 返回格式 | 与 Python 返回的 JSON 结构一致 |
| 错误兜底 | 非法输入的处理行为与 Python 一致 |

### 1.2 保留 Rust 内部优秀设计

内部实现可以（也应该）优于 Python，但**不得破坏对外契约**：

- **文件持久化**：Python 是 session 级内存，Rust 可改为跨会话持久化（`~/.hermes/todos.json`）
- **并发安全**：`Arc<Mutex<...>>` 保护共享状态
- **内部扩展字段**：如 `created_at` 等元数据，对外序列化时不暴露即可
- **Trait 抽象**：`TodoBackend` trait 使后端可替换，也便于测试 mock

### 1.3 禁止事项

- 不随意改 workspace 成员（新 crate 需明确动机并修改根 `Cargo.toml`）
- 改 `Cargo.toml` 前必须在 workspace 中核对已有版本，**禁止重复添加同名 crate**
- 不得在未读 fixture 的情况下修改 `expected` 来「骗过」测试
- 不猜测路径、trait 名、依赖版本——用仓库搜索或 Read 工具查证

---

## 二、todo 工具移植案例

### 2.1 Python 外部契约（目标）

```
工具名: todo
参数:
  todos  — 可选数组，每项含 id / content / status
  merge  — 可选 bool，默认 false

行为:
  todos 缺省 / null  → read（返回当前列表）
  merge=false        → 全量替换（按 id 去重，保留最后出现）
  merge=true         → 按 id 局部更新已有项 + 追加新项

status 枚举: pending | in_progress | completed | cancelled

返回:
{
  "todos": [...],
  "summary": {
    "total": N,
    "pending": N,
    "in_progress": N,
    "completed": N,
    "cancelled": N
  }
}
```

### 2.2 兜底规则（与 Python 一致）

| 字段 | 异常情况 | 兜底值 |
|------|----------|--------|
| `id` | 空字符串 / 缺失 | `"?"` |
| `content` | 空字符串 / 缺失 | `"(no description)"` |
| `status` | 非法值 | `"pending"` |

### 2.3 Rust 内部实现要点

**文件持久化路径**：

```
~/.hermes/todos.json
Windows: C:\Users\<user>\.hermes\todos.json
```

读取优先级：`$HOME` → `$USERPROFILE` → `.`（当前目录）

**并发安全**：`Mutex<Vec<StoredItem>>`，读写均加锁

**内部存储类型**（多一个 `created_at`）：

```rust
struct StoredItem {
    id: String,
    content: String,
    status: String,
    #[serde(default)]
    created_at: String,   // 对外不暴露
}
```

**写盘时机**：`write_all` 和 `merge_items` 操作完成后立即调用 `save()`

---

## 三、用户明确的编码约束

以下约束均为用户在开发过程中明确提出，必须严格遵守：

### 3.1 只改 todo 核心功能文件

> "不要改除了 todo 核心功能外的代码，如果有严重错误可以修改，警告不要管"

- **只动** `crates/hermes-tools/src/tools/todo.rs` 和 `crates/hermes-tools/src/backends/todo.rs`
- 其他文件有编译**错误**才修改，**警告一律忽略**（不运行 clippy，不修 warning）
- 不新增结论文档/总结文档，只做必要代码与测试改动

### 3.2 实现要求"简单正确，不要 over engineering"

- 不新增独立 action；删除语义通过 replace（全量替换）自然支持
- 不引入新的错误体系，使用 crate 已有的 `ToolError` / `AgentError`
- 测试 mock 后端用 `std::sync::Mutex`（非 tokio），足够轻量

### 3.3 验证顺序

每次改动后按以下顺序验证（警告不处理）：

```bash
cargo build -p hermes-tools
cargo test -p hermes-tools todo   # 先跑 todo 专项
cargo test -p hermes-tools        # 再跑全量（4个既有失败与 todo 无关，属正常）
```

**跳过** `cargo clippy`（用户要求警告不管）

---

## 四、测试覆盖清单

每个移植模块应至少覆盖以下场景：

| # | 场景 | 测试位置 |
|---|------|----------|
| 1 | read 模式返回 todos + summary | `tools/todo.rs` |
| 2 | replace 模式全量覆盖 | `tools/todo.rs` |
| 3 | merge 更新已有项（仅更新传入字段） | `tools/todo.rs` |
| 4 | merge 追加新项、保留已有项 | `tools/todo.rs` |
| 5 | duplicate id 去重（保留最后出现） | `tools/todo.rs` |
| 6 | `cancelled` 状态在 summary 中正确计数 | `tools/todo.rs` |
| 7 | 非法 status 回退 `pending` | `tools/todo.rs` |
| 8 | 持久化读写（写盘→重新加载验证） | `backends/todo.rs` |

测试用 `MockTodoBackend`（`Mutex<Vec<TodoItem>>`）做单元测试，用 `tempfile::tempdir()` 做后端集成测试。

---

## 五、通用移植 SOP 精简版

```
0. 确认 Rust edition 2024
1. 读 crates/hermes-parity-tests/fixtures/registry.json 找到模块 id
2. 读 docs/sop/<id>.md，打开 Python / Rust 源文件
3. 以 Python 外部契约为目标改写 Rust（仅 touched crate）
4. cargo build -p <crate>              （失败最多重试 3 次，仍失败则停止报告）
5. cargo test -p <crate>               （失败记录 case id + diff，修复后重试）
6. 提交信息格式：parity(<id>): port from python@<commit>
```

---

## 六、环境问题记录

### Windows 下 cargo 不在 PATH

**现象**：PowerShell 提示 `cargo: command not found`

**原因**：cargo 安装在 `%USERPROFILE%\.cargo\bin`，PowerShell 会话未继承该路径

**修复**：
```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
```

或使用完整路径执行 cargo 二进制。

---

## 七、已知非 todo 既有失败测试（可忽略）

全量测试中以下 4 个失败与 todo 无关，属 Windows 平台兼容性或其他模块既有问题：

| 失败测试 | 原因 |
|----------|------|
| `code_execution_stubs::tests::matches_python_web_search_only` | Windows `\r\n` 行尾差异 |
| `tool_dispatch_helpers::tests::overlapping_paths_not_parallel` | 并行策略断言问题 |
| `tool_policy::tests::policy_from_env_defaults_to_relaxed_when_unset` | 默认策略预设问题 |
| `tools::messaging::tests::handler_send_file` | 消息发送失败 |

---

*最后更新：2026-05-26*
