# ContextLattice Replay Evidence Index

Generated: `2026-06-24T00:21:39.244232+00:00`

## Gate State

- Release gate: **PASS**
- CI gate: **PASS**
- Test coverage gate: **PASS**
- SOTA harness gate: **PASS**
- Queue pending: `0`

## Replay Evidence Groups

| Domain | Status | Fixtures | Rust tests | ContextLattice topic |
| --- | --- | ---: | ---: | --- |
| `workflow-replay-and-terminal-snapshots` | `covered_by_rust_contracts` | 1 | 2 | `hermes-agent-ultra/parity/sota/workflow-replay-and-terminal-snapshots` |
| `protocol-differential-contracts` | `covered_by_rust_contracts` | 1 | 2 | `hermes-agent-ultra/parity/sota/protocol-differential-contracts` |
| `fault-injection-matrix` | `covered_by_rust_contracts` | 1 | 8 | `hermes-agent-ultra/parity/sota/fault-injection-matrix` |

## Checkpoint Template

`contextlattice_write -p hermes-agent-ultra -t parity/sota -f docs/parity/contextlattice-replay-evidence-index.md --stdin`
