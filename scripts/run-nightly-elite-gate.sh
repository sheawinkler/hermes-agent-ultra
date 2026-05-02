#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/run-nightly-elite-gate.sh [options]

Run the nightly elite validation gate on local main.

Options:
  --repo-root <path>   Repository root (default: script parent)
  --log-dir <path>     Log directory (default: ~/.hermes-agent-ultra/logs/elite-gate)
  --skip-pull          Do not run `git pull --ff-only origin main`
  --allow-non-main     Allow execution outside `main` (for local testing)
  --allow-dirty        Allow execution with a dirty worktree (for local testing)
  -h, --help           Show help
USAGE
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
LOG_DIR="${HOME}/.hermes-agent-ultra/logs/elite-gate"
DO_PULL="1"
ALLOW_NON_MAIN="0"
ALLOW_DIRTY="0"
FAILED_STEP="bootstrap"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo-root)
      REPO_ROOT="${2:?missing value for --repo-root}"
      shift 2
      ;;
    --log-dir)
      LOG_DIR="${2:?missing value for --log-dir}"
      shift 2
      ;;
    --skip-pull)
      DO_PULL="0"
      shift
      ;;
    --allow-non-main)
      ALLOW_NON_MAIN="1"
      shift
      ;;
    --allow-dirty)
      ALLOW_DIRTY="1"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

SUMMARY_SINK_ORDER="${AUTOMATION_SUMMARY_SINK_ORDER:-contextlattice,github,local}"
SUMMARY_CONTEXT_PROJECT="${AUTOMATION_SUMMARY_CONTEXT_PROJECT:-$(basename "${REPO_ROOT}")}"
SUMMARY_TOPIC_PATH="${AUTOMATION_SUMMARY_CONTEXT_TOPIC_PATH:-ops/nightly-elite-gate}"
SUMMARY_FILE_NAME="${AUTOMATION_SUMMARY_CONTEXT_FILE_NAME:-ops/nightly-elite-gate.md}"
SUMMARY_AGENT_ID="${AUTOMATION_SUMMARY_CONTEXT_AGENT_ID:-hermes_ultra_nightly}"
SUMMARY_GITHUB_ISSUE="${AUTOMATION_SUMMARY_GITHUB_ISSUE:-0}"
SUMMARY_LOCAL_PATH="${AUTOMATION_SUMMARY_LOCAL_PATH:-${LOG_DIR}/nightly-elite-gate-fallback.log}"
SUMMARY_PUBLISHER="${REPO_ROOT}/scripts/publish_automation_summary.py"

mkdir -p "${LOG_DIR}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
LOG_PATH="${LOG_DIR}/nightly-elite-gate-${STAMP}.log"
SUMMARY_PATH="${LOG_DIR}/nightly-elite-gate-latest.json"
SUMMARY_BODY_PATH="${LOG_DIR}/nightly-elite-gate-summary-${STAMP}.md"
SUMMARY_META_PATH="${LOG_DIR}/nightly-elite-gate-summary-${STAMP}.json"

run_gate() {
  echo "[elite-gate] started at $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "[elite-gate] repo=${REPO_ROOT}"
  cd "${REPO_ROOT}"

  FAILED_STEP="branch_check"
  local branch
  branch="$(git rev-parse --abbrev-ref HEAD)"
  if [[ "${branch}" != "main" && "${ALLOW_NON_MAIN}" != "1" ]]; then
    echo "[elite-gate] error: expected main branch, got '${branch}'" >&2
    return 10
  fi

  FAILED_STEP="dirty_check"
  if [[ -n "$(git status --short)" && "${ALLOW_DIRTY}" != "1" ]]; then
    echo "[elite-gate] error: working tree is dirty; refusing nightly gate" >&2
    return 11
  fi

  if [[ "${DO_PULL}" == "1" ]]; then
    FAILED_STEP="git_pull"
    git pull --ff-only origin main
  fi

  FAILED_STEP="cargo_test"
  echo "[elite-gate] running cargo test -p hermes-cli"
  cargo test -p hermes-cli

  FAILED_STEP="deterministic_replay"
  echo "[elite-gate] running deterministic replay suite"
  "${REPO_ROOT}/scripts/run-deterministic-replay-suite.sh"

  FAILED_STEP="write_summary"
  local head_sha
  head_sha="$(git rev-parse HEAD)"
  cat > "${SUMMARY_PATH}" <<EOF
{
  "ok": true,
  "timestamp_utc": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "repo_root": "${REPO_ROOT}",
  "head_sha": "${head_sha}",
  "log_path": "${LOG_PATH}",
  "checks": [
    "cargo test -p hermes-cli",
    "scripts/run-deterministic-replay-suite.sh"
  ]
}
EOF
  echo "[elite-gate] complete"
  echo "[elite-gate] summary=${SUMMARY_PATH}"
}

set +e
run_gate > >(tee "${LOG_PATH}") 2>&1
RUN_RC=$?
set -e

cd "${REPO_ROOT}"
HEAD_SHA="$(git rev-parse HEAD 2>/dev/null || true)"

if [[ "${RUN_RC}" -ne 0 ]]; then
  cat > "${SUMMARY_PATH}" <<EOF
{
  "ok": false,
  "timestamp_utc": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "repo_root": "${REPO_ROOT}",
  "head_sha": "${HEAD_SHA}",
  "log_path": "${LOG_PATH}",
  "failed_step": "${FAILED_STEP}",
  "exit_code": ${RUN_RC},
  "checks": [
    "cargo test -p hermes-cli",
    "scripts/run-deterministic-replay-suite.sh"
  ]
}
EOF
fi

cat > "${SUMMARY_BODY_PATH}" <<EOF
Nightly elite gate completed.

- exit_code: \`${RUN_RC}\`
- failed_step: \`${FAILED_STEP}\`
- summary_path: \`${SUMMARY_PATH}\`
- log_path: \`${LOG_PATH}\`
- checks:
  - \`cargo test -p hermes-cli\`
  - \`scripts/run-deterministic-replay-suite.sh\`
EOF

cat > "${SUMMARY_META_PATH}" <<EOF
{
  "head_sha": "${HEAD_SHA}",
  "log_path": "${LOG_PATH}",
  "summary_path": "${SUMMARY_PATH}",
  "exit_code": ${RUN_RC},
  "failed_step": "${FAILED_STEP}",
  "checks": [
    "cargo test -p hermes-cli",
    "scripts/run-deterministic-replay-suite.sh"
  ]
}
EOF

if [[ -f "${SUMMARY_PUBLISHER}" ]]; then
  set +e
  python3 "${SUMMARY_PUBLISHER}" \
    --repo-root "${REPO_ROOT}" \
    --summary-kind "nightly_elite_gate" \
    --status "$( [[ "${RUN_RC}" -eq 0 ]] && echo "ok" || echo "failed" )" \
    --title "Nightly Elite Gate Summary" \
    --summary-body-file "${SUMMARY_BODY_PATH}" \
    --metadata-file "${SUMMARY_META_PATH}" \
    --sink-order "${SUMMARY_SINK_ORDER}" \
    --context-project "${SUMMARY_CONTEXT_PROJECT}" \
    --context-topic-path "${SUMMARY_TOPIC_PATH}" \
    --context-file-name "${SUMMARY_FILE_NAME}" \
    --context-agent-id "${SUMMARY_AGENT_ID}" \
    --github-issue "${SUMMARY_GITHUB_ISSUE}" \
    --local-path "${SUMMARY_LOCAL_PATH}" \
    --json >/dev/null
  PUBLISH_RC=$?
  set -e
  if [[ "${PUBLISH_RC}" -ne 0 ]]; then
    echo "[elite-gate] warning: summary publish failed; fallback file=${SUMMARY_LOCAL_PATH}" >&2
  fi
else
  echo "[elite-gate] warning: summary publisher missing at ${SUMMARY_PUBLISHER}" >&2
fi

exit "${RUN_RC}"
