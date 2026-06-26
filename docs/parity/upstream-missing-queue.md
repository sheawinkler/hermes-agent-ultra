# Upstream Missing Patch Queue

Generated: `2026-06-26T20:14:21.352111+00:00`

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
| pending | 28 |
| ported | 484 |
| superseded | 6528 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `9362ce2575e0` | #22 | feat(skills): add html-artifact skill, fold in sketch + architecture-diagram + concept-diagrams (#48899) |
| `92451151c642` | #22 | Revert "feat(skills): add html-artifact skill, fold in sketch + architecture-diagram + concept-diagrams (#48899)" |
| `2bd1977d8fad` | #26 | chore: release v0.17.0 (2026.6.19) |
| `d799284b1554` | #21 | feat(optional-skills/creative-ideation): expand to v2.1.0 method library (#42402) |
| `2609bcccca30` | #25 | feat(i18n): add complete Spanish translation |
| `242962e1f5a0` | #22 | docs(providers): clarify vllm qwen reasoning output |
| `defeda8c559f` | #22 | docs: sync documentation with current implementation |
| `98ecd0beeba9` | #26 | docs(mcp): fix stale ~0.75s discovery-wait reference in late-refresh docstring |
| `b1ab5a8ae1d9` | #21 | docs(antigravity-cli): add delegation patterns + output/bounding caveats |
| `72e4cca00ecc` | #26 | docs(config): correct MCP docs path in cli-config.yaml.example |
| `37c37c9dc511` | #26 | fix(antigravity): register google-antigravity ProviderProfile + AUTHOR_MAP |
| `84e1d31e5442` | #22 | refactor(kanban): fold worker/orchestrator skills into injected guidance (#50473) |
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
