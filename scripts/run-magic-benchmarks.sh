#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

: "${CARGO_TARGET_DIR:=/Volumes/wd_black/hermes-agent-ultra/target}"
export CARGO_TARGET_DIR

cargo test -p hermes-tools tools::magic --lib
cargo test -p hermes-tools register_builtins::tests::builtin_registry_registers_core_tool_surfaces_for_parity --lib
cargo fmt --all --check
