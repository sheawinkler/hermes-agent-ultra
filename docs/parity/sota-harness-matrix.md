# SOTA Harness Matrix

Generated: `2026-06-10T23:45:00Z`

## Gate

- Gate: **PASS**
- Critical gaps: `0`
- Missing Rust test refs: `0`
- Domain coverage ratio: `1.0`

## Domains

| Domain | Status | Fixtures | Rust tests |
| --- | --- | ---: | ---: |
| `workflow-replay-and-terminal-snapshots` | `covered_by_rust_contracts` | 1 | 2 |
| `protocol-differential-contracts` | `covered_by_rust_contracts` | 1 | 2 |
| `fault-injection-matrix` | `covered_by_rust_contracts` | 1 | 8 |

## What This Adds

- Workflow replay turns operator CLI journeys into deterministic fixtures, including terminal-facing non-TTY diagnostics.
- Protocol differential contracts compare normalized ACP, MCP, and gateway behavior against fixture expectations.
- Fault injection expands the existing Rust chaos harness with connection reset, auth expiry, and malformed tool payload scenarios, while retaining stream recovery tests.

## Source Artifacts

- `crates/hermes-parity-tests/tests/fixtures/sota_workflow_replay.json`
- `crates/hermes-protocol-parity-tests/tests/fixtures/protocol_differential_contracts.json`
- `crates/hermes-agent/src/testdata/adapter_chaos_profiles.json`
