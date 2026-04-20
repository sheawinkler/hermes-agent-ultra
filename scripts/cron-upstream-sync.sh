#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# Cron has a minimal PATH; include common tool locations explicitly.
export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:${PATH:-}"

cd "${REPO_ROOT}"
exec /usr/bin/env bash "${REPO_ROOT}/scripts/sync-upstream.sh" \
  --repo-root "${REPO_ROOT}" \
  --mode branch-pr \
  --test-cmd "cargo test -p hermes-gateway"
