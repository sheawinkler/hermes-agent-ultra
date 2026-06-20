# Chronos Managed Cron Contract

Status: Rust runtime contract for NAS-mediated managed cron.

Chronos lets a hosted Hermes Ultra gateway scale to zero while scheduled jobs
remain armed. The Rust runtime computes each job's next `fire_at`, asks Nous
Account Service (NAS) to arm exactly one one-shot for that time, and accepts an
authenticated NAS callback at `/api/cron/fire`.

The scheduler provider is active only when `cron.provider` is `chronos`.
Without complete Chronos config or a Nous Portal access token, the Rust
`CronScheduler` falls back to the built-in in-process ticker.

## Config

`config.yaml`:

```yaml
cron:
  provider: chronos
  chronos:
    portal_url: https://portal.nousresearch.com
    callback_url: https://agent.example.com
    expected_audience: agent:<instance_id>
    nas_jwks_url: https://portal.nousresearch.com/.well-known/jwks.json
```

Equivalent environment overrides:

- `HERMES_CRON_PROVIDER`
- `HERMES_CHRONOS_PORTAL_URL`
- `HERMES_CHRONOS_CALLBACK_URL`
- `HERMES_CHRONOS_EXPECTED_AUDIENCE`
- `HERMES_CHRONOS_NAS_JWKS_URL`
- `HERMES_CHRONOS_TOKEN_LEEWAY_SECONDS`
- `HERMES_CHRONOS_REQUEST_TIMEOUT_SECONDS`

## Agent To NAS

For every active job with a `next_run`, Hermes POSTs:

`POST {portal_url}/api/agent-cron/provision`

```json
{
  "job_id": "job-id",
  "fire_at": "2026-06-20T12:34:56Z",
  "agent_callback_url": "https://agent.example.com",
  "dedup_key": "job-id:2026-06-20T12:34:56Z"
}
```

Auth is the existing Nous Portal bearer token from `auth.json` or
`TOOL_GATEWAY_USER_TOKEN`. No scheduler secret is stored in the agent.

Hermes also uses:

- `POST /api/agent-cron/cancel` with `{"job_id":"..."}`
- `GET /api/agent-cron/list` returning `{"armed":[{"job_id":"...","fire_at":"..."}]}`

## NAS To Agent

NAS calls:

`POST {callback_url}/api/cron/fire`

Headers:

```text
Authorization: Bearer <NAS-minted JWT>
```

Body:

```json
{
  "job_id": "job-id",
  "fire_at": "2026-06-20T12:34:56Z"
}
```

The Rust verifier checks the JWT signature against `nas_jwks_url`, requires an
asymmetric RS/ES algorithm, validates `aud`, `iss`, `exp`, optional `nbf`, and
requires `purpose == "cron_fire"`. Invalid, expired, wrong-audience, or
wrong-purpose tokens return `401` and do not run jobs.

Valid callbacks return `202` immediately. The job runs in the background via the
same `CronScheduler` execution path as local scheduled jobs, so completion
persistence, repeat limits, delivery, `context_from`, workdir serialization, and
last-output capture stay identical.

Duplicate or stale fires are accepted but not dispatched. If `fire_at` is
present and does not match the job's current `next_run`, the callback is treated
as stale. If the job is already running, the callback is treated as a duplicate.

