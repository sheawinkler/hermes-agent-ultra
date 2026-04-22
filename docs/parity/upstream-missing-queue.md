# Upstream Missing Patch Queue

Generated: `2026-04-22T08:01:15.807500+00:00`

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
| pending | 3798 |
| ported | 66 |
| superseded | 723 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `16274d5a82e9` | #26 | fix: Windows git 'unable to write loose object' + venv pip path |
| `ddae1aa2e97c` | #26 | fix: install.ps1 exits entire PowerShell window when run via iex |
| `1900e5238b3e` | #26 | fix: git clone fails on Windows with 'copy-fd: Invalid argument' |
| `83fa442c1bf7` | #26 | fix: use env vars for git windows.appendAtomically on Windows |
| `c9afbbac0b49` | #26 | feat: install to %LOCALAPPDATA%\hermes on Windows |
| `354af6cceedb` | #26 | chore: remove unnecessary migration code from install.ps1 |
| `4766b3cdb9d0` | #26 | fix: fall back to ZIP download when git clone fails on Windows |
| `535b46f8130c` | #26 | feat: ZIP-based update fallback for Windows |
| `f084538cb9ae` | #26 | Move vision items to GitHub issues (#314, #315) |
| `54909b0282e0` | #26 | fix(setup): improve shell config detection for PATH setup |
| `4f5ffb890959` | #26 | fix: NoneType not iterable error when summarizing at max iterations |
| `de0af4df6616` | #21 | refactor: enhance software-development skills with Hermes integration |
| `a1c25046a978` | #20 | fix(timezone): add timezone-aware clock across agent, cron, and execute_code |
| `ffec21236d21` | #23 | feat: enhance Home Assistant integration with service discovery and setup |
| `6a51fd23dfc8` | #21 | feat: add AgentMail skill for agent-owned email inboxes (#329) |
| `73f2998d48be` | #25 | fix: update setup wizard logic to handle terminal availability |
| `fa3d7b3d0348` | #26 | feat: add interactive setup for messaging platforms in gateway CLI |
| `1754bdf1e875` | #25 | docs: update AGENTS.md, README.md, and messaging.md to include interactive setup for messaging platforms |
| `fafb9c23bf76` | #26 | fix: strip emoji characters from menu choices in interactive setup |
| `556a132f2db3` | #26 | refactor: update platform status function to return plain-text strings |
| `b7821b6dc1b6` | #26 | enhance: improve gateway setup messaging and service installation prompts |
| `95e3f4b0017c` | #26 | refactor: enhance gateway service setup messaging and installation prompts |
| `1538be45de27` | #25 | fix: improve gateway setup messaging for non-interactive environments |
| `e39de2e75289` | #20 | fix(gateway): match _quick_key to _generate_session_key for WhatsApp DMs |
| `d8f10fa51576` | #26 | feat: implement allowlist feature for user access in gateway setup |
| `152e0800e627` | #26 | feat: add detailed setup instructions for Telegram, Discord, and Slack platforms |
| `f90a627f9afd` | #26 | fix(gateway): add missing UTF-8 encoding to file I/O preventing crashes on Windows |
| `87a16ad2e522` | #20 | fix(session): use database session count for has_any_sessions (#351) |
| `7f9777a0b045` | #26 | feat: add container resource configuration prompts in setup wizard |
| `3db3d6036836` | #20 | refactor: extract build_session_key() as single source of truth |
| `0ea6c343259a` | #24 | feat: add OpenThoughts-TBLite evaluation environment and configuration files |
| `ee7fde653149` | #24 | feat: add OpenThoughts-TBLite evaluation script |
| `c45aeb45b127` | #23 | fix(whatsapp): wait for connected status and log bridge output |
| `8d2d8cc728a0` | #26 | refactor: add exception handling and docstring to has_any_sessions |
| `70a0a5ff4a20` | #20 | fix: exclude current session from session_search results |
| `4805be011960` | #20 | fix: prevent --force from overriding dangerous verdict in should_allow_install |
| `34badeb19c80` | #23 | fix(whatsapp): initialize data variable and close log handle on error paths |
| `d3504f84aff4` | #20 | fix(gateway): use filtered history length for transcript message extraction |
| `b2a9f6beaa5a` | #26 | feat: enable up/down arrow history navigation in CLI |
| `e9ab711b667e` | #26 | Fix context overrun crash with local LLM backends (fixes #348) |
| `093acd72dd2e` | #20 | fix: catch exceptions from check_fn in is_toolset_available() |
| `8311e8984bb6` | #20 | fix: preflight context compression + error handler ordering for model switches |
| `4c7232941210` | #26 | feat: add backend validation for required binaries in setup wizard |
| `6f4941616d9a` | #26 | fix(gateway): include history_offset in error return path |
| `ff3a47915627` | #26 | fix: coerce session_id and data to string in process tool handler |
| `3e2ed18ad0dd` | #26 | fix: fallback to main model endpoint when auxiliary summary client fails |
| `405c7e08beb8` | #21 | feat: enhance ascii-art skill with pyfiglet and asciiart.eu search |
| `0dba3027c119` | #21 | feat: expand ascii-art skill with cowsay, boxes, toilet, image-to-ascii |
| `11a5a6472900` | #21 | feat: add emojicombos.com as primary ASCII art search source |
| `41adca4e772c` | #26 | fix: strip internal fields from API messages in _handle_max_iterations |
| `141b12bd39be` | #26 | refactor: clean up type hints and docstrings in session_search_tool |
| `d0d9897e81f0` | #26 | refactor: clean up transcription_tools after PR #262 merge |
| `078e2e4b19ef` | #26 | fix(cli): Ctrl+C clears input buffer before exiting |
| `2af2f148ab3f` | #21 | refactor: rewrite duckduckgo-search skill for accuracy and usability |
| `3221818b6e55` | #26 | fix: respect OPENAI_BASE_URL when resolving API key priority |
| `d400fb8b2310` | #25 | feat: add /update slash command for gateway platforms |
| `7d47e3b77696` | #26 | fix: pass stable task_id in CLI and gateway to preserve sandbox state across turns |
| `ca3337259561` | #26 | fix: pass task_id to _create_environment as well, to prevent cross-session state mixing |
| `11a7c6b11208` | #20 | fix: update mock agent signature to accept task_id after PR #419 |
| `b4b426c69d82` | #20 | test: add coverage for tee, process substitution, and full-path rm patterns |
| `a1767fd69c90` | #23 | feat(whatsapp): consolidate tool progress into single editable message |
| `1708dcd2b243` | #23 | feat: implement edit_message() for Telegram/Discord/Slack and fix fallback regression |
| `7d79ce92ac22` | #26 | Improve type hints and error diagnostics in vision_tools |
| `ada3713e777c` | #22 | feat: add documentation website (Docusaurus) |
| `82cb1752d95e` | #23 | fix(whatsapp): replace Linux-only fuser with cross-platform port cleanup |
| `87f4e4cb9b6c` | #26 | chore: remove Windows install options from landing page |
| `93d93fdea459` | #26 | feat: add gateway setup wizard and update steps to landing page |
| `15561ec425a7` | #26 | feat: add WebResearchEnv RL environment for multi-step web research |
| `e25ad79d5d85` | #20 | fix: use _max_tokens_param in max-iterations retry path |
| `30ff3959242d` | #26 | feat: add issue and PR templates |
| `c4e520fd6e55` | #26 | docs: add documentation & housekeeping checklist to PR template |
| `56dc9277d724` | #25 | ci: add test workflow for PRs and main branch |
| `d92266d7c048` | #25 | ci: pin tests to Python 3.11 only |
| `938499ddfbd3` | #26 | fix: add missing empty-content guard after think-block stripping in retry path |
| `71c0cd00e56f` | #21 | docs: fix spelling of 'publicly' |
| `16cb6d1a6e87` | #26 | fix(gateway): return response from /retry handler instead of discarding it |
| `e36c8cd49a34` | #24 | fix: add missing re.DOTALL flag to DeepSeek V3 tool call parser |
| `1e312c6582e9` | #24 | feat(environments): add Daytona cloud sandbox backend |
| `c43451a50b5c` | #26 | feat(terminal): integrate Daytona backend into tool pipeline |
| `690b8bb56341` | #26 | feat(cli): add Daytona config mapping and env var sync |
| `df61054a8490` | #26 | feat(cli): add Daytona to setup wizard, doctor, and status display |
| `435530018b14` | #24 | fix(daytona): resolve cwd by detecting home directory inside the sandbox |
| `ea2f7ef2f6a2` | #26 | docs(config): add Daytona disk limit hint and fix default cwd in example |
| `36214d14db03` | #26 | fix(cli): use correct visibility filter string in codex API model fetch |
| `d5efb82c7c54` | #20 | test(daytona): add unit and integration tests for Daytona backend |
| `ad57bf1e4bea` | #26 | fix(cli): use correct dict key for codex auth file path in status output |
| `1faa9648d3bb` | #24 | chore(daytona): cap the disk size to current maximum on daytona sandboxes |
| `577da79a472c` | #20 | fix(daytona): make disk cap visible and use SDK enum for sandbox state |
| `5279540bb4f1` | #26 | fix(daytona): add missing config mappings in gateway, CLI defaults, and config display |
| `3a41079fac7e` | #26 | fix(daytona): add optional dependency group to pyproject.toml |
| `4f1464b3af7d` | #24 | fix(daytona): default disk to 10GB to match platform limit |
| `efc7a7b95707` | #20 | fix(daytona): don't guess /root on cwd probe failure, keep constructor default; update tests to reflect this |
| `14a11d24b4b5` | #26 | fix: handle None args in build_tool_preview |
| `a6499b610760` | #20 | fix(daytona): use shell timeout wrapper instead of broken SDK exec timeout |
| `48e65631f641` | #26 | Fix auth store file lock for Windows (msvcrt) with reentrancy support |
| `dcba291d45d9` | #26 | Use pywinpty instead of ptyprocess on Windows for PTY support |
| `81986022b7bd` | #26 | Add explicit encoding="utf-8" to all config/data file open() calls |
| `d7d10b14cd51` | #22 | feat(tools): add support for self-hosted firecrawl |
| `9079a2781421` | #26 | fix: prompt box and response box span full terminal width on wide screens |
| `55b173dd033e` | #26 | refactor: move shutil import to module level |

