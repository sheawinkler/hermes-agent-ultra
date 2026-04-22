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
