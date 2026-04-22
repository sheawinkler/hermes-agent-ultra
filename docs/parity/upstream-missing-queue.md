# Upstream Missing Patch Queue

Generated: `2026-04-22T07:56:08.596483+00:00`

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
| pending | 4048 |
| ported | 66 |
| superseded | 473 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `a1838271285a` | #24 | feat: enhance README and improve environment configuration |
| `2bf96ad24461` | #26 | feat: add ephemeral prefill messages and system prompt loading |
| `a30b2f34ebc6` | #26 | feat: add landing page for Hermes Agent |
| `e049441d9343` | #26 | feat: add reasoning effort configuration for agent |
| `d64f62c2ef01` | #26 | feat: enhance spinner output handling in display module |
| `c100541f07d1` | #26 | refactor: remove direct stdout handling in spinner class |
| `681141a5265f` | #26 | fix: ansi escapes causing broken terminal cli output |
| `c1d9e9a28575` | #26 | refactor: improve stdout handling in KawaiiSpinner class |
| `cc6bea8b90b9` | #26 | feat: enhance session search tool with parent session resolution and parallel summarization |
| `fd76ff60acb4` | #26 | fix: improve stdout/stderr handling in delegate_task function |
| `99af12af3f6f` | #26 | chore: update landing page hero text for improved messaging |
| `6845852e827a` | #26 | refactor: update failure message handling in display module and add debug logging in code execution tool |
| `91907789af08` | #26 | refactor: remove temporary debug logging in code execution tool |
| `80b90dd0d9e1` | #26 | refactor: update landing page metadata for clarity and engagement |
| `9166d56f1713` | #26 | style: enhance landing page responsiveness and layout |
| `9ec4f7504be4` | #26 | Provide example datagen config scripts |
| `6d74d424d320` | #26 | refactor: update job execution configuration loading in scheduler |
| `6877d5f3b5c8` | #26 | docs: add note on message delivery in cronjob_tools |
| `41df8ee4f53b` | #26 | refactor: enhance interrupt handling in AIAgent class |
| `f64a87209d8f` | #26 | refactor: enhance session content handling in AIAgent and update TTS output path |
| `757d012ab5fc` | #21 | refactor: remove outdated skills and references from MLOps |
| `740dd928f769` | #21 | Release set of skills |
| `69d3d3c15aaa` | #26 | Hide Hermes model until next release with agentic capabilities |
| `3e311a009278` | #26 | Update banner image to new version |
| `b5dbf8e43df9` | #26 | Update model version in hermes_cli to use openai/gpt-5.3-codex |
| `21a59a4a7ca2` | #25 | refactor: improve SSH cloning process in install script |
| `cd66546e2449` | #26 | refactor: enhance install script output and command handling |
| `5a07e2640536` | #26 | fix: align threading docstring with implementation |
| `3c5bf5b9d8d6` | #26 | refactor: enhance error handling in user prompts |
| `d72b9eadece1` | #26 | More fixes for windoze |
| `8fc28c34ce96` | #20 | test: reorganize test structure and add missing unit tests |
| `cbff32585d80` | #26 | one more windoze fix? |
| `9a858b8d6743` | #26 | add identifier for openrouter calls |
| `55a0178490f1` | #26 | refactor: enhance configuration loading for GatewayRunner |
| `e3cb957a10a6` | #26 | refactor: streamline reasoning configuration checks in AIAgent |
| `609b19b63086` | #20 | Add OpenAI Codex provider runtime and responses integration (without .agent/PLANS.md) |
| `ce175d73722d` | #20 | Fix Codex Responses continuation and schema parity |
| `3ba8b15f13a9` | #26 | Tone down Codex docs and prompt wording |
| `7a3656aea21d` | #26 | refactor: integrate Nous Portal support in auxiliary client |
| `cbde8548f4b5` | #26 | Fix for gateway not using nous auth: issue #28 |
| `e63986b53487` | #20 | Harden Codex stream handling and ack continuation |
| `47f16505d2e0` | #20 | Omit optional function_call id in Responses replay input |
| `91bdb9eb2d8e` | #20 | Fix Codex stream fallback for Responses completion gaps |
| `74c662b63a8c` | #20 | Harden Codex auth refresh and responses compatibility |
| `b6d7e222c1f6` | #24 | Fix Docker backend failures on macOS |
| `0310170869aa` | #26 | Fix subagent auth: propagate parent API key to child agents |
| `f1311ad3dee4` | #21 | refactor: update Obsidian vault path handling |
| `95b6bd5df62b` | #26 | Harden agent attack surface: scan writes to memory, skills, cron, and context files |
| `9fc0ca0a724a` | #25 | add full support for whatsapp |
| `eb88474dd80d` | #26 | fix: strip emoji characters from menu labels in TerminalMenu |
| `e5bd25c73f66` | #26 | Fix: #41 |
| `5a569eb1b653` | #26 | fix: resolve .env and config paths from HERMES_HOME, not PROJECT_ROOT |
| `d2c932d3aceb` | #26 | add session resumption for cli with easy copy paste command |
| `3c1e31de3e3b` | #26 | Implement session continuation feature in CLI |
| `76badfed6360` | #25 | Enhance CLI documentation and functionality for session resumption |
| `9eb4a4a48163` | #26 | fix: gateway credential resolution, memory flush auth, and LLM_MODEL fallback |
| `9cc2cf324168` | #21 | Add youtube transcript collection skill: |
| `6c86c7c4a96e` | #21 | Add output format examples for YouTube content |
| `cf3236ed2793` | #26 | fix: resolve .env path from ~/.hermes/ in cli.py, matching run_agent.py pattern |
| `1b8eb85eeb83` | #26 | Add npm audit checks for Node.js packages in doctor.py |
| `f2891b70d026` | #26 | fix: respect HERMES_HOME env var in gateway and cron scheduler |
| `696e2316a861` | #26 | fix: respect HERMES_HOME and add encoding fallback in rl_cli.py |
| `9dc5615b9d86` | #26 | fix: use HERMES_HOME constant in doctor.py directory check |
| `688ccf05cbdd` | #25 | Format |
| `ebe25fefd6ad` | #25 | Add missing mkdir |
| `d372eb1f0e58` | #26 | feat: add uv.lock file for package management |
| `178658bf9fb2` | #20 | test: enhance session source tests and add validation for chat types |
| `cb92fbe749fb` | #21 | feat: add Notion block types reference documentation |
| `7a4241e4065e` | #21 | Co-authored-by: Dogila Developer <valeshera11@gmail.com> |
| `254aafb2650e` | #26 | Fix SystemExit traceback during atexit cleanup on Ctrl+C |
| `240f33a06fd4` | #24 | feat(docker): add support check for Docker's --storage-opt option |
| `fed9f06c4ed4` | #26 | fix: add SSH backend to terminal requirements check |
| `0ac3af8776d5` | #20 | test: add unit tests for 8 untested modules |
| `2efd9bbac47a` | #20 | fix: resolve symlink bypass in write deny list on macOS |
| `b699cf8c4843` | #20 | test: remove /etc platform-conditional tests from file_operations |
| `90ca2ae16b8d` | #20 | test: add unit tests for run_agent.py (AIAgent) |
| `3227cc65d14c` | #26 | fix: prevent false positives in recursive delete detection |
| `f5c09a3ababb` | #20 | test: add regression tests for recursive delete false positive fix |
| `0bb8d8faf562` | #25 | fix: prevent silent abort in piped install when interactive prompts fail (#69) |
| `96043a8f7e48` | #26 | fix(whatsapp): skip agent's own replies in bridge message handler |
| `f02f64723791` | #26 | fix(whatsapp): per-contact DM session isolation and user identity in context |
| `760fb2ca0efe` | #25 | feat(install): enhance installation script for build tools and interactive prompts |
| `bf9dd83c1053` | #26 | fix(cli): improve description extraction for toolsets |
| `de197bd7cb85` | #26 | fix(cli): prevent crash in save_config_value when model is a string |
| `c21b071e7702` | #26 | fix(cli): prevent paste detection from destroying multi-line input |
| `2c28d9f5604e` | #26 | fix(cli): respect explicit --max-turns value even when it equals default |
| `7f36259f8834` | #26 | fix(cli): show correct config file path in /config command |
| `f92875bc3e1c` | #26 | fix(cli): reduce spinner flickering under patch_stdout |
| `669e4d02975f` | #21 | add experimental google workspace command center skill |
| `ab4bbf2fb2f3` | #26 | feat: add Honcho AI-native memory integration |
| `1fd0fcddb274` | #26 | feat: integrate Honcho with USER.md memory system |
| `70d1abf81b6d` | #26 | refactor: run Honcho and USER.md in tandem |
| `1a97e8200070` | #26 | feat(cli): add /verbose slash command to toggle debug output at runtime |
| `715825eac38a` | #26 | fix(cli): enhance provider configuration check for environment variables |
| `a5ea272936a8` | #26 | refactor: streamline API key retrieval in transcription and TTS tools |
| `7c1f90045e98` | #25 | docs: update README and tools configuration for improved toolset management |
| `0a231c078364` | #26 | feat(config): synchronize terminal settings with environment variables |
| `f0458ebdb881` | #26 | feat(config): enhance terminal environment variable management |
| `58fce0a37bab` | #26 | feat(api): implement dynamic max tokens handling for various providers |
| `b267e3409212` | #26 | feat(cli): add auto-restart functionality for hermes-gateway service when updating |

