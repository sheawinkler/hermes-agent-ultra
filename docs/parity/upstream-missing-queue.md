# Upstream Missing Patch Queue

Generated: `2026-04-21T21:50:18.247047+00:00`

- Range: `main..upstream/main`; total commits tracked: `4536`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 1615 |
| #21 | GPAR-02 skills parity | 194 |
| #22 | GPAR-03 UX parity | 507 |
| #23 | GPAR-04 gateway/plugin-memory parity | 507 |
| #24 | GPAR-05 environments+parsers+benchmarks | 64 |
| #25 | GPAR-06 packaging/docs/install parity | 152 |
| #26 | GPAR-07 upstream queue backfill | 1497 |

| Disposition | Commit Count |
| --- | ---: |
| pending | 4474 |
| ported | 12 |
| superseded | 50 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `1614c15bb112` | #26 | rate limits |
| `ab7293bed652` | #26 | don't log exit code !=0 as terminal failure |
| `6af6ff2a0a46` | #26 | updates for stability and speed |
| `4071ba29dac7` | #26 | Enhance batch processing and tool validation |
| `66daebe88f00` | #26 | Implement enhanced response handling and tool call validation in run_agent |
| `13d360030fe0` | #26 | Enhance tool normalization and API integration across modules |
| `b66c093316b9` | #26 | add default datagen example script |
| `6e3dbb8d8b68` | #26 | Enhance batch processing with progress tracking and update AIAgent for OpenRouter detection |
| `b32cc4b09dd3` | #26 | Refactor batch processing with rich progress tracking and update logging in AIAgent |
| `6eb76c7c1a37` | #26 | Enhance batch processing and image generation tools |
| `47555602d7f1` | #26 | Add mini-swe-agent runner and trajectory compressor |
| `ba19d530ad24` | #25 | Update environment configuration and enhance terminal tool integration |
| `b78076cac75f` | #26 | Enhance trajectory_compressor.py with new input options and sampling functionality |
| `54ca0997ee98` | #26 | Update .gitignore to include additional directories and files |
| `248acf715e0f` | #25 | Add browser automation tools and enhance environment configuration |
| `5438b64e32b9` | #26 | Add new shell scripts for various task runs |
| `4c05ef0ba8f0` | #26 | Enhance logging and tool initialization for improved performance |
| `f8846f85a142` | #26 | Add package.json and package-lock.json for project setup |
| `7ea17bb9576b` | #25 | Update environment configuration and enhance tool definitions |
| `771cf41fea1f` | #26 | Update environment configuration and enhance terminal tool integration |
| `e8c6135a9145` | #26 | Update documentation for project structure and tool integration |
| `8e8b6be690ec` | #26 | Add timeout configuration for trajectory processing |
| `f172f7d4aa14` | #21 | Add skills tools and enhance model integration |
| `b292192467e3` | #25 | Enhance documentation for skills system and project structure |
| `4b68d30b0e92` | #26 | Moved "architecture" dir to "docs" for clarity |
| `8e986584f44f` | #26 | Update .gitignore to include private keys and CLI config |
| `bc76a032ba29` | #26 | Add a claude code-like CLI |
| `c360da4f3531` | #25 | Enhance documentation for CLI and tool integration |
| `20f287547275` | #26 | Implement browser session inactivity timeout and cleanup |
| `32254d301023` | #26 | Add skills guidance to system prompts in run_agent.py |
| `8f5f99c22ab5` | #21 | Add new skills descriptions and enhance skills tool functionality |
| `9c8d707530c0` | #26 | Update .gitignore to include additional ignored files |
| `3db83b682411` | #26 | Revise TODO.md to introduce Subagent Architecture and Interactive Clarifying Questions Tool |
| `affc4e9a8fed` | #26 | Update TODO.md |
| `971ed2bbdf61` | #25 | Implement sudo support across terminal environments |
| `bbeed5b5d12d` | #25 | Enhance session logging and interactive sudo support |
| `9b4d9452ba11` | #25 | Add context compression feature for long conversations |
| `e114f09f70be` | #26 | Implement reasoning extraction and enhance assistant message handling |
| `c935a604f876` | #26 | Refactor TODO.md to reorganize task sections and update descriptions |
| `a3ba41fce21e` | #25 | Implement cron job management system for scheduled tasks (similar to OpenAI's Pulse but the AI can also schedule jobs) |
| `619c72e566fa` | #23 | Enhance CLI with multi-platform messaging integration and configuration management |
| `3488576bd873` | #26 | Update terminal configuration and enhance CLI model management |
| `da4167560f57` | #26 | Enhance terminal backend selection in setup wizard |
| `ef409c6a24f4` | #25 | Enhance repository cloning in install script |
| `aa6394e94fdf` | #26 | Update install script to support SSH and HTTPS repository URLs |
| `69a338610a7a` | #26 | Enhance repository cloning logic in install script |
| `e87bee9ccd42` | #26 | Refactor setup wizard for improved API key and provider configuration |
| `bbb5776763e4` | #26 | Enhance tool availability checks and user feedback in CLI |
| `fef504f03869` | #25 | Refactor configuration file management and improve user feedback |
| `3ee788dacc79` | #26 | Implement configuration migration system and enhance CLI setup |
| `ff776b57bf4f` | #25 | Remove outdated .cursorrules file and add comprehensive AGENTS.md documentation |
| `c9011fc7e192` | #25 | Add uninstall command to CLI and update documentation |
| `be91af7551f6` | #26 | Refactor TODO list and remove completed items |
| `76d929e17725` | #26 | Implement dangerous command approval system for terminal tool |
| `5d3398aa8a20` | #26 | Refactor terminal tool command approval process and enhance CLI feedback |
| `3e634aa7e4f5` | #26 | Update requirements and enhance environment variable loading in gateway |
| `17a5efb416b5` | #25 | Enhance messaging gateway configuration and security features |
| `7eac4ee9fe9f` | #26 | Update agent configuration for maximum tool-calling iterations |
| `a09b018bd50e` | #23 | Implement continuous typing indicator in message handling |
| `e7f0ffbf5d1e` | #26 | Add tool progress notifications for messaging channels |
| `9d9eea9ac970` | #25 | Enhance agent configuration and documentation for tool progress and working directory |
| `488deb04a4f9` | #23 | fix telegram, import asyncio |
| `221fb17c5e39` | #23 | Refine typing indicator behavior in message handling |
| `212460289b51` | #26 | Enhance skills tool to have an arg so it is more reliably called, and error handling in agent |
| `beeb7896e07e` | #23 | Refactor message handling and error logging in agent and gateway |
| `9bfe185a2e31` | #26 | Implement interrupt handling for agent and CLI input and persistent prompt line at bottom of CLI :) |
| `51a6b7d2b5dc` | #23 | Implement interrupt handling for message processing in GatewayRunner and BasePlatformAdapter |
| `f018999da978` | #26 | initial RL training tools and loop |
| `8380895ae31f` | #25 | Update README.md |
| `f6574978de39` | #25 | Add RL training configuration and tools |
| `12bbca95ecf4` | #26 | Add tinker-atropos submodule and update RL training tools |
| `3c0d0dba49f9` | #25 | Update RL tools and enhance configuration management |
| `5c3105b4376c` | #26 | Enhance RL test inference with WandB integration and real-time output streaming |
| `533c06426941` | #25 | Add file manipulation tools and enhance setup scripts |
| `ac797259232e` | #25 | Update dependencies and enhance installation scripts |
| `07b615e96ed4` | #24 | Add support for Atropos Agentic RL environments (requires branch tool_call_support in Atropos atm) |
| `c0494b3558df` | #26 | Update pyproject.toml to refine dependency management |
| `a478e4458567` | #26 | Increase max_token_length in TerminalTestEnv to 16000 for enhanced processing capacity |
| `a8809bbd3e4b` | #25 | Transition installation to uv for py version and speed to be easier to streamline |
| `d999d9876d9b` | #26 | Enhance async tool execution and error handling in Hermes agent for Atropos integration |
| `f12ea1bc027b` | #26 | Enhance BatchRunner and AIAgent with new configuration options, default model now opus 4.6, default summarizer gemini flash 3 |
| `dd70d57b9bc3` | #26 | Refactor BatchRunner and AIAgent for enhanced reasoning and tool management, improved tool definitions for fileops |
| `c441681dc2e4` | #26 | Update default model to 'anthropic/claude-opus-4.6' and refine terminal working directory settings |
| `192ce958c37d` | #26 | Enhance CLI command handling and introduce resource cleanup features |
| `7a11be9f3fdd` | #26 | Enhance browser tool functionality and cleanup process |
| `1b1307d0d120` | #26 | Implement Anthropic prompt caching for Claude models via OpenRouter |
| `e8343f2d870e` | #26 | Refactor Singularity environment for persistent container management |
| `35ad3146a8ab` | #24 | Add new environments and enhance tool context functionality |
| `ad042fdd68c0` | #24 | Update terminalbench_2 configuration for enhanced performance and evaluation |
| `5ec75e38b978` | #26 | Enhance tool execution and logging in HermesAgentLoop |
| `6b4a8d0b175c` | #26 | Add terminal configuration options and enhance environment setup |
| `ba3fea24f10c` | #24 | Enhance TerminalBench 2 configuration and evaluation handling |
| `999a28062d1f` | #26 | Implement graceful exit cleanup for terminal tool |
| `85e629e9154c` | #24 | Add cleanup functionality for orphaned sandboxes in TerminalBench2EvalEnv |
| `9b0f2a16ca90` | #26 | Enhance CLI functionality with retry and undo commands |
| `62ba69a29d4e` | #26 | Fix gateway exit code to enable systemd auto-restart on connection failure |
| `a32ad1a656f0` | #23 | Fix infinite interrupt loop in gateway by consuming pending messages with .pop() and clearing interrupt events before recursion |
| `140d609e0c8b` | #26 | Refine agent history conversion logic in GatewayRunner |
| `cfe2f3fe15d0` | #26 | Implement interrupt handling for long-running tool executions in AIAgent |
| `669545f5518c` | #21 | Add diagramming skills for Excalidraw |

