# hermes-acp-server 实现方案

> **分支**：基于 `feat/wecom_websocket` 新建 `feat/acp-server`
> **crate 名**：`hermes-acp-server`
> **策略**：选项 B -- 复用 `hermes-acp` 的协议类型，独立实现传输层与事件机制
> **参考文档**：`D:\workSpace\AI_Router\docs\` + `D:\workSpace\git_clone_test\FlowyClaw\docs\acp\AI_Router\`

---

## 一、目标

为 Hermes Agent 新增一个独立的 ACP Agent Server，通过跨平台 IPC（Windows Named Pipe / Unix Domain Socket）提供 ACP 协议服务，使 AI_Router（Cherry）等外部 ACP Client 能够连接并驱动 Hermes Agent。

核心原则：

- **独立 crate**，不修改 `hermes-acp` 或其他现有 crate 的源码
- **跨平台**：Windows Named Pipe + Unix Domain Socket，条件编译切换
- **实时推流**：prompt 执行期间通过 mpsc channel 实时推送 `session/update`
- **与 wecom 分支隔离**：所有改动在 `hermes-acp-server` crate 内完成

---

## 二、依赖关系

```
hermes-acp-server
  |-- hermes-acp          # 仅用协议类型（AcpRequest/AcpResponse/SessionUpdate/...）和 AcpHandler trait
  |-- tokio               # async runtime + IPC (named_pipe / net::UnixListener)
  |-- serde / serde_json  # JSON 序列化
  |-- tracing             # 日志
  |-- uuid                # session ID 生成
  |-- async-trait         # trait 定义

被依赖：
hermes-cli ----> hermes-acp-server   # CLI 的 `acp serve` 命令
```

**不依赖**：`hermes-agent`、`hermes-gateway`、`hermes-tools` 等业务 crate。通过 `PromptExecutor` trait 解耦。

---

## 三、crate 结构

```
crates/hermes-acp-server/
|-- Cargo.toml
|-- src/
    |-- lib.rs              # 公共 API + re-exports
    |-- server.rs           # AcpPipeServer -- IPC accept 循环 + 多连接管理
    |-- connection.rs       # AcpConnection -- 单连接状态机 + NDJSON 读写
    |-- ndjson.rs           # NDJSON 行缓冲（LF only，防御 CRLF）
    |-- event_bridge.rs     # mpsc 事件桥接 -- 实时 session/update 推送
    |-- executor.rs         # PromptExecutor trait -- 向外委托 agent 执行
    |-- session.rs          # PipeSession -- 连接级 session 状态（轻量，不依赖 hermes-acp::SessionManager）
    |-- platform/
        |-- mod.rs          # IpcListener / IpcStream trait + 工厂函数
        |-- windows.rs      # Named Pipe 实现（tokio::net::windows::named_pipe）
        |-- unix.rs         # Unix Domain Socket 实现（tokio::net::UnixListener）
```

---

## 四、模块详细设计

### 4.1 platform/ -- 跨平台 IPC 抽象

```rust
// platform/mod.rs

/// 跨平台 IPC 监听器。
pub trait IpcListener: Send + Sync {
    /// 接受一个新连接。
    async fn accept(&self) -> Result<Box<dyn IpcStream>, IpcError>;
    /// 返回 IPC 端点地址（用于日志）。
    fn endpoint(&self) -> &str;
}

/// 跨平台 IPC 数据流。
pub trait IpcStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {
    fn peer_label(&self) -> String;
}

/// 根据平台和配置创建 IpcListener。
pub fn create_listener(pipe_path: &str) -> Result<Box<dyn IpcListener>, IpcError>;
```

| 平台 | 实现 | 默认路径 |
|------|------|---------|
| Windows | `tokio::net::windows::named_pipe::ServerOptions` | `\\.\pipe\AIPC-acp` |
| Linux/macOS | `tokio::net::UnixListener` | `/tmp/hermes-acp.sock` |

`pipe_path` 通过 `AcpServerConfig` 可配置，不硬编码。

### 4.2 ndjson.rs -- NDJSON 行缓冲

```rust
pub struct NdjsonReader<R> {
    inner: BufReader<R>,
    buffer: String,
}

impl<R: AsyncRead + Unpin> NdjsonReader<R> {
    /// 读取下一行完整 NDJSON（以 \n 分隔，自动剥离 \r）。
    pub async fn read_line(&mut self) -> Option<Result<String, NdjsonError>>;

    /// 检查连接是否关闭。
    pub fn is_eof(&self) -> bool;
}

pub struct NdjsonWriter<W> {
    inner: W,
}

impl<W: AsyncWrite + Unpin> NdjsonWriter<W> {
    /// 写入一行 NDJSON（自动追加 \n，使用 LF 不用 CRLF）。
    pub async fn write_json(&mut self, value: &Value) -> io::Result<()>;

    /// 写入原始 NDJSON 行。
    pub async fn write_line(&mut self, line: &str) -> io::Result<()>;
}
```

关键约束（来自 NDJSON 踩坑文档）：
- 每行必须以 `\n` 结尾，永远使用 LF 不用 CRLF
- 一个 frame 不等于一行 -- 行缓冲自行按 `\n` 分割
- 剥离尾部 `\r`（防御性处理 Windows 服务端可能发送的 CRLF）

### 4.3 executor.rs -- Prompt 执行委托

```rust
/// 流式事件，由 executor 在 prompt 执行期间实时推送。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum StreamEvent {
    AgentMessageChunk {
        content: StreamContent,
    },
    AgentThoughtChunk {
        content: StreamContent,
    },
    ToolCall {
        tool_call_id: String,
        title: String,
        kind: String,
        raw_input: Option<Value>,
        status: String,  // "pending" | "completed"
    },
    ToolCallUpdate {
        tool_call_id: String,
        status: String,
        content: Vec<StreamContent>,
    },
}

/// 流式内容块 -- 独立于 hermes-acp 的 ContentBlock，
/// 确保序列化格式与 Cherry ACP SDK Zod 校验完全对齐。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamContent {
    Text { text: String },
    Image { url: String, #[serde(skip_serializing_if = "Option::is_none")] alt: Option<String> },
}

/// Prompt 执行结果（最终返回给客户端）。
pub struct PromptResult {
    pub stop_reason: StopReason,
    pub usage: Option<Usage>,
}

/// 向外委托的 prompt 执行 trait。
/// hermes-cli 提供具体实现，桥接到 hermes-agent 的 agent loop。
#[async_trait]
pub trait PromptExecutor: Send + Sync {
    async fn execute(
        &self,
        session: &PipeSession,
        prompt_text: &str,
        history: &[Value],
        event_tx: tokio::sync::mpsc::Sender<StreamEvent>,
    ) -> Result<PromptResult, String>;
}
```

**不直接依赖 `hermes-agent`**。`hermes-cli` 在集成时提供实现：

```rust
// hermes-cli 侧（示意，不在本 crate 内）
struct HermesExecutor { /* agent handle */ }

impl PromptExecutor for HermesExecutor {
    async fn execute(&self, session, prompt, history, event_tx) -> Result<PromptResult, String> {
        // 调用 agent loop，在每个 streaming delta 时:
        //   event_tx.send(StreamEvent::AgentMessageChunk { ... }).await
        // agent 完成时返回 PromptResult
    }
}
```

### 4.4 session.rs -- 连接级 session 状态

```rust
/// 单个 ACP 连接的 session 状态（轻量级）。
pub struct PipeSession {
    pub session_id: String,
    pub cwd: Option<String>,
    pub mode: Option<String>,       // session/set_mode 设置的当前 Skill
    pub client_name: Option<String>, // initialize 中的 clientInfo.name
    pub client_title: Option<String>,
    pub history: Vec<Value>,
    pub created_at: Instant,
}
```

不使用 `hermes-acp::SessionManager`（那个是为 stdin 单连接 + 持久化设计的）。新 crate 用自己的轻量 session 结构，每个连接一个实例。

### 4.5 connection.rs -- 单连接状态机

```rust
/// 连接级协议状态。
enum ConnectionState {
    Connected,
    Initialized,
    SessionReady,
    Active,  // prompt 正在执行
}

/// 管理一个 ACP 客户端连接的完整生命周期。
pub struct AcpConnection {
    state: ConnectionState,
    session: Option<PipeSession>,
    config: AcpServerConfig,
    executor: Arc<dyn PromptExecutor>,
    // 事件通道（prompt 执行期间实时推送）
    event_rx: Option<tokio::sync::mpsc::Receiver<StreamEvent>>,
}
```

协议处理流程：

```
客户端连接
  |
  +-- NDJSON read loop（一个 tokio task）
  |   +-- initialize      --> 状态 --> Initialized，提取 clientInfo
  |   +-- authenticate    --> 返回 -32601（Named Pipe 信任边界）
  |   +-- session/new     --> 创建 PipeSession，状态 --> SessionReady
  |   +-- session/prompt  --> 状态 --> Active，spawn executor + event_bridge
  |   +-- session/cancel  --> 取消活跃 prompt
  |   +-- session/ping    --> 返回空 result
  |   +-- session/set_mode--> 更新 session.mode
  |   +-- cherry/shutdown --> 通知 server 层
  |   +-- 未知方法       --> 返回 -32601
  |
  +-- NDJSON write loop（另一个 tokio task）
      +-- 收到 StreamEvent --> 格式化为 session/update notification --> 写入 pipe
      +-- 收到 prompt 完成 --> 写入 prompt result response
```

### 4.6 event_bridge.rs -- 实时事件桥接

```rust
/// 从 executor 的 mpsc channel 读取 StreamEvent，
/// 转换为 ACP session/update notification 格式，
/// 通过 NdjsonWriter 实时写入 IPC stream。
pub async fn bridge_events(
    mut rx: tokio::sync::mpsc::Receiver<StreamEvent>,
    writer: &mut NdjsonWriter<impl AsyncWrite + Unpin>,
    session_id: &str,
) {
    while let Some(event) = rx.recv().await {
        let notification = format_session_update(session_id, &event);
        if let Err(e) = writer.write_json(&notification).await {
            tracing::warn!("event bridge write error: {e}");
            break;
        }
    }
}

/// 将 StreamEvent 转换为 Cherry 期望的 session/update notification 格式。
fn format_session_update(session_id: &str, event: &StreamEvent) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": event  // StreamEvent 的 Serialize 输出
        }
    })
}
```

`StreamEvent` 的 `Serialize` 输出需要与 Cherry 期望的格式一致。关键字段映射：

| StreamEvent 变体 | Cherry 期望的 sessionUpdate 值 |
|---|---|
| `AgentMessageChunk` | `"agent_message_chunk"` |
| `AgentThoughtChunk` | `"agent_message_chunk"`（reasoning content） |
| `ToolCall` | `"tool_call"` |
| `ToolCallUpdate` | `"tool_call_update"` |

`StreamContent` 必须包含 `type` 字段（ACP SDK Zod 校验要求）。

### 4.7 server.rs -- ACP Pipe Server

```rust
/// ACP Pipe Server 配置。
pub struct AcpServerConfig {
    /// IPC 端点路径。
    /// Windows 默认: `\\.\pipe\AIPC-acp`
    /// Unix 默认: `/tmp/hermes-acp.sock`
    pub pipe_path: String,
    /// 最大并发连接数（默认 5）。
    pub max_connections: usize,
    /// Agent 信息（initialize 响应用）。
    pub agent_info: AgentInfo,
    /// Prompt 执行器。
    pub executor: Arc<dyn PromptExecutor>,
}

/// Agent 品牌信息。
pub struct AgentInfo {
    pub name: String,    // 如 "hermes-agent"
    pub title: String,   // 如 "Hermes Agent Ultra"，Cherry 用来显示
    pub version: String,
}

/// ACP Pipe Server 主结构。
pub struct AcpPipeServer {
    config: AcpServerConfig,
    listener: Box<dyn IpcListener>,
    connections: Arc<Mutex<HashMap<String, Arc<AcpConnection>>>>,
    shutdown: AtomicBool,
}

impl AcpPipeServer {
    pub async fn new(config: AcpServerConfig) -> Result<Self, AcpServerError>;
    
    /// 启动 accept 循环，为每个连接 spawn 处理 task。
    pub async fn run(&self) -> Result<(), AcpServerError>;
    
    /// 优雅停机。
    pub async fn shutdown(&self);
    
    /// 当前活跃连接数。
    pub fn connection_count(&self) -> usize;
    
    /// 是否有 Cherry 客户端连接。
    pub fn has_cherry_client(&self) -> bool;
}
```

accept 循环伪码：

```
loop {
    if shutdown -> break
    if connections.len() >= max_connections -> 拒绝新连接 + warn
    
    stream = listener.accept().await
    
    conn_id = uuid()
    conn = AcpConnection::new(conn_id, config, executor)
    
    spawn task: conn.run(NdjsonReader(stream), NdjsonWriter(stream))
    
    connections.insert(conn_id, conn)
}
```

---

## 五、与 Cherry 协议的对齐

### 5.1 initialize 响应

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "protocolVersion": 1,
    "agentInfo": {
      "name": "hermes-agent",
      "title": "Hermes Agent Ultra",
      "version": "0.1.0"
    },
    "agentCapabilities": {
      "promptCapabilities": { "streaming": true },
      "sessionCapabilities": { "fork": false, "list": false, "resume": false }
    },
    "authMethods": []
  }
}
```

### 5.2 authenticate

Named Pipe 信任边界，返回 `-32601`（Method not found）。

### 5.3 session/new

从 `_meta` 提取 `source`/`channel`/`skillId`，创建 `PipeSession`。

```json
响应: { "sessionId": "acp:main:<uuid>" }
```

### 5.4 session/prompt

1. 创建 `mpsc::channel<StreamEvent>(128)`
2. spawn `event_bridge` task：`rx` -> NDJSON write
3. 调用 `executor.execute(session, prompt, history, tx)`
4. executor 执行期间通过 `tx.send(StreamEvent::AgentMessageChunk{...})` 实时推送
5. executor 返回 `PromptResult`
6. 发送 prompt result response

```json
// 流式推送（多次）
{ "jsonrpc":"2.0", "method":"session/update", "params":{ "sessionId":"...", "update":{ "sessionUpdate":"agent_message_chunk", "content":{ "type":"text", "text":"你好" }}}}

// 最终结果
{ "jsonrpc":"2.0", "id":3, "result":{ "stopReason":"end_turn" }}
```

### 5.5 session/ping

```json
{ "jsonrpc":"2.0", "id":4, "result":{} }
```

### 5.6 session/cancel

清理活跃 prompt 的 event channel，发送空 result。

### 5.7 cherry/shutdown

收到后标记 server shutdown flag，当前连接处理完毕后停止 accept。

---

## 六、ContentBlock 格式约束

Cherry 的 ACP SDK 使用 Zod 校验，以下字段 **必须** 正确：

| 约束 | 说明 |
|------|------|
| `ContentBlock.type` | 必须包含 `"type": "text"` / `"image"` 等，缺少则 notification 被**静默丢弃** |
| `tool_call.status` | 只能用 `"pending"` / `"completed"`，`"in_progress"` 等会被静默丢弃 |
| NDJSON `\n` | 每行必须以 LF 结尾，缺少会导致整个通信链永久阻塞 |
| LF only | 不使用 CRLF，客户端仅归一化尾部 CRLF，内嵌 CRLF 不处理 |

---

## 七、实现步骤

### Phase 1：基础设施（可独立编译验证）

| 步骤 | 内容 | 验证 |
|------|------|------|
| 1.1 | 创建 crate 骨架 + `Cargo.toml` + workspace 注册 | `cargo build -p hermes-acp-server` 编译通过 |
| 1.2 | `ndjson.rs` -- NDJSON 行缓冲 reader/writer | 单元测试：模拟 chunk 分割、CRLF 归一化 |
| 1.3 | `platform/mod.rs` -- IpcListener/IpcStream trait | 编译通过 |
| 1.4 | `platform/windows.rs` -- Named Pipe 实现 | 编译通过（Windows 条件编译） |
| 1.5 | `platform/unix.rs` -- Unix Domain Socket 实现 | 编译通过（Unix 条件编译） |

### Phase 2：协议处理

| 步骤 | 内容 | 验证 |
|------|------|------|
| 2.1 | `executor.rs` -- PromptExecutor trait + StreamEvent + StreamContent | 编译通过 |
| 2.2 | `session.rs` -- PipeSession | 编译通过 |
| 2.3 | `connection.rs` -- 状态机 + NDJSON dispatch | 单元测试：initialize/authenticate/session/new/ping 路径 |
| 2.4 | `event_bridge.rs` -- mpsc -> NDJSON 转换 | 单元测试：StreamEvent 序列化格式符合 Cherry 期望 |

### Phase 3：Server + 集成

| 步骤 | 内容 | 验证 |
|------|------|------|
| 3.1 | `server.rs` -- AcpPipeServer accept 循环 | 集成测试：Mock client 连接 + 完整握手 |
| 3.2 | `lib.rs` -- 公共 API + re-exports | 编译通过 |
| 3.3 | CLI 集成 -- `hermes acp serve` 命令（在 hermes-cli crate 中） | 手动验证：启动 server -> Cherry 连接 |
| 3.4 | HermesExecutor 实现（在 hermes-cli crate 中） | 端到端：Cherry 发送 prompt -> 收到流式响应 |

### Phase 4：边界处理

| 步骤 | 内容 | 验证 |
|------|------|------|
| 4.1 | 串行 prompt 保护（同一连接同时只有一个 prompt） | 测试：并发 prompt 第二个被拒绝 |
| 4.2 | 连接断开时清理资源 | 测试：client drop -> server 日志 + 资源释放 |
| 4.3 | `cherry/shutdown` 处理 | 测试 |
| 4.4 | `session/set_mode` 支持 | 测试 |

---

## 八、不包含在 MVP 中的功能

| 功能 | 计划阶段 |
|------|---------|
| Permission 流程（requestPermission） | Phase 2 迭代 |
| daemon 模式 + PID 文件 | Phase 2 迭代 |
| 心跳检测 + 超时断连 | Phase 2 迭代 |
| session/load、session/fork、session/resume | Phase 2 迭代 |
| `tool_call_update` 中的 `kind` 字段（read/write/...） | 后续 |

---

## 九、对现有 crate 的影响

| crate | 改动 |
|-------|------|
| `hermes-acp` | **零改动**。新 crate 只引用其 protocol types 和 AcpHandler trait |
| `hermes-core` | **零改动** |
| `hermes-agent` | **零改动** |
| `hermes-cli` | **微小改动**：新增 `acp serve` 子命令，提供 HermesExecutor 实现 |
| 根 `Cargo.toml` | 新增 `hermes-acp-server` 到 workspace members |

---

## 十、风险与缓解

| 风险 | 缓解 |
|------|------|
| `hermes-acp` 的 `SessionUpdate` enum 与 Cherry 期望的 `sessionUpdate` 字段名不匹配 | 新 crate 定义自己的 `StreamEvent`，`#[serde(tag = "sessionUpdate")]` 精确控制序列化 |
| `hermes-acp` 的 `ContentBlock` enum 缺少 `type` discriminator | 新 crate 使用 `#[serde(tag = "type")]` 自定义 `StreamContent`，不依赖 hermes-acp 的 ContentBlock |
| tokio Named Pipe 在 Windows 上的 accept 模式与 Unix 不同 | 已通过 `IpcListener` trait 抽象隔离，Windows 用 `ServerOptions::create` + `connect()` 循环，Unix 用标准 `UnixListener::accept()` |
| Cherry SDK Zod 校验静默丢弃格式不匹配的 notification | 在 `event_bridge` 中增加 debug 日志，记录每条发出的 notification 完整 JSON |

---

## 十一、后续迭代方向

1. **Permission 流程**：`requestPermission` -> Cherry 展示 UI -> 用户批准/拒绝 -> 回传给 server
2. **心跳**：连接空闲超时 + 定期 ping
3. **daemon 模式**：server 作为后台进程运行，PID 文件管理
4. **多 session 支持**：同一连接多个 session（当前 MVP 为单 session）
5. **Gateway 集成**：如果 hermes-gateway 已在运行，ACP server 可桥接到 gateway 而非直接调用 agent loop
