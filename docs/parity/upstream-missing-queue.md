# Upstream Missing Patch Queue

Generated: `2026-04-22T08:13:59.987394+00:00`

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
| pending | 898 |
| ported | 66 |
| superseded | 3623 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `fc6cb5b970f0` | #26 | fix: tighten AST check to module-level only |
| `ef04de3e9851` | #22 | docs: update tool-adding instructions for auto-discovery |
| `ba24f058ed34` | #26 | docs: fix stale docstring reference to _discover_tools in mcp_tool.py |
| `c5688e7c8ba4` | #20 | fix(gateway): break compression-exhaustion infinite loop and auto-reset session (#9893) |
| `8548893d1472` | #20 | feat: entry-level Podman support — find_docker() + rootless entrypoint (#10066) |
| `a8b7db35b217` | #20 | fix: interrupt agent immediately when user messages during active run (#10068) |
| `93fe4ead83bb` | #20 | fix: warn on invalid context_length format in config.yaml (#10067) |
| `50c35dcabe9f` | #26 | fix: stale agent timeout, uv venv detection, empty response after tools (#9051, #8620, #9400) |
| `9855190f23a2` | #26 | feat(compressor): smart collapse, dedup, anti-thrashing, template upgrade, hardening |
| `5d5d21556e12` | #20 | fix: sync client.api_key during UnicodeEncodeError ASCII recovery (#10090) |
| `772cfb6c4ec7` | #26 | fix: stale agent timeout, uv venv detection, empty response after tools, compression model fallback (#9051, #8620, #9400) (#10093) |
| `029938fbed2e` | #20 | fix(cli): defensive subparser routing for argparse bpo-9338 (#10113) |
| `9932366f3cac` | #20 | feat(doctor): add Command Installation check for hermes bin symlink |
| `da8bab77fb76` | #20 | fix(cli): restore messaging toolset for gateway platforms |
| `857b543543ab` | #22 | feat: add skill analytics to the dashboard |
| `df7be3d8aef6` | #26 | fix(cli): /model picker shows curated models instead of full catalog (#10146) |
| `03446e06bbfe` | #26 | fix(send_message): accept Matrix room IDs and user MXIDs as explicit targets |
| `180b14442f88` | #20 | test: add _parse_target_ref Matrix coverage for salvaged PR #6144 |
| `e69526be799e` | #20 | fix(send_message): URL-encode Matrix room IDs and add Matrix to schema examples (#10151) |
| `a4e1842f1217` | #20 | fix: strip reasoning item IDs from Responses API input when store=False (#10217) |
| `7b2700c9afca` | #26 | fix(browser): use 127.0.0.1 instead of localhost for CDP default (#10231) |
| `2546b7acea9b` | #23 | fix(gateway): suppress duplicate replies on interrupt and streaming flood control |
| `8bc9b5a0b4c6` | #21 | fix(skills): use `is None` check for coordinates in find-nearby to avoid dropping valid 0.0 values |
| `dedc4600dd31` | #21 | fix(skills): handle missing fields in Google Workspace token file gracefully instead of crashing with KeyError |
| `1c4d3216d385` | #20 | fix(cron): include job_id in delivery and guide models on removal workflow (#10242) |
| `4bcb2f2d2632` | #26 | feat(send_message): add native media attachment support for Discord |
| `47e6ea84bb36` | #20 | fix: file handle bug, warning text, and tests for Discord media send |
| `33ae40389071` | #23 | fix(gateway): fix matrix lingering typing indicator |
| `41e2d61b3fcc` | #23 | feat(discord): add native send_animation for inline GIF playback |
| `722331a57de9` | #26 | fix: replace hardcoded ~/.hermes with display_hermes_home() in agent-facing text (#10285) |
| `33c615504d72` | #22 | feat: add inline token count etc and fix venv |
| `cc15b55bb937` | #20 | chore: uptick |
| `9931d1d814a4` | #22 | chore: cleanup |
| `aa398ad65530` | #20 | fix(cron): preserve skill env passthrough in worker thread |
| `da448d4fce50` | #20 | test(cron): add regression test for credential_files ContextVar propagation (#10462) |
| `dee592a0b1c4` | #20 | fix(gateway): route synthetic background events by session |
| `2276b721410d` | #20 | fix: follow-up improvements for watch notification routing (#9537) |
| `f61cc464f0a1` | #20 | fix: include thread_id in _parse_session_key and fix stale parts reference |
| `d2f85383e874` | #23 | fix: change default OPENVIKING_ACCOUNT from root to default |
| `990030c26ed6` | #26 | feat: add contrib map |
| `8b167af66bba` | #23 | feat: add ov agent header |
| `0c30385be246` | #23 | chore: update doc |
| `5082a9f66ca7` | #23 | fix: wire agent/account/user params through _VikingClient |
| `f3ec4b3a1608` | #23 | Fix OpenViking integration issues: explicit session creation, better error logging |
| `7856d304f203` | #23 | fix(openviking): commit session on /new and context compression |
| `8275fa597a70` | #23 | refactor(memory): promote on_session_reset to base provider hook |
| `7cb06e3bb3b4` | #23 | refactor(memory): drop on_session_reset — commit-only is enough |
| `d1d425e9d0e0` | #26 | chore: add ZaynJarvis bytedance email to AUTHOR_MAP |
| `4b4b4d47bcbf` | #22 | feat: just more cleaning |
| `cb7b740e3288` | #22 | feat: add subagent details |
| `6391b46779e4` | #20 | fix: bound auxiliary client cache to prevent fd exhaustion in long-running gateways (#10200) (#10470) |
| `0d25e1c14609` | #26 | fix: prevent premature loop exit when weak models return empty after substantive tool calls (#10472) |
| `a418ddbd8b9e` | #20 | fix: add activity heartbeats to prevent false gateway inactivity timeouts (#10501) |
| `93f6f66872dc` | #20 | fix(interrupt): preserve pre-start terminal interrupts |
| `af4bf505b375` | #20 | fix: add on_memory_write bridge to sequential tool execution path (#10174) (#10507) |
| `2edbf155608a` | #23 | fix: enforce TTL in MessageDeduplicator + use yaml for gateway --config (#10306, #10216) (#10509) |
| `19142810edfd` | #20 | fix: /debug privacy — auto-delete pastes after 1 hour, add privacy notices (#10510) |
| `861efe274bbe` | #26 | fix: add ensure_ascii=False to all MCP json.dumps calls (#10234) (#10512) |
| `91980e351830` | #20 | fix: deduplicate memory provider tools to prevent 400 on strict providers (#10511) |
| `824c33729da3` | #20 | fix(session_search): coerce limit to int to prevent TypeError with non-int values (#10522) |
| `305a702e09db` | #20 | fix: /browser connect CDP override now takes priority over Camofox (#10523) |
| `c4674cbe2110` | #26 | fix: parse string schedules in cron update_job() (#10129) (#10521) |
| `22d22cd75c65` | #23 | fix: auto-register all gateway commands as Discord slash commands (#10528) |
| `a9197f9bb18c` | #23 | fix(memory): discover user-installed memory providers from $HERMES_HOME/plugins/ (#10529) |
| `e36c804bc2a6` | #20 | fix: prevent already_sent from swallowing empty responses after tool calls (#10531) |
| `b3b88a279b97` | #20 | fix: prevent stale os.environ leak after clear_session_vars (#10304) (#10527) |
| `407d27bd82ba` | #26 | feat: add SECURITY.md |
| `1b12f9b1d6ce` | #26 | docs: add terminal bypass test to Out of Scope section |
| `57e4b6115528` | #22 | feat: change to $ when in ! mode |
| `18396af31ede` | #26 | fix: handle cross-device shutil.move failure in tirith auto-install (#10127) (#10524) |
| `096260ce7852` | #23 | fix(telegram): authorize update prompt callbacks |
| `23f1fa22af4c` | #26 | fix(kimi): include kimi-coding-cn in Kimi base URL resolution (#10534) |
| `d4eba82a377a` | #26 | fix(streaming): don't suppress final response when commentary message is sent |
| `efd1ddc6e163` | #20 | fix: sanitize api_messages and extra string fields during ASCII-codec recovery (#6843) |
| `902f1e6ede20` | #26 | chore: add MestreY0d4-Uninter to AUTHOR_MAP and .mailmap |
| `93b6f4522479` | #20 | fix: always retry on ASCII codec UnicodeEncodeError — don't gate on per-component sanitization |
| `3b4ecf8ee70f` | #20 | fix: remove 'q' alias from /quit so /queue's 'q' alias works (#10467) (#10538) |
| `96cc556055f5` | #20 | fix(copilot): preserve base URL and gpt-5-mini routing |
| `ddaadfb9f077` | #26 | chore: add helix4u to AUTHOR_MAP |
| `f1df83179f77` | #20 | fix(doctor): skip health check for OpenCode Go (no shared /models endpoint) |
| `eb3d928da6a8` | #26 | chore: add counterposition to AUTHOR_MAP |
| `de3f8bc6cef8` | #20 | fix terminal workdir validation for Windows paths |
| `1d4b9c1a7400` | #20 | fix(gateway): don't treat group session user_id as thread_id in shutdown notifications (#10546) |
| `c9f78d110ad2` | #22 | feat: good vibes indi |
| `ee9c0a3ed07d` | #20 | fix(security): add JWT token and Discord mention redaction (#10547) |
| `f4724803b42d` | #20 | fix(runtime): surface malformed proxy env and base URL before client init |
| `21afc9502aa3` | #26 | fix: respect explicit api_mode for custom GPT-5 endpoints (#10473) (#10548) |
| `0cb8c51fa582` | #20 | feat: native AWS Bedrock provider via Converse API |
| `2918328009ca` | #26 | fix: show correct env var name in provider API key error (#9506) (#10563) |
| `2fbdc2c8faa2` | #20 | feat(discord): add channel_prompts config |
| `90a6336145cc` | #23 | fix: remove redundant key normalization and defensive getattr in channel_prompts |
| `620c296b1de2` | #20 | fix: discord mock setup and AUTHOR_MAP for channel_prompts tests |
| `0d05bd34f831` | #23 | feat: extend channel_prompts to Telegram, Slack, and Mattermost |
| `9d9b424390c4` | #20 | fix: Nous Portal rate limit guard — prevent retry amplification (#10568) |
| `c483b4cecaa9` | #26 | fix: use POSIX ps -A instead of BSD -ax for Docker compat (#9723) (#10569) |
| `e402906d48e9` | #21 | fix: five HERMES_HOME profile-isolation leaks (#10570) |
| `63d045b51af9` | #26 | fix: pass HERMES_HOME to execute_code subprocess (#6644) |
| `4fdcae6c91cd` | #26 | fix: use absolute skill_dir for external skills (#10313) (#10587) |
| `44941f0ed15b` | #23 | fix: activate WeCom callback message deduplication (#10305) (#10588) |
| `33ff29dfae3f` | #23 | fix(gateway): defer background review notifications until after main reply |

