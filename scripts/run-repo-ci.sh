#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
UPSTREAM_REMOTE="${UPSTREAM_REMOTE:-upstream}"
UPSTREAM_URL="${UPSTREAM_URL:-https://github.com/NousResearch/hermes-agent.git}"
UPSTREAM_REF="${UPSTREAM_REF:-${UPSTREAM_REMOTE}/main}"
SKIP_UPSTREAM_FETCH="${SKIP_UPSTREAM_FETCH:-false}"

usage() {
  cat <<'USAGE'
Usage: scripts/run-repo-ci.sh [--skip-upstream-fetch]

Run the repository-local CI contract. This is the authoritative gate for local
changes; hosted GitHub Actions are a convenience mirror when they run.

Environment:
  CARGO_TARGET_DIR       Optional external Cargo target directory.
  CARGO_INCREMENTAL      Optional Cargo incremental setting.
  UPSTREAM_REMOTE        Upstream remote name (default: upstream).
  UPSTREAM_URL           Upstream remote URL (default: NousResearch/hermes-agent).
  UPSTREAM_REF           Upstream ref used by parity gates (default: upstream/main).
  SKIP_UPSTREAM_FETCH    true/false; skip refreshing upstream/main when true.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-upstream-fetch)
      SKIP_UPSTREAM_FETCH=true
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

run() {
  echo
  echo "[repo-ci] $*"
  "$@"
}

refresh_upstream() {
  if [[ "${SKIP_UPSTREAM_FETCH}" == "true" ]]; then
    echo "[repo-ci] skipping upstream fetch; using ${UPSTREAM_REF} as-is"
    return
  fi
  if ! git -C "${REPO_ROOT}" remote get-url "${UPSTREAM_REMOTE}" >/dev/null 2>&1; then
    git -C "${REPO_ROOT}" remote add "${UPSTREAM_REMOTE}" "${UPSTREAM_URL}"
  fi
  run git -C "${REPO_ROOT}" fetch --no-tags --depth=1 \
    "${UPSTREAM_REMOTE}" \
    "+refs/heads/main:refs/remotes/${UPSTREAM_REMOTE}/main"
}

cd "${REPO_ROOT}"

refresh_upstream

run python3 scripts/generate-parity-matrix.py --local-ref HEAD
run python3 scripts/generate-workstream-status.py
run python3 scripts/generate-test-intent-mapping.py
run python3 scripts/generate-test-coverage-audit.py --check
run python3 scripts/generate-adapter-matrix.py
run python3 scripts/validate-intentional-divergence.py --check --allow-warnings
run python3 scripts/generate-shared-diff-backlog.py --local-ref HEAD --no-fetch
run python3 scripts/generate-upstream-patch-queue.py --local-ref HEAD --max-commits 0
run python3 scripts/generate-global-parity-proof.py
run python3 scripts/generate-release-readiness-summary.py --check

run cargo fmt --all --check
run bash scripts/clippy-warning-gate.sh --check
run cargo test -p hermes-source-parity-tests --test rust_module_size_policy -- --nocapture
run bash scripts/check-runtime-placeholders.sh
run cargo test --workspace
run cargo test -p hermes-parity-tests
run cargo test -p hermes-source-parity-tests --test cli_command_contract
run cargo test -p hermes-protocol-parity-tests --test protocol_differential_contracts
run python3 scripts/run-upstream-slash-parity-gate.py --upstream-ref "${UPSTREAM_REF}"
run python3 scripts/run-upstream-surface-coverage-gate.py --repo-root . --upstream-ref "${UPSTREAM_REF}" --local-ref HEAD
run cargo test -p hermes-source-parity-tests --test global_parity_governance

for script in \
  scripts/upstream_webhook_sync.py \
  scripts/publish_automation_summary.py \
  scripts/run-golden-parity-harness.py \
  scripts/run-upstream-slash-parity-gate.py \
  scripts/run-upstream-surface-coverage-gate.py \
  scripts/generate-shared-diff-backlog.py \
  scripts/generate-test-coverage-audit.py \
  scripts/generate-release-readiness-summary.py \
  scripts/release_secret_scan.py; do
  run python3 -m py_compile "${script}"
done

echo
printf '[repo-ci] PASS\n'
