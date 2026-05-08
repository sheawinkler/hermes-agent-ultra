# ContextLattice-First Intelligence 16+ (Implemented)

This tranche reformulates the prior 16-item intelligence/performance set so memory and reasoning behavior is explicitly ContextLattice-first.

## Elements
1. Mandatory preflight handshake before deep execution.
2. Scoped retrieval-first discipline for objective/repo workflows.
3. Automatic broader same-project retrieval when scoped hits are empty/degraded.
4. Context-pack requirement for broad or multi-file tasks.
5. Grounding-required retrieval posture.
6. Retrieval-debug-on-execution posture (mode-dependent).
7. Lifecycle checkpoint policy for durable progress writes.
8. Scoped readback verification before finalization.
9. Contradiction check across evidence layers.
10. Numeric-fact verbatim-copy integrity rule.
11. Required project-scoping for memory operations.
12. Structured checkpoint payload contract (`project/file/topic`).
13. Explicit deep retry budget profile.
14. Explicit regular retry budget profile.
15. Objective analytics writeback requirement.
16. Summary sink-order policy (`contextlattice -> github -> local`).
17. ContextLattice telemetry folded into adaptive intelligence/performance autopilot.

## Implementation Surfaces
- `crates/hermes-cli/src/alpha_runtime.rs`
  - Expanded `ContextLatticePolicy` surface and defaults.
  - Added policy-mode setters (`max|balanced|fast`).
- `crates/hermes-cli/src/commands.rs`
  - Added `/objective context [status|list|max|balanced|fast]`.
  - Included ContextLattice policy in objective status output.
- `crates/hermes-agent/src/agent_loop.rs`
  - Added ContextLattice-first intelligence system hint for objective/repo/connect intents when tools are available.
- `scripts/run-performance-autopilot.py`
  - Added ContextLattice preflight/telemetry section.
  - Added ContextLattice-aware recommendations and adaptive-index penalties.

## Operator Usage
- Show current policy: `/objective context status`
- List policy presets: `/objective context list`
- Max intelligence mode: `/objective context max`
- Balanced mode: `/objective context balanced`
- Speed-focused mode: `/objective context fast`
- Run adaptive loop: `/ops autopilot run` then `/ops autopilot recommend` or `/ops autopilot apply`
