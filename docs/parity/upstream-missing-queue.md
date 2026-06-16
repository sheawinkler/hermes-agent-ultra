# Upstream Missing Patch Queue

Generated: `2026-06-16T07:25:59.077811+00:00`

- Range: `main..upstream/main`; total commits tracked: `5961`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 2574 |
| #21 | GPAR-02 skills parity | 119 |
| #22 | GPAR-03 UX parity | 878 |
| #23 | GPAR-04 gateway/plugin-memory parity | 416 |
| #24 | GPAR-05 environments+parsers+benchmarks | 22 |
| #25 | GPAR-06 packaging/docs/install parity | 128 |
| #26 | GPAR-07 upstream queue backfill | 1824 |

| Disposition | Commit Count |
| --- | ---: |
| mirrored | 70 |
| pending | 24 |
| ported | 288 |
| superseded | 5579 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `2d474e39c7ee` | #20 | fix(acp): preserve memory provider tools |
| `2681c5a12d8d` | #20 | fix(photon): correct gateway start command (#45566) |
| `cc14b74718aa` | #22 | docs(profile): update clone-from references |
| `bf8effad023b` | #20 | fix(utils): copy fallback for atomic replace across devices (#43852) |
| `a218a0f1569c` | #20 | fix(agent,gateway,doctor): add SSL CA cert bundle fail-fast guard |
| `dc90ca4e1740` | #26 | fix(ssl): run CA guard during agent initialization |
| `7aaae7acd0d6` | #20 | fix(ssl): align guard docs and escape hatch |
| `8d5d36d79358` | #20 | fix(dispatch): forward session_id into registry.dispatch (#28479) |
| `5e851bc6bc51` | #26 | fix(discord): cap slash commands at Discord's 100-command limit |
| `b00060ce545c` | #20 | fix(agent): expose HERMES_REAL_HOME in subprocess envs for profile isolation |
| `723c2331bd23` | #20 | fix: make profile subprocess HOME policy explicit |
| `972a9885ee20` | #20 | fix(mcp): block exfil-shaped stdio server configs (#46083) |
| `efbe1635dd2e` | #20 | fix(gateway): include replied-to media attachments (#46107) |
| `bff78a34dc44` | #20 | feat(zai): add GLM-5.2 with verified 1M context window |
| `f3fe99863d13` | #20 | revert(web): remove keyless Parallel search fallback (#46350) |
| `61ee2dbfdb40` | #20 | fix(s6): make profile gateway log parent writable (#46291) |
| `c1a70a543925` | #20 | 🐛 fix(disk-cleanup): prune protected cleanup walks |
| `40699c329265` | #20 | 🐛 fix(disk-cleanup): avoid brittle sweep review issues |
| `975b9f0a5426` | #22 | docs: recommend standard installer for development (#46646) |
| `5b2604df999c` | #26 | fix(state): skip redundant trigram backfill before v11 FTS rebuild |
| `3e7e9b24d40c` | #20 | fix: harden salvaged session and browser improvements |
| `c66ecf0bc30f` | #20 | feat(delegation): async background subagents via delegate_task(background=true) (#40946) |
| `5a0e0d35b94f` | #20 | fix(mattermost): preserve thread-local delivery hygiene |
| `5bfed0fe071a` | #22 | feat(skills): add optional payments skills (Stripe Link, MPP, Projects) (#31343) |
