# Upstream Missing Patch Queue

Generated: `2026-06-26T21:33:31.127926+00:00`

- Range: `main..upstream/main`; total commits tracked: `7173`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 3234 |
| #21 | GPAR-02 skills parity | 130 |
| #22 | GPAR-03 UX parity | 968 |
| #23 | GPAR-04 gateway/plugin-memory parity | 489 |
| #24 | GPAR-05 environments+parsers+benchmarks | 23 |
| #25 | GPAR-06 packaging/docs/install parity | 146 |
| #26 | GPAR-07 upstream queue backfill | 2183 |

| Disposition | Commit Count |
| --- | ---: |
| mirrored | 76 |
| pending | 31 |
| ported | 490 |
| superseded | 6576 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `2609bcccca30` | #25 | feat(i18n): add complete Spanish translation |
| `defeda8c559f` | #22 | docs: sync documentation with current implementation |
| `7130d60861a9` | #22 | feat(providers): remove google-gemini-cli + google-antigravity OAuth providers (#50492) |
| `a911bcda18cf` | #22 | docs: stop recommending pip install; curl installer is the only supported path (#51743) |
| `6da615c77cf8` | #26 | fix(desktop): scope onboarding runtime check to connected provider |
| `d8fe1c0b4195` | #20 | test(desktop): cover scoped onboarding runtime readiness checks |
| `aab49f6927cc` | #20 | feat(pets): generation RPCs, non-blocking gallery + gateway plumbing |
| `b674f7ba28c4` | #26 | feat(pets): offer backend setup when generation is unavailable |
| `1fe013ee16f1` | #20 | feat(pets): polish generate flow and reduce hatch CPU pressure |
| `e92b5c6af8be` | #20 | feat(pets): quality-first OpenRouter model chain + stronger atlas gates + global pet-gen notifications |
| `7078d9d1e29d` | #26 | fix(pets): raise generation timeouts for the slow quality-first model path |
| `f168631be0ce` | #20 | fix(agent): gate verify-on-stop nudge off for messaging surfaces |
| `b8d220f2684c` | #26 | feat(desktop): wire project settings and shell chrome |
| `890e890281e4` | #26 | chore(desktop): update package lock |
| `e7d2f0b93ca2` | #24 | fix(windows): suppress console flashes and harden gateway restarts |
| `ff813659880f` | #26 | feat(desktop): in-app spot editor for the file preview pane |
| `1c8594b634e4` | #20 | fix(desktop): show remote backend updates without counts |
| `594380d44a45` | #20 | fix(tui): make stop interrupt queued desktop turns |
| `dd980aaba1d1` | #26 | feat(desktop): Alt+wheel to scale the pet, never cropped |
| `7d1b72a15d34` | #26 | feat(desktop): zoom the pet toward the cursor |
| `bf60bbb6c59b` | #26 | refactor(desktop): collapse overlay zoom-anchor math |
| `62fe9fd1011a` | #22 | style(desktop,tui): fix all lint/type/formatting issues |
| `3cf900eb67c0` | #20 | fix(install): discard managed lockfile churn before stashing |
| `2e322466b14e` | #20 | feat(dashboard-auth): drain shared-bearer-secret provider plugin |
| `81ac562bf0e5` | #26 | feat(desktop): inline embed detection + module primitives |
| `0c190083cd9a` | #26 | feat(desktop): lazy embed renderers + fenced diagrams/alerts |
| `e36d9862ece4` | #26 | feat(desktop): render embeds, fences and alerts in assistant markdown |
| `da0ed979facd` | #26 | feat(desktop): zoomable primitive — open full, pan/zoom, copy |
| `8559246bfb04` | #26 | feat(desktop): rebuild the clarify prompt to match the chat UI |
| `54b50037e1e4` | #26 | fix(desktop): treat a pending prompt as paused-on-you, not working |
| `db6ced47128d` | #26 | feat(desktop): consent gate for inline embeds (per-embed / per-service) |
