# hermes-eval

`hermes-eval` runs benchmark datasets against Hermes Agent rollouts and writes reproducible JSON reports.

## Quick Start

Run the configured smoke benchmark with a deterministic noop rollout:

```bash
cargo run -p hermes-eval --bin hermes-bench -- \
  --benchmark crates/hermes-eval/benchmarks/configured-smoke.toml \
  --rollout noop \
  --print-json
```

Run against a real `AgentLoop` rollout:

```bash
cargo run -p hermes-eval --bin hermes-bench -- \
  --benchmark crates/hermes-eval/benchmarks/configured-smoke.toml \
  --rollout agent \
  --model nous:Hermes-4-70B \
  --concurrency 2
```

By default, reports are written to `evals/<benchmark>-<timestamp>.json`. Override with `--output`.

## Dataset Format

Datasets are `.toml` or `.json` files with:

- `benchmark`: metadata (`id`, `name`, `source`, `version`)
- `tasks[]`: each task includes:
  - `task_id`, `instruction`
  - optional `category`, `context`, `timeout_secs`
  - optional heuristic checks: `expected_contains`, `expected_regex`, `expected_any`, `min_length`
  - optional `judge_prompt` for LLM-as-judge (enabled via `HERMES_EVAL_LLM_JUDGE=1`)

LLM judge settings:

- `HERMES_EVAL_JUDGE_API_KEY` (or `OPENAI_API_KEY`)
- `HERMES_EVAL_JUDGE_BASE_URL` (default `https://api.openai.com/v1`)
- `HERMES_EVAL_JUDGE_MODEL` (default `gpt-4o-mini`)
