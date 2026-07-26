# Behavioral Similarity Diff

Generated: `2026-07-19T00:53:41.466421+00:00`

## Gate

- Gate: **FAIL**
- Similarity ratio: `1.0`
- Superior cases: `7`
- Regressions: `0`
- Gaps: `0`
- Unverified cases: `0`
- Missing Rust test refs: `24`

## Summary

| Metric | Value |
| --- | ---: |
| `total_cases` | 13 |
| `equal_or_better_cases` | 13 |
| `superior_cases` | 7 |
| `equivalent_cases` | 5 |
| `intentional_divergence_cases` | 1 |
| `regressions` | 0 |
| `gaps` | 0 |
| `unverified_cases` | 0 |
| `missing_rust_test_refs` | 24 |
| `missing_source_artifacts` | 1 |
| `behavioral_similarity_ratio` | 1.0 |

## Classification Counts

| Classification | Count |
| --- | ---: |
| `equivalent` | 5 |
| `intentional_divergence` | 1 |
| `superior` | 7 |

## Domain Counts

| Domain | Count |
| --- | ---: |
| `agent-loop-and-runtime` | 1 |
| `cli-command-surface` | 1 |
| `cron-and-scheduler-runtime` | 1 |
| `gateway-platform-behavior` | 1 |
| `memory-plugin-integration` | 1 |
| `model-provider-auth` | 2 |
| `model-provider-runtime` | 1 |
| `protocol-and-fault-tolerance` | 1 |
| `repo-research-planning` | 1 |
| `rust-only-runtime` | 1 |
| `skills-management-contract` | 1 |
| `tool-runtime-behavior` | 1 |

## Cases

| Case | Domain | Classification | Score |
| --- | --- | --- | ---: |
| `interactive-slash-command-surface` | `cli-command-surface` | `equivalent` | 1.0 |
| `tool-use-and-web-research` | `tool-runtime-behavior` | `equivalent` | 1.0 |
| `contextlattice-memory-first-loop` | `memory-plugin-integration` | `superior` | 1.0 |
| `repo-research-planning-contract` | `repo-research-planning` | `superior` | 1.0 |
| `source-synthesis-quorum` | `agent-loop-and-runtime` | `superior` | 1.0 |
| `provider-auth-repair-and-model-routing` | `model-provider-auth` | `equivalent` | 1.0 |
| `openai-pro-oauth-responses-backend` | `model-provider-auth` | `equivalent` | 1.0 |
| `local-openai-compatible-backends` | `model-provider-runtime` | `superior` | 1.0 |
| `gateway-platform-authorization` | `gateway-platform-behavior` | `equivalent` | 1.0 |
| `telegram-cron-topic-delivery` | `cron-and-scheduler-runtime` | `superior` | 1.0 |
| `skills-governance-and-marketplace` | `skills-management-contract` | `superior` | 1.0 |
| `runtime-rust-only-safety` | `rust-only-runtime` | `superior` | 1.0 |
| `fault-and-protocol-resilience` | `protocol-and-fault-tolerance` | `intentional_divergence` | 1.0 |

## Gate Failures

- `missing_rust_test_refs`: behavioral cases reference missing Rust tests
- `missing_source_artifacts`: behavioral cases reference missing source artifacts

## Case Issues

- `interactive-slash-command-surface` `missing_source_artifact`: source artifact does not exist
- `interactive-slash-command-surface` `missing_rust_test_ref`: referenced Rust test was not found
- `tool-use-and-web-research` `missing_rust_test_ref`: referenced Rust test was not found
- `tool-use-and-web-research` `missing_rust_test_ref`: referenced Rust test was not found
- `contextlattice-memory-first-loop` `missing_rust_test_ref`: referenced Rust test was not found
- `contextlattice-memory-first-loop` `missing_rust_test_ref`: referenced Rust test was not found
- `repo-research-planning-contract` `missing_rust_test_ref`: referenced Rust test was not found
- `source-synthesis-quorum` `missing_rust_test_ref`: referenced Rust test was not found
- `source-synthesis-quorum` `missing_rust_test_ref`: referenced Rust test was not found
- `source-synthesis-quorum` `missing_rust_test_ref`: referenced Rust test was not found
- `provider-auth-repair-and-model-routing` `missing_rust_test_ref`: referenced Rust test was not found
- `provider-auth-repair-and-model-routing` `missing_rust_test_ref`: referenced Rust test was not found
- `provider-auth-repair-and-model-routing` `missing_rust_test_ref`: referenced Rust test was not found
- `provider-auth-repair-and-model-routing` `missing_rust_test_ref`: referenced Rust test was not found
- `openai-pro-oauth-responses-backend` `missing_rust_test_ref`: referenced Rust test was not found
- `openai-pro-oauth-responses-backend` `missing_rust_test_ref`: referenced Rust test was not found
- `local-openai-compatible-backends` `missing_rust_test_ref`: referenced Rust test was not found
- `local-openai-compatible-backends` `missing_rust_test_ref`: referenced Rust test was not found
- `local-openai-compatible-backends` `missing_rust_test_ref`: referenced Rust test was not found
- `gateway-platform-authorization` `missing_rust_test_ref`: referenced Rust test was not found
- `gateway-platform-authorization` `missing_rust_test_ref`: referenced Rust test was not found
- `gateway-platform-authorization` `missing_rust_test_ref`: referenced Rust test was not found
- `telegram-cron-topic-delivery` `missing_rust_test_ref`: referenced Rust test was not found
- `telegram-cron-topic-delivery` `missing_rust_test_ref`: referenced Rust test was not found
- `telegram-cron-topic-delivery` `missing_rust_test_ref`: referenced Rust test was not found

