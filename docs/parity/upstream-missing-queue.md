# Upstream Missing Patch Queue

Generated: `2026-06-25T18:58:02.554663+00:00`

- Range: `origin/main..upstream/main`; total commits tracked: `7021`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 3146 |
| #21 | GPAR-02 skills parity | 130 |
| #22 | GPAR-03 UX parity | 957 |
| #23 | GPAR-04 gateway/plugin-memory parity | 488 |
| #24 | GPAR-05 environments+parsers+benchmarks | 22 |
| #25 | GPAR-06 packaging/docs/install parity | 146 |
| #26 | GPAR-07 upstream queue backfill | 2132 |

| Disposition | Commit Count |
| --- | ---: |
| mirrored | 76 |
| pending | 171 |
| ported | 421 |
| superseded | 6353 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `cfb55de5ea49` | #21 | Update Stripe Projects skill docs (#48673) |
| `9362ce2575e0` | #22 | feat(skills): add html-artifact skill, fold in sketch + architecture-diagram + concept-diagrams (#48899) |
| `f9ffe0bc3f61` | #26 | fix(desktop): resume stored session id on notification click |
| `069011dd0c8f` | #26 | test(desktop): cover runtime->stored notification id resolution |
| `92451151c642` | #22 | Revert "feat(skills): add html-artifact skill, fold in sketch + architecture-diagram + concept-diagrams (#48899)" |
| `9a2f2756f7e6` | #26 | fix(desktop): allow selecting slash output and shell logs in thread (#49063) |
| `6cb04be779de` | #26 | feat(desktop): Keys tab groups by backend provider identity |
| `ee0de638d719` | #26 | feat(desktop): add API-keys search; keep provider lists priority-sorted |
| `d91b8d8368bb` | #26 | test(desktop): make keyVar a typed EnvVarInfo factory |
| `b936f92b25b4` | #26 | fix(desktop): render send/prefill directive notices (/goal, /undo) (#49073) |
| `db744e7d1e58` | #21 | feat(simplify-code): add risk-tiered application, Chesterton's Fence, slop + silent failure detection |
| `9026a8c78974` | #22 | feat(gateway): add Raft bundled platform plugin with activity hooks |
| `7d86178cf51a` | #26 | fix(raft): set stdin=DEVNULL on bridge subprocess |
| `6308d3416ab9` | #26 | fix(desktop): rename "Restart messaging" -> "Restart gateway" |
| `553cf4f97757` | #26 | feat(desktop): restart the gateway from Cmd+K, with statusbar spinner feedback |
| `a1639921ac44` | #26 | fix(desktop): offer a Restart gateway action on messaging save/toggle toasts |
| `929dbf777801` | #26 | fix(desktop): make rendered logs selectable so they can be copied |
| `f06508836dd4` | #26 | docs(security): enumerate cron job scripts in §2.3 credential scoping |
| `2bd1977d8fad` | #26 | chore: release v0.17.0 (2026.6.19) |
| `866f1d65c4aa` | #26 | chore(desktop): sync package.json version fallback to 0.17.0 (#49236) |
| `7a7b56d49830` | #23 | fix(windows): prefer managed node for whatsapp and desktop |
| `d4e7dd609da6` | #23 | refactor(windows): tidy managed-node resolver helpers |
| `a7983d5ad768` | #20 | fix(dashboard): hide sidecar sessions from history (#49269) |
| `d799284b1554` | #21 | feat(optional-skills/creative-ideation): expand to v2.1.0 method library (#42402) |
| `8ebe37f6ad2d` | #26 | feat(desktop): notify renderer when GPU acceleration is disabled due to remote display |
| `1b7b4d138a67` | #26 | fix(desktop): handle slash exec dispatch payloads (#49358) |
| `236f0597e562` | #26 | feat(desktop): pop the composer out into a draggable floating window |
| `f697c97e02f0` | #26 | fix(desktop): keep floating composer radius consistent with docked |
| `eed78d6ebb51` | #26 | fix(desktop): composer popout polish — peel-off placement, panels, chip editing |
| `ae8db1ab531b` | #26 | fix(desktop): mute hidden link-title window so historical links don't autoplay audio |
| `7eb9678c5470` | #26 | test(desktop): cover link-title window audio muting |
| `5600105478ff` | #20 | refactor(gateway): migrate slack/dingtalk/whatsapp/matrix/feishu/telegram/wecom/email/sms adapters to bundled plugins |
| `404fe730b7a2` | #26 | fix: add tooltips to right sidebar header buttons |
| `838daca9f4cf` | #26 | chore(desktop): format tooltip indentation + author map for #49697 |
| `75b36a138f43` | #22 | feat(pets): TUI pet pane, picker + gateway RPCs |
| `86b990fe0fac` | #26 | feat(desktop): floating pet, pop-out overlay + Cmd+K picker |
| `6fd839ac84d0` | #22 | docs(pets): feature guide, petdex skill + catalog |
| `491579fa05ef` | #23 | fix(whatsapp): resolve bridge dir with HERMES_HOME mirror in Docker |
| `37fa3c58b40e` | #21 | docs(kanban-worker): document kanban_complete artifacts deliverable param (#49854) |
| `31bdb60013c9` | #22 | docs(skills): fix himalaya CLI arg order and download flag |
| `2b08a4295a65` | #26 | docs(README.zh-CN): update Windows install from 'not supported' to native PowerShell |
| `9e4348f28ac1` | #25 | docs(windows): document uv.exe AV false positive |
| `f6275a59e790` | #26 | docs(contributing): add "search first" guidance to cut duplicate PRs |
| `4c206b972d49` | #26 | fix(gateway): correct sys.path insertion in plugins to prevent cron namespace collision (#49410) |
| `79f297834a9b` | #26 | fix(gateway): widen cron namespace-collision fix to all migrated adapters |
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
| `0e47f68a479a` | #26 | fix(desktop): rename branched session via session.title RPC |
| `7f43378931f3` | #26 | test(desktop): cover renameSessionPreferringRpc routing |
| `ed81f0b633c7` | #26 | fix(desktop): log session.title RPC failure before REST fallback |
| `65a477f12e35` | #26 | feat(desktop): add Update now button to About panel (#50186) |
| `2a4542333ee1` | #26 | fix(photon): classify Envoy overflow errors as retryable; add typing cooldown |
| `9578e52795e3` | #26 | fix(photon): detect unexpected sidecar death and trigger reconnect |
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
| `64a507da44d2` | #20 | feat(relay): handle passthrough_forward over the WS (Phase 5 §5.1, gateway half) (#50702) |
| `61c266b0dc75` | #26 | style(desktop): soften dark-mode syntax highlighting |
| `8845f3316c26` | #20 | fix(security): restrict dashboard plugin backend import to bundled plugins (#43719) |
| `d4fa2db1c5df` | #26 | fix(desktop): show all of a provider's models when searching the composer picker |
| `17dfc6bec4a8` | #26 | fix(desktop): set AppUserModelID on Windows so notifications fire (#50808) |
| `f2e37549c673` | #20 | feat(computer_use): cross-platform cua-driver (macOS/Windows/Linux) |
| `e3505c7f73a4` | #26 | fix(computer_use): reconcile Linux gate with stale "gated off" comments |
| `79f270f54962` | #26 | fix(desktop): portal floating composer to body so it can't be clipped off-screen |
| `aff5ae692fb2` | #26 | fix(desktop): move composer out of contain wrapper instead of portaling |
| `ea5fa505d974` | #26 | fix(desktop): clamp floating composer to the thread area, not the whole window |
| `de7ad8b78eae` | #26 | fix(desktop): guarantee out-of-bounds composer is reclamped on load |
| `ff08e60c63ad` | #21 | feat(skills): add cloudflare-temporary-deploy optional skill (#50849) |
| `0223ea5f590a` | #26 | feat(computer-use): surface macOS permission preflight in the desktop |
| `2dfcead68367` | #26 | feat(computer-use): make the preflight cross-platform (win/linux) |
| `a6b670d4a251` | #26 | fix(desktop): avoid stack overflow on embedded image replay |
| `3fffecbdafec` | #26 | feat(desktop): add timeline rail for long chat threads |
| `ba9e3a491bfa` | #23 | feat(memory): Honcho OAuth connect — desktop and CLI flows + token refresh (#44335) |
