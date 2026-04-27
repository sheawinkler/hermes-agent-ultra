#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/setup-upstream-webhook-launchd.sh [options]

One-command guided setup for upstream webhook launchd services.
Creates/updates launchd plists and env file, shows current config, applies optional
overrides, loads services, and prints status/log tails.

Options:
  --repo-root <path>       Repository root (default: script parent)
  --label-prefix <prefix>  launchd label prefix (default: com.hermes_agent_ultra)
  --agents-dir <path>      LaunchAgents directory (default: ~/Library/LaunchAgents)
  --env-file <path>        Runtime env file (default: ~/.hermes-agent-ultra/upstream-webhook-sync.env)
  --log-dir <path>         Log directory (default: ~/.hermes-agent-ultra/logs)
  --tail <n>               Tail size for final status output (default: 20)
  --set KEY=VALUE          Set/update env key (repeatable)
  --no-auto-secret         Do not auto-generate GITHUB_WEBHOOK_SECRET
  --non-interactive        Disable prompts; only use existing values + --set overrides
  --show-only              Show detected config and exit without loading services
  --no-load                Do not load/start services after setup
  -h, --help               Show help
USAGE
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "${SCRIPT_DIR}/.." && pwd)}"
LABEL_PREFIX="com.hermes_agent_ultra"
AGENTS_DIR="${HOME}/Library/LaunchAgents"
ENV_FILE="${HOME}/.hermes-agent-ultra/upstream-webhook-sync.env"
LOG_DIR="${HOME}/.hermes-agent-ultra/logs"
TAIL_N=20
INTERACTIVE="1"
SHOW_ONLY="0"
LOAD_SERVICES="1"
AUTO_SECRET="1"
declare -a SET_OVERRIDES=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo-root)
      REPO_ROOT="${2:?missing value for --repo-root}"
      shift 2
      ;;
    --label-prefix)
      LABEL_PREFIX="${2:?missing value for --label-prefix}"
      shift 2
      ;;
    --agents-dir)
      AGENTS_DIR="${2:?missing value for --agents-dir}"
      shift 2
      ;;
    --env-file)
      ENV_FILE="${2:?missing value for --env-file}"
      shift 2
      ;;
    --log-dir)
      LOG_DIR="${2:?missing value for --log-dir}"
      shift 2
      ;;
    --tail)
      TAIL_N="${2:?missing value for --tail}"
      shift 2
      ;;
    --set)
      SET_OVERRIDES+=("${2:?missing value for --set}")
      shift 2
      ;;
    --no-auto-secret)
      AUTO_SECRET="0"
      shift
      ;;
    --non-interactive)
      INTERACTIVE="0"
      shift
      ;;
    --show-only)
      SHOW_ONLY="1"
      LOAD_SERVICES="0"
      shift
      ;;
    --no-load)
      LOAD_SERVICES="0"
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

if [[ ! -t 0 || ! -t 1 ]]; then
  INTERACTIVE="0"
fi

LISTENER_LABEL="${LABEL_PREFIX}.upstream_webhook_listener"
WORKER_LABEL="${LABEL_PREFIX}.upstream_webhook_worker"
LISTENER_PLIST="${AGENTS_DIR}/${LISTENER_LABEL}.plist"
WORKER_PLIST="${AGENTS_DIR}/${WORKER_LABEL}.plist"
CURRENT_HOST_SHORT="$(hostname -s 2>/dev/null || hostname)"
CURRENT_HOST_FULL="$(hostname 2>/dev/null || echo "${CURRENT_HOST_SHORT}")"

INSTALL_SCRIPT="${REPO_ROOT}/scripts/install-upstream-webhook-launchd.sh"
STATUS_SCRIPT="${REPO_ROOT}/scripts/status-upstream-webhook-launchd.sh"

if [[ ! -x "${INSTALL_SCRIPT}" ]]; then
  echo "Missing installer script: ${INSTALL_SCRIPT}" >&2
  exit 2
fi
if [[ ! -x "${STATUS_SCRIPT}" ]]; then
  echo "Missing status script: ${STATUS_SCRIPT}" >&2
  exit 2
fi

set_env_value() {
  local file="$1"
  local key="$2"
  local value="$3"
  local tmp
  tmp="$(mktemp)"
  awk -v k="${key}" -v v="${value}" '
    BEGIN { done=0 }
    $0 ~ ("^" k "=") { print k "=" v; done=1; next }
    { print }
    END { if (!done) print k "=" v }
  ' "${file}" > "${tmp}"
  mv "${tmp}" "${file}"
  chmod 600 "${file}" >/dev/null 2>&1 || true
}

generate_secret() {
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex 32
    return
  fi
  python3 - <<'PY'
import secrets
print(secrets.token_hex(32))
PY
}

mask_secret() {
  local value="$1"
  local n="${#value}"
  if [[ -z "${value}" ]]; then
    echo "<unset>"
  elif [[ "${n}" -le 6 ]]; then
    echo "<set:${n} chars>"
  else
    echo "${value:0:2}***${value:n-2:2}"
  fi
}

load_env() {
  if [[ -f "${ENV_FILE}" ]]; then
    # shellcheck disable=SC1090
    source "${ENV_FILE}"
  fi
}

host_matches() {
  local allowed="$1"
  local short="$2"
  local full="$3"
  local allowed_short="${allowed%%.*}"
  [[ "${allowed}" == "${short}" || "${allowed}" == "${full}" || "${allowed_short}" == "${short}" ]]
}

print_config_summary() {
  echo
  echo "Current webhook sync configuration:"
  echo "  repo root: ${REPO_ROOT}"
  echo "  env file:  ${ENV_FILE}"
  echo "  backend:   ${UPSTREAM_SYNC_BACKEND:-sqlite}"
  echo "  repo/ref:  ${UPSTREAM_SYNC_EXPECTED_REPO:-<unset>} @ ${UPSTREAM_SYNC_EXPECTED_REF:-<unset>}"
  echo "  endpoint:  ${UPSTREAM_SYNC_HOST:-127.0.0.1}:${UPSTREAM_SYNC_PORT:-8099}${UPSTREAM_SYNC_PATH:-/github/upstream-sync}"
  echo "  strategy:  ${UPSTREAM_SYNC_STRATEGY:-merge}"
  echo "  strict gate: ${UPSTREAM_SYNC_STRICT_RISK_GATE:-1}"
  echo "  elite gate: ${UPSTREAM_SYNC_ELITE_GATE:-0}"
  echo "  elite cmd: ${UPSTREAM_SYNC_ELITE_CMD:-python3 scripts/run-elite-sync-gate.py}"
  echo "  elite rollback cmd: ${UPSTREAM_SYNC_ELITE_ROLLBACK_CMD:-<unset>}"
  echo "  parity drift check disabled: ${UPSTREAM_SYNC_DISABLE_PARITY_DRIFT_CHECK:-0}"
  echo "  parity upstream ref: ${UPSTREAM_SYNC_PARITY_UPSTREAM_REF:-upstream/main}"
  echo "  parity parent issue: ${UPSTREAM_SYNC_PARITY_PARENT_ISSUE:-13}"
  echo "  parity labels: ${UPSTREAM_SYNC_PARITY_LABELS:-parity,parity-upkeep}"
  echo "  parity auto-open issues: ${UPSTREAM_SYNC_PARITY_OPEN_ISSUES:-1}"
  echo "  global parity check disabled: ${UPSTREAM_SYNC_DISABLE_GLOBAL_PARITY_CHECK:-0}"
  echo "  global parity parent issue: ${UPSTREAM_SYNC_GLOBAL_PARITY_PARENT_ISSUE:-19}"
  echo "  global parity labels: ${UPSTREAM_SYNC_GLOBAL_PARITY_LABELS:-parity,parity-upkeep}"
  echo "  global parity auto-open issues: ${UPSTREAM_SYNC_GLOBAL_PARITY_OPEN_ISSUES:-1}"
  echo "  global parity max queue commits: ${UPSTREAM_SYNC_GLOBAL_PARITY_MAX_QUEUE_COMMITS:-0}"
  echo "  runtime role: ${UPSTREAM_SYNC_RUNTIME_ROLE:-dev}"
  echo "  allowed host: ${UPSTREAM_SYNC_ALLOWED_HOSTNAME:-<unset>} (current: ${CURRENT_HOST_FULL})"
  echo "  webhook secret: $(mask_secret "${GITHUB_WEBHOOK_SECRET:-}")"
  case "${UPSTREAM_SYNC_BACKEND:-sqlite}" in
    sqlite)
      echo "  sqlite path: ${UPSTREAM_SYNC_SQLITE_PATH:-<unset>}"
      ;;
    sqs)
      echo "  sqs queue:   ${UPSTREAM_SYNC_SQS_QUEUE_URL:-<unset>}"
      echo "  sqs region:  ${UPSTREAM_SYNC_SQS_REGION:-<unset>}"
      ;;
    kafka)
      echo "  kafka bootstrap: ${UPSTREAM_SYNC_KAFKA_BOOTSTRAP:-<unset>}"
      echo "  kafka topic:     ${UPSTREAM_SYNC_KAFKA_TOPIC:-<unset>}"
      echo "  kafka group:     ${UPSTREAM_SYNC_KAFKA_GROUP_ID:-<unset>}"
      ;;
  esac
  echo "  listener plist: ${LISTENER_PLIST} $( [[ -f "${LISTENER_PLIST}" ]] && echo '[present]' || echo '[missing]' )"
  echo "  worker plist:   ${WORKER_PLIST} $( [[ -f "${WORKER_PLIST}" ]] && echo '[present]' || echo '[missing]' )"
}

validate_config() {
  local backend="${UPSTREAM_SYNC_BACKEND:-sqlite}"
  local role="${UPSTREAM_SYNC_RUNTIME_ROLE:-dev}"
  local allowed_host="${UPSTREAM_SYNC_ALLOWED_HOSTNAME:-}"
  local ok=1

  case "${backend}" in
    sqlite|sqs|kafka) ;;
    *)
      echo "Config error: UPSTREAM_SYNC_BACKEND must be sqlite|sqs|kafka (got '${backend}')." >&2
      ok=0
      ;;
  esac

  if [[ "${role}" != "dev" ]]; then
    echo "Config error: UPSTREAM_SYNC_RUNTIME_ROLE must be 'dev' for this launchd stack." >&2
    ok=0
  fi
  if [[ -z "${allowed_host}" ]]; then
    echo "Config error: UPSTREAM_SYNC_ALLOWED_HOSTNAME must be set to your dev host." >&2
    ok=0
  elif ! host_matches "${allowed_host}" "${CURRENT_HOST_SHORT}" "${CURRENT_HOST_FULL}"; then
    echo "Config error: this machine ('${CURRENT_HOST_FULL}') does not match UPSTREAM_SYNC_ALLOWED_HOSTNAME='${allowed_host}'." >&2
    ok=0
  fi

  if [[ -z "${GITHUB_WEBHOOK_SECRET:-}" ]]; then
    echo "Warning: GITHUB_WEBHOOK_SECRET is unset. Signature verification is disabled." >&2
  fi

  case "${backend}" in
    sqlite)
      if [[ -z "${UPSTREAM_SYNC_SQLITE_PATH:-}" ]]; then
        echo "Config error: UPSTREAM_SYNC_SQLITE_PATH is required for sqlite backend." >&2
        ok=0
      fi
      ;;
    sqs)
      if [[ -z "${UPSTREAM_SYNC_SQS_QUEUE_URL:-}" ]]; then
        echo "Config error: UPSTREAM_SYNC_SQS_QUEUE_URL is required for sqs backend." >&2
        ok=0
      fi
      ;;
    kafka)
      if [[ -z "${UPSTREAM_SYNC_KAFKA_BOOTSTRAP:-}" || -z "${UPSTREAM_SYNC_KAFKA_TOPIC:-}" ]]; then
        echo "Config error: UPSTREAM_SYNC_KAFKA_BOOTSTRAP and UPSTREAM_SYNC_KAFKA_TOPIC are required for kafka backend." >&2
        ok=0
      fi
      ;;
  esac

  [[ "${ok}" -eq 1 ]]
}

echo "Preparing launchd files and env template..."
bash "${INSTALL_SCRIPT}" \
  --repo-root "${REPO_ROOT}" \
  --label-prefix "${LABEL_PREFIX}" \
  --agents-dir "${AGENTS_DIR}" \
  --env-file "${ENV_FILE}" \
  --log-dir "${LOG_DIR}" \
  --no-load >/dev/null

load_env
if [[ "${AUTO_SECRET}" == "1" && -z "${GITHUB_WEBHOOK_SECRET:-}" ]]; then
  set_env_value "${ENV_FILE}" "GITHUB_WEBHOOK_SECRET" "$(generate_secret)"
  load_env
  echo "Auto-generated GITHUB_WEBHOOK_SECRET in ${ENV_FILE}."
fi
print_config_summary

if [[ "${#SET_OVERRIDES[@]}" -gt 0 ]]; then
  echo
  echo "Applying --set overrides..."
  for pair in "${SET_OVERRIDES[@]}"; do
    if [[ "${pair}" != *=* ]]; then
      echo "Invalid --set value '${pair}'. Expected KEY=VALUE." >&2
      exit 2
    fi
    key="${pair%%=*}"
    value="${pair#*=}"
    if [[ -z "${key}" ]]; then
      echo "Invalid --set key in '${pair}'." >&2
      exit 2
    fi
    set_env_value "${ENV_FILE}" "${key}" "${value}"
    echo "  set ${key}"
  done
fi

load_env

if [[ "${INTERACTIVE}" == "1" ]]; then
  echo
  if [[ "${AUTO_SECRET}" == "0" && -z "${GITHUB_WEBHOOK_SECRET:-}" ]]; then
    read -r -s -p "Enter GITHUB_WEBHOOK_SECRET (leave empty to skip): " prompt_secret
    echo
    if [[ -n "${prompt_secret}" ]]; then
      set_env_value "${ENV_FILE}" "GITHUB_WEBHOOK_SECRET" "${prompt_secret}"
      unset prompt_secret
    fi
  fi

  load_env
  case "${UPSTREAM_SYNC_BACKEND:-sqlite}" in
    sqs)
      if [[ -z "${UPSTREAM_SYNC_SQS_QUEUE_URL:-}" ]]; then
        read -r -p "Enter UPSTREAM_SYNC_SQS_QUEUE_URL: " prompt_sqs
        if [[ -n "${prompt_sqs}" ]]; then
          set_env_value "${ENV_FILE}" "UPSTREAM_SYNC_SQS_QUEUE_URL" "${prompt_sqs}"
          unset prompt_sqs
        fi
      fi
      ;;
    kafka)
      if [[ -z "${UPSTREAM_SYNC_KAFKA_BOOTSTRAP:-}" ]]; then
        read -r -p "Enter UPSTREAM_SYNC_KAFKA_BOOTSTRAP: " prompt_bootstrap
        if [[ -n "${prompt_bootstrap}" ]]; then
          set_env_value "${ENV_FILE}" "UPSTREAM_SYNC_KAFKA_BOOTSTRAP" "${prompt_bootstrap}"
          unset prompt_bootstrap
        fi
      fi
      if [[ -z "${UPSTREAM_SYNC_KAFKA_TOPIC:-}" ]]; then
        read -r -p "Enter UPSTREAM_SYNC_KAFKA_TOPIC: " prompt_topic
        if [[ -n "${prompt_topic}" ]]; then
          set_env_value "${ENV_FILE}" "UPSTREAM_SYNC_KAFKA_TOPIC" "${prompt_topic}"
          unset prompt_topic
        fi
      fi
      ;;
  esac
fi

load_env
print_config_summary

if ! validate_config; then
  echo
  echo "Setup stopped due to config errors. Fix ${ENV_FILE} and re-run this command." >&2
  exit 2
fi

if [[ "${SHOW_ONLY}" == "1" ]]; then
  echo
  echo "Show-only mode complete."
  exit 0
fi

if [[ "${LOAD_SERVICES}" == "1" ]]; then
  echo
  echo "Loading launchd services..."
  bash "${INSTALL_SCRIPT}" \
    --repo-root "${REPO_ROOT}" \
    --label-prefix "${LABEL_PREFIX}" \
    --agents-dir "${AGENTS_DIR}" \
    --env-file "${ENV_FILE}" \
    --log-dir "${LOG_DIR}" >/dev/null
else
  echo
  echo "Skipped launchd load/start (--no-load)."
fi

echo
echo "Final launchd status:"
bash "${STATUS_SCRIPT}" \
  --label-prefix "${LABEL_PREFIX}" \
  --log-dir "${LOG_DIR}" \
  --tail "${TAIL_N}"
