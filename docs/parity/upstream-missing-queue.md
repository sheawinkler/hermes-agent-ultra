# Upstream Missing Patch Queue

Generated: `2026-06-25T02:46:52.230351+00:00`

- Range: `main..upstream/main`; total commits tracked: `6978`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 3118 |
| #21 | GPAR-02 skills parity | 130 |
| #22 | GPAR-03 UX parity | 957 |
| #23 | GPAR-04 gateway/plugin-memory parity | 488 |
| #24 | GPAR-05 environments+parsers+benchmarks | 22 |
| #25 | GPAR-06 packaging/docs/install parity | 145 |
| #26 | GPAR-07 upstream queue backfill | 2118 |

| Disposition | Commit Count |
| --- | ---: |
| mirrored | 76 |
| pending | 239 |
| ported | 356 |
| superseded | 6307 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `73cd8622f9fc` | #22 | feat(billing): /billing terminal billing — interactive TUI + CLI client (#45449) |
| `36851fa576eb` | #20 | fix(docker): support WebUI installs from read-only sources (#48541) |
| `cfb55de5ea49` | #21 | Update Stripe Projects skill docs (#48673) |
| `c7b7f92ec14a` | #20 | fix(openviking): sync structured turns with tool parts |
| `d7cd0bc0863c` | #20 | fix(openviking): preserve structured sync attribution |
| `9362ce2575e0` | #22 | feat(skills): add html-artifact skill, fold in sketch + architecture-diagram + concept-diagrams (#48899) |
| `fcac0f94d484` | #23 | fix(openviking): guard empty tool_id in batch skip set; reuse env_var_enabled |
| `27a6e188c4b4` | #23 | refactor(openviking): derive recall-tool name set from canonical schemas |
| `2d4046c6de97` | #23 | refactor(openviking): reuse pre-scanned tool_input for pending tool calls |
| `be2c2beb96e5` | #23 | refactor(openviking): name tool_status constants and alias sets |
| `f9ffe0bc3f61` | #26 | fix(desktop): resume stored session id on notification click |
| `069011dd0c8f` | #26 | test(desktop): cover runtime->stored notification id resolution |
| `bce1e36b5769` | #20 | fix(discord): unwrap dict choices + soft-boundary truncate clarify buttons |
| `92451151c642` | #22 | Revert "feat(skills): add html-artifact skill, fold in sketch + architecture-diagram + concept-diagrams (#48899)" |
| `9a2f2756f7e6` | #26 | fix(desktop): allow selecting slash output and shell logs in thread (#49063) |
| `fad4b40d9d38` | #20 | fix(model): persist /model switch by default across sessions |
| `6cb04be779de` | #26 | feat(desktop): Keys tab groups by backend provider identity |
| `ee0de638d719` | #26 | feat(desktop): add API-keys search; keep provider lists priority-sorted |
| `d91b8d8368bb` | #26 | test(desktop): make keyVar a typed EnvVarInfo factory |
| `b936f92b25b4` | #26 | fix(desktop): render send/prefill directive notices (/goal, /undo) (#49073) |
| `caaa916289f2` | #20 | fix(gateway): don't let delayed Discord status messages partition history backfill |
| `db744e7d1e58` | #21 | feat(simplify-code): add risk-tiered application, Chesterton's Fence, slop + silent failure detection |
| `6c44471bfdb8` | #23 | fix(hindsight): lazy-install cloud client dependency |
| `13d4b5fe2f45` | #23 | fix(hindsight): align client version to 0.6.1 across all sources |
| `b0e47a98f9ed` | #20 | fix(managed-scope): honor managed scope in all standalone config loaders |
| `9026a8c78974` | #22 | feat(gateway): add Raft bundled platform plugin with activity hooks |
| `7d86178cf51a` | #26 | fix(raft): set stdin=DEVNULL on bridge subprocess |
| `6308d3416ab9` | #26 | fix(desktop): rename "Restart messaging" -> "Restart gateway" |
| `553cf4f97757` | #26 | feat(desktop): restart the gateway from Cmd+K, with statusbar spinner feedback |
| `a1639921ac44` | #26 | fix(desktop): offer a Restart gateway action on messaging save/toggle toasts |
| `929dbf777801` | #26 | fix(desktop): make rendered logs selectable so they can be copied |
| `b6e2a54a94f5` | #20 | fix(mcp): address adversarial review round 1 (cache parity, gates, races) |
| `f06508836dd4` | #26 | docs(security): enumerate cron job scripts in §2.3 credential scoping |
| `ba49fb51a585` | #20 | fix(discord): hydrate channel context when replying to a message (#49212) |
| `40722058e532` | #20 | fix(mcp): keep short-TTL HTTP sessions alive with configurable ping keepalive |
| `2bd1977d8fad` | #26 | chore: release v0.17.0 (2026.6.19) |
| `866f1d65c4aa` | #26 | chore(desktop): sync package.json version fallback to 0.17.0 (#49236) |
| `7a7b56d49830` | #23 | fix(windows): prefer managed node for whatsapp and desktop |
| `d4e7dd609da6` | #23 | refactor(windows): tidy managed-node resolver helpers |
| `a7983d5ad768` | #20 | fix(dashboard): hide sidecar sessions from history (#49269) |
| `d799284b1554` | #21 | feat(optional-skills/creative-ideation): expand to v2.1.0 method library (#42402) |
| `5f55f0ff85f0` | #20 | feat(teams): native send_video/send_voice/send_document attachments (#49308) |
| `8ebe37f6ad2d` | #26 | feat(desktop): notify renderer when GPU acceleration is disabled due to remote display |
| `8cf7df867e7d` | #20 | fix(plugins): silence raft check_fn log spam for users without raft CLI |
| `1b7b4d138a67` | #26 | fix(desktop): handle slash exec dispatch payloads (#49358) |
| `236f0597e562` | #26 | feat(desktop): pop the composer out into a draggable floating window |
| `f697c97e02f0` | #26 | fix(desktop): keep floating composer radius consistent with docked |
| `eed78d6ebb51` | #26 | fix(desktop): composer popout polish — peel-off placement, panels, chip editing |
| `ae8db1ab531b` | #26 | fix(desktop): mute hidden link-title window so historical links don't autoplay audio |
| `7eb9678c5470` | #26 | test(desktop): cover link-title window audio muting |
| `a7dd98c8609c` | #23 | fix(env): guard remaining malformed int/float env var casts with utils helpers |
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
| `29e5e127c6f1` | #20 | fix(telegram): recover reply text from native rich echo |
| `c1f11f8c69f9` | #20 | fix(telegram): index streamed rich finals via editMessageText too |
| `8666fd7635ba` | #26 | fix(desktop): preserve other providers' hide-all in model visibility dialog |
| `461fcc096479` | #26 | test(desktop): harden model-visibility toggle + dedupe default expansion |
| `04730f32e7e8` | #20 | fix(cli): warn when in-session model switch will preflight-compress |
| `1ca29723f0ea` | #20 | fix(cli): log instead of swallow preflight-warning errors; consistent TUI warning field |
| `dd042fc4dfb1` | #20 | fix(tools): preserve core tools when a platform bundle is disabled |
| `796f618f9987` | #20 | fix(telegram): keep chunk markers outside code fences |
| `c7e8854cb383` | #20 | fix(tui): persist session messages on force-quit / signal shutdown |
| `fb3d31ba8b77` | #26 | feat(desktop): add Update now button to About panel |
| `0e47f68a479a` | #26 | fix(desktop): rename branched session via session.title RPC |
| `7f43378931f3` | #26 | test(desktop): cover renameSessionPreferringRpc routing |
| `ed81f0b633c7` | #26 | fix(desktop): log session.title RPC failure before REST fallback |
| `31e59fe44d18` | #20 | fix(telegram): preserve newlines in rich slash-command output (#46070) |
| `a9669323922f` | #20 | fix(telegram): exempt tables from rich newline hard-breaks |
| `65a477f12e35` | #26 | feat(desktop): add Update now button to About panel (#50186) |
| `ea056b05598c` | #20 | fix(telegram): avoid rich messages for CJK text |
| `d0de4601d204` | #20 | fix(tui): /compress shows a before/after summary (#46686) |
| `7bc6f1806284` | #23 | fix(hindsight): skip local_embedded daemon when running as root |
| `93ea9b04aff2` | #20 | fix(gateway): cap inbound media download size to prevent memory exhaustion |
| `6183e8ce1b5e` | #20 | fix(telegram): make Bot API 10.1 rich messages opt-in (default off) |
| `587b5b9ac223` | #23 | fix(backup): capture memory-provider state stored outside HERMES_HOME (#50325) |
| `2a4542333ee1` | #26 | fix(photon): classify Envoy overflow errors as retryable; add typing cooldown |
