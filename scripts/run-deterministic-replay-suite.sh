#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

echo "[deterministic-replay-suite] running hermes-cli e2e replay suite"
cargo test -p hermes-cli --test e2e_replay_suite -- --nocapture
