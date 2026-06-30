# Session Lifecycle

Hermes Ultra owns session lifecycle in the Rust runtime. This document maps the upstream gateway session concepts to the Rust surfaces that are live in this repository.

## Runtime Owners

| Concern | Rust owner | Notes |
| --- | --- | --- |
| Gateway conversation lanes | `crates/hermes-gateway/src/gateway/*` | Routes incoming platform messages, busy queues, resets, lifecycle hooks, and delivery state. |
| Persisted chat sessions | `crates/hermes-agent/src/session_persistence.rs` | Stores session metadata, snapshots, transcripts, trajectories, pruning, and maintenance. |
| CLI/TUI session controls | `crates/hermes-cli/src/app/*`, `crates/hermes-cli/src/main/interactive_resume.rs` | Handles resume, snapshot writes, startup stubs, interrupted-session finalization, and user-visible session state. |
| ACP session API | `crates/hermes-acp/src/handler/*`, `crates/hermes-acp/src/session.rs` | Exposes create/load/resume/fork lifecycle methods without executing prompts during metadata-only resume flows. |
| Gateway lifecycle events | `crates/hermes-gateway/src/gateway/lifecycle_methods.rs` | Emits reset/finalize/progress hooks and preserves lifecycle context for observers. |

## Key Rules

- Session identity is deterministic per platform/channel/thread/user lane where the platform supplies enough metadata.
- Explicit reset/new-session flows create a new session identity and persist the transition instead of mutating the prior transcript in place.
- Resume flows load prior metadata and transcript state without running a prompt executor during metadata-only session inspection.
- Interrupted TUI sessions persist user text and partial assistant output once, avoiding duplicate partial tails.
- Gateway busy queues drain FIFO follow-up turns and expose lifecycle hooks for reset/finalize observers.
- Session persistence uses atomic replacement and maintenance pruning so stale files do not corrupt active sessions.

## Contract Tests

The lifecycle contract is covered by Rust tests, including:

- `crates/hermes-agent/src/session_persistence.rs` session persist/load, replacement, logs, trajectories, metadata round-trip, migration, and prune/vacuum tests.
- `crates/hermes-cli/src/app.rs` startup stub, snapshot persistence, and count-limit pruning tests.
- `crates/hermes-cli/src/app/tests/session_runtime.rs` interrupted TUI finalization and environment/session runtime tests.
- `crates/hermes-acp/src/handler/tests/lifecycle_auth.rs` ACP initialize/create/load/resume/fork lifecycle coverage.
- `crates/hermes-acp/src/handler/tests/session_state.rs` metadata-only resume and session-state update coverage.
- `crates/hermes-gateway/src/gateway/tests/hook_lifecycle.rs` busy queue drain and lifecycle hook coverage.
- `crates/hermes-gateway/src/gateway/tests/status_profiles.rs` background task lifecycle command coverage.

## Divergence From Upstream Python Docs

Upstream documents the Python gateway implementation (`gateway/session.py`, `gateway/run.py`). Hermes Ultra intentionally does not vendor that Python session stack. The equivalent behavior is first-class Rust runtime behavior, with parity guarded by source-parity contracts and the tests listed above.
