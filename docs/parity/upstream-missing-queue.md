# Upstream Missing Patch Queue

Generated: `2026-06-16T02:12:02.309717+00:00`

- Range: `HEAD..upstream/main`; total commits tracked: `5956`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 2570 |
| #21 | GPAR-02 skills parity | 119 |
| #22 | GPAR-03 UX parity | 877 |
| #23 | GPAR-04 gateway/plugin-memory parity | 416 |
| #24 | GPAR-05 environments+parsers+benchmarks | 22 |
| #25 | GPAR-06 packaging/docs/install parity | 128 |
| #26 | GPAR-07 upstream queue backfill | 1824 |

| Disposition | Commit Count |
| --- | ---: |
| mirrored | 70 |
| pending | 121 |
| ported | 266 |
| superseded | 5499 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `bb5cb3283898` | #23 | refactor(honcho): canonicalize identity-mapping on pinUserPeer, migrate legacy key |
| `d7dfeed6dc42` | #23 | feat(honcho-setup): replace deployment-shape prompt with gateway-gated identity tree |
| `99feb036077a` | #23 | docs(honcho): demote pinPeerName to deprecated alias; document gateway identity tree |
| `2708c33c7570` | #23 | docs(honcho): anonymize example peer name to alice |
| `1544813bfe56` | #20 | chore(honcho): replace example Telegram UID with placeholder |
| `d62979a6f34f` | #20 | feat(desktop): composer status stack, live subagent windows, editable prompts (#44630) |
| `79c3ed3cc91a` | #26 | fix(desktop): new chat honours the active profile instead of rubberbanding to default (#45057) |
| `46d758bb3e07` | #26 | feat(desktop): window translucency slider in Appearance settings (#45086) |
| `2f9d18711fb9` | #20 | fix(ci): remove pytest-timeout, use per-file timeout only |
| `8044bf0206c1` | #25 | fix(ci): only save test durations when tests pass |
| `1e25358a8f22` | #20 | refactor(desktop): use port 0 for ephemeral port discovery instead of PortPool reservation |
| `c61815232abe` | #20 | Update model correctly when updating from dashboard |
| `8c3c08c50be8` | #20 | Update implementation to make it cleaner |
| `bc3f4ed70fa5` | #20 | Skip redundant model switch |
| `6b4073648ece` | #20 | fix(tui): config.yaml wins over env model seed in per-turn sync |
| `05b9c84ca4b1` | #20 | Add Telegram Bot API 10.1 rich message support |
| `652dd9c9f248` | #20 | fix: rich messages follow-ups — reply_parameters, send latch, opt-in default |
| `2e874ef87926` | #26 | fix(desktop): allow dismissing settled tool rows |
| `b16e22b8f272` | #26 | fix(desktop): persist tool-row dismissal across virtualization; keep caret hittable |
| `bbf020e709ec` | #26 | feat(desktop): follow streaming output at bottom + jump-to-bottom button (#45263) |
| `e90672696ea7` | #26 | feat(desktop): worktree-aware sidebar grouping + composer/sidebar UX fixes |
| `0595af0ad19b` | #26 | feat(desktop): move workspace/worktree drag handle into the leading icon |
| `dd12a5403de9` | #26 | refactor(desktop): extract shared WorkspaceHeader for repo + worktree rows |
| `1899c8f507c3` | #21 | fix(skills): run youtube transcript helper through uv |
| `1a3cd3d436a1` | #26 | refactor(desktop): collapse sidebar drag-reorder into one generic ReorderableList |
| `78ce91750ec6` | #26 | fix(desktop): crisp terminal text via opaque xterm canvas |
| `d14f6c95632b` | #26 | fix(desktop): stop streaming autoscroll bounce; move attachments below user bubble |
| `7c226cc57fe6` | #26 | perf(desktop): isolate streaming re-renders & cut layout thrash |
| `edc36f3a4589` | #26 | perf(desktop): incremental markdown rendering during streams |
| `3cf7d43262d4` | #26 | perf(desktop): faster session resume & warm AudioContext at idle |
| `7d183f64979f` | #26 | fix(desktop): theme the image-gen placeholder instead of a white square (#45354) |
| `bf090deed33e` | #26 | fix(desktop): stop stranding queued prompts across backend bounces |
| `f23a4b7bb3b8` | #26 | fix(desktop): keep queued drains quiet on transient "session busy" |
| `18916376f198` | #26 | fix(desktop): never surface "session busy" — retry every submit past it |
| `7f302c91b240` | #26 | chore: uptick |
| `1e755ff5568a` | #26 | fix(desktop): keep recents sorted unless manually reordered (#45404) |
| `76b93869d8ed` | #26 | fix(desktop): rebuild thread autoscroll on use-stick-to-bottom |
| `77687156b4b8` | #26 | fix(desktop): tighten multiline user prompt spacing |
| `b15dc58064eb` | #26 | fix(desktop): keep generated images in the tool slot, not inline |
| `b82d2e549fa5` | #26 | fix(desktop): keep the diffusion placeholder circular at any aspect |
| `b6c7ebf028d8` | #20 | fix(tui): honor provider_routing config in the desktop/TUI backend (#44953) |
| `bdd3868b577a` | #26 | fix(desktop): keep profile color picker open from the context menu (#45489) |
| `8cf9d8689d56` | #22 | fix(desktop): keep composer usable during reconnect (#45488) |
| `2d474e39c7ee` | #20 | fix(acp): preserve memory provider tools |
| `266b5a19f128` | #26 | feat(desktop): expand the full command inline from the approval bar |
| `5d6c16e97237` | #26 | test(desktop): cover the inline command expander on the approval bar |
| `2681c5a12d8d` | #20 | fix(photon): correct gateway start command (#45566) |
| `573b964dc780` | #25 | fix(installer): clear an unmerged git index before stashing on update |
| `a59d5e37e8ab` | #20 | feat(telegram): make rich messages always on (#45584) |
| `e256f4aae493` | #20 | fix(gateway): don't restore a bare billing provider as the resumed session's provider |
| `643dc8279306` | #20 | Fix custom provider identity loss in session persistence |
| `2667601c05cd` | #20 | fix(tui): keep reasoning-only assistant turns visible on session resume |
| `aa53a78d6703` | #26 | fix(desktop): hand off Windows bootstrap recovery (#45594) |
| `4373e802a1b9` | #25 | fix(docs): reuse healthy skills index during Pages deploys (#45616) |
| `28bf8fb47d38` | #22 | feat(dashboard): clone profiles from any source |
| `cc14b74718aa` | #22 | docs(profile): update clone-from references |
| `425e777f54b8` | #26 | fix(desktop): polish slash command completion (space/tab/click + typed args) (#45760) |
| `0a865e5948cb` | #26 | fix(desktop): bypass Chromium editing pipeline for large paste & select-delete (#45812) |
| `bf8effad023b` | #20 | fix(utils): copy fallback for atomic replace across devices (#43852) |
| `6b76284c7769` | #26 | fix(desktop): surface off-screen approvals via the jump-to-bottom control (#45853) |
| `a218a0f1569c` | #20 | fix(agent,gateway,doctor): add SSL CA cert bundle fail-fast guard |
| `dc90ca4e1740` | #26 | fix(ssl): run CA guard during agent initialization |
| `7aaae7acd0d6` | #20 | fix(ssl): align guard docs and escape hatch |
| `8d5d36d79358` | #20 | fix(dispatch): forward session_id into registry.dispatch (#28479) |
| `12682d96b9c8` | #20 | feat(telegram): restore rich messages opt-out |
| `e986e3fc689c` | #26 | fix: add provider account removal |
| `1b16c481708d` | #20 | fix: guard OAuth account removal |
| `b4ba3f5e3b37` | #26 | feat(desktop): add curated completion cue for agent turn completion (#42480) |
| `630a4ef03c8e` | #26 | feat(desktop): native OS notifications with per-type toggles |
| `b0288ae9b6ea` | #26 | feat(desktop): move completion-sound picker into Notifications settings |
| `9cbb91abd3a8` | #26 | fix(desktop): clarify UX — loading, enter-to-send, radio align (#46014) |
| `715b691723c6` | #20 | fix(desktop): show summarizing indicator during auto-compaction |
| `49dd91d682a4` | #26 | fix(desktop): show copied checkmark on session Copy ID (#46030) |
| `1eb13744b4ab` | #26 | fix(desktop): polish compaction indicator and preserve scrollback |
| `d842155da1e8` | #20 | Keep resumed profile cwd scoped to profile DB |
| `9f33d673e9e6` | #20 | fix(tui): persist resumed profile cwd updates to profile db |
| `5e851bc6bc51` | #26 | fix(discord): cap slash commands at Discord's 100-command limit |
| `0428945b5b07` | #20 | fix(desktop): keep profile homes out of bootstrap (#46073) |
| `b00060ce545c` | #20 | fix(agent): expose HERMES_REAL_HOME in subprocess envs for profile isolation |
| `723c2331bd23` | #20 | fix: make profile subprocess HOME policy explicit |
| `a4ee1f223d5f` | #25 | fix(install): make `npm install -g` packages reachable on PATH |
| `1db8f7ea8094` | #25 | fix(install): repair existing managed-Node global prefix on re-run |
| `972a9885ee20` | #20 | fix(mcp): block exfil-shaped stdio server configs (#46083) |
| `efbe1635dd2e` | #20 | fix(gateway): include replied-to media attachments (#46107) |
| `288f7026e332` | #20 | fix(messaging): correct Weixin personal account labeling |
| `08d89e7aba14` | #26 | fix(desktop): limit thinking shimmer to the disclosure label (#46197) |
| `a1f51feb72b4` | #20 | fix(telegram): avoid rich final duplicate previews (#46206) |
| `bff78a34dc44` | #20 | feat(zai): add GLM-5.2 with verified 1M context window |
| `f3fe99863d13` | #20 | revert(web): remove keyless Parallel search fallback (#46350) |
| `8fe334b056d4` | #26 | fix(desktop): inset hover-reveal trigger past the adjacent scrollbar (#44159) |
| `61ee2dbfdb40` | #20 | fix(s6): make profile gateway log parent writable (#46291) |
| `a376ca00819e` | #23 | feat(hindsight): make observation scopes configurable on retain |
| `c1a70a543925` | #20 | 🐛 fix(disk-cleanup): prune protected cleanup walks |
| `40699c329265` | #20 | 🐛 fix(disk-cleanup): avoid brittle sweep review issues |
| `975b9f0a5426` | #22 | docs: recommend standard installer for development (#46646) |
| `92a456f711eb` | #22 | fix(cli,deps): clear esbuild audit loop |
| `49e743985aaf` | #20 | fix: route minimax m3 reasoning controls through profile |
| `fbabf438a17c` | #26 | fix(desktop): sync $connection on profile switch so remote profiles attach images as bytes |
| `bee13817f069` | #26 | test(desktop): cover $connection resync on profile switch |
| `5b2604df999c` | #26 | fix(state): skip redundant trigram backfill before v11 FTS rebuild |
