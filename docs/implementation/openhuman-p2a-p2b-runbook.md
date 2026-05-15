# OpenHuman P2a/P2b Runbook

## Scope

This tranche adds operator reliability hardening (`P2a`) and adaptive intelligence surfaces (`P2b`) on top of the prior OpenHuman-aligned command surfaces.

Follow-on tranche:

- `docs/implementation/openhuman-p3-swarms-runbook.md`

## P2a: Reliability + Control Plane

### 1) Trigger triage learning loop

Commands:

- `/triage status`
- `/triage eval <source> <payload>`
- `/triage queue <source> <payload>`
- `/triage feedback <source> <outcome> <payload>`

Behavior:

- Feedback writes to `~/.hermes-agent-ultra/triage/learning.json`.
- Evaluation now applies bounded learned severity bias (`-3..+3`) by source and prior feedback notes.
- Supported outcomes: `critical|confirmed|useful|neutral|false-positive|drop`.

### 2) Subconscious guard packs + dry-run

Commands:

- `/subconscious status`
- `/subconscious profile [status|list|strict|balanced|dev|clear]`
- `/subconscious run [n] [--dry-run] [profile=<strict|balanced|dev>]`

Behavior:

- Profile defaults to `balanced`, overridable with `HERMES_SUBCONSCIOUS_PROFILE`.
- `strict` blocks non-low-risk runs, `balanced` blocks high-risk runs, `dev` allows all pending runs.
- Dry-run previews dispatch decisions without queue mutation.

### 3) Integrations repair + snapshot export

Commands:

- `/integrations repair`
- `/integrations snapshot`

Behavior:

- Repair returns a concrete remediation plan for auth, oauth runtime gate, memory probe, and follow-up checks.
- Snapshot writes consolidated control-plane JSON to `~/.hermes-agent-ultra/logs/integrations-snapshot-<session>-<ts>.json`.

### 4) Boot profile thresholds

Commands:

- `/boot`
- `/boot quick`
- `/boot profile [status|list|dev|standard|prod|clear]`

Behavior:

- `dev`: warnings do not block overall PASS.
- `standard`: warnings produce WARN.
- `prod`: warnings are treated as blockers (overall FAIL unless fully clean).

## P2b: Adaptive Intelligence Surfaces

### 1) Walkthrough telemetry + insights

Commands:

- `/walkthrough start [quick|full]`
- `/walkthrough next`
- `/walkthrough done <step-id>`
- `/walkthrough insights`

Behavior:

- Events append to `~/.hermes-agent-ultra/walkthrough/events.jsonl`.
- Insights summarize starts by mode, step completion/drop-off signal, and resume hint.

### 2) Compression rules recommend + autotune

Commands:

- `/compress rules recommend`
- `/compress rules autotune`
- `/compress rules autotune apply [user|project]`

Behavior:

- Recommendation derives from current conversation/TOOL payload shape.
- Autotune apply writes the selected plane and projects recommended knobs into runtime env.

### 3) OAuth gate manifest override

Env / files:

- `HERMES_OAUTH_GATE_MANIFEST_PATH=<path>` (explicit override)
- fallback: `~/.hermes-agent-ultra/oauth-gate-manifest.json`
- fallback default: built-in manifest values

Behavior:

- OAuth runtime gate minimums can be provider-specific via manifest.
- `/integrations status` auth panel now prints active `oauth_manifest` source.

## Validation

Targeted tests:

- `cargo test -p hermes-cli p2_ -- --nocapture`

Broader command tests:

- `cargo test -p hermes-cli p0_ -- --nocapture`
- `cargo test -p hermes-cli p1_ -- --nocapture`

Package regression:

- `cargo test -p hermes-cli --lib`
