---
sidebar_position: 6
title: "Teams Meetings"
description: "Rust Teams meeting pipeline: CLI fetch/replay, Graph artifact checks, and STT fallback limits — not an automatic webhook consumer"
---

# Microsoft Teams Meetings

The **Rust** Teams meeting surface is a **CLI-driven** pipeline library plus `hermes teams-pipeline` commands. It can resolve a meeting via Microsoft Graph, prefer transcript artifacts, fall back to recording download + STT (with hard size limits), store job records locally, and **manually replay** a stored job.

It is **not** a complete automatic “Graph webhook → summary → Notion/Linear/Teams” product in this runtime:

| Claim | Rust reality |
|-------|----------------|
| Gateway / `msgraph_webhook` receives Graph notifications and runs the pipeline | **Unsupported** — no Rust gateway adapter invokes `TeamsMeetingPipeline::run_notification` |
| Presence of `notion` / `linear` / `teams_delivery` under `meeting_pipeline` | **Fail-closed** — `TeamsPipelineConfig::from_value` rejects those keys even when `enabled: false`; CLI validate/run fail rather than no-op |
| Python `teams_pipeline` / `msgraph_webhook` gateway plugin | **Upstream/source reference only** — not installed by the Rust Docker/release package; not the Rust CLI wiring |

For day-2 operator commands under the same Rust contract: [Operate the Teams Meeting Pipeline](/docs/guides/operate-teams-meeting-pipeline). Optional historical webhook plugin notes: [Microsoft Graph Webhook Listener](/docs/user-guide/messaging/msgraph-webhook) (source/reference — not a shipped Rust consumer).

## What the Rust path does

1. **Manual / CLI job run** — `hermes teams-pipeline run|replay <job-id> --organizer-user-id <entra-object-id-uuid>` loads a stored job and runs artifact resolution + optional STT + summary generation in-process. `--organizer-user-id` is **required**, non-blank, and must be a Microsoft Entra **object ID UUID** (not UPN/email).
2. **Artifact inspection** — `hermes teams-pipeline fetch --organizer-user-id <entra-object-id-uuid> (--meeting-id <id>|--join-web-url <url>)` resolves a meeting and prints transcript/recording metadata. Same required organizer UUID; no sink delivery.
3. **Graph subscription admin** — create/list/renew/delete/maintain Graph subscriptions and check token health (subscription HTTP admin only — **not** a notification consumer).
4. **Config / store validation** — `hermes teams-pipeline validate` checks Graph app-only env credentials (`MSGRAPH_*`), parses `platforms.teams.extra.meeting_pipeline` into runtime config, and reports local store path/stats. It does **not** inspect `msgraph_webhook` enablement or Teams delivery mode fields.
5. **Local job store** — durable job JSON under the Teams pipeline store path (default under Hermes home; overridable).

Optional STT fallback uses the same `TranscriptionHandler` as the agent tool, with the Teams pipeline passing its **internal temp root** as an extra trusted media root for files it wrote (see [Voice & TTS — Transcription](/docs/user-guide/features/tts#transcription-tool-stt)).

## Verified media / Graph limits (Rust)

These caps are enforced in the Rust pipeline implementation:

| Limit | Value |
|-------|--------|
| Max recording download | **512 MiB** |
| Max transcript payload | **8 MiB** |
| Max STT upload segment | **24_000_000 bytes** (~24 MB; margin under common 25 MB provider caps) |
| Max ffmpeg audio segments | **128** |
| Graph JSON / pagination bounds | additional internal caps (4 MiB single JSON, pagination page/item/byte ceilings) |

Oversized artifacts fail closed rather than partial-upload guessing.

## Prerequisites

- Working Hermes install with the `teams-pipeline` CLI entrypoint
- Microsoft Graph **app-only** credentials for the meeting resources you will fetch
- Existing [Microsoft Teams bot setup](/docs/user-guide/messaging/teams) only if you use the **separate** Teams messaging adapter for chat — it is not wired as a pipeline sink writer on the Rust CLI path
- `ffmpeg` if you enable recording → audio extract for STT fallback

A public HTTPS Graph webhook URL is **not** required for the Rust CLI/manual path. Subscription admin commands can still talk to Graph; nothing in Rust gateway code consumes those notifications today.

## Step 1: Microsoft Graph credentials

```bash
MSGRAPH_TENANT_ID=<tenant-id>
MSGRAPH_CLIENT_ID=<client-id>
MSGRAPH_CLIENT_SECRET=<client-secret>
```

Used by the Graph client for meeting resolution, artifact fetches, token health, and subscription admin.

## Step 2: Pipeline knobs (optional)

Pipeline knobs live under `platforms.teams.extra.meeting_pipeline` when a `teams` platform entry exists. **Do not** put `notion`, `linear`, or `teams_delivery` in this object on the Rust path — their presence is rejected.

```yaml
platforms:
  teams:
    enabled: true
    extra:
      meeting_pipeline:
        transcript_min_chars: 80
        transcript_required: false
        transcription_fallback: true
        ffmpeg_extract_audio: true
        # Do NOT add notion / linear / teams_delivery here on Rust —
        # TeamsPipelineConfig rejects those keys (fail closed).
```

`build_pipeline_runtime_config` reads **only** `platforms.teams.extra.meeting_pipeline` (or `{}`). It does **not** merge top-level Teams `delivery_mode` / channel / webhook fields. Parsing that object through `TeamsPipelineConfig::from_value` fails if unsupported sink keys are present.

## Step 3: CLI operations

```bash
# Inspect / validate
hermes teams-pipeline validate
hermes teams-pipeline token-health
hermes teams-pipeline list
hermes teams-pipeline show <job-id>

# Fetch artifacts (manual local path only)
# --organizer-user-id REQUIRED: Microsoft Entra object ID UUID (not UPN/email)
hermes teams-pipeline fetch --organizer-user-id <entra-object-id-uuid> --meeting-id <id>
# or: --join-web-url <url>

# Manually run / replay a stored job (local summary only; same required organizer UUID)
hermes teams-pipeline run <job-id> --organizer-user-id <entra-object-id-uuid>
# alias: hermes teams-pipeline replay <job-id> --organizer-user-id <entra-object-id-uuid>

# Graph subscription administration (not a notification consumer)
hermes teams-pipeline subscriptions
hermes teams-pipeline subscribe --resource <graph-resource> --notification-url <url> ...
hermes teams-pipeline maintain-subscriptions
```

Jobs appear in `list` when something has already written them into the local store (manual seeding, tests, or a **non-Rust** orchestrator). The Rust gateway does not create jobs from live Graph webhooks.

## What is not shipped (Rust)

- No `msgraph_webhook` platform adapter in `hermes-gateway` that accepts Graph POSTs and calls `run_notification`
- No automatic “start gateway → meetings summarize themselves” loop
- No external sink config under Rust `meeting_pipeline` — `notion` / `linear` / `teams_delivery` keys are **rejected** (not silently skipped), even with `enabled: false`
- No claim that an upstream Python plugin is packaged into the Rust release artifact

### Optional Python / source plugin (separate)

Automatic webhook orchestration or Notion/Linear/Teams sinks, if they exist at all, belong to an **out-of-tree / source** Python gateway plugin you maintain yourself. That surface is **not** the Rust CLI contract and is **not** installed by the Docker/release package. Do not mix Python plugin YAML (including sink blocks) into the Rust `meeting_pipeline` object expecting a soft ignore.

## Validation

```bash
hermes teams-pipeline validate
hermes teams-pipeline token-health
```

`validate` returns Graph credential booleans, `runtime: "manual_local_only"`, an `external_sinks` unsupported notice, store path/stats, and any meeting_pipeline parse issues. It does **not** report `msgraph_webhook` enabled state or Teams delivery configuration.

## Troubleshooting

| Problem | What to check |
|---------|---------------|
| Expected automatic summaries after Graph events | Unsupported on Rust — use CLI `fetch`/`run` with `--organizer-user-id`, or a separate orchestrator |
| `meeting_pipeline` contains `notion` / `linear` / `teams_delivery` | Expected fail-closed parse/validate error — remove those keys on Rust |
| `fetch`/`run` errors about `--organizer-user-id` | Required non-blank Microsoft Entra **object ID UUID** (not UPN/email) on every Graph artifact fetch/run |
| Jobs do not appear in `list` | Nothing has written the local store; Rust gateway does not enqueue from webhooks |
| Transcript-first never succeeds | Graph permissions and transcript artifact existence for that meeting |
| Recording / STT fallback fails | `ffmpeg`, recording permissions, and media limits (512 MiB recording, ~24 MB STT segment) |
| Expecting `validate` to check webhook/delivery setup | It does not — only Graph env credentials, meeting_pipeline parse, and store stats |

## Related Docs

- [Microsoft Teams bot setup](/docs/user-guide/messaging/teams) (outbound messaging adapter — separate from pipeline sinks)
- [Operate the Teams Meeting Pipeline](/docs/guides/operate-teams-meeting-pipeline) (operator worksheet; verify against this Rust contract)
- [Voice & TTS — Transcription](/docs/user-guide/features/tts#transcription-tool-stt) (STT tool + approved media roots)
