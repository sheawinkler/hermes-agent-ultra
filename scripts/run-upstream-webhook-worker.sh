#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "${SCRIPT_DIR}/.." && pwd)}"
ENV_FILE="${UPSTREAM_SYNC_ENV_FILE:-$HOME/.hermes-agent-ultra/upstream-webhook-sync.env}"

if [[ -f "${ENV_FILE}" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "${ENV_FILE}"
  set +a
fi

is_true() {
  case "${1:-}" in
    1|true|TRUE|yes|YES|on|ON) return 0 ;;
    *) return 1 ;;
  esac
}

host_matches() {
  local allowed="$1"
  local short="${2:-}"
  local full="${3:-}"
  local allowed_short="${allowed%%.*}"
  [[ "${allowed}" == "${short}" || "${allowed}" == "${full}" || "${allowed_short}" == "${short}" ]]
}

ROLE="${UPSTREAM_SYNC_RUNTIME_ROLE:-dev}"
ALLOWED_HOSTNAME="${UPSTREAM_SYNC_ALLOWED_HOSTNAME:-}"
GUARD_BYPASS="${UPSTREAM_SYNC_DISABLE_DEV_GUARD:-0}"
CURRENT_HOST_SHORT="$(hostname -s 2>/dev/null || hostname)"
CURRENT_HOST_FULL="$(hostname 2>/dev/null || echo "${CURRENT_HOST_SHORT}")"

if ! is_true "${GUARD_BYPASS}"; then
  if [[ "${ROLE}" != "dev" ]]; then
    echo "Refusing to start worker: UPSTREAM_SYNC_RUNTIME_ROLE='${ROLE}' (dev required)." >&2
    exit 3
  fi
  if [[ -n "${ALLOWED_HOSTNAME}" ]] && ! host_matches "${ALLOWED_HOSTNAME}" "${CURRENT_HOST_SHORT}" "${CURRENT_HOST_FULL}"; then
    echo "Refusing to start worker on host '${CURRENT_HOST_FULL}'. Allowed host is '${ALLOWED_HOSTNAME}'." >&2
    exit 3
  fi
fi

BACKEND="${UPSTREAM_SYNC_BACKEND:-sqlite}"
SQLITE_PATH="${UPSTREAM_SYNC_SQLITE_PATH:-${REPO_ROOT}/.sync-queue/upstream-events.db}"
SQS_QUEUE_URL="${UPSTREAM_SYNC_SQS_QUEUE_URL:-}"
SQS_REGION="${UPSTREAM_SYNC_SQS_REGION:-${AWS_REGION:-}}"
KAFKA_BOOTSTRAP="${UPSTREAM_SYNC_KAFKA_BOOTSTRAP:-127.0.0.1:9092}"
KAFKA_TOPIC="${UPSTREAM_SYNC_KAFKA_TOPIC:-hermes-upstream-sync}"
KAFKA_GROUP_ID="${UPSTREAM_SYNC_KAFKA_GROUP_ID:-hermes-upstream-worker}"

MAX_ATTEMPTS="${UPSTREAM_SYNC_MAX_ATTEMPTS:-3}"
SYNC_TIMEOUT_SEC="${UPSTREAM_SYNC_TIMEOUT_SEC:-1800}"
POLL_INTERVAL_SEC="${UPSTREAM_SYNC_POLL_INTERVAL_SEC:-10}"
STRATEGY="${UPSTREAM_SYNC_STRATEGY:-merge}"
CONFLICT_LABEL="${UPSTREAM_SYNC_CONFLICT_LABEL:-upstream-sync-conflict}"

STRICT_RISK_GATE="${UPSTREAM_SYNC_STRICT_RISK_GATE:-1}"
ALLOW_RISK_PATHS="${UPSTREAM_SYNC_ALLOW_RISK_PATHS:-0}"
NO_TESTS="${UPSTREAM_SYNC_NO_TESTS:-0}"
NO_PR="${UPSTREAM_SYNC_NO_PR:-0}"
PARITY_DRIFT_DISABLE="${UPSTREAM_SYNC_DISABLE_PARITY_DRIFT_CHECK:-0}"
PARITY_UPSTREAM_REF="${UPSTREAM_SYNC_PARITY_UPSTREAM_REF:-upstream/main}"
PARITY_PARENT_ISSUE="${UPSTREAM_SYNC_PARITY_PARENT_ISSUE:-13}"
PARITY_LABELS="${UPSTREAM_SYNC_PARITY_LABELS:-parity,parity-upkeep}"
PARITY_OPEN_ISSUES="${UPSTREAM_SYNC_PARITY_OPEN_ISSUES:-1}"
GLOBAL_PARITY_DISABLE="${UPSTREAM_SYNC_DISABLE_GLOBAL_PARITY_CHECK:-0}"
GLOBAL_PARITY_PARENT_ISSUE="${UPSTREAM_SYNC_GLOBAL_PARITY_PARENT_ISSUE:-19}"
GLOBAL_PARITY_LABELS="${UPSTREAM_SYNC_GLOBAL_PARITY_LABELS:-parity,parity-upkeep}"
GLOBAL_PARITY_OPEN_ISSUES="${UPSTREAM_SYNC_GLOBAL_PARITY_OPEN_ISSUES:-1}"
GLOBAL_PARITY_MAX_QUEUE_COMMITS="${UPSTREAM_SYNC_GLOBAL_PARITY_MAX_QUEUE_COMMITS:-0}"
ELITE_GATE="${UPSTREAM_SYNC_ELITE_GATE:-0}"
ELITE_CMD="${UPSTREAM_SYNC_ELITE_CMD:-python3 scripts/run-elite-sync-gate.py}"
ELITE_ROLLBACK_CMD="${UPSTREAM_SYNC_ELITE_ROLLBACK_CMD:-}"

ARGS=(
  worker
  --repo-root "${REPO_ROOT}"
  --backend "${BACKEND}"
  --max-attempts "${MAX_ATTEMPTS}"
  --sync-timeout-sec "${SYNC_TIMEOUT_SEC}"
  --poll-interval-sec "${POLL_INTERVAL_SEC}"
  --strategy "${STRATEGY}"
  --conflict-label "${CONFLICT_LABEL}"
  --parity-upstream-ref "${PARITY_UPSTREAM_REF}"
  --parity-parent-issue "${PARITY_PARENT_ISSUE}"
  --parity-labels "${PARITY_LABELS}"
  --global-parity-parent-issue "${GLOBAL_PARITY_PARENT_ISSUE}"
  --global-parity-labels "${GLOBAL_PARITY_LABELS}"
  --global-parity-max-queue-commits "${GLOBAL_PARITY_MAX_QUEUE_COMMITS}"
)

case "${BACKEND}" in
  sqlite)
    ARGS+=(--sqlite-path "${SQLITE_PATH}")
    ;;
  sqs)
    [[ -n "${SQS_QUEUE_URL}" ]] || {
      echo "UPSTREAM_SYNC_SQS_QUEUE_URL is required for sqs backend" >&2
      exit 2
    }
    ARGS+=(--sqs-queue-url "${SQS_QUEUE_URL}")
    [[ -n "${SQS_REGION}" ]] && ARGS+=(--sqs-region "${SQS_REGION}")
    ;;
  kafka)
    ARGS+=(--kafka-bootstrap "${KAFKA_BOOTSTRAP}" --kafka-topic "${KAFKA_TOPIC}" --kafka-group-id "${KAFKA_GROUP_ID}")
    ;;
  *)
    echo "Unsupported UPSTREAM_SYNC_BACKEND='${BACKEND}' (expected sqlite|sqs|kafka)" >&2
    exit 2
    ;;
esac

if [[ "${STRICT_RISK_GATE}" == "1" || "${STRICT_RISK_GATE}" == "true" ]]; then
  ARGS+=(--strict-risk-gate)
fi
if [[ "${ALLOW_RISK_PATHS}" == "1" || "${ALLOW_RISK_PATHS}" == "true" ]]; then
  ARGS+=(--allow-risk-paths)
fi
if [[ "${NO_TESTS}" == "1" || "${NO_TESTS}" == "true" ]]; then
  ARGS+=(--no-tests)
fi
if [[ "${NO_PR}" == "1" || "${NO_PR}" == "true" ]]; then
  ARGS+=(--no-pr)
fi
if [[ "${PARITY_DRIFT_DISABLE}" == "1" || "${PARITY_DRIFT_DISABLE}" == "true" ]]; then
  ARGS+=(--disable-parity-drift-check)
fi
if [[ "${PARITY_OPEN_ISSUES}" == "0" || "${PARITY_OPEN_ISSUES}" == "false" ]]; then
  ARGS+=(--no-parity-open-issues)
fi
if [[ "${GLOBAL_PARITY_DISABLE}" == "1" || "${GLOBAL_PARITY_DISABLE}" == "true" ]]; then
  ARGS+=(--disable-global-parity-check)
fi
if [[ "${GLOBAL_PARITY_OPEN_ISSUES}" == "0" || "${GLOBAL_PARITY_OPEN_ISSUES}" == "false" ]]; then
  ARGS+=(--no-global-parity-open-issues)
fi
if [[ "${ELITE_GATE}" == "1" || "${ELITE_GATE}" == "true" ]]; then
  ARGS+=(--elite-gate)
  if [[ -n "${ELITE_CMD}" ]]; then
    ARGS+=(--elite-cmd "${ELITE_CMD}")
  fi
  if [[ -n "${ELITE_ROLLBACK_CMD}" ]]; then
    ARGS+=(--elite-rollback-cmd "${ELITE_ROLLBACK_CMD}")
  fi
fi

if [[ -n "${UPSTREAM_SYNC_ASSIST_CMD:-}" ]]; then
  ARGS+=(--assist-cmd "${UPSTREAM_SYNC_ASSIST_CMD}")
fi

cd "${REPO_ROOT}"
exec python3 "${REPO_ROOT}/scripts/upstream_webhook_sync.py" "${ARGS[@]}"
