# Upstream Sync Automation

This fork includes automation to keep `main` close to upstream while preserving
fork-specific history.

## Scripts

- `scripts/sync-upstream.sh`
  - One-shot upstream sync (`fetch -> strategy apply -> test -> push`)
  - Default mode: create a sync branch and open a PR
  - Supports `--strategy merge|cherry-pick`
  - Emits timestamped reports under `.sync-reports/`
  - Can auto-create a labeled GitHub issue on conflict
- `scripts/cron-upstream-sync.sh`
  - Cron-safe wrapper around `sync-upstream.sh`
  - Uses a lock file (`~/.hermes/locks/upstream-sync.lock`) to avoid overlap
  - Reads optional env knobs: `SYNC_STRATEGY`, `REPORT_DIR`, `CONFLICT_LABEL`, `CREATE_CONFLICT_ISSUE`
- `scripts/install-upstream-sync-cron.sh`
  - Installs/updates a crontab entry with a stable marker

## One-shot Manual Sync

```bash
bash scripts/sync-upstream.sh --dry-run
bash scripts/sync-upstream.sh
```

Cherry-pick mode for linear upstream replay:

```bash
bash scripts/sync-upstream.sh --strategy cherry-pick
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
- Requires a clean working tree.
- `gh` CLI is optional; without it the script still pushes the sync branch. Conflict issue auto-creation is disabled when `gh` is unavailable.
- Cron entry exports `REPO_ROOT` explicitly so the wrapper runs against the
  intended repository path.
- On conflicts, the script writes `.sync-reports/upstream-sync-<timestamp>-conflict.txt`.
  - For `--strategy cherry-pick`, it also records rollback tag `rollback/upstream-sync-<timestamp>`.
- Default verification command is:
  - `cargo test -p hermes-gateway`
