# ContextLattice Fixture-Reference Evidence Index

Generated: `2026-07-19T00:53:42.937344+00:00`

## Authority and Scope

- Coverage scope: `fixture_reference_regression`
- Capability-promotion authority: `false`
- Production complete: `false`
- Registry/fixture entries and their referenced Rust tests preserve regression coverage only. They do not establish an implemented product capability or executable protocol, client, or chaos evidence.

## Gate State

- Release gate: **FAIL**
- CI gate: **FAIL**
- Test coverage gate: **PASS**
- Fixture/reference regression gate: **PASS**
- Production complete: `false`
- Queue pending: `1067`

## Fixture-Reference Evidence Groups

| Domain | Regression status | Fixture references | Rust tests | ContextLattice topic |
| --- | --- | ---: | ---: | --- |
| `workflow-replay-and-terminal-snapshots` | `fixture_reference_regression_coverage` | 1 | 2 | `hermes-agent-ultra/parity/sota/workflow-replay-and-terminal-snapshots` |
| `protocol-differential-contracts` | `fixture_reference_regression_coverage` | 1 | 2 | `hermes-agent-ultra/parity/sota/protocol-differential-contracts` |
| `fault-injection-matrix` | `fixture_reference_regression_coverage` | 1 | 8 | `hermes-agent-ultra/parity/sota/fault-injection-matrix` |

## Checkpoint Template

`contextlattice_write -p hermes-agent-ultra -t parity/sota -f docs/parity/contextlattice-replay-evidence-index.md --stdin`
