# Hermes Ultra production-gap closure

Updated: 2026-07-17
Baseline commit: `c002f8c4efe475c89c9f14d3995d44e6a5df5b27`
Working state: intentionally dirty, unstaged integration baseline. Never replace it with a whole worktree copy.

## Program contract

A capability is complete only when it has:

1. a production callsite;
2. a real dispatch or consumer branch;
3. a failure-path test;
4. a smoke exercise through the shipped path; and
5. an eval ledger recording baseline, metric, fixed holdout, cost, latency, tool calls, failures, and a reproduction artifact.

Fixtures, catalogs, diagnostics, prose, and generated status documents do not prove implementation.

The live P0 evaluation record is `p0-eval-ledger.json`. Null measurements are explicitly incomplete and cannot promote a packet.

## Upstream provenance rule

Raw symmetric commit counts are not a parity gap when there is no trusted merge base. Establish an explicit reviewed baseline or patch-ID plus tree-content equivalence map first, collapse rebases/cherry-picks/equivalent implementations, and classify only unmatched semantic changes.

## Resource envelope

- Keep unrelated ContextLattice, algotrader, OrbStack, and agent processes alive.
- Use no more than six concurrent read-only agents initially.
- Run one heavy Rust build at a time with `CARGO_BUILD_JOBS=4`, `CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-target/p0-eval}`, and `CARGO_INCREMENTAL=0`.
- Prefer focused package checks and scenario tests before repository-wide gates.
- Checkpoint accepted decisions and completed packets to ContextLattice.

## Active packets

| Packet | Handoff | State |
|---|---|---|
| P0-A parity provenance | `p0-a-parity-provenance.md` | byte-level private source digest plus aggregate-only sanitized public projection; 36 focused tests passed |
| P0-C queue and resource integrity | `p0-c-queue-resource.md` | core queue/resource suite passed (616); speech/media regression suite passed (146) but final security review found five blockers under correction; native Discord final rerun pending |
| P0-D recovery and redaction | `p0-d-recovery.md` | implemented; 226 focused tests passed |
| Discord native ingress | `p0-discord-driver.md` | isolated implementation under backpressure correction and integration review |
| Durable DM grants | `p0-durable-dm-grants.md` | integrated; independent review passed; 128 focused contract tests passed |
| P1 typed run records and outbox | `p1-runrecord-outbox.md`, `p1-transaction-design.md` | design review found mandatory transaction/migration/privacy/recovery gaps; correction pending P0 closure |
| Kimi K3 coding route | `kimi-k3.md` | paid-only and local profile incorrect; deferred without API spend |
| Issue #702 fake-feature closure | `fake-feature-audit.md` | cockpit now reports typed `0 complete / 12 partial / 3 facade-only` truth with six focused regressions; dependency-ordered implementation closure active |

## Integration discipline

For isolated worktrees, compare each listed file against this live dirty baseline and integrate only intentional deltas. Do not copy directories or whole files whose baseline also changed. Re-read and re-test after every integration packet.
