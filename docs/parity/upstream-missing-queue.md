# Upstream Missing Patch Queue

Generated: `2026-07-21T22:45:58.039625+00:00`
Truth status: **FAIL**

- Canonical ledger: `docs/parity/upstream-source-ledger.json` (`0a35c83f7dfad4fbad5624bb82d75581f19b6f437aef52c4d4fb09f43de4828f`).
- Fetched range: `c002f8c4efe475c89c9f14d3995d44e6a5df5b27..146f4ed07d1c10031c86a5e7f2ac78d11b253dd9`.
- Complete range commits: `1839`; unowned: `1343`.

| Disposition | Commit Count |
| --- | ---: |
| pending | 1343 |
| superseded | 496 |

## Source and Classification Errors

| Code | Count |
| --- | ---: |
| unowned_divergence | 1 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `15c8c34c915e` | #20 | test(memory): cover Honcho config schema, host-block backend, and panel |
| `2a632807e054` | #23 | refactor(memory): move provider config schemas into their plugins |
| `a9cce6d84483` | #20 | fix(memory): write honcho config through the plugin's own resolution, locked |
| `41e59f6126d4` | #23 | fix(memory): retry failed config schema loads instead of caching None |
| `0c8cf21882be` | #20 | feat(nemo-relay): activate dynamic plugins |
| `2f74e29e3b1e` | #20 | fix(nemo-relay): preserve managed interceptor outcomes |
| `f846a350c7fa` | #20 | fix(nemo-relay): harden dynamic plugin lifecycle |
| `76c063b3d979` | #20 | fix(memory): profile-scope the provider config endpoints |
| `35099685beb1` | #20 | feat(memory): provider actions extension point |
| `220e7ca274e0` | #23 | refactor(honcho): list per-session first among session strategies |
| `a182ddbf9f8c` | #23 | feat(memory): per-field info tooltips + name the profile in the full-config modal |
| `8eac52054bf3` | #20 | fix(desktop): keep configured MoA presets in model picker |
| `832c5f9bc901` | #26 | Fix slow Z.AI startup by caching auto-detected endpoint to disk |
| `6eeed3f1e884` | #26 | fix: don't flip active_provider when caching Z.AI probe result |
| `1192f29450f1` | #26 | fix: Z.AI endpoint persist failure must not break URL resolution |
| `7ecc822e1165` | #20 | fix(cron): stop the ticker from stalling forever on a wedged jobs lock (#60703) (#60855) |
| `465cbe8bb51d` | #20 | test(tools): add unit tests for skill_gist |
| `aab351bfa6c6` | #20 | fix(delegation): route async results to origin session |
| `4b27be111480` | #20 | fix(delegation): fail-closed orphan handling + session-scoped delegation lifecycle |
| `b64b802131ff` | #20 | feat(models): swap curated Tencent Hy3 Preview for GA tencent/hy3, drop owl-alpha (#60943) |
| `ee0b54e16cbb` | #26 | docs(i18n): align translated CONTRIBUTING files with pyproject Python range (3.11-3.13) |
| `5057f03bfdce` | #22 | docs(i18n): align translated CONTRIBUTING files with pyproject Python range (3.11-3.13) |
| `f75f3cd71399` | #20 | fix(delegation): route async delegate_task results back to originating session |
| `65372395eb29` | #20 | fix(delegation): positive-proof ownership for the post-turn drain |
| `e10c8eba00b0` | #26 | fix(whatsapp): use windows_detach_popen_kwargs to prevent console window flash on Windows |
| `efb226b586a5` | #20 | fix(cli): preserve chat -q answer by gating exit-summary screen clear (#53009) |
| `58e1647b4982` | #20 | test(cli): update FakeCLI._print_exit_summary for new clear_screen kwarg |
| `63c4100fe618` | #23 | perf(yuanbao): bounded-concurrency inbound media resolve |
| `b848fcbf11df` | #23 | feat(Yuanbao) optimizes media resource processing speed: parallel download |
| `d39c62409b5f` | #20 | fix(delegate): pin async completion to spawning parent session (#57498) |
| `75efd73961a8` | #20 | fix(gateway): never resurrect ended sessions for delegation completions; /new severs in-flight delegations |
| `0cf2e39c411c` | #20 | feat(gateway): add webhook payload filters |
| `ae5e39005bde` | #23 | fix(gateway): run webhook route scripts off the event loop + AUTHOR_MAP entry |
| `76381e2a8e3a` | #20 | fix(compression): stop compaction thrash — 75% trigger floor under 512K, no summary output cap, reasoning-trace exclusion (#60989) |
| `aabfedcac03c` | #22 | docs(webhook): complete filters + route-scripts coverage across doc surfaces (#60983) |
| `1508ece2d98e` | #26 | Merge origin/main: unify declared-schema and instance-schema provider config |
| `62ada5175c2a` | #22 | feat(xai): add grok-4.5 (GA) to model catalog, context lengths, and reasoning-effort allowlist (#60887) |
| `d6a275b735d7` | #20 | fix(gateway): compact hygiene transcripts in place |
| `346e5673de8d` | #20 | 🐛 fix(dashboard): offload cron profile scans |
| `49fa04a2356d` | #20 | 🐛 fix(dashboard): use managed cron threadpool |
| `7d0ddbb2ff50` | #20 | fix(dashboard): cache gateway PID status probes |
| `492be28e5061` | #20 | fix(web): bound dashboard action log tails |
| `1e2ad17afd50` | #20 | fix: descendant CTE must dedup (UNION) to survive parent-chain cycles |
| `664878b0baa0` | #20 | fix: guard /api/status active count against missing state.db |
| `c95cf313c9c3` | #20 | test: patch the cached PID probe name in profile-unification status tests |
| `0d5549a945d7` | #26 | fix(api): add pagination to GET /api/sessions/{id}/messages |
| `22eb1af23a89` | #20 | perf(state): add compact_rows to skip system_prompt blob in session list queries |
| `4f220fc88b82` | #20 | fix: follow-up for salvaged #60347/#43653/#47437 |
| `7570bb4ad490` | #20 | fix: offset-without-limit was silently ignored in get_messages |
| `7a25017a0022` | #20 | test: accept compact_rows kwarg in tui_gateway session fakes |
| `31e39dec84cb` | #20 | test: expect compact_rows in the read-only status-count fake |
| `0569a637d037` | #20 | fix(agent): guard response.text access in _summarize_api_error against httpx.ResponseNotRead |
| `74e28f7d10b5` | #20 | style(tests): merge import blocks in test_summarize_api_error |
| `be1346cf2a0e` | #26 | fix(gateway): reload fallback_providers on live agent create/reuse |
| `e721ad89e30d` | #20 | test(gateway): cover fallback_providers reload for live sessions |
| `0a01b2087d0c` | #20 | fix(gateway): harden fallback-chain refresh from review findings |
| `e21ba912210e` | #20 | perf(skills): speed up snapshot prompt builds |
| `2a293319b17b` | #20 | fix: run CLI new-session memory flush off-thread |
| `d8bc4f242f6e` | #20 | fix: serialize /new end→switch boundary on the memory manager worker |
| `e0ed5dc9ed93` | #20 | refactor: address Phase-2 review findings on /new boundary handoff |
| `fac85518fc8c` | #26 | Merge pull request #61147 from NousResearch/bb/desktop-tool-window-merge |
| `39d09453f95e` | #25 | feat(desktop): ts-ify everything |
| `7a65530fa5b3` | #22 | fix(js): set @types/node to node 22, what's required in "engines" |
| `513dba42e6a0` | #22 | chore(models): drop x-ai/grok-4.3 from OpenRouter/Nous curated lists in favor of grok-4.5 (#61097) |
| `88a58ff1355e` | #26 | Merge pull request #61277 from NousResearch/bb/fix-desktop-tsx-electron40 |
| `3e24b16f5660` | #22 | fix(dashboard): support mobile OAuth login |
| `473407174bf3` | #20 | test(dashboard): assert server still gates OAuth endpoints without cookie |
| `32a0f9e17a57` | #26 | fix(model_metadata): avoid str(tools) token estimate stalls |
| `f4d5cfd0fdeb` | #20 | test(model_metadata): cache tools schema token estimate |
| `55dbc3ffb5e4` | #20 | fix(model_metadata): bound the tools-token estimate cache |
| `cbf685356d99` | #26 | fix(gateway): sync HERMES_HOME before refreshing system systemd units |
| `8f18f6c69523` | #20 | test(gateway): cover sudo system-unit refresh adopting HERMES_HOME |
| `d54a8f7079e4` | #20 | refactor(gateway): funnel HERMES_HOME sync through a single chokepoint |
| `3fe7f6d27a00` | #20 | fix: preserve fallback switch notice on successful fallback |
| `a4ba8c9640c0` | #26 | chore: map poowis2011@hotmail.com → Umi4Life for PR #47377 salvage |
| `c889941916e6` | #26 | fix(model_metadata): cache detect_local_server_type result for process lifetime |
| `91ece5c2fc9d` | #20 | perf(model-metadata): resolve localhost to IPv4 in detect_local_server_type |
| `9a18a2de1216` | #20 | fix(agent): probe localhost via IPv4 for LM Studio detection |
| `c454d32feb26` | #26 | fix(model_tools): honor model.context_length to skip OpenRouter probe on banner |
| `040e30aa722a` | #20 | perf(model_metadata): cache ollama /api/show probe + normalize context-cache keys |
| `20cb385328f1` | #20 | fix(model_metadata): widen localhost->IPv4 rewrite to all sibling probe sites |
| `f556edc10d9b` | #20 | fix(model_metadata): address Phase-2 review findings on probe caches |
| `750c1310a641` | #26 | fix(caching): include Kimi/Moonshot in OpenRouter prompt cache policy (#25970) |
| `fbbb8415c327` | #20 | test(caching): pin Kimi/Moonshot OpenRouter cache policy (#25970) |
| `1d689e192032` | #20 | fix(caching): use canonical Kimi-family matcher in cache policy |
| `8cfada0df4e7` | #20 | test(dashboard): pin cron fire + blueprint handlers off the event loop |
| `4ed910c6894f` | #20 | fix(cli): mock systemd preflight in gateway service tests for non-systemd environments (#15187) |
| `724ab9098dfb` | #20 | perf: avoid broad message prep deepcopies |
| `63ddd022a203` | #20 | refactor(salvage): scope #40632 to the two live copy-on-write sites |
| `e7648d59129a` | #20 | test: restore gemma-3-27b to keep-extra_content coverage |
| `f5bc18f90117` | #20 | fix: write expanded HERMES_WEB_DIST back for web_server's raw read |
| `cb79518d4fbe` | #26 | Merge pull request #61385 from kshitijk4poor/fix/dashboard-residual-cron-event-loop-io |
| `3ed7c8a8da6d` | #26 | Merge pull request #61388 from kshitijk4poor/fix/dashboard-validate-web-dist |
| `1e16120603b4` | #20 | fix(reasoning): add deepseek-v4-flash and deepseek-v4-pro to reasoning timeout floor |
| `411d59976410` | #20 | test: fold deepseek-v4 cases into canonical reasoning-floor test, drop duplicate file |
| `5a4249146fc4` | #26 | perf(skills): cache skill discovery results by directory mtime |
| `9e9608ecc304` | #20 | fix: harden skill-discovery cache signature + TTL |
| `cbdf87b21fed` | #20 | fix: return per-call copies from the skill-discovery cache |
| `709da844b5b6` | #20 | feat(gateway): attach MEDIA: caption to the media bubble on standalone sends |
| `73b611ad1972` | #26 | Merge pull request #61415 from kshitijk4poor/fix/media-tag-caption |
