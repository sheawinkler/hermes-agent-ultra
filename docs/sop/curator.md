# SOP: `curator`

| 字段 | 值 |
|------|-----|
| registry `id` | _未录入（尚未纳入 parity 测试）_ |
| Python | `hermes_cli/curator.py`（CLI 入口）、`agent/curator.py`（后台引擎） |
| Rust CLI handler | `crates/hermes-cli/src/commands.rs` → `handle_curator_command()` |
| Rust 后台引擎 | `crates/hermes-skills/src/curator.rs`（状态机 + LLM review 框架） |
| Rust Prompt | `crates/hermes-skills/src/curator_prompt.rs`（CURATOR_REVIEW_PROMPT） |
| Rust 配置 | `crates/hermes-config/src/config.rs` → `CuratorConfig` |
| Rust Gateway | `crates/hermes-gateway/src/commands.rs` + `gateway.rs` |
| Crate | `hermes-cli`, `hermes-skills`, `hermes-config`, `hermes-gateway` |
| Fixtures | _无（尚未纳入 parity 测试）_ |

## 架构概览

### 数据流

```
SLASH_COMMANDS 注册
    │  "/curator" — "Skill curator/control-plane compatibility surface"
    ▼
canonical_command("/curator") → "/curator"
    │
    ├─── CLI/TUI 路径 ──────────────────────────────────────────────
    │    handle_slash_command() match
    │    │  "/curator" => handle_curator_command(app, args).await
    │    ▼
    │    handle_curator_command()
    │    │  解析 args[0] 作为子命令
    │    │  获取 skills_dir = hermes_config::hermes_home().join("skills")
    │    ▼
    │    ├── "status" / ""  → hermes_skills::agent_created_report()
    │    ├── "pin"          → hermes_skills::set_pinned(dir, name, true)
    │    ├── "unpin"        → hermes_skills::set_pinned(dir, name, false)
    │    ├── "archive"      → hermes_skills::archive_skill(dir, name)
    │    ├── "restore"      → hermes_skills::restore_skill(dir, name)
    │    ├── "list-archived"→ std::fs::read_dir(dir.join(".archive"))
    │    ├── "run"          → apply_automatic_transitions() + state 更新
    │    ├── "pause"        → hermes_skills::set_paused(dir, true)
    │    ├── "resume"       → hermes_skills::set_paused(dir, false)
    │    └── _              → 帮助文本
    │
    └─── Gateway/IM 路径 ───────────────────────────────────────────
         handle_command() match
         │  "/curator" => 解析子命令 → GatewayCommandResult 变体
         ▼
         apply_command_result() match
         │  CuratorStatus / CuratorRun / CuratorPause / ...
         ▼
         execute_curator_*() 方法
         │  调用 hermes_skills API → 格式化 plain text → Reply
```

### 子命令实现状态

| 子命令 | CLI/TUI | Gateway/IM | 底层 API |
|--------|---------|------------|----------|
| (无参数) / `status` | ✅ | ✅ | `agent_created_report()` |
| `pin <name>` | ✅ | ✅ | `set_pinned()` |
| `unpin <name>` | ✅ | ✅ | `set_pinned()` |
| `archive <name>` | ✅ | ✅ | `archive_skill()` |
| `restore <name>` | ✅ | ✅ | `restore_skill()` |
| `list-archived` | ✅ | ✅ | `fs::read_dir(.archive)` |
| `run [--dry-run]` | ✅ 自动迁移 | ✅ 自动迁移 | `apply_automatic_transitions()` |
| `pause` | ✅ | ✅ | `set_paused(true)` |
| `resume` | ✅ | ✅ | `set_paused(false)` |

## Curator 状态文件

**路径**: `~/.hermes/skills/.curator_state`（JSON）

```json
{
  "last_run_at": "2026-06-09T10:30:00+00:00",
  "last_run_duration_seconds": 45.2,
  "last_run_summary": "auto: 50 checked, 3 marked stale, 1 archived, 0 reactivated",
  "last_run_summary_shown_at": null,
  "last_report_path": null,
  "paused": false,
  "run_count": 5
}
```

**特性**：
- 原子写入（tempfile + rename）
- 容错加载（JSON 解析失败返回 default）
- Windows 兼容（rename 前删除目标文件）

## 配置

**结构**: `CuratorConfig`（定义于 `hermes-config` 和 `hermes-skills` 镜像）

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `enabled` | `bool` | `true` | curator 是否启用 |
| `interval_hours` | `u64` | 168 | 运行间隔（小时），默认 7 天 |
| `min_idle_hours` | `u64` | 2 | 触发前所需最小空闲时间 |
| `stale_after_days` | `u64` | 30 | 标记为 stale 的不活跃天数 |
| `archive_after_days` | `u64` | 90 | 自动归档的不活跃天数 |
| `prune_builtins` | `bool` | `true` | 是否修剪内置技能 |

**辅助任务配置**（YAML）：
```yaml
gateway:
  auxiliary:
    curator:
      provider: auto
      model: ""
      timeout: 600
  curator:
    enabled: true
    interval_hours: 168
    stale_after_days: 30
    archive_after_days: 90
```

## 自动状态迁移引擎

**函数**: `apply_automatic_transitions(skills_dir, config) -> TransitionResult`

**规则**（优先级从高到低）：
1. **跳过 pinned** 技能
2. **Archive**: anchor ≤ archive_cutoff 且当前非 archived → 归档
3. **Stale**: anchor ≤ stale_cutoff 且当前为 active → 标记为 stale
4. **Reactivation**: anchor > stale_cutoff 且当前为 stale → 重新激活

**活跃锚点** = max(last_used_at, last_viewed_at, last_patched_at)

## LLM Review 框架

### 已实现的组件

| 组件 | 函数 | 状态 |
|------|------|------|
| Prompt 模板 | `CURATOR_REVIEW_PROMPT` (curator_prompt.rs) | ✅ 完整移植 |
| Prompt 构建 | `build_curator_prompt(skills_dir)` | ✅ 生成技能清单表格 |
| 异步编排 | `run_curator_review(skills_dir, config, dry_run, llm_runner)` | ✅ 框架完成 |
| YAML 解析 | `parse_structured_summary(llm_final)` | ✅ regex + serde_yaml |
| 权威信号提取 | `extract_absorbed_into_declarations(tool_calls)` | ✅ |
| 启发式分类 | `classify_removed_skills(removed, after_names, tool_calls)` | ✅ |
| 6 级调和 | `reconcile_classification(...)` | ✅ |
| 报告生成 | `write_curator_report(report, base_dir)` | ✅ run.json + REPORT.md |

### 设计模式

```rust
pub async fn run_curator_review<F, Fut>(
    skills_dir: &Path,
    config: &CuratorConfig,
    dry_run: bool,
    llm_runner: Option<F>,  // 由上层提供，避免循环依赖
) -> Result<CuratorRunRecord, CuratorError>
where
    F: FnOnce(String) -> Fut + Send,
    Fut: Future<Output = Result<CuratorReviewResult, CuratorError>> + Send,
```

## Gateway/IM 集成

### GatewayCommandResult 变体

```rust
CuratorStatus,
CuratorRun { dry_run: bool },
CuratorPause,
CuratorResume,
CuratorPin { name: String },
CuratorUnpin { name: String },
CuratorArchive { name: String },
CuratorRestore { name: String },
CuratorListArchived,
```

### 执行方法

| 方法 | 位置 | 功能 |
|------|------|------|
| `execute_curator_status()` | gateway.rs | 状态摘要（enabled/paused + 统计） |
| `execute_curator_run(dry_run)` | gateway.rs | 自动转换 + 状态更新 |
| `execute_curator_pause_resume(pause)` | gateway.rs | 切换 paused 标记 |
| `execute_curator_pin_unpin(name, pin)` | gateway.rs | 固定/取消固定 |
| `execute_curator_archive(name)` | gateway.rs | 归档技能 |
| `execute_curator_restore(name)` | gateway.rs | 恢复技能 |
| `execute_curator_list_archived()` | gateway.rs | 列出已归档技能 |

### 输出格式

Plain text 输出，兼容所有 IM 平台（微信、飞书、Telegram、Discord、Slack）。

## 与 Python 端差异对照

| 项目 | Python | Rust |
|------|--------|------|
| 命令定义 | `commands.py: CommandDef("curator", ...)` | `commands.rs: SLASH_COMMANDS` + Gateway enum |
| 平台范围 | CLI + TUI + Gateway 全平台 | ✅ CLI + TUI + Gateway 全平台 |
| `status` | 含 pinned 列表 + most/least active top 5 | 简化版：显示所有 agent-created 技能 |
| `run` 自动迁移 | ✅ 完整规则引擎 | ✅ 完整规则引擎 |
| `run` LLM review | 完整：fork AIAgent + 分类调和 + 报告 | ⚠️ 框架完成，调用点未连接 |
| `pause` / `resume` | ✅ state JSON 持久化 | ✅ state JSON 持久化 |
| `--background` 模式 | ✅ daemon thread | ⚠️ 未实现调用点 |
| `--dry-run` 模式 | ✅ 不写状态 | ✅ 不写状态 |
| 配置读取 | 从 config.yaml 读取 | ⚠️ 硬编码默认值 |
| 报告写入 | ✅ logs/curator/{timestamp}/ | ✅ 函数实现，未从命令调用 |
| 备份/回滚 | `backup` / `rollback` 子命令 | ❌ 未移植 |
| `prune` 子命令 | ✅ 独立 prune 命令 | ❌ 未移植 |

---

## 遗留事项（详细）

### P0：LLM Review 调用点连接

**现状**：`run_curator_review()` 异步函数已完整实现，但 CLI 和 Gateway 都未调用它。当前 `/curator run` 仅执行自动状态转换。

**需要做的工作**：

1. **实现 LLM runner callback**（在 `hermes-cli` 或 `hermes-agent` 中）：
   ```rust
   // 需要在 hermes-cli 或 hermes-agent 中实现类似：
   let llm_runner = |prompt: String| async move {
       // 解析 auxiliary["curator"] 配置
       // 构造 AIAgent 实例
       // 设置 enabled_toolsets = [skill_manage, skills_list, skill_view, terminal]
       // 设置 skip_memory=true, quiet_mode=true
       // 执行对话并收集 tool calls
       // 返回 CuratorReviewResult
   };
   ```

2. **在 CLI `run` 子命令中替换**：
   ```rust
   // 当前：
   let result = apply_automatic_transitions(&skills_dir, &config);
   
   // 目标：
   let record = run_curator_review(&skills_dir, &config, dry_run, Some(llm_runner)).await?;
   ```

3. **在 Gateway `execute_curator_run()` 中同步更新**

**阻塞原因**：需要 `hermes-agent` crate 提供 AIAgent spawn API，且 hermes-skills 不能直接依赖 hermes-agent（循环依赖），所以必须通过上层注入 callback。

**预估工作量**：中等（需要理解 hermes-agent 的 RunConversationParams 并实现 callback 适配层）

---

### P0：从配置文件读取 CuratorConfig

**现状**：CLI 和 Gateway 都使用 `CuratorConfig::default()`（硬编码）。

**位置**：
- `crates/hermes-cli/src/commands.rs:5271` — `// TODO: read from gateway config`
- `crates/hermes-gateway/src/gateway.rs:2305` — 同样硬编码

**需要做的工作**：
- 在 `handle_curator_command()` 中从 app 的 config 读取 `CuratorConfig`
- 在 Gateway 的 `execute_curator_*()` 方法中从 `self.config` 读取
- 确认 `GatewayConfig.curator` 字段已正确序列化/反序列化

**预估工作量**：小（仅需替换几行代码，前提是确认 config 加载链路正确）

---

### P1：后台模式（--background）

**现状**：CLI `run` 子命令解析了 `--dry-run` 标志但未实现 `--background`/`--sync` 的区分行为。

**Python 行为**：
- `--background`：LLM pass 在 daemon thread 中运行，CLI 立即返回
- `--sync`/`--synchronous`：等待 LLM pass 完成再返回（默认为 sync）
- `--synchronous` 优先于 `--background`

**需要做的工作**：
- 实现 `tokio::spawn` 后台模式
- 确保后台任务完成后更新 `.curator_state` 的 `last_run_summary` 和 `last_report_path`
- 后台模式下 CLI 输出："LLM pass running in background — check `/curator status` later"

**依赖**：P0 LLM Review 调用点

---

### P1：报告写入调用

**现状**：`write_curator_report()` 函数已实现，但未从任何命令路径调用。

**需要做的工作**：
- 在 `run_curator_review()` 完成后调用 `write_curator_report()`
- 将报告路径存入 `CuratorState.last_report_path`
- Gateway `execute_curator_status()` 中显示报告路径

**依赖**：P0 LLM Review 调用点

---

### P1：调度器集成

**现状**：`should_run_now()` 和 `maybe_run_curator()` 已实现，但未被任何 session 主循环调用。

**需要做的工作**：
- 在 agent session 空闲检测中调用 `maybe_run_curator()`
- 在 cron/定时任务框架中注册 curator 定期检查
- 确保多 session 并发时不重复运行（通过 `.curator_state` 的 `last_run_at` 互斥）

**阻塞原因**：需要了解 hermes-agent 的 session idle 回调机制

---

### P2：backup / rollback 子命令

**Python 功能**：
- `backup`：将当前 skills 目录快照到 `.curator_backups/{timestamp}/`
- `rollback`：从指定备份恢复

**Rust 现状**：完全未实现

**预估工作量**：小（纯文件系统操作，无复杂逻辑）

---

### P2：prune 子命令

**Python 功能**：
- 手动触发一次 prune pass（仅清理，不做 LLM consolidation）
- 支持 `--force` 跳过确认

**Rust 现状**：完全未实现

**预估工作量**：小（调用现有 `apply_automatic_transitions` + archive 即可）

---

### P2：status 输出增强

**Python 输出包含但 Rust 未实现的信息**：
- Most active top 5 / Least recently active top 5
- 配置参数展示（interval, stale_after_days 等）
- 距下次运行时间的倒计时

**预估工作量**：小（纯格式化逻辑）

---

### P3：Python 测试迁移

**现有 Python 测试**（仍引用 Python 实现）：
- `tests/hermes_cli/test_curator_run.py`
- `tests/agent/test_curator.py`
- `tests/agent/test_curator_backup.py`
- `tests/agent/test_curator_reports.py`

**需要做的工作**：
- 为 Rust 实现添加对应的 `cargo test` 单元测试
- 考虑是否纳入 `hermes-parity-tests` fixture 体系

---

## 构建和运行

```bash
# 编译所有相关 crate
cargo build -p hermes-skills -p hermes-config -p hermes-cli -p hermes-gateway

# 运行 hermes-skills 单元测试
cargo test -p hermes-skills

# 风格检查
cargo clippy -p hermes-skills -- -D warnings
cargo clippy -p hermes-gateway -- -D warnings

# 运行 CLI curator 测试
cargo test -p hermes-cli curator
```
