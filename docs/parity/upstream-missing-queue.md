# Upstream Missing Patch Queue

Generated: `2026-06-16T10:22:05.579269+00:00`

- Range: `main..upstream/main`; total commits tracked: `6077`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 2633 |
| #21 | GPAR-02 skills parity | 119 |
| #22 | GPAR-03 UX parity | 886 |
| #23 | GPAR-04 gateway/plugin-memory parity | 426 |
| #24 | GPAR-05 environments+parsers+benchmarks | 22 |
| #25 | GPAR-06 packaging/docs/install parity | 134 |
| #26 | GPAR-07 upstream queue backfill | 1857 |

| Disposition | Commit Count |
| --- | ---: |
| mirrored | 74 |
| pending | 21 |
| ported | 299 |
| superseded | 5683 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `cc8e5ec2afbf` | #20 | refactor(gateway): migrate Discord adapter to bundled plugin (full Teams parity) |
| `7849a3d73f2d` | #26 | fix(gateway,discord-plugin): _platform_status must respect is_connected=False, not silently fall back to check_fn |
| `b689624aeeef` | #25 | feat(ci): 4-way matrix slicing with LPT duration-balanced distribution |
| `510df6eaf47e` | #25 | test: 4-way slice benchmark (with cache save) |
| `e7cb5d4b68c3` | #25 | fix: clean push triggers |
| `dc4b0465b558` | #25 | feat(ci): use 6-way slicing based on benchmark results |
| `be89c2e4fa41` | #25 | ci(supply-chain): anchor install-hook regex at repo root (#31744) |
| `d8703e27f5c3` | #22 | feat(skills-hub): health checks, freshness badge, and a watchdog cron (#32345) |
| `2681c5a12d8d` | #20 | fix(photon): correct gateway start command (#45566) |
| `cc14b74718aa` | #22 | docs(profile): update clone-from references |
| `a218a0f1569c` | #20 | fix(agent,gateway,doctor): add SSL CA cert bundle fail-fast guard |
| `dc90ca4e1740` | #26 | fix(ssl): run CA guard during agent initialization |
| `7aaae7acd0d6` | #20 | fix(ssl): align guard docs and escape hatch |
| `723c2331bd23` | #20 | fix: make profile subprocess HOME policy explicit |
| `61ee2dbfdb40` | #20 | fix(s6): make profile gateway log parent writable (#46291) |
| `c1a70a543925` | #20 | 🐛 fix(disk-cleanup): prune protected cleanup walks |
| `40699c329265` | #20 | 🐛 fix(disk-cleanup): avoid brittle sweep review issues |
| `975b9f0a5426` | #22 | docs: recommend standard installer for development (#46646) |
| `c66ecf0bc30f` | #20 | feat(delegation): async background subagents via delegate_task(background=true) (#40946) |
| `5a0e0d35b94f` | #20 | fix(mattermost): preserve thread-local delivery hygiene |
| `5bfed0fe071a` | #22 | feat(skills): add optional payments skills (Stripe Link, MPP, Projects) (#31343) |
