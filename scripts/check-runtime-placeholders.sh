#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

PATTERN="not yet implemented|TODO: implement|unimplemented!\\(|todo!\\(|placeholder summary|stub-only|placeholder that records the join intent|minimal stub LLM"

if HITS="$(rg -n -i --color never --glob '!**/tests/**' --glob '!target/**' --glob '!scripts/check-runtime-placeholders.sh' "${PATTERN}" crates scripts 2>/dev/null)"; then
  if [[ -n "${HITS}" ]]; then
    echo "Runtime placeholder markers detected:"
    echo "${HITS}"
    exit 1
  fi
fi

echo "No runtime placeholder markers detected."
