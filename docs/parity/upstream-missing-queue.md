# Upstream Missing Patch Queue

Generated: `2026-04-22T08:14:02.133161+00:00`

- Range: `main..upstream/main`; total commits tracked: `4587`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 1631 |
| #21 | GPAR-02 skills parity | 200 |
| #22 | GPAR-03 UX parity | 525 |
| #23 | GPAR-04 gateway/plugin-memory parity | 507 |
| #24 | GPAR-05 environments+parsers+benchmarks | 64 |
| #25 | GPAR-06 packaging/docs/install parity | 152 |
| #26 | GPAR-07 upstream queue backfill | 1508 |

| Disposition | Commit Count |
| --- | ---: |
| pending | 398 |
| ported | 66 |
| superseded | 4123 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `cd59af17cc09` | #20 | fix(agent): silence quiet_mode in python library use |
| `175cf7e6bb4e` | #26 | fix: tighten quiet-mode salvage follow-ups |
| `c94d26c69bf5` | #20 | fix(cli): sanitize interactive command output |
| `e0171314030f` | #20 | feat(cron): add wakeAgent gate — scripts can skip the agent entirely |
| `1d1e1277e496` | #20 | fix(gateway): flush undelivered tail before segment reset to preserve streamed text (#8124) |
| `62ce6a38ae8d` | #23 | fix(gateway): cancel_background_tasks must drain late-arrivals (#12471) |
| `b668c09ab2e4` | #20 | fix(gateway): strip cursor from frozen message on empty fallback continuation (#7183) |
| `588333908c52` | #20 | fix(telegram): warn on docker-only media paths |
| `ff63e2e005eb` | #20 | fix: tighten telegram docker-media salvage follow-ups |
| `150382e8b790` | #20 | fix(gateway): stop typing loops on session interrupt |
| `8466268ca58f` | #23 | fix(gateway): keep typing loop overrides backward-compatible |
| `4b6ff0eb7fa2` | #20 | fix: tighten gateway interrupt salvage follow-ups |
| `4f0e49dc7bd0` | #26 | chore: add sgaofen to AUTHOR_MAP |
| `cc59d133dc52` | #23 | fix(feishu): split fenced code blocks in post payload |
| `a9debf10ffd6` | #23 | fix(feishu): harden fenced post row splitting |
| `957ca79e8ed2` | #23 | fix(feishu): drop dead helper and cover repeated fenced blocks |
| `66ee081dc181` | #21 | skills: move 7 niche mlops/mcp skills to optional (#12474) |
| `206a449b2991` | #23 | feat(webhook): direct delivery mode for zero-LLM push notifications (#12473) |
| `7fa01fafa557` | #21 | feat: add maps skill (OpenStreetMap + Overpass + OSRM, no API key) |
| `de491fdf0e4a` | #21 | chore: remove unit tests from maps skill |
| `ea0bd81b84e4` | #21 | feat(skills): consolidate find-nearby into maps as a single location skill |
| `a3b76ae36d37` | #26 | chore(attribution): add AUTHOR_MAP entry for Mibayy |
| `d5fc8a5e00df` | #20 | fix(tui): reject /model and agent-mutating slash passthroughs while running (#12548) |
| `c567adb58abb` | #20 | fix(tui): session.create build thread must clean up if session.close races (#12555) |
| `a521005fe5e5` | #23 | fix(discord): close two low-severity adapter races (#12558) |
| `a6fe5d08727c` | #20 | fix(tui-gateway): dispatch slow RPC handlers on a thread pool (#12546) |
| `ab6eaaff2610` | #26 | chore(tui-gateway): inline one-off RPC_POOL_WORKERS, compact _LONG_HANDLERS |
| `596280a40bc2` | #22 | chore(tui): /clean pass — inline one-off locals, tighten ConfirmPrompt |
| `393175e60ce1` | #26 | chore(tui-gateway): inline _run_and_emit — one-off wrapper, belongs inside dispatch |
| `d32e8d2ace98` | #22 | fix(tui): drain message queue on every busy → false transition |
| `923539a46b80` | #22 | fix: add nous-research/ui package |
| `045b28733e09` | #20 | fix(compression): resolve missing config attribute in feasibility check |
| `7bd1a3a4b151` | #20 | test(compression): cover real init feasibility override |
| `13294c2d1831` | #26 | feat(compression): summaries now respect the conversation's language |
| `bca03eab2080` | #20 | fix(model_switch): enumerate dict-format models in /model picker |
| `5a23f3291a2a` | #20 | fix(model_switch): section 3 base_url/model/dedup follow-up |
| `7e3b3565740b` | #23 | refactor(discord): slim down the race-polish fix (#12644) |
| `fd119a1c4a9a` | #20 | fix(agent): refresh skills prompt cache when disabled skills change |
| `3143d3233077` | #20 | feat(providers): add per-provider and per-model request_timeout_seconds config |
| `f1fe29d1c368` | #20 | feat(providers): extend request_timeout_seconds to all client paths |
| `c11ab6f64df6` | #20 | feat(providers): enforce request_timeout_seconds on OpenAI-wire primary calls |
| `611657487f5e` | #22 | docs(providers): call out Bedrock as not covered by request_timeout_seconds |
| `0a02fbd842bd` | #20 | fix(environments): prevent terminal hang when commands background children (#8340) |
| `f336ae3d7de8` | #20 | fix(environments): use incremental UTF-8 decoder in select-based drain |
| `2d54e17b8248` | #23 | fix(feishu): allow bot-originated mentions from other bots |
| `014248567b23` | #23 | fix(feishu): hydrate bot open_id for manual-setup users |
| `eb247e6c0aba` | #26 | chore: add bingo906 numeric qq email to AUTHOR_MAP |
| `023208b17a5f` | #26 | fix(agent): respect HTTP_PROXY/HTTPS_PROXY when using custom httpx transport |
| `d48d6fadff6a` | #20 | test(run_agent): pin proxy-env forwarding through keepalive transport |
| `ef73367fc521` | #20 | feat: add Discord server introspection and management tool (#4753) |
| `06845b6a0308` | #21 | feat(creative): add pixel-art-arcade and pixel-art-snes skills |
| `bbc8499e8c9d` | #21 | refactor(creative): consolidate pixel-art skills into single preset-based skill |
| `13febe60ca26` | #26 | chore(release): add dodo-reach to AUTHOR_MAP |
| `91eea7544ffe` | #21 | refactor(creative): promote pixel-art from optional to built-in skills |
| `4d0846b64053` | #26 | Fix Cloudflare 403s for openai-codex provider on server IPs |
| `cca327807932` | #20 | fix(codex): pin correct Cloudflare headers and extend to auxiliary client |
| `db60c982765c` | #26 | docs(memory): steer agents to save declarative facts, not instructions (#12665) |
| `60fd4b7d16c4` | #22 | fix: use grid/cell components |
| `d2c2e344691a` | #20 | fix(patch): catch silent persistence failures and escape-drift in tool-call transport (#12669) |
| `aa5bd0923214` | #20 | fix(tests): unstick CI — sweep stale tests from recent merges (#12670) |
| `3dea497b2068` | #20 | feat(providers): route gemini through the native AI Studio API |
| `d393104bad62` | #20 | fix(gemini): tighten native routing and streaming replay |
| `823b6d08ed1a` | #22 | fix: imports |
| `ddd28329ff54` | #20 | fix(tui): /model picker surfaces curated list, matching classic CLI (#12671) |
| `c1949e844b68` | #22 | fix: imports |
| `2f67ef92eba1` | #25 | ci: add path filters to Docker and test workflows, remove supply chain audit |
| `19db7fa3d1ff` | #25 | ci(security): narrow supply-chain-audit to high-signal patterns only |
| `a47f5d3ea2e3` | #25 | ci: bump test-job timeout from 10m to 20m (#12718) |
| `a3a49324052c` | #20 | fix(mcp-oauth): bidirectional auth_flow bridge + absolute expires_at (salvage #12025) (#12717) |
| `d50a9b20d27e` | #20 | terminal: steer long-lived server commands to background mode |
| `af53039dbc47` | #26 | chore(release): add etherman-os and mark-ramsell to AUTHOR_MAP |
| `abfc1847b7bc` | #20 | fix(terminal): rewrite `A && B &` to `A && { B & }` to prevent subshell leak |
| `d40a828a8bb5` | #21 | feat(pixel-art): add hardware palettes and video animation (#12725) |
| `424e9f36b0ff` | #20 | refactor: remove smart_model_routing feature (#12732) |
| `0d353ca6a89c` | #22 | fix(tui): bound retained state against idle OOM |
| `6f79b8f01daf` | #20 | fix(kimi): route temperature override by base_url — kimi-k2.5 needs 1.0 on api.moonshot.ai |
| `50d6799389a3` | #20 | fix: propagate kimi base-url temperature overrides |
| `5d01fc4e6f20` | #26 | chore(attribution): add taeng02@icloud.com → taeng0204 |
| `88185e7147ce` | #26 | fix(gemini): list Gemini 3 preview models in google-gemini-cli/gemini pickers (#12776) |
| `c9b833feb353` | #20 | fix(ci): unblock test suite + cut ~2s of dead Z.AI probes from every AIAgent |
| `ad4680cf74d4` | #20 | fix(ci): stub resolve_runtime_provider in cron wake-gate tests + shield update-check timeout test from thread race |
| `b2f8e231ddc2` | #20 | fix(test): test get_update_result timeout behavior, not result-value identity |
| `323e827f4aae` | #20 | test: remove 8 flaky tests that fail under parallel xdist scheduling (#12784) |
| `1cf1016e72fd` | #20 | fix(run_agent): preserve dotted Bedrock inference-profile model IDs (#11976) |
| `09195be9796c` | #22 | docs: repoint tui.md skin reference to features/skins.md |
| `48cb8d20b258` | #25 | Fix for broken docker build |
| `ca3a0bbc54c0` | #20 | fix(model-picker): dedup overlapping providers: dict and custom_providers: list entries |
| `728265265531` | #20 | fix(gateway): silence pairing codes when a user allowlist is configured (#9337) |
| `1ee3b79f1d8f` | #20 | fix(gateway): include QQBOT in allowlist-aware unauthorized DM map |
| `be3bec55bef2` | #26 | chore(release): add draix to AUTHOR_MAP |
| `52a972e9273c` | #20 | fix(gateway): namespace voice mode state by platform to prevent cross-platform collision (#12542) |
| `491cf25eefef` | #20 | test(voice): update existing voice_mode tests for platform-prefixed keys |
| `65a31ee0d544` | #20 | fix(anthropic): complete third-party Anthropic-compatible provider support (#12846) |
| `b53f74a4899f` | #20 | fix(auth): use ssl.SSLContext for CA bundle instead of deprecated string path (#12706) |
| `a4ba0754ed7d` | #20 | test: drop platform-dependent _resolve_verify test file |
| `35e7bf6b005a` | #20 | fix(models): validate MiniMax models against static catalog (#12611, #12460, #12399, #12547) |
| `6a228d52f707` | #23 | fix(webhook): validate HMAC signature before rate limiting (#12544) |
| `fc5fda5e381c` | #20 | fix(display): render <missing old_text> in memory previews instead of empty quotes (#12852) |
| `6c0c62595278` | #23 | fix(gateway): accept finalize kwarg in all platform edit_message overrides |
| `5157f5427f19` | #26 | chore(release): add jackjin1997 qq email to AUTHOR_MAP |

