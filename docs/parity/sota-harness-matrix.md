# Fixture-Reference Harness Matrix

Generated: `2026-07-19T00:26:02Z`

## Authority and Scope

- Coverage scope: `fixture_reference_regression`
- Capability-promotion authority: `false`
- Production complete: `false`
- Registry/fixture entries and their referenced Rust tests preserve regression coverage only. They do not establish an implemented product capability or executable protocol, client, or chaos evidence.

## Fixture-Reference Regression Gate

- Fixture/reference gate: **PASS**
- Unresolved fixture-reference gaps: `0`
- Missing referenced Rust test refs: `0`
- Declared fixture-category coverage ratio: `1.0`

## Fixture-Reference Domains

| Domain | Regression status | Fixture references | Rust tests |
| --- | --- | ---: | ---: |
| `workflow-replay-and-terminal-snapshots` | `fixture_reference_regression_coverage` | 1 | 2 |
| `protocol-differential-contracts` | `fixture_reference_regression_coverage` | 1 | 2 |
| `fault-injection-matrix` | `fixture_reference_regression_coverage` | 1 | 8 |

## What This Records

- Workflow fixtures record selected operator-journey outputs and the Rust tests that reference them; they do not prove every listed journey is an implemented production capability.
- Protocol fixtures record normalized ACP, MCP, and gateway expectations; they are not executable protocol or client evidence.
- Fault fixtures record named failure categories and selected Rust tests; they are not an executed external chaos result.

## Source Artifacts

- `crates/hermes-parity-tests/tests/fixtures/sota_workflow_replay.json`
- `crates/hermes-protocol-parity-tests/tests/fixtures/protocol_differential_contracts.json`
- `crates/hermes-agent/src/testdata/adapter_chaos_profiles.json`
