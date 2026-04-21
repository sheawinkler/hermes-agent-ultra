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

