#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/sync-upstream.sh [options]

Syncs upstream changes into this fork in a reproducible way.

Options:
  --repo-root <path>      Repository root (default: script parent)
  --origin <name>         Origin remote name (default: origin)
  --upstream <name>       Upstream remote name (default: upstream)
  --base-branch <name>    Branch to sync (default: main)
  --mode <mode>           branch-pr (default) | direct-main
  --no-tests              Skip post-merge test command
  --test-cmd <command>    Verification command (default: cargo test -p hermes-gateway)
  --no-pr                 Do not open a PR in branch-pr mode
  --dry-run               Show what would happen and exit
  -h, --help              Show help
EOF
}

log() {
  printf '[sync-upstream] %s\n' "$*"
}

die() {
  printf '[sync-upstream] ERROR: %s\n' "$*" >&2
  exit 1
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
ORIGIN_REMOTE="origin"
UPSTREAM_REMOTE="upstream"
BASE_BRANCH="main"
MODE="branch-pr"
RUN_TESTS="1"
TEST_CMD="cargo test -p hermes-gateway"
CREATE_PR="1"
DRY_RUN="0"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo-root)
      REPO_ROOT="${2:?missing value for --repo-root}"
      shift 2
      ;;
    --origin)
      ORIGIN_REMOTE="${2:?missing value for --origin}"
      shift 2
      ;;
    --upstream)
      UPSTREAM_REMOTE="${2:?missing value for --upstream}"
      shift 2
      ;;
    --base-branch)
      BASE_BRANCH="${2:?missing value for --base-branch}"
      shift 2
      ;;
    --mode)
      MODE="${2:?missing value for --mode}"
      shift 2
      ;;
    --no-tests)
      RUN_TESTS="0"
      shift
      ;;
    --test-cmd)
      TEST_CMD="${2:?missing value for --test-cmd}"
      shift 2
      ;;
    --no-pr)
      CREATE_PR="0"
      shift
      ;;
    --dry-run)
      DRY_RUN="1"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "Unknown argument: $1"
      ;;
  esac
done

[[ "${MODE}" == "branch-pr" || "${MODE}" == "direct-main" ]] || \
  die "--mode must be branch-pr or direct-main"

if [[ "${MODE}" == "direct-main" ]]; then
  CREATE_PR="0"
fi

command -v git >/dev/null 2>&1 || die "git is required"
if [[ "${CREATE_PR}" == "1" ]] && ! command -v gh >/dev/null 2>&1; then
  log "gh CLI not found; disabling PR creation."
  CREATE_PR="0"
fi

cd "${REPO_ROOT}"
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || \
  die "Not a git repository: ${REPO_ROOT}"

git remote get-url "${ORIGIN_REMOTE}" >/dev/null 2>&1 || \
  die "Origin remote '${ORIGIN_REMOTE}' is not configured"
git remote get-url "${UPSTREAM_REMOTE}" >/dev/null 2>&1 || \
  die "Upstream remote '${UPSTREAM_REMOTE}' is not configured"

if [[ -n "$(git status --porcelain)" ]]; then
  die "Working tree is not clean. Commit/stash changes before syncing."
fi

log "Fetching ${ORIGIN_REMOTE}/${BASE_BRANCH} and ${UPSTREAM_REMOTE}/${BASE_BRANCH}..."
git fetch "${ORIGIN_REMOTE}" "${BASE_BRANCH}" --prune
git fetch "${UPSTREAM_REMOTE}" "${BASE_BRANCH}" --prune

ORIGIN_REF="refs/remotes/${ORIGIN_REMOTE}/${BASE_BRANCH}"
UPSTREAM_REF="refs/remotes/${UPSTREAM_REMOTE}/${BASE_BRANCH}"

ORIGIN_SHA="$(git rev-parse "${ORIGIN_REF}")"
UPSTREAM_SHA="$(git rev-parse "${UPSTREAM_REF}")"

if git merge-base --is-ancestor "${UPSTREAM_REF}" "${ORIGIN_REF}"; then
  log "No upstream updates to apply. ${ORIGIN_REF} already contains ${UPSTREAM_REF}."
  exit 0
fi

TIMESTAMP="$(date -u +%Y%m%d-%H%M%S)"
SYNC_BRANCH="chore/upstream-sync-${TIMESTAMP}"
COMMITS_TO_SYNC="$(git log --oneline --no-decorate "${ORIGIN_REF}..${UPSTREAM_REF}" || true)"
if [[ -z "${COMMITS_TO_SYNC}" ]]; then
  COMMITS_TO_SYNC="(origin and upstream have diverged; merge commit will reconcile histories)"
fi

if [[ "${DRY_RUN}" == "1" ]]; then
  log "Dry run summary:"
  log "  repo_root:      ${REPO_ROOT}"
  log "  base branch:    ${BASE_BRANCH}"
  log "  origin sha:     ${ORIGIN_SHA}"
  log "  upstream sha:   ${UPSTREAM_SHA}"
  log "  sync branch:    ${SYNC_BRANCH}"
  log "  mode:           ${MODE}"
  log "  run tests:      ${RUN_TESTS}"
  log "  test command:   ${TEST_CMD}"
  log "  create pr:      ${CREATE_PR}"
  printf '%s\n' "${COMMITS_TO_SYNC}"
  exit 0
fi

log "Checking out ${BASE_BRANCH} and updating from ${ORIGIN_REMOTE}..."
git checkout "${BASE_BRANCH}"
git pull --ff-only "${ORIGIN_REMOTE}" "${BASE_BRANCH}"

log "Creating sync branch ${SYNC_BRANCH}..."
git checkout -b "${SYNC_BRANCH}"

log "Merging ${UPSTREAM_REF} into ${SYNC_BRANCH}..."
if ! git merge --no-edit "${UPSTREAM_REF}"; then
  git merge --abort || true
  die "Merge conflict detected. Resolve manually."
fi

if [[ "${RUN_TESTS}" == "1" ]]; then
  log "Running verification: ${TEST_CMD}"
  bash -lc "${TEST_CMD}"
fi

log "Pushing ${SYNC_BRANCH} to ${ORIGIN_REMOTE}..."
git push -u "${ORIGIN_REMOTE}" "${SYNC_BRANCH}"

if [[ "${MODE}" == "direct-main" ]]; then
  log "Fast-forwarding ${BASE_BRANCH} to ${SYNC_BRANCH} and pushing..."
  git checkout "${BASE_BRANCH}"
  git merge --ff-only "${SYNC_BRANCH}"
  git push "${ORIGIN_REMOTE}" "${BASE_BRANCH}"
  log "Direct-main sync complete."
  exit 0
fi

if [[ "${CREATE_PR}" == "1" ]]; then
  TITLE="chore: sync upstream ${BASE_BRANCH} (${TIMESTAMP})"
  BODY_FILE="$(mktemp)"
  trap 'rm -f "${BODY_FILE}"' EXIT
  cat > "${BODY_FILE}" <<EOF
Automated upstream sync.

- upstream ref: \`${UPSTREAM_REF}\`
- upstream sha: \`${UPSTREAM_SHA}\`
- origin sha before sync: \`${ORIGIN_SHA}\`
- verification: \`${TEST_CMD}\`

Commits pending from upstream at sync start:
\`\`\`
${COMMITS_TO_SYNC}
\`\`\`
EOF

  if gh pr create --base "${BASE_BRANCH}" --head "${SYNC_BRANCH}" --title "${TITLE}" --body-file "${BODY_FILE}"; then
    log "PR created successfully."
  else
    log "PR creation failed (branch was pushed). Create PR manually if needed."
  fi
fi

log "Sync branch prepared: ${SYNC_BRANCH}"
