# Global Parity Gate Report

Generated: `2026-07-26T11:08:37.994027+00:00`

## Gate Status

- CI gate: **FAIL**
- CI gate mode: `tree-drift`
- Release gate: **FAIL**
- Release gate mode: `functional`

## Source Truth

- Source truth: **FAIL**
- Provenance or required-metric errors: `3`

## Metrics

| Metric | Value |
| --- | ---: |
| `max_files_only_upstream` | 4764.0 |
| `max_unowned_divergences` | 0.0 |
| `max_divergence_review_overdue` | 0.0 |
| `min_test_intent_mapping_ratio` | 1.0 |
| `min_test_coverage_tracked_behavior_ratio` | 1.0 |
| `max_test_coverage_audit_critical_gaps` | 0.0 |
| `max_test_coverage_missing_rust_refs` | 0.0 |
| `min_fixture_reference_category_coverage_ratio` | 1.0 |
| `max_fixture_reference_critical_gaps` | 0.0 |
| `max_fixture_reference_missing_rust_refs` | 0.0 |
| `min_behavioral_similarity_ratio` | 1.0 |
| `min_behavioral_superiority_cases` | 7.0 |
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
| `max_queue_pending_commits` | 2501.0 |

## Drift Observations (Non-Gating)

- Commit reachability counts are not parity evidence.
- `commits_behind`: `17982.0`
- `upstream_patch_missing`: `16531.0`

## GPAR Ticket Completion

| Ticket | Complete |
| --- | --- |
| `GPAR-01` | yes |
| `GPAR-02` | yes |
| `GPAR-03` | yes |
| `GPAR-04` | yes |
| `GPAR-05` | yes |
| `GPAR-06` | no |
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

## Fixture-Reference Regression Hygiene (Non-Authoritative)

- Coverage scope: `fixture_reference_regression`
- Capability-promotion authority: `false`
- Production complete: `false`
- Fixture-reference matrix metrics below are regression hygiene only and do not establish production completeness.
- Fixture/reference gate: **PASS**
- Fixture-category coverage ratio: `1.0`
- Unresolved fixture-reference gaps: `0`
- Missing referenced Rust test refs: `0`
- Referenced Rust tests: `12`

## Behavioral Similarity Diff

- Behavioral gate: **PASS**
- Similarity ratio: `1.0`
- Superior cases: `7`
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

- Upstream missing commits tracked: `3113`.
- By target ticket:
  - `#20`: `1613`
  - `#21`: `45`
  - `#22`: `206`
  - `#23`: `110`
  - `#24`: `2`
  - `#25`: `40`
  - `#26`: `1097`

