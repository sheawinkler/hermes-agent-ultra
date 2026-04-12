# Hermes Agent (Rust)

**[English](./README.md)** | **[中文](./README_ZH.md)** | **[日本語](./README_JA.md)** | **[한국어](./README_KO.md)**

[Hermes Agent](https://github.com/NousResearch/hermes-agent) 的 Rust 重写版 — [Nous Research](https://nousresearch.com) 出品的自我进化 AI Agent。

---

## 为什么用 Rust？真正的价值

### Python AI Agent 的天花板

今天的 Python AI Agent 有一个公开的秘密：它们都是**单用户玩具**。在 $5 的 VPS 上跑一个，同时接 Telegram + Discord + Slack，10 个并发对话就能让它崩溃。Python GIL、asyncio 的协作式调度、满天飞的 `Dict[str, Any]`，意味着：

- **一个对话卡住，所有对话都卡。** 某个 session 里一个慢工具调用，整个事件循环冻结。
- **内存膨胀。** 每个对话都是字典套字典，没人知道数据到底长什么样。50 个 session 的 gateway 轻松吃掉 2GB+ 内存。
- **静默损坏。** key 名打错（`"mesage"` 而不是 `"message"`），穿过所有层毫无察觉，直到打到 LLM API 返回垃圾。
- **部署摩擦。** `pip install` 带 40+ 依赖，版本冲突，平台特定的 wheel（试试在 ARM Linux 上装 `faster-whisper`），500MB 的 virtualenv。

这些不是 bug，是语言的天花板。

### Rust 到底改变了什么

**1. 单二进制，零依赖**

```bash
# Python：祈祷目标机器有 Python 3.11+、pip、venv 和兼容的 wheel
curl -fsSL install.sh | bash  # 装完 500MB+

# Rust：一个 15MB 的二进制，到处能跑
scp hermes user@server:~/
./hermes
```

这是最大的部署优势。一个能跑在树莓派、$3/月 VPS、断网服务器、Docker scratch 镜像上的 AI Agent。没有运行时，没有解释器，没有依赖地狱。对于边缘 AI、IoT、不能装 Python 的企业环境 — 这是唯一的路。

**2. 真并发，不是假并发**

Python 的 asyncio 是协作式的 — 一个工具调用做了 CPU 密集操作（解析 10MB JSON、正则匹配、上下文压缩），所有东西都被阻塞。Rust 的 tokio 给你：

- **真正的并行工具执行。** `JoinSet` 把工具调用分发到 OS 线程。30 秒的浏览器抓取不会阻塞 50ms 的文件读取。
- **无锁消息路由。** Gateway 可以同时处理 16 个平台的消息，没有 GIL。
- **可预测的延迟。** 没有 GC 暂停。不会在流式输出中途突然冻结 200ms。

对于服务 100+ 并发对话的多用户 gateway，这是"能用"和"稳定可用"的区别。

**3. 编译器即架构守卫**

Python 代码库有 9,913 行的 `run_agent.py` 和 7,905 行的 `gateway/run.py`。这些文件有机生长到这个体量，因为 Python 没有机制阻止它。任何文件可以 import 任何东西，任何函数可以修改任何全局变量，类型检查器是可选的且经常被忽略。

Rust 的 crate 系统让这在物理上不可能：

```
hermes-core          ← 定义 trait，不属于任何人
hermes-agent         ← 依赖 core，看不到 gateway
hermes-gateway       ← 依赖 core，看不到 agent 内部
hermes-tools         ← 依赖 core，看不到 provider 细节
```

循环依赖？编译错误。忘记处理错误？编译错误。传错消息类型给工具？编译错误。这不是纪律 — 是物理定律。架构不会随时间退化，因为编译器不允许。

**4. 类型安全，用在最关键的地方**

Python 版本里，"消息"是 `Dict[str, Any]`，"工具调用"是 `Dict[str, Any]`，"配置"是 `Dict[str, Any]`。出问题时，你在凌晨 3 点的生产环境收到一个 `KeyError`。

Rust 里：

```rust
pub struct Message {
    pub role: MessageRole,        // 枚举，不是字符串
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub reasoning_content: Option<ReasoningContent>,
    pub cache_control: Option<CacheControl>,
}
```

每个字段有类型，每个变体被枚举，每个错误路径被处理。LLM 返回意外 JSON？`serde` 在边界就捕获了。工具 handler 返回错误类型？编译不过。这消灭了困扰 Python Agent 生产环境的一整类 bug。

**5. 长期运行 Agent 的内存效率**

AI Agent 不是请求-响应服务器。它运行数天、数周、数月。它积累对话历史、技能文件、记忆条目、会话状态。Python 的引用计数 GC 和 dict 开销意味着内存不可预测地增长。

Rust 的所有权模型意味着：
- 会话结束的瞬间，对话历史就被释放。没有 GC 延迟。
- 工具结果在插入上下文后立即截断和释放。
- 100 个并发会话的全部 agent 状态只需 ~50MB，而不是 2GB。

对于在便宜 VPS 上 24/7 运行的个人 AI Agent，这是 $3/月和 $20/月的区别。

---

## 架构决策

### 基于 Trait 的抽象

每个集成点都是一个 trait：

| Trait | 用途 | 实现 |
|-------|------|------|
| `LlmProvider` | LLM API 调用 | OpenAI, Anthropic, OpenRouter, Generic |
| `ToolHandler` | 工具执行 | 18 种工具类型 |
| `PlatformAdapter` | 消息平台 | 16 个平台 |
| `TerminalBackend` | 命令执行 | Local, Docker, SSH, Daytona, Modal, Singularity |
| `MemoryProvider` | 持久化记忆 | 文件, SQLite |
| `SkillProvider` | 技能管理 | 文件存储 + Hub |

这意味着你可以替换任何组件而不影响其他部分。想加新的 LLM provider？实现 `LlmProvider`。新的消息平台？实现 `PlatformAdapter`。Agent loop 不知道也不关心。

### 错误层级

```
AgentError（顶层）
├── LlmApi(String)
├── ToolExecution(String)      ← 从 ToolError 自动转换
├── Gateway(String)            ← 从 GatewayError 自动转换
├── Config(String)             ← 从 ConfigError 自动转换
├── RateLimited { retry_after_secs }
├── Interrupted { message }
├── ContextTooLong
├── MaxTurnsExceeded
└── Io(String)
```

每种错误类型通过 `From` trait 自动转换。编译器确保每个错误路径都被处理。不再有 `except Exception: pass`。

### Workspace 结构

```
crates/
├── hermes-core           # 共享类型、trait、错误类型
├── hermes-agent          # Agent loop、provider、上下文、记忆
├── hermes-tools          # 工具注册、分发、所有工具实现
├── hermes-gateway        # 消息网关、平台适配器
├── hermes-cli            # CLI 二进制、TUI、命令
├── hermes-config         # 配置加载和合并
├── hermes-intelligence   # Prompt 构建、模型路由、用量追踪
├── hermes-skills         # 技能管理、存储、安全守卫
├── hermes-environments   # 终端后端
├── hermes-cron           # Cron 调度
└── hermes-mcp            # Model Context Protocol
```

---

## 竞争壁垒

AI Agent 赛道挤满了 Python 项目，它们都撞上了同一个天花板。能活下来的是那些能做到：

1. **到处能跑** — 不只是开发者 MacBook 上装了 Python 3.11 和 40 个 pip 包，而是边缘设备、嵌入式系统、断网的企业服务器、$3 的 VPS。

2. **多用户扩展** — 用一个进程服务一个团队、一个家庭、一个社区，不会因为一个对话拖慢其他所有对话。

3. **长期稳定运行** — 没有内存泄漏，没有 GC 暂停，没有在长期运行的会话中悄悄积累的类型错误。

4. **嵌入其他系统** — Rust 库可以被 C、C++、Python（PyO3）、Node.js（napi）、Go（CGo）和 WASM 调用。Python Agent 只能被 Python 调用。

Rust 重写不是为了让 LLM API 调用快几毫秒（那些本来就是 I/O bound 的）。而是构建让 AI Agent 达到生产级的**基础设施层**：可部署、可嵌入、可靠、高效。

---

## 当前状态

早期阶段。架构和核心抽象已经稳固。与 Python 版本的功能对等度约 10%。详见 [GAP_ANALYSIS.md](./GAP_ANALYSIS.md)。

已完成：
- Agent loop（流式输出、中断处理、并行工具执行）
- 4 个 LLM provider（OpenAI、Anthropic、OpenRouter、Generic）
- 工具注册和分发框架
- 16 个平台适配器结构
- 6 个终端后端
- 技能管理 + 安全守卫
- 配置加载和合并
- 会话持久化（SQLite）
- Cron 调度框架
- 524 个测试通过

## 构建

```bash
cargo build --release
```

## 运行

```bash
cargo run --release -p hermes-cli
```

## 测试

```bash
cargo test --workspace
```

## 许可证

MIT — 见 [LICENSE](LICENSE)。

基于 [Nous Research](https://nousresearch.com) 的 [Hermes Agent](https://github.com/NousResearch/hermes-agent)。
