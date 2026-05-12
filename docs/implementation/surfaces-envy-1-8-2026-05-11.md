# Missing Surfaces 1-8 + Envy Features 1-8 (2026-05-11)

Scope: complete the remaining high-signal UX/runtime deltas without duplicating previously landed parity work.

## Surgical Plan

1. Missing Surface 1: eliminate stream-finalization race so completed runs do not appear one turn late.
2. Missing Surface 2: suppress leaked internal scaffold/tool markup in rendered transcript.
3. Missing Surface 3: add first-class auth lifecycle slash surface for active provider runtime credentials.
4. Missing Surface 4: add fast telemetry slash surface for lane/runtime/gate status from the active session.
5. Missing Surface 5: add failure-first runbooks as slash surface to reduce operator recovery latency.
6. Missing Surface 6: harden session snapshot integrity flows (`/sessions doctor`, resume validity checks).
7. Missing Surface 7: add task-depth controls so long/complex tasks are explicit and tunable at runtime.
8. Missing Surface 8: add objective verification output that includes concrete file existence verification signals.

9. Envy Feature 1: operator control for repo-review tool profile mode (`off|balanced|focus`).
10. Envy Feature 2: explicit operator escape hatch for tool-profile narrowing in active requests.
11. Envy Feature 3: richer activity-lane telemetry with delta markers (`Δtokens`, `Δtools`, `Δfiles`).
12. Envy Feature 4: policy-deny remediation hints inline in tool cards.
13. Envy Feature 5: broaden slash alias ergonomics (`/whoami`, `/rb`) and reduce compatibility friction.
14. Envy Feature 6: branch checkpoint ergonomics (`/branch list|diff|merge`) for session forking workflows.
15. Envy Feature 7: route-health detail surfacing inside `/qos` and `/ops` output paths.
16. Envy Feature 8: objective verify mode (`/objective verify`) with trend+ledger+artifact presence summary.

## Implementation Notes (No Duplicate Work)

- Reused and extended existing command framework, activity lane, and objective ledger/trend infrastructure.
- Avoided re-adding already shipped parity surfaces (capability router, mission/autopilot, context governance).
- Added only missing operational glue and hardening behavior on top of existing Rust-native architecture.

## Verification

- Rust tests (targeted):
  - `/auth`, `/runbook`, `/telemetry` command handler coverage
  - scaffold suppression + tool policy remediation rendering tests
  - repo-review tool-profile mode/escape-hatch tests
  - command-registry compatibility tests
- UI-TUI test: `externalLink` helper suite
- Interactive PTY smoke test against real `hermes-agent-ultra` binary:
  - `/auth status`
  - `/runbook list`
  - `/telemetry lane`
  - live prompt execution path with real auth failure surfaced in transcript
