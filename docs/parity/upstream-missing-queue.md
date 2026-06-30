# Upstream Missing Patch Queue

Generated: `2026-06-30T03:26:00.450945+00:00`

- Range: `origin/main..upstream/main`; total commits tracked: `7656`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 3494 |
| #21 | GPAR-02 skills parity | 131 |
| #22 | GPAR-03 UX parity | 1003 |
| #23 | GPAR-04 gateway/plugin-memory parity | 503 |
| #24 | GPAR-05 environments+parsers+benchmarks | 25 |
| #25 | GPAR-06 packaging/docs/install parity | 165 |
| #26 | GPAR-07 upstream queue backfill | 2335 |

| Disposition | Commit Count |
| --- | ---: |
| mirrored | 97 |
| pending | 35 |
| ported | 527 |
| superseded | 6997 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `5636c22828b0` | #20 | feat(photon): upgrade spectrum-ts sidecar to v7.0.0 |
| `4345b3e767c7` | #20 | fix(photon): upgrade spectrum-ts sidecar to v8.0.0 |
| `882730026739` | #20 | fix(photon): correlate tapbacks to bot message context |
| `50f685521734` | #20 | feat(moa): make /moa one-shot only; route preset switching through the model picker |
| `f67c0b3e60ba` | #21 | docs(hermes-agent skill): cover v0.13–v0.17 features, fix stale claims, tighten (#53566) |
| `cd592c105cbb` | #20 | feat(send_message): native WhatsApp media delivery via Baileys bridge (#53598) |
| `917f6bdb00b8` | #20 | fix(tools): let vision pick any provider+model, not just OpenRouter (#53606) |
| `ef17cd204d75` | #20 | fix(windows): stop subprocess console-window popups + add CI guard (#53791) |
| `5db1430af9ec` | #20 | fix(windows): stop terminal-window popups from background spawns (#53810) |
| `2ecca1e7d3e7` | #23 | fix(windows): capture is not a no-window boundary; route flashing spawns through chokepoint (#53829) |
| `d3d621f7c38b` | #20 | revert(windows): roll back terminal-popup PRs #53791 #53810 #53829 (#53853) |
| `163cb24d45d8` | #22 | feat(moa): render reference-model blocks in TUI and desktop, not just CLI (#53855) |
| `a94f657a5059` | #20 | fix(tui): route completion RPCs to the pool so they can't freeze the TUI (#53895) |
| `d43e0cf304a1` | #20 | fix(agent): config-driven intent-ack continuation for all api_modes (#27881) (#53943) |
| `a8c862900b9c` | #20 | fix(tui): sanitize replay history on WebUI/TUI session resume (#29086) (#53939) |
| `fde1c8570ffe` | #20 | fix(tui_gateway): suppress WS peer-hangup teardown error flood (#50005) (#54126) |
| `6d879d486b19` | #20 | fix(dashboard): close PTY WebSocket on child EOF to stop FD leak (#54028) (#54123) |
| `5c2c85c5452f` | #20 | fix(tui): start MCP discovery for websocket sessions |
| `cb982ad997c5` | #20 | fix(windows): hide console-window flash on backend git/gh/wmic/bash subprocess spawns |
| `1ffa01f35fb8` | #20 | test(windows): cover no-window backend subprocess flags |
| `eeca59f48919` | #20 | fix(windows): hide remaining backend console-flash legs missed on main |
| `b31b0b9d95d1` | #22 | docs: reconcile docs with code across last 3 releases (#54254) |
| `9a0010fd469f` | #20 | fix(windows): cover remaining console-flash spawn legs (#54417) |
| `e5d22ab80d97` | #20 | fix(daytona): quote single-upload mkdir parent path (#54440) |
| `ee22d853eb13` | #26 | fix(windows): hide pdftoppm console flash on PDF attach |
| `520212cc593d` | #26 | feat(desktop): stream agent terminal output live instead of polling |
| `e117cfdff08b` | #20 | feat(desktop): live agent terminals + agent-driven tab close |
| `adacb16d6243` | #26 | fix(desktop): make agent terminal tabs fully readable |
| `dff491a2b993` | #22 | feat(cli): add headless `hermes serve` backend; desktop no longer launches `dashboard` |
| `476875acb9f0` | #22 | Add dashboard backup upload and download |
| `fd324562d3ad` | #20 | feat(desktop): add context usage breakdown popover |
| `808ba82125e2` | #25 | feat(ci): add CI timing report |
| `66ba9e06d925` | #25 | change(ci): remove lint PR comment |
| `cca8b4ef4e3f` | #25 | fix(ci): unify amd64/arm64 docker pipelines |
| `41c85fb9469b` | #26 | fix(agents.md): fix documentation on subprocess isolation in tests |
