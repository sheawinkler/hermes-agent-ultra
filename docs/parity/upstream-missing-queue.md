# Upstream Missing Patch Queue

Generated: `2026-06-16T16:28:08.867Z`

- Range: `main..upstream/main`; total commits tracked: `6101`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 2645 |
| #21 | GPAR-02 skills parity | 119 |
| #22 | GPAR-03 UX parity | 886 |
| #23 | GPAR-04 gateway/plugin-memory parity | 427 |
| #24 | GPAR-05 environments+parsers+benchmarks | 22 |
| #25 | GPAR-06 packaging/docs/install parity | 134 |
| #26 | GPAR-07 upstream queue backfill | 1868 |

| Disposition | Commit Count |
| --- | ---: |
| mirrored | 74 |
| pending | 21 |
| ported | 305 |
| superseded | 5701 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `cc8e5ec2afbf` | #20 | refactor(gateway): migrate Discord adapter to bundled plugin (full Teams parity) |
| `7849a3d73f2d` | #26 | fix(gateway,discord-plugin): _platform_status must respect is_connected=False, not silently fall back to check_fn |
| `d8703e27f5c3` | #22 | feat(skills-hub): health checks, freshness badge, and a watchdog cron (#32345) |
| `2681c5a12d8d` | #20 | fix(photon): correct gateway start command (#45566) |
| `cc14b74718aa` | #22 | docs(profile): update clone-from references |
| `a218a0f1569c` | #20 | fix(agent,gateway,doctor): add SSL CA cert bundle fail-fast guard |
| `dc90ca4e1740` | #26 | fix(ssl): run CA guard during agent initialization |
| `7aaae7acd0d6` | #20 | fix(ssl): align guard docs and escape hatch |
| `723c2331bd23` | #20 | fix: make profile subprocess HOME policy explicit |
| `61ee2dbfdb40` | #20 | fix(s6): make profile gateway log parent writable (#46291) |
| `975b9f0a5426` | #22 | docs: recommend standard installer for development (#46646) |
| `c92a95a130cc` | #26 | feat(desktop): move model selector from statusbar to composer |
| `989d5d0cb72a` | #26 | fix(desktop): declutter date-pinned model snapshots in the picker |
| `0e81d2fb71c1` | #26 | feat(desktop): per-model effort/fast presets in the picker |
| `a0ec4f52b948` | #20 | feat(desktop): disconnect external (CLI-managed) providers |
| `dd0e3e0a052a` | #26 | fix(desktop): tighten thread content top padding |
| `5b3fa2636632` | #20 | fix(photon): unify project identifiers and update documentation for Spectrum provisioning |
| `a68ac0c49af1` | #26 | feat(desktop): allow /browser connect on a local gateway (#47245) |
| `cb6b4127e795` | #20 | refactor(desktop): make composer model picker sticky session state |
| `7d938cc5c9c7` | #20 | fix(desktop): keep live model switch metadata truthful |
| `80e4b8985ea9` | #26 | feat(desktop): tighten composer model picker interactions |
