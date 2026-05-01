# Remaining Queue Implementation Plan (2026-05-01)

## Objective
Drive `docs/parity/upstream-missing-queue.json` pending commits from current state to zero while preserving Rust-first runtime integrity and evidence-backed parity governance.

## Baseline
- Queue total: 1309 commits
- Pending: 195
- Distribution: ticket 20 (96), 26 (48), 22 (25), 23 (16), 25 (4), 21 (4), 24 (2)
- Gate blocker: `GPAR-06=false` because pending > 0

## Execution Strategy
1. Implement real runtime gaps first
- Patch concrete missing capabilities directly in Rust before queue relabeling.
- Current tranche target: local `piper` TTS backend support.
- Verification: crate-level tests must pass.

2. Resolve pending queue with explicit evidence notes
- For each pending entry:
  - `ported` when direct Rust implementation evidence exists.
  - `superseded` only when upstream delta is Python/web/plugin-release-only and Rust ownership is explicit.
- Never leave implicit/blank notes.
- Never use generic “done” claims without file-level evidence.

3. Regenerate parity proof artifacts
- Recompute queue markdown/json summaries.
- Re-run parity matrix and global proof generators.
- Confirm release gate status reflects queue closure.

4. Validate and ship
- Run targeted tests for patched runtime surfaces.
- Run parity scripts.
- Commit in chronological tranches.
- Push branch and merge to `main`.

## Classification Rules (Queue Resolution)
- `ported` rules
  - `/reload-mcp` warning behavior parity: Rust command handler evidence in `crates/hermes-cli/src/commands.rs`.
  - Piper TTS provider parity: Rust tool backend evidence in `crates/hermes-tools/src/backends/tts.rs` and tool schema in `crates/hermes-tools/src/tools/tts.rs`.
- `superseded` rules
  - release metadata/AUTHOR_MAP churn
  - docs-only or website-only deltas
  - upstream web/ui-tui Node frontend deltas when Rust TUI owns UX
  - upstream Python-test-only deltas when Rust test suites cover behavior domain
  - plugin-only Python runtime wiring with Rust-native adapter/toolset ownership
  - packaging/nix/docker Python-release helper deltas not impacting Rust runtime semantics

## Completion Criteria
- `pending == 0` in upstream queue artifact
- `GPAR-06 == true` in global parity proof
- no unresolved queue row lacks owner/notes
- runtime tests for newly added parity behavior pass

## Risk Controls
- If any entry cannot be credibly classified as `ported` or `superseded`, keep it pending and isolate as a focused implementation task.
- No claim of feature completion without a concrete Rust file path and test signal.
