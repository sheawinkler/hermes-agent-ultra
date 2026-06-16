# 新客户端 API 对接文档（LLM 对话）

> 本文档基于 FlowyClaw 现有 Electron 客户端（`src/flowy/api.ts`、`electron/utils/openclaw-auth.ts`、`electron/workbench/video/planner/flowy-llm-fallback-adapter.ts` 等）与 Go 服务端（`code/backend/internal/handlers/model.go`、`code/backend/internal/services/model_proxy/chat.go`、`code/backend/internal/routes/routes.go`）整理，供新客户端（如 FlowyMes）对接云端 LLM 对话能力。
>
> **前置依赖**：用户登录与 JWT 获取方式见 [新客户端 API 对接文档（用户账户 & 激活上报）](./new-client-api-user-activation.md)。

---

## 1. 通用约定

### 1.1 两套 Base URL

Flowy 云端 API 分为 **业务 JSON API** 与 **OpenAI 兼容 LLM API** 两套路径，客户端需分别构造 Base URL：

| 用途 | Base URL（示例） | 说明 |
|------|------------------|------|
| 业务 API（模型列表、积分余额等） | `https://server.flowyaipc.cn/claw` | 与账户文档一致 |
| LLM 对话 API | `https://server.flowyaipc.cn/claw/v1` | OpenAI 兼容，**无** `/api/v1` 前缀 |

路径映射（与账户文档相同）：

- 客户端业务请求：`{业务根}/model/availableListClaw` → 服务端 `/api/v1/model/availableListClaw`
- 客户端 LLM 请求：`{LLM根}/chat/completions` → 服务端 `/v1/chat/completions`

现有客户端参考（`shared/flowy-server.ts`）：

```typescript
const businessBase = getCurrentFlowyServerBase('/claw');      // https://{host}/claw
const llmBase = getCurrentFlowyServerBase('/claw/v1');         // https://{host}/claw/v1
```

### 1.2 认证

LLM 相关接口（`/v1/*`、`/anthropic/v1/*`）支持两种凭证，**二选一**：

| 凭证类型 | 格式 | 获取方式 |
|----------|------|----------|
| C 端 JWT | `Authorization: Bearer eyJ...` | 邮箱/微信登录（见账户文档） |
| 用户 API Key | `Authorization: Bearer flowy-...` | 登录后在「用户 API 密钥」管理接口创建（仅 JWT 可管理） |

**建议同时携带**（与现有客户端保持一致）：

```typescript
const headers: Record<string, string> = {
  'Content-Type': 'application/json',
  Authorization: `Bearer ${token}`,
  token, // 与 Bearer 相同
};
```

### 1.3 响应格式差异（重要）

| 接口类型 | 成功响应 | 失败响应 |
|----------|----------|----------|
| 业务 API（如模型列表、session 上报、积分） | `{ "code": 200, "msg": "...", "data": {...} }` | `{ "code": <业务码>, "msg": "..." }` |
| LLM 对话（`/chat/completions` 等） | **上游 OpenAI 兼容 JSON / SSE**，不包装 `code/msg/data` | `{ "code": <业务码>, "msg": "..." }` |

对接 `/v1/chat/completions` 时：

- **HTTP 200 + `Content-Type: application/json`**：直接解析 OpenAI 格式 body
- **HTTP 200 + `Content-Type: text/event-stream`**：按 SSE 解析（见 §5.3）
- **HTTP 4xx/5xx**：解析 `{ code, msg }` 业务错误体

### 1.4 401 处理

与账户文档一致：HTTP `401`，或业务 API 返回 `code === 401`，均应清除本地 Token 并引导重新登录。

---

## 2. 推荐调用流程

```
登录获取 JWT
    ↓
GET /credits/balance（可选，对话前检查积分）
    ↓
GET /model/availableListClaw（获取可用模型及 endpoint）
    ↓
POST /v1/chat/session（上报 sessionId，建议每次新会话调用一次）
    ↓
POST /v1/chat/completions（非流式或 stream: true）
```

**sessionId 说明**：

- 在调用 `/v1/chat/completions`、`/v1/embeddings`、`/v1/rerank` 前建议上报
- 服务端固定 `clientId = "PC"`，客户端无需传递
- 用于将模型调用记录按会话聚合（写入 `tb_user_chat` / `tb_user_chat_detail`）
- 高可用设计：上报失败（`stored: false`）**不阻塞**后续对话

---

## 3. 获取可用模型列表

### 3.1 业务模型列表（推荐）

| 项 | 值 |
|----|-----|
| 客户端路径 | `GET {业务根}/model/availableListClaw` |
| 服务端路径 | `GET /api/v1/model/availableListClaw` |
| 需登录 | 是 |

**Query 参数**

| 参数 | 必填 | 说明 |
|------|------|------|
| `category` | 否 | 模型分类，默认 `1`（对话）。常量见服务端 `ModelCategoryChat = 1` |

**Success Response**

```json
{
  "code": 200,
  "msg": "操作成功",
  "data": {
    "cloud": [
      {
        "id": "AIPC-glm-4.7",
        "name": "GLM-4.7",
        "extra": "{\"input\":[\"text\"],\"reasoning\":true,\"credit_rate\":1.2}",
        "endpoint": "https://server.flowyaipc.cn/claw/v1",
        "anthropic_endpoint": "https://server.flowyaipc.cn/claw/anthropic/v1",
        "icon": "https://...",
        "category": 1,
        "created_at": "2026-01-15T08:00:00Z"
      }
    ]
  }
}
```

**字段说明**

| 字段 | 说明 |
|------|------|
| `id` | **对话时 `model` 字段应使用的标识**，格式为 `AIPC-{tb_model.name}` |
| `name` | 展示名称（`tb_model.display_name`） |
| `extra` | JSON 字符串，常见字段见下表 |
| `endpoint` | LLM Base URL，即 `{host}/claw/v1` |
| `anthropic_endpoint` | Anthropic 兼容 Base URL（若未配置可能为空） |
| `category` | `1` = 对话模型 |

**`extra` 解析示例**（与 `src/flowy/api.ts` 一致）：

```typescript
interface ModelExtra {
  input: string[];      // 如 ["text"]、["text", "image"]
  reasoning: boolean;   // 是否为推理/思考模型
  credit_rate: number;  // 积分倍率（展示用）
}
```

**客户端用法**：

1. 调用 `GET /model/availableListClaw` 获取 `data.cloud`
2. 用户选择模型后，将选中项的 `id`（如 `AIPC-glm-4.7`）作为对话请求的 `model` 字段
3. 请求 URL 使用该项的 `endpoint` + `/chat/completions`（通常各模型 `endpoint` 相同）

### 3.2 OpenAI 兼容模型列表（可选）

| 项 | 值 |
|----|-----|
| 客户端路径 | `GET {LLM根}/models` |
| 服务端路径 | `GET /v1/models` |
| 需登录 | 是（JWT 或 API Key） |

**Success Response**（OpenAI 格式，无 `code/msg` 包装）：

```json
{
  "object": "list",
  "data": [
    {
      "id": "GLM-4.7",
      "object": "model",
      "created": 1736928000,
      "owned_by": "system"
    }
  ]
}
```

> 注意：此处 `data[].id` 为展示名（`display_name`），**与 `availableListClaw` 返回的 `id`（`AIPC-` 前缀）不同**。新客户端对接建议以 §3.1 为准。

---

## 4. 上报会话 ID

| 项 | 值 |
|----|-----|
| 客户端路径 | `POST {LLM根}/chat/session` |
| 服务端路径 | `POST /v1/chat/session` |
| 需登录 | 是 |

**Request Body**

```json
{
  "sessionId": "sess_20260403_001"
}
```

| 字段 | 必填 | 说明 |
|------|------|------|
| `sessionId` | 是 | 去首尾空格；长度 1–128；建议使用客户端生成的 UUID 或业务会话 ID |

**Success Response**

```json
{
  "code": 200,
  "msg": "Success",
  "data": {
    "stored": true
  }
}
```

| `stored` | 含义 |
|----------|------|
| `true` | 已成功写入 Redis，后续对话将归因到该 session |
| `false` | 未写入（参数无效或 Redis 异常），**可继续调用对话接口** |

**Error Response**

| HTTP | code | 说明 |
|------|------|------|
| 401 | 401 | Token 无效 |
| 400 | 400 | 用户 ID 异常 |

---

## 5. 对话补全（核心接口）

### 5.1 基本信息

| 项 | 值 |
|----|-----|
| 客户端路径 | `POST {LLM根}/chat/completions` |
| 服务端路径 | `POST /v1/chat/completions` |
| 需登录 | 是（JWT 或 `flowy-` API Key） |
| Content-Type | `application/json` |

### 5.2 请求 Body

遵循 [OpenAI Chat Completions API](https://platform.openai.com/docs/api-reference/chat/create) 格式。常用字段：

```json
{
  "model": "AIPC-glm-4.7",
  "messages": [
    { "role": "system", "content": "你是一个有帮助的助手。" },
    { "role": "user", "content": "你好" }
  ],
  "stream": false,
  "temperature": 0.7,
  "max_tokens": 4096
}
```

| 字段 | 必填 | 说明 |
|------|------|------|
| `model` | 建议填 | 见 §5.4 模型路由规则；省略时走自动选路 |
| `messages` | 是 | OpenAI 消息数组，支持多轮对话 |
| `stream` | 否 | 默认 `false`；`true` 时返回 SSE |
| `temperature` | 否 | 采样温度 |
| `max_tokens` | 否 | 最大输出 token |
| `response_format` | 否 | 如 `{ "type": "json_object" }` 要求 JSON 输出 |

**多模态（图文）消息**：

```json
{
  "role": "user",
  "content": [
    { "type": "text", "text": "描述这张图片" },
    { "type": "image_url", "image_url": { "url": "https://example.com/a.png" } }
  ]
}
```

服务端会根据 `image_url` 检测视觉需求，仅路由到支持视觉的渠道模型（`extra.input` 含 `image` 的模型）。

### 5.3 非流式响应

**HTTP 200**，`Content-Type: application/json`，body 为 OpenAI 兼容格式：

```json
{
  "id": "chatcmpl-xxx",
  "object": "chat.completion",
  "created": 1710000000,
  "model": "glm-4.7",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "你好！有什么可以帮你的？"
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 12,
    "completion_tokens": 18,
    "total_tokens": 30
  }
}
```

**推理模型**：部分模型在 `message` 或流式 `delta` 中额外返回 `reasoning_content` 字段（思考过程），`content` 为最终回答。客户端可按需分别展示。

**提取助手回复**：

```typescript
const content = response.choices?.[0]?.message?.content;
// content 可能是 string 或 array（多 part），按 OpenAI 规范解析
```

### 5.4 流式响应（SSE）

请求 `"stream": true` 时，**HTTP 200**，`Content-Type: text/event-stream`。

每个事件块格式（OpenAI 兼容）：

```
data: {"id":"chatcmpl-xxx","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"你"},"finish_reason":null}]}

data: {"id":"chatcmpl-xxx","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"好"},"finish_reason":null}]}

data: [DONE]

```

**解析规则**：

1. 按行读取，处理以 `data: ` 开头的行
2. 内容为 `[DONE]` 表示流结束
3. 其余行 JSON 解析后，累加 `choices[0].delta.content`（及可选的 `delta.reasoning_content`）
4. 最后一个含 `usage` 的 chunk 可用于 token 统计（若上游返回）

**可选调试事件**：

默认 **不** 在 SSE 末尾追加内部 `event: debug`。若需要路由、用量、积分等追溯信息，请求头加：

```
X-Flowy-Stream-Debug: 1
```

此时流末尾可能追加：

```
event: debug
data: {"user_id":123,"channel_id":1,"credit_consumed":15,...}

```

> 标准 OpenAI 客户端应 **忽略** 非 `data:` 行；仅调试工具解析 `event: debug`。

**流式解析示例（TypeScript）**：

```typescript
async function* streamChatCompletions(
  llmBase: string,
  token: string,
  body: Record<string, unknown>,
  signal?: AbortSignal,
): AsyncGenerator<string> {
  const res = await fetch(`${llmBase}/chat/completions`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      Authorization: `Bearer ${token}`,
      token,
    },
    body: JSON.stringify({ ...body, stream: true }),
    signal,
  });

  if (!res.ok) {
    const err = await res.json().catch(() => ({}));
    throw new Error(err.msg || `HTTP ${res.status}`);
  }

  const reader = res.body!.getReader();
  const decoder = new TextDecoder();
  let buffer = '';

  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });

    const lines = buffer.split('\n');
    buffer = lines.pop() ?? '';

    for (const line of lines) {
      if (!line.startsWith('data: ')) continue;
      const payload = line.slice(6).trim();
      if (payload === '[DONE]') return;
      try {
        const json = JSON.parse(payload);
        const delta = json.choices?.[0]?.delta?.content;
        if (typeof delta === 'string' && delta.length > 0) {
          yield delta;
        }
      } catch {
        // 跳过无法解析的行
      }
    }
  }
}
```

### 5.5 模型路由规则（`model` 字段）

服务端根据 `model` 字符串决定路由策略（`model_proxy/chat.go`）：

| `model` 格式 | 行为 |
|--------------|------|
| `AIPC-{name}` | 去掉 `AIPC-` 前缀，**精确匹配** `tb_model.name`，路由到对应渠道 |
| `flowy/{name}` | 去掉 `flowy/` 前缀，**精确匹配** `tb_model.name` |
| 其他（如 `Pro/MiniMaxAI/MiniMax-M2.5`）或无 `AIPC-`/`flowy/` 前缀 | **自动选路**：根据 prompt 复杂度、是否含图片等选择 tier，再按权重/价格选渠道 |
| 省略 `model` | 同「自动选路」 |

**新客户端推荐**：使用 `availableListClaw` 返回的 `id`（`AIPC-{name}`），行为确定、与 FlowyClaw 一致。

### 5.6 积分与计费

- 对话成功后按 token 用量扣减积分（配置项 `model.credit_consume_enabled` 为 `true` 时生效）
- 对话前可调用 `GET {业务根}/credits/balance` 查询余额
- 相同请求体可能命中服务端缓存（`CacheEnabled`），命中时不重复扣费

---

## 6. 积分相关（对话前/后）

### 6.1 查询余额

| 项 | 值 |
|----|-----|
| 客户端路径 | `GET {业务根}/credits/balance` |
| 需登录 | 是（仅 JWT） |

```json
{
  "code": 200,
  "msg": "Success",
  "data": {
    "balance": 12345
  }
}
```

`balance` 为整数积分，已排除过期批次。

### 6.2 查询模型调用记录（可选）

| 项 | 值 |
|----|-----|
| 客户端路径 | `GET {业务根}/model/myCalls` |
| Query | `page`（默认 1）、`pageSize`（默认 10，最大 200） |

```json
{
  "code": 200,
  "msg": "Success",
  "data": {
    "list": [
      {
        "chat_id": 1001,
        "model_name": "glm-4.7",
        "channel_model_id": 42,
        "prompt_tokens": 100,
        "completion_tokens": 200,
        "cache_tokens": 0,
        "credit_consumed": 15,
        "created_at": "2026-04-03T10:00:00Z"
      }
    ],
    "total": 1
  }
}
```

---

## 7. 其他 LLM 接口（可选）

以下接口与 `/v1/chat/completions` 共用认证（JWT 或 API Key）与计费逻辑，按需对接。

| 客户端路径 | Method | 说明 |
|-----------|--------|------|
| `{LLM根}/responses` | POST | OpenAI Responses API 兼容 |
| `{LLM根}/embeddings` | POST | 文本嵌入 |
| `{LLM根}/rerank` | POST | 重排序 |
| `{LLM根}/images/generations` | POST | 文生图 |
| `{LLM根}/images/edits` | POST | 图片编辑 |
| `{LLM根}/audio/transcriptions` | POST | 语音转写 |
| `{Anthropic根}/messages` | POST | Anthropic Messages API 兼容 |

**Anthropic 路径**：`{host}/claw/anthropic/v1/messages`（可用模型列表项的 `anthropic_endpoint` + `/messages`）。

Anthropic 请求体会被服务端转换为 OpenAI 格式再路由，响应再转回 Anthropic SSE/JSON。

---

## 8. 错误码

对话接口失败时返回业务 JSON（非 OpenAI `error` 对象）：

```json
{
  "code": 402,
  "msg": "积分不足"
}
```

| HTTP | code | errorKey（参考） | 说明 | 客户端建议 |
|------|------|------------------|------|------------|
| 401 | 401 | `error.auth.*` | 未登录或 Token/API Key 无效 | 清除凭证，跳转登录 |
| 402 | 402 | `error.insufficient_credit` | 积分不足 | 提示充值/签到 |
| 429 | 429 | `error.rate_limited` | 触发限流 | 退避重试 |
| 400 | 400 | `error.invalid_param` | 参数无效（如 model） | 检查 model 与 messages |
| 500 | 500 | `error.all_channel_models_failed` | 全部渠道失败 | 提示稍后重试 |
| 500 | 500 | `error.internal` | 内部错误 | 记录日志，稍后重试 |

上游渠道返回的错误在非流式场景下可能透传为对应 HTTP 状态码 + 上游 body；流式中途失败时连接可能直接中断。

---

## 9. 完整对接示例

以下示例假设已完成登录，`token` 为 JWT，且登录时传 `"app": "flowymes"`（见账户文档）。

```typescript
const BUSINESS_BASE = 'https://server.flowyaipc.cn/claw';
const LLM_BASE = 'https://server.flowyaipc.cn/claw/v1';

function authHeaders(token: string): Record<string, string> {
  return {
    'Content-Type': 'application/json',
    Authorization: `Bearer ${token}`,
    token,
  };
}

// 1. 获取模型列表
async function fetchModels(token: string) {
  const res = await fetch(`${BUSINESS_BASE}/model/availableListClaw`, {
    headers: authHeaders(token),
  });
  const json = await res.json();
  if (json.code !== 200) throw new Error(json.msg);
  return json.data.cloud as Array<{ id: string; name: string; endpoint: string }>;
}

// 2. 上报 session
async function reportSession(token: string, sessionId: string) {
  await fetch(`${LLM_BASE}/chat/session`, {
    method: 'POST',
    headers: authHeaders(token),
    body: JSON.stringify({ sessionId }),
  });
  // stored=false 也可继续
}

// 3. 非流式对话
async function chatOnce(token: string, modelId: string, userMessage: string) {
  const sessionId = crypto.randomUUID();
  await reportSession(token, sessionId);

  const res = await fetch(`${LLM_BASE}/chat/completions`, {
    method: 'POST',
    headers: authHeaders(token),
    body: JSON.stringify({
      model: modelId,
      messages: [{ role: 'user', content: userMessage }],
      stream: false,
    }),
  });

  if (!res.ok) {
    const err = await res.json().catch(() => ({}));
    throw new Error(err.msg || `HTTP ${res.status}`);
  }

  const data = await res.json();
  return data.choices?.[0]?.message?.content ?? '';
}

// 使用
const token = '...'; // 来自登录
const models = await fetchModels(token);
const modelId = models[0]?.id ?? 'AIPC-glm-4.7';
const reply = await chatOnce(token, modelId, '你好');
console.log(reply);
```

---

## 10. 推荐对接顺序（Checklist）

### 10.1 基础能力

- [ ] 配置业务 Base（`/claw`）与 LLM Base（`/claw/v1`）
- [ ] 实现 JWT 持久化与 401 处理（见账户文档）
- [ ] 请求头同时带 `Authorization` 与 `token`

### 10.2 模型与对话

- [ ] `GET /model/availableListClaw` 拉取并展示模型列表
- [ ] 解析 `extra` 展示 `reasoning`、`input`（是否支持图片）、`credit_rate`
- [ ] 每次新会话调用 `POST /v1/chat/session`
- [ ] 实现 `POST /v1/chat/completions` 非流式
- [ ] 实现 SSE 流式解析（`data:` 行 + `[DONE]`）
- [ ] 对话 `model` 使用列表项 `id`（`AIPC-{name}`）

### 10.3 积分与体验

- [ ] 对话前 `GET /credits/balance` 检查余额
- [ ] 处理 402 积分不足、429 限流
- [ ] （可选）`GET /model/myCalls` 展示历史用量
- [ ] （可选）推理模型展示 `reasoning_content`

### 10.4 高级（按需）

- [ ] Anthropic SDK 对接 `{host}/claw/anthropic/v1/messages`
- [ ] 用户 API Key（`flowy-`）用于脚本/服务端调用
- [ ] `X-Flowy-Stream-Debug: 1` 调试路由与扣费

---

## 11. 接口速查表

| 功能 | Method | 客户端路径 | Base | 需登录 |
|------|--------|-----------|------|--------|
| 可用模型列表 | GET | `/model/availableListClaw` | 业务 `/claw` | 是 |
| 积分余额 | GET | `/credits/balance` | 业务 `/claw` | 是 |
| 上报 session | POST | `/chat/session` | LLM `/claw/v1` | 是 |
| **对话补全** | POST | `/chat/completions` | LLM `/claw/v1` | 是 |
| OpenAI 模型列表 | GET | `/models` | LLM `/claw/v1` | 是 |
| 调用记录 | GET | `/model/myCalls` | 业务 `/claw` | 是 |
| Anthropic 对话 | POST | `/messages` | `/claw/anthropic/v1` | 是 |
| Embeddings | POST | `/embeddings` | LLM `/claw/v1` | 是 |

---

## 12. 相关源码索引

| 用途 | 路径 |
|------|------|
| 客户端模型列表 | `src/flowy/api.ts` → `getAvailableModelList` |
| 客户端 LLM 调用示例 | `electron/workbench/video/planner/flowy-llm-fallback-adapter.ts` |
| LLM Base URL | `shared/flowy-server.ts` → `getCurrentFlowyServerBase('/claw/v1')` |
| OpenClaw 云端 Provider 配置 | `electron/utils/openclaw-auth.ts` |
| 服务端路由 | `code/backend/internal/routes/routes.go` |
| 对话代理实现 | `code/backend/internal/services/model_proxy/chat.go` |
| Session 上报 | `code/backend/internal/handlers/model.go` → `ReportChatSession` |
| 服务端 API 总览 | `code/backend/API.md` § AI（OpenAI 兼容） |
| 账户与登录 | [new-client-api-user-activation.md](./new-client-api-user-activation.md) |
