# Rust Parity Compatibility Policy

## Purpose
This policy defines how `hermes-agent-ultra` maintains parity with `NousResearch/hermes-agent`
while keeping Rust-first implementation ownership.

## Decision Rules
1. Default rule: implement required behavior in Rust.
2. If Rust implementation is blocked, FFI fallback is allowed only with explicit rationale and
   a follow-up Rust-native migration issue.
3. Python/TS runtime file imports are not used as the parity mechanism.
4. Divergences must be declared in `docs/parity/intentional-divergence.json`.
5. Upstream commit deltas are processed through the parity upkeep queue before release.

## Compatibility Tiers
1. `rust-native`: feature implemented and validated in Rust crates.
2. `rust-native+ultra`: Rust parity plus intentional ultra extension.
3. `ffi-temporary`: temporary bridge pending Rust replacement.
4. `intentional-divergence`: upstream behavior intentionally not mirrored; rationale required.

## WS Mapping
1. WS2: core runtime parity in Rust (`crates/hermes-agent`, `crates/hermes-cli`, config/runtime wiring).
2. WS3: tool/adapter runtime parity (live backends, platform wiring).
3. WS4: skills parity strategy (catalog governance and divergence classification).
4. WS5: UX parity strategy (CLI/TUI-first surface and explicit web divergence policy).
5. WS6: test/CI parity gate.
6. WS7: security/secrets/store/webhook hardening parity.
7. WS8: this compatibility contract and auditability rules.

## Release Gate
A parity release requires:
1. `docs/parity/parity-matrix.json` refreshed against current `upstream/main`.
2. `docs/parity/workstream-status.json` refreshed and showing complete status for WS2-WS8.
3. Any non-`rust-native` item documented with rationale and owner.
