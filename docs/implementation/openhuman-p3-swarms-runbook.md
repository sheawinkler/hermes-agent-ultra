# OpenHuman P3 Swarms Runbook

## Scope

P3 delivers swarm-oriented orchestration on top of existing quorum execution so we do not fork runtime behavior.

Coverage in this tranche:

1. Swarm runtime integration (engine layer)
2. Swarm command/control surface
3. Reliability and cost guardrails
4. Acceptance gates and tests

## 1) Engine Layer

Implemented in `hermes-intelligence`:

- `src/swarm_runtime.rs`
  - `SwarmExecutionMode` (`concurrent|sequential|graph`)
  - `SwarmRuntimeStatus` (feature/linkage status + notes)
  - `SwarmExecutionPlan` (deterministic plan contract)
  - `swarm_runtime_status()`
  - `build_swarm_execution_plan(...)`

Feature-gated external engine wiring:

- Optional dependency: `swarms-rs` (renamed crate key: `swarms`)
- Feature flag: `swarms`
- Status reports whether `swarms-rs` is compiled/linked or quorum-fallback is active.

## 2) Command Surface

Implemented in `hermes-cli` slash commands:

- `/swarm status`
- `/swarm plan [concurrent|sequential|graph]`
- `/swarm run [passes] [mode]`
- `/swarm cancel`
- `/swarm artifact`
- `/swarms` (alias to `/swarm`)

Compatibility controls:

- `/swarm on|off|voters|models` forwards to quorum policy controls.
- `run` arms one-shot fanout and preserves existing quorum synthesis/artifact path.

## 3) Reliability + Cost Guardrails

- Pass cap bounded to `1..8` (`HERMES_QUORUM_VOTER_PASSES`).
- Voter quorum threshold derived deterministically (`required_success` majority).
- Artifact discovery prefers current session artifacts, then newest global artifact.
- No duplicate orchestration loop introduced; swarm surface reuses proven quorum execution path.

## 4) Acceptance Criteria

Validation gates completed:

- `cargo check -p hermes-intelligence`
- `cargo check -p hermes-intelligence --features swarms`
- `cargo test -p hermes-intelligence swarm_runtime -- --nocapture`
- `cargo test -p hermes-cli p3_swarm -- --nocapture`

New tests:

- `commands::tests::p3_swarm_commands_registered_and_completable`
- `commands::tests::p3_swarm_status_plan_run_cancel_surface_is_handled`
- `swarm_runtime::tests::required_success_uses_majority_when_voters_gt_two`

