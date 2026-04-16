# Hermes Agent (Rust)

**[English](./README.md)** | **[中文](./README_ZH.md)** | **[日本語](./README_JA.md)** | **[한국어](./README_KO.md)**

[Hermes Agent](https://github.com/NousResearch/hermes-agent) 的生产级 Rust 重写版 — [Nous Research](https://nousresearch.com) 出品的自我进化 AI Agent。

**84,000+ 行 Rust 代码 · 16 个 crate · 641 个测试 · 17 个平台适配器 · 30 个工具后端 · 8 个记忆插件 · 6 个跨平台发布目标**

---

## Python v2026.4.13 对齐状态

基线目标：`NousResearch/hermes-agent@v2026.4.13`（`1af2e18d408a9dcc2c61d6fc1eef5c6667f8e254`）。

- 进度：**10 / 13** 个范围内对齐项已完成。
- 已完成重点：提示词分层/核心 guidance 对齐、智能路由基础运行时切换与回退、memory 工具语义与容量限制、内置 `MEMORY.md`/`USER.md` 快照注入、memory 生命周期钩子（`on_memory_write`、`queue_prefetch`、`on_pre_compress`、`on_session_end`、`on_delegation`）、`session_search` 双模式与 `role_filter`/limit 对齐。
- 剩余重点：`resolve_turn_route` 运行时签名字段的完整对齐，以及 Python 风格 skills+memory 驱动的自进化行为闭环对齐。

### TODO（对齐追踪）

- [x] Long Memory：内置 memory action/target 语义 + 字符限制。
- [x] Long Memory：会话启动时 memory 快照注入。
- [x] Long Memory：生命周期钩子（`on_memory_write`、`on_pre_compress`、`on_session_end`、`on_delegation`）。
- [x] Session Search：recent 模式（空 query）、关键词模式、`role_filter`、`limit <= 5`。
- [x] Session Search：子会话到父会话归并（parent session 归一化）。
- [x] Session Search：Python 等价的按会话 LLM 摘要流水线。
- [x] Session Search：hidden/internal source 过滤规则。
- [x] Session Search：按运行时上下文自动注入并排除当前活跃会话 lineage。
- [x] Smart Model Selection：逐轮 cheap-route 与 policy recommendation 路由。
- [x] Smart Model Selection：路由 provider 构建失败时回退主 provider。
- [ ] Smart Model Selection：Python `resolve_turn_route` 完整运行时签名字段（`api_mode`、`command`、`args`、`credential_pool`、`signature`）端到端对齐。
- [ ] Self-Evolution：Python 风格 memory/skills 驱动自动适应闭环对齐。
- [ ] Self-Evolution：基于 Python `v2026.4.13` 行为基线的 parity 验证测试。

### 能力实现状态（你要求的检查清单）

状态说明：`implemented` = 当前代码库可用；`partial` = 已有实现但与目标描述尚未完全等价。

| 能力 | 状态 | 说明 |
|---|---|---|
| 交互式 CLI + one-shot（`crates/hermes-cli`） | implemented | 已有 TUI 交互模式与 `chat --query` 单次执行路径。 |
| Agent loop：流式 + 工具执行 + 上下文压缩 | implemented | 已实现 `run_stream`、并行工具执行、自动压缩。 |
| Prompt caching | partial | 已有系统提示词构建缓存，但跨请求完整缓存语义尚未完全对齐。 |
| Provider：Anthropic / OpenAI chat-compatible / OpenAI Responses / OpenRouter-compatible | implemented | `hermes-agent`/`api_bridge`/扩展 provider 适配已具备。 |
| 内置工具：文件/终端/补丁/记忆/web/vision + 可选 code execution | implemented | 工具集合已覆盖这些类别；代码执行可通过策略/工具集控制开启。 |
| 运行时 MCP 工具发现（stdio + HTTP） | implemented | MCP client 已支持 stdio/http 配置与运行时 tools 列举。 |
| MCP prompts/resources bridge + capability gating | partial | prompts/resources API 能力已存在；严格 capability-gated bridge 行为仍在收口。 |
| 本地 memory 快照 + 请求级技能匹配/注入 | implemented | `MEMORY.md`/`USER.md` 快照注入与 skills prompt 编排已接入。 |
| SQLite 会话历史 + resume | implemented | `sessions.db` 持久化与会话加载/恢复流程已存在。 |
| 多模型支持（OpenAI/Anthropic/OpenRouter） | implemented | 路由与 provider 栈支持。 |
| 内置工具数量（你写 26） | implemented | Rust 当前已超过该数量（30+ 工具后端）。 |
| TUI：交互聊天 + 30+ slash 命令 + 工具进度 + 状态栏 | implemented | TUI、状态栏、丰富 slash 命令处理已具备。 |
| 上下文感知自动加载（`AGENTS.md`/`CLAUDE.md`/`MEMORY.md`/`USER.md`） | implemented | 上下文文件加载与 memory 快照加载已具备。 |
| Memory 系统：SQLite + FTS5 + 跨会话持久化 | implemented | 会话持久化 + FTS `session_search` 已实现。 |
| Skills 系统：YAML 技能创建与管理 | implemented | skills 工具链与 skill store/hub 已具备。 |
| 人格系统：coder/writer/analyst 切换 | partial | 人格切换功能已实现；具体预置人格依赖本地人格文件。 |
| 上下文压缩：自动 + 手动 | implemented | loop 自动压缩 + 手动 slash 命令路径已存在。 |
| 子 Agent 委托 | partial | `delegate_task` 与委托相关钩子已实现；完整自治子代理编排仍在演进。 |
| 消息推送：Telegram/Discord/Slack API | implemented | gateway 平台适配已具备。 |
| 安全：路径校验、危险命令拦截、搜索深度限制 | partial | 命令审批与凭据/文件防护已具备；部分安全维度仍在持续补齐。 |
| 中文输入：TUI UTF-8 全支持 | implemented | Rust/TUI 链路可正常处理 UTF-8 输入输出。 |

## 亮点

### 单二进制，零依赖

一个 ~16MB 的二进制文件。不需要 Python、pip、virtualenv、Docker。能跑在树莓派、$3/月 VPS、断网服务器、Docker scratch 镜像上。

```bash
scp hermes user@server:~/
./hermes
```

### 自进化策略引擎

Agent 从自身执行中学习。三层自适应系统：

- **L1 — 模型与重试调优。** 多臂老虎机算法根据历史成功率、延迟和成本，为每个任务选择最佳模型。重试策略根据任务复杂度动态调整。
- **L2 — 长任务规划。** 自动决定并行度、子任务拆分和检查点间隔。
- **L3 — Prompt 与记忆塑形。** 系统提示词和记忆上下文根据累积反馈逐请求优化和裁剪。

策略版本管理，支持灰度发布、硬门限回滚和审计日志。引擎随时间自动改进，无需手动调参。

### 真并发

Rust 的 tokio 运行时提供真正的并行执行 — 不是 Python 的协作式 asyncio。`JoinSet` 将工具调用分发到 OS 线程。30 秒的浏览器抓取不会阻塞 50ms 的文件读取。Gateway 同时处理 17 个平台的消息，没有 GIL。

### 17 个平台适配器

Telegram、Discord、Slack、WhatsApp、Signal、Matrix、Mattermost、钉钉、飞书、企业微信、微信、Email、SMS、BlueBubbles、Home Assistant、Webhook、API Server。

### 30 个工具后端

文件操作、终端、浏览器、代码执行、网页搜索、视觉、图像生成、TTS、语音转写、记忆、消息、委托、定时任务、技能、会话搜索、Home Assistant、RL 训练、URL 安全检查、OSV 漏洞检查等。
内置 `memory` 工具已对齐 Python 语义：`action=add|replace|remove`、`target=memory|user`，并使用 `old_text` 子串匹配 replace/remove。
内置 memory 存储容量也与 Python 默认一致：`memory` ≈ 2200 字符，`user` ≈ 1375 字符。
内置 `session_search` 支持 Python 风格双模式：省略 `query` 可浏览最近会话；带 `query` 可关键词召回，支持 `role_filter` 且 `limit` 上限为 5。
`session_search` 在有辅助模型凭据时可进行按会话 LLM 摘要（`HERMES_SESSION_SEARCH_SUMMARY_API_KEY` 或 `OPENAI_API_KEY`，可选 base/model 覆盖）。

### 8 个记忆插件

Mem0、Honcho、Holographic、Hindsight、ByteRover、OpenViking、RetainDB、Supermemory。

### 6 个终端后端

Local、Docker、SSH、Daytona、Modal、Singularity。

### MCP（Model Context Protocol）支持

内置 MCP 客户端和服务端。连接外部工具提供者，或将 Hermes 工具暴露给其他 MCP 兼容的 Agent。

### ACP（Agent Communication Protocol）

Agent 间通信，支持会话管理、事件流和权限控制。

---

## 架构

### 16 个 Crate 的 Workspace

```
crates/
├── hermes-core           # 共享类型、trait、错误层级
├── hermes-agent          # Agent loop、LLM provider、上下文、记忆插件
├── hermes-tools          # 工具注册、分发、30 个工具后端
├── hermes-gateway        # 消息网关、17 个平台适配器
├── hermes-cli            # CLI/TUI 二进制、斜杠命令
├── hermes-config         # 配置加载、合并、YAML 兼容
├── hermes-intelligence   # 自进化引擎、模型路由、Prompt 构建
├── hermes-skills         # 技能管理、存储、安全守卫
├── hermes-environments   # 终端后端（Local/Docker/SSH/Daytona/Modal/Singularity）
├── hermes-cron           # Cron 调度和持久化
├── hermes-mcp            # Model Context Protocol 客户端/服务端
├── hermes-acp            # Agent Communication Protocol
├── hermes-rl             # 强化学习运行
├── hermes-http           # HTTP/WebSocket API 服务
├── hermes-auth           # OAuth 令牌交换
└── hermes-telemetry      # OpenTelemetry 集成
```

### 基于 Trait 的抽象

| Trait | 用途 | 实现 |
|-------|------|------|
| `LlmProvider` | LLM API 调用 | OpenAI, Anthropic, OpenRouter, Generic |
| `ToolHandler` | 工具执行 | 30 个工具后端 |
| `PlatformAdapter` | 消息平台 | 17 个平台 |
| `TerminalBackend` | 命令执行 | Local, Docker, SSH, Daytona, Modal, Singularity |
| `MemoryProvider` | 持久化记忆 | 8 个记忆插件 + 文件/SQLite |
| `SkillProvider` | 技能管理 | 文件存储 + Hub |

---

## 安装

下载对应平台的最新 release 二进制：

```bash
# macOS (Apple Silicon)
curl -LO https://github.com/Lumio-Research/hermes-agent-rs/releases/latest/download/hermes-macos-aarch64.tar.gz
tar xzf hermes-macos-aarch64.tar.gz && sudo mv hermes /usr/local/bin/

# macOS (Intel)
curl -LO https://github.com/Lumio-Research/hermes-agent-rs/releases/latest/download/hermes-macos-x86_64.tar.gz
tar xzf hermes-macos-x86_64.tar.gz && sudo mv hermes /usr/local/bin/

# Linux (x86_64)
curl -LO https://github.com/Lumio-Research/hermes-agent-rs/releases/latest/download/hermes-linux-x86_64.tar.gz
tar xzf hermes-linux-x86_64.tar.gz && sudo mv hermes /usr/local/bin/

# Linux (ARM64)
curl -LO https://github.com/Lumio-Research/hermes-agent-rs/releases/latest/download/hermes-linux-aarch64.tar.gz
tar xzf hermes-linux-aarch64.tar.gz && sudo mv hermes /usr/local/bin/

# Linux (musl / Alpine / Docker)
curl -LO https://github.com/Lumio-Research/hermes-agent-rs/releases/latest/download/hermes-linux-x86_64-musl.tar.gz
tar xzf hermes-linux-x86_64-musl.tar.gz && sudo mv hermes /usr/local/bin/
```

所有 release 二进制：https://github.com/Lumio-Research/hermes-agent-rs/releases

## 从源码构建

```bash
cargo build --release
```

## 运行

```bash
hermes              # 交互式聊天
hermes --help       # 所有命令
hermes gateway start  # 启动多平台网关
hermes doctor       # 检查依赖和配置
```

## 测试

```bash
cargo test --workspace   # 641 个测试
```

## 许可证

MIT — 见 [LICENSE](LICENSE)。

基于 [Nous Research](https://nousresearch.com) 的 [Hermes Agent](https://github.com/NousResearch/hermes-agent)。
