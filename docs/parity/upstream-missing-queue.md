# Upstream Missing Patch Queue

Generated: `2026-06-26T21:00:26.742687+00:00`

- Range: `main..upstream/main`; total commits tracked: `7116`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 3202 |
| #21 | GPAR-02 skills parity | 130 |
| #22 | GPAR-03 UX parity | 960 |
| #23 | GPAR-04 gateway/plugin-memory parity | 488 |
| #24 | GPAR-05 environments+parsers+benchmarks | 23 |
| #25 | GPAR-06 packaging/docs/install parity | 146 |
| #26 | GPAR-07 upstream queue backfill | 2167 |

| Disposition | Commit Count |
| --- | ---: |
| mirrored | 76 |
| pending | 18 |
| ported | 489 |
| superseded | 6533 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `2609bcccca30` | #25 | feat(i18n): add complete Spanish translation |
| `defeda8c559f` | #22 | docs: sync documentation with current implementation |
| `7130d60861a9` | #22 | feat(providers): remove google-gemini-cli + google-antigravity OAuth providers (#50492) |
| `ff08e60c63ad` | #21 | feat(skills): add cloudflare-temporary-deploy optional skill (#50849) |
| `97888fed483c` | #25 | fix(install): drop system-browser fallback + auto-repair stale snap override |
| `a911bcda18cf` | #22 | docs: stop recommending pip install; curl installer is the only supported path (#51743) |
| `6da615c77cf8` | #26 | fix(desktop): scope onboarding runtime check to connected provider |
| `d8fe1c0b4195` | #20 | test(desktop): cover scoped onboarding runtime readiness checks |
| `aab49f6927cc` | #20 | feat(pets): generation RPCs, non-blocking gallery + gateway plumbing |
| `b674f7ba28c4` | #26 | feat(pets): offer backend setup when generation is unavailable |
| `1fe013ee16f1` | #20 | feat(pets): polish generate flow and reduce hatch CPU pressure |
| `e92b5c6af8be` | #20 | feat(pets): quality-first OpenRouter model chain + stronger atlas gates + global pet-gen notifications |
| `7078d9d1e29d` | #26 | fix(pets): raise generation timeouts for the slow quality-first model path |
| `b8d220f2684c` | #26 | feat(desktop): wire project settings and shell chrome |
| `890e890281e4` | #26 | chore(desktop): update package lock |
| `e7d2f0b93ca2` | #24 | fix(windows): suppress console flashes and harden gateway restarts |
| `ff813659880f` | #26 | feat(desktop): in-app spot editor for the file preview pane |
| `233ef98afe2f` | #20 | fix(docker): skip symlinked stage2 chown targets (#52789) |
