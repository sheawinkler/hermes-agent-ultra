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
    echo "Refusing to start listener: UPSTREAM_SYNC_RUNTIME_ROLE='${ROLE}' (dev required)." >&2
    exit 3
  fi
  if [[ -n "${ALLOWED_HOSTNAME}" ]] && ! host_matches "${ALLOWED_HOSTNAME}" "${CURRENT_HOST_SHORT}" "${CURRENT_HOST_FULL}"; then
    echo "Refusing to start listener on host '${CURRENT_HOST_FULL}'. Allowed host is '${ALLOWED_HOSTNAME}'." >&2
    exit 3
  fi
fi

BACKEND="${UPSTREAM_SYNC_BACKEND:-sqlite}"
HOST="${UPSTREAM_SYNC_HOST:-127.0.0.1}"
PORT="${UPSTREAM_SYNC_PORT:-8099}"
PATH_ROUTE="${UPSTREAM_SYNC_PATH:-/github/upstream-sync}"
EXPECTED_REPO="${UPSTREAM_SYNC_EXPECTED_REPO:-NousResearch/hermes-agent}"
EXPECTED_REF="${UPSTREAM_SYNC_EXPECTED_REF:-refs/heads/main}"

SQLITE_PATH="${UPSTREAM_SYNC_SQLITE_PATH:-${REPO_ROOT}/.sync-queue/upstream-events.db}"
SQS_QUEUE_URL="${UPSTREAM_SYNC_SQS_QUEUE_URL:-}"
SQS_REGION="${UPSTREAM_SYNC_SQS_REGION:-${AWS_REGION:-}}"
KAFKA_BOOTSTRAP="${UPSTREAM_SYNC_KAFKA_BOOTSTRAP:-127.0.0.1:9092}"
KAFKA_TOPIC="${UPSTREAM_SYNC_KAFKA_TOPIC:-hermes-upstream-sync}"

ARGS=(
  listen
  --host "${HOST}"
  --port "${PORT}"
  --path "${PATH_ROUTE}"
  --expected-repo "${EXPECTED_REPO}"
  --expected-ref "${EXPECTED_REF}"
  --backend "${BACKEND}"
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
    ARGS+=(--kafka-bootstrap "${KAFKA_BOOTSTRAP}" --kafka-topic "${KAFKA_TOPIC}")
    ;;
  *)
    echo "Unsupported UPSTREAM_SYNC_BACKEND='${BACKEND}' (expected sqlite|sqs|kafka)" >&2
    exit 2
    ;;
esac

cd "${REPO_ROOT}"
exec python3 "${REPO_ROOT}/scripts/upstream_webhook_sync.py" "${ARGS[@]}"
