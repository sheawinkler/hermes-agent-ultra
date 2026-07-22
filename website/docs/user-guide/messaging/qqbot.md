# QQ Bot

Outbound delivery adapter for the **Official QQ Bot API (v2)**. The Rust `QqBotAdapter` can send text (plain or markdown) and images to a known user/group chat id when something in Hermes invokes delivery. It is **not** a conversational QQ gateway consumer.

## Shipped contract

| Capability | Status in this runtime |
|------------|------------------------|
| Outbound text / markdown | ✅ `send_message` → QQ REST (`msg_type` 0 or 2) |
| Outbound image | ✅ `send_image_url` (remote `http(s)` or local `file://`); falls back to text on failure |
| Non-image file send | Text placeholder only (`[Attachment: name]`), not a real QQ file upload |
| Message edit | No-op (not supported) |
| Inbound WebSocket / gateway recv | **Unsupported** — `start()` only marks the adapter running; no WS loop, no event consumer |
| Inbound DM / group / guild chat | **Unsupported** — configuring QQ does not yield a chat bot that receives QQ messages |
| Voice / audio intake | **Unsupported** |
| STT / `asr_refer_text` / Tencent ASR | **Unsupported** — not present in `QqBotAdapter` |
| Python deps (`aiohttp`, `httpx`) | **Not required** — adapter is Rust/`reqwest` |

Use QQ when another Hermes surface already has a target chat id and needs to **push** a reply or notification (for example cron/webhook `deliver: qqbot` with an explicit chat id). Do **not** expect setup alone to open a two-way QQ conversation.

## Prerequisites

1. A QQ Bot application at [q.qq.com](https://q.qq.com) with **App ID** and **App Secret**.
2. A target **user or group open id** you will pass as `chat_id` on send (group ids are detected when they look like `group_*` / `grp_*` / `qqgroup_*`).
3. Build/runtime with the `gateway-qqbot` feature enabled (adapter is feature-gated).

No extra Python packages are needed for this adapter.

## Configuration

Credentials and options live on the platform config the gateway loads — **not** an inbound bot policy surface.

```yaml
platforms:
  qqbot:                    # alias key `qq` is also accepted at registration
    enabled: true
    extra:
      app_id: "your-app-id"           # required
      client_secret: "your-secret"    # required
      markdown_support: true          # default true — msg_type 2 markdown vs msg_type 0 plain text
```

`QqBotConfig` also carries a `proxy` field (`AdapterProxyConfig`). The current gateway registration path constructs the adapter with the default proxy config unless you wire proxy another way — do not document unshipped proxy env vars.

### Credential helpers (setup / onboarding)

Interactive setup and portal/QR onboarding may prompt for or read:

| Variable | Role |
|----------|------|
| `QQ_APP_ID` | Seeds `platforms.qqbot.extra.app_id` during setup |
| `QQ_CLIENT_SECRET` | Seeds `platforms.qqbot.extra.client_secret` during setup |
| `QQ_PORTAL_HOST` | Onboarding portal host override only (not read by `QqBotAdapter` send path) |

At runtime the adapter uses **`platforms.qqbot.extra.app_id` / `client_secret`** (and `markdown_support`). If those are missing while the platform is enabled, registration skips the adapter.

### Not implemented (do not configure for this adapter)

These names appear in older docs or allowlist tables; **`QqBotAdapter` does not implement inbound allowlists, home-channel recv, or STT**:

- `QQ_STT_API_KEY`, `QQ_STT_BASE_URL`, `QQ_STT_MODEL`
- `platforms.qqbot.extra.stt.*`
- Inbound-only meanings of `QQ_ALLOWED_USERS` / `QQ_GROUP_ALLOWED_USERS` / `QQ_ALLOW_ALL_USERS` as “who can message the bot” (there is no inbound consumer to enforce)
- WebSocket gateway intents, sandbox recv channels, `aiohttp` / `httpx`

## What send does

1. Obtains an app access token from `https://bots.qq.com/app/getAppAccessToken`.
2. Posts to `https://api.sgroup.qq.com`:
   - Text: `/v2/users/{chat_id}/messages` or `/v2/groups/{chat_id}/messages`
   - Images: upload via `/v2/.../files`, then media message (`msg_type` 7); on failure, sends caption+URL as text

There is no receive path, attachment download, or speech pipeline in this file.

## Troubleshooting

### Adapter skipped at gateway start

- Platform not `enabled`, or `app_id` / `client_secret` empty in `platforms.qqbot.extra`
- Binary built without `gateway-qqbot`

### Send fails / auth errors

- Invalid App ID or secret at q.qq.com
- Network path to `bots.qq.com` / `api.sgroup.qq.com`
- Wrong `chat_id` shape (user vs group endpoint selection)

### “Bot never answers in QQ”

Expected for this runtime: **inbound chat is unsupported**. Enabling QQ only registers an outbound sender. Pair delivery with a system that already knows the target open id (cron, webhook deliver, etc.); do not wait for a WebSocket inbox.

### Voice / STT / attachments from QQ users

Not shipped on `QqBotAdapter`. Use other platforms plus the callable `transcription` tool if you need speech-to-text elsewhere — see [Voice & TTS](../features/tts.md#transcription-tool-stt).
