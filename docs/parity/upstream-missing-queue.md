# Upstream Missing Patch Queue

Generated: `2026-04-22T08:13:51.773890+00:00`

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
| pending | 2898 |
| ported | 66 |
| superseded | 1623 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `b72f522e30fb` | #20 | test: fake minisweagent for docker cwd mount regressions |
| `c51e7b4af784` | #20 | feat(privacy): redact PII from LLM context when privacy.redact_pii is enabled |
| `5479bb0e0cd7` | #26 | feat(gateway): streaming token delivery — StreamingConfig, GatewayStreamConsumer, already_sent |
| `9a423c348737` | #20 | fix(privacy): skip PII redaction on Discord/Slack (mentions need real IDs) |
| `2ba219fa4b96` | #20 | feat(cli): add file path autocomplete in the input prompt (#1545) |
| `99369b926c1b` | #20 | fix: always fall back to non-streaming on ANY streaming error |
| `57be18c02689` | #20 | feat: smart approvals + /stop command (inspired by OpenAI Codex) |
| `8e07f9ca560f` | #20 | fix: audit fixes — 5 bugs found and resolved |
| `9d1483c7e647` | #20 | feat(browser): /browser connect — attach browser tools to live Chrome via CDP |
| `447594be286e` | #20 | feat: first-class plugin architecture + hide status bar cost by default (#1544) |
| `1ecfe68675aa` | #26 | feat: improve memory prioritization + aggressive skill updates (inspired by OpenAI Codex) |
| `73f39a77614e` | #26 | feat(browser): auto-launch Chrome when /browser connect finds no debugger |
| `97990e7ad55d` | #20 | feat: first-class plugin architecture (#1555) |
| `71e35311f59f` | #26 | fix(browser): model waits for user instruction after /browser connect |
| `fc4080c58a4d` | #26 | fix(cli): add <THINKING> to streaming tag suppression list |
| `c0b88018eb8c` | #26 | feat: ship streaming disabled by default — opt-in via config |
| `23b9d88a763c` | #26 | docs: add streaming config to cli-config.yaml.example and defaults |
| `606f57a3ab7c` | #20 | fix(terminal): add Singularity/Apptainer preflight availability check |
| `43b8ecd172db` | #20 | fix(tests): use case-insensitive regex in singularity preflight tests |
| `d3687d3e817e` | #26 | docs: document planned live reasoning token display as future enhancement |
| `942950f5b9aa` | #26 | feat(cli): live reasoning token streaming — dim box above response |
| `5e5c92663dbf` | #20 | fix: hermes update causes dual gateways on macOS (launchd) (#1567) |
| `25a1f1867fa9` | #26 | fix(gateway): prevent message flooding on adapters without edit support |
| `d998cac319ec` | #20 | fix(anthropic): retry 429/529 errors and surface error details to users |
| `2158c44efdca` | #26 | fix: Anthropic OAuth compatibility — Claude Code identity fingerprinting (#1597) |
| `63635744bf2a` | #21 | Refactor ascii-video skill: creative-first SKILL.md, consolidate reference files |
| `181077b7859b` | #26 | fix: hide Honcho session line on CLI load when no API key configured (#1582) |
| `6794e79bb497` | #20 | feat: add /bg as alias for /background slash command (#1590) |
| `ce430fed4c49` | #25 | installer: clarify why sudo is needed at every prompt (#1602) |
| `60e38e82eca9` | #20 | fix: auto-detect D-Bus session bus for systemctl --user on headless servers (#1601) |
| `8d0a96a8bf7f` | #20 | fix: context counter shows cached token count in status bar |
| `673f13215115` | #20 | fix(gateway): Recover stale service state |
| `285300528bf9` | #20 | fix: isolate test_anthropic_adapter from local credentials |
| `474301adc6c1` | #26 | fix: improve execute_code error logging and harden cleanup (#1623) |
| `63e88326a804` | #26 | feat: Hermes-native PKCE OAuth flow for Claude Pro/Max subscriptions |
| `b79806250143` | #26 | fix: improve OAuth login UX for headless/SSH users |
| `46176c8029ce` | #20 | refactor: centralize slash command registry (#1603) |
| `bd3b0c712bf3` | #26 | fix: make OAuth login URL prominent for SSH/headless users |
| `19c8ad3d3d61` | #26 | fix: add Claude Code user-agent to OAuth token exchange/refresh requests |
| `e3f9894cafe9` | #23 | fix: send_animation metadata, MarkdownV2 inline code splitting, tirith cosign-free install (#1626) |
| `4768ea624d70` | #20 | fix: skip stale cron jobs on gateway restart instead of firing immediately |
| `3576f44a577f` | #22 | feat: add Vercel AI Gateway provider (#1628) |
| `d44b6b7f1b09` | #26 | feat(browser): multi-provider cloud browser support + Browser Use integration |
| `67546746d484` | #26 | fix(gateway): overwrite stale PID in gateway_state.json on restart |
| `37862f74fa1e` | #26 | chore: release v0.3.0 (v2026.3.17) |
| `634c1f67523a` | #20 | fix: sanitize corrupted .env files on read and during migration |
| `b6a51c955eec` | #20 | fix: clear stale ANTHROPIC_TOKEN during migration, remove false *** detection |
| `e9f1a8e39bfb` | #20 | fix: gate ANTHROPIC_TOKEN cleanup to config version 8→9 migration |
| `1c61ab6bd9ec` | #20 | fix: unconditionally clear ANTHROPIC_TOKEN on v8→v9 migration |
| `c16870277cd2` | #20 | test: add regression test for stale PID in gateway_state.json (#1631) |
| `4b96d10bc356` | #26 | fix(cli): invalidate update-check cache after hermes update |
| `949fac192f99` | #20 | fix(tools): remove unnecessary crontab requirement from cronjob tool (#1638) |
| `365d175100f2` | #26 | fix: apply MarkdownV2 formatting in _send_telegram for proper rendering |
| `19eaf5d9567c` | #20 | test: fix telegram mock to include ParseMode constant |
| `374411831153` | #20 | feat(cli): two-stage /model autocomplete with ghost text suggestions (#1641) |
| `4920c5940fe0` | #23 | feat: auto-detect local file paths in gateway responses for native media delivery (#1640) |
| `8e20a7e03519` | #23 | fix(gateway): strip MEDIA: and [[audio_as_voice]] tags from message body |
| `2d368195032f` | #21 | feat: add Base blockchain optional skill |
| `96dac22194d5` | #20 | fix: prevent infinite 400 loop on context overflow + block prompt injection via cache files (#1630, #1558) |
| `12afccd9caec` | #23 | fix(tools): chunk long messages in send_message_tool before dispatch (#1552) |
| `d7029489d6c6` | #26 | fix: show custom endpoint models in /model via live API probe (#1645) |
| `1f6a1f0028e1` | #26 | fix(tools): chunk long messages in send_message_tool before platform dispatch |
| `1b2d6c424cf4` | #20 | fix: add --yes flag to bypass confirmation in /skills install and uninstall (#1647) |
| `0351e4fa9000` | #23 | fix: add metadata param to base send_image and forward in send_animation |
| `4cb6735541db` | #20 | fix(approval): show full command in dangerous command approval (#1553) |
| `40e2f8d9f0df` | #26 | feat(provider): add OpenCode Zen and OpenCode Go providers |
| `7d91b436e47d` | #20 | fix: exclude hidden directories from find/grep search backends (#1558) |
| `68fbcdaa0659` | #20 | fix: add browser_console to browser toolset and core tools list (#1084) |
| `f2414bfd457d` | #20 | feat: allow custom endpoints to use responses API via api_mode override (#1651) |
| `49043b7b7d07` | #20 | feat: add /tools disable/enable/list slash commands with session reset (#1652) |
| `8992babaa393` | #26 | fix(cli): flush stdout during agent loop to prevent macOS display freeze (#1624) |
| `4e66d221511b` | #26 | fix(claw): warn when API keys are skipped during OpenClaw migration (#1580) |
| `766f4aae2b2f` | #20 | refactor: tie api_mode to provider config instead of env var (#1656) |
| `cb0deb5f9da5` | #21 | feat: add NeuTTS optional skill + local TTS provider backend |
| `6a320e8bfe60` | #20 | fix(security): block sandbox backend creds from subprocess env (#1264) |
| `2c7c30be69d0` | #20 | fix(security): harden terminal safety and sandbox file writes (#1653) |
| `60b67e2b476e` | #26 | fix(gateway): cap interrupt recursion depth to prevent resource exhaustion (#816) |
| `c8582fc4a2f1` | #23 | fix(discord): persist thread participation across gateway restarts |
| `d0faf77208d9` | #26 | fix(gateway): /model shows active fallback model instead of config default (#1615) |
| `9ece1ce2de7c` | #23 | feat(gateway): inject reply-to message context for out-of-session replies (#1594) |
| `693f5786acba` | #26 | perf: use ripgrep for file search (200x faster than find) |
| `e2e53d497fc0` | #26 | fix: recognize Claude Code OAuth credentials in startup gate (#1455) |
| `d50e0711c25c` | #21 | refactor(tts): replace NeuTTS optional skill with built-in provider + setup flow |
| `556e0f4b4326` | #22 | fix(docker): add explicit env allowlist for container credentials (#1436) |
| `6c6d12033fea` | #25 | fix: email send_typing metadata + ☤ Hermes staff symbol (#1431, #1420) |
| `35d948b6e185` | #22 | feat: add Kilo Code (kilocode) as first-class inference provider (#1666) |
| `342a0ad372c6` | #26 | fix(whatsapp): support LID format in self-chat mode (#1556) |
| `a3ac142c8329` | #26 | fix(core): guard print() calls in run_conversation() against OSError |
| `65be657a7913` | #21 | feat(skills): add Sherlock OSINT username search skill |
| `d9d937b7f7f4` | #26 | fix: detect Claude Code version dynamically for OAuth user-agent |
| `7042a748f577` | #26 | feat: add Alibaba Cloud provider and Anthropic base_url override (#1673) |
| `d15694241977` | #23 | fix(telegram): aggregate split text messages before dispatching (#1674) |
| `a1c81360a57d` | #20 | feat(cli): skin-aware light/dark theme mode with terminal auto-detection |
| `71c6b1ee992f` | #26 | fix: remove ANTHROPIC_BASE_URL env var to avoid collisions (#1675) |
| `ef67037f8ee5` | #23 | feat: add SMS (Telnyx) platform adapter |
| `fd61ae13e590` | #23 | revert: revert SMS (Telnyx) platform adapter for review |
| `1d5a39e00228` | #20 | fix: thread safety for concurrent subagent delegation (#1672) |
| `d9a7b83ae3dd` | #26 | fix: make _is_write_denied robust to Path objects (#1678) |
| `6020db024308` | #21 | feat: add inference.sh integration (infsh tool + skill) (#1682) |
| `30c417fe7092` | #20 | feat: add website blocklist enforcement for web/browser tools (#1064) |

