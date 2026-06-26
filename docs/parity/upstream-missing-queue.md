# Upstream Missing Patch Queue

Generated: `2026-06-26T17:21:43.590412+00:00`

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
| pending | 129 |
| ported | 473 |
| superseded | 6438 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `cfb55de5ea49` | #21 | Update Stripe Projects skill docs (#48673) |
| `9362ce2575e0` | #22 | feat(skills): add html-artifact skill, fold in sketch + architecture-diagram + concept-diagrams (#48899) |
| `92451151c642` | #22 | Revert "feat(skills): add html-artifact skill, fold in sketch + architecture-diagram + concept-diagrams (#48899)" |
| `db744e7d1e58` | #21 | feat(simplify-code): add risk-tiered application, Chesterton's Fence, slop + silent failure detection |
| `f06508836dd4` | #26 | docs(security): enumerate cron job scripts in §2.3 credential scoping |
| `2bd1977d8fad` | #26 | chore: release v0.17.0 (2026.6.19) |
| `866f1d65c4aa` | #26 | chore(desktop): sync package.json version fallback to 0.17.0 (#49236) |
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
| `8666fd7635ba` | #26 | fix(desktop): preserve other providers' hide-all in model visibility dialog |
| `461fcc096479` | #26 | test(desktop): harden model-visibility toggle + dedupe default expansion |
| `fb3d31ba8b77` | #26 | feat(desktop): add Update now button to About panel |
| `65a477f12e35` | #26 | feat(desktop): add Update now button to About panel (#50186) |
| `6bbacc223899` | #26 | fix(desktop): make cold-start port-announcement deadline tolerant |
| `f72690825e76` | #26 | fix(desktop/windows): stop in-app update from cascading into a backend restart loop (#50381) |
| `745c4db235bd` | #26 | feat(desktop/windows): show update-in-progress feedback before the desktop exits (#50419) (#50448) |
| `7785655b4ece` | #26 | fix(desktop): keep the floating composer in-bounds so it can't be lost off-screen |
| `37c37c9dc511` | #26 | fix(antigravity): register google-antigravity ProviderProfile + AUTHOR_MAP |
| `16aeba17078d` | #26 | fix(desktop): clamp composer peel-off under cursor |
| `bef1d3e4ff6a` | #26 | fix(desktop): filter undefined entries in AttachmentList to prevent refText crash on session switch (#49624) |
| `13ce8119067e` | #26 | fix: show desktop approval fallback (#46548) |
| `e5e25836350a` | #26 | fix(desktop): relaunch on Linux after in-app update instead of hanging (#45205) |
| `84e1d31e5442` | #22 | refactor(kanban): fold worker/orchestrator skills into injected guidance (#50473) |
| `0a7ae28ebc1a` | #26 | fix(compressor): remove logging.basicConfig from library class __init__ |
| `7130d60861a9` | #22 | feat(providers): remove google-gemini-cli + google-antigravity OAuth providers (#50492) |
| `0768ed3b33e4` | #26 | docs(agents): fix stale platform adapter path in token-lock note |
| `b9b4756ab480` | #22 | fix dashboard chat session titles |
| `a61baa961572` | #26 | feat(desktop): PR-style file diffs in chat |
| `c6fbd5a10494` | #26 | style(desktop): lead --dt-font-mono with bundled JetBrains Mono |
| `ac128af1cec3` | #26 | feat(desktop): syntax-highlight inline diffs via Shiki |
| `61c266b0dc75` | #26 | style(desktop): soften dark-mode syntax highlighting |
| `d4fa2db1c5df` | #26 | fix(desktop): show all of a provider's models when searching the composer picker |
| `17dfc6bec4a8` | #26 | fix(desktop): set AppUserModelID on Windows so notifications fire (#50808) |
| `79f270f54962` | #26 | fix(desktop): portal floating composer to body so it can't be clipped off-screen |
| `aff5ae692fb2` | #26 | fix(desktop): move composer out of contain wrapper instead of portaling |
| `ea5fa505d974` | #26 | fix(desktop): clamp floating composer to the thread area, not the whole window |
| `de7ad8b78eae` | #26 | fix(desktop): guarantee out-of-bounds composer is reclamped on load |
| `ff08e60c63ad` | #21 | feat(skills): add cloudflare-temporary-deploy optional skill (#50849) |
| `a6b670d4a251` | #26 | fix(desktop): avoid stack overflow on embedded image replay |
| `3fffecbdafec` | #26 | feat(desktop): add timeline rail for long chat threads |
| `cb17a9efb2df` | #26 | fix(desktop): stop auto-opening tool previews |
| `d0af7fc954fe` | #26 | feat(desktop): detect tool previews into composer status stack |
| `48a8f8416937` | #26 | fix(desktop): toggle preview rail and open in browser |
| `7daa6d83fcaa` | #26 | style(desktop): soften inline code and expanded tool chrome |
| `45540cfb5ef1` | #25 | ci: run only the lanes a PR affects (python/frontend/site) |
| `2977e7454377` | #25 | ci: build Docker on main + release only, never on PRs |
| `56b4ef74a631` | #25 | ci: make dependency installs resilient to transient flakes |
| `05c896cf5249` | #25 | ci: refactor paths & clones |
| `a0471e24648e` | #25 | fix(ci): only run supplychain checks in pr |
| `9fd2b2cb9fab` | #26 | fix(desktop): replace native title tooltips with styled Tip component |
| `97888fed483c` | #25 | fix(install): drop system-browser fallback + auto-repair stale snap override |
| `935f2bc48daa` | #26 | docs(relay): add §3.4 — obligations on a future scale-to-zero behaviour layer (#51633) |
| `281a439ad483` | #26 | fix(desktop): guard composer mutations when the composer core isn't bound (#51728) |
| `a911bcda18cf` | #22 | docs: stop recommending pip install; curl installer is the only supported path (#51743) |
| `8446c1570683` | #26 | docs(chronos): pin hop-1 auth to the hosted-agent bootstrap token |
| `66a0907c9566` | #26 | fix(desktop): keep configured onboarding state on fallback runtime probes |
| `7243111c57bb` | #26 | test(desktop): cover fallback timeout onboarding downgrade regression |
| `d398076c2117` | #26 | fix(desktop): show non-blocking notification on fallback runtime probe |
| `a4a74ca9e9a0` | #26 | fix(desktop): use notify() with stable id for fallback notification |
| `6da615c77cf8` | #26 | fix(desktop): scope onboarding runtime check to connected provider |
| `d8fe1c0b4195` | #20 | test(desktop): cover scoped onboarding runtime readiness checks |
| `2de7549fe0fe` | #26 | feat(desktop): remember window size/position/maximized across launches (salvage #39154) |
| `aab49f6927cc` | #20 | feat(pets): generation RPCs, non-blocking gallery + gateway plumbing |
| `743985bf1ec4` | #26 | feat(pets): Pokédex generate UI — overlay, animated egg, hatch FX, manage |
| `b674f7ba28c4` | #26 | feat(pets): offer backend setup when generation is unavailable |
| `a268dfff0a05` | #26 | fix(desktop): make Agents indicator match the Spawn-tree panel |
| `8d1706ae5cb2` | #26 | fix(desktop): wire Ctrl+B voice, declutter voice settings, stop endless TTS hang |
| `2a75c4a8cb4a` | #26 | fix(desktop): give the gateway reconnect loop an escape hatch |
| `93192059c96c` | #26 | fix(desktop): let the session watchdog heal a stuck "looping" turn |
| `1fe013ee16f1` | #20 | feat(pets): polish generate flow and reduce hatch CPU pressure |
| `a6485bddb855` | #26 | fix(desktop): don't report a bogus update count for a shallow checkout |
| `cb6edbf448e7` | #26 | fix(desktop): skip the rev-list count when it is discarded anyway |
| `65b13e9dbc93` | #26 | fix(desktop): route gateway restart / status / update to the active profile |
| `00779800f650` | #26 | fix(desktop): hide platform/internal toolsets from the Skills & Tools list |
| `9a4600c5fb9b` | #26 | fix(desktop): stop the update overlay looking frozen while it works |
| `2ea94c6c4581` | #26 | fix(pets): make inline generate cancel discard draft flow |
| `284be6cc247c` | #26 | Merge pull request #52210 from helix4u/fix/desktop-update-progress-visibility |
| `7e2db0a140db` | #26 | fix(desktop): stop refText crash on undefined composer attachment holes |
| `4aeaba692251` | #26 | test(desktop): cover undefined/null attachment holes in ref helpers |
| `cbe5c5689f9c` | #26 | perf(desktop): bound tool-result rendering so big /learn runs don't freeze (#52273) |
| `f2c45e2c816d` | #26 | fix(desktop): limit pending tool shimmer to action verb |
| `281b333cc5f0` | #26 | test(desktop): cover localized tool title shimmer |
| `e92b5c6af8be` | #20 | feat(pets): quality-first OpenRouter model chain + stronger atlas gates + global pet-gen notifications |
| `7078d9d1e29d` | #26 | fix(pets): raise generation timeouts for the slow quality-first model path |
