# Upstream Missing Patch Queue

Generated: `2026-04-21T21:28:38.469639+00:00`

- Range: `main..upstream/main`; total commits tracked: `4534`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 1614 |
| #21 | GPAR-02 skills parity | 194 |
| #22 | GPAR-03 UX parity | 506 |
| #23 | GPAR-04 gateway/plugin-memory parity | 507 |
| #24 | GPAR-05 environments+parsers+benchmarks | 64 |
| #25 | GPAR-06 packaging/docs/install parity | 152 |
| #26 | GPAR-07 upstream queue backfill | 1497 |

| Disposition | Commit Count |
| --- | ---: |
| pending | 4524 |
| ported | 10 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `21d80ca68346` | #26 | initital commit |
| `122d8788ae23` | #26 | terminal tool |
| `a49596cbb2c1` | #26 | terminal tool |
| `6d346250b18d` | #25 | readme |
| `bab9c75b5b40` | #26 | more detailed desc |
| `45d0b0b1427b` | #25 | change command |
| `10b4cfeace24` | #25 | fix history leakage |
| `bf4223f3818f` | #26 | implement first pass of scrape/crawl content compression |
| `cde7e64418e0` | #26 | add vision model tool, cli updates for exclusive and inclusive toolsets |
| `3078053795e8` | #26 | add mixture of agents tool |
| `ebb46ba0e6a7` | #26 | add image generation tool |
| `bc71dffd4cb7` | #26 | update requirements for fal image api |
| `e1710378b738` | #26 | update model_tools for imagen and moa |
| `f4ff1f496b25` | #26 | update gitignore |
| `58d5fa1e4cec` | #26 | update fal requirements |
| `96cff783357c` | #26 | cleanup |
| `4ece87efb0dd` | #26 | update to firecrawl |
| `587d1cf72095` | #26 | Fix Web Tools, Upgrade MoA to GPT5, Add Trajectory Saving |
| `c7fa4447b831` | #26 | cleanup |
| `17608c11422b` | #25 | Update to use toolsets and make them easy to create and configure |
| `2082c7caa308` | #26 | update gitignore |
| `c5386ed7e642` | #26 | add better logging when requests fail |
| `045a1737f899` | #26 | - message graphs |
| `066514e2a9be` | #26 | add more architecture docs |
| `e5e77381f0fb` | #26 | Made to be more descriptive from comments |
| `0411ca188099` | #25 | Add environment configuration file, restructure tool imports, and enhance README setup instructions |
| `a7ff4d49e94f` | #20 | A bit of restructuring for simplicity and organization |
| `c42d9055ed23` | #26 | Move test run back to repo root. weirdness occurred |
| `6fac6fecde92` | #26 | Enhance import handling for Hecate in terminal_tool.py to manage local folder shadowing and improve error reporting for import failures. |
| `bc5f0e62d9e6` | #25 | Add support for enabling all toolsets with 'all' or '*' alias in README and toolset resolution logic |
| `0e2e69a71dda` | #25 | Add batch processing capabilities with checkpointing and statistics tracking, along with toolset distribution management. Update README and add test scripts for validation. |
| `22b6d5866c10` | #26 | Fix some issues around async and tool constraints |
| `a398d320b7f3` | #26 | update gitignore |
| `d36790de9153` | #25 | Add ephemeral system prompt support in batch and agent runners. Update README with usage examples and documentation for the new feature. Ensure prompt is not saved to trajectories. |
| `8d256779d8fa` | #26 | Update vision_tools.py to include image downloading and base64 conversion features. |
| `de9c0edc515a` | #26 | some bugfixes |
| `faecbddd9b3e` | #26 | fix terminal interactivity |
| `a6ec79730cde` | #26 | terminal tool |
| `f6f75cbe2b5d` | #20 | update webtools |
| `0ca3e0aaa95c` | #26 | update snapshot |
| `a4db3fdee5b9` | #26 | fix leakage |
| `fbd3a2fdb88e` | #26 | prevent leakage of morph instances between tasks |
| `c82741c3d8da` | #20 | some cleanups |
| `d90fcd4e2b9d` | #26 | update gitignore |
| `c27787f09f9f` | #26 | fix gitignore again |
| `0fbc0475f3c9` | #26 | update snapshot id for ipython |
| `2d8f6c46f124` | #26 | log first 20 chars |
| `0c618482c408` | #26 | add logging of prefix of tool call and tool response |
| `f957ec226789` | #26 | update distribution and gitignore |
| `f81395975025` | #26 | add simple terminal |
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

