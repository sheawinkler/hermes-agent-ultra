# Upstream Missing Patch Queue

Generated: `2026-04-22T01:29:13.505547+00:00`

- Range: `main..upstream/main`; total commits tracked: `4559`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 1622 |
| #21 | GPAR-02 skills parity | 199 |
| #22 | GPAR-03 UX parity | 517 |
| #23 | GPAR-04 gateway/plugin-memory parity | 507 |
| #24 | GPAR-05 environments+parsers+benchmarks | 64 |
| #25 | GPAR-06 packaging/docs/install parity | 152 |
| #26 | GPAR-07 upstream queue backfill | 1498 |

| Disposition | Commit Count |
| --- | ---: |
| pending | 4183 |
| ported | 24 |
| superseded | 352 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `153cd5bb44ef` | #26 | Refactor skills tool integration and enhance system prompt |
| `8fb44608bfe4` | #21 | Update SKILL.md and related references to implement container binding for labeled shapes and arrows in Excalidraw |
| `abe925e21260` | #26 | Update hermes-discord toolset to enable full terminal access with safety checks |
| `ada0b4f131ba` | #23 | Enhance image handling in platform adapters |
| `137ce05324d0` | #26 | Add image generation tool to toolsets for messaging platforms |
| `07501bef14bf` | #26 | Add Project_notes.md — centralized status tracker for all side projects |
| `fc792a4be927` | #26 | Update Project_notes.md: grailed-embedding-search status and TODOs (June 2025) |
| `389ac5e017ed` | #26 | pass extrabody for agentloop to ban and allowlist providers on openrouter, control thinking, etc |
| `a291cc99cf70` | #26 | more extra kwarg support for provider selection etc on openrouter in agent rl envs and evals |
| `1b7bc299f373` | #24 | Enhance TerminalBench2 environment with task filtering due to incompat with modal and logging improvements |
| `f23856df8ef2` | #26 | Add kill_modal script to manage Modal applications and better handling of file and terminal tools |
| `f5be6177b231` | #23 | Add Text-to-Speech (TTS) functionality with multiple providers |
| `ed010752dd1f` | #26 | Update .env.example to use new Docker, Singularity, and Modal images for Python 3.11 with Node.js 20 support |
| `3099a2f53c85` | #26 | Add timestamp to active system prompt in AIAgent |
| `84718d183abb` | #26 | Add platform-specific formatting hints and identity for AIAgent |
| `586b0a7047ea` | #26 | Add Text-to-Speech (TTS) support with Edge TTS and ElevenLabs integration |
| `ff9ea6c4b1c6` | #26 | Enhance TTS tool to support platform-specific audio formats |
| `eb49936a60aa` | #25 | Update documentation and installation scripts for TTS audio formats |
| `5404a8fcd8a5` | #23 | Enhance image handling and analysis capabilities across platforms |
| `69aa35a51c3d` | #23 | Add messaging platform enhancements: STT, stickers, Discord UX, Slack, pairing, hooks |
| `2f34e6fd3017` | #26 | Update OpenAI configuration prompts for clarity and detail |
| `e0c9d495ef77` | #26 | Refine configuration migration process to improve user experience |
| `dd5fe334f3b4` | #26 | Refactor configuration handling to improve user experience |
| `0f58dfdea4e2` | #26 | Enhance agent response handling and transcript logging |
| `635bec06cbb2` | #26 | Update tool definitions handling in GatewayRunner |
| `60812ae0418d` | #25 | Enhance configuration checks and persona file creation in doctor and install scripts |
| `45a8098d3afe` | #26 | Remove browserbase SDK check and add Node.js and agent-browser validation in doctor script |
| `01a3a6ab0d2d` | #26 | Implement cleanup guard to prevent multiple executions on exit |
| `8117d0adabe3` | #26 | Refactor file operations and environment management in file_tools and terminal_tool |
| `2c7deb41f6f7` | #26 | Fix Modal backend not working from CLI |
| `a7609c97be5f` | #26 | Update docs to match backend key rename and CWD behavior |
| `48b5cfd0851e` | #26 | Add skip_context_files option to AIAgent for batch processing |
| `061fa7090720` | #25 | Add background process management with process tool, wait, PTY, and stdin support |
| `bdac541d1ee2` | #26 | Rename OPENAI_API_KEY to HERMES_OPENAI_API_KEY in configuration and codebase for clarity and to avoid conflicts. Update related documentation and error messages to reflect the new key name, ensuring backward compatibility with existing setups. |
| `ec59d71e6083` | #26 | Update PTY write handling in ProcessRegistry to ensure data is encoded as bytes before writing. This change improves compatibility with string inputs and clarifies the expected data type in comments. |
| `6731230d7340` | #26 | Add special handling for 'process' tool in _build_tool_preview function |
| `d0f82e6dcca6` | #26 | Removing random project notes doc |
| `e184f5ab3a51` | #26 | Add todo tool for agent task planning and management |
| `3b615b0f7a89` | #26 | Enhance tool previews in AIAgent and GatewayRunner |
| `1e316145724d` | #26 | Refactor tool activity messages in AIAgent for improved CLI output |
| `a7f52911e1c6` | #26 | Refactor CLI output formatting in AIAgent |
| `dfa3c6265c7e` | #26 | Refactor CLI input prompt and layout in HermesCLI |
| `54cbf30c1430` | #26 | Refactor dynamic prompt and layout in HermesCLI |
| `d7cef744ecc9` | #26 | Add autocomplete and multiline support in HermesCLI input |
| `d9a8e421a4a2` | #26 | Enhance multiline input handling in HermesCLI |
| `41608beb3585` | #26 | Update multiline input handling in HermesCLI |
| `50ef18644ba5` | #26 | Update multiline input instructions in HermesCLI |
| `225ae32e7aff` | #26 | Enhance CLI layout with floating completion menu |
| `9e85408c7bfd` | #25 | Add todo tool for task management and enhance CLI features |
| `d59e93d5e9c6` | #26 | Enhance platform toolset configuration and CLI toolset handling |
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
| `3191a9ba11d4` | #23 | feat: add new conversation command and enhance command handling |
| `d49af633f06a` | #26 | feat: enhance command execution with stdin support |
| `057d3e1810a2` | #26 | feat: enhance search functionality in ShellFileOperations |
| `d070b8698d39` | #26 | fix: escape file glob patterns in ShellFileOperations |
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

