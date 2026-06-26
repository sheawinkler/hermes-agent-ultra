# Upstream Missing Patch Queue

Generated: `2026-06-26T18:52:58.048662+00:00`

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
| pending | 53 |
| ported | 475 |
| superseded | 6512 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `cfb55de5ea49` | #21 | Update Stripe Projects skill docs (#48673) |
| `9362ce2575e0` | #22 | feat(skills): add html-artifact skill, fold in sketch + architecture-diagram + concept-diagrams (#48899) |
| `92451151c642` | #22 | Revert "feat(skills): add html-artifact skill, fold in sketch + architecture-diagram + concept-diagrams (#48899)" |
| `db744e7d1e58` | #21 | feat(simplify-code): add risk-tiered application, Chesterton's Fence, slop + silent failure detection |
| `f06508836dd4` | #26 | docs(security): enumerate cron job scripts in §2.3 credential scoping |
| `2bd1977d8fad` | #26 | chore: release v0.17.0 (2026.6.19) |
| `d799284b1554` | #21 | feat(optional-skills/creative-ideation): expand to v2.1.0 method library (#42402) |
| `37fa3c58b40e` | #21 | docs(kanban-worker): document kanban_complete artifacts deliverable param (#49854) |
| `31bdb60013c9` | #22 | docs(skills): fix himalaya CLI arg order and download flag |
| `2b08a4295a65` | #26 | docs(README.zh-CN): update Windows install from 'not supported' to native PowerShell |
| `9e4348f28ac1` | #25 | docs(windows): document uv.exe AV false positive |
| `f6275a59e790` | #26 | docs(contributing): add "search first" guidance to cut duplicate PRs |
| `46cc0345ae8a` | #21 | docs(skills): add hermes-agent verification rule |
| `5eb158e3173d` | #21 | docs(hermes-agent skill): document project context files and their discovery rules |
| `2609bcccca30` | #25 | feat(i18n): add complete Spanish translation |
| `df4015bbc176` | #26 | docs: session lifecycle documentation |
| `eec9c1d84ebd` | #26 | docs(agents): clarify background delegation durability |
| `f80088f035de` | #26 | docs: add missing Prerequisites/How to Run sections to SKILL.md template |
| `242962e1f5a0` | #22 | docs(providers): clarify vllm qwen reasoning output |
| `95d970a7521c` | #21 | docs: sharpen software-development skills |
| `defeda8c559f` | #22 | docs: sync documentation with current implementation |
| `98ecd0beeba9` | #26 | docs(mcp): fix stale ~0.75s discovery-wait reference in late-refresh docstring |
| `b1ab5a8ae1d9` | #21 | docs(antigravity-cli): add delegation patterns + output/bounding caveats |
| `72e4cca00ecc` | #26 | docs(config): correct MCP docs path in cli-config.yaml.example |
| `37c37c9dc511` | #26 | fix(antigravity): register google-antigravity ProviderProfile + AUTHOR_MAP |
| `84e1d31e5442` | #22 | refactor(kanban): fold worker/orchestrator skills into injected guidance (#50473) |
| `0a7ae28ebc1a` | #26 | fix(compressor): remove logging.basicConfig from library class __init__ |
| `7130d60861a9` | #22 | feat(providers): remove google-gemini-cli + google-antigravity OAuth providers (#50492) |
| `0768ed3b33e4` | #26 | docs(agents): fix stale platform adapter path in token-lock note |
| `b9b4756ab480` | #22 | fix dashboard chat session titles |
| `ff08e60c63ad` | #21 | feat(skills): add cloudflare-temporary-deploy optional skill (#50849) |
| `45540cfb5ef1` | #25 | ci: run only the lanes a PR affects (python/frontend/site) |
| `2977e7454377` | #25 | ci: build Docker on main + release only, never on PRs |
| `56b4ef74a631` | #25 | ci: make dependency installs resilient to transient flakes |
| `05c896cf5249` | #25 | ci: refactor paths & clones |
| `a0471e24648e` | #25 | fix(ci): only run supplychain checks in pr |
| `97888fed483c` | #25 | fix(install): drop system-browser fallback + auto-repair stale snap override |
| `935f2bc48daa` | #26 | docs(relay): add §3.4 — obligations on a future scale-to-zero behaviour layer (#51633) |
| `a911bcda18cf` | #22 | docs: stop recommending pip install; curl installer is the only supported path (#51743) |
| `8446c1570683` | #26 | docs(chronos): pin hop-1 auth to the hosted-agent bootstrap token |
| `6da615c77cf8` | #26 | fix(desktop): scope onboarding runtime check to connected provider |
| `d8fe1c0b4195` | #20 | test(desktop): cover scoped onboarding runtime readiness checks |
| `aab49f6927cc` | #20 | feat(pets): generation RPCs, non-blocking gallery + gateway plumbing |
| `b674f7ba28c4` | #26 | feat(pets): offer backend setup when generation is unavailable |
| `1fe013ee16f1` | #20 | feat(pets): polish generate flow and reduce hatch CPU pressure |
| `e92b5c6af8be` | #20 | feat(pets): quality-first OpenRouter model chain + stronger atlas gates + global pet-gen notifications |
| `7078d9d1e29d` | #26 | fix(pets): raise generation timeouts for the slow quality-first model path |
| `a6a28ce3e217` | #25 | fix(ci): run CI on all PRs to anywhere |
| `b8d220f2684c` | #26 | feat(desktop): wire project settings and shell chrome |
| `890e890281e4` | #26 | chore(desktop): update package lock |
| `e7d2f0b93ca2` | #24 | fix(windows): suppress console flashes and harden gateway restarts |
| `ff813659880f` | #26 | feat(desktop): in-app spot editor for the file preview pane |
| `233ef98afe2f` | #20 | fix(docker): skip symlinked stage2 chown targets (#52789) |
