# Hermes Agent (Rust)

**[English](./README.md)** | **[中文](./README_ZH.md)** | **[日本語](./README_JA.md)** | **[한국어](./README_KO.md)**

[Hermes Agent](https://github.com/NousResearch/hermes-agent) 的生产级 Rust 重写版 — [Nous Research](https://nousresearch.com) 出品的自我进化 AI Agent。

**84,000+ 行 Rust 代码 · 16 个 crate · 641 个测试 · 17 个平台适配器 · 30 个工具后端 · 8 个记忆插件 · 6 个跨平台发布目标**

---

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
