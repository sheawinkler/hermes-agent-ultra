# Ultra Autonomy Surfaces

Hermes Ultra closes the Mercury-style autonomy gaps as Rust-native runtime surfaces.
The implementation is intentionally additive: existing agent loop, gateway, dashboard,
ContextLattice, approval, and tool registry paths remain authoritative.

## Runtime Surfaces

| Item | Rust surface | Purpose |
| --- | --- | --- |
| 1 | `ultra_autonomy action=loop_evaluate` | Detect identical tool loops, repeated failures, no-action loops, and repeated output text. |
| 2 | `ultra_autonomy action=board_*` | JSON-backed task boards with dependencies, blocked/question states, comments, attachments metadata, and token budgets. |
| 3 | `ultra_autonomy action=events` | Dashboard/gateway event envelopes for board, memory, objective, and subagent updates. |
| 4 | `ultra_autonomy action=resource_plan` | CPU/RAM/token/user-override admission plan for subagent concurrency. |
| 5 | approval hardline tests | Guards shell substitution/chaining laundering such as `echo $(rm -rf /)`. |
| 6 | `ultra_autonomy action=memory_lifecycle` | ContextLattice-first hot/warm/archive memory projection over Hermes memory providers. |
| 7 | `ultra_autonomy action=memory_resolve` | Memory reinforcement, conflict supersession, and provenance notes. |
| 8 | `hermes-ultra up` | One-command always-on service contract using existing gateway service management. |
| 9 | `ultra_autonomy action=channel_surface` | CLI/dashboard/gateway/Telegram skill, status, and permission surface map. |
| 10 | `ultra_autonomy action=objective_bridge` | Materialize durable objectives into dependency-tracked board cards with verification gates. |
| 11 | `ultra_autonomy action=outcome_rehearsal` | Deterministically score plan, tool use, verification, checkpoint, and recovery evidence before promotion. |
| 12 | `ultra_autonomy action=recall_quality` | Score ContextLattice/synthesis recall against implementation and verification outcomes, not just retrieval availability. |

## Dashboard Contract

The dashboard HTTP server exposes the same autonomy cockpit through JSON-RPC:

```json
{"method":"harness.autonomy","params":{}}
```

The result wraps the `/harness autonomy` payload under `result.autonomy`. Dashboard clients
can also use the emitted event names from `ultra_autonomy action=events` to render SSE-like
updates without inventing a separate event vocabulary.

## ContextLattice Synergy

`memory_lifecycle` makes ContextLattice hot by default and uses a stronger boost than generic
providers. The intent is that ContextLattice remains the durable project/objective memory plane,
while external memory providers enrich recall without replacing provenance-backed checkpoints.

## Verification

Targeted checks:

```bash
CARGO_TARGET_DIR=/Volumes/wd_black/hermes-agent-ultra/target CARGO_INCREMENTAL=0 cargo test -p hermes-tools ultra_autonomy --lib
CARGO_TARGET_DIR=/Volumes/wd_black/hermes-agent-ultra/target CARGO_INCREMENTAL=0 cargo test -p hermes-tools ultra_autonomy_evals --lib
CARGO_TARGET_DIR=/Volumes/wd_black/hermes-agent-ultra/target CARGO_INCREMENTAL=0 cargo test -p hermes-tools test_shell_substitution_and_chain_laundering_are_guarded --lib
CARGO_TARGET_DIR=/Volumes/wd_black/hermes-agent-ultra/target CARGO_INCREMENTAL=0 cargo test -p hermes-tools harness_cockpit --lib
CARGO_TARGET_DIR=/Volumes/wd_black/hermes-agent-ultra/target CARGO_INCREMENTAL=0 cargo test -p hermes-tools builtin_registry_registers_core_tool_surfaces_for_parity --lib
CARGO_TARGET_DIR=/Volumes/wd_black/hermes-agent-ultra/target CARGO_INCREMENTAL=0 cargo test -p hermes-http rpc_harness_autonomy_exposes_dashboard_surface --lib
CARGO_TARGET_DIR=/Volumes/wd_black/hermes-agent-ultra/target CARGO_INCREMENTAL=0 cargo test -p hermes-cli cli_parse_up_command --lib
```
