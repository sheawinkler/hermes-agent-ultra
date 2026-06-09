# hermes-acp-server -- 启动模型与状态管理补充

> 补充主体方案文档 `docs/plans/acp-server-plan.md` 中未详细设计的运行时交互部分。

---

## 一、对公共组件的改动清单

所有核心逻辑在 `hermes-acp-server` crate 内实现。对其他 crate 的改动**最小化**：

| 改动 | crate | 文件 | 范围 |
|------|-------|------|------|
| workspace members 加一行 | 根目录 | `Cargo.toml` | 1 行 |
| 新增 dependency | 根目录 | `Cargo.toml` | 1 行 |
| 新增 dependency | `hermes-cli` | `Cargo.toml` | 1 行 |
| `SLASH_COMMANDS` 加一条 | `hermes-cli` | `commands.rs` | 1 行常量 |
| `handle_slash_command` match 加分支 | `hermes-cli` | `commands.rs` | 1 行路由 |
| autocomplete 加一组 | `hermes-cli` | `commands.rs` | 1 行 |
| help 文本加一条 | `hermes-cli` | `commands.rs` | 1 行 |
| `App` struct 加一个字段 | `hermes-cli` | `app.rs` | `pub acp_server: Option<Arc<AcpPipeServer>>` |
| 新增 handler 文件 | `hermes-cli` | `acp_command.rs` | **新文件**，所有 ACP slash command 逻辑 |

**不修改**：`hermes-acp`、`hermes-core`、`hermes-agent`、`hermes-tools`、`hermes-gateway` 等任何其他 crate。

---

## 二、Slash Command 设计

### 2.1 命令注册

```
("/acp", "ACP Agent Server controls (start|stop|status|restart|connections)")
```

子命令：

| 子命令 | 说明 |
|--------|------|
| `start` | 后台启动 ACP Pipe Server |
| `stop` | 停止 ACP Pipe Server，断开所有客户端 |
| `status` | 显示当前状态（监听端点、连接数、Cherry 在线状态） |
| `restart` | stop + start |
| `connections` | 列出所有活跃连接的详细信息 |

无参数时等同于 `status`。

### 2.2 后台运行模型

`/acp start` 的执行流程：

```
用户输入 /acp start
  |
  +-- 检查 App.acp_server 是否已存在
  |     |-- 已存在 --> 输出 "[ACP server already running]" + 状态摘要
  |     |-- 不存在 --> 继续
  |
  +-- 创建 AcpServerConfig
  |     |-- pipe_path: 从 config 或默认值
  |     |-- agent_info: name="hermes-agent", title="Hermes Agent Ultra", version=...
  |     |-- executor: 创建 HermesExecutor（持有 App.agent 的 Arc 引用）
  |
  +-- AcpPipeServer::new(config)
  |
  +-- tokio::spawn(server.run())
  |     |-- accept 循环在独立 tokio task 中运行
  |     |-- 不阻塞主对话循环
  |
  +-- App.acp_server = Some(Arc::new(server))
  |
  +-- 输出 "[ACP server started on \\.\pipe\AIPC-acp]"
```

关键：`AcpPipeServer` 本身是 `Send + Sync + 'static`，可以安全地放在 `Arc` 中。`run()` 方法在 spawn 的 task 内执行，通过 `AtomicBool` 接收 shutdown 信号。

### 2.3 停机流程

```
用户输入 /acp stop
  |
  +-- 检查 App.acp_server
  |     |-- None --> "[ACP server not running]"
  |     |-- Some --> 继续
  |
  +-- server.shutdown()  // 设置 AtomicBool，accept 循环退出
  |     |-- 所有活跃连接被优雅关闭
  |     |-- Cherry 收到 pipe 断开，进入重连模式
  |
  +-- App.acp_server = None
  |
  +-- 输出 "[ACP server stopped]"
```

### 2.4 状态展示

`/acp status` 输出格式：

**运行中：**
```
ACP Server: running
Endpoint: \\.\pipe\AIPC-acp
Connections: 1/5
  [1] ai-cherry / AI_Cherry v1.0.0  session: acp:main:a3f2...  mode: code-assistant
Uptime: 2h 15m
```

**未运行：**
```
ACP Server: stopped
Use /acp start to begin listening.
```

---

## 三、App struct 集成

### 3.1 新增字段

```rust
// hermes-cli/src/app.rs

pub struct App {
    // ... 现有字段 ...
    
    /// Background ACP Pipe Server (started via /acp start).
    #[cfg(feature = "acp-server")]
    pub acp_server: Option<Arc<hermes_acp_server::AcpPipeServer>>,
}
```

使用 feature gate `acp-server` 控制，不启用 feature 时该字段不存在，零影响。

### 3.2 HermesExecutor 实现

在 `hermes-cli` 中新建 `acp_command.rs`，包含：

```rust
use hermes_acp_server::{PromptExecutor, PipeSession, StreamEvent, PromptResult};
use hermes_acp_server::protocol::{StopReason, Usage};

/// Bridge between ACP PromptExecutor trait and the hermes agent loop.
pub(crate) struct HermesExecutor {
    pub agent: Arc<AgentLoop>,
    pub model: String,
}

#[async_trait]
impl PromptExecutor for HermesExecutor {
    async fn execute(
        &self,
        session: &PipeSession,
        prompt_text: &str,
        history: &[Value],
        event_tx: tokio::sync::mpsc::Sender<StreamEvent>,
    ) -> Result<PromptResult, String> {
        // 1. 将 ACP history 转换为 hermes Message 格式
        // 2. 将 prompt_text 作为 user message 注入 agent loop
        // 3. 在 streaming callback 中：
        //      event_tx.send(StreamEvent::AgentMessageChunk {
        //          content: StreamContent::Text { text: delta }
        //      }).await
        // 4. agent 完成后返回 PromptResult
        todo!("Phase 3 实现")
    }
}
```

### 3.3 /acp handler 实现

```rust
// hermes-cli/src/acp_command.rs

pub(crate) fn handle_acp_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let action = args.first().copied().unwrap_or("status");
    match action {
        "start" => start_acp_server(app),
        "stop" => stop_acp_server(app),
        "status" => show_acp_status(app),
        "restart" => {
            stop_acp_server(app)?;
            start_acp_server(app)
        }
        "connections" => show_acp_connections(app),
        _ => {
            emit_command_output(
                app,
                "Usage: /acp [start|stop|status|restart|connections]",
            );
            Ok(CommandResult::Handled)
        }
    }
}
```

---

## 四、生命周期时序

```
Hermes 启动
  |-- App::new() 初始化
  |-- acp_server = None  (不自动启动)
  |-- 主对话循环开始
  |
  |-- 用户输入 /acp start
  |     |-- spawn AcpPipeServer::run()
  |     |-- acp_server = Some(...)
  |     |-- pipe 端点开始监听
  |
  |-- Cherry (AI_Router) 启动
  |     |-- 连接 \\.\pipe\AIPC-acp
  |     |-- initialize 握手
  |     |-- session/new
  |     |-- session/prompt → agent loop 执行 → 流式推送
  |
  |-- 用户继续在 Hermes 主对话中正常聊天（不受影响）
  |
  |-- 用户输入 /acp stop
  |     |-- server.shutdown()
  |     |-- Cherry 断连，进入重连等待
  |     |-- acp_server = None
  |
  |-- 用户输入 /quit
  |     |-- 如果 acp_server 在运行 → 自动 shutdown
  |     |-- 退出
```

**ACP prompt 和用户主对话共享同一个 agent loop**（与 FlowyClaw 的 main session 模型一致）。Cherry 的 prompt 和用户在终端的输入使用同一个 session，上下文自然衔接。

---

## 五、Feature Gate 策略

```toml
# hermes-cli/Cargo.toml

[features]
default = []
acp-server = ["hermes-acp-server"]
```

- 默认不启用，`cargo build` 不包含 ACP server 功能
- 启用：`cargo build --features acp-server`
- 不启用时：`App` 没有 `acp_server` 字段，`/acp` 命令不存在，零开销

---

## 六、需要讨论的决策点

### 6.1 共享 session 还是独立 session

**共享 session**（与 FlowyClaw 一致）：
- Cherry 的 prompt 和用户终端输入共用 agent 的对话历史
- 优点：上下文连贯，Cherry 能"看到"用户之前在终端聊的内容
- 缺点：两条输入通道可能交错，需要串行化

**独立 session**：
- Cherry 有自己的 agent session，与终端用户的 session 分离
- 优点：互不干扰
- 缺点：上下文不共享，Cherry 无法获知终端对话内容

建议：**MVP 先用独立 session**，后续迭代可选共享。

### 6.2 是否支持 config 文件配置

是否允许在 `hermes` 的 config 文件中设置：

```yaml
acp_server:
  enabled: true           # 启动时自动 start
  pipe_path: "\\.\pipe\AIPC-acp"
  max_connections: 5
```

如果 `enabled: true`，Hermes 启动时自动执行 `/acp start`，无需手动输入。

建议：**MVP 不做**，纯 slash command 手动控制。后续迭代加 config 支持。
