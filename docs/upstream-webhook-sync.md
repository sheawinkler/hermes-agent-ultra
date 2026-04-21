# Upstream Webhook Queue Sync

This runbook describes the webhook-driven sync path that complements cron-based
syncing.

## Why This Exists

Cron polling works, but webhook-triggered syncs reduce lag and wasted runs:

- Upstream push event arrives immediately
- Event is queued
- Worker processes event and runs `scripts/sync-upstream.sh`
- Risk gate blocks sensitive diffs for review/implementation

This allows fast sync for low-risk deltas while preserving safety for parity work
that needs manual coding.

## Components

- `scripts/upstream_webhook_sync.py listen`
  - Receives GitHub webhook `push` events
  - Verifies `X-Hub-Signature-256` if `GITHUB_WEBHOOK_SECRET` is set
  - Filters by expected upstream repo/ref
  - Publishes to queue backend
- `scripts/upstream_webhook_sync.py worker`
  - Consumes queue event
  - Runs `scripts/sync-upstream.sh` with strict risk gate
  - Classifies result:
    - `done`: merged/cherry-picked and branch/PR flow completed
    - `risk_blocked`: strict gate matched high-risk paths
    - `conflict`: merge/cherry-pick conflict
    - `dead`: repeated transient failures exhausted retries

## Queue Backends

Backends are selectable with `--backend`:

- `sqlite` (default)
  - No external dependency
  - Durable local queue in `.sync-queue/upstream-events.db`
  - Good default for single-host deployment
- `sqs` (optional)
  - Requires `boto3`
  - Uses SQS queue URL
- `kafka` (optional)
  - Requires `kafka-python`
  - Uses topic and consumer group

## End-to-End Flow

1. GitHub sends `push` webhook for `Lumio-Research/hermes-agent-rs` `refs/heads/main`.
2. Listener validates signature and scope filters.
3. Event is enqueued.
4. Worker dequeues event and runs:
   - `scripts/sync-upstream.sh --strict-risk-gate ...`
5. If sync succeeds:
   - branch/PR path proceeds (or direct mode if configured)
6. If strict risk gate blocks or conflict occurs:
   - event is marked terminal (`risk_blocked` / `conflict`)
   - implementation work is required before parity is restored

## Where Coding Work Happens

Coding implementation occurs when the worker hits terminal non-green outcomes:

- `risk_blocked`
  - high-risk files changed upstream
  - expected path: implement/port changes safely in this fork
- `conflict`
  - rebase/cherry-pick/merge conflict resolution with behavior-preserving edits

Optional assist hook (`UPSTREAM_SYNC_ASSIST_CMD`) can run automatically on
`risk_blocked`, `conflict`, or terminal retry exhaustion to open/trigger your
Nous/Codex workflow.

## Nous/Codex Assist Integration

Set a command that the worker runs with event/report context:

```bash
export UPSTREAM_SYNC_ASSIST_CMD='echo "assist on $UPSTREAM_SYNC_OUTCOME $UPSTREAM_SYNC_REPORT_PATH"'
```

The worker exports:

- `UPSTREAM_SYNC_DELIVERY_ID`
- `UPSTREAM_SYNC_REPOSITORY`
- `UPSTREAM_SYNC_REF`
- `UPSTREAM_SYNC_AFTER_SHA`
- `UPSTREAM_SYNC_OUTCOME`
- `UPSTREAM_SYNC_REPORT_PATH`

You can replace the `echo` command with a Nous invocation script that opens a
task to port blocked upstream changes.

## Quick Start (SQLite)

Start listener:

```bash
python3 scripts/upstream_webhook_sync.py listen \
  --backend sqlite \
  --sqlite-path .sync-queue/upstream-events.db \
  --host 0.0.0.0 \
  --port 8099 \
  --path /github/upstream-sync \
  --expected-repo Lumio-Research/hermes-agent-rs \
  --expected-ref refs/heads/main
```

Start worker:

```bash
python3 scripts/upstream_webhook_sync.py worker \
  --backend sqlite \
  --sqlite-path .sync-queue/upstream-events.db \
  --repo-root /Users/sheawinkler/Documents/Projects/hermes-agent-ultra \
  --strategy merge \
  --strict-risk-gate
```

## Reliability Notes

- Keep strict risk gate enabled for unattended runs.
- Do not auto-bypass risk gates in production.
- For high throughput, use SQS/Kafka and scale workers horizontally.
- Keep retry count modest (`--max-attempts`) to avoid repeated unsafe retries.
