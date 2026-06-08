#!/usr/bin/env bash
# Scan ~/.hermes-agent-ultra/skills for SkillGuard violations (CI / local audit).
set -euo pipefail

SKILLS_DIR="${1:-${HOME}/.hermes-agent-ultra/skills}"
MODE="${HERMES_SKILL_GUARD_MODE:-strict}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [[ ! -d "$SKILLS_DIR" ]]; then
  echo "No skills directory at $SKILLS_DIR — nothing to scan."
  exit 0
fi

export HERMES_SKILL_GUARD_MODE="$MODE"
echo "Scanning skills in $SKILLS_DIR (mode=$MODE)..."

cd "$REPO_ROOT"
cargo run -q -p hermes-cli -- skills audit "$SKILLS_DIR"
echo "skills guard scan OK"
