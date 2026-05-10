# Hermes Agent Ultra: Items 1-12 Surgical Plan + Implementation Notes (2026-05-10)

Scope: complete coverage for the 12 previously proposed upgrades while reusing existing Rust-first surfaces and minimizing net-new code.

## 1) Universal OAuth self-heal loop
Implementation:
- Generalized one-shot auth auto-repair from Nous-only to provider-aware OAuth verification.
- Added provider inference from overrides/model prefix/error host patterns.
- Auto-runs `auth verify <provider>` and retries once for OAuth-capable providers.
Files:
- `crates/hermes-cli/src/main.rs`

## 2) Adaptive tool-budget governor
Status:
- Already implemented.
Evidence surfaces:
- `crates/hermes-agent/src/agent_loop.rs` (`governor_*`, tool-loop guard, discovery-budget policy).

## 3) Agent live reasoning lane phases/progress
Implementation:
- Added structured phase events (`ui_event=phase`) with progress percentages.
- Wired App runtime to emit `preflight`, `dispatch`, `inference`, `recovery`, `finalize` phases.
- TUI now tracks/render phase name + progress in Activity Lane and heartbeat pulses.
Files:
- `crates/hermes-cli/src/app.rs`
- `crates/hermes-cli/src/tui.rs`

## 4) Deterministic replay + diff runner
Status:
- Already implemented.
Evidence surfaces:
- `/raw trace ...` and `/studio replay status|verify|diff ...`
- `crates/hermes-cli/src/commands.rs`

## 5) Skills risk firewall v2
Status:
- Already implemented.
Evidence surfaces:
- skill bundle security scanning, plugin security checks, credential guard paths in:
- `crates/hermes-cli/src/commands.rs`
- `crates/hermes-tools/src/tools/*`

## 6) Auto shell/env resolver
Implementation delta:
- Preserved existing bash/zsh fallback but now prefers the user's current `$SHELL` when supported (`bash`/`zsh`) before fallback sequence.
Files:
- `crates/hermes-environments/src/local.rs`

## 7) ContextLattice objective autopin
Implementation:
- Added objective-driven ContextLattice topic autopin.
- When an objective contract exists and topic path is unset/default (or prior objective autopin), runtime pins `CONTEXTLATTICE_TOPIC_PATH=runbooks/objective/<objective_id>`.
- Emits lifecycle + phase note for operator visibility.
Files:
- `crates/hermes-cli/src/app.rs`

## 8) Provider capability guardrails
Status:
- Already implemented.
Evidence surfaces:
- `/model explain`, `/model why-not`, capability filtering and incompatibility reporting.
- `crates/hermes-cli/src/commands.rs`
- `crates/hermes-cli/src/alpha_runtime.rs`

## 9) Session resilience upgrade
Status:
- Already implemented.
Evidence surfaces:
- session persistence/snapshots/resume surfaces and guardrails.
- `crates/hermes-cli/src/app.rs`, `crates/hermes-cli/src/main.rs`

## 10) Upstream parity watchdog
Status:
- Already implemented.
Evidence surfaces:
- parity reports and drift gates in scripts/docs:
- `scripts/run-differential-parity-gate.py`
- `scripts/upstream_webhook_sync.py`
- `docs/upstream-webhook-sync.md`

## 11) Cost/latency autopilot
Status:
- Already implemented.
Evidence surfaces:
- autopilot profiles + reporting + recommendation/apply flows:
- `scripts/run-performance-autopilot.py`
- `crates/hermes-cli/src/commands.rs`

## 12) Operator-mode toggles
Implementation delta:
- Added `/ops mode ...` alias routing to policy profiles (`strict|standard|dev`) to make operator flow explicit inside `/ops`.
Files:
- `crates/hermes-cli/src/commands.rs`

## Test strategy for this tranche
- Unit tests for OAuth inference and one-shot auto-verify provider mapping.
- Unit tests for TUI phase state/progress behavior.
- Unit tests for shell preference order in login wrapper.
- Existing regression suites for replay/policy/objective/model capabilities remain authoritative.
