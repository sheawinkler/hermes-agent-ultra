# 新客户端 API 对接文档（用户账户 & 激活上报）

> 本文档基于 FlowyClaw 现有 Electron 客户端（`src/flowy/api.ts`、`electron/flowy/device-activation.ts` 等）与 Go 服务端（`code/backend/internal/handlers`、`code/backend/internal/routes/routes.go`）整理，供新客户端按相同契约实现对接。

---

## 1. 通用约定

### 1.1 服务端地址

现有客户端通过 `getCurrentFlowyServerBase()` 构造 API 根路径，规则如下：

| 环境 | 域名（示例） | API 根路径 |
|------|-------------|-----------|
| 国内生产 | `server.flowyaipc.cn` | `https://server.flowyaipc.cn/claw` |
| 国际生产 | `server.flowyaipc.com` | `https://server.flowyaipc.com/claw` |
| 测试（仅 dev + 开关开启） | `test.flowyaipc.cn` | `https://test.flowyaipc.cn/claw` |

**路径映射说明：**

- 客户端实际请求路径形如：`{API根}/user/doLoginByEmail`
- 服务端 Gin 路由注册在 `/api/v1/user/doLoginByEmail`
- 网关/反向代理将 `/claw/*` 映射到 `/api/v1/*`（OAuth2 代码中亦采用此规则：`/claw/` + 去掉 `/api/v1/` 前缀）

下文 **「客户端路径」** 均相对于 `{API根}`（即 `/claw` 前缀）；**「服务端路径」** 为 `/api/v1/...` 形式，二者等价。

### 1.2 请求头

| Header | 必填 | 说明 |
|--------|------|------|
| `Content-Type: application/json` | POST 有 body 时 | JSON 请求体 |
| `Authorization: Bearer <JWT>` | 需登录接口 | 标准 Bearer Token |
| `token: <JWT>` | 可选但建议携带 | 现有客户端与 Bearer **同时发送**，保持兼容 |

参考实现（`src/flowy/api.ts`）：

```typescript
const headers: Record<string, string> = { 'Content-Type': 'application/json' };
if (token) {
  headers['token'] = token;
  headers['Authorization'] = `Bearer ${token}`;
}
```

### 1.3 统一响应格式

```json
{
  "code": 200,
  "msg": "操作成功",
  "data": { }
}
```

| 字段 | 说明 |
|------|------|
| `code` | 业务状态码；**客户端应以 `code === 200` 判断成功**，不要仅依赖 HTTP 200 |
| `msg` | 国际化文案，**不要用于业务分支** |
| `data` | 业务数据；部分成功接口无 data（如设备激活） |

**401 处理（现有客户端行为）：**

- HTTP 状态码为 `401`，或
- HTTP 200 但 body 中 `code === 401`

均应视为登录态失效，清除本地 Token 并引导重新登录。

**错误响应示例：**

```json
{
  "code": 400,
  "msg": "验证码无效或已过期"
}
```

服务端错误键（errorKey）与 HTTP 状态码对应关系见各接口说明；客户端对接时建议记录 `code` 与 `msg` 便于排查。

### 1.4 品牌 Channel

`channel` 用于用户隔离（同邮箱/微信在不同 channel 下为不同用户）。现有客户端取 `BRAND.id`：

| 品牌示例 | `channel` 值 |
|---------|-------------|
| FlowyAIPC | `flowy` |
| GMK | `gmk` |
| 其他 OEM | 见各 `brands/*.ts` 的 `id` 字段 |

未传时服务端默认 `flowy`。

### 1.5 客户端 App 标识（`app`）

登录相关接口可选传 `app`，用于运营统计（写入 `tb_users.app_aipc` / `app_herdsman` / `app_flowymes`）：

| 值 | 说明 |
|----|------|
| `aipc` | AIPC 桌面客户端（现有 FlowyClaw 固定传此值） |
| `herdsman` | Herdsman 客户端 |
| `flowymes` | **FlowyMes 新客户端（对接时请固定传此值）** |
| 未传 / 未知 | 服务端登录时默认按 `aipc` 处理，但不更新未知列 |

大小写不敏感。登录成功后服务端会将对应列从 `0` 幂等更新为 `1`（仅首次生效）。

### 1.6 JWT 说明

- 算法：HS256
- Audience：`c`（C 端用户 Token；含 `sys` audience 的 Token 会被拒绝）
- Claims：`user_id`、`email`（可选）、username（可选）
- 默认有效期：配置项 `jwt.expire_hours`，示例配置为 **24 小时**

登录成功后返回的 `data` 即为 JWT 字符串，后续需登录接口均携带此 Token。

---

## 2. 邮箱登录

### 2.1 整体流程

```
发送验证码 → 用户输入验证码 → 邮箱登录 → 获取用户信息 → （可选）设备激活
     │                              │
     └─ 返回 validCodeReqNo ────────┘
```

现有客户端：`EmailLogin.tsx` → `sendEmailCode` / `loginByEmail` / `loginByToken` → `useLoginHandler` 触发激活。

---

### 2.2 发送邮箱验证码

| 项 | 值 |
|----|-----|
| 客户端路径 | `POST /user/getEmailRegisterValidCode` |
| 服务端路径 | `POST /api/v1/user/getEmailRegisterValidCode` |
| 认证 | 不需要 |

**Request Body：**

```json
{
  "email": "user@example.com",
  "channel": "flowy"
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `email` | string | 是 | 合法邮箱格式 |
| `channel` | string | 否 | 品牌 channel，默认 `flowy` |
| `app` | string | 否 | 客户端 App 标识，影响邮件模板等 |

**Success Response（`code: 200`）：**

```json
{
  "code": 200,
  "msg": "操作成功",
  "data": "c1f8e4d7-c8a0-4b3e-8b1e-5e6f4d8a3b1c"
}
```

`data` 为 **验证码请求号 `validCodeReqNo`**（UUID），登录时必须原样回传。

**Error：**

| HTTP | code | 场景 |
|------|------|------|
| 400 | 400 | 邮箱格式无效（binding 失败） |
| 500 | 500 | 发送或存储验证码失败 |

**客户端实现要点：**

- 发送成功后启动 60 秒倒计时，防止频繁重发（现有 UI 逻辑）
- 保存 `validCodeReqNo` 至登录完成

---

### 2.3 邮箱验证码登录

| 项 | 值 |
|----|-----|
| 客户端路径 | `POST /user/doLoginByEmail` |
| 服务端路径 | `POST /api/v1/user/doLoginByEmail` |
| 认证 | 不需要 |

**Request Body（与现有客户端完全一致）：**

```json
{
  "email": "user@example.com",
  "validCode": "123456",
  "validCodeReqNo": "c1f8e4d7-c8a0-4b3e-8b1e-5e6f4d8a3b1c",
  "inviteCode": "ABCD1234",
  "channel": "flowy",
  "device": "",
  "app": "flowymes"
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `email` | string | 是 | 与发码邮箱一致 |
| `validCode` | string | 是 | 用户收到的 6 位验证码 |
| `validCodeReqNo` | string | 是 | 发码接口返回的 UUID |
| `channel` | string | 否 | 品牌 channel |
| `inviteCode` | string | 否 | 邀请码；填写后登录成功即尝试绑定，**绑定失败会阻断登录**（与微信不同） |
| `device` | string | 否 | 可传空字符串 `""` |
| `app` | string | **建议** | FlowyMes 客户端固定传 `flowymes`（见 §1.5） |

> 现有 FlowyClaw 桌面客户端传 `"app": "aipc"`；新 FlowyMes 客户端请改为 `"app": "flowymes"`。

**Success Response：**

```json
{
  "code": 200,
  "msg": "Login successful",
  "data": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9..."
}
```

`data` 为 JWT Token 字符串。

**Error：**

| HTTP | code | 场景 |
|------|------|------|
| 400 | 400 | 参数无效、验证码错误、邀请码无效/已参与/不能邀请自己 |
| 500 | 500 | 登录失败（含验证码校验失败等） |

**登录后服务端行为：**

- 邮箱不存在则自动注册，并发放免费套餐/新用户积分
- 更新 `last_login_ip`、`last_login_time`
- 标记 `app_aipc` / `app_herdsman` / `app_flowymes`（若 `app` 有效）

---

## 3. 微信扫码登录（网页 OAuth）

> 现有 FlowyClaw 桌面客户端使用的是 **微信开放平台网页扫码登录（WxLogin SDK）**，不是公众号扫码关注（`/auth/wechat-mp/*`）。下文按现有客户端实现描述。

### 3.1 整体流程

```
嵌入 WxLogin 二维码
    → 用户扫码授权
    → 微信重定向到 /auth/third/callback?platform=WECHAT&code=...
    → 客户端拦截回调 URL（Electron 主进程）
    → 追加 channel / inviteCode / app 后 GET 回调 URL 换 Token
    → GET /user/me 拉用户信息
    → 登录成功 → 设备激活
```

### 3.2 WxLogin 初始化参数

参考 `src/flowy/auth-providers/wechat/WeChatLogin.tsx`：

| 参数 | 值 | 说明 |
|------|-----|------|
| `appid` | `BRAND.wechatAppId` | 如 flowy 品牌：`wxc7a38fe55e162569`；gmk：`wx413de9863ef7ea1c` |
| `scope` | `snsapi_login` | 网页应用扫码登录 |
| `redirect_uri` | 见下 | **需 URL 编码** |
| `self_redirect` | `true` | 在 iframe 内跳转 |
| `state` | 随机字符串 | 防 CSRF |

**redirect_uri 构造：**

```
{微信专用API根}/auth/third/callback?platform=WECHAT
```

- 微信登录 API 根：现有客户端使用 `getCurrentWeChatFlowyServerBase()`，**固定走国内域名**（即使国际版也用 `server.flowyaipc.cn/claw` 或测试域名），与邮箱登录 API 根可能不同。
- 完整示例：`https://server.flowyaipc.cn/claw/auth/third/callback?platform=WECHAT`
- 传给 WxLogin 时需 `encodeURIComponent(redirect_uri)`

### 3.3 拦截 OAuth 回调

桌面端在 Electron 主进程监听 iframe 导航（`electron/flowy/auth/wechat.ts`）：

- 当 URL 包含 `/auth/third/callback` 且 **不含** `channel=` 时，视为微信原始回调，取消导航并将完整 URL 发给渲染进程（事件 `wechat:login-callback`）
- 渲染进程自行请求服务端换 Token，避免 iframe 跳转导致二维码消失

**移动/Web 客户端**：可在授权完成后直接读取 redirect URL 中的 `code`，或让后端 302 到自定义 scheme。

### 3.4 用 code 换取 JWT

| 项 | 值 |
|----|-----|
| 客户端路径 | `GET /auth/third/callback` |
| 服务端路径 | `GET /api/v1/auth/third/callback` |
| 认证 | 不需要 |

**Query Parameters：**

| 参数 | 必填 | 说明 |
|------|------|------|
| `platform` | 是 | 固定 `WECHAT` |
| `code` | 是 | 微信回调携带的授权码 |
| `channel` | 建议 | 品牌 channel，默认 `flowy` |
| `inviteCode` | 否 | 邀请码；绑定失败**不阻断**登录 |
| `app` | 建议 | FlowyMes 客户端追加 `app=flowymes`（FlowyClaw 传 `app=aipc`） |

**现有客户端拼接示例：**

原始回调：`https://server.flowyaipc.cn/claw/auth/third/callback?platform=WECHAT&code=XXX&state=YYY`

追加参数后 GET：

```
https://server.flowyaipc.cn/claw/auth/third/callback?platform=WECHAT&code=XXX&state=YYY&channel=flowy&inviteCode=ABCD1234&app=flowymes
```

**Success Response：**

```json
{
  "code": 200,
  "msg": "Login successful",
  "data": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9..."
}
```

**Error：**

| HTTP | code | 场景 |
|------|------|------|
| 400 | 400 | 缺少 `code` 或不支持的 `platform` |
| 500 | 500 | 微信登录失败 |

**说明：**

- 服务端还支持 `platform=GOOGLE`（国际版场景），参数需额外 `redirect_uri`
- 若 query 含 `source=website`，服务端会 302 到官网并带 `token`，桌面客户端不走此分支

### 3.5 微信公众号扫码关注登录（备选方案）

现有桌面客户端**未使用**，但服务端已实现，适用于 H5/无 WxLogin SDK 场景：

| 步骤 | 方法 | 路径 | 说明 |
|------|------|------|------|
| 1. 创建会话 | POST | `/auth/wechat-mp/session` | Body: `{ "channel": "flowy", "inviteCode": "" }` → 返回 `sessionId`、`qrImageUrl`、`expiresIn` |
| 2. 轮询状态 | GET | `/auth/wechat-mp/session/status?sessionId=...` | `pending` / `confirmed`+`token` / `expired` |
| 3. 微信服务器回调 | GET/POST | `/auth/wechat-mp/callback` | 仅微信平台调用，客户端无需对接 |

轮询建议间隔 2–3 秒，会话默认 300 秒过期。

---

## 4. 用户个人中心相关 API

登录成功后，现有客户端会立即调用 `GET /user/me`，并在侧边栏/个人中心周期性同步积分等信息。

### 4.1 获取当前用户信息

| 项 | 值 |
|----|-----|
| 客户端路径 | `GET /user/me` |
| 服务端路径 | `GET /api/v1/user/me` |
| 认证 | **需要** Bearer Token |

**Success Response（`code: 200`）— 结构说明：**

`data` 包含用户表字段 + 扩展会员信息。核心字段如下（完整字段以服务端 `UserMe` / `tb_users` 为准）：

```json
{
  "code": 200,
  "msg": "操作成功",
  "data": {
    "id": 1,
    "open_id": "wx_openid_123",
    "union_id": "wx_unionid_123",
    "nickname": "Test User",
    "avatar": "http://example.com/avatar.jpg",
    "email": "test@example.com",
    "phone": null,
    "country": "CN",
    "city": "Beijing",
    "language": "zh-CN",
    "channel": "flowy",
    "status": 1,
    "last_login_ip": "127.0.0.1",
    "last_login_time": "2023-10-27T10:00:00+08:00",
    "app_aipc": 0,
    "app_herdsman": 0,
    "app_flowymes": 1,
    "time_zone": "Asia/Shanghai",
    "created_at": "2023-10-27T10:00:00+08:00",
    "updated_at": "2023-10-27T10:00:00+08:00",
    "deleted_at": null,
    "currentPlan": {
      "planId": 2,
      "code": "ProMonth",
      "name": "专业版（月）",
      "nameEn": "Pro (Monthly)",
      "planPeriod": "MONTH",
      "currency": "CNY",
      "startAt": "2023-10-27T10:00:00+08:00",
      "endAt": "2023-11-26T10:00:00+08:00",
      "status": "ACTIVE",
      "source": "personal"
    },
    "currentWranglerMembership": null,
    "team": null
  }
}
```

| 字段 | 说明 |
|------|------|
| `currentPlan` | 当前个人订阅套餐；无则为 null |
| `currentWranglerMembership` | 牧马人会员；无则为 null |
| `team` | 团队信息（含 `role`、`plan` 等）；无则为 null |
| `status` | 1=正常 |

**客户端用法：**

- 登录后立即调用，缓存至本地 auth store
- 展示昵称/头像/邮箱；微信用户可能无 email
- 现有 store 会合并本地 `creditsBalance` 等动态字段，避免被 `/user/me` 覆盖

---

### 4.2 绑定邮箱（微信用户补绑邮箱）

微信登录用户若无邮箱，可通过 Web 个人中心或客户端对接以下接口（服务端已实现，桌面端当前主要在 Web `#profile?tab=account` 完成）。

#### 4.2.1 发送绑定邮箱验证码

| 项 | 值 |
|----|-----|
| 客户端路径 | `POST /user/getBindEmailValidCode` |
| 服务端路径 | `POST /api/v1/user/getBindEmailValidCode` |
| 认证 | 需要 |

**Request Body：**

```json
{
  "email": "user@example.com"
}
```

**Success：** `data` 为 `validCodeReqNo`（UUID），与登录验证码**独立存储**。

**Error：**

| code | 场景 |
|------|------|
| 400 | 同 channel 下邮箱已被占用 |
| 401 | 未登录 |

#### 4.2.2 绑定邮箱

| 项 | 值 |
|----|-----|
| 客户端路径 | `POST /user/bindEmail` |
| 服务端路径 | `POST /api/v1/user/bindEmail` |
| 认证 | 需要 |

**Request Body：**

```json
{
  "email": "user@example.com",
  "validCode": "123456",
  "validCodeReqNo": "c1f8e4d7-c8a0-4b3e-8b1e-5e6f4d8a3b1c"
}
```

**Success：** `data` 为**新的 JWT**（邮箱写入 Token claims），客户端应替换本地 Token。

---

### 4.3 积分余额

| 项 | 值 |
|----|-----|
| 客户端路径 | `GET /credits/balance` |
| 服务端路径 | `GET /api/v1/credits/balance` |
| 认证 | 需要 |

**Success：**

```json
{
  "code": 200,
  "msg": "Success",
  "data": {
    "balance": 12345
  }
}
```

现有客户端在侧边栏展示积分，并定期调用 `fetchBalance()`。

---

### 4.4 积分分类用量（个人中心 Tooltip）

| 项 | 值 |
|----|-----|
| 客户端路径 | `GET /credits/usageByType` |
| 服务端路径 | `GET /api/v1/credits/usageByType` |
| 认证 | 需要 |

**Query（可选）：**

| 参数 | 默认 | 说明 |
|------|------|------|
| `includeTeamSeat` | `1` | 传 `0`/`false` 排除团队席位积分 |

**Success — `data` 结构：**

```json
{
  "serverTime": "2026-03-11T08:00:00+08:00",
  "includeTeamSeat": true,
  "list": [
    {
      "type": "DAILY_CHECKIN",
      "title": "每日签到",
      "total": 200,
      "used": 0,
      "remaining": 200,
      "buckets": [
        {
          "expireAt": "2026-03-11T23:59:59+08:00",
          "total": 200,
          "used": 0,
          "remaining": 200
        }
      ]
    },
    {
      "type": "PLAN",
      "title": "套餐积分",
      "total": 10000,
      "used": 3000,
      "remaining": 7000,
      "buckets": []
    }
  ]
}
```

`type` 枚举：`DAILY_CHECKIN` | `PLAN` | `PACK` | `SIGNUP` | `TEAM_SEAT` | `OTHER`

---

### 4.5 每日签到

| 项 | 值 |
|----|-----|
| 客户端路径 | `POST /credits/checkin` |
| 服务端路径 | `POST /api/v1/credits/checkin` |
| 认证 | 需要 |

**Request Body（现有客户端）：**

```json
{
  "timeZone": "Asia/Shanghai"
}
```

也可通过 Query：`?timeZone=Asia/Shanghai`

| 字段 | 说明 |
|------|------|
| `timeZone` | IANA 时区名；建议传 `Intl.DateTimeFormat().resolvedOptions().timeZone` |

**Success：**

```json
{
  "code": 200,
  "msg": "Success",
  "data": {
    "alreadyCheckedIn": false,
    "grantedPoints": 200,
    "balance": 10200,
    "checkInAt": "2026-03-11T08:09:51.123+08:00",
    "dayKey": 20260311
  }
}
```

| 字段 | 说明 |
|------|------|
| `alreadyCheckedIn` | 今日是否已签到 |
| `grantedPoints` | 本次发放积分（重复签到为 0） |
| `balance` | 签到后可用余额 |
| `dayKey` | 日期键，如 `20260311`；客户端用于本地防重复签到 |

---

### 4.6 签到记录查询

| 项 | 值 |
|----|-----|
| 客户端路径 | `GET /credits/checkinRecords?month=2026-03` |
| 服务端路径 | `GET /api/v1/credits/checkinRecords` |
| 认证 | 需要 |

`month` 格式固定 `yyyy-MM`。

---

### 4.7 客户端版本包上报

| 项 | 值 |
|----|-----|
| 客户端路径 | `POST /user/clientPackage` |
| 服务端路径 | `POST /api/v1/user/clientPackage` |
| 认证 | 需要 |

**Request Body（现有客户端）：**

```json
{
  "packageType": "stable",
  "appVersion": "1.2.3",
  "platform": "windows"
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `packageType` | string | 是 | `alpha` / `beta` / `stable`（由版本号后缀推断） |
| `appVersion` | string | 否 | 应用版本号 |
| `platform` | string | 否 | `windows` / `linux` / `mac` |
| `clientId` | string | 否 | 客户端实例 ID |

现有客户端登录后自动上报一次，失败 5 秒后重试。

---

### 4.8 在线心跳

| 项 | 值 |
|----|-----|
| 客户端路径 | `POST /presence/heartbeat` |
| 服务端路径 | `POST /api/v1/presence/heartbeat` |
| 认证 | 需要 |

**Request Body（现有客户端）：**

```json
{
  "platform": "windows",
  "appVersion": "1.2.3"
}
```

| 字段 | 说明 |
|------|------|
| `platform` | `windows` / `linux` / `mac`（由 OS 映射） |
| `appVersion` | 应用版本 |
| `clientId` | 可选 UUID |

**Success：**

```json
{
  "code": 200,
  "msg": "Success",
  "data": {
    "serverTime": "2026-03-12T08:00:00.000000000Z",
    "offlineWindowMs": 90000,
    "userId": 1,
    "clientIdAccepted": false
  }
}
```

现有客户端：登录后立即上报一次，之后每 **60 秒** 上报；允许空 body。

---

### 4.9 Web 个人中心跳转（现有客户端行为）

桌面端不直接调用更多 profile API，而是通过外链打开 Web 个人中心，并附带 Token：

```
https://{官网域名}/?token={JWT}&language=zh#profile?tab=records
```

域名规则（`CreditsDisplay.tsx`）：

| 品牌 | 国内 | 国际 |
|------|------|------|
| flowy | `flowyaipc.cn` | `flowyaipc.com` |
| gmk | `claw.gmktec.cn` | `claw.gmktec.com` |

新客户端若内置个人中心页面，需自行对接订单、发票等 API（见 `code/backend/API.md`）；若沿用 Web 个人中心，按上述 URL 规则跳转即可。

---

## 5. 设备激活上报

### 5.1 整体流程（现有 Electron 客户端）

```
登录成功 (useLoginHandler)
    → 调用 device:activateAfterLogin(token)  [Main 进程]
    → 检查本地是否已上报（同 appVersion）
    → 采集设备信息 + 可选 GeoIP
    → POST /device/activate
    → 成功则本地标记已上报
```

**触发时机：** 每次邮箱/微信登录成功后 **fire-and-forget**（失败不阻断进入主页）。

**去重策略（本地）：**

- 若 `deviceActivationUploaded === true` 且 `deviceActivationUploadedAppVersion === 当前版本`，跳过
- 应用版本升级后会重新上报

---

### 5.2 激活上报接口

| 项 | 值 |
|----|-----|
| 客户端路径 | `POST /device/activate` |
| 服务端路径 | `POST /api/v1/device/activate` |
| 认证 | **需要** Bearer Token |

**Request Body（与 `electron/flowy/device-activation.ts` 一致）：**

```json
{
  "channel": "flowy",
  "mac": "00:1A:2B:3C:4D:5E",
  "sn": "SN202603030001",
  "activateTimestamp": 1741052716000,
  "cpuChipId": "BFEBFBFF000906EA",
  "appVersion": "1.2.3",
  "osVersion": "Windows_NT 10.0.26200",
  "xpuBrand": null,
  "publicIP": "112.124.xx.xx",
  "countryCode": "CN",
  "postal": "100080",
  "latitude": "39.908823",
  "longitude": "116.397470",
  "isp": "China Telecom",
  "timezone": "GMT+8",
  "currency": "CNY"
}
```

**字段说明：**

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `channel` | string | 否 | 品牌 channel（`BRAND.id`） |
| `mac` | string | **是** | 第一块非内部网卡 MAC，大写，如 `AA:BB:CC:DD:EE:FF` |
| `sn` | string | **是** | 设备序列号；读不到则生成并持久化 `CLAWSN{timestamp}{random}` |
| `activateTimestamp` | number | **是** | 激活时间 **毫秒**时间戳（`Date.now()`） |
| `cpuChipId` | string | **是** | Windows：`Win32_Processor.ProcessorId`；其他平台：CPU 型号 SHA256 前 16 位大写，前缀 `CPU` |
| `appVersion` | string | **是** | 客户端版本号 |
| `osVersion` | string | 否 | 如 `{os.type()} {os.release()}` |
| `xpuBrand` | string | 否 | GPU/XPU 品牌 |
| `publicIP` | string | 否 | 公网 IP（现有客户端调 `https://ipapi.co/json/`） |
| `countryCode` | string | 否 | 国家代码 |
| `postal` | string | 否 | 邮编；无则传 `"0"` |
| `latitude` | string/number | 否 | 纬度；无则 `"0"` |
| `longitude` | string/number | 否 | 经度；无则 `"0"` |
| `isp` | string | 否 | 运营商 |
| `timezone` | string | 否 | 如 `GMT+8` |
| `currency` | string | 否 | 货币代码 |

**服务端校验（`ActivateDevice` handler）：**

- `mac`、`sn`、`cpuChipId`、`activateTimestamp > 0`、`appVersion` 均不能为空，否则 400
- `latitude`/`longitude` 支持 JSON 字符串或数字

**Success Response：**

```json
{
  "code": 200,
  "msg": "操作成功"
}
```

无 `data` 字段（`OKNoData`）。

**Error：**

| HTTP | code | 场景 |
|------|------|------|
| 400 | 400 | 必填字段缺失或 body 非法 |
| 401 | 401 | 未登录 |
| 500 | 500 | 入库失败 |

**幂等与去重（服务端）：**

按 `mac` + `cpuChipId` + `channel` + `appVersion` + `activate_state=1` 判断：

- 已存在成功记录 → 直接返回 200，**不重复插入**
- 同一机器不同 `appVersion` 可各保留一条记录

**客户端成功判定（现有逻辑）：**

```typescript
// HTTP ok 且 (无 body 或 body.code === 200)
if (!response.ok) return false;
const body = JSON.parse(text);
if (body && typeof body.code === 'number') return body.code === 200;
return true;
```

---

### 5.3 设备信息采集参考实现

| 信息 | Windows | macOS | Linux | 回退 |
|------|---------|-------|-------|------|
| SN | `Win32_BIOS.SerialNumber` | `system_profiler SPHardwareDataType` | `/sys/class/dmi/id/product_serial` | 生成随机 SN 并持久化 |
| CPU ID | PowerShell `Win32_Processor.ProcessorId` | CPU model hash | CPU model hash | 空则 400 |
| MAC | 第一块非 internal 且非全零 MAC | 同左 | 同左 | 空则 400 |

GeoIP 失败时地理字段传空字符串或 `"0"`，不阻断激活。

---

## 6. 推荐对接顺序（Checklist）

### 6.1 登录模块

- [ ] 配置 API 根路径（`/claw`）与 `channel`
- [ ] 实现邮箱发码 + 登录（保存 `validCodeReqNo`）
- [ ] 实现微信 WxLogin + callback 换 Token（注意微信 API 根可能与邮箱不同）
- [ ] 登录成功后调用 `GET /user/me` 缓存用户信息
- [ ] 持久化 JWT；401 时清除并跳转登录页
- [ ] 登录请求固定传 `"app": "flowymes"`（邮箱登录 body、微信 callback query）
- [ ] （可选）邀请码 `inviteCode` 输入框，最长 16 字符

### 6.2 个人中心

- [ ] `GET /user/me` 展示用户资料与会员状态
- [ ] `GET /credits/balance` + `GET /credits/usageByType` 展示积分
- [ ] `POST /credits/checkin` 每日签到
- [ ] `POST /user/clientPackage` 版本上报
- [ ] `POST /presence/heartbeat` 在线心跳（60s 间隔）
- [ ] 微信用户：`POST /user/getBindEmailValidCode` + `POST /user/bindEmail`

### 6.3 激活上报

- [ ] 登录成功后异步调用 `POST /device/activate`
- [ ] 本地缓存 SN、激活成功标记、已上报版本号
- [ ] 版本升级后重新上报
- [ ] 请求头同时带 `Authorization` 与 `token`

---

## 7. 接口速查表

| 功能 | Method | 客户端路径 | 需登录 |
|------|--------|-----------|--------|
| 发送邮箱验证码 | POST | `/user/getEmailRegisterValidCode` | 否 |
| 邮箱登录 | POST | `/user/doLoginByEmail` | 否 |
| 微信 OAuth 换 Token | GET | `/auth/third/callback?platform=WECHAT&code=...` | 否 |
| 公众号扫码会话 | POST | `/auth/wechat-mp/session` | 否 |
| 公众号扫码轮询 | GET | `/auth/wechat-mp/session/status` | 否 |
| 当前用户信息 | GET | `/user/me` | 是 |
| 发送绑定邮箱验证码 | POST | `/user/getBindEmailValidCode` | 是 |
| 绑定邮箱 | POST | `/user/bindEmail` | 是 |
| 积分余额 | GET | `/credits/balance` | 是 |
| 积分分类用量 | GET | `/credits/usageByType` | 是 |
| 每日签到 | POST | `/credits/checkin` | 是 |
| 签到记录 | GET | `/credits/checkinRecords` | 是 |
| 客户端版本上报 | POST | `/user/clientPackage` | 是 |
| 在线心跳 | POST | `/presence/heartbeat` | 是 |
| **设备激活上报** | POST | `/device/activate` | 是 |

---

*文档生成依据 FlowyClaw 仓库当前代码；若服务端路由或字段有变更，以 `code/backend/API.md` 及 handler 源码为准。*
