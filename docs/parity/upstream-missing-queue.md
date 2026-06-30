# Upstream Missing Patch Queue

Generated: `2026-06-30T08:36:56.665532+00:00`

- Range: `main..upstream/main`; total commits tracked: `7672`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 3505 |
| #21 | GPAR-02 skills parity | 132 |
| #22 | GPAR-03 UX parity | 1003 |
| #23 | GPAR-04 gateway/plugin-memory parity | 503 |
| #24 | GPAR-05 environments+parsers+benchmarks | 25 |
| #25 | GPAR-06 packaging/docs/install parity | 165 |
| #26 | GPAR-07 upstream queue backfill | 2339 |

| Disposition | Commit Count |
| --- | ---: |
| mirrored | 97 |
| pending | 20 |
| ported | 540 |
| superseded | 7015 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `50f685521734` | #20 | feat(moa): make /moa one-shot only; route preset switching through the model picker |
| `f67c0b3e60ba` | #21 | docs(hermes-agent skill): cover v0.13–v0.17 features, fix stale claims, tighten (#53566) |
| `917f6bdb00b8` | #20 | fix(tools): let vision pick any provider+model, not just OpenRouter (#53606) |
| `163cb24d45d8` | #22 | feat(moa): render reference-model blocks in TUI and desktop, not just CLI (#53855) |
| `a94f657a5059` | #20 | fix(tui): route completion RPCs to the pool so they can't freeze the TUI (#53895) |
| `d43e0cf304a1` | #20 | fix(agent): config-driven intent-ack continuation for all api_modes (#27881) (#53943) |
| `a8c862900b9c` | #20 | fix(tui): sanitize replay history on WebUI/TUI session resume (#29086) (#53939) |
| `fde1c8570ffe` | #20 | fix(tui_gateway): suppress WS peer-hangup teardown error flood (#50005) (#54126) |
| `6d879d486b19` | #20 | fix(dashboard): close PTY WebSocket on child EOF to stop FD leak (#54028) (#54123) |
| `5c2c85c5452f` | #20 | fix(tui): start MCP discovery for websocket sessions |
| `b31b0b9d95d1` | #22 | docs: reconcile docs with code across last 3 releases (#54254) |
| `dff491a2b993` | #22 | feat(cli): add headless `hermes serve` backend; desktop no longer launches `dashboard` |
| `476875acb9f0` | #22 | Add dashboard backup upload and download |
| `fd324562d3ad` | #20 | feat(desktop): add context usage breakdown popover |
| `808ba82125e2` | #25 | feat(ci): add CI timing report |
| `66ba9e06d925` | #25 | change(ci): remove lint PR comment |
| `cca8b4ef4e3f` | #25 | fix(ci): unify amd64/arm64 docker pipelines |
| `41c85fb9469b` | #26 | fix(agents.md): fix documentation on subprocess isolation in tests |
| `9ce79cd64212` | #20 | feat(xai): Imagine public-URL storage, chaining & video edit/extend |
| `d4c14011ebbc` | #21 | feat(claude-design): add surface-first conditioning + slop diagnostic (#55399) |
