# Global Parity Proof

Generated: `2026-07-21T22:47:25.092180+00:00`

## Gate Status

- CI gate: **FAIL**
- CI gate mode: `tree-drift`
- Release gate: **FAIL**
- Release gate mode: `functional`

## Source Truth

- Source truth: **FAIL**
- Provenance or required-metric errors: `5`

## Metrics

| Metric | Value |
| --- | ---: |
| `max_files_only_upstream` | 3439.0 |
| `max_unowned_divergences` | 0.0 |
| `max_divergence_review_overdue` | 0.0 |
| `min_test_intent_mapping_ratio` | 1.0 |
| `min_test_coverage_tracked_behavior_ratio` | 1.0 |
| `max_test_coverage_audit_critical_gaps` | 0.0 |
| `max_test_coverage_missing_rust_refs` | 0.0 |
| `min_sota_harness_domain_coverage_ratio` | 1.0 |
| `max_sota_harness_critical_gaps` | 0.0 |
| `max_sota_harness_missing_rust_refs` | 0.0 |
| `min_behavioral_similarity_ratio` | 1.0 |
| `min_behavioral_superiority_cases` | 8.0 |
| `max_behavioral_regressions` | 0.0 |
| `max_behavioral_gaps` | 0.0 |
| `max_behavioral_unverified_cases` | 0.0 |
| `max_behavioral_missing_rust_refs` | 0.0 |
| `min_deep_problem_solving_ratio` | 1.0 |
| `min_deep_problem_solving_superiority_cases` | 10.0 |
| `min_deep_problem_solving_total_cases` | 10.0 |
| `max_deep_problem_solving_regressions` | 0.0 |
| `max_deep_problem_solving_gaps` | 0.0 |
| `max_deep_problem_solving_unverified_cases` | 0.0 |
| `max_deep_problem_solving_missing_rust_refs` | 0.0 |
| `max_queue_pending_commits` | 1343.0 |

## Drift Observations (Non-Gating)

- Commit reachability counts are not parity evidence.
- `commits_behind`: `1.0`
- `upstream_patch_missing`: `1.0`

## GPAR Ticket Completion

| Ticket | Complete |
| --- | --- |
| `GPAR-01` | yes |
| `GPAR-02` | yes |
| `GPAR-03` | yes |
| `GPAR-04` | yes |
| `GPAR-05` | yes |
| `GPAR-06` | yes |
| `GPAR-07` | no |
| `GPAR-08` | yes |
| `GPAR-09` | yes |
| `GPAR-10` | yes |
| `GPAR-11` | yes |

## Test Coverage Audit

- Audit gate: **PASS**
- Tracked behavior coverage ratio: `1.0`
- Critical gaps: `0`
- Missing Rust test refs: `0`

## SOTA Harness Matrix

- Harness gate: **PASS**
- Domain coverage ratio: `1.0`
- Critical gaps: `0`
- Missing Rust test refs: `0`
- Direct Rust tests: `12`

## Behavioral Similarity Diff

- Behavioral gate: **PASS**
- Similarity ratio: `1.0`
- Superior cases: `8`
- Regressions: `0`
- Gaps: `0`
- Unverified cases: `0`
- Missing Rust test refs: `0`

## Deep Problem-Solving Diff

- Deep problem-solving gate: **PASS**
- Deep problem-solving ratio: `1.0`
- Superior cases: `10`
- Regressions: `0`
- Gaps: `0`
- Unverified cases: `0`
- Missing Rust test refs: `0`

## Queue Summary

- Upstream missing commits tracked: `1839`.
- By target ticket:
  - `#20`: `898`
  - `#21`: `15`
  - `#22`: `93`
  - `#23`: `69`
  - `#24`: `2`
  - `#25`: `32`
  - `#26`: `730`

