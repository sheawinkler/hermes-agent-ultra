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

