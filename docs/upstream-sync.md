# Upstream Sync Automation

This fork includes automation to keep `main` close to upstream while preserving
fork-specific history.

## Scripts

- `scripts/sync-upstream.sh`
  - One-shot upstream sync (`fetch -> strategy apply -> test -> push`)
  - Default mode: create a sync branch and open a PR
  - Supports `--draft-pr` for safer review-first PR creation
  - Supports `--pr-labels` to apply metadata/risk labels on the created PR
  - Runs adversarial regression gate by default (`scripts/run-redteam-gate.py`)
  - Supports `--no-redteam-gate` and `--redteam-cmd` overrides
  - Supports `--strategy merge|cherry-pick`
  - Supports strict risk gating via `--strict-risk-gate`
  - Emits timestamped reports under `.sync-reports/`
  - Can auto-create a labeled GitHub issue on conflict
- `scripts/cron-upstream-sync.sh`
  - Cron-safe wrapper around `sync-upstream.sh`
  - Uses a lock file (`~/.hermes/locks/upstream-sync.lock`) to avoid overlap
  - Reads optional env knobs: `SYNC_STRATEGY`, `REPORT_DIR`, `CONFLICT_LABEL`, `CREATE_CONFLICT_ISSUE`, `STRICT_RISK_GATE`, `ALLOW_RISK_PATHS`, `RISK_PATHS_FILE`
- `scripts/install-upstream-sync-cron.sh`
  - Installs/updates a crontab entry with a stable marker
- `scripts/run-adapter-chaos-harness.py`
  - Runs deterministic adapter chaos scenarios (timeout/5xx/rate-limit)
  - Emits JSON diagnostics under `.sync-reports/adapter-chaos-<timestamp>.json`
- `scripts/run-zero-copy-hotpath-bench.py`
  - Runs zero-copy policy hot-path benchmark test and captures ns/eval evidence
  - Emits JSON diagnostics under `.sync-reports/zero-copy-hotpath-<timestamp>.json`

## One-shot Manual Sync

```bash
bash scripts/sync-upstream.sh --dry-run
bash scripts/sync-upstream.sh
bash scripts/sync-upstream.sh --draft-pr
bash scripts/sync-upstream.sh --draft-pr --pr-labels "upstream-sync,parity-sync,risk-reviewed"
bash scripts/sync-upstream.sh --redteam-cmd "python3 scripts/run-redteam-gate.py --suite scripts/redteam-cases.json"
python3 scripts/run-adapter-chaos-harness.py --repo-root .
python3 scripts/run-zero-copy-hotpath-bench.py --repo-root .
```

Cherry-pick mode for linear upstream replay:

```bash
bash scripts/sync-upstream.sh --strategy cherry-pick
```

Strict risk gate (blocks sync if high-risk paths changed upstream):

```bash
bash scripts/sync-upstream.sh --strict-risk-gate
```

Explicit one-run bypass after manual review:

```bash
bash scripts/sync-upstream.sh --strict-risk-gate --allow-risk-paths
```

## Direct-to-main Mode (No PR)

```bash
bash scripts/sync-upstream.sh --mode direct-main
```

Use this only when you explicitly want automation to push `main` directly.

## Install Cron Schedule

Default schedule is every 6 hours at minute 17:

```bash
bash scripts/install-upstream-sync-cron.sh
```

Custom schedule example (daily at 03:10):

```bash
bash scripts/install-upstream-sync-cron.sh "10 3 * * *"
```

Default log path:
- `~/.hermes/logs/upstream-sync.log`

Default report path:
- `.sync-reports/upstream-sync-<timestamp>.txt`

## Operational Notes

- Requires configured `origin` and `upstream` remotes.
- By default, sync validates that `upstream` points to `NousResearch/hermes-agent`.
  - Override only when intentional: `ALLOW_NON_OFFICIAL_UPSTREAM=1`.
  - Optional override target repo string: `EXPECTED_UPSTREAM_REPO=<owner/repo>`.
- Requires a clean working tree.
- `gh` CLI is optional; without it the script still pushes the sync branch. Conflict issue auto-creation is disabled when `gh` is unavailable.
- Sync PR bodies now include parity queue summary, drift artifact paths, and test guidance for merge reviewers.
- Sync report includes `redteam_report` when adversarial gate runs.
- Cron entry exports `REPO_ROOT` explicitly so the wrapper runs against the
  intended repository path.
- On conflicts, the script writes `.sync-reports/upstream-sync-<timestamp>-conflict.txt`.
  - For `--strategy cherry-pick`, it also records rollback tag `rollback/upstream-sync-<timestamp>`.
- Strict risk patterns default file:
  - `scripts/upstream-risk-paths.txt`
- Cron installer defaults to `STRICT_RISK_GATE=1` so unattended syncs pause on sensitive upstream changes.
- Default verification command is:
  - `cargo test -p hermes-gateway`

## Webhook + Queue Mode

For event-driven sync (push-triggered, not schedule-triggered), see:

- `docs/upstream-webhook-sync.md`

This path supports SQLite (default), SQS, and Kafka queue backends and is the
recommended architecture when you want near-real-time upstream ingestion with
strict risk gating.

## Parity Matrix Snapshot

To generate a reproducible parity snapshot against `upstream/main`:

```bash
python3 scripts/generate-parity-matrix.py
```

Artifacts are written to:

- `docs/parity/parity-matrix.json`
- `docs/parity/parity-matrix.md`
- `docs/parity/intentional-divergence.json` (tracked divergence registry)

The matrix uses tree-level blob comparison (works with divergent histories) and
`git cherry` patch-id mapping for represented vs missing upstream commits.
