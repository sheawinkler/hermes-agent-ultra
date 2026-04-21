# Global Parity Program (Issues #19-#28)

This runbook maps each GPAR ticket to executable artifacts and checks.

## Ticket Mapping

- `#20 GPAR-01` tests + CI parity closure
  - `docs/parity/test-intent-mapping.json`
  - `crates/hermes-parity-tests/tests/global_parity_governance.rs`
  - `crates/hermes-parity-tests/fixtures/hermes_core/tool_call_parser.json`
- `#21 GPAR-02` skills + optional-skills parity
  - test-intent mapping entry: `skills-management-contract`
  - divergence governance for skills catalog strategy
- `#22 GPAR-03` UX parity scope governance
  - divergence registry entry: `rust-cli-tui-primary-ux-surface`
  - global parity proof completion tracking
- `#23 GPAR-04` gateway/platform/plugin-memory parity
  - `docs/parity/adapter-feature-matrix.json`
  - memory plugin discovery matrix from Rust sources
- `#24 GPAR-05` environments + parser + benchmark parity
  - parser fixtures in `crates/hermes-parity-tests/fixtures/hermes_core/tool_call_parser.json`
  - test-intent mapping entries for parser + environments
- `#25 GPAR-06` packaging/docs/install/workflow parity
  - `docs/parity/shared-different-classification.json`
- `#26 GPAR-07` upstream missing patch queue backfill
  - `docs/parity/upstream-missing-queue.json`
  - `docs/parity/upstream-missing-queue.md`
- `#27 GPAR-08` intentional-divergence burn-down
  - `scripts/validate-intentional-divergence.py`
  - `docs/parity/divergence-validation.json`
- `#28 GPAR-09` final parity proof gate + release checklist
  - `scripts/generate-global-parity-proof.py`
  - `docs/parity/global-parity-proof.json`
  - `.github/workflows/parity-audit.yml`

## Continuous Upkeep

- Webhook worker now runs global parity audit in addition to CLI surface drift checks.
- Scheduled GitHub workflow (`Parity Audit`) runs daily and comments issue `#19` when CI parity gate fails.
- CI (`.github/workflows/ci.yml`) enforces parity artifact generation and global parity governance contract tests.
