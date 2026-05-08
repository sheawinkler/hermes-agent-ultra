# Surface + Envy 65 Closure (2026-05-08)

## Scope
This closure artifact tracks the user-requested tranche:
- 15 verified missing/weak surface items
- Envy backlog items 1-50

## A) 15 verified weak surfaces (implemented in this patch)

1. `/snapshot` promoted from compatibility notice to first-class command handler
2. `/rollback` promoted to first-class rollback handler (undo/load/latest)
3. `/queue` promoted to first-class queue handler (enqueue + status)
4. `/steer` promoted to first-class steering injection handler
5. `/btw` promoted to first-class ephemeral side-question queue handler
6. `/sethome` promoted to first-class home marker handler
7. `/paste` promoted to first-class clipboard capture workflow
8. `/gquota` promoted to first-class Gemini auth/quota diagnostics surface
9. `/approve` promoted to first-class local pairing approval surface
10. `/deny` promoted to first-class local pairing denial/revoke surface
11. non-TUI `/ask` upgraded with deterministic option parsing fallback
12. command routing updated so these surfaces no longer dispatch to compatibility stubs
13. pairing approval/deny listing and bulk operations added (`all`, `pending`)
14. snapshot/rollback UX now includes actionable guidance and latest-session behavior
15. session steering state now persists as explicit system context for subsequent turns

Code surface touched:
- `crates/hermes-cli/src/commands.rs`

## B) Envy backlog items 1-50 (verified on baseline main)

Backlog items 1-50 correspond to the ALPHA tranche already merged and closed in this repository.

Verification source:
- `gh issue list --state all --limit 200`
- Closed issues `[ALPHA-001]` through `[ALPHA-050]` present and closed
- Examples include Objective OS, Subagents, ContextLattice lifecycle, Loop runtime durability, Trading objective loops, Kraken/algotraderv2 loop automation, and Autoresearch pipeline layers.

Representative issue range mapping:
- `#179` -> `[ALPHA-001]`
- ...
- `#228` -> `[ALPHA-050]`

## Validation run for this patch

- `cargo fmt --all`
- `cargo test -p hermes-cli --lib commands:: -- --nocapture`
- `cargo check -p hermes-cli`
- `cargo test -p hermes-cli --lib`

All checks passed in this branch.
