# Upstream Sync Automation

This fork includes automation to keep `main` close to upstream while preserving
fork-specific history.

## Scripts

- `scripts/sync-upstream.sh`
  - One-shot upstream sync (`fetch -> merge -> test -> push`)
  - Default mode: create a sync branch and open a PR
- `scripts/cron-upstream-sync.sh`
  - Cron-safe wrapper around `sync-upstream.sh`
  - Uses a lock file (`~/.hermes/locks/upstream-sync.lock`) to avoid overlap
- `scripts/install-upstream-sync-cron.sh`
  - Installs/updates a crontab entry with a stable marker

## One-shot Manual Sync

```bash
bash scripts/sync-upstream.sh --dry-run
bash scripts/sync-upstream.sh
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

## Operational Notes

- Requires configured `origin` and `upstream` remotes.
- Requires a clean working tree.
- `gh` CLI is optional; without it the script still pushes the sync branch.
- Cron entry exports `REPO_ROOT` explicitly so the wrapper runs against the
  intended repository path.
- Default verification command is:
  - `cargo test -p hermes-gateway`
