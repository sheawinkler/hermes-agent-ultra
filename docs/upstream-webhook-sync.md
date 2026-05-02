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
  - Runs adversarial red-team gate by default (disable with `--no-redteam-gate`)
  - Can run consolidated elite gate (`--elite-gate`, command override via `--elite-cmd`)
  - Passes PR labels (`--pr-labels`) through to sync script for auto-labeled parity PRs
  - Runs CLI command/action parity drift check (`HEAD` vs `upstream/main` or event SHA)
  - Runs full global parity audit chain (matrix/status/intent/adapter/divergence/queue/proof)
  - Emits machine-readable drift artifacts under `.sync-reports/cli-surface-drift-*.json`
  - Emits global parity artifacts under `.sync-reports/global-parity-drift-*.json`
  - Comments parent upkeep issue (`#13` by default) when drift is detected
  - Comments global parity epic issue (`#19` by default) when global parity gate drifts
  - Auto-opens tagged parity drift issues for new drift fingerprints
  - Auto-creates per-upstream-commit upkeep issues with SHA-level dedupe/state
    - default parent tracker issue: `#43`
    - default state file: `.sync-reports/upkeep-commit-queue-state.json`
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

1. GitHub sends `push` webhook for `NousResearch/hermes-agent` `refs/heads/main`.
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
  --expected-repo NousResearch/hermes-agent \
  --expected-ref refs/heads/main
```

Start worker:

```bash
python3 scripts/upstream_webhook_sync.py worker \
  --backend sqlite \
  --sqlite-path .sync-queue/upstream-events.db \
  --repo-root /Users/sheawinkler/Documents/Projects/hermes-agent-ultra \
  --strategy merge \
  --strict-risk-gate \
  --draft-pr \
  --pr-labels upstream-sync,parity-sync \
  --redteam-cmd "python3 scripts/run-redteam-gate.py" \
  --elite-gate \
  --elite-cmd "python3 scripts/run-elite-sync-gate.py" \
  --elite-rollback-cmd "git reset --hard origin/main"
```

Optional parity drift flags:

```bash
python3 scripts/upstream_webhook_sync.py worker \
  --parity-upstream-ref upstream/main \
  --parity-parent-issue 13 \
  --parity-labels parity,parity-upkeep
```

Upkeep commit queue flags:

```bash
python3 scripts/upstream_webhook_sync.py worker \
  --upkeep-commit-parent-issue 43 \
  --upkeep-commit-labels parity,parity-upkeep,upstream-sync \
  --upkeep-commit-max-per-event 40
```

Disable drift checks or issue auto-open:

```bash
python3 scripts/upstream_webhook_sync.py worker \
  --disable-parity-drift-check \
  --no-parity-open-issues
```

Disable global parity checks or issue auto-open:

```bash
python3 scripts/upstream_webhook_sync.py worker \
  --disable-global-parity-check \
  --no-global-parity-open-issues
```

Disable upkeep commit queue issueing:

```bash
python3 scripts/upstream_webhook_sync.py worker \
  --disable-upkeep-commit-queue
```

## launchd Deployment (Recommended on macOS)

Single-command guided setup (recommended):

```bash
bash scripts/setup-upstream-webhook-launchd.sh
```

This command will:

- scaffold/update launchd plist files and env file
- show what is already configured (with secret masking)
- auto-generate `GITHUB_WEBHOOK_SECRET` if missing
- prompt for remaining missing critical values in interactive terminals
- load/start listener + worker and print final status/log tails
- enforce dev-only runtime guardrails (role + hostname)

After auto-generation, copy `GITHUB_WEBHOOK_SECRET` from the env file to the
GitHub webhook `Secret` field for signature verification.

Non-interactive examples:

```bash
bash scripts/setup-upstream-webhook-launchd.sh --show-only
bash scripts/setup-upstream-webhook-launchd.sh --no-auto-secret
```

### Dev/Runtime Separation Guard

Launch wrappers now require:

- `UPSTREAM_SYNC_RUNTIME_ROLE=dev`
- `UPSTREAM_SYNC_ALLOWED_HOSTNAME` matching the local host

These are auto-populated by the installer for your current machine. If the env
file is copied to a runtime/agent host, listener/worker startup will refuse to
run unless the guard is explicitly bypassed with:

- `UPSTREAM_SYNC_DISABLE_DEV_GUARD=1` (not recommended)

Install user agents:

```bash
bash scripts/install-upstream-webhook-launchd.sh
```

Inspect status and logs:

```bash
bash scripts/status-upstream-webhook-launchd.sh
```

Uninstall agents:

```bash
bash scripts/uninstall-upstream-webhook-launchd.sh
```

The installer creates:

- `~/Library/LaunchAgents/com.hermes_agent_ultra.upstream_webhook_listener.plist`
- `~/Library/LaunchAgents/com.hermes_agent_ultra.upstream_webhook_worker.plist`
- `~/.hermes-agent-ultra/upstream-webhook-sync.env` (runtime config + secrets)
- Logs under `~/.hermes-agent-ultra/logs/`

Relevant env keys in `~/.hermes-agent-ultra/upstream-webhook-sync.env`:

- `UPSTREAM_SYNC_DISABLE_PARITY_DRIFT_CHECK=0|1`
- `UPSTREAM_SYNC_ELITE_GATE=0|1`
- `UPSTREAM_SYNC_ELITE_CMD=python3 scripts/run-elite-sync-gate.py`
- `UPSTREAM_SYNC_ELITE_ROLLBACK_CMD=<optional rollback command>`
- `UPSTREAM_SYNC_PARITY_UPSTREAM_REF=upstream/main`
- `UPSTREAM_SYNC_PARITY_PARENT_ISSUE=13`
- `UPSTREAM_SYNC_PARITY_LABELS=parity,parity-upkeep`
- `UPSTREAM_SYNC_PARITY_OPEN_ISSUES=1|0`
- `UPSTREAM_SYNC_DISABLE_GLOBAL_PARITY_CHECK=0|1`
- `UPSTREAM_SYNC_GLOBAL_PARITY_PARENT_ISSUE=19`
- `UPSTREAM_SYNC_GLOBAL_PARITY_LABELS=parity,parity-upkeep`
- `UPSTREAM_SYNC_GLOBAL_PARITY_OPEN_ISSUES=1|0`
- `UPSTREAM_SYNC_GLOBAL_PARITY_MAX_QUEUE_COMMITS=0` (0 = full upstream missing range)
- `UPSTREAM_SYNC_UPKEEP_COMMIT_STATE_PATH=.sync-reports/upkeep-commit-queue-state.json`

## Reliability Notes

- Keep strict risk gate enabled for unattended runs.
- Do not auto-bypass risk gates in production.
- For high throughput, use SQS/Kafka and scale workers horizontally.
- Keep retry count modest (`--max-attempts`) to avoid repeated unsafe retries.
