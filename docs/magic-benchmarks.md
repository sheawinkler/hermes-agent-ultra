# Magic Benchmarks

Hermes Agent Ultra exposes a Rust-native magic harness benchmark surface for the competitive coding-agent outcomes that make a terminal harness feel unusually capable.

```text
magic_benchmark
  -> hash_edit smoke
  -> resolve_conflict smoke
  -> lsp_inspect symbols smoke
  -> stream_rule_guard smoke
  -> minimize_output smoke
```

The benchmark is intentionally deterministic and local. It does not require GitHub Actions, remote services, Python, or paid provider calls.

## Run

```sh
CARGO_TARGET_DIR=/Volumes/wd_black/hermes-agent-ultra/target scripts/run-magic-benchmarks.sh
```

The script runs focused Rust tests for the magic harness and then checks formatting for the Rust workspace.
