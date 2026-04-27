# Upstream Missing Patch Queue

Generated: `2026-04-27T07:48:07.794476+00:00`

- Range: `main..upstream/main`; total commits tracked: `675`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 242 |
| #21 | GPAR-02 skills parity | 21 |
| #22 | GPAR-03 UX parity | 181 |
| #23 | GPAR-04 gateway/plugin-memory parity | 42 |
| #25 | GPAR-06 packaging/docs/install parity | 6 |
| #26 | GPAR-07 upstream queue backfill | 183 |

| Disposition | Commit Count |
| --- | ---: |
| pending | 328 |
| ported | 38 |
| superseded | 309 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `023b1bff11c2` | #20 | fix(delegate): resolve subagent approval prompts without deadlocking parent TUI (#15491) |
| `e5647d7863d3` | #22 | docs: consolidate dashboard themes and plugins into Extending the Dashboard (#15530) |
| `5401a0080d97` | #20 | fix: recalculate token budgets on model switch in ContextCompressor |
| `df485628ce02` | #26 | chore(release): map Readon's git email to GitHub login |
| `9830905dabd5` | #20 | fix(tools): recover non-configurable toolsets from composite resolution |
| `81987f0350b6` | #20 | feat(discord): split discord_server into discord + discord_admin tools |
| `db09477b774c` | #20 | feat(feishu): wire feishu doc/drive tools into hermes-feishu composite |
| `0702231dd884` | #23 | feat(session): add guild_id/parent_chat_id/message_id to SessionSource |
| `47b02e961cb7` | #23 | feat(discord): populate guild_id, parent_chat_id, message_id on SessionSource |
| `5ae07e7b5cca` | #26 | fix(session): gate stale "no Discord APIs" note on DISCORD_BOT_TOKEN |
| `591deeb9280e` | #26 | feat(session): inject Discord IDs block when discord tool is loaded |
| `6ed37e0f42dc` | #20 | feat(tools): make discord/discord_admin opt-in, Discord-only |
| `b35d692f45d5` | #26 | chore(release): map ash@users.noreply.github.com to ash |
| `f92006ce1cda` | #20 | fix(compression): reserve system+tools headroom when aux binds threshold (#15631) |
| `ac05daa18902` | #20 | fix(tools): dedupe bundled plugin toolsets with built-in entries (#15634) |
| `6e561ffa6d47` | #26 | fix(update): poll is-active instead of one-shot sleep(3) after gateway restart (#15639) |
| `97d54f0e4df5` | #20 | fix(terminal): three-layer defense against watch_patterns notification spam (#15642) |
| `af22421e87a7` | #22 | feat(dashboard): page-scoped plugin slots for built-in pages (#15658) |
| `cf2fabc40fb6` | #22 | docs(dashboard): document page-scoped plugin slots (#15662) |
| `d635e2df3fd7` | #20 | fix(compression): pass provider to context length resolver in feasibility check |
| `ea01bdcebe1f` | #20 | refactor(memory): remove flush_memories entirely (#15696) |
| `7c8c031f60da` | #26 | feat: add `hermes -z <prompt>` one-shot mode (#15702) |
| `a9fa73a620db` | #26 | feat(oneshot): add --model / --provider / HERMES_INFERENCE_MODEL (#15704) |
| `5006b2204b32` | #26 | fix(update): honor RestartSec when polling for gateway respawn (#15707) |
| `7c17accb29bc` | #20 | fix: /stop now immediately aborts streaming retry loop |
| `9daa0620a6bd` | #26 | fix(agent): ordering fix in _copy_reasoning_content_for_api — cross-provider reasoning isolation |
| `e9c47c70422d` | #20 | fix(tui): honor launch model overrides |
| `57b43fdd4bf9` | #20 | fix(tui): preserve provider precedence on startup |
| `4db58d45d4e0` | #20 | fix(tui): address startup provider review |
| `2dfcc8087a8e` | #20 | fix(tui): avoid network lookup during startup |
| `e48a497d166b` | #20 | fix(tui): share static model detection |
| `5e52011de363` | #22 | fix(tui): bind provider as model alias |
| `48bdd2445e8e` | #22 | fix(tui): apply ui-tui fix pass and restore type-check |
| `fdcbd2257b2d` | #20 | fix(tui): resolve startup model aliases statically |
| `a046483e8601` | #22 | fix(tui): share overlay close controls |
| `c6fdf48b79eb` | #20 | fix(tui): sync inference model after switches |
| `6e83d90eb490` | #22 | refactor(tui): tighten overlay helpers |
| `919274b60ef0` | #22 | fix(tui): align overlay q shortcut casing |
| `bcc5362432de` | #22 | fix(tui): honor client copy shortcut over ssh |
| `a68793b6c4a9` | #22 | refactor(tui): share remote shell detection |
| `876bb600443c` | #22 | fix(tui): trim whitespace-only selection chrome |
| `132620ba3d23` | #22 | refactor(tui): simplify remote copy hotkey hints |
| `bba16943f650` | #22 | fix(tui): preserve rendered indentation in selections |
| `1735ced93b15` | #22 | fix(tui): preserve code block indentation in selection |
| `bd66e55a0245` | #22 | fix(tui): track rendered spaces for selection copy |
| `b1c18e5a41b8` | #22 | refactor(tui): format screen imports |
| `31d7f1951a55` | #22 | fix(tui): clamp copied selection bounds |
| `88b65cc82a5f` | #26 | Update run_agent.py |
| `5ae608152ec4` | #26 | fix: remove has_reasoning guard — inject empty reasoning_content for DeepSeek/Kimi tool_calls unconditionally |
| `47420a84b9dc` | #21 | docs(obliteratus): link YouTube video guide in SKILL.md (#15808) |
| `dc4d92f131ee` | #22 | docs: embed tutorial videos on webhooks + auxiliary models pages (#15809) |
| `ad0ac894783a` | #20 | fix: DeepSeek/Kimi thinking mode requires reasoning_content on ALL assistant messages |
| `3944b2250660` | #22 | fix(tui): suspend Ink properly when opening $EDITOR via Ctrl+G |
| `c58956a9a282` | #22 | fix(tui): accept Alt+G as Ctrl+G fallback in VSCode/Cursor terminals |
| `4c797bfae973` | #26 | fix(cli): accept Alt+G as Ctrl+G fallback in VSCode/Cursor terminals |
| `25ba6a4a7475` | #20 | fix(gateway): make reasoning session-scoped by default |
| `b2d3308f985f` | #20 | fix(doctor): accept bare custom provider |
| `01cf2c65cc72` | #26 | chore(release): map iris@growthpillars.co to irispillars (#15825) |
| `2c56dce0edec` | #26 | fix(model): preserve custom endpoint credentials and accept cloud models not in /v1/models |
| `5fac6c344051` | #26 | fix(cli): write editor draft to prompt.md so syntax highlighting works |
| `1fdc31b214d8` | #20 | fix(config): preserve custom provider api key refs |
| `8bbeaea6c74d` | #20 | fix(config): broaden api-key ref lookup to templated base_url |
| `db7c5735f070` | #22 | fix: prefer vim over nano for $EDITOR fallback (CLI + TUI) |
| `1b8ca9254f38` | #22 | fix(tui): save live transcript from slash command |
| `2536a36f6fab` | #22 | fix(tui): route /save through session.save JSON-RPC |
| `d056b610b797` | #22 | fix: avoid prompt_toolkit complex tempfile bug and prefer nvim first |
| `7fd8dc0bfbe8` | #22 | fix: preserve prompt_toolkit editor picker and mirror it in TUI |
| `81e01f6ee981` | #20 | fix(agent): preserve Codex message items for replay |
| `4d170134efef` | #26 | chore(release): map nerijusn76@gmail.com to Nerijusas (#15833) |
| `83129e72de7b` | #22 | refactor(tui): tighten editor handoff helpers |
| `45e1228a8a5a` | #20 | fix(cli): suppress OSError EIO on interrupt shutdown |
| `edce7522a51e` | #26 | chore(release): add AUTHOR_MAP entry for voidborne-d personal email |
| `1d80e92c7efa` | #20 | test(discord): add guild to fake e2e messages |
| `14dd8e9a727d` | #22 | fix(tui): address Copilot review on editor handoff |
| `dc5e02ea7fef` | #26 | feat(cli): implement hermes update --check flag (fixes #10318) |
| `ce0513dd2e82` | #26 | chore(release): map Feranmi10 personal email |
| `0a15dbdc435c` | #23 | feat(api_server): add POST /v1/runs/{run_id}/stop endpoint |
| `01535a4732a1` | #23 | fix(api_server): cap stop-run wait at 5s so interrupt can't hang handler |
| `4c591c28193d` | #26 | chore(release): map fqsy1416@gmail.com to EKKOLearnAI |
| `125de02056ea` | #20 | fix(context): honor custom_providers context_length on /model switch + bump probe tier to 256K (#15844) |
| `3a7653dd1f0c` | #26 | feat: Add Azure Foundry provider with OpenAI/Anthropic API mode selection |
| `6ef3a47ce5c0` | #26 | fix: use Azure API key directly for Azure endpoints, bypass OAuth token priority chain |
| `d8e4c7214e1a` | #26 | fix: Azure Anthropic short-circuit in resolve_runtime_provider — bypass custom runtime when provider=anthropic + azure.com URL |
| `7bfa9442dea1` | #26 | fix: skip OAuth token refresh for Azure Anthropic endpoints — prevents ~/.claude/.credentials.json from overwriting Azure key mid-session |
| `c15064fa372c` | #26 | fix: pass api-version as default_query param, not in base_url — SDK was producing malformed URLs like /anthropic?api-version=.../v1/messages |
| `24b4b24d7946` | #26 | fix: preserve URL query params for Azure OpenAI and custom endpoints |
| `ac571142841c` | #20 | fix(agent): support Azure OpenAI gpt-5.x on chat/completions endpoint |
| `731e1ef8cb69` | #20 | feat(azure-foundry): auto-detect transport, models, context length |
| `7c50ed707c42` | #22 | docs(azure-foundry): add provider guide, env vars, release AUTHOR_MAP |
| `91a7a0acbeaf` | #20 | fix(tui): restore skills search RPC |
| `a55de5bcd017` | #20 | feat(setup): auto-reconfigure on existing installs (#15879) |
| `eb28145f3682` | #20 | feat(approval): hardline blocklist for unrecoverable commands (#15878) |
| `59b56d445c34` | #22 | feat(hooks): add duration_ms to post_tool_call + transform_tool_result (#15429) |
| `db4e4acca0f7` | #22 | perf(tui): stabilize long-session scrolling |
| `14fcff60c93d` | #22 | style(tui): apply formatter |
| `458ce792d24e` | #22 | fix(tui): persist model switches by default |
| `19d75d179751` | #22 | perf(tui): coalesce composer echo updates |
| `9bb3bc422dcf` | #22 | perf(tui): optimistically echo simple input |
| `5cd41d2b3b1d` | #22 | perf(tui): widen native input echo |
| `ee7ef33b02f0` | #22 | fix(tui): queue busy submissions gracefully |

