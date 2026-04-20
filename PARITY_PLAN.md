# Hermes Rust 100% Parity — AI 辅助交付计划

> **目标**：在 **8 周内（单人 + AI）** 或 **5 周内（双人 + AI）** 让 `hermes-agent-rust` 达到与 `hermes-agent` (Python) **用户感知 100% 功能对等**。
>
> **基准**：Python `NousResearch/hermes-agent@v2026.4.13`
> **交付定义**：每个模块通过 Python fixture 对照测试 + 端到端集成测试

---

## 一、总体时间线

```
Week 0  │ 基础设施搭建（parity 测试框架、fixture 生成、CI）
Week 1  │ P0-A: 文件系统快照 + process_registry + channel_directory
Week 2  │ P0-B: send_message 完整链路 + 安全策略引擎
Week 3  │ P0-C: 训练/评测环境（SWE + benchmarks + parsers）
Week 4  │ P0-D: voice_mode + mixture_of_agents + TTS/transcription 完整
Week 5  │ P1-A: 浏览器矩阵 + auxiliary_client + context_compressor
Week 6  │ P1-B: copilot_acp_client + models_dev + context_references
Week 7  │ P2:   CLI 命令补全 + 根目录脚本 (batch_runner 等)
Week 8  │ 集成回归 + 17 平台真机联调 + 发布
```

---

## 二、基础设施（Week 0）

### 0.1 Parity 测试框架（必须先做）

创建 `crates/hermes-parity-tests/`，包含：

```
hermes-parity-tests/
├── fixtures/                    # Python 侧录制的输入输出
│   ├── checkpoint_manager/
│   ├── send_message/
│   └── ...
├── src/
│   ├── harness.rs              # 对照 runner
│   └── recorder.rs             # Python 端录制工具（PyO3 或 subprocess）
└── Cargo.toml
```

**录制脚本（Python 端）** `scripts/record_fixtures.py`：

```python
# 对每个 Python 模块批量跑测试用例，把 (input, output, side_effects) 存为 JSON
# 目标：让 Rust 侧 cargo test --package hermes-parity-tests 可复现
```

### 0.2 AI 开发环境配置

在项目根加 `.cursor/rules/` 或 `AGENTS.md`：

```markdown
# Hermes Rust Parity Rules

## 移植任务通用约定
1. 任何移植任务必须先读对应的 Python 源文件 + 相关 Rust crate 目录结构
2. 所有 public API 签名保留 Python 命名（snake_case → Rust snake_case 即可）
3. 错误类型用 crate 内已有的 `AgentError` / `ToolError`，不要新建
4. 日志用 `tracing::{debug,info,warn,error}`，不要 println!
5. async 函数用 tokio 运行时，不要用 async-std
6. 测试必须对照 fixtures/<module_name>/*.json 做断言
7. 每个 PR 只移植一个模块，commit message 用：`parity(<module>): port from python v2026.4.13`

## 禁止事项
- 禁止修改 crate 工作空间结构
- 禁止引入新的顶层依赖（必须与 workspace Cargo.toml 已有版本对齐）
- 禁止跳过 clippy warnings
```

### 0.3 CI 流水线（`.github/workflows/parity.yml`）

```yaml
jobs:
  parity:
    steps:
      - cargo fmt --check
      - cargo clippy --workspace --all-targets -- -D warnings
      - cargo test --workspace --all-features
      - cargo test --package hermes-parity-tests
```

**Week 0 产出**：
- [x] parity 测试框架编译通过（`crates/hermes-parity-tests`，`cargo test -p hermes-parity-tests`）
- [x] 至少 3 个模块的 Python fixture 已录制（`anthropic_adapter`×2 文件 + `hermes_core`×1；golden 与 `scripts/record_fixtures.py` 可交叉验证）
- [x] AI rules 文件就位（根目录 `AGENTS.md`）
- [x] CI 绿灯（`.github/workflows/ci.yml` 含 parity 步骤；与 `cargo test --workspace` 等价覆盖）

---

## 三、Sprint 详细计划

### Week 1 — P0-A：文件系统与进程管理

#### Day 1-2：`checkpoint_manager`（Python 541 行 → Rust 预计 ~700 行）

**依赖**：`git2` crate（已在 workspace 里的话直接用，否则加）

**Prompt 模板**：

```
读取 @research/hermes-agent/tools/checkpoint_manager.py 的完整实现。

任务：在 crates/hermes-tools/src/backends/checkpoint.rs 实现等价的 Rust 版本。

要求：
1. 使用 git2-rs 操作影子 git 仓库（路径 ~/.hermes/shadow/<workspace_hash>）
2. 保持 Python 的 snapshot/rollback/list/diff 接口语义
3. 每个 turn 前自动创建 commit，commit message 格式：`turn-{turn_id}-{timestamp}`
4. 注册到 ToolRegistry，tool name 为 `checkpoint`
5. 添加 fixture 对照测试 @crates/hermes-parity-tests/fixtures/checkpoint_manager/*.json

参考已有实现：
- @crates/hermes-tools/src/backends/file.rs（学习 trait 实现风格）
- @crates/hermes-tools/src/registry.rs（学习注册机制）
```

**验收**：
- [ ] `cargo test -p hermes-tools checkpoint` 全绿
- [ ] 能对 10MB 级工作区做 <100ms 快照
- [ ] 手动测试：修改文件 → rollback 能完整恢复

#### Day 3：`process_registry`（Python 1045 行 → Rust ~1200 行）

**Prompt 模板**：

```
当前的 crates/hermes-tools/src/tools/process_registry.rs 只有 61 行（stub）。

任务：参照 @research/hermes-agent/tools/process_registry.py 完整重写。

要重点保留的能力：
1. 后台进程启动（spawn + PID 管理）
2. 日志捕获（stdout/stderr 分别 ring buffer，默认 10MB）
3. 优雅终止（SIGTERM → timeout → SIGKILL）
4. 列表/状态查询/输出检索
5. 崩溃重启策略（可选）
6. 进程持久化到 ~/.hermes/processes.json，重启后恢复追踪

使用 tokio::process，不要用 std::process。
```

#### Day 4-5：`channel_directory`（Python 272 行 + 持久化）

**Prompt 模板**：

```
Rust 现状：crates/hermes-gateway/src/channel_directory.rs 只有内存 HashMap。

任务：对齐 @research/hermes-agent/gateway/channel_directory.py，加上：
1. 持久化到 ~/.hermes/channel_directory.json
2. 跨平台 channel 解析（"telegram:12345" / "discord:67890"）
3. 启动时加载，更新时原子写入（写临时文件 + rename）
4. channel alias / 昵称系统
5. 与 send_message_tool 对接的查询接口

测试：验证 kill -9 进程后重启能恢复全部 channel 映射。
```

**Week 1 交付**：
- [ ] `checkpoint` 工具可用
- [ ] `process_registry` 等价 Python
- [ ] `channel_directory` 持久化正确
- [ ] Parity 测试通过率 ≥ 90%

---

### Week 2 — P0-B：消息投递与安全策略

#### Day 1-3：`send_message_tool` 真实投递（Python 1043 行）

**依赖**：Week 1 的 `channel_directory`

**Prompt 模板**：

```
读取：
- @research/hermes-agent/tools/send_message_tool.py
- @research/hermes-agent/gateway/delivery.py
- @crates/hermes-tools/src/tools/messaging.rs（当前 stub）
- @crates/hermes-gateway/src/delivery.rs（已有 delivery 基础设施）

任务：改造 messaging.rs 使其：
1. 不再返回 status=pending，而是真实调用 Gateway 投递
2. 解析 channel_ref（支持 alias、平台前缀、user_id 裸数字）
3. 大消息自动分片（markdown_split 已存在，直接复用）
4. 媒体附件（图片/音频/文件）上传
5. 失败重试 + 降级（主平台失败时走 fallback 平台）

不要在 tool 内重复实现 HTTP 客户端，一律通过 GatewayAdapter trait 委托给对应平台适配器。
```

#### Day 4：`tirith_security`（Python 670 行）

**Prompt 模板**：

```
读取 @research/hermes-agent/tools/tirith_security.py 理解其策略模型。

在 crates/hermes-tools/src/ 新建 tirith.rs，实现：
1. 策略 DSL 解析（YAML/JSON 规则）
2. 工具调用前的 pre-check hook
3. 违规行为分级（warn / block / require_approval）
4. 与 approval.rs 集成

规则样例必须能从 ~/.hermes/tirith_rules.yaml 加载。
```

#### Day 5：`website_policy`（Python 282 行）

**Prompt 模板**：

```
读取 @research/hermes-agent/tools/website_policy.py。

在 crates/hermes-tools/src/website_policy.rs 实现：
1. URL 白/黑名单（按域名/路径/正则）
2. 对 web_tools.rs 和 browser.rs 提供 pre-check 接口
3. 策略热加载（文件 mtime 轮询）
```

**Week 2 交付**：
- [ ] send_message 能真实投递到 Telegram + Discord + Slack（至少这 3 个真机测试）
- [ ] tirith + website_policy 集成进 tool pipeline
- [ ] 安全策略 fixture 测试通过

---

### Week 3 — P0-C：训练/评测环境（最难的一周）

> ⚠️ **高风险周**：Python 生态深度依赖（swe-bench / datasets），建议采用"Python subprocess 兜底"策略

#### Day 1-2：搭建 `hermes-environments` 训练环境子模块

**新增目录结构**：

```
crates/hermes-environments/src/
├── training/              # 新增
│   ├── mod.rs
│   ├── base_env.rs       # HermesBaseEnv trait
│   ├── agent_loop.rs     # 训练专用 loop
│   ├── tool_context.rs
│   ├── patches.rs
│   └── parsers/          # tool_call_parsers 各模型
│       ├── anthropic.rs
│       ├── openai.rs
│       ├── qwen.rs
│       └── ...
```

#### Day 3-4：`hermes_swe_env` + `web_research_env` + `agentic_opd_env`

**Prompt 模板**：

```
读取 @research/hermes-agent/environments/hermes_swe_env/ 全部文件。

策略：采用混合实现
- 核心 env loop、tool 调用、trajectory 记录用 Rust
- SWE-bench 数据集加载通过 `python3 -c "..."` subprocess 调用（避免重写 datasets 库）

在 crates/hermes-environments/src/training/swe.rs 实现，保持 default.yaml 配置格式兼容。
```

#### Day 5：`benchmarks/`（tblite + terminalbench_2 + yc_bench）

**Prompt 模板**：

```
每个 benchmark 对应一个 submodule：
- training/benchmarks/tblite.rs
- training/benchmarks/terminalbench.rs
- training/benchmarks/yc_bench.rs

每个实现：
1. 数据集加载（复用 Python subprocess 兜底）
2. task 执行驱动
3. 评分 / pass@k 计算
4. 结果序列化（与 Python 输出格式 bit-exact 对齐）
```

**Week 3 交付**：
- [ ] 能跑通一个 SWE-bench 小样例（10 个任务）并与 Python 结果一致
- [ ] 3 个 benchmark 的 runner 可用
- [ ] tool_call_parsers 覆盖所有 Python 支持的模型

---

### Week 4 — P0-D：多媒体工具

#### Day 1-2：`voice_mode`（Python 1016 行）

**Prompt 模板**：

```
当前 voice_mode.rs 只有 31 行占位。

读取 @research/hermes-agent/tools/voice_mode.py 完整理解。

实现要点：
1. 音频录制（cpal crate）
2. VAD（Voice Activity Detection）简单阈值实现
3. 流式转写（接现有 transcription.rs）
4. 语音命令触发（wake word 可选，先跳过）
5. TTS 回放（接 Week 4 的 TTS）

前端 WebSocket 端点：/ws/voice
```

#### Day 3：`mixture_of_agents_tool`（Python 562 行）

**Prompt 模板**：

```
当前 mixture_of_agents.rs 只有 32 行。

读取 @research/hermes-agent/tools/mixture_of_agents_tool.py。

实现：
1. 多 provider 并行 prompt 发送（tokio::join_all）
2. 聚合层（aggregator model）合成最终回答
3. 支持 aggregator 配置（model / prompt template）
4. 成本汇总 + 延迟上报
```

#### Day 4-5：TTS + Transcription 完整

**Prompt 模板（TTS）**：

```
当前 tts_premium.rs 只是 ElevenLabs queued 占位。

读取 @research/hermes-agent/tools/tts_tool.py（983 行）+ neutts_synth.py（104 行）。

实现：
1. 多后端：ElevenLabs（真实 HTTP）+ OpenAI TTS + neutts（ONNX 本地推理）
2. 流式音频输出（chunked）
3. 缓存（按文本 hash）到 ~/.hermes/tts_cache/

NeuTTS 用 ort crate 加载 ONNX 模型。模型文件从 ~/.hermes/models/neutts/ 加载。
```

**Prompt 模板（Transcription）**：

```
当前 transcription.rs 107 行，仅 Whisper API。

读取 @research/hermes-agent/tools/transcription_tools.py（708 行）。

补齐：
1. 本地 Whisper（whisper-rs crate）
2. 流式转写（分段 + 实时输出）
3. 多语言支持
4. 说话人分离（可选，v1 跳过）
```

**Week 4 交付**：
- [ ] voice_mode 能完整走 录制 → 转写 → TTS 回放
- [ ] mixture_of_agents 真实并行调用 3 个 provider
- [ ] TTS 三后端可用

---

### Week 5 — P1-A：浏览器与 Agent 深度

#### Day 1-3：浏览器矩阵

**Prompt 模板**：

```
当前 backends/browser.rs 只有 CamoFox CDP 薄封装。

读取：
- @research/hermes-agent/tools/browser_camofox.py（592 行）
- @research/hermes-agent/tools/browser_providers/*.py
- @research/hermes-agent/tools/browser_tool.py（2218 行）

重构 backends/browser.rs 为 trait + 多实现：
- trait BrowserProvider { async fn navigate/click/fill/screenshot/... }
- CamoFoxProvider（完整 CDP WebSocket 驱动，用 chromiumoxide crate）
- BrowserbaseProvider（HTTP API）
- BrowserUseProvider（subprocess Python 兜底可接受）
- FirecrawlProvider（已有 web 后端抽离）

browser_tool.rs 作为统一入口，按配置选择 provider。
```

#### Day 4：`auxiliary_client`（Python 2261 行）

**Prompt 模板**：

```
读取 @research/hermes-agent/agent/auxiliary_client.py（2261 行）。

这个模块很大，分三步移植：
1. 先扫读，列出所有 public 函数 + 职责
2. 在 crates/hermes-intelligence/src/auxiliary_client.rs 按职责分 region 实现
3. 重点是"辅助 LLM 调用"：session_search 摘要、title 生成、insights 提取、review pass

复用现有的 provider.rs / credential_pool.rs，不要重复造轮子。
```

#### Day 5：`context_compressor` 完整化

**Prompt 模板**：

```
当前 compression.rs 只有 32 行。

读取 @research/hermes-agent/agent/context_compressor.py（738 行）。

补齐：
1. 多策略：summarize / truncate / drop_oldest / importance_score
2. tool result 独立压缩（保留关键字段）
3. 图片/附件单独处理
4. 压缩配额管理（目标 token / 实际 token）
5. 压缩前后对比日志
```

**Week 5 交付**：
- [ ] 浏览器 4 个 provider 可切换
- [ ] auxiliary_client 全部接口覆盖
- [ ] context_compressor 通过压缩质量测试（压缩率 ≥ 60%，任务完成率无损）

---

### Week 6 — P1-B：Agent 层剩余

| Day | 模块 | Python LOC | Rust 目标 |
|---|---|---|---|
| 1-2 | `copilot_acp_client` | 570 | `hermes-agent/src/copilot_acp.rs` |
| 3 | `models_dev` | 670 | `hermes-intelligence/src/models_dev.rs` |
| 4 | `context_references` | 491 | `hermes-agent/src/context_refs.rs` |
| 5 | 缓冲 / 收尾 + 集成测试 | — | — |

**Prompt 模板（copilot_acp_client）**：

```
读取 @research/hermes-agent/agent/copilot_acp_client.py（570 行）+ smart_model_routing 中涉及 copilot_acp 的部分。

实现 crates/hermes-agent/src/copilot_acp.rs：
1. GitHub Copilot 的 ACP 子进程启动
2. 握手 + session 初始化
3. prompt/completion 转发
4. 错误与重连逻辑

复用 hermes-acp crate 的协议层，不要重新实现 JSON-RPC。
```

---

### Week 7 — P2：CLI 与脚本

**策略**：CLI 命令基本是样板，AI 一天能批量出 3-5 个。

#### Day 1-2：高价值 CLI 命令

```
批量任务 prompt：

读取 @research/hermes-agent/hermes_cli/ 下这些文件，依次移植到 Rust：
1. nous_subscription.py → crates/hermes-cli/src/nous_subscription.rs
2. codex_models.py      → crates/hermes-cli/src/codex_models.rs
3. region.py            → crates/hermes-cli/src/region.rs
4. memory_setup.py      → crates/hermes-cli/src/memory_setup.rs
5. runtime_provider.py  → 补齐 crates/hermes-cli/src/app.rs 中的 runtime_providers

每个模块完成后：
- 注册 subcommand 到 cli.rs
- 添加 --help 文本（与 Python 一致）
- 写至少一个 integration test
```

#### Day 3：CLI 命令补丁（剩余）

```
批量补齐：
- status.py → 把 main.rs 的 run_status 从 80 行扩展到 Python 的 465 行等价
- gateway.py → 把 2510 行拆成 gateway_cmd.rs 子模块
- auth_commands.py → auth.rs 补全
- clipboard.py → 独立 crate 或 tui/clipboard.rs 模块
- plugins_cmd → commands.rs 的 plugins subcommand
```

#### Day 4-5：根目录 Python 模块

```
移植：
- batch_runner.py (1287)     → hermes-rl/batch_runner.rs（完整化）
- trajectory_compressor.py (1455) → hermes-rl/trajectory_compressor.rs（完整化）
- mini_swe_runner.py (709)   → hermes-rl/mini_swe_runner.rs（新增）
- rl_cli.py (446)            → hermes-cli 的 rl subcommand
- model_tools.py (577)       → hermes-tools/src/model_tools.rs
- mcp_serve.py (867)         → 验证 hermes-mcp crate 是否已覆盖

运维脚本单独处理：
- scripts/discord-voice-doctor.rs（Rust 版）
- scripts/sample_and_compress.rs
```

**Week 7 交付**：
- [ ] 所有 Python CLI 命令在 Rust 侧都有对应 `hermes <subcommand>`
- [ ] `hermes --help` 输出与 Python `hermes --help` diff 只剩版权/版本行

---

### Week 8 — 集成、联调、发布

#### Day 1-2：17 平台真机联调

对每个 Gateway 平台：
1. 用测试账号发一条 "hello"
2. 验证 bot 能响应
3. 发图片 / 音频验证媒体投递
4. 记录问题单

**Prompt 模板**：

```
我在 @platform 测试时遇到 <具体错误>。

Python 端的等价路径是 @gateway/platforms/<platform>.py 的 <具体函数>。

Rust 端在 @crates/hermes-gateway/src/platforms/<platform>.rs。

对比两边实现，找出差异并修复。
```

#### Day 3：性能回归

```bash
# 对比 Python 和 Rust 的端到端延迟
cargo bench --workspace
python benchmarks/e2e_latency.py

# 目标：Rust 在所有场景不慢于 Python，冷启动 ≥ 3x 快
```

#### Day 4：文档

- 更新 `README.md`（把 "13/13 parity" 改为 "Full parity with Python v2026.4.13"）
- 更新 `README_ZH.md` / `README_JA.md` / `README_KO.md`
- 写 `MIGRATION.md`（Python 用户如何迁移）

#### Day 5：发布

- Cut release tag `v1.0.0`
- GitHub Actions 自动构建 6 平台二进制
- Homebrew formula 更新
- 安装脚本 smoke test

---

## 四、风险与缓解

| 风险 | 概率 | 影响 | 缓解 |
|---|---|---|---|
| Week 3 训练环境实现超期 | 高 | 延期 1-2 周 | 提前准备 Python subprocess 兜底方案 |
| 浏览器 CDP 完整驱动调试困难 | 中 | 延期 3-5 天 | 用 chromiumoxide crate，已有成熟实现 |
| NeuTTS ONNX 模型体积/许可 | 中 | 可能放弃本地 TTS | 只保留 ElevenLabs + OpenAI 两个云端 |
| 17 平台账号无法全部获取 | 中 | 部分平台只能自动化测试 | 按优先级：TG/DC/Slack 必测，其他冒烟即可 |
| AI 生成代码 clippy 失败循环 | 中 | 每模块额外 0.5-1h | prompt 里强制要求 `#[allow(...)]` 显式声明 |

---

## 五、每日工作节奏（推荐）

```
09:00 - 09:30  读 Python 源文件 + 对应 Rust 现状（不写代码，只理解）
09:30 - 10:00  生成 fixture（跑 Python 脚本录制）
10:00 - 12:00  第一轮 AI prompt → 得到骨架代码
12:00 - 13:00  午休 + cargo build + 看编译错误
13:00 - 16:00  第二轮 AI prompt（带编译错误）→ 修正 → 通过编译
16:00 - 17:30  跑 parity 测试，修 diff
17:30 - 18:00  commit + push + 更新 TODO
```

---

## 六、AI Prompt 通用模板

### 模板 1：模块移植

```
### 背景
我在把 @https://github.com/NousResearch/hermes-agent 移植到 Rust（hermes-agent-rust）。

### 任务
把 @<python_file_absolute_path> 的 <module_name> 移植到 Rust。

### 上下文（按需读取）
- Python 源文件：<path>
- 相关 Rust crate：<path>
- 已有类似实现（参考风格）：<path>
- 错误类型约定：@crates/hermes-core/src/error.rs
- Fixture 测试：@crates/hermes-parity-tests/fixtures/<module>/

### 约束
1. 保持 Python 行为 bit-exact（对照 fixture）
2. 只改 <target_file>，不动其他文件（除非必须）
3. 用 tracing 打日志，不要 println
4. 所有 public 函数加 doc comment
5. 用 thiserror 定义错误，不要手写 Display

### 交付
1. 实现代码
2. 单元测试（覆盖率 ≥ 80%）
3. 一行 commit message：`parity(<module>): port from python v2026.4.13`
```

### 模板 2：调试修复

```
### 现状
@<rust_file> 在跑 @<test_file> 的 `<test_name>` 时失败：

```
<编译错误 或 测试输出>
```

### Python 等价实现
@<python_file> 的 <function_name>（行 X-Y）

### 任务
对照 Python 逻辑，修复 Rust 实现。只改必要的行，保留其他代码不变。
```

### 模板 3：重构合并

```
### 背景
@<rust_file_1> 和 @<rust_file_2> 有重复逻辑，对应 Python 的 <python_function>。

### 任务
1. 抽出公共函数到 @<shared_module>
2. 两个调用点改为引用
3. 保证所有现有测试通过（cargo test -p <crate>）
```

---

## 七、检查点（每周结束）

每周五下班前必须检查：

- [ ] 本周所有模块 `cargo test` 通过
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 无警告
- [ ] Parity 测试覆盖率 ≥ 85%
- [ ] 更新 `README.md` 的 parity 进度表
- [ ] TODO tracker 更新到 GitHub Issue

---

## 八、发布后（Week 9+）

Parity 达成只是开始。后续工作：

1. **性能优化**：Rust 相对 Python 应有 3-10x 提升，量化并在 README 展示
2. **Rust-only 增强**：用 Rust 生态独有能力（零拷贝、SIMD、真并行）做 Python 做不到的优化
3. **社区维护**：跟进 Python 主线新版本，持续增量 parity

---

**最后建议**：

- 每天开始前把**当天的 Prompt 模板**提前写好贴到 Cursor，不要临场想
- 保持 **AI 产出 → 人工 review → 编译验证 → 测试验证** 的四步闭环，不要跳过 review
- Week 3 最容易翻车，提前加一天缓冲
- 周末休息，疲劳编码会让 AI 加速比跌到 1.5x 以下

**祝顺利。**
