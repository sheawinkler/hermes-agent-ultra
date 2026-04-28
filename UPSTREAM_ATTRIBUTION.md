# Upstream Attribution and Ownership

This repository is an independent Rust implementation and product line:

- Product repository: `sheawinkler/hermes-agent-ultra`
- Canonical upstream reference: `NousResearch/hermes-agent`

## Credit and Provenance

1. Upstream history remains credited to upstream contributors.
2. Ultra-specific Rust implementation and operations layers remain credited to this repository's contributors.
3. Feature parity work is tracked through explicit parity artifacts and sync workflows rather than silent code copy.
4. Intentional divergences are documented in `docs/parity/intentional-divergence.json`.

## Current Sync Snapshot

Fetched on `2026-04-28`:

- `origin/main`: `22e5906eaac119e3788109c9554476d2a5ea301f`
- `upstream/main`: `4bf0e75ae95fe33b47391a73bcf9bf5c128dd75b`
- Upstream remote URL: `git@github.com:NousResearch/hermes-agent.git`

The latest sync report is stored at:

- `.sync-reports/upstream-sync-20260428-182056.txt`

## Why Sync Uses Queue/Gates

Ultra and upstream histories can diverge significantly over time.  
To keep parity rigorous and auditable, Ultra uses:

- fetch + parity queue generation
- differential parity gates
- controlled branch/PR sync automation
- explicit conflict triage and risk gating

Instead of blindly merging unrelated histories into the Rust workspace.

## Audit Commands

```bash
git fetch upstream main --prune
git rev-list --left-right --count origin/main...upstream/main
git log --oneline origin/main..upstream/main | head -100
git diff --name-status origin/main...upstream/main
```

## Legal Note

Use of upstream-referenced content must remain within the terms of each source repository's license and attribution requirements. This repository does not claim upstream ownership.
