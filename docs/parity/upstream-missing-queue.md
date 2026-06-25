# Upstream Missing Patch Queue

Generated: `2026-06-25T00:35:24.714987+00:00`

- Range: `origin/main..upstream/main`; total commits tracked: `6907`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 3078 |
| #21 | GPAR-02 skills parity | 130 |
| #22 | GPAR-03 UX parity | 956 |
| #23 | GPAR-04 gateway/plugin-memory parity | 486 |
| #24 | GPAR-05 environments+parsers+benchmarks | 22 |
| #25 | GPAR-06 packaging/docs/install parity | 144 |
| #26 | GPAR-07 upstream queue backfill | 2091 |

| Disposition | Commit Count |
| --- | ---: |
| mirrored | 76 |
| pending | 251 |
| ported | 335 |
| superseded | 6245 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `547a014e7eae` | #26 | fix(desktop): avoid stack overflow rendering huge fenced blocks |
| `b82eca2bebd8` | #26 | fix(desktop): isolate message render crashes from the root boundary |
| `435c706e8e5a` | #26 | fix(desktop): stop a failed turn leaking into every other thread |
| `f4100f439430` | #26 | fix(desktop): list markers and quote border follow RTL message direction |
| `0138282f97c9` | #26 | perf(desktop): keep oversized messages from freezing the chat |
| `c6c8abbadb80` | #20 | refactor: remove agent-callable send_message tool (#47856) |
| `c2fa302e933a` | #26 | Merge pull request #47913 from xxxigm/fix/desktop-backend-skew-toast-nag |
| `b07b7894ec55` | #26 | fix(desktop): keep streaming painting in unfocused secondary chat windows (#47919) |
| `33b1d144590a` | #20 | fix(desktop): pin Electron below the broken native extract-zip install (#47792) |
| `5a00bd151896` | #20 | fix(desktop): persist /title set before the first message instead of queuing (#47987) |
| `ee41aa0c1a0a` | #26 | feat(desktop): add dismiss control to chat error banners (#47985) |
| `fd674af47fa6` | #20 | fix(photon): preserve text in mixed iMessage attachments (salvage #46513) (#46818) |
| `016bce1a09ba` | #26 | fix(desktop): recover stranded session windows when resume fails (#47655) |
| `f8098c6b6fe5` | #20 | fix(desktop): resolve electronDist to the actual electron install location (#48081) |
| `6092be413d59` | #20 | Harden hosted Docker install tree against self-modification (#47490) |
| `ab1a42fcea4f` | #26 | docs: relay<->connector cross-repo contract (v1, experimental) |
| `5feec8b4cfcb` | #20 | test(gateway): enforce relay contract-doc ⟷ Python conformance |
| `6e20c1992ff9` | #26 | docs(gateway): rewrite contract §6 to the A2 trust-boundary model |
| `c1f9eb0ec4b9` | #20 | fix(desktop): resolve electronDist dynamically + self-heal blocked installs (supersedes #48081/#48082) (#48091) |
| `86f2946fbe78` | #22 | fix(dashboard): recover the Chat tab when the agent session ends (NS-504) (#47674) |
| `4b7a18600393` | #26 | fix(desktop): retry the self-update rebuild once so the app relaunches (#48122) |
| `c276b017adc4` | #20 | feat(relay): connector⇄gateway channel auth + signed-HTTP inbound receiver + enroll CLI (#48147) |
| `ae8fa11097e1` | #20 | feat(cron): cron.provider config + plugins/cron discovery + resolver |
| `4440d77bf32d` | #20 | fix(update): scope install-method stamp to the code tree, not $HERMES_HOME (#48188) |
| `4c8bbe641696` | #20 | feat(cron): Chronos NAS-mediated managed-cron provider (scale-to-zero) |
| `3fc7b624d860` | #20 | feat(cron,gateway): NAS-JWT fire verifier + /api/cron/fire webhook (Chronos) |
| `b75757d4aa85` | #22 | feat(cron): wire on_jobs_changed, cron.chronos config, docs + agent↔NAS contract |
| `0b54a33a3467` | #26 | fix(langfuse): scope trace state by turn/request ids |
| `e1d10ec1ed29` | #20 | refactor(langfuse): extract _scope_prefix from _trace_key |
| `f4fbaa6cda8b` | #20 | fix(langfuse): bound _TRACE_STATE growth from non-finalizing turns |
| `2a5d51c16e94` | #23 | fix(openviking): adapt memory provider for current api |
| `2f7c4858a764` | #20 | fix(tui): refresh tool snapshot when MCP discovery lands after agent build (#48403) |
| `92e6d8c858f6` | #26 | fix(desktop): dispose open PTY sessions in before-quit handler |
| `5ffbfed193ad` | #20 | feat(mcp-catalog): add official Unreal Engine 5.8 MCP server |
| `0fa7d6f6609c` | #20 | fix(desktop): never persist or restore a named custom provider as bare "custom" (#48547) |
| `51ee5b2c94d0` | #20 | fix(desktop,tui): surface self-improvement review summary + honor memory_notifications |
| `73cd8622f9fc` | #22 | feat(billing): /billing terminal billing — interactive TUI + CLI client (#45449) |
| `4ed2f3399418` | #26 | fix(thread): allow scrolling long user messages in chat history (#48619) |
| `9705e7944ae4` | #20 | fix(picker): remove max_models=50 cap in interactive model pickers |
| `49596b70cb2d` | #20 | fix(gateway): resume follows the compression tip so post-compression replies render |
| `769f307042d2` | #26 | fix(npm): lock react-simple-icons to 13.11.1 |
| `03d9a95a74b2` | #20 | fix(desktop): show Hindsight memory provider (#37546) |
| `d2c53ff5583e` | #20 | feat(relay): WS-only inbound on the gateway adapter (Phase 3) (#48294) |
| `36851fa576eb` | #20 | fix(docker): support WebUI installs from read-only sources (#48541) |
| `c34840e22e08` | #20 | fix(cron): serve /api/cron/fire on the dashboard app (hosted-agent surface) |
| `620fd59b8e6f` | #20 | feat(model-picker): add Refresh Models control to bust stale model cache (#48691) |
| `cfb55de5ea49` | #21 | Update Stripe Projects skill docs (#48673) |
| `c02192ff6ace` | #20 | feat(image-gen): add image-to-image / editing to image_generate (#48705) |
| `c7b7f92ec14a` | #20 | fix(openviking): sync structured turns with tool parts |
| `d7cd0bc0863c` | #20 | fix(openviking): preserve structured sync attribution |
| `9362ce2575e0` | #22 | feat(skills): add html-artifact skill, fold in sketch + architecture-diagram + concept-diagrams (#48899) |
| `fcac0f94d484` | #23 | fix(openviking): guard empty tool_id in batch skip set; reuse env_var_enabled |
| `27a6e188c4b4` | #23 | refactor(openviking): derive recall-tool name set from canonical schemas |
| `2d4046c6de97` | #23 | refactor(openviking): reuse pre-scanned tool_input for pending tool calls |
| `be2c2beb96e5` | #23 | refactor(openviking): name tool_status constants and alias sets |
| `1699525638ed` | #20 | fix(tui): route pending-input commands via command.dispatch (#48848) |
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
| `93d6e730288e` | #20 | fix(mcp): expose late-connecting MCP tools to the agent (TUI/CLI/gateway) |
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
