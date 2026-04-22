# Upstream Missing Patch Queue

Generated: `2026-04-22T06:27:44.061590+00:00`

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
| pending | 4148 |
| ported | 66 |
| superseded | 373 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
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
| `db23f51bc63a` | #26 | feat: introduce skills management features in AIAgent and CLI |
| `e1604b2b4abc` | #26 | feat: enhance user authorization checks in GatewayRunner |
| `6037b6a5abff` | #26 | Fix session saving to DB with full conversation history (not just user/assistant messages without tool calls) |
| `16d0aa7b4d01` | #26 | feat: enhance job delivery mechanism in scheduler |
| `e0ed44388f16` | #26 | fix: improve error messaging for chat ID and home channel configuration |
| `08e4dc256372` | #26 | feat: implement channel directory and message mirroring for cross-platform communication |
| `c7857dc1d406` | #26 | feat: enhance AIAgent's tool usage nudges and content handling |
| `90af34bc8336` | #24 | feat: enhance interrupt handling and container resource configuration |
| `d8a369e19405` | #20 | refactor: update API key checks in WebToolsTester |
| `8fedbf87d92e` | #20 | feat: add cleanup utility for test artifacts in checkpoint resumption tests |
| `d18c753b3ce0` | #26 | refactor: streamline scratchpad handling in AIAgent |
| `38db6e936660` | #26 | fix: correct toolset ID mapping in welcome banner |
| `4f9f5f70e397` | #26 | fix: handle missing toolset IDs in welcome banner |
| `224c900532b3` | #26 | refactor: update session loading method in SessionStore |
| `79f88317385d` | #26 | refactor: improve message source tagging in GatewayRunner |
| `674a6f96d36d` | #23 | feat: unify set-home command naming across platforms |
| `b3bf21db565f` | #26 | refactor: update environment variable configuration and add multi-select checklist for tool setup |
| `6447a6020cad` | #25 | feat: add Node.js installation support to the setup script |
| `4d1f2ea5228b` | #26 | refactor: remove unused multi_select_cursor_brackets_style in prompt_checklist function |
| `0858ee2f2701` | #26 | refactor: rename HERMES_OPENAI_API_KEY to VOICE_TOOLS_OPENAI_KEY |
| `cefe038a8718` | #26 | refactor: enhance environment variable configuration and setup wizard |
| `0edfc7fa49aa` | #26 | refactor: update tool progress environment variable defaults and improve setup wizard prompts |
| `f209a92b7ec1` | #26 | refactor: enhance setup wizard for messaging platform configuration |
| `98e3a26b2a2d` | #26 | refactor: update user prompt in setup wizard for item selection |
| `a9d16c40c7d8` | #26 | refactor: streamline API key prompt in setup wizard |
| `b103bb4c8bc0` | #26 | feat: add interactive tool configuration command |
| `d802db4de07b` | #26 | refactor: improve tool configuration prompts for clarity |
| `7a6d4666a2e7` | #26 | refactor: clarify user prompts in checklist interfaces |
| `75d251b81a26` | #26 | feat: add API key requirement checks for toolsets |
| `54dd1b3038fe` | #25 | feat: enhance README and update API client initialization |

