#!/usr/bin/env bash
set -euo pipefail

REPO="${REPO:-sheawinkler/hermes-agent-ultra}"
VERSION="${VERSION:-v0.21.3}"
KEEP_TMP="${KEEP_TMP:-false}"
RUN_SETUP_HELP="${RUN_SETUP_HELP:-true}"
HERMES_SMOKE_COMMAND_TIMEOUT_SECONDS="${HERMES_SMOKE_COMMAND_TIMEOUT_SECONDS:-30}"

usage() {
  cat <<'USAGE'
Usage: scripts/smoke-release-artifact.sh [--version TAG] [--repo OWNER/REPO] [--keep-tmp]

Install Hermes Agent Ultra from published GitHub release artifacts into a
throwaway directory, then verify the released binary surfaces without touching
user/global installs.

Checks:
  - downloads the release install.sh from GitHub Releases, not the local tree
  - fails if the installer falls back to building from source
  - installs hermes-agent-ultra and hermes-ultra into a temp bin directory
  - leaves an existing hermes command untouched unless legacy alias is opted in
  - verifies version, setup help, auth status, memory status, route health,
    systems release/status, and one-true-harness tool registry entries

Environment:
  HERMES_SMOKE_COMMAND_TIMEOUT_SECONDS
                         Per-command probe timeout in seconds (default: 30;
                         0 disables)
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION="${2:?--version requires a tag}"
      shift 2
      ;;
    --repo)
      REPO="${2:?--repo requires owner/repo}"
      shift 2
      ;;
    --keep-tmp)
      KEEP_TMP=true
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

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "Missing required command: $1" >&2
    exit 1
  }
}

assert_contains() {
  local file="$1"
  local expected="$2"
  if ! grep -Fq -- "$expected" "$file"; then
    echo "Expected ${file} to contain: ${expected}" >&2
    echo "--- ${file} ---" >&2
    sed -n '1,220p' "$file" >&2 || true
    exit 1
  fi
}

assert_not_contains() {
  local file="$1"
  local unexpected="$2"
  if grep -Fqi -- "$unexpected" "$file"; then
    echo "Unexpected ${file} content: ${unexpected}" >&2
    echo "--- ${file} ---" >&2
    sed -n '1,220p' "$file" >&2 || true
    exit 1
  fi
}

run_capture() {
  local label="$1"
  shift
  local out="${TMP_ROOT}/${label}.out"
  echo "[release-smoke] ${label}: $*" >&2

  if [[ "${HERMES_SMOKE_COMMAND_TIMEOUT_SECONDS}" == "0" ]]; then
    HERMES_HOME="${HERMES_HOME_SMOKE}" "$@" >"${out}" 2>&1
    echo "${out}"
    return
  fi

  HERMES_HOME="${HERMES_HOME_SMOKE}" "$@" >"${out}" 2>&1 &
  local pid="$!"
  local elapsed=0
  local status=0
  while kill -0 "${pid}" >/dev/null 2>&1; do
    if (( elapsed >= HERMES_SMOKE_COMMAND_TIMEOUT_SECONDS )); then
      {
        echo ""
        echo "Command timed out after ${HERMES_SMOKE_COMMAND_TIMEOUT_SECONDS}s: $*"
      } >>"${out}"
      kill "${pid}" >/dev/null 2>&1 || true
      sleep 1
      kill -9 "${pid}" >/dev/null 2>&1 || true
      wait "${pid}" >/dev/null 2>&1 || true
      echo "Release smoke probe '${label}' timed out." >&2
      echo "--- ${out} ---" >&2
      sed -n '1,220p' "${out}" >&2 || true
      exit 124
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done
  wait "${pid}" || status="$?"
  if [[ "${status}" -ne 0 ]]; then
    echo "Release smoke probe '${label}' failed with status ${status}." >&2
    echo "--- ${out} ---" >&2
    sed -n '1,220p' "${out}" >&2 || true
    exit "${status}"
  fi
  echo "${out}"
}

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    echo "Missing required command: shasum or sha256sum" >&2
    exit 1
  fi
}

need_cmd curl
need_cmd grep
need_cmd mktemp
need_cmd sed

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/hermes-release-smoke.XXXXXX")"
if [[ "${KEEP_TMP}" != "true" ]]; then
  trap 'rm -rf "${TMP_ROOT}"' EXIT
else
  echo "[release-smoke] keeping temp dir: ${TMP_ROOT}"
fi

BIN_DIR="${TMP_ROOT}/bin"
HERMES_HOME_SMOKE="${TMP_ROOT}/home"
INSTALLER="${TMP_ROOT}/install.sh"
INSTALL_LOG="${TMP_ROOT}/install.log"
mkdir -p "${BIN_DIR}" "${HERMES_HOME_SMOKE}"

# Sentinel for upstream NousResearch Hermes coexistence. The installer must not
# replace this unless INSTALL_LEGACY_ALIAS is explicitly true.
cat > "${BIN_DIR}/hermes" <<'SENTINEL'
#!/usr/bin/env bash
echo upstream-hermes-sentinel
SENTINEL
chmod +x "${BIN_DIR}/hermes"

INSTALLER_URL="https://github.com/${REPO}/releases/download/${VERSION}/install.sh"
echo "[release-smoke] downloading installer: ${INSTALLER_URL}"
curl -fsSL "${INSTALLER_URL}" -o "${INSTALLER}"
chmod +x "${INSTALLER}"

PATH="${BIN_DIR}:${PATH}" \
INSTALL_LEGACY_ALIAS=false \
RUN_SETUP_MODE=never \
HERMES_INSTALL_PROBE_TIMEOUT_SECONDS=15 \
"${INSTALLER}" "${VERSION}" --dir "${BIN_DIR}" --hermes-home "${HERMES_HOME_SMOKE}" --skip-setup \
  >"${INSTALL_LOG}" 2>&1

assert_contains "${INSTALL_LOG}" "Installing hermes-agent-ultra from https://github.com/${REPO}/releases/download/${VERSION}/"
assert_contains "${INSTALL_LOG}" "Legacy hermes alias not installed by default"
assert_contains "${INSTALL_LOG}" "Installed to ${BIN_DIR}/hermes-agent-ultra"
assert_not_contains "${INSTALL_LOG}" "building from source"
assert_not_contains "${INSTALL_LOG}" "Release asset not available"

test -x "${BIN_DIR}/hermes-agent-ultra"
test -L "${BIN_DIR}/hermes-ultra"
if [[ "$("${BIN_DIR}/hermes")" != "upstream-hermes-sentinel" ]]; then
  echo "Installer clobbered existing hermes command" >&2
  exit 1
fi

VERSION_OUT="$(run_capture version "${BIN_DIR}/hermes-ultra" --version)"
assert_contains "${VERSION_OUT}" "${VERSION#v}"

if [[ "${RUN_SETUP_HELP}" == "true" ]]; then
  SETUP_HELP_OUT="$(run_capture setup-help "${BIN_DIR}/hermes-ultra" setup --help)"
  assert_contains "${SETUP_HELP_OUT}" "Run the interactive setup wizard"
fi

AUTH_OUT="$(run_capture auth-status "${BIN_DIR}/hermes-ultra" auth status openai)"
assert_contains "${AUTH_OUT}" "Auth status"
assert_contains "${AUTH_OUT}" "provider='openai'"

MEMORY_OUT="$(run_capture memory-status "${BIN_DIR}/hermes-ultra" memory status)"
assert_contains "${MEMORY_OUT}" "Memory provider"

ROUTE_HEALTH_OUT="$(run_capture route-health "${BIN_DIR}/hermes-ultra" route-health show --json)"
assert_contains "${ROUTE_HEALTH_OUT}" '"summary"'
assert_contains "${ROUTE_HEALTH_OUT}" '"overall"'

SYSTEMS_OUT="$(run_capture systems-status "${BIN_DIR}/hermes-ultra" systems status --json)"
assert_contains "${SYSTEMS_OUT}" '"kind": "hermes.systems.status"'
assert_contains "${SYSTEMS_OUT}" "Provider capability registry"
assert_contains "${SYSTEMS_OUT}" "Release gate"

RELEASE_OUT="$(run_capture systems-release "${BIN_DIR}/hermes-ultra" systems release --json)"
assert_contains "${RELEASE_OUT}" '"kind": "hermes.systems.release_gate"'
assert_contains "${RELEASE_OUT}" '"passed": true'

TOOLS_OUT="$(run_capture tools-list "${BIN_DIR}/hermes-ultra" tools list)"
for surface in harness_cockpit tool_policy_simulate objective_snapshot ops_snapshot integrations_snapshot auth_snapshot skills_list skill_view; do
  assert_contains "${TOOLS_OUT}" "${surface}"
done

INSTALL_SHA="$(sha256_file "${INSTALLER}")"
BIN_SHA="$(sha256_file "${BIN_DIR}/hermes-agent-ultra")"
cat <<REPORT
[release-smoke] PASS
version=${VERSION}
repo=${REPO}
temp_dir=${TMP_ROOT}
installer_sha256=${INSTALL_SHA}
binary_sha256=${BIN_SHA}
canonical_bin=${BIN_DIR}/hermes-agent-ultra
primary_bin=${BIN_DIR}/hermes-ultra
legacy_hermes=$(${BIN_DIR}/hermes)
REPORT
