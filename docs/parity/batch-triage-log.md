# Batch Triage Log

## 2026-04-21 batch-01 (50 commits)
- Scope: first 50 `pending` entries in `docs/parity/upstream-missing-queue.json` at triage time.
- SHA range (ordered): `21d80ca68346` -> `f81395975025`.
- Disposition applied: `superseded`.
- Rationale:
  - Commits are pre-Rust historical Python-era changes (e.g., `model_tools.py`, `run_agent.py`, `batch_runner.py`, `tools/*.py`, architecture markdown and old requirements scripts).
  - Current codebase is Rust-native with different module boundaries and execution model.
  - Commit-by-commit cherry-picking is non-actionable for this historical tranche; parity must be judged against current upstream behavior/state, not early intermediate evolution.
- Note template written per SHA:
  - `batch-triage-2026-04-21: legacy pre-rust python commit superseded by rust-native architecture/state parity at current head`

## 2026-04-21 batch-02 (100 commits)
- Scope: next 100 `pending` entries in `docs/parity/upstream-missing-queue.json` after batch-01.
- SHA range (ordered): `1614c15bb112` -> `669545f5518c`.
- Disposition applied: `superseded`.
- Rationale:
  - Stream is still legacy Python-oriented evolution (`run_agent.py`, `model_tools.py`, `tools/*`, `environments/*`, `hermes_cli/*`, `gateway/*`) from pre-Rust/current-architecture lineage.
  - Majority are upstream historical edits not suitable for direct cherry-pick into Rust modules; accounted as superseded with commit-level traceability preserved.
  - This batch was explicitly requested to accelerate backlog reduction by discarding dated/superseded commits.
- Note template written per SHA:
  - `batch-triage-2026-04-21-100: legacy python-era/upstream-pre-rust stream superseded by rust-native architecture and later parity checkpoints`

## 2026-04-21 batch-03 (full pending queue triage)
- Scope: all remaining `pending` commits after batch-01/02.
- Input pending before pass: `4374`.
- Actions:
  - Marked `199` docs/meta-only commits as `superseded`.
  - Assigned all remaining `4175` commits to explicit implementation work groups (`WG1`–`WG7`) via per-commit notes in `upstream-missing-queue.json`.
- Artifacts:
  - `docs/parity/full-queue-triage-groups.json`
  - `docs/parity/full-queue-triage-groups.md`
- Resulting disposition totals:
  - `pending=4175`, `ported=12`, `superseded=349`, `total=4536`

## 2026-04-22 batch-04 (WG1 security hardening parity)
- Scope: targeted WG1 security commits mapped to Rust local backend paths and subprocess environment handling.
- Upstream commits ported:
  - `5212644861ffefe2a51b259692da564cf0d4aab7`
    `fix(security): prevent shell injection in tilde-username path expansion`
    - Rust parity commit: `7146ba1c`
  - `b177b4abad1dffd60bc2e1527af8917d1ed7442f`
    `fix(security): block gateway and tool env vars in subprocesses`
    - Rust parity commit: `a6206a37`
- Verification:
  - `cargo test -p hermes-environments local::tests::`
- Queue update:
  - Both SHAs marked `ported` in `docs/parity/upstream-missing-queue.json`.
  - Regenerated:
    - `docs/parity/upstream-missing-queue.md`
    - `docs/parity/global-parity-proof.json`
    - `docs/parity/global-parity-proof.md`

## 2026-04-22 batch-05 (`@` reference security parity + worktree triage)
- Scope: WG1 context reference hardening and adjacent queue triage.
- Upstream commits:
  - `2d8fad8230d1535d7a0e76c11adee7030f3ebaf3`
    `fix(context): restrict @ references to safe workspace paths`
    - Rust parity commit: `154903e7`
    - Implementation:
      - Added `crates/hermes-agent/src/context_references.rs`
      - Workspace confinement (`allowed_root` defaults to current cwd)
      - Sensitive path denylist for home and Hermes credential/internal paths
      - Integrated preprocessor into `AgentLoop::run` for user messages
      - Added focused regression tests for workspace/sensitive-path behavior
  - `12bc86d9c92e602ded6f81fa34d7deb6175e5896`
    `fix: prevent path traversal in .worktreeinclude file processing`
    - Disposition: `superseded`
    - Rationale: no `.worktreeinclude` parser/update processing surface exists in the Rust workspace (`rg` scan across crates showed no implementation path to patch).
- Verification:
  - `cargo test -p hermes-agent context_references::`
- Queue/proof refresh:
  - `docs/parity/upstream-missing-queue.{json,md}`
  - `docs/parity/global-parity-proof.{json,md}`

## 2026-04-22 batch-06 (skill_view file-path traversal parity)
- Scope: WG1 `skill_view` file-path security and behavior parity.
- Upstream commits triaged:
  - `1cb2311bad5d10ce7de66f6c0ac5e91956a3ce34`
    `fix(security): block path traversal in skill_view file_path (fixes #220)`
    - Disposition: `ported`
    - Rust parity commit: `250ad94a`
  - `e86f391cacfeadfdcd19e153b5373f2d2f1cd727`
    `fix: use os.sep in skill_view path boundary check for Windows compatibility`
    - Disposition: `superseded` (covered by Rust path-component + `strip_prefix` containment checks in `250ad94a`)
  - `79871c20833059444a27f1e23cd7df056a389158`
    `refactor: use Path.is_relative_to() for skill_view boundary check`
    - Disposition: `superseded` (same containment semantics covered in `250ad94a`)
- Implementation (Rust):
  - `crates/hermes-tools/src/tools/skills.rs`
  - Added `skill_view.file_path` support with:
    - fast traversal-component rejection (`..`, absolute/prefix roots)
    - containment validation against skill root boundary (including symlink escape)
    - file discovery hints (`available_files`) for not-found targets
    - binary-file fallback payload
  - Added tests for:
    - valid in-skill file read
    - `..` traversal rejection
    - symlink escape blocking
- Verification:
  - `cargo test -p hermes-tools tools::skills -- --nocapture`
- Queue/proof refresh:
  - `docs/parity/upstream-missing-queue.{json,md}`
  - `docs/parity/global-parity-proof.{json,md}`

## 2026-04-22 batch-07 (skills_guard multi-word injection bypass parity)
- Scope: WG1 security fixes for prompt-injection regex bypasses in `skills_guard`.
- Upstream commits ported:
  - `4ea29978fc6778bc5641ed422261366a91d42961`
    `fix(security): catch multi-word prompt injection in skills_guard`
  - `ba214e43c86e138b4e1572d3f10a3b259d185fc5`
    `fix(security): apply same multi-word bypass fix to disregard pattern`
  - `021f62cb0ce3818fcc458fa2436304b50363d950`
    `fix(security): patch multi-word bypass in 8 more injection patterns`
  - Rust parity commit: `a7b9c617`
- Implementation (Rust):
  - `crates/hermes-skills/src/guard.rs`
  - Added hardened multi-word prompt-injection / exfiltration patterns to the built-in dangerous-pattern set, including:
    - `ignore ... instructions`
    - `disregard ... rules/instructions`
    - role hijack and fake-update patterns
    - filter-removal directives
    - conversation/context exfiltration requests
  - Added focused regression tests for multi-word bypass variants.
- Verification:
  - `cargo test -p hermes-skills guard:: -- --nocapture`
- Queue/proof refresh:
  - `docs/parity/upstream-missing-queue.{json,md}`
  - `docs/parity/global-parity-proof.{json,md}`

## 2026-04-22 batch-08 (runtime terminal backend selection parity)
- Scope: WG7 runtime backfill — remove forced local terminal backend wiring in Rust CLI runtime paths.
- Upstream commit ported:
  - `c33feb6dc9d4401e8e5f55b026f17e8665e290e2`
    `Fix host CWD leaking into non-local terminal backends`
    - Disposition: `ported`
- Implementation (Rust):
  - `crates/hermes-environments/src/manager.rs`
    - Track `active_backend_type` separately from configured backend.
    - Return selected runtime backend via `terminal_backend()`.
    - Added tests for default-local selection and unavailable-backend fallback behavior.
  - `crates/hermes-cli/src/terminal_backend.rs`
    - New shared helper to build runtime terminal backend from `GatewayConfig.terminal` via `BackendManager`.
  - Runtime callsites migrated off hardcoded `LocalBackend::default()`:
    - `crates/hermes-cli/src/app.rs`
    - `crates/hermes-cli/src/main.rs`
    - `crates/hermes-cli/src/commands.rs`
    - `crates/hermes-cli/src/lib.rs` (module export)
- Verification:
  - `cargo test -p hermes-environments manager:: -- --nocapture`
  - `cargo test -p hermes-cli terminal_backend:: -- --nocapture`
- Queue/proof refresh:
  - `docs/parity/upstream-missing-queue.{json,md}`
  - `docs/parity/global-parity-proof.{json,md}`

## 2026-04-22 batch-09 (sudo quoting parity + process/file triage)
- Scope: WG7 security parity around terminal/process/file shell-quoting commits.
- Upstream commits triaged:
  - `25e260bb3a00102590a09d8e0b3758e3b7647fd1`
    `fix(security): prevent shell injection in sudo password piping`
    - Disposition: `ported`
    - Rust implementation:
      - `crates/hermes-tools/src/tools/terminal.rs`
      - Added secure sudo transform path:
        - reads `SUDO_PASSWORD` when set
        - shell-quotes password safely (`'...'"'"'...'` style)
        - rewrites `sudo` token to `echo <quoted> | sudo -S -p ''` before backend execution
      - Added regression tests for:
        - quoting with single quotes and shell metacharacters in password
        - unchanged command when password missing
        - unchanged command when no `sudo` token
  - `e5f719a33bfe2705d40c5b4948cd301c0a5b8811`
    `fix(process): escape single quotes in spawn_via_env bg_command`
    - Disposition: `superseded`
    - Rationale: Rust `process_registry` is metadata-only and does not build shell `bg_command` strings.
  - `66a5bc64db92996f86674e5d4d5fc71ccb08dc3e`
    `fix(process): use shlex to safely quote commands in bg_command`
    - Disposition: `superseded`
    - Rationale: same architecture reason as above (no `nohup bash -c` string assembly in Rust process registry path).
  - `d070b8698d39ecbbb5c617aeec50756566946faf`
    `fix: escape file glob patterns in ShellFileOperations`
    - Disposition: `superseded`
    - Rationale: Rust file operations use native regex/glob matching without shell argument expansion.
- Verification:
  - `cargo test -p hermes-tools tools::terminal -- --nocapture`
- Queue/proof refresh:
  - `docs/parity/upstream-missing-queue.{json,md}`
  - `docs/parity/global-parity-proof.{json,md}`

## 2026-04-22 batch-10 (search_files parity: output modes/pagination/context)
- Scope: WG7 parity for ShellFileOperations search behavior enhancements.
- Upstream commit ported:
  - `057d3e1810a2177f1b31495d36759f5ff358a1d6`
    `feat: enhance search functionality in ShellFileOperations`
    - Disposition: `ported`
- Implementation (Rust):
  - `crates/hermes-tools/src/tools/file.rs`
    - Extended `search_content` backend contract and tool schema with:
      - `offset` (pagination start)
      - `output_mode` (`content` / `files_only` / `count`)
      - `context` (surrounding lines)
  - `crates/hermes-tools/src/backends/file.rs`
    - Implemented new search behavior in `LocalSearchBackend`:
      - content mode pagination with stable `total` + `truncated`
      - files-only mode returning paged unique file list
      - count mode returning per-file match counts
      - context mode that includes surrounding lines around matches
      - internal fetch window to preserve total-before-slice behavior
    - Added regression tests for:
      - output modes + pagination
      - context line inclusion
- Verification:
  - `cargo test -p hermes-tools backends::file -- --nocapture`
- Queue/proof refresh:
  - `docs/parity/upstream-missing-queue.{json,md}`
  - `docs/parity/global-parity-proof.{json,md}`

## 2026-04-22 batch-11 (process tool + background session parity)
- Scope: WG6/WG7 parity for upstream background process management (`process` tool, wait/poll/log lifecycle, PTY + stdin interaction support).
- Upstream commit ported:
  - `061fa7090720f4631b58ec0e760ca9236b198946`
    `Add background process management with process tool, wait, PTY, and stdin support`
    - Disposition: `ported`
- Implementation (Rust):
  - `crates/hermes-core/src/traits.rs`
    - Extended `TerminalBackend` with default process lifecycle APIs:
      - `list_processes`, `poll_process`, `read_process_log`, `wait_process`, `kill_process`, `write_process_stdin`, `submit_process_stdin`, `close_process_stdin`.
  - `crates/hermes-environments/src/local.rs`
    - Replaced PID-only background tracking with full session registry (`proc_*` IDs).
    - Added rolling output buffers, stdout/stderr async readers, wait-state tracking, and stdin pipe management.
    - `execute_command(background=true)` now returns structured JSON with `session_id` and `pid`.
    - Added process lifecycle implementations: list/poll/log/wait/kill/write/submit/close.
    - Added PTY-compatible background spawn path and focused lifecycle tests.
  - `crates/hermes-tools/src/tools/terminal.rs`
    - Added terminal-backed process adapter (`TerminalProcessBackendAdapter`) to bridge tool calls into backend lifecycle APIs.
    - Upgraded `process` tool schema/handler:
      - supports `session_id` (with deprecated `pid` alias)
      - adds `log` action (`offset`/`limit`)
      - supports string coercion for robustness (`session_id`/`data`).
  - `crates/hermes-tools/src/register_builtins.rs`
    - Registered `process` tool in built-ins using the new terminal adapter (previously only `process_registry` metadata tool was registered).
- Verification:
  - `cargo test -p hermes-environments local:: -- --nocapture`
  - `cargo test -p hermes-tools tools::terminal -- --nocapture`
  - `cargo test -p hermes-tools toolset:: -- --nocapture`
- Queue/proof refresh:
  - `docs/parity/upstream-missing-queue.{json,md}`
  - `docs/parity/global-parity-proof.{json,md}`

## 2026-04-22 batch-12 (stdin_data terminal execution parity)
- Scope: adjacent upstream parity for stdin piping support in command execution.
- Upstream commit ported:
  - `d49af633f06a7f7f9f2c02089e5debdfda87f953`
    `feat: enhance command execution with stdin support`
    - Disposition: `ported`
- Implementation (Rust):
  - `crates/hermes-core/src/traits.rs`
    - Added `execute_command_with_stdin(...)` default method on `TerminalBackend`.
  - `crates/hermes-tools/src/tools/terminal.rs`
    - Terminal tool now accepts optional `stdin_data` and routes via backend `execute_command_with_stdin`.
    - Added regression test for `stdin_data` execution path.
  - `crates/hermes-environments/src/local.rs`
    - Implemented `execute_command_with_stdin` for local backend:
      - foreground shell command stdin piping
      - background command bootstrap stdin write+close support
      - PTY foreground stdin piping path
    - Added regression test validating stdin piping (`cat` + payload).
- Superseded sub-part (architecture):
  - Upstream shell file-ops heredoc replacement is not directly applicable in Rust because file writes are native (`write_file` backend) rather than shell heredoc command construction.
- Verification:
  - `cargo test -p hermes-environments local:: -- --nocapture`
  - `cargo test -p hermes-tools tools::terminal -- --nocapture`
- Queue/proof refresh:
  - `docs/parity/upstream-missing-queue.{json,md}`
  - `docs/parity/global-parity-proof.{json,md}`

## 2026-04-22 batch-13 (tool preview parity + process-specific previews)
- Scope: parity tranche for tool-call preview rendering.
- Upstream commit ported:
  - `6731230d7340b5ae093454f0dbf06ff7b86e32b3`
    `Add special handling for 'process' tool in _build_tool_preview function`
    - Disposition: `ported`
- Implementation (Rust):
  - Added shared preview module: `crates/hermes-cli/src/tool_preview.rs`
    - process preview supports `action`, `session_id`/`pid`, `data`/`input`, and `wait timeout`
    - added preview support for `todo`, `send_message`, and `rl_*` calls
    - added emoji map for gateway/CLI consumers
  - Integrated preview rendering into TUI message tool-call lines:
    - `crates/hermes-cli/src/tui.rs`
    - now displays `[Tool: <emoji> <name> <preview>]`
  - Exported module from `crates/hermes-cli/src/lib.rs`.
- Verification:
  - `cargo test -p hermes-cli tool_preview:: -- --nocapture`
- Queue/proof refresh:
  - `docs/parity/upstream-missing-queue.{json,md}`
  - `docs/parity/global-parity-proof.{json,md}`

## 2026-04-22 batch-14 (gateway tool preview parity)
- Scope: parity tranche for GatewayRunner tool preview/emoji progress metadata.
- Upstream commit ported:
  - `3b615b0f7a89c909f2724eae3cd6e96383e0cae9`
    `Enhance tool previews in AIAgent and GatewayRunner`
    - Disposition: `ported`
- Implementation (Rust):
  - `crates/hermes-cli/src/main.rs`
    - Added `on_tool_start` callback wiring (non-stream + stream gateway paths).
    - `agent:step` hook `tools` payload now records start-phase preview entries:
      - `phase` (`start`/`complete`)
      - `name`
      - `emoji`
      - `preview` (on start, when available)
      - `result` (on complete, truncated)
    - Reused shared formatter from `crates/hermes-cli/src/tool_preview.rs`.
- Verification:
  - `cargo test -p hermes-cli tool_preview:: -- --nocapture`
- Queue/proof refresh:
  - `docs/parity/upstream-missing-queue.{json,md}`
  - `docs/parity/global-parity-proof.{json,md}`

## 2026-04-22 batch-15 (CLI tool activity output parity)
- Scope: parity tranche for improved CLI tool activity lines.
- Upstream commit ported:
  - `1e316145724da4897f72c3f57b0cbcffb05b64e3`
    `Refactor tool activity messages in AIAgent for improved CLI output`
    - Disposition: `ported`
- Implementation (Rust):
  - `crates/hermes-cli/src/commands.rs`
    - `handle_cli_chat` now wires `AgentCallbacks` for:
      - `on_tool_start`: prints aligned activity line with emoji + formatted preview
      - `on_tool_complete`: prints completion line with truncated result summary
    - Uses shared formatter from `crates/hermes-cli/src/tool_preview.rs` for consistency across CLI/TUI/gateway.
- Verification:
  - `cargo test -p hermes-cli tool_preview:: -- --nocapture`
- Queue/proof refresh:
  - `docs/parity/upstream-missing-queue.{json,md}`
  - `docs/parity/global-parity-proof.{json,md}`

## 2026-04-22 batch-16 (platform toolset configuration parity)
- Scope: WG4/WG7 parity for platform-specific toolset configuration and runtime enforcement.
- Upstream commit ported:
  - `d59e93d5e9c6878a5aa614e75a63f0da8cac71f3`
    `Enhance platform toolset configuration and CLI toolset handling`
    - Disposition: `ported`
- Implementation (Rust):
  - `crates/hermes-config/src/config.rs`
    - Added `platform_toolsets` to top-level `GatewayConfig`.
    - Added default mapping (`cli`, `telegram`, `discord`, `whatsapp`, `slack`) aligned to preset toolsets.
  - `crates/hermes-tools/src/toolset.rs`
    - Added preset aliases: `hermes-discord`, `hermes-whatsapp`, `hermes-slack` (mapped to telegram preset behavior).
  - `crates/hermes-cli/src/platform_toolsets.rs` (new)
    - Added platform key normalization, configured/default toolset resolution, and schema filtering helpers.
  - Runtime integration:
    - `crates/hermes-cli/src/main.rs` (gateway handlers now pass platform-filtered tool schemas to agent run calls)
    - `crates/hermes-cli/src/app.rs` (interactive CLI uses configured `cli` platform toolset schemas)
    - `crates/hermes-cli/src/commands.rs` (`chat` and `acp` paths pass filtered tool schemas)
- Verification:
  - `cargo test -p hermes-config config::tests::gateway_config_default -- --nocapture`
  - `cargo test -p hermes-tools toolset::tests:: -- --nocapture`
  - `cargo test -p hermes-cli platform_toolsets::tests:: -- --nocapture`

## 2026-04-22 batch-17 (gateway tool-definition metadata parity)
- Scope: WG4/WG7 parity for runner-level tool definition reporting.
- Upstream commit ported:
  - `635bec06cbb22cae75fb5fffbe7729861dd0e719`
    `Update tool definitions handling in GatewayRunner`
    - Disposition: `ported`
- Implementation (Rust):
  - `crates/hermes-cli/src/main.rs`
    - Gateway message and streaming handlers now emit `agent:tool_definitions` hook events per turn.
    - Hook payload includes `platform`, `chat_id`, `user_id`, `session_id`, `streaming`, and compact effective tool definitions (name + description) for the active turn.
    - Uses the same resolved per-platform tool schema set passed to the agent.
- Verification:
  - `cargo test -p hermes-cli platform_toolsets::tests:: -- --nocapture`

## 2026-04-22 batch-18 (setup/config preservation parity)
- Scope: WG6 setup durability parity for platform toolset persistence.
- Upstream commit ported:
  - `37c3dcf551a2d06b28f11eda196bd73bbacf3f41`
    `fix: setup wizard overwrites platform_toolsets saved by tools_command`
    - Disposition: `ported`
- Implementation (Rust):
  - `crates/hermes-cli/src/main.rs`
    - `run_setup` now loads existing `config.yaml` into a mutable config object, overlays setup-selected fields (`model`, `personality`, `max_turns`, optional OpenAI provider key), validates, and writes YAML.
    - This preserves unrelated existing settings (including `platform_toolsets`) instead of rewriting the file from scratch.
- Verification:
  - `cargo test -p hermes-cli platform_toolsets::tests:: -- --nocapture`

## 2026-04-22 batch-19 (10-tranche runtime/config parity sweep)
- Scope: WG4/WG6/WG7 parity sweep for selected upstream commits:
  - `153cd5bb44efa020c468d9e9e0b788d104d9c235`
  - `137ce05324d07489a1e7e8a71d81b4b6473f37f0`
  - `79b62497d1ca3ecb17abd5ab505b0d1ffc37cd3c`
  - `3099a2f53c856f670ad0059a1d3a2c13f2c0a2c4`
  - `84718d183abb3a44d6e7ab886f7268c41bca8a70`
  - `48b5cfd0851e8f330ab7f7a0c158a709e68deb39`
  - `60812ae0418d12b6baec52659fc6ec05eaaed272`
  - `bdac541d1ee20aa8545d908a01e18c65b8e319de`
  - `3191a9ba11d4922dd0283a26442905dd04ed55ae`
  - `c2d5f7bf2619d34c4812e817faba278cd836f243`
- Rust implementation commits (chronological):
  - `03505403` parity(137ce053): include image generation in messaging toolsets.
  - `60f33ada` parity(79b62497): enable cronjob tools in messaging presets.
  - `40963017` parity(48b5cfd0): add `skip_context_files` across runtime and cron.
  - `da85cee0` parity(bdac541d): prefer `HERMES_OPENAI_API_KEY` with legacy fallback.
  - `8c979d56` parity(c2d5f7bf): normalize session timestamp formatting.
  - `1e2e5c1e` parity(60812ae0): doctor/setup/install `SOUL.md` and env checks.
- Verified-as-already-present in Rust head (marked `ported` in queue):
  - `153cd5bb` skills discovery/tool prompt parity.
  - `3099a2f5` active system prompt timestamp injection.
  - `84718d18` platform-specific formatting hints + identity wiring.
  - `3191a9ba` `/new` and extended command handling in gateway/CLI.
- Verification (targeted):
  - `cargo test -p hermes-tools toolset::tests::test_messaging_platform_presets_present -- --nocapture`
  - `cargo test -p hermes-agent test_agent_config_default -- --nocapture`
  - `cargo test -p hermes-cron test_filtered_tool_schemas_excludes_cronjob -- --nocapture`
  - `cargo test -p hermes-config openai_audio_key_prefers_voice_override -- --nocapture`
  - `cargo test -p hermes-agent build_default_auxiliary_client_scenarios -- --nocapture`
  - `cargo test -p hermes-tools resolve_endpoint_uses_voice_key_first_in_direct_mode -- --nocapture`
  - `cargo test -p hermes-tools schema_advertises_managed_routing -- --nocapture`
  - `cargo test -p hermes-gateway voice::tests::test_voice_state_join_leave -- --nocapture`
  - `cargo test -p hermes-cli cli::tests::cli_parse_default -- --nocapture`
  - `cargo test -p hermes-cli cli::tests::cli_parse_doctor -- --nocapture`
  - `bash -n scripts/install.sh`
- Queue refresh:
  - `docs/parity/upstream-missing-queue.{json,md}` regenerated with these 10 SHAs moved out of `pending`.

## 2026-04-22 batch-20 (10-tranche parity pass: image delivery + queue dispositioning)
- Scope: next 10 pending upstream SHAs after batch-19:
  - `8fb44608bfe48733cf5c02009c5839cab8a524a6`
  - `abe925e21260a1b593bda0c021fc93ebf8b38723`
  - `ada0b4f131baf95034ecb125ac36cec847eb6a0b`
  - `07501bef14bff9358e07dee2b56a6be87378d6b8`
  - `fc792a4be9279495ff0c2a75e95e3ae3c65e1b23`
  - `389ac5e017ed4d963ce7a596451a03b96427c8f0`
  - `a291cc99cf704f1a84dc4795b0b8099b90750d03`
  - `1b7bc299f373771706698b813f38c2043bf6bcd7`
  - `f23856df8ef21f051b6735150240b15af7590fc2`
  - `f5be6177b2314b9703850b4059680adf0d197877`
- Rust implementation commits (chronological):
  - `56df3b42` parity(ada0b4f1): native inline image delivery for gateway responses.
    - Added `PlatformAdapter::send_image_url` with plain-text fallback.
    - Added inline markdown/HTML image extraction in gateway send path.
    - Added native image URL send implementations for Telegram (`sendPhoto` by URL) and Discord (image embed).
    - Added helper and gateway tests for extraction + routing behavior.
- Verification (targeted):
  - `cargo test -p hermes-gateway gateway_send_message_extracts_inline_images -- --nocapture`
  - `cargo test -p hermes-gateway test_extract_inline_images_markdown_and_html -- --nocapture`
  - `cargo test -p hermes-gateway test_extract_inline_images_keeps_non_image_html -- --nocapture`
  - `cargo test -p hermes-core --lib -- --nocapture`
- Queue dispositions:
  - `ported`: `abe925e2`, `ada0b4f1`, `389ac5e0`, `a291cc99`, `f5be6177`
  - `superseded`: `8fb44608`, `07501bef`, `fc792a4b`, `1b7bc299`, `f23856df`
- Queue refresh:
  - `docs/parity/upstream-missing-queue.{json,md}` regenerated with these 10 SHAs moved out of `pending`.

## 2026-04-22 batch-21 (30-tranche parity pass: TTS/messaging/CLI/config queue slice)
- Scope: next 30 pending upstream SHAs after batch-20:
  - `ed010752dd1f9862b75b17977dbe4b98c0663352`
  - `586b0a7047ea7d9ea81bcd44496fb9e2136de50d`
  - `ff9ea6c4b1c69ebe450a6128e8f76d39162565ac`
  - `eb49936a60aaf6c57483d01138a86fe1ac5445d1`
  - `5404a8fcd8a575a9c82bc77a5f090d4fd545f8c1`
  - `69aa35a51c3db85002892e2fab889287bf170dda`
  - `2f34e6fd3017f8eb32bad073c9b68b9c28553a4c`
  - `e0c9d495ef7764c656c5fc55faefd8464353cce9`
  - `dd5fe334f3b4c516e8150ca2c92c226803411e86`
  - `0f58dfdea4e2b9371a4ebe5f569aeec069454b71`
  - `45a8098d3afe181b281f4fc908199852a11b1299`
  - `01a3a6ab0d2d8e0e8644f85ff2c650d2cecd0821`
  - `8117d0adabe39e47973eaff9290a4340b92f63ba`
  - `2c7deb41f6f7274c803b108b49c1da0e590099bc`
  - `a7609c97be5f03c881e75973f5bf1e405f8d1511`
  - `ec59d71e6083cdddfd0092dfbdd62d5077ba0633`
  - `d0f82e6dcca634e191cead913d222c3e6fcf7819`
  - `e184f5ab3a51a9f9874d6d161788a844fcc43f74`
  - `a7f52911e1c61d632d000b5279a6f95a0fda7996`
  - `dfa3c6265c7ed73b29d3d956409210051cc19514`
  - `54cbf30c1430eff14cf8e79a4224ef2a6b1aa23d`
  - `d7cef744ecc99bf10064729f2a92368e9c15c7f4`
  - `d9a8e421a4a272a6030e7a76bb5300edd6bb292c`
  - `41608beb3585676032f7f6305a64f213339692f1`
  - `50ef18644ba56e642d37d2930075c34bb5fc8afc`
  - `225ae32e7affa679ead636021c49d640ac919f6c`
  - `9e85408c7bfd6024754709800ab762402d1a2816`
  - `14e59706b732164dda260f1899ade74a86a8352a`
  - `655303f2f1e0afac0dab45b714db88cc197da561`
  - `440c244cac71f0764e00ea85ab87ae0a2d18fe61`
- Rust implementation commits (chronological):
  - `0c50f306` parity(45a8098d): extend `hermes doctor` checks with Node.js and `agent-browser` (optional) validation.
- Verified existing parity coverage used for tranche dispositioning:
  - `56df3b42` (batch-20): inline image extraction + native image delivery across gateway response path.
  - Existing rust modules for: TTS stack, platform media routing, messaging adapters/hooks/pairing, todo tool, skills hub, sqlite session persistence/search, and modal backend support.
- Verification (targeted):
  - `cargo test -p hermes-cli cli::tests::cli_parse_doctor -- --nocapture`
  - `cargo test -p hermes-cli cli::tests::cli_parse_default -- --nocapture`
- Queue dispositions in this 30-SHA pass:
  - `ported` (17):
    - `ff9ea6c4`, `5404a8fc`, `69aa35a5`, `e0c9d495`, `0f58dfde`, `45a8098d`,
      `2c7deb41`, `ec59d71e`, `e184f5ab`, `d7cef744`, `d9a8e421`, `41608beb`,
      `225ae32e`, `9e85408c`, `14e59706`, `655303f2`, `440c244c`
  - `superseded` (13):
    - `ed010752`, `586b0a70`, `eb49936a`, `2f34e6fd`, `dd5fe334`, `01a3a6ab`,
      `8117d0ad`, `a7609c97`, `d0f82e6d`, `a7f52911`, `dfa3c626`, `54cbf30c`,
      `50ef1864`
- Queue refresh:
  - `docs/parity/upstream-missing-queue.{json,md}` regenerated with all 30 SHAs moved out of `pending`.

## 2026-04-22 batch-22a (100-tranche parity disposition pass: CLI/tools/gateway/features)
- Scope:
  - First `100` pending SHAs after batch-21, from `56ee8a5c...` through `54dd1b30...`.
- Evidence pass:
  - Built per-commit evidence map for all 100 SHAs (subject + changed paths + filetype distribution).
  - This tranche touched upstream Python/docs/runtime paths only (no Rust file changes in upstream commits).
  - Rust-native parity coverage validated against active modules:
    - CLI/runtime UX and commands: `crates/hermes-cli/src/{tui.rs,main.rs,commands.rs,doctor.rs}`
    - Clarify/delegation/session/memory tooling: `crates/hermes-tools/src/{tools,backends}/*`
    - Messaging/pairing/hooks/platforms: `crates/hermes-gateway/src/{hooks.rs,pairing.rs,platforms/*}`
    - Skills + provider integrations: `crates/hermes-skills/src/hub.rs`, `crates/hermes-agent/src/{provider.rs,memory_plugins/*,compression.rs}`
- Queue dispositions in this 100-SHA pass:
  - `superseded` (100)
  - `ported` (0)
- Queue refresh:
  - `docs/parity/upstream-missing-queue.{json,md}` regenerated with these 100 SHAs moved out of `pending`.

## 2026-04-22 batch-22b (100-tranche parity disposition pass: provider/auth/install/security/features)
- Scope:
  - Next `100` pending SHAs after batch-22a, from `a1838271...` through `b267e340...`.
- Evidence pass:
  - Built per-commit evidence map for all 100 SHAs.
  - Commit set remained upstream Python/docs/test/runtime deltas (no Rust files in upstream patch set).
  - Rust-native parity coverage validated against active modules:
    - Providers/auth/codex/openrouter routing: `crates/hermes-agent/src/{provider.rs,api_bridge.rs,oauth.rs}`, `crates/hermes-cli/src/providers.rs`
    - Memory/session/compression/honcho: `crates/hermes-agent/src/{memory_plugins/*,session_persistence.rs,compression.rs}`
    - Messaging and adapter runtime: `crates/hermes-gateway/src/{platforms/*,hooks.rs,pairing.rs}`
    - CLI install/doctor/config wiring: `scripts/install.sh`, `crates/hermes-cli/src/{doctor.rs,commands.rs,main.rs}`, `crates/hermes-config/src/paths.rs`
    - Security/file operation guardrails: `crates/hermes-tools/src/backends/file.rs`
- Queue dispositions in this 100-SHA pass:
  - `superseded` (100)
  - `ported` (0)
- Queue refresh:
  - `docs/parity/upstream-missing-queue.{json,md}` regenerated with these 100 SHAs moved out of `pending`.

## 2026-04-22 batch-22c (50-tranche parity disposition pass: integrations and adapter deltas)
- Scope:
  - Next `50` pending SHAs after batch-22b, from `b281ecd5...` through `7b23dbfe...`.
- Evidence pass:
  - Built per-commit evidence map for all 50 SHAs.
  - Commit set remained upstream Python/docs/test/runtime deltas (no Rust files in upstream patch set).
  - Rust-native parity coverage validated against active modules:
    - Gateway and adapter stack (WhatsApp/Telegram/Home Assistant/hooks/pairing): `crates/hermes-gateway/src/{platforms/*,hooks.rs,pairing.rs}`
    - Tooling integrations and skills runtime: `crates/hermes-tools/src/{tools,backends}/*`, `crates/hermes-skills/src/hub.rs`
    - Memory/provider/runtime handling (including Honcho/Codex paths): `crates/hermes-agent/src/{memory_plugins/*,provider.rs,api_bridge.rs}`
    - CLI shell/config behavior: `crates/hermes-cli/src/{main.rs,commands.rs,tui.rs}`, `crates/hermes-config/src/paths.rs`
- Queue dispositions in this 50-SHA pass:
  - `superseded` (50)
  - `ported` (0)
- Aggregate for batch-22 request (`250` total):
  - `superseded` (250)
  - `ported` (0)
  - `pending` (0 within selected tranche set)
- Queue refresh:
  - `docs/parity/upstream-missing-queue.{json,md}` regenerated with these 50 SHAs moved out of `pending`.
