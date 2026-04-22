# Upstream Missing Patch Queue

Generated: `2026-04-22T06:20:19.707790+00:00`

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
| pending | 4178 |
| ported | 49 |
| superseded | 360 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `ed010752dd1f` | #26 | Update .env.example to use new Docker, Singularity, and Modal images for Python 3.11 with Node.js 20 support |
| `586b0a7047ea` | #26 | Add Text-to-Speech (TTS) support with Edge TTS and ElevenLabs integration |
| `ff9ea6c4b1c6` | #26 | Enhance TTS tool to support platform-specific audio formats |
| `eb49936a60aa` | #25 | Update documentation and installation scripts for TTS audio formats |
| `5404a8fcd8a5` | #23 | Enhance image handling and analysis capabilities across platforms |
| `69aa35a51c3d` | #23 | Add messaging platform enhancements: STT, stickers, Discord UX, Slack, pairing, hooks |
| `2f34e6fd3017` | #26 | Update OpenAI configuration prompts for clarity and detail |
| `e0c9d495ef77` | #26 | Refine configuration migration process to improve user experience |
| `dd5fe334f3b4` | #26 | Refactor configuration handling to improve user experience |
| `0f58dfdea4e2` | #26 | Enhance agent response handling and transcript logging |
| `45a8098d3afe` | #26 | Remove browserbase SDK check and add Node.js and agent-browser validation in doctor script |
| `01a3a6ab0d2d` | #26 | Implement cleanup guard to prevent multiple executions on exit |
| `8117d0adabe3` | #26 | Refactor file operations and environment management in file_tools and terminal_tool |
| `2c7deb41f6f7` | #26 | Fix Modal backend not working from CLI |
| `a7609c97be5f` | #26 | Update docs to match backend key rename and CWD behavior |
| `ec59d71e6083` | #26 | Update PTY write handling in ProcessRegistry to ensure data is encoded as bytes before writing. This change improves compatibility with string inputs and clarifies the expected data type in comments. |
| `d0f82e6dcca6` | #26 | Removing random project notes doc |
| `e184f5ab3a51` | #26 | Add todo tool for agent task planning and management |
| `a7f52911e1c6` | #26 | Refactor CLI output formatting in AIAgent |
| `dfa3c6265c7e` | #26 | Refactor CLI input prompt and layout in HermesCLI |
| `54cbf30c1430` | #26 | Refactor dynamic prompt and layout in HermesCLI |
| `d7cef744ecc9` | #26 | Add autocomplete and multiline support in HermesCLI input |
| `d9a8e421a4a2` | #26 | Enhance multiline input handling in HermesCLI |
| `41608beb3585` | #26 | Update multiline input handling in HermesCLI |
| `50ef18644ba5` | #26 | Update multiline input instructions in HermesCLI |
| `225ae32e7aff` | #26 | Enhance CLI layout with floating completion menu |
| `9e85408c7bfd` | #25 | Add todo tool for task management and enhance CLI features |
| `14e59706b732` | #21 | Add Skills Hub — universal skill search, install, and management from online registries |
| `655303f2f1e0` | #26 | Add skill name resolution and enhanced install confirmation in Skills Hub |
| `440c244cac71` | #25 | feat: add persistent memory system + SQLite session store |
| `56ee8a5cc68a` | #26 | refactor: remove 'read' action from memory tool and agent logging |
| `a4bc6f73d77d` | #26 | refactor: simplify CLI layout by integrating inline completions |
| `ac0a70b3698a` | #26 | feat: enhance input area height adjustment in CLI |
| `37fb01b17d44` | #26 | feat: enhance conversation display with ANSI escape codes |
| `8e4d0131543e` | #26 | feat: improve ANSI text rendering in CLI |
| `21c3e9973ac7` | #26 | feat: enhance CLI output formatting with dynamic borders |
| `d0c8dd78c253` | #26 | fix: ensure proper output rendering in CLI by flushing stdout |
| `2daf5e4296a4` | #26 | fix: improve CLI output rendering and response display |
| `5c545e67f350` | #26 | feat: add styled border frame to input area in CLI |
| `0e8ee051c64a` | #26 | feat: replace framed input with horizontal rules in CLI |
| `109dffb2428b` | #26 | fix: refine dynamic height adjustment for input area in CLI |
| `3f4b494c616f` | #26 | refactor: streamline thinking spinner behavior in AIAgent |
| `4f57d7116d9f` | #26 | Improved stdout handling in the terminal tool to prevent deadlocks by implementing a background thread to continuously drain output, ensuring smooth command execution without blocking. |
| `b88e441a076a` | #26 | feat: implement cross-channel messaging functionality |
| `59cb0cecb214` | #25 | feat: add messaging gateway startup functionality |
| `53e13fe1f12c` | #26 | feat: add Slack and WhatsApp setup prompts in setup wizard |
| `4d5f29c74ca9` | #25 | feat: introduce skill management tool for agent-created skills and skills migration to ~/.hermes |
| `9350e26e681e` | #26 | feat: introduce clarifying questions tool for interactive user engagement |
| `748f0b2b5fc1` | #26 | feat: enhance clarify tool with configurable timeout and countdown display |
| `783acd712d6a` | #20 | feat: implement code execution sandbox for programmatic tool calling |
| `273b367f0511` | #26 | fix: update documentation and return types for web tools |
| `3b90fa5c9ba5` | #26 | fix: increase default timeout for code execution sandbox |
| `ba8b80a16314` | #26 | refactor: improve memory entry handling and file operations |
| `f9eb5edb9653` | #26 | refactor: rename search tool for clarity and consistency |
| `c0d412a736f1` | #26 | refactor: update search tool parameters and documentation for clarity |
| `90e521112876` | #25 | feat: implement subagent delegation for task management |
| `ba07d9d5e3a1` | #26 | feat: enhance task delegation with spinner updates and progress display |
| `c007b9e5bd19` | #25 | chore: update installer banner text for branding consistency |
| `cfef34f7a61f` | #25 | feat: add multi-provider authentication and inference provider selection |
| `f6daceb449c4` | #26 | feat: add interactive model selection and saving functionality |
| `77a3dda59d3a` | #25 | feat: enhance README and CLI with multi-provider model selection |
| `a3d760ff12fc` | #26 | feat: implement provider deactivation and enhance configuration updates |
| `24c241d29b3a` | #21 | add github project management skill |
| `5c4c0c0cbaf4` | #25 | feat: update branding and visuals across the project |
| `630bd3d78913` | #26 | feat: improve password prompt handling in terminal tool |
| `9a19fe1f5090` | #26 | chore: remove deprecated session viewer and exported data files |
| `70dd3a16dccd` | #20 | Cleanup time! |
| `c48817f69b22` | #26 | chore: update agent-browser dependency and clean up stale daemon processes |
| `b33ed9176ff8` | #26 | feat: update database schema and enhance message persistence |
| `5b3f708fcb44` | #26 | feat: enhance stale daemon cleanup and improve error logging in browser tool |
| `6903c4605ceb` | #26 | chore: update package-lock.json with new dependencies and version upgrades |
| `3dfc0a9679d6` | #21 | feat: add PPTX editing and creation skills with comprehensive documentation |
| `7283b9f6cf0c` | #26 | feat: extend browser session management with improved thread safety and timeout configuration |
| `a54a27595bf4` | #26 | fix: update browser command connection instructions to prevent session conflicts |
| `3976962621d5` | #25 | fix: update session logging directory path in README and code |
| `3555c6173d0f` | #26 | refactor: remove temporary API payload logging and enhance session log structure |
| `b6247b71b5a7` | #26 | refactor: update tool descriptions for clarity and conciseness |
| `a885d2f24029` | #26 | refactor: implement structured logging across multiple modules |
| `cbff1b818c30` | #20 | refactor: remove obsolete Nous API test scripts |
| `748fd3db8858` | #23 | refactor: enhance error handling with structured logging across multiple modules |
| `7ee7221af11f` | #26 | refactor: consolidate debug logging across tools with shared DebugSession class |
| `ecb430effeca` | #23 | refactor: enhance API interaction and message handling in AIAgent |
| `c98ee9852594` | #26 | feat: implement interactive prompts for sudo password and command approval in CLI |
| `bff37075f61e` | #26 | feat: enhance CLI input handling with password masking and placeholder text |
| `5c2926102bf8` | #26 | fix: improve placeholder handling and hint height in CLI |
| `8f6788474b0d` | #26 | feat: enhance logging in AIAgent for quiet mode |
| `0729ef7353c1` | #26 | fix: refine environment creation condition in terminal_tool |
| `7cb6427dea43` | #25 | refactor: streamline cron job handling and update CLI commands |
| `61349398828b` | #25 | refactor: deduplicate toolsets, unify async bridging, fix approval race condition, harden security |
| `08ff1c1aa8a4` | #26 | More major refactor/tech debt removal! |
| `9018e9dd70ce` | #26 | refactor: update tool registration and documentation |
| `9123cfb5dd4d` | #24 | Refactor Terminal and AIAgent cleanup |
| `51b95236f976` | #26 | refactor: move model metadata functions to agent/model_metadata.py |
| `b1f55e3ee578` | #25 | refactor: reorganize agent and CLI structure for improved clarity |
| `ededaaa87410` | #23 | Hermes Agent UX Improvements |
| `f072801f3862` | #26 | refactor: remove unused compression model variable in AIAgent |
| `e223b4ac096b` | #26 | Enhance agent guidance with memory and session search tools |
| `250b2ca01adf` | #26 | fix: update MEMORY_GUIDANCE for clarity |
| `df2ec585f1d3` | #26 | fix: clarify MEMORY_GUIDANCE phrasing |
| `3c6750f37b28` | #26 | feat: enhance memory management features in AIAgent and CLI |

