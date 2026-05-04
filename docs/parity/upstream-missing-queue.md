# Upstream Missing Patch Queue

Generated: `2026-05-04T05:34:36.981959+00:00`

- Range: `main..upstream/main`; total commits tracked: `1447`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 575 |
| #21 | GPAR-02 skills parity | 49 |
| #22 | GPAR-03 UX parity | 333 |
| #23 | GPAR-04 gateway/plugin-memory parity | 115 |
| #24 | GPAR-05 environments+parsers+benchmarks | 4 |
| #25 | GPAR-06 packaging/docs/install parity | 16 |
| #26 | GPAR-07 upstream queue backfill | 355 |

| Disposition | Commit Count |
| --- | ---: |
| pending | 138 |
| ported | 51 |
| superseded | 1258 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `e2a490560610` | #22 | feat(dashboard): add Plugins page with enable/disable, auth status, install/remove |
| `6549b0f2b7fe` | #26 | fix(security): address CodeQL path-traversal and info-exposure findings |
| `7dc85495e05d` | #22 | style(plugins): make page full width |
| `9550d0fd46d1` | #22 | fix(plugins): show 'Plugins' in page header instead of 'Web UI' |
| `a52363231faa` | #22 | refactor(plugins): move rescan button to page header, remove redundant title |
| `c73b799de70d` | #22 | feat(dashboard): add hide/show toggle for dashboard plugins in sidebar |
| `443950e82736` | #26 | fix(tui): pass user_providers as dict to match CLI model-switch pipeline |
| `36fa8a4d28cf` | #22 | fix(tui): show absolute position numbers in model picker |
| `26f7f6850757` | #22 | feat(tui): show all providers in /model picker with inline API key setup |
| `f4c761c6a095` | #22 | feat(tui): add inline provider disconnect via 'd' keybind in /model picker |
| `c8e506c383f4` | #22 | fix(tui): address code review feedback on model picker |
| `c23c7c994bf8` | #22 | fix(tui): address remaining review feedback — ordering and digit shortcuts |
| `fc78e708ed0c` | #26 | fix(update): don't crash hermes update if skill config scan fails (#18257) |
| `41fa1f1b5cf5` | #20 | fix(acp): run /steer as a regular prompt on idle sessions (#18258) |
| `f0dc919f92c5` | #20 | fix(compression): include system prompt + tool schemas in token estimates (#18265) |
| `bfb704684ec6` | #20 | fix(deepseek): use non-empty reasoning_content placeholder for V4 Pro thinking mode |
| `e2eb561e8e1a` | #20 | fix(curator): rewrite cron job skill refs after consolidation (#18253) |
| `4caad285a602` | #23 | feat(gateway): auto-delete slash-command system notices after TTL (#18266) |
| `50c046331dc7` | #20 | feat(update): add --yes/-y flag to skip interactive prompts (#18261) |
| `7c6c5619a7b8` | #22 | docs(sidebar): collapse exploding skills tree to a single Skills node (#18259) |
| `265bd59c1d9f` | #20 | feat: /goal — persistent cross-turn goals (Ralph loop) (#18262) |
| `1be3b74cfb45` | #20 | fix(gateway): honor MATRIX_HOME_ROOM in onboarding |
| `77dd6d54699f` | #26 | chore(release): add mikeyobrien to AUTHOR_MAP |
| `9ca72a69a730` | #20 | fix(moonshot): fill missing type before enum cleanup to handle anyOf branches without explicit type |
| `9cb5baeacfc7` | #26 | chore(release): map hendrixfreire for moonshot salvage |
| `2af8b8ff3712` | #20 | fix(moonshot): also strip nullable/enum after anyOf collapse |
| `cf2b2d31ce77` | #22 | docs: add Persistent Goals (/goal) feature page (#18275) |
| `c6eebfc25a57` | #22 | docs: publish llms.txt and llms-full.txt for agent-friendly ingestion (#18276) |
| `dfe512c58db6` | #21 | fix(paths): route achievements plugin + profile-tui through HERMES_HOME |
| `a49f4c617da3` | #25 | fix: prevent tui rebuilding assets |
| `a2a32688ca8a` | #22 | docs(website): add User Stories and Use Cases collage page (#18282) |
| `b7ad3f478f9b` | #23 | fix(yuanbao): enforce owner identity check on group slash commands |
| `75e1339d4cdb` | #23 | fix(telegram): send seed message after creating DM topics (#18334) |
| `a01c1f7305bd` | #26 | fix: kanban button |
| `c5b4c4816566` | #20 | fix: lazy session creation — defer DB row until first message (#18370) |
| `77c0bc6b13c8` | #20 | fix(curator): defer first run and add --dry-run preview (#18373) (#18389) |
| `f99676e31540` | #20 | fix(gateway): auto-restart when source files change out from under us (#17648) (#18409) |
| `0b76d23d1acf` | #22 | makes the Persistent Goals docs accessible in the docs nav (and llms.txt) (#18481) |
| `7cda0e522443` | #23 | fix(gateway/slack): ephemeral ack and routing for slash commands |
| `0ab2d752ffda` | #20 | feat(gateway): private notice delivery and Slack format_message fixes |
| `f34d298495b0` | #26 | chore: add probepark to AUTHOR_MAP |
| `8fcc160f6b97` | #23 | fix(gateway/slack): review fixes — scope ephemeral to commands, user isolation |
| `a717199bbf31` | #20 | fix(slack): exclude reserved Slack commands from native slash manifest |
| `2b3923ff138f` | #23 | fix(gateway): coerce scalar free_response_channels to str before split |
| `5cdc39e29a03` | #20 | fix(gateway): preserve case-sensitive chat IDs in DeliveryTarget.parse |
| `a147164d3c4c` | #23 | fix(slack): preserve per-user slash-command session isolation |
| `d05a87e68662` | #23 | fix(gateway): clear slack assistant thread status |
| `f903ceece034` | #26 | chore: add contributors to AUTHOR_MAP for Slack batch salvage |
| `585d6778da28` | #26 | fix: allow WebSocket connections from non-loopback IPs in --insecure mode (#18633) |
| `f98b5d00a49b` | #26 | fix: gateway systemd unit now retries indefinitely with backoff (#18639) |
| `97acd66b4c58` | #20 | fix(curator): authoritative absorbed_into on delete + restore cron skill links on rollback (#18671) (#18731) |
| `c73594fe4196` | #20 | fix(skills): rescan skill_commands cache when platform scope changes (#18739) |
| `e2cea6eeba36` | #20 | fix(gateway): include external_dirs skills in Telegram/Discord slash commands (#18741) |
| `c5e3a6fb5bb3` | #20 | fix(cli): decode .env as UTF-8 to avoid GBK crash on Windows |
| `98c98821ff1e` | #26 | chore(release): map CoreyNoDream email for AUTHOR_MAP |
| `699b3679bcaf` | #20 | fix(constants): warn once when get_hermes_home() falls back under an active profile (#18746) |
| `9bf260472bca` | #20 | fix(tools): deduplicate tool names at API boundary for Vertex/Azure/Bedrock |
| `2470434d6099` | #23 | fix(telegram): probe polling liveness after reconnect to detect wedged Updater |
| `8825e9044c26` | #20 | fix(discord): complete #18741 for /skill autocomplete and drop legacy 25x25 caps (#18745) |
| `6ec74aec0705` | #20 | fix(gateway): match disabled/optional skills by frontmatter slug, not dir name (#18753) |
| `10297fa23c98` | #23 | fix(discord): `/reload-skills` now refreshes the `/skill` autocomplete live (#18754) |
| `2ef1ad280bee` | #26 | fix: prefer ~/.hermes/.env over os.environ when seeding credential pool |
| `9c626ef8ea8b` | #26 | chore(release): map franksong2702 email for AUTHOR_MAP |
| `0a6865b328ee` | #20 | test(credential_pool): regression coverage for .env vs os.environ precedence |
| `292d2fb42fe3` | #23 | fix(discord): close old client before reconnect to prevent zombie websockets (#18187) |
| `e363ced3c395` | #20 | test(discord): regression coverage for zombie-websocket guard in connect() |
| `5eac6084bc78` | #20 | fix(discord): warn on 32-char clamp collisions in the /skill collector (#18759) |
| `7696ddc59eba` | #26 | fix(cli): robust paste file expansion and process_loop error handling (#17666) |
| `50f9f389ec1d` | #26 | chore(release): map ambition0802 email for AUTHOR_MAP |
| `1dce90893016` | #20 | fix(gateway): shutdown + restart hygiene (drain timeout, false-fatal, success log) (#18761) |
| `13f344c5ce2f` | #20 | fix(agent): try fallback providers at init when primary credential pool is exhausted (#17929) |
| `e444d8f29cea` | #26 | fix(gateway): config.yaml wins over .env for agent/display/timezone settings (#18764) |
| `38dd057e91dc` | #23 | fix(feishu): finalize remote document downloads inside httpx.AsyncClient context (#18502) |
| `762eb79f1e19` | #23 | fix(gateway): tighten httpx keepalive and close whatsapp typing-response leak (#18451) |
| `73bcd83dba7e` | #26 | chore(release): map beibi9966 email for AUTHOR_MAP |
| `af981227937f` | #20 | fix(auxiliary): propagate explicit_api_key to _try_openrouter() |
| `5d3be898a867` | #22 | docs(tts): mention xAI custom voice support (#18776) |
| `d409a4409c8f` | #20 | fix(model): avoid bedrock credential probe in provider picker |
| `4f37669170bb` | #20 | fix(tools): reconfigure enabled unconfigured toolsets |
| `e26f9b207041` | #20 | fix(acp): route Zed thoughts to reasoning callbacks |
| `ef9a08a872d1` | #20 | fix(acp): polish Zed context and tool rendering |
| `72c8037a24b5` | #20 | fix(acp): polish common tool rendering |
| `b294d1d0229f` | #20 | fix(acp): keep read-file starts compact |
| `eb612f55748d` | #20 | fix(acp): keep web extract rendering compact |
| `19854c7cd2f0` | #20 | Schedule ACP history replay and fence file output |
| `9987f3d82486` | #20 | fix(acp): compact Zed tool replay rendering |
| `a22465e07ab4` | #23 | fix(weixin): send_weixin_direct cross-loop session check |
| `9b5b88b5e028` | #26 | chore: add MottledShadow to AUTHOR_MAP |
| `457c7b76cd69` | #20 | feat(openrouter): add response caching support (#19132) |
| `c4c0e5abc2b5` | #26 | fix: After _clamp_command_names truncates skill names to fit the 32-cha… |
| `5d5b8912bece` | #20 | test: add tests for cmd_key preservation through name clamping |
| `19ba9e43b621` | #20 | fix(gateway/discord): require allowlist auth on slash commands |
| `c14bf441a313` | #26 | chore: add 0xyg3n noreply email to AUTHOR_MAP |
| `6c1322b9972c` | #23 | fix(slack): close previous handler in connect() to prevent zombie Socket Mode connections |
| `0a97ce6bff49` | #26 | chore: add nftpoetrist to AUTHOR_MAP |
| `f1e0292517c1` | #26 | fix(gateway): resume sessions after crash/restart instead of blanket suspend |
| `bf3239472ff1` | #26 | chore: add millerc79 to AUTHOR_MAP |
| `934103476f31` | #23 | fix(gateway): send /new response before cancel_session_processing to avoid race (#18912) |
| `7a22c639dc84` | #26 | chore: add shellybotmoyer to AUTHOR_MAP |
| `1148c4624173` | #23 | fix(gateway): correct ws scheme conversion for https urls |

