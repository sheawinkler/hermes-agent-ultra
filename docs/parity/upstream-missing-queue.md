# Upstream Missing Patch Queue

Generated: `2026-04-28T18:57:36.674661+00:00`

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
| pending | 68 |
| ported | 47 |
| superseded | 560 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `df485628ce02` | #26 | chore(release): map Readon's git email to GitHub login |
| `5ae07e7b5cca` | #26 | fix(session): gate stale "no Discord APIs" note on DISCORD_BOT_TOKEN |
| `591deeb9280e` | #26 | feat(session): inject Discord IDs block when discord tool is loaded |
| `b35d692f45d5` | #26 | chore(release): map ash@users.noreply.github.com to ash |
| `6e561ffa6d47` | #26 | fix(update): poll is-active instead of one-shot sleep(3) after gateway restart (#15639) |
| `5006b2204b32` | #26 | fix(update): honor RestartSec when polling for gateway respawn (#15707) |
| `9daa0620a6bd` | #26 | fix(agent): ordering fix in _copy_reasoning_content_for_api — cross-provider reasoning isolation |
| `88b65cc82a5f` | #26 | Update run_agent.py |
| `5ae608152ec4` | #26 | fix: remove has_reasoning guard — inject empty reasoning_content for DeepSeek/Kimi tool_calls unconditionally |
| `4c797bfae973` | #26 | fix(cli): accept Alt+G as Ctrl+G fallback in VSCode/Cursor terminals |
| `01cf2c65cc72` | #26 | chore(release): map iris@growthpillars.co to irispillars (#15825) |
| `5fac6c344051` | #26 | fix(cli): write editor draft to prompt.md so syntax highlighting works |
| `4d170134efef` | #26 | chore(release): map nerijusn76@gmail.com to Nerijusas (#15833) |
| `edce7522a51e` | #26 | chore(release): add AUTHOR_MAP entry for voidborne-d personal email |
| `dc5e02ea7fef` | #26 | feat(cli): implement hermes update --check flag (fixes #10318) |
| `ce0513dd2e82` | #26 | chore(release): map Feranmi10 personal email |
| `4c591c28193d` | #26 | chore(release): map fqsy1416@gmail.com to EKKOLearnAI |
| `3a7653dd1f0c` | #26 | feat: Add Azure Foundry provider with OpenAI/Anthropic API mode selection |
| `6ef3a47ce5c0` | #26 | fix: use Azure API key directly for Azure endpoints, bypass OAuth token priority chain |
| `d8e4c7214e1a` | #26 | fix: Azure Anthropic short-circuit in resolve_runtime_provider — bypass custom runtime when provider=anthropic + azure.com URL |
| `7bfa9442dea1` | #26 | fix: skip OAuth token refresh for Azure Anthropic endpoints — prevents ~/.claude/.credentials.json from overwriting Azure key mid-session |
| `c15064fa372c` | #26 | fix: pass api-version as default_query param, not in base_url — SDK was producing malformed URLs like /anthropic?api-version=.../v1/messages |
| `24b4b24d7946` | #26 | fix: preserve URL query params for Azure OpenAI and custom endpoints |
| `2511207cb088` | #25 | chore: revert docs |
| `67dcace41234` | #26 | docs(config): show options in comments for display settings (#16038) |
| `63bf7a29b6a3` | #26 | fix(run_agent): prevent reasoning_content regression in DeepSeek/Kimi tool-call replay |
| `c5196f1fc2f4` | #26 | chore(release): map focusflow.app.help@gmail.com to yes999zc |
| `9ef1ae138ab3` | #26 | fix(docker): don't chown config.yaml after gosu drop (#15865) (#16096) |
| `e3901d5b257d` | #26 | fix(run_agent): background review fork inherits parent's live runtime (#16099) |
| `8443998dc3bf` | #26 | fix(auth): resolve API keys from ~/.hermes/.env and credential_pool |
| `d7a346824626` | #26 | fix(prompts): replace [SYSTEM: with [IMPORTANT: to avoid Azure content filter |
| `20cb706e034e` | #26 | chore: extend [SYSTEM:→[IMPORTANT: rename + AUTHOR_MAP |
| `eaa7e2db670b` | #26 | feat(cli,tui): surface /queue, /bg, /steer in agent-running placeholder (#16118) |
| `d993a3f450aa` | #26 | fix(gateway): use /hermes sethome in onboarding hint on Slack |
| `ae7687cdc5e6` | #26 | chore(release): map zhiyanliu in AUTHOR_MAP |
| `897dc3a2bb30` | #25 | fix(install+update): add /usr/local/bin PATH guard for RHEL root non-login shells (#16191) |
| `878c196738ec` | #26 | chore(release): map hhhonzik in AUTHOR_MAP |
| `6a3102f9d469` | #26 | chore(release): map hhuang91 in AUTHOR_MAP |
| `edadeaf495c7` | #26 | chore(release): map Satoshi-agi and kunlabs in AUTHOR_MAP |
| `36e352afa73b` | #26 | preserve the original comment |
| `aa7b5acfcd47` | #26 | pass attribution check |
| `822b507a729c` | #26 | chore(release): map maxims-oss in AUTHOR_MAP |
| `755a2804247d` | #26 | chore(release): map Wang-tianhao in AUTHOR_MAP |
| `82f842277e8b` | #26 | perf(tui): profile harness gains --loop, --save, --compare |
| `5db6db891c5e` | #26 | chore(release): map ghostmfr in AUTHOR_MAP |
| `87477756fd40` | #26 | chore(release): map Ito-69 in AUTHOR_MAP |
| `2a0fc97c76b9` | #26 | chore(release): map mewwts in AUTHOR_MAP |
| `3b60abb6bb7e` | #26 | fix(sessions): delete on-disk transcript files during prune and delete (#3015) |
| `a01e767b249b` | #26 | fix(gateway): respect config.yaml slack.enabled when SLACK_BOT_TOKEN env var is set |
| `bdc1adf711dc` | #26 | chore(release): map haru398801, badgerbees, xnbi in AUTHOR_MAP |
| `f01e4402a97f` | #26 | chore(release): map georgeglessner in AUTHOR_MAP |
| `55e9329ee6f6` | #26 | feat(config): register bundled-skill API keys in OPTIONAL_ENV_VARS |
| `34eb1aaa9a80` | #26 | fix(update): use npm ci to stop rewriting package-lock on every update (#16295) |
| `77d4766602ef` | #26 | fix(gateway): clear pending model note on auto-reset paths too |
| `36b13709f528` | #26 | chore(release): map johnncenae in AUTHOR_MAP |
| `87610ce3808d` | #26 | fix(tools): coerce quoted use_gateway in image_gen UI detection |
| `ebad6d3f1e3a` | #26 | chore(release): map yoimexex@gmail.com -> Yoimex |
| `cb51baeceb84` | #26 | chore(release): map Tosko4 in AUTHOR_MAP |
| `16e243e067e5` | #26 | fix(timeouts): guard load_config() call against runtime exceptions |
| `366351b94dea` | #26 | refactor(timeouts): drop redundant ImportError in except clause |
| `91512b821074` | #26 | fix(whatsapp_identity): guard against path traversal and silent mapping errors |
| `6993e566badc` | #26 | fix(whatsapp_identity): pin identifier regex to ASCII, clarify it's defense-in-depth |
| `b288934dffcc` | #26 | fix(discord_tool): coerce limit parameter to int before min() call |
| `d308ae27e178` | #26 | fix(nix): refresh tui npm deps hash |
| `859e09b7ced2` | #26 | chore(release): map xiahu889889@proton.me to xiahu88988 |
| `3e68809fe0c5` | #26 | chore(release): map romanornr noreply email |
| `bda2dbc29edc` | #26 | fix(compressor): apply bare-string guard to protect-tail boundary scan |
| `a131c134bc6c` | #26 | chore(release): map BadTechBandit in AUTHOR_MAP |

