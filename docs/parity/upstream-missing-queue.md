# Upstream Missing Patch Queue

Generated: `2026-07-26T11:07:58.958431+00:00`
Truth status: **FAIL**

- Canonical ledger: `docs/parity/upstream-source-ledger.json` (`d2f1cd35e6ead070a3ab3809f69d0753c9fe03fe1b6c3cab8533262b9432ee74`).
- Fetched range: `9017766a85934e4e657a24b1c6722df15a0f682f..eb52760564dbba2e5971fa54bd67384e281cd3b8`.
- Complete range commits: `3113`; unowned: `2501`.

| Disposition | Commit Count |
| --- | ---: |
| pending | 2501 |
| superseded | 612 |

## Source and Classification Errors

| Code | Count |
| --- | ---: |
| unowned_divergence | 1 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `101b9f8dcdde` | #26 | feat(memory): dispatch /config reads+writes on provider storage backend |
| `4ef0672cf9da` | #26 | feat(desktop): inline memory config panel + full-config modal |
| `15c8c34c915e` | #20 | test(memory): cover Honcho config schema, host-block backend, and panel |
| `2a632807e054` | #23 | refactor(memory): move provider config schemas into their plugins |
| `822c8226d7a5` | #26 | refactor(desktop): group memory provider config UI under settings/memory |
| `a9cce6d84483` | #20 | fix(memory): write honcho config through the plugin's own resolution, locked |
| `4d4db4281ee2` | #26 | fix(desktop): submit only edited fields from the full-config modal |
| `0e09cc5cd457` | #26 | fix(desktop): remount provider config panel when the provider changes |
| `41e59f6126d4` | #23 | fix(memory): retry failed config schema loads instead of caching None |
| `1f94e74a2dfd` | #26 | refactor(memory): collapse duplicated per-backend field helpers |
| `29016a50bcf6` | #26 | perf(memory): run provider config I/O off the event loop |
| `703305751dee` | #23 | style(memory): flatten comment blocks to single lines |
| `4a6751a2bcce` | #26 | fix(desktop): preserve project cwd for new sessions |
| `0c8cf21882be` | #20 | feat(nemo-relay): activate dynamic plugins |
| `2f74e29e3b1e` | #20 | fix(nemo-relay): preserve managed interceptor outcomes |
| `f846a350c7fa` | #20 | fix(nemo-relay): harden dynamic plugin lifecycle |
| `76c063b3d979` | #20 | fix(memory): profile-scope the provider config endpoints |
| `35099685beb1` | #20 | feat(memory): provider actions extension point |
| `1ba70a6f70c4` | #26 | fix(desktop): inline error + retry for failed panel loads, calmer surfaces |
| `8333b0448313` | #26 | fix(desktop): solid background wells for provider config inputs |
| `a182ddbf9f8c` | #23 | feat(memory): per-field info tooltips + name the profile in the full-config modal |
| `7faacb6dab09` | #26 | fix(desktop): drop panel tint, cap info tooltip width |
| `832c5f9bc901` | #26 | Fix slow Z.AI startup by caching auto-detected endpoint to disk |
| `c75e1d1b8760` | #26 | chore: add veradim to AUTHOR_MAP for PR #41201 salvage |
| `6eeed3f1e884` | #26 | fix: don't flip active_provider when caching Z.AI probe result |
| `1192f29450f1` | #26 | fix: Z.AI endpoint persist failure must not break URL resolution |
| `98804dbeef91` | #26 | fix(tui_gateway): back off notification poller when session is busy |
| `5413c42f2cb8` | #26 | chore: add SiteupAgencia to AUTHOR_MAP for #57435 salvage |
| `aab351bfa6c6` | #20 | fix(delegation): route async results to origin session |
| `4b27be111480` | #20 | fix(delegation): fail-closed orphan handling + session-scoped delegation lifecycle |
| `b64b802131ff` | #20 | feat(models): swap curated Tencent Hy3 Preview for GA tencent/hy3, drop owl-alpha (#60943) |
| `ee0b54e16cbb` | #26 | docs(i18n): align translated CONTRIBUTING files with pyproject Python range (3.11-3.13) |
| `f75f3cd71399` | #20 | fix(delegation): route async delegate_task results back to originating session |
| `65372395eb29` | #20 | fix(delegation): positive-proof ownership for the post-turn drain |
| `67d64124c3cc` | #26 | feat(desktop): autosave the inline provider panel, drop its Save button |
| `beafc55c7d99` | #26 | fix(desktop): dismiss stale prompt overlays |
| `63c4100fe618` | #23 | perf(yuanbao): bounded-concurrency inbound media resolve |
| `b848fcbf11df` | #23 | feat(Yuanbao) optimizes media resource processing speed: parallel download |
| `d39c62409b5f` | #20 | fix(delegate): pin async completion to spawning parent session (#57498) |
| `75efd73961a8` | #20 | fix(gateway): never resurrect ended sessions for delegation completions; /new severs in-flight delegations |
| `ae5e39005bde` | #23 | fix(gateway): run webhook route scripts off the event loop + AUTHOR_MAP entry |
| `8e734810dfcb` | #26 | fix(desktop): continue the selected stored session instead of minting a new one (#55578) (#60874) |
| `aabfedcac03c` | #22 | docs(webhook): complete filters + route-scripts coverage across doc surfaces (#60983) |
| `1508ece2d98e` | #20 | Merge origin/main: unify declared-schema and instance-schema provider config |
| `62ada5175c2a` | #22 | feat(xai): add grok-4.5 (GA) to model catalog, context lengths, and reasoning-effort allowlist (#60887) |
| `4d611ba0c3b1` | #26 | chore: AUTHOR_MAP entry for nullptr0807 (PR #60956 salvage) |
| `9a4341aa90ec` | #20 | fix(dashboard): keep status responsive when session db locks |
| `346e5673de8d` | #20 | 🐛 fix(dashboard): offload cron profile scans |
| `49fa04a2356d` | #20 | 🐛 fix(dashboard): use managed cron threadpool |
| `7d0ddbb2ff50` | #20 | fix(dashboard): cache gateway PID status probes |
| `492be28e5061` | #20 | fix(web): bound dashboard action log tails |
| `d7e4d94e2fa4` | #26 | perf(dashboard): recursive-CTE descendant lookup in _session_latest_descendant |
| `24d5bda1ef2f` | #26 | fix(dashboard): run GET /api/sessions session-DB read off the event loop |
| `206c04239207` | #26 | chore: AUTHOR_MAP entries for salvaged contributors (#53511/#53966/#39140) |
| `664878b0baa0` | #20 | fix: guard /api/status active count against missing state.db |
| `c95cf313c9c3` | #20 | test: patch the cached PID probe name in profile-unification status tests |
| `0d5549a945d7` | #26 | fix(api): add pagination to GET /api/sessions/{id}/messages |
| `4df16c429bca` | #20 | Refresh on upstream/main: resolve conflicts (no behavior change) |
| `4f220fc88b82` | #20 | fix: follow-up for salvaged #60347/#43653/#47437 |
| `7570bb4ad490` | #20 | fix: offset-without-limit was silently ignored in get_messages |
| `7a25017a0022` | #20 | test: accept compact_rows kwarg in tui_gateway session fakes |
| `31e39dec84cb` | #20 | test: expect compact_rows in the read-only status-count fake |
| `1792a3aa7261` | #26 | chore: add gauravsaxena1997 to AUTHOR_MAP (PR #59868 salvage) |
| `74e28f7d10b5` | #20 | style(tests): merge import blocks in test_summarize_api_error |
| `be1346cf2a0e` | #26 | fix(gateway): reload fallback_providers on live agent create/reuse |
| `e721ad89e30d` | #20 | test(gateway): cover fallback_providers reload for live sessions |
| `0a01b2087d0c` | #20 | fix(gateway): harden fallback-chain refresh from review findings |
| `e21ba912210e` | #20 | perf(skills): speed up snapshot prompt builds |
| `449706cb5219` | #26 | chore: add dexhunter to AUTHOR_MAP (PR #60339 salvage) |
| `2a293319b17b` | #20 | fix: run CLI new-session memory flush off-thread |
| `d8bc4f242f6e` | #20 | fix: serialize /new end→switch boundary on the memory manager worker |
| `e0ed5dc9ed93` | #20 | refactor: address Phase-2 review findings on /new boundary handoff |
| `6d6521025297` | #26 | feat(desktop): group tool calls across text-less assistant messages |
| `fac85518fc8c` | #26 | Merge pull request #61147 from NousResearch/bb/desktop-tool-window-merge |
| `39d09453f95e` | #25 | feat(desktop): ts-ify everything |
| `7a65530fa5b3` | #22 | fix(js): set @types/node to node 22, what's required in "engines" |
| `56a8e81d33a5` | #26 | cleanup(desktop): `npm run fix` for fmtting |
| `bf913abc2efc` | #26 | fix(desktop): stop using tsx to boot Electron main in dev |
| `88a58ff1355e` | #26 | Merge pull request #61277 from NousResearch/bb/fix-desktop-tsx-electron40 |
| `3e24b16f5660` | #22 | fix(dashboard): support mobile OAuth login |
| `473407174bf3` | #20 | test(dashboard): assert server still gates OAuth endpoints without cookie |
| `32a0f9e17a57` | #26 | fix(model_metadata): avoid str(tools) token estimate stalls |
| `f4d5cfd0fdeb` | #20 | test(model_metadata): cache tools schema token estimate |
| `55dbc3ffb5e4` | #20 | fix(model_metadata): bound the tools-token estimate cache |
| `cbf685356d99` | #26 | fix(gateway): sync HERMES_HOME before refreshing system systemd units |
| `8f18f6c69523` | #20 | test(gateway): cover sudo system-unit refresh adopting HERMES_HOME |
| `d54a8f7079e4` | #20 | refactor(gateway): funnel HERMES_HOME sync through a single chokepoint |
| `a4ba8c9640c0` | #26 | chore: map poowis2011@hotmail.com → Umi4Life for PR #47377 salvage |
| `c889941916e6` | #26 | fix(model_metadata): cache detect_local_server_type result for process lifetime |
| `91ece5c2fc9d` | #20 | perf(model-metadata): resolve localhost to IPv4 in detect_local_server_type |
| `040e30aa722a` | #20 | perf(model_metadata): cache ollama /api/show probe + normalize context-cache keys |
| `20cb385328f1` | #20 | fix(model_metadata): widen localhost->IPv4 rewrite to all sibling probe sites |
| `f556edc10d9b` | #20 | fix(model_metadata): address Phase-2 review findings on probe caches |
| `750c1310a641` | #26 | fix(caching): include Kimi/Moonshot in OpenRouter prompt cache policy (#25970) |
| `fbbb8415c327` | #20 | test(caching): pin Kimi/Moonshot OpenRouter cache policy (#25970) |
| `1d689e192032` | #20 | fix(caching): use canonical Kimi-family matcher in cache policy |
| `74609f926c35` | #26 | fix(dashboard): run residual cron profile I/O off the event loop |
| `8cfada0df4e7` | #20 | test(dashboard): pin cron fire + blueprint handlers off the event loop |
| `d928017742d5` | #20 | fix(dashboard): validate HERMES_WEB_DIST before startup |
| `4ed910c6894f` | #20 | fix(cli): mock systemd preflight in gateway service tests for non-systemd environments (#15187) |
