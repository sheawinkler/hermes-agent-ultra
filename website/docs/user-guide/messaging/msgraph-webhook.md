---
sidebar_position: 23
title: "Microsoft Graph Webhook Listener"
description: "Upstream/source-plugin reference for msgraph_webhook — not a shipped Rust gateway consumer"
---

# Microsoft Graph Webhook Listener

:::warning Not in the Rust binary release

Docker and release packages for this project ship the **Rust** runtime only. There is **no** packaged Python gateway manifest/CI that installs an `msgraph_webhook` listener into the Rust artifact.

This page documents an **upstream / optional source-tree plugin** shape for operators who intentionally run a separate Python gateway from source. It is **reference material**, not the primary product path.

The verified **Rust** Teams meeting contract is manual CLI only — see [Teams Meetings](/docs/user-guide/messaging/teams-meetings). In Rust:

- `hermes-gateway` does **not** host an `msgraph_webhook` adapter that accepts Graph POSTs
- nothing invokes `TeamsMeetingPipeline::run_notification` from a webhook
- `hermes gateway run` does **not** start a Graph notification consumer for meetings

:::

## What this page is (and is not)

| Statement | Status |
|-----------|--------|
| Rust release automatically receives Graph change notifications | **False** |
| `hermes gateway run` is the primary shipped consumer of Graph webhooks | **False** (Rust) |
| Optional Python/source `msgraph_webhook` platform (if you build that tree yourself) | **Upstream/reference** — not installed by the Rust package |
| Rust `hermes teams-pipeline` CLI (fetch / run / replay / subscription admin) | **Shipped** — see [Teams Meetings](/docs/user-guide/messaging/teams-meetings) |

Do **not** treat the sections below as “enable this and the Rust gateway becomes an automatic meeting bot.”

## Upstream / optional source plugin (reference)

If you are working from a full monorepo source tree that still contains a Python gateway plugin named `msgraph_webhook`, the **historical** design was:

- bind an HTTP listener for Graph validation + change notifications
- authenticate notifications with a shared `clientState`
- hand accepted events to a pipeline consumer (when that consumer is wired)

That design is **not** the Rust release packaging. Shared docs may still list these config keys and env names for the optional source plugin; they are **not** inspected by `hermes teams-pipeline validate` and enabling them does **not** create a Rust listener.

### Historical config shape (source plugin only)

```yaml
# Only meaningful if you separately run the upstream Python gateway plugin.
# Not consumed as a live adapter by the Rust hermes-gateway binary.
platforms:
  msgraph_webhook:
    enabled: true
    extra:
      port: 8646
      client_state: "replace-with-a-strong-secret"
      accepted_resources:
        - "communications/onlineMeetings"
```

```bash
# Same caveat: env names apply only to the optional Python/source plugin.
# They are ignored by Rust and are not read by hermes teams-pipeline validate.
MSGRAPH_WEBHOOK_ENABLED=true
MSGRAPH_WEBHOOK_PORT=8646
MSGRAPH_WEBHOOK_CLIENT_STATE=<generate-with-openssl-rand-hex-32>
MSGRAPH_WEBHOOK_ACCEPTED_RESOURCES=communications/onlineMeetings
```

Historical HTTP surface of that plugin (not Rust):

- `POST /msgraph/webhook` — change notifications
- `GET /msgraph/webhook?validationToken=...` — subscription validation
- `GET /health` — readiness

### Historical settings table (source plugin only)

| Setting | Default | Description |
|---------|---------|-------------|
| `host` | `0.0.0.0` | Bind address for the HTTP listener (plugin). |
| `port` | `8646` | Bind port (plugin). |
| `webhook_path` | `/msgraph/webhook` | Path Graph would POST to (plugin). |
| `health_path` | `/health` | Readiness path (plugin). |
| `client_state` | — | Shared secret Graph echoes; treat as a strong secret if you run the plugin. |
| `accepted_resources` | `[]` (accept all) | Resource allowlist patterns (plugin). |
| `max_seen_receipts` | `5000` | Dedupe cache size (plugin). |
| `allowed_source_cidrs` | `[]` (allow all) | Optional source-IP allowlist (plugin). |

Equivalent env vars historically used the `MSGRAPH_WEBHOOK_*` prefix — see [environment variables](/docs/reference/environment-variables#microsoft-graph-teams-meetings) for name inventory. Presence of those names is not proof the Rust binary implements the listener.

## What to use instead (Rust)

For the shipped Rust binary:

1. [Teams Meetings](/docs/user-guide/messaging/teams-meetings) — CLI fetch, manual `run`/`replay`, Graph subscription **admin**, store inspect, media limits
2. [Operate the Teams Meeting Pipeline](/docs/guides/operate-teams-meeting-pipeline) — operator commands under the same Rust contract
3. Optional STT via `TranscriptionHandler` when the CLI pipeline falls back to recording audio

Graph app credentials (`MSGRAPH_TENANT_ID` / `MSGRAPH_CLIENT_ID` / `MSGRAPH_CLIENT_SECRET`) remain relevant for **CLI Graph client** calls (fetch, subscriptions, token-health). They do not imply a webhook server.

## Troubleshooting (Rust-first)

| Problem | Reality check |
|---------|----------------|
| Enabled `msgraph_webhook` but no POSTs arrive / nothing listens | Expected on Rust release — no gateway adapter |
| `hermes gateway run` and expected meeting summaries | Gateway does not consume Graph meeting webhooks in Rust |
| Expecting `hermes teams-pipeline validate` to report webhook enablement | It does not — validate checks Graph env credentials, `meeting_pipeline` parse, and store stats only |
| Need automatic Graph → summary | Outside verified Rust packaging; requires a separate non-Rust orchestrator you supply from source — not the release artifact |

## Related Docs

- [Teams Meetings (Rust contract)](/docs/user-guide/messaging/teams-meetings)
- [Operate the Teams Meeting Pipeline](/docs/guides/operate-teams-meeting-pipeline)
- [Register a Microsoft Graph Application](/docs/guides/microsoft-graph-app-registration)
- [Microsoft Teams bot setup](/docs/user-guide/messaging/teams) — chat bot adapter (separate from Graph webhooks)
