# Reasonix vs Hermes: 缓存架构详细对比文档

> 基于对两个项目源码的全面阅读生成。
> - **Reasonix**: `C:\Users\test\Desktop\reference\DeepSeek-Reasonix` (Go)
> - **Hermes**: `c:\Users\test\Desktop\work\hermes-agent-ultra` (Rust + Python 遗留)

---

## 一、总体架构对比

| 维度 | Reasonix (Go) | Hermes Agent Ultra (Rust) |
|------|---------------|---------------------------|
| **目标平台** | DeepSeek (prefix caching 自动) | Anthropic Claude (cache_control 显式标记) |
| **缓存触发方式** | **被动**: DeepSeek 服务端自动检测字节相同的前缀 | **主动**: 客户端在消息上注入 `cache_control: {"type":"ephemeral"}` |
| **核心策略** | Append-only + 压缩时重写 | system_and_3 breakpoints + 客户端缓存 |
| **缓存诊断** | ✅ PrefixShape + CacheDiagnostics | ⚠️ 仅 Prometheus 指标，无逐轮诊断 |
| **客户端缓存** | ❌ 无（直接发送到 API） | ✅ 多级 hash-keyed 缓存 |
| **上下文压缩** | ✅ 单层（50%/80%/90% 三级阈值） | ✅ 双保险（Gateway 85% + Agent 50%） |
| **Token 省流** | ✅ 剪枝 + Token 经济模式 | ❌ 无 token 经济模式 |

---

## 二、核心缓存策略详细对比

### 2.1 服务端 Prompt 缓存

#### Reasonix: 被动 DeepSeek Prefix Caching

```
┌───────────────────────────────────────────────────────┐
│ DeepSeek API 服务端自动检测                             │
│                                                       │
│ Request N:   [system][user1][asst1][tool1][user2]...  │
│ Request N+1: [system][user1][asst1][tool1][user2]...[new msg] │
│              └─────── 缓存命中 ───────┘└─ 新计算 ─┘    │
│                                                       │
│ 客户端不需要做任何标记，DeepSeek 自己比较                │
│ 字节前缀并只对新 token 计费                               │
└───────────────────────────────────────────────────────┘
```

**核心文件**: `internal/agent/cache_shape.go`

设计原则（来自 `REASONIX.md` Line 14-16）:
> Cache-first: the system-prompt prefix (base prompt + tools + memory) must stay byte-stable across turns so DeepSeek's automatic prefix cache stays warm. Never mutate it mid-session — ride the turn tail instead.

#### Hermes: 主动 Anthropic cache_control

```
┌───────────────────────────────────────────────────────┐
│ 客户端在消息上注入 cache_control 断点                    │
│                                                       │
│ Breakpoint 1: [system]          ← 持久缓存             │
│  ...中间消息不被标记...                                 │
│ Breakpoint 2: [倒数第3条非系统]  ┐                      │
│ Breakpoint 3: [倒数第2条非系统]  ├─ 滚动窗口            │
│ Breakpoint 4: [倒数第1条非系统]  ┘                      │
│                                                       │
│ 最多 4 个断点（Anthropic 限制），系统 prompt 占用一个    │
└───────────────────────────────────────────────────────┘
```

**核心文件**: `crates/hermes-agent/src/prompt_caching.rs`

关键实现:
- `build_cache_marker()` — 构建 `{"type":"ephemeral", "ttl":"1h"}` 标记
- `apply_anthropic_cache_control_in_place()` — 原地注入，避免额外分配
- `anthropic_prompt_cache_policy()` — 自动检测 provider 兼容性

**支持的 provider 矩阵**:
| Provider | 模式 | 缓存 | Native Layout |
|----------|------|------|---------------|
| api.anthropic.com | anthropic_messages | ✅ true | ✅ true |
| openrouter.ai (Claude) | chat_completions | ✅ true | ❌ false |
| nousresearch (Claude) | chat_completions | ✅ true | ❌ false |
| nousresearch (Qwen) | chat_completions | ✅ true | ❌ false |
| minimax.io / minimaxi.com | anthropic_messages | ✅ true | ✅ true |
| opencode/alibaba (Qwen) | chat_completions | ✅ true | ❌ false |
| 其他 | any | ❌ false | ❌ false |

---

### 2.2 关键差异: 为什么 Reasonix 能"90%+ 从缓存重放"

Reasonix 的 `cachehit_e2e_test.go` 包含一个 mock DeepSeek 服务端，通过**字节前缀匹配**来精确计算缓存命中率:

```go
// mock DeepSeek 的核心逻辑
func (m *mockDeepSeek) handler(w http.ResponseWriter, r *http.Request) {
    msgs := decodeMessages(body)
    common := commonPrefixMsgs(m.prevMessages, msgs)  // 逐消息字节比较
    hitChars := charsOf(msgs[:common])                  // 前缀命中的字符数
    totalChars := charsOf(msgs)                         // 总字符数
    // ...
}
```

**测试证实**: 在 append-only 模式下（无压缩），14 轮对话后缓存命中率爬升至 90%+。E2E 测试实测:

```
turn  0: prompt=  691 hit=    0 miss=  691 → cache 0%
turn  1: prompt= 1293 hit=  691 miss=  602 → cache 53%
turn  2: prompt= 1895 hit= 1293 miss=  602 → cache 68%
...
turn 13: prompt= 8517 hit= 7915 miss=  602 → cache 93%
```

**Hermes vs Reasonix 服务端缓存的关键差异**:
- Reasonix 利用 DeepSeek 的**自动** prefix caching — 整个前缀只要字节不变就免费
- Hermes 使用 Anthropic 的**显式** cache_control — 只有被标记的 4 个断点能被缓存
- 这意味着在长对话中，Reasonix 的缓存范围更大（整个前缀 vs 仅 4 个标记点）

---

### 2.3 上下文压缩策略对比

#### Reasonix: 三阶段压缩 (compact.go)

```
触发条件：prompt_tokens >= contextWindow × 80%

Stage 1: PruneStaleToolResults（免费）
├─ 将可压缩区域中 >1KB 的旧工具结果替换为占位符
├─ "elided tool result — read_file, N bytes dropped…"
└─ tool_call/result 配对完好，assistant 内容不变

Stage 2: planCompaction（预算规划）
├─ head = system + first_user_turn + prior_digests（固定保留）
├─ tail = 最近 N 条消息（token 预算制，默认 16384 tokens）
└─ region = msgs[head:start]（需要压缩的区域）

Stage 3: partitionFold（选择性折叠）
├─ 小用户回合 → kept verbatim（用户的陈述永不被摘要）
├─ 之前的 digest → kept verbatim（永不重新摘要已有摘要）
├─ 其余 → fold into summary（LLM summary）
└─ 组装: head + kept + <compaction-summary> + tail
```

关键设计:
- **用户陈述永不被摘要**: `partitionFold()` 中所有短用户消息保持逐字
- **已有摘要不重新摘要**: `isCompactionSummary()` 检测 `<compaction-summary>` 标签
- **连续压缩卡住自动停止**: `compactStuck = true` 防止缓存崩溃循环
- **机械回退**: summarizer 失败时用 `mechanicalFoldDigest()` 代替，确保上下文释放

#### Hermes: 双保险压缩

```
┌──────────────────────────┐
│ Gateway Session Hygiene  │ 阈值 85%（安全网）
│ 粗略估算，session 积累    │
└─────────┬────────────────┘
          │
          ▼
┌──────────────────────────┐
│ Agent ContextCompressor  │ 阈值 50%（主力）
│ 精确 API 报告 token 计数  │
└──────────────────────────┘
```

**Rust 实现**: `crates/hermes-intelligence/src/context_engine.rs`
  - `DefaultContextEngine` — 摘要替换，保留尾部 `keep_ratio` 比例
  - `ImportanceBasedEngine` — 按重要性评分 + token 预算过滤
  - LLM 辅助摘要（`HERMES_CONTEXT_SUMMARY_URL`），失败回退到启发式

**压缩策略对比**:
| 方面 | Reasonix | Hermes |
|------|----------|--------|
| 阈值 | 50%/80%/90% 三级 | 50%/85% 两级 |
| 尾部策略 | Token 预算制 (16384) | 比例制 + protect_last_n |
| 用户消息处理 | 分开：短消息逐字保留 | 混合：统一压缩 |
| 摘要增量 | ✅ 多次摘要累积不丢失 | ❌ 重新摘要整个中间区域 |
| 缓存友好 | ✅ 短用户消息逐字→byte-stable | ⚠️ 压缩后缓存整体失效 |
| 压缩卡住保护 | ✅ stuck guard 自动暂停 | ❌ 无 |
| 摘要失败回退 | ✅ mechanicalFoldDigest | ⚠️ 仅 log warning, 丢弃中间轮次 |

---

## 三、缓存诊断体系对比

### 3.1 Reasonix: 精细的 PrefixShape 诊断

```go
type PrefixShape struct {
    SystemHash        string   // SHA-256(系统prompt)
    ToolsHash         string   // SHA-256(归一化工具schema)
    PrefixHash        string   // SHA-256(系统+工具连体)
    LogRewriteVersion int      // compaction 重写计数
    ToolSchemaTokens  int      // 工具 schema 估算 token 数
}

// 使用: 每轮后 CompareShape(prev, cur, usage)
type CacheDiagnostics struct {
    PrefixHash          string
    PrefixChanged       bool
    PrefixChangeReasons []string   // "system" | "tools" | "log_rewrite"
    SystemHash          string
    ToolsHash           string
    LogRewriteVersion   int
    ToolSchemaTokens    int
    CacheMissTokens     int       // 当前轮的 miss
    CacheHitTokens      int       // 当前轮的 hit
}
```

**诊断输出示例** (来自 `cache_diagnostics_test.go`):
- Turn 1: `PrefixChanged=false, CacheHit=0, CacheMiss=100`（首次请求无缓存）
- Turn 2 (添加了工具): `PrefixChanged=true, Reasons=[tools], CacheHit=80, CacheMiss=20`

**工具 schema 归一化** (确保哈希稳定性):
```go
func normalizeToolSchemas(schemas []provider.ToolSchema) []provider.ToolSchema {
    sort.Slice(out, func(i, j int) bool {
        if out[i].Name != out[j].Name { return out[i].Name < out[j].Name }
        if out[i].Description != out[j].Description { return out[i].Description < out[j].Description }
        return string(out[i].Parameters) < string(out[j].Parameters)
    })
}
```

### 3.2 Hermes: Prometheus 遥测

```rust
// hermes-telemetry/src/lib.rs
pub prompt_cache_hits: AtomicU64,
pub prompt_cache_misses: AtomicU64,

pub fn record_prompt_cache_hit() {
    METRICS.prompt_cache_hits.fetch_add(1, Ordering::Relaxed);
}
```

**差距**: Hermes 有计数但**没有逐轮诊断**——不知道缓存为什么 miss（系统 prompt 变了？工具 schema 变了？压缩了？）。

---

## 四、客户端缓存对比

### 4.1 Hermes: 独有的多层客户端缓存

Reasonix 没有客户端缓存层（消息直接序列化发送）。Hermes 有:

#### Level 1: Turn API Messages Cache (`llm_caller.rs`)

```
ApiMessagesCacheKey { message_count, total_chars, prefetch_hash, 
                      model_hash, ephemeral_len, cache_ttl_hash, ... }

→ 同一 turn 内 LLM 重试时复用已组装的 Arc<[Message]>
→ 内容变化时自动失效 (tool_repairs > 0 || seq_repairs > 0)
```

#### Level 2: ProviderSerializeCache (`provider_serialize_cache.rs`)

```
4 个子缓存，独立 hash-keyed:

├─ sanitized_openai_messages    ← OpenAI 格式消息清洗
├─ formatted_openai_tools       ← OpenAI 工具 schema 格式化
├─ converted_anthropic_messages ← Anthropic 协议转换
└─ formatted_anthropic_tools    ← Anthropic 工具格式化

键: MessagesCacheKey { count, content_hash, strict, model_hash, profile_hash }
```

#### Level 3: Cached System Prompt (`agent_state.rs`)

```
cached_system_prompt
→ 整个 session 只构建一次，压缩时失效
```

**价值**: 在工具调用密集的多轮对话中，避免重复序列化/转换大量消息。

---

## 五、缓存友好设计模式对比

### 5.1 时间戳处理

| 项目 | 做法 | 文件 |
|------|------|------|
| **Reasonix** | Session 中无实时时间戳，压缩后归档文件用 `time.Now()` 命名但与 API 请求无关 | `compact.go` (line 626) |
| **Hermes** | 工具 schema 只注入静态时区偏移，不放当前时间；system prompt 只用 date-only | `cronjob.rs` (line 17-19), `time.rs` (line 87-93) |

**Hermes 的明确注释**:
```rust
// Only inject the static timezone offset — NOT the current time.
// Injecting a live timestamp would bust the LLM prompt cache every minute.
```

### 5.2 工具 Schema 稳定性

| 项目 | 做法 |
|------|------|
| **Reasonix** | `normalizeToolSchemas()` — 按 name/description/parameters 排序，确保哈希稳定 |
| **Hermes** | Skill reload 明确告知 "no prompt cache was invalidated"；工具 schema 变化时主动失效客户端缓存 |

### 5.3 Plan Mode

| 项目 | 做法 |
|------|------|
| **Reasonix** | `SetPlanMode()` — 不改 system prompt/工具/session，缓存完美保持 |
| **Hermes** | 无 plan mode 概念，但压缩触发时会自动失效缓存 |

### 5.4 Reasoning/Round-trip 处理

| 项目 | 做法 |
|------|------|
| **Reasonix** | `reasoning_content` **不重新上传**给 API — provider 层面在构建请求时丢弃它，因为重发 reasoning 是付费 prompt 且无缓存收益 |
| **Hermes** | `agent_runtime_helpers` 中有 `needs_thinking_reasoning_pad()` 处理 DeepSeek/Qwen/Kimi 思考模式，但未明确剥离 reasoning |

---

## 六、Token 省流对比

### 6.1 Reasonix: Token Economy Mode

`internal/boot/token_profile.go`

```
TokenModeEconomy: 默认只暴露 15 个核心工具 (bash, read_file, write_file, 
glob, grep, ls, edit_file, ...)。其他来源 (skills, MCP, CodeGraph, LSP, 
web_fetch, install_source, task) 通过 connect_tool_source 工具按需加载。
```

设计动机: 工具 schema 是系统 prompt 前缀的一部分。减少默认工具 = 减少前缀 token = 增加缓存命中部分的占比。

### 6.2 Hermes: 无等效 Token Economy

Hermes 的工具全部在启动时注册，没有动态按需加载的 token 省流机制。

---

## 七、E2E 缓存测试对比

### 7.1 Reasonix: 完整的缓存测试套件

`internal/agent/cachehit_e2e_test.go` (666 行)

| 测试 | 覆盖场景 |
|------|---------|
| `TestCacheHitPrefixStable` | 字节稳定的前缀路径：每请求 re-send 完整历史，验证 hit=前一轮 total |
| `TestCacheHitClimbsWithoutCompaction` | 14 轮无压缩对话，验证峰值>90% |
| `TestCacheHitSurvivesTooSmallWindow` | 窗口太小导致重复压缩→stuck guard 触发→缓存恢复 |
| `TestReasoningRoundTripCost` | reasoning 往返对缓存命中率的拖累量化 |
| `TestSessionAggregateCacheRate` | 会话级聚合缓存率 vs 单轮缓存率 |
| `TestReleaseCacheGuard` | 8 个 scenario 的 release 缓存 guard，确保不低于 90% |

所有测试使用 **mock DeepSeek 服务端**，通过 `commonPrefixMsgs()` 字节比对模拟真实 API 行为。

### 7.2 Hermes: 单元测试覆盖

- `prompt_caching.rs` 中的测试: marker 构建、system_and_3 策略、tool 缓存、policy 矩阵
- `cache_diagnostics_test.go` 对应的 parity 测试: 无
- **无 E2E 缓存命中率测试**

---

## 八、可借鉴的改进建议

基于以上对比，Hermes 可以从 Reasonix 借鉴以下设计:

### 8.1 高优先级

1. **PrefixShape 诊断系统** — 在 Hermes 中添加类似 Reasonix 的 `CacheDiagnostics`，在每个 Usage 事件中报告前缀变化原因（system/tools/rewrite），便于用户理解缓存行为。

2. **压缩卡住保护 (Stuck Guard)** — 当连续两轮压缩后 prompt 仍超阈值时，自动暂停压缩而非每轮反复破坏缓存。

3. **摘要增量策略** — Reasonix 的 `partitionFold` 将小用户回合和已有摘要逐字保留，避免重复摘要丢失信息。Hermes 目前的 `compress()` 重新摘要整个中间区域。

### 8.2 中优先级

4. **Token Economy Mode** — 提供 `token_mode: economy` 配置，默认只暴露核心工具，非核心工具按需加载，减小系统 prompt 的 token 占用。

5. **Reasoning 剥离** — 不在 API 请求中重发 `reasoning_content`（对 DeepSeek 等计费平台），减少不必要的 prompt token。

6. **E2E 缓存命中率测试** — 添加类似 Reasonix 的 mock 服务端测试，量化缓存命中率是否符合预期。

### 8.3 低优先级

7. **服务端缓存 Provider 扩展** — 让 `anthropic_prompt_cache_policy()` 也检测 DeepSeek API，利用其自动 prefix caching（无需显式 cache_control 标记）。

---

## 九、文件索引

### Reasonix 关键文件
| 文件 | 职责 |
|------|------|
| `internal/agent/cache_shape.go` | PrefixShape 定义 + CompareShape 诊断 |
| `internal/agent/cachehit_e2e_test.go` | E2E 缓存命中率测试 (666行) |
| `internal/agent/compact.go` | 三阶段压缩实现 (641行) |
| `internal/agent/prune.go` | 陈旧工具结果剪枝 |
| `internal/agent/agent.go` | Run 主循环, stream, 缓存累积 |
| `internal/event/event.go` | CacheDiagnostics 事件结构 |
| `internal/boot/token_profile.go` | Token Economy Mode |
| `REASONIX.md` | 项目级缓存哲学声明 |
| `benchmarks/context-maintenance-e2e/main.go` | 真实 API E2E 基准 |

### Hermes 关键文件
| 文件 | 职责 |
|------|------|
| `crates/hermes-agent/src/prompt_caching.rs` | Anthropic cache_control 实现 |
| `crates/hermes-agent/src/api_messages.rs` | ApiMessagesCacheKey + 消息组装 |
| `crates/hermes-agent/src/provider_serialize_cache.rs` | 4路客户端序列化缓存 |
| `crates/hermes-agent/src/llm_caller.rs` | turn_api_messages_cache 集成 |
| `crates/hermes-agent/src/agent_runtime_helpers.rs` | anthropic_prompt_cache_policy |
| `crates/hermes-intelligence/src/context_engine.rs` | 双引擎上下文压缩 |
| `crates/hermes-telemetry/src/lib.rs` | prompt_cache_hits/misses 指标 |
| `crates/hermes-core/src/time.rs` | date-only system prompt |
| `crates/hermes-tools/src/tools/cronjob.rs` | 静态时区偏移（避免时间戳破坏缓存） |
| `crates/hermes-gateway/src/agent_cache.rs` | Gateway agent 配置签名缓存 |
| `website/docs/developer-guide/context-compression-and-caching.md` | 缓存文档 |
