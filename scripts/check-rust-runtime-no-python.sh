#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

DIRECT_SPAWN_PATTERN='(Command::new\("python|tokio::process::Command::new\("python|std::process::Command::new\("python)'
REFERENCE_PATTERN='agent_orchestration\.py|scripts/agent_orchestration\.py|HERMES_CONTEXTLATTICE_ORCH_SCRIPT|HERMES_KANBAN_CONTEXTLATTICE_SCRIPT'
PYTHON_DEFAULT_PATTERN="default: 'python'|default\": \"python\"|Supports Python|primarily Python|run Python|py_compile"

if HITS="$(rg -n --color never "${DIRECT_SPAWN_PATTERN}" crates 2>/dev/null)"; then
  if [[ -n "${HITS}" ]]; then
    echo "Rust runtime Python process spawns detected:"
    echo "${HITS}"
    exit 1
  fi
fi

if HITS="$(rg -n --color never "${REFERENCE_PATTERN}" crates 2>/dev/null)"; then
  if [[ -n "${HITS}" ]]; then
    echo "Rust runtime Python ContextLattice script references detected:"
    echo "${HITS}"
    exit 1
  fi
fi

if HITS="$(rg -n --color never "${PYTHON_DEFAULT_PATTERN}" crates/hermes-tools/src/tools/code_execution.rs crates/hermes-tools/src/backends/code_execution.rs 2>/dev/null)"; then
  if [[ -n "${HITS}" ]]; then
    echo "Python-default code execution contract detected:"
    echo "${HITS}"
    exit 1
  fi
fi

echo "No Rust runtime Python execution paths detected."
