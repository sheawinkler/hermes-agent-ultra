---
title: "Teams Meeting Pipeline"
sidebar_label: "Teams Meeting Pipeline"
description: "Operate the Rust Teams meeting pipeline via Hermes CLI — manual fetch/run/replay, Graph subscription admin, local job store"
---

{/* This page is auto-generated from the skill's SKILL.md by website/scripts/generate-skill-docs.py. Edit the source SKILL.md, not this page. */}

# Teams Meeting Pipeline

Operate the Rust Teams meeting pipeline via Hermes CLI — manual fetch/run/replay, Graph subscription admin, local job store. No webhook consumer or sink writers in the Rust release.

## Skill metadata

| | |
|---|---|
| Source | Bundled (installed by default) |
| Path | `skills/productivity/teams-meeting-pipeline` |
| Version | `1.2.0` |
| Author | Hermes Agent + Teknium |
| License | MIT |
| Tags | `Teams`, `Microsoft Graph`, `Meetings`, `Productivity`, `Operations` |

## Reference: full SKILL.md

:::info
The following is the complete skill definition that Hermes loads when this skill is triggered. This is what the agent sees as instructions when the skill is active.
:::

# Teams Meeting Pipeline

Use this skill whenever the user asks about Microsoft Teams meeting summaries, transcripts, recordings, action items, Graph subscriptions, or any operational question about the Teams meeting pipeline. Works in any language — the triggers below are examples, not an exhaustive list.

Everything operator-facing is a `hermes teams-pipeline` subcommand run via the terminal tool. There are no new model tools for this pipeline — the CLI is the surface.

## Rust release contract (read first)

Docker/release packages ship **Rust only**. On that path:

- Manual CLI: `validate`, `token-health`, `fetch`/`run`/`replay` (both require `--organizer-user-id` Entra object-ID UUID), `list`/`show`, Graph **subscription admin**
- **No** gateway `msgraph_webhook` consumer — Graph POSTs are not ingested by `hermes-gateway`
- **No** automatic “subscription notification → job → summary” loop in the Rust binary
- **No** Notion / Linear / Teams sink writers — `TeamsPipelineConfig` **rejects** `notion`, `linear`, and `teams_delivery` keys under `meeting_pipeline` (even `enabled: false`)
- `hermes teams-pipeline validate` checks Graph `MSGRAPH_*` env presence, parses `platforms.teams.extra.meeting_pipeline`, and reports store stats — it does **not** inspect webhook enablement, `TEAMS_*` delivery env, or Teams `delivery_mode`

Optional Python/source gateway plugins (webhook listener, sink writers) are a **separate** out-of-tree surface. Do not treat them as shipped Rust behavior. See `/docs/user-guide/messaging/msgraph-webhook` (reference only).

## When to use this skill

The user is asking to:

- summarize a Teams meeting / extract action items / pull meeting notes **via CLI** (job already in the local store, or after a manual `fetch`/seed)
- check pipeline status, inspect a stored meeting job, or list recent jobs
- re-run a stored job that failed or needs a fresh **local** summary (not external re-delivery)
- validate Microsoft Graph CLI credentials after changing env or `meeting_pipeline` knobs
- troubleshoot empty `list`, failed jobs, or Graph token/permission errors
- manage Graph webhook **subscriptions as HTTP admin only** (create/list/renew/delete) — knowing that Rust does not consume the notifications

Multilingual trigger examples (not exhaustive):

- English: "summarize the Teams meeting", "pipeline status", "replay job X"
- Turkish: "Teams meeting özetle", "action item çıkar", "toplantı notu", "pipeline durumu", "replay job"

## Prerequisites

Before using the pipeline, verify these are set in `${HERMES_HOME:-~/.hermes}/.env`:

```bash
MSGRAPH_TENANT_ID=...
MSGRAPH_CLIENT_ID=...
MSGRAPH_CLIENT_SECRET=...
```

If any are missing, direct the user to the Azure app registration guide at `/docs/guides/microsoft-graph-app-registration` — they need an Azure AD app registration with admin-consented Graph **read** application permissions for transcripts/recordings before CLI `fetch`/`run` can call Graph.

Do **not** require `TEAMS_DELIVERY_MODE`, `TEAMS_INCOMING_WEBHOOK_URL`, `TEAMS_TEAM_ID` / `TEAMS_CHANNEL_ID` / `TEAMS_CHAT_ID`, or `MSGRAPH_WEBHOOK_*` for the Rust path — those are optional Python/source-only and ignored by the Rust release.

## Command reference

### Status and inspection (start here)

```bash
hermes teams-pipeline validate              # Graph env + meeting_pipeline parse + store — run first after any change
hermes teams-pipeline token-health          # Graph token status
hermes teams-pipeline token-health --force-refresh   # force a fresh token acquisition
hermes teams-pipeline list                  # recent meeting jobs in the local store
hermes teams-pipeline list --status failed  # only failed jobs
hermes teams-pipeline show <job-id>         # full detail of one job
hermes teams-pipeline subscriptions         # current Graph subscriptions (admin listing only)
```

### Manual run / debug

`--organizer-user-id` is **required** on both `fetch` and `run`/`replay`. Value must be a non-blank Microsoft Entra **object ID UUID** (not UPN, email, or display name). Used for all Graph artifact access scoping.

```bash
hermes teams-pipeline run <job-id> --organizer-user-id <entra-object-id-uuid>   # local summary only — no sink re-delivery
hermes teams-pipeline replay <job-id> --organizer-user-id <entra-object-id-uuid>
hermes teams-pipeline fetch --organizer-user-id <entra-object-id-uuid> --meeting-id <id>
hermes teams-pipeline fetch --organizer-user-id <entra-object-id-uuid> --join-web-url "<url>"
```

### Subscription management (admin only — not a Rust notification consumer)

Creating a Graph subscription does **not** make the Rust gateway receive or process notifications. Only create/renew subscriptions if a **non-Rust** notification URL consumer you maintain will handle POSTs.

```bash
hermes teams-pipeline subscribe \
  --resource communications/onlineMeetings/getAllTranscripts \
  --notification-url https://<your-non-rust-host>/msgraph/webhook \
  --client-state "<secret-you-control>"

hermes teams-pipeline renew-subscription <sub-id> --expiration <iso-8601>
hermes teams-pipeline delete-subscription <sub-id>
hermes teams-pipeline maintain-subscriptions            # renew near-expiry ones
hermes teams-pipeline maintain-subscriptions --dry-run  # show what would be renewed
```

## Decision tree for common asks

- User asks "why didn't I get a summary for today's meeting?" → On Rust, automatic Graph→summary is **unsupported**. Check `list` / `list --status failed`. If no job exists, nothing wrote the local store (Rust gateway does not enqueue from webhooks). Seed/run via CLI or a separate orchestrator.
- User asks "is setup working?" → `validate`, then `token-health`. Optionally `fetch --organizer-user-id <uuid> --meeting-id …` for a known meeting. Do **not** treat `subscriptions` as proof of automatic ingestion.
- User asks "re-run summary for meeting X" → `list` to find the job ID, `run <job-id> --organizer-user-id <uuid>` for a **local** re-summary. There is no external Teams/Notion/Linear re-delivery on Rust. If it fails, `show <job-id>` and `fetch` with the same required organizer UUID to dry-run artifact resolution.
- User asks "add meeting X to the pipeline" → Rust is **not** subscription-driven ingestion. Use `fetch --organizer-user-id <uuid> …` for artifacts; ensure a job exists in the local store, then `run` with the same flag.

## Critical pitfall: Graph subscriptions expire in 72 hours

Microsoft Graph caps change-notification subscriptions at 72 hours and **will not auto-renew them**. This matters **only** if you intentionally run a **non-Rust** notification consumer against those subscriptions. Rust release does not consume the POSTs either way.

If the user maintains an external consumer and reports notifications stopped:

1. Run `hermes teams-pipeline subscriptions` — empty or past `expirationDateTime` means renew/recreate.
2. Recreate with `subscribe` as shown above (URL must hit **their** consumer, not an assumed Rust listener).
3. Schedule `maintain-subscriptions` via cron/systemd if they need continuous external reception. See `/docs/guides/operate-teams-meeting-pipeline`.

Do **not** tell the user that renewing subscriptions alone restores automatic summaries inside the Rust binary.

## Other pitfalls

- **Transcript not available yet.** Teams takes time after a meeting ends to generate the transcript artifact. `fetch --organizer-user-id <uuid> --meeting-id …` on a just-ended meeting may return empty. Wait 2–5 minutes and retry.
- **Missing / invalid `--organizer-user-id`.** `fetch` and `run`/`replay` require a non-blank Microsoft Entra **object ID UUID**. UPN/email/display name are rejected.
- **Sink / delivery config on Rust.** Do not set `notion` / `linear` / `teams_delivery` under `meeting_pipeline` — parse/validate **fail closed**. Do not troubleshoot missing Teams channel posts via `TEAMS_*` or `delivery_mode` on the Rust path; those env/config keys are ignored by Rust (optional Python/source only).
- **Graph app permissions.** A token acquires cleanly (`token-health` passes) but Graph API calls return 401/403 when permissions were added but admin consent wasn't re-granted. Have the user revisit the app registration in the Azure portal and click "Grant admin consent" again.
- **Jobs missing from `list`.** Expected until something writes the local store (manual seed, tests, or a non-Rust orchestrator). `hermes gateway run` does not create meeting jobs from Graph webhooks in Rust.

## Optional Python / source plugins (separate)

If the user is deliberately running an upstream/source-tree Python gateway with `msgraph_webhook` or sink writers, that is **outside** the Rust release contract. Point them at `/docs/user-guide/messaging/msgraph-webhook` as reference only, and keep Rust CLI instructions above unchanged. Never mix Python sink YAML keys into the Rust `meeting_pipeline` object. External Teams delivery remains unsupported in this Rust release; the optional source plugin is not packaged or verified.

## Related docs

Point the user to these when they need more depth than this skill covers:

- Azure app registration walkthrough: `/docs/guides/microsoft-graph-app-registration`
- Full Rust pipeline contract: `/docs/user-guide/messaging/teams-meetings`
- Operator runbook: `/docs/guides/operate-teams-meeting-pipeline`
- Optional source webhook reference (not Rust): `/docs/user-guide/messaging/msgraph-webhook`
