# 客户端微信扫码登录：二维码获取方式分析

> 基于 FlowyClaw 代码库梳理，结论供新客户端（如 FlowyMes）对接参考。  
> 相关账户登录文档：[new-client-api-user-activation.md](./new-client-api-user-activation.md) §3。

---

## 1. 核心结论

**FlowyClaw 桌面客户端的用户登录页（`/login/wechat`）不会向 Flowy 服务端请求二维码。**

二维码由 **微信开放平台官方 WxLogin JS SDK** 在渲染进程内嵌 iframe 时，直接向 **微信服务器** 拉取并展示。客户端只负责：

1. 在 `index.html` 加载微信 SDK 脚本；
2. 用品牌 `wechatAppId`、`redirect_uri` 等参数初始化 `new WxLogin(...)`；
3. 用户扫码后，由 Electron 主进程拦截 OAuth 回调 URL，渲染进程再请求 Flowy 服务端 **用 `code` 换 JWT**。

```
┌─────────────┐     加载 SDK      ┌──────────────────┐
│ index.html  │ ────────────────► │ res.wx.qq.com    │
│ wxLogin.js  │                   │ (微信 CDN)       │
└─────────────┘                   └────────┬─────────┘
                                           │ iframe 嵌入
                                           ▼
                                  ┌──────────────────┐
                                  │ open.weixin.qq.com│
                                  │ 生成并展示二维码   │  ◄── 二维码来源（非 Flowy API）
                                  └────────┬─────────┘
                                           │ 用户扫码授权
                                           ▼
                                  redirect → /auth/third/callback?code=...
                                           │
                                           ▼
                                  Flowy 服务端换 Token（此时才访问 Flowy）
```

---

## 2. 仓库内存在三种「微信扫码」场景（勿混淆）

| 场景 | 用途 | 二维码来源 | 当前 FlowyClaw 是否用于用户登录 |
|------|------|------------|--------------------------------|
| **A. 开放平台网页 OAuth（WxLogin）** | 用户账户登录 | 微信 CDN / 开放平台 iframe | **是**（国内版默认登录页） |
| **B. 公众号扫码关注（wechat-mp）** | 用户账户登录（备选方案） | Flowy 服务端调微信 CGI 生成 | **否**（服务端已实现，客户端 `src/` 无引用） |
| **C. iLink 机器人绑定** | OpenClaw 微信**渠道**接入 | 腾讯 `ilinkai.weixin.qq.com` | **否**（仅 Channels 设置页，与用户登录无关） |

下文 §3 详述 **场景 A**（用户登录）；§4、§5 简述 B、C 以免对接时选错方案。

---

## 3. 场景 A：用户登录二维码（WxLogin SDK）

### 3.1 入口与开关

| 项 | 说明 |
|----|------|
| 路由 | `/login/wechat` → `WeChatLogin` 组件 |
| 源码 | `src/flowy/auth-providers/wechat/WeChatLogin.tsx` |
| 启用条件 | 仅 **国内版**（`edition === 'domestic'`），见 `shared/edition.ts` → `isWeChatLoginEnabledForEdition` |
| 默认登录页 | 国内版默认 `/login/wechat`，国际版默认 `/login/email` |

### 3.2 SDK 加载

在 `index.html` 全局引入微信官方脚本（**非 npm 依赖**）：

```html
<script src="https://res.wx.qq.com/connect/zh_CN/htmledition/js/wxLogin.js"></script>
```

页面加载后，`window.WxLogin` 为构造函数，由 React 组件在 `useEffect` 中调用。

### 3.3 二维码如何「出现」

组件挂载 `#wx_login_container` 容器后执行：

```typescript
const redirectUri = `${WECHAT_CONFIG.serverBase}/auth/third/callback?platform=WECHAT`;

new window.WxLogin({
  self_redirect: true,
  id: 'wx_login_container',
  appid: WECHAT_CONFIG.appid,           // BRAND.wechatAppId
  scope: 'snsapi_login',                // 网页应用扫码登录
  redirect_uri: encodeURIComponent(redirectUri),
  state: Math.random().toString(36).substr(2),
  style: '',
  href: realTheme === 'dark' ? WECHAT_STYLE_DARK : WECHAT_STYLE_LIGHT, // 自定义 iframe 样式
});
```

**WxLogin 内部行为（微信官方实现，本仓库无源码）：**

- 在 `id` 对应 DOM 内插入 **iframe**；
- iframe 指向微信开放平台「网站应用微信登录」页面（`open.weixin.qq.com/connect/qrconnect` 一类 URL）；
- **二维码图片由微信服务器生成**，随 iframe 内容一起展示；
- 客户端 **没有** 单独的「获取二维码 URL / 图片」HTTP 接口调用。

### 3.4 关键配置项

| 配置 | 来源 | 示例 |
|------|------|------|
| `appid` | 品牌配置 `BRAND.wechatAppId` | flowy/thunderobot: `wxc7a38fe55e162569`；gmk: `wx413de9863ef7ea1c` |
| `serverBase` | `resolveWeChatFlowyServerBase()` | **固定国内域名**：`https://server.flowyaipc.cn/claw`（或测试 `test.flowyaipc.cn/claw`） |
| `redirect_uri` | `{serverBase}/auth/third/callback?platform=WECHAT` | 须在**微信开放平台**该 AppID 下配置为合法回调域 |
| `scope` | 固定 `snsapi_login` | 网页扫码登录 |
| `self_redirect` | `true` | OAuth 回调在 iframe 内跳转（便于 Electron 拦截） |

品牌 `wechatAppId` 定义见 `brands/*.ts`（如 `brands/flowy.ts`）。

### 3.5 扫码后的完整链路（二维码之后）

二维码展示 **不涉及** Flowy API；用户扫码授权后才会访问 Flowy：

```
用户扫码确认
    ↓
iframe 导航至 redirect_uri（带 code、state）
    ↓
Electron 主进程 webRequest 拦截（electron/flowy/auth/wechat.ts）
    - URL 含 /auth/third/callback 且不含 channel= → 取消请求，IPC 发送 wechat:login-callback
    ↓
渲染进程 WeChatLogin 收到回调 URL
    ↓
追加 query：channel={BRAND.id}、inviteCode（可选）
    ↓
GET 完整 callback URL 换 JWT（src/flowy/api.ts → loginByWeChatCallback）
    - 额外 set app=aipc（FlowyClaw）；新客户端应改为 app=flowymes
    ↓
GET /user/me 拉用户信息
    ↓
登录成功（useLoginHandler）
```

**换 Token 请求：**

```typescript
// loginByWeChatCallback：useBase2=false，直接使用完整 callback URL
const url = new URL(callbackUrl);
url.searchParams.set('app', 'aipc');
await flowyHttpClient().get(url.toString(), null, false);
```

等价服务端：`GET /api/v1/auth/third/callback?platform=WECHAT&code=...&channel=...&app=...`  
处理器：`code/backend/internal/handlers/auth.go` → `ThirdPartyCallback` → `services.WechatLogin`。

### 3.6 Electron 为何要拦截回调

`self_redirect: true` 时，授权完成后 iframe 会尝试加载 `redirect_uri`。若直接放行：

- iframe 会跳转到 Flowy 服务端 JSON 响应页，**二维码区域被替换**，体验差；
- 因此主进程对「微信原始回调」（无 `channel=`）**cancel 请求**，把 URL 交给渲染进程自行 `fetch` 换 Token。

拦截逻辑见 `electron/flowy/auth/wechat.ts` 中 `onBeforeSendHeaders`：

- 含 `/auth/third/callback` 且 **不含** `channel=` → `cancel: true` + `wechat:login-callback`
- 含 `channel=` → 放行（渲染进程主动换 Token 的请求）

另：主进程会 cancel 对 `localhost.weixin.qq.com` 的请求（PC 微信未安装时的 SDK 探测噪音）。

### 3.7 新客户端对接建议（若复用 WxLogin 方案）

1. **不要**实现 `GET/POST` 向 Flowy 拉「登录二维码」——Flowy 用户登录无此接口。
2. 在微信开放平台注册网站应用，配置与 `redirect_uri` 一致的授权回调域。
3. 嵌入 WxLogin（或等价的开放平台 OAuth 扫码页），`appid` 使用对应品牌 ID。
4. `redirect_uri` 建议仍用国内 API 根 + `/auth/third/callback?platform=WECHAT`。
5. 授权完成后用 `code` 调 Flowy `GET /auth/third/callback`，并传 `channel`、`app=flowymes`。
6. 非 Electron 环境：可直接 302 到自定义 URL scheme，或在 WebView 里监听 `redirect_uri` 前缀取 `code`。

---

## 4. 场景 B：公众号扫码关注登录（服务端有、客户端未用）

Flowy 服务端实现了 **公众号带参二维码** 登录，与 WxLogin **不是同一条路**：

| 步骤 | 接口 | 二维码 |
|------|------|--------|
| 创建会话 | `POST /api/v1/auth/wechat-mp/session` | 响应 `data.qrImageUrl` |
| 轮询状态 | `GET /api/v1/auth/wechat-mp/session/status?sessionId=...` | — |
| 用户动作 | 微信扫二维码并**关注服务号** | — |

**二维码生成位置（服务端）**：`code/backend/internal/services/wechat_mp_login.go`

1. 生成 `sessionId`，写入 Redis；
2. 用服务号 `access_token` 调微信 CGI：`POST https://api.weixin.qq.com/cgi-bin/qrcode/create`（`QR_STR_SCENE`，scene 为 sessionId）；
3. 返回展示 URL：`https://mp.weixin.qq.com/cgi-bin/showqrcode?ticket={ticket}`。

**检索结论**：`src/` 下 **无任何** `wechat-mp` / `WechatMP` 引用，FlowyClaw 桌面客户端 **未使用** 此方案做用户登录。

新客户端若无法使用开放平台 WxLogin（例如只有服务号、无网站应用），可改用此 API；需服务端配置 `mp_server_token` 等（见 `code/backend/API.md` §3.1）。

---

## 5. 场景 C：微信渠道机器人 QR（非用户登录）

Channels 页「连接微信」使用的是 **OpenClaw 微信插件 + 腾讯 iLink**，与用户账户登录无关。

| 项 | 值 |
|----|-----|
| 触发 | `POST /api/channels/wechat/start`（Electron 本地 HTTP，`electron/api/routes/channels.ts`） |
| QR 获取 | `GET https://ilinkai.weixin.qq.com/ilink/bot/get_bot_qrcode?bot_type=3` |
| 实现 | `electron/utils/wechat-login.ts` → `fetchWeChatQrCode` |
| 展示 | `qrcode_img_content` 为 URL 或内容时，可能转为 `data:image/png;base64,...`（`renderQrPngDataUrl`） |
| 轮询 | `GET .../ilink/bot/get_qrcode_status?qrcode=...` |

这是 **IM 机器人绑定** 流程，扫码后得到 `bot_token`，不是用户 JWT。

---

## 6. 对比速查

| 问题 | 答案 |
|------|------|
| 登录页二维码谁生成的？ | **微信开放平台**（WxLogin iframe） |
| Flowy 服务端是否返回登录 QR 图片？ | **否**（OAuth 方案）；**是**（仅 wechat-mp 方案，客户端未用） |
| 客户端登录前要不要调 Flowy API？ | **不要**；扫码授权后才调 `/auth/third/callback` |
| `wechatAppId` 从哪来？ | 构建时品牌 `brands/{brand}.ts` |
| 国际版有没有微信登录页？ | **没有**（`isWeChatLoginEnabledForEdition` 仅 domestic） |
| Channels 里微信 QR 是登录吗？ | **不是**，是 iLink 机器人绑定 |

---

## 7. 相关源码索引

| 用途 | 路径 |
|------|------|
| 微信 SDK 脚本引入 | `index.html` |
| 登录页 + WxLogin 初始化 | `src/flowy/auth-providers/wechat/WeChatLogin.tsx` |
| 国内版/微信登录开关 | `shared/edition.ts`、`src/flowy/auth/edition.ts` |
| 微信 API 根（国内固定） | `shared/flowy-server.ts` → `resolveWeChatFlowyServerBase` |
| 品牌 AppID | `brands/flowy.ts`、`brands/gmk.ts` 等 |
| OAuth 回调拦截 | `electron/flowy/auth/wechat.ts` |
| 主进程注册拦截 | `electron/main/index.ts` → `setupWeChatCallback` |
| code 换 JWT | `src/flowy/api.ts` → `loginByWeChatCallback` |
| 服务端 OAuth 回调 | `code/backend/internal/handlers/auth.go` → `ThirdPartyCallback` |
| 公众号 QR 登录（未接客户端） | `code/backend/internal/services/wechat_mp_login.go` |
| 渠道机器人 QR | `electron/utils/wechat-login.ts`、`electron/api/routes/channels.ts` |
