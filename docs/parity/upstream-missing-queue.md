# Upstream Missing Patch Queue

Generated: `2026-05-01T04:50:41.634533+00:00`

- Range: `main..upstream/main`; total commits tracked: `1309`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 514 |
| #21 | GPAR-02 skills parity | 46 |
| #22 | GPAR-03 UX parity | 313 |
| #23 | GPAR-04 gateway/plugin-memory parity | 97 |
| #24 | GPAR-05 environments+parsers+benchmarks | 4 |
| #25 | GPAR-06 packaging/docs/install parity | 15 |
| #26 | GPAR-07 upstream queue backfill | 320 |

| Disposition | Commit Count |
| --- | ---: |
| pending | 195 |
| ported | 49 |
| superseded | 1065 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `4523965de9eb` | #22 | feat(dashboard): add profiles management page |
| `58c07867e3b1` | #20 | fix(dashboard): keep profiles list resilient |
| `1745cfc6d73b` | #22 | fix(dashboard): avoid node-only ui imports in browser |
| `3e200b64fbac` | #22 | fix(profiles): update terminal command for copying based on profile name |
| `ae11a310582a` | #20 | feat(profiles): add profile setup command endpoint and wrapper creation |
| `469e4df3c257` | #20 | fix(profiles): preserve skills on dashboard profile creation |
| `9b62c98170c4` | #22 | chore(dashboard): restore package lock metadata |
| `4c0cc77e94f4` | #22 | fix(dashboard): keep ui imports browser-safe after rebase |
| `cb0e2e2f36b5` | #26 | Potential fix for pull request finding |
| `7a4da315a2bc` | #25 | fix(docker): add curl to apt dependencies |
| `98a428fd61b9` | #20 | fix(cli): recover from leaked mouse tracking escapes |
| `d05497f8126d` | #22 | fix(tui): reset terminal modes on startup and exit |
| `87e259a67832` | #20 | fix(cli): tighten mouse leak sanitizer |
| `7fae87bc00da` | #20 | fix(gateway): refresh cached agents after MCP tool changes |
| `4d7fc0f37ced` | #23 | feat(gateway,cli): confirm /reload-mcp to warn about prompt cache invalidation |
| `8f144fe36b2a` | #20 | feat: pluggable platform adapter registry + IRC reference implementation |
| `2e20f6ae2d69` | #20 | feat: complete plugin platform parity — all 12 integration points |
| `457128d4e81c` | #26 | fix: wire PII redaction + token empty warnings for plugin platforms |
| `e464cde58fff` | #23 | feat: final platform plugin parity — webhook delivery, platform hints, docs |
| `52d9e5782537` | #26 | feat: dynamic toolset generation for plugin platforms |
| `1f1608067ca1` | #20 | feat(gateway): unify setup flows, load platforms dynamically from registry |
| `6e42daf7dd30` | #26 | fix(nix): bundle plugins/ and expose it via HERMES_BUNDLED_PLUGINS |
| `868bc1c2425e` | #20 | feat(irc): add interactive setup |
| `71c8ca17dc89` | #22 | chore(salvage): strip duplicated/merge-corrupted blocks from PR #17664 |
| `4d363499dba9` | #20 | feat(plugins): bundled platform plugins auto-load by default |
| `828d3a320bbc` | #20 | fix(anthropic): reactive recovery for OAuth 1M-context beta rejection (#17752) |
| `b06a06e6087a` | #25 | fix(docker): restore trailing newline on Dockerfile |
| `f73364b1c4ac` | #20 | fix(ci): stabilize main test suite regressions (#17660) |
| `ce0c3ae49390` | #20 | fix(aux): remove hardcoded Codex fallback model, drop Codex from auto chain (#17765) |
| `62a5d7207d15` | #20 | feat(plugins): bundle hermes-achievements + scan full session history (#17754) |
| `718e4e2e7ec8` | #26 | fix(plugins): register dynamically-loaded modules in sys.modules before exec |
| `3c27efbb914a` | #22 | feat(dashboard): configure main + auxiliary models from Models page (#17802) |
| `21e695fcb6e3` | #20 | fix: clean up defensive shims and finish CI stabilization from #17660 (#17801) |
| `b3137d758c9e` | #20 | feat(teams): add Microsoft Teams platform adapter as a plugin |
| `a696bceafaa2` | #26 | fix(tools_config): handle plugin platforms in platform_tool_universe |
| `ca5bebef00d3` | #26 | fix(teams): send images as attachments instead of markdown links |
| `39b0bc377ccd` | #26 | fix(teams): override send_image_file for local image attachments |
| `45780edbbf02` | #26 | feat(teams): keep card body visible after approval button click |
| `e23bb18dac34` | #26 | fix(teams): rewrite interactive_setup to use teams CLI flow |
| `26787ce63815` | #20 | test(gateway): isolate plugin adapter imports and guard the anti-pattern |
| `aa7bf329bc06` | #20 | feat(gateway): centralize audio routing + FLAC support + Telegram doc fallback (#17833) |
| `fd0796947f67` | #20 | fix: stabilize CI — TS widen, sys.modules restore, WS subscriber race (#17836) |
| `5b85a7d35160` | #20 | fix(update): kill stale dashboard processes instead of warning (#17832) |
| `2facea7f7156` | #20 | feat(tts): add command-type provider registry under tts.providers.<name> (#17843) |
| `0ad4f55aa8d7` | #20 | feat(dashboard): add --stop and --status flags (#17840) |
| `25caaa4a709f` | #26 | feat(tips): add cost-saving tips from April 30 tip-of-the-day (#17841) |
| `97a851bf970d` | #23 | fix(openviking): normalize summary pseudo-URIs to prevent v0.3.3 500s |
| `bff8ab031130` | #20 | test(openviking): add helper regression coverage |
| `10e43edc096f` | #23 | fix(openviking): fallback summary reads to content/read for file URIs |
| `5d253e65b799` | #23 | fix(openviking): pre-check fs/stat to route file URIs before hitting directory-only endpoints |
| `d2536a72bf27` | #20 | fix(acp): replay session history on load |
| `658947480a01` | #20 | fix(acp): drop dead message_id kwarg from replay chunks |
| `0da968e521f3` | #22 | fix(curator): unify under auxiliary.curator (hermes model, dashboard) (#17868) |
| `2662bfb7560d` | #20 | fix(tests): make test_update_stale_dashboard immune to hermes_cli.main reload (#17881) |
| `8d302e37a896` | #20 | feat(tts): add Piper as a native local TTS provider (closes #8508) (#17885) |
| `cb130bf7765f` | #24 | fix(ssh): prevent tar from overwriting remote home dir permissions |
| `663ba9a58fc6` | #20 | fix(gateway): drain pending messages via fresh task, not recursion (#17758) |
| `f44f1f96151c` | #23 | fix(gateway): preserve session guard across in-band drain handoff |
| `f54935738c68` | #20 | fix(cron): surface agent run_conversation failure flags as job failure |
| `362996e269bd` | #20 | fix(runtime_provider): _get_named_custom_provider must honour transport field on v12+ providers dict |
| `01d7c87eccfe` | #26 | chore(release): map zicochaos to GitHub login |
| `3858f9419e22` | #20 | fix: handle gateway Ctrl+C shutdown cleanly |
| `19f9be1dffaf` | #20 | fix(tools): serialize concurrent hermes_tools RPC calls from execute_code |
| `5af8fa5c8cb7` | #26 | chore(release): map Heltman email to username for AUTHOR_MAP |
| `ca87c822ede2` | #26 | fix(gateway): guard yaml.safe_load and float() env var casts against crash |
| `411f586c6710` | #26 | refactor(gateway): extract _float_env helper for env-var float casts |
| `04ea895ffb4f` | #23 | feat(gateway/signal): add support for multiple images sending |
| `3de8e2168359` | #23 | feat(gateway): native send_multiple_images for Telegram, Discord, Slack, Mattermost, Email |
| `cc5b9fb581bd` | #20 | fix(transport): omit thinking_config for Gemma on the gemini provider (#17426) |
| `fbb3775770c9` | #20 | fix(gateway): enforce auth check in busy-session path to prevent unauthorized injection (#17775) |
| `0dd373ec4397` | #20 | fix(context): honor model.context_length for Ollama num_ctx and all display paths |
| `70ae678af1bd` | #26 | chore(release): map rob@atlas.lan to @rmoen |
| `e0fa2cf97259` | #20 | fix(tools): isolate get_tool_definitions quiet_mode cache + dedup LCM injection (#17335) |
| `201f7caed843` | #26 | fix: prevent bare 'custom' slug in model.provider (#17478) |
| `61fec7689d21` | #26 | chore(release): map Andy283 gitee email in AUTHOR_MAP |
| `3fc4c63d387f` | #20 | test(model_switch): update regression to reflect bare-custom guard |
| `b50bc13ef99d` | #20 | fix(config): preserve YAML lists in hermes config set (#17876) |
| `87f5e1a25a21` | #20 | test(ssh): update tar pipe assertion for --no-overwrite-dir |
| `d1d0ef6dbda9` | #20 | fix(gateway): persist user message on transient agent failures (#7100) |
| `e8e5985ce6ad` | #26 | fix(curator): seed defaults on update, create logs/curator dir, defer fire import (#17927) |
| `eda1d516dc7b` | #26 | fix(skills): exclude .archive from skill index walk |
| `a845177ebea7` | #26 | fix(skills): also exclude .archive in skills_tool + add author map entry |
| `4c792865b44d` | #20 | test(gateway): pin cleanup invariants for #17758 in-band drain hand-off |
| `4178ab3c0765` | #26 | fix(skills): wire bump_use() into skill invocation and preload paths (#17782) |
| `ae8930afa52c` | #26 | fix(skills): also bump_use on skill_view tool invocation |
| `9a145406031a` | #25 | fix(nix): replace magic-nix-cache with Cachix (#17928) |
| `407dfbb02198` | #24 | fix(ci): stabilize current main test regressions |
| `cad7944b9291` | #22 | fix(tui): reset extended keyboard modes |
| `e30de51ee9e2` | #26 | fix(cli): tighten terminal leak fast path |
| `4e296dcdda9d` | #26 | fix(auxiliary): pass raw base_url to _maybe_wrap_anthropic for correct transport detection (#17467) |
| `2d3c041338e4` | #25 | change(nix): dedupe nix lockfile checking scripts in ci (#18000) |
| `b9d9fa7df81b` | #20 | fix(tui): respect max turns config |
| `cdf9793d6d6b` | #20 | fix(acp): advertise and forward image prompts |
| `8b290a5908fb` | #20 | feat(curator): split archived into consolidated vs pruned with model + heuristic classification (#17941) |
| `7913d6a90f8c` | #26 | chore(author-map): add y0shua1ee and 0xDevNinja for curator PRs (#18031) |
| `564a649e6ae7` | #20 | fix(curator): scan nested archive subdirs in restore_skill |
| `f4b76fa27282` | #20 | fix: use skill activity in curator status |
| `7c0742220221` | #22 | feat(tui): add a mini help menu when u write ? in the input field |
| `d60a9917d342` | #20 | feat(curator): show most-used and least-used skills in `hermes curator status` (#18033) |
| `699a9c11a99f` | #20 | test(acp): accept prompt persistence kwargs in mocks |

