# Upstream Missing Patch Queue

Generated: `2026-04-22T08:13:53.944760+00:00`

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
| pending | 2398 |
| ported | 66 |
| superseded | 2123 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `0426bb745f0c` | #20 | fix: reset default SOUL.md to baseline identity text (#3159) |
| `26bfdc22b4d8` | #21 | feat: add godmode jailbreaking skill + docs (#3157) |
| `4a56e2cd88c3` | #26 | fix(display): show tool progress for substantive tools, not just "preparing" |
| `9989e579da21` | #26 | fix: add request timeouts to send_message_tool HTTP calls (#3162) |
| `43af094ae34d` | #26 | fix(agent): include tool tokens in preflight estimate, guard context probe persistence (#3164) |
| `36af1f3baf3f` | #23 | feat(telegram): Private Chat Topics with functional skill binding (#2598) |
| `c6fe75e99bc6` | #20 | fix(gateway): fingerprint full auth token in agent cache signature (#3247) |
| `2c719f0701b9` | #26 | fix(auth): migrate OAuth token refresh to platform.claude.com with fallback (#3246) |
| `62f8aa9b03ef` | #20 | fix: MCP toolset resolution for runtime and config (#3252) |
| `b7b3294c4a92` | #20 | fix(skills): preserve trust for skills-sh identifiers + reduce resolution churn (#3251) |
| `3a7907b27816` | #26 | fix(security): prevent zip-slip path traversal in self-update (#3250) |
| `b81d49dc450a` | #20 | fix(state): SQLite concurrency hardening + session transcript integrity (#3249) |
| `a8e02c7d4929` | #20 | fix: align Nous Portal model slugs with OpenRouter naming (#3253) |
| `76ed15dd4dec` | #20 | fix(security): normalize input before dangerous command detection (#3260) |
| `e9e7fb06835d` | #20 | fix(gateway): track background task references in GatewayRunner (#3254) |
| `41ee207a5ea6` | #20 | fix: catch KeyboardInterrupt in exit cleanup handlers (#3257) |
| `db241ae6cef5` | #20 | feat(sessions): add --source flag for third-party session isolation (#3255) |
| `3a86328847e4` | #23 | fix(gateway): add request timeouts to HA, Email, Mattermost, SMS adapters (#3258) |
| `243ee67529ff` | #23 | fix: store asyncio task references to prevent GC mid-execution (#3267) |
| `72250b5f62f4` | #22 | feat: config-gated /verbose command for messaging gateway (#3262) |
| `e5d14445efd5` | #20 | fix(security): restrict subagent toolsets to parent's enabled set (#3269) |
| `6610c377baef` | #23 | fix(telegram): self-reschedule reconnect when start_polling fails (#3268) |
| `95dc9aaa7563` | #20 | feat: add managed tool gateway and Nous subscription support |
| `148f46620f52` | #23 | fix(matrix): add backoff for SyncError in sync loop (#3280) |
| `bdccdd67a1c3` | #20 | fix: OpenClaw migration overwrites defaults and setup wizard skips imported sections (#3282) |
| `716e616d28ce` | #20 | fix(tui): status bar duplicates and degrades during long sessions (#3291) |
| `bde45f5a2adf` | #23 | fix(gateway): retry transient send failures and notify user on exhaustion (#3288) |
| `08fa326bb059` | #26 | feat(gateway): deliver background review notifications to user chat (#3293) |
| `0375b2a0d720` | #20 | fix(gateway): silence background agent terminal output (#3297) |
| `2d232c999115` | #20 | feat(cli): configurable busy input mode + fix /queue always working (#3298) |
| `3c57eaf7442b` | #26 | fix: YAML boolean handling for tool_progress config (#3300) |
| `18d28c63a795` | #23 | fix: add explicit hermes-api-server toolset for API server platform (#3304) |
| `60fdb58ce471` | #20 | fix(agent): update context compressor limits after fallback activation (#3305) |
| `f008ee1019b3` | #20 | fix(session): preserve reasoning fields in rewrite_transcript (#3311) |
| `ad764d351351` | #26 | fix(auxiliary): catch ImportError from build_anthropic_client in vision auto-detection (#3312) |
| `005786c55db8` | #20 | fix(gateway): include per-platform ALLOW_ALL and SIGNAL_GROUP in startup allowlist check (#3313) |
| `1519c4d477a1` | #26 | fix(session): add /resume CLI handler, session log truncation guard, reopen_session API (#3315) |
| `a8df7f996404` | #20 | fix: gateway token double-counting with cached agents (#3306) |
| `867eefdd9fa7` | #23 | fix(signal): track SSE keepalive comments as connection activity (#3316) |
| `22cfad157b8e` | #20 | fix: gateway token double-counting — use absolute set instead of increment (#3317) |
| `03396627a633` | #20 | fix(ci): pin acp <0.9 and update retry-exhaust test (#3320) |
| `3f95e741a77d` | #23 | fix: validate empty user messages to prevent Anthropic API 400 errors (#3322) |
| `58ca875e191e` | #20 | feat(gateway): surface session config on /new, /reset, and auto-reset (#3321) |
| `a2847ea7f0a6` | #23 | fix(gateway): add media download retry to Mattermost, Slack, and base cache (#3323) |
| `b8b1f24fd755` | #20 | fix: handle addition-only hunks in V4A patch parser (#3325) |
| `be416cdfa94e` | #20 | fix: guard config.get() against YAML null values to prevent AttributeError (#3377) |
| `75fcbc44ce89` | #20 | feat(telegram): auto-discover fallback IPs via DoH when api.telegram.org is unreachable (#3376) |
| `915df02bbf19` | #26 | fix(streaming): stale stream detector race causing spurious RemoteProtocolError |
| `b7bcae49c639` | #26 | fix: SQLite WAL write-lock contention causing 15-20s TUI freeze (#3385) |
| `41d9d0807847` | #23 | fix(telegram): fall back to no thread_id on 'Message thread not found' (#3390) |
| `5a1e2a307ae4` | #20 | perf(ttft): salvage easy-win startup optimizations from #3346 (#3395) |
| `eb2127c1dccc` | #20 | fix(cron): prevent recurring job re-fire on gateway crash/restart loop (#3396) |
| `e0dbbdb2c946` | #20 | fix: eliminate 'Event loop is closed' / 'Press ENTER to continue' during idle (#3398) |
| `8ecd7aed2c3b` | #20 | fix: prevent reasoning box from rendering 3x during tool-calling loops (#3405) |
| `cc4514076b89` | #26 | feat(nix): add suffix PATHs during nix build for more agent-friendliness (#3274) |
| `5127567d5dfc` | #20 | perf(ttft): cache skills prompt with shared skill_utils module (salvage #3366) (#3421) |
| `f57ebf52e9bc` | #23 | fix(api-server): cancel orphaned agent + true interrupt on SSE disconnect (salvage #3399) (#3427) |
| `fd8c465e423c` | #22 | feat: add Hugging Face as a first-class inference provider (#3419) |
| `fb46a90098e0` | #20 | fix: increase API timeout default from 900s to 1800s for slow-thinking models (#3431) |
| `6f11ff53ad2b` | #20 | fix(anthropic): use model-native output limits instead of hardcoded 16K (#3426) |
| `e4e04c200541` | #20 | fix: make tirith block verdicts approvable instead of hard-blocking (#3428) |
| `ab09f6b568a6` | #20 | feat: curate HF model picker with OpenRouter analogues (#3440) |
| `658692799dbb` | #20 | fix: guard aux LLM calls against None content + reasoning fallback + retry (salvage #3389) (#3449) |
| `8fdfc4b00c16` | #20 | fix(agent): detect thinking-budget exhaustion on truncation, skip useless retries (#3444) |
| `b6b87dedd4ac` | #26 | fix: discover plugins before reading plugin toolsets in tools_config (#3457) |
| `83043e9aa836` | #26 | fix: add timeout to subprocess calls in context_references (#3469) |
| `388fa5293d90` | #26 | fix(matrix): add missing matrix entry in PLATFORMS dict (#3473) |
| `03f24c1edd87` | #26 | fix: session_search fallback preview on summarization failure (salvage #3413) (#3478) |
| `15cfd2082083` | #20 | fix: cap context pressure percentage at 100% in display (#3480) |
| `09796b183b50` | #20 | fix: alibaba provider default endpoint and model list (#3484) |
| `290c71a707e1` | #20 | fix(gateway): scope progress thread fallback to Slack only (salvage #3414) (#3488) |
| `6ed974044499` | #23 | fix: prevent unbounded growth of _seen_uids in EmailAdapter (#3490) |
| `9d4b3e5470fb` | #20 | fix: harden hermes update against diverged history, non-main branches, and gateway edge cases (salvage #3489) (#3492) |
| `831e8ba0e5d9` | #20 | feat: tool-use enforcement + strip budget warnings from history (#3528) |
| `e295a2215acd` | #20 | fix(gateway): include user-local bin paths in systemd unit PATH (#3527) |
| `80a899a8e290` | #20 | fix: enable fine-grained tool streaming for Claude/OpenRouter + retry SSE errors (#3497) |
| `d313a3b7d752` | #26 | fix: auto-repair jobs.json with invalid control characters (#3537) |
| `411e3c153989` | #23 | fix(api-server): allow Idempotency-Key in CORS headers (#3530) |
| `455bf2e853a6` | #22 | feat: activate plugin lifecycle hooks (pre/post_llm_call, session start/end) (#3542) |
| `735ca9dfb20a` | #20 | refactor: replace swe-rex with native Modal SDK for Modal backend (#3538) |
| `df6ce848e9d1` | #20 | fix(provider): remove MiniMax /v1→/anthropic auto-correction to allow user override (#3553) |
| `be3929263392` | #26 | fix(cli): guard .strip() against None values from YAML config (#3552) |
| `be322efdf2a0` | #23 | fix(matrix): harden e2ee access-token handling (#3562) |
| `393929831e02` | #20 | fix(gateway): preserve transcript on /compress and hygiene compression (salvage #3516) (#3556) |
| `901494d72892` | #20 | feat: make tool-use enforcement configurable via agent.tool_use_enforcement (#3551) |
| `1d0a11936863` | #26 | fix(display): show reasoning before response when tool calls suppress content (#3566) |
| `558cc14ad91e` | #26 | chore: release v0.5.0 (v2026.3.28) (#3568) |
| `33c89e52ec37` | #23 | fix(whatsapp): add **kwargs to media sending methods to accept metadata (#3571) |
| `09ebf8b2526f` | #23 | feat(api-server): add /v1/health alias for OpenAI compatibility (#3572) |
| `327373289101` | #23 | fix(api-server): add CORS headers to streaming SSE responses (#3573) |
| `c0aa06f300e5` | #20 | fix(test): update streaming test to match PR #3566 behavior change (#3574) |
| `e97c0cb578ed` | #23 | fix: replace hardcoded ~/.hermes paths with get_hermes_home() for profile support |
| `49a49983e4e2` | #23 | feat(api-server): add Access-Control-Max-Age to CORS preflight responses (#3580) |
| `df1bf0a20903` | #23 | feat(api-server): add basic security headers (#3576) |
| `d6b4fa2e9f35` | #23 | fix: strip @botname from commands so /new@TigerNanoBot resolves correctly (#3581) |
| `ba3bbf5b5376` | #20 | fix: add missing mattermost/matrix/dingtalk toolsets + platform consistency tests (salvage #3512) (#3583) |
| `924857c3e374` | #20 | fix: prevent tool name/arg concatenation for Ollama-compatible endpoints (#3582) |
| `2dd286c1624c` | #26 | fix: write models.dev disk cache atomically (#3588) |
| `6893c3befca7` | #22 | fix(gateway): inject PATH + VIRTUAL_ENV into launchd plist for macOS service (#3585) |
| `d7c41f3cef59` | #23 | fix(telegram): honor proxy env vars in fallback transport (salvage #3411) (#3591) |

