---
title: "Operate the Teams Meeting Pipeline"
description: "Rust CLI runbook for Teams meetings — manual fetch/replay; no automatic webhook product in the release binary"
---

# Operate the Teams Meeting Pipeline

Use this guide with the **Rust** contract in [Teams Meetings](/docs/user-guide/messaging/teams-meetings).

:::warning Release binary scope

Docker/release packages ship **Rust only**. There is no packaged Python gateway that turns Graph webhooks into automatic meeting jobs.

- **Shipped:** `hermes teams-pipeline` CLI (validate, token-health, subscriptions admin, fetch, list/show, **manual** `run`/`replay`)
- **Not shipped in Rust:** `msgraph_webhook` listener, gateway auto-ingest, external sink keys (`notion` / `linear` / `teams_delivery` — **rejected** if present under `meeting_pipeline`)
- **Upstream/source only:** optional Python plugin docs such as [Microsoft Graph Webhook Listener](/docs/user-guide/messaging/msgraph-webhook) — reference if you build that tree yourself; **not** installed by the Rust artifact

:::

This page covers:

- Rust operator CLI flows
- optional Graph subscription maintenance (admin API only — not a notification consumer)
- failure triage under the Rust contract
- go-live checks that do **not** assume automatic webhook delivery

## Core operator commands (Rust)

### Validate the config snapshot

```bash
hermes teams-pipeline validate
```

Reports Graph app-only credential presence (`MSGRAPH_*`), whether `platforms.teams.extra.meeting_pipeline` parses (failing closed if `notion` / `linear` / `teams_delivery` keys are present), `runtime: "manual_local_only"`, an external-sinks unsupported notice, and store path/stats. It does **not** inspect `msgraph_webhook` enablement or Teams platform delivery fields.

### Inspect token health

```bash
hermes teams-pipeline token-health
hermes teams-pipeline token-health --force-refresh
```

### Inspect / maintain Graph subscriptions (admin only)

```bash
hermes teams-pipeline subscriptions
hermes teams-pipeline maintain-subscriptions
hermes teams-pipeline maintain-subscriptions --dry-run
```

These talk to Microsoft Graph’s subscription APIs. They do **not** start an HTTP listener and do **not** process notification POSTs in the Rust gateway.

If you create subscriptions that point at a notification URL, something **outside** the Rust release binary must implement that URL — the Rust package does not.

### Automating subscription renewal (only if you use Graph subscriptions)

**Microsoft Graph subscriptions expire in at most 72 hours.** Renewal matters only if you actually created remote subscriptions for a consumer you operate separately.

If you only use CLI `fetch` / manual jobs and never create Graph subscriptions, you can skip renewal automation.

#### Option 1: Hermes cron (script job)

```bash
mkdir -p ~/.hermes/scripts
cat > ~/.hermes/scripts/maintain-teams-subscriptions.sh <<'EOF'
#!/usr/bin/env bash
exec hermes teams-pipeline maintain-subscriptions
EOF
chmod +x ~/.hermes/scripts/maintain-teams-subscriptions.sh

hermes cron create "0 */12 * * *" \
  --name "teams-pipeline-maintain-subscriptions" \
  --no-agent \
  --script maintain-teams-subscriptions.sh \
  --deliver local
```

#### Option 2: systemd timer

Create a oneshot service that runs `hermes teams-pipeline maintain-subscriptions` every 12 hours (same pattern as any other host timer). Ensure `MSGRAPH_*` credentials are in the service environment.

#### Option 3: crontab

```cron
0 */12 * * * /usr/local/bin/hermes teams-pipeline maintain-subscriptions >> /var/log/hermes/teams-pipeline-maintain.log 2>&1
```

### Inspect jobs / manual replay

```bash
hermes teams-pipeline list
hermes teams-pipeline list --status failed
hermes teams-pipeline show <job-id>
# --organizer-user-id REQUIRED: Microsoft Entra object ID UUID (not UPN/email)
hermes teams-pipeline run <job-id> --organizer-user-id <entra-object-id-uuid>
# alias: hermes teams-pipeline replay <job-id> --organizer-user-id <entra-object-id-uuid>
```

Jobs exist only if something already wrote them to the local store (manual seed, tests, or a **non-Rust** orchestrator). The Rust gateway does not enqueue jobs from live Graph webhooks. `run`/`replay` produce a **local** summary only — external Teams delivery remains unsupported in this Rust release (optional source plugin not packaged/verified).

### Dry-run meeting artifact fetches

```bash
hermes teams-pipeline fetch --organizer-user-id <entra-object-id-uuid> --meeting-id <meeting-id>
hermes teams-pipeline fetch --organizer-user-id <entra-object-id-uuid> --join-web-url "<join-url>"
```

`--organizer-user-id` is **required** and must be a non-blank Microsoft Entra **object ID UUID** (not UPN/email). Fetches transcript/recording metadata via Graph. Manual local path only — no sink delivery; sink keys under `meeting_pipeline` are config-rejected, not soft-ignored.

## Routine runbook (Rust)

### After install

```bash
hermes teams-pipeline validate
hermes teams-pipeline token-health --force-refresh
hermes teams-pipeline fetch --organizer-user-id <entra-object-id-uuid> --meeting-id <known-meeting-id>   # optional smoke
```

Do **not** wait for “a real meeting event” to appear automatically in `list` on the Rust release — that path is not shipped.

### Periodic checks

- `hermes teams-pipeline maintain-subscriptions --dry-run` — only if you manage remote subscriptions
- `hermes teams-pipeline list --status failed` — only if you have jobs in the store
- Re-check Graph credentials after rotation

### Before changing Graph credentials

- update `MSGRAPH_*`
- `hermes teams-pipeline token-health --force-refresh`
- `hermes teams-pipeline validate`

## Failure triage (Rust contract)

### No jobs are being created

**Expected** on Rust release if you only started the gateway. There is no webhook consumer.

To get a job you must seed/run via CLI (or a separate orchestrator you maintain outside the package).

### “I enabled msgraph_webhook / ran hermes gateway run”

Neither starts a Graph meeting notification consumer in Rust. See [msgraph-webhook](/docs/user-guide/messaging/msgraph-webhook) (upstream/source reference only).

### Jobs fail before summarization

Check:

- transcript permissions and availability
- recording permissions and artifact availability
- `ffmpeg` if recording → STT fallback is enabled
- media limits (512 MiB recording, 8 MiB transcript, ~24 MB STT segment) — [Teams Meetings](/docs/user-guide/messaging/teams-meetings#verified-media--graph-limits-rust)
- Graph token health

### Config rejected for Notion / Linear / Teams delivery keys

Rust `TeamsPipelineConfig::from_value` **rejects** the presence of `notion`, `linear`, or `teams_delivery` under `meeting_pipeline` — even when set to `enabled: false`. Remove those keys. This is fail-closed unsupported-config, not a silent skip.

The separate Teams **messaging** adapter (chat bot) is unrelated outbound chat — not pipeline sink wiring. Any automatic sinks live only in optional **out-of-tree Python/source** plugins, not the Rust release package.

### Duplicate or unexpected replays

Usually a manual `hermes teams-pipeline run <job-id> --organizer-user-id <entra-object-id-uuid>`. There is no automatic webhook replay loop in Rust.

## Go-live checklist (Rust)

- [ ] Graph app-only credentials present (`MSGRAPH_TENANT_ID` / `CLIENT_ID` / `CLIENT_SECRET`)
- [ ] `hermes teams-pipeline validate` understood as Graph env + meeting_pipeline parse + store only (does **not** read webhook enablement or Teams delivery mode)
- [ ] `hermes teams-pipeline token-health --force-refresh` succeeds
- [ ] `ffmpeg` installed if you will use recording STT fallback
- [ ] You accept **manual** `fetch` / `run|replay` with required `--organizer-user-id` (Entra object ID UUID, not UPN/email) as the shipped path
- [ ] You do **not** expect automatic Graph → summary from `hermes gateway run`
- [ ] Rust `meeting_pipeline` has **no** `notion` / `linear` / `teams_delivery` keys (presence fails closed)
- [ ] If you create Graph subscriptions at all: renewal is scheduled **and** a non-Rust notification URL consumer exists
- [ ] Optional: document any out-of-tree orchestrator you run yourself (not part of the Rust package)

## Delivery-mode / sink notes

| Topic | Rust reality |
|-------|----------------|
| Teams `delivery_mode` / channel fields on `platforms.teams` | **Not read** by `build_pipeline_runtime_config` or `validate` |
| `notion` / `linear` / `teams_delivery` under `meeting_pipeline` | **Rejected** by `TeamsPipelineConfig::from_value` (fail closed, including `enabled: false`) |
| Optional Python/source plugin sinks | Separate surface if you build that tree yourself — **not** the Rust release binary; do not put those keys in the Rust config object |

## Operator worksheet

| Item | Value |
|------|-------|
| Graph tenant ID | |
| Graph client ID | |
| Store path override, if any | |
| Manual test meeting ID / join URL | |
| Out-of-tree orchestrator (if any) | none / describe |
| Owner for credential rotation | |

## Related docs

- [Teams Meetings (Rust contract)](/docs/user-guide/messaging/teams-meetings)
- [Microsoft Graph Webhook Listener](/docs/user-guide/messaging/msgraph-webhook) — upstream/source reference only
- [Microsoft Teams bot setup](/docs/user-guide/messaging/teams)
