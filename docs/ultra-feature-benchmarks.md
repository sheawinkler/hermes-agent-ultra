# Ultra Feature Benchmarks

Hermes Agent Ultra exposes a Rust-native ultra-feature benchmark surface for the competitive coding-agent outcomes that make a terminal harness feel unusually capable.

```text
ultra-feature-16
  -> ultra-feature-2 smoke
  -> ultra-feature-5 smoke
  -> ultra-feature-6 symbols smoke
  -> ultra-feature-10 smoke
  -> ultra-feature-14 smoke
```

The benchmark is intentionally deterministic and local. It does not require GitHub Actions, remote services, Python, or paid provider calls.

## Run

```sh
CARGO_TARGET_DIR=/Volumes/wd_black/hermes-agent-ultra/target scripts/run-ultra-feature-benchmarks.sh
```

The script runs focused Rust tests for the ultra-feature harness and then checks formatting for the Rust workspace.
