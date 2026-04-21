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

ARGS=(
  worker
  --repo-root "${REPO_ROOT}"
  --backend "${BACKEND}"
  --max-attempts "${MAX_ATTEMPTS}"
  --sync-timeout-sec "${SYNC_TIMEOUT_SEC}"
  --poll-interval-sec "${POLL_INTERVAL_SEC}"
  --strategy "${STRATEGY}"
  --conflict-label "${CONFLICT_LABEL}"
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

if [[ -n "${UPSTREAM_SYNC_ASSIST_CMD:-}" ]]; then
  ARGS+=(--assist-cmd "${UPSTREAM_SYNC_ASSIST_CMD}")
fi

cd "${REPO_ROOT}"
exec python3 "${REPO_ROOT}/scripts/upstream_webhook_sync.py" "${ARGS[@]}"
