#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/sync-upstream.sh [options]

Syncs upstream changes into this fork in a reproducible way.

Options:
  --repo-root <path>        Repository root (default: script parent)
  --origin <name>           Origin remote name (default: origin)
  --upstream <name>         Upstream remote name (default: upstream)
  --base-branch <name>      Branch to sync (default: main)
  --mode <mode>             branch-pr (default) | direct-main
  --strategy <strategy>     merge (default) | cherry-pick
  --report-dir <path>       Report directory (default: <repo>/.sync-reports)
  --conflict-label <label>  Label for conflict issues (default: upstream-sync-conflict)
  --no-conflict-issue       Disable auto issue creation on conflicts
  --strict-risk-gate        Block sync when high-risk file paths changed upstream
  --no-strict-risk-gate     Disable strict high-risk path blocking (default)
  --allow-risk-paths        Override strict risk gate for this run
  --risk-paths-file <path>  Glob pattern file for high-risk path checks
  --no-tests                Skip post-sync verification command
  --test-cmd <command>      Verification command (default: cargo test -p hermes-gateway)
  --redteam-gate            Run adversarial red-team gate after verification (default)
  --no-redteam-gate         Skip adversarial red-team gate
  --redteam-cmd <command>   Red-team gate command (default: python3 scripts/run-redteam-gate.py)
  --elite-gate              Run consolidated elite sync gate (optional)
  --no-elite-gate           Skip consolidated elite sync gate (default)
  --elite-cmd <command>     Elite gate command (default: python3 scripts/run-elite-sync-gate.py)
  --elite-rollback-cmd <c>  Optional rollback command executed by elite gate on failure
  --no-doc-refresh          Skip generated parity/readme doc refresh
  --doc-refresh-cmd <cmd>   Override generated docs refresh command
  --no-pr                   Do not open a PR in branch-pr mode
  --draft-pr                Open PR as draft in branch-pr mode
  --pr-labels <csv>         Labels to apply on created PR (default: upstream-sync,parity-sync)
  --dry-run                 Show what would happen, emit report, and exit
  -h, --help                Show help
USAGE
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
SYNC_STRATEGY="merge"
RUN_TESTS="1"
TEST_CMD="cargo test -p hermes-gateway"
RUN_REDTEAM_GATE="${RUN_REDTEAM_GATE:-1}"
REDTEAM_CMD="${REDTEAM_CMD:-python3 scripts/run-redteam-gate.py}"
RUN_ELITE_GATE="${RUN_ELITE_GATE:-0}"
ELITE_CMD="${ELITE_CMD:-python3 scripts/run-elite-sync-gate.py}"
ELITE_ROLLBACK_CMD="${ELITE_ROLLBACK_CMD:-}"
RUN_DOC_REFRESH="${RUN_DOC_REFRESH:-1}"
DOC_REFRESH_CMD="${DOC_REFRESH_CMD:-python3 scripts/generate-parity-dashboard.py --repo-root . && python3 scripts/generate-readme-sync-status.py --repo-root .}"
CREATE_PR="1"
PR_DRAFT="0"
PR_LABELS="${PR_LABELS:-upstream-sync,parity-sync}"
CREATE_CONFLICT_ISSUE="1"
CONFLICT_LABEL="upstream-sync-conflict"
STRICT_RISK_GATE="${STRICT_RISK_GATE:-0}"
ALLOW_RISK_PATHS="0"
RISK_MATCH_COUNT="0"
RISK_PATHS_FILE=""
DRY_RUN="0"
REPORT_DIR=""

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
    --strategy)
      SYNC_STRATEGY="${2:?missing value for --strategy}"
      shift 2
      ;;
    --report-dir)
      REPORT_DIR="${2:?missing value for --report-dir}"
      shift 2
      ;;
    --conflict-label)
      CONFLICT_LABEL="${2:?missing value for --conflict-label}"
      shift 2
      ;;
    --no-conflict-issue)
      CREATE_CONFLICT_ISSUE="0"
      shift
      ;;
    --strict-risk-gate)
      STRICT_RISK_GATE="1"
      shift
      ;;
    --no-strict-risk-gate)
      STRICT_RISK_GATE="0"
      shift
      ;;
    --allow-risk-paths)
      ALLOW_RISK_PATHS="1"
      shift
      ;;
    --risk-paths-file)
      RISK_PATHS_FILE="${2:?missing value for --risk-paths-file}"
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
    --redteam-gate)
      RUN_REDTEAM_GATE="1"
      shift
      ;;
    --no-redteam-gate)
      RUN_REDTEAM_GATE="0"
      shift
      ;;
    --redteam-cmd)
      REDTEAM_CMD="${2:?missing value for --redteam-cmd}"
      shift 2
      ;;
    --elite-gate)
      RUN_ELITE_GATE="1"
      shift
      ;;
    --no-elite-gate)
      RUN_ELITE_GATE="0"
      shift
      ;;
    --elite-cmd)
      ELITE_CMD="${2:?missing value for --elite-cmd}"
      shift 2
      ;;
    --elite-rollback-cmd)
      ELITE_ROLLBACK_CMD="${2:?missing value for --elite-rollback-cmd}"
      shift 2
      ;;
    --no-doc-refresh)
      RUN_DOC_REFRESH="0"
      shift
      ;;
    --doc-refresh-cmd)
      DOC_REFRESH_CMD="${2:?missing value for --doc-refresh-cmd}"
      shift 2
      ;;
    --no-pr)
      CREATE_PR="0"
      shift
      ;;
    --draft-pr)
      PR_DRAFT="1"
      shift
      ;;
    --pr-labels)
      PR_LABELS="${2:?missing value for --pr-labels}"
      shift 2
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
[[ "${SYNC_STRATEGY}" == "merge" || "${SYNC_STRATEGY}" == "cherry-pick" ]] || \
  die "--strategy must be merge or cherry-pick"

if [[ "${MODE}" == "direct-main" ]]; then
  CREATE_PR="0"
fi

command -v git >/dev/null 2>&1 || die "git is required"

if [[ -z "${REPORT_DIR}" ]]; then
  REPORT_DIR="${REPO_ROOT}/.sync-reports"
fi
if [[ -z "${RISK_PATHS_FILE}" ]]; then
  RISK_PATHS_FILE="${REPO_ROOT}/scripts/upstream-risk-paths.txt"
fi
mkdir -p "${REPORT_DIR}"

if [[ "${CREATE_PR}" == "1" ]] && ! command -v gh >/dev/null 2>&1; then
  log "gh CLI not found; disabling PR creation."
  CREATE_PR="0"
fi
if [[ "${CREATE_CONFLICT_ISSUE}" == "1" ]] && ! command -v gh >/dev/null 2>&1; then
  log "gh CLI not found; disabling conflict issue creation."
  CREATE_CONFLICT_ISSUE="0"
fi

cd "${REPO_ROOT}"
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || \
  die "Not a git repository: ${REPO_ROOT}"

git remote get-url "${ORIGIN_REMOTE}" >/dev/null 2>&1 || \
  die "Origin remote '${ORIGIN_REMOTE}' is not configured"
git remote get-url "${UPSTREAM_REMOTE}" >/dev/null 2>&1 || \
  die "Upstream remote '${UPSTREAM_REMOTE}' is not configured"

UPSTREAM_URL="$(git remote get-url "${UPSTREAM_REMOTE}")"
EXPECTED_UPSTREAM_REPO="${EXPECTED_UPSTREAM_REPO:-NousResearch/hermes-agent}"
ALLOW_NON_OFFICIAL_UPSTREAM="${ALLOW_NON_OFFICIAL_UPSTREAM:-0}"
UPSTREAM_URL_LOWER="${UPSTREAM_URL,,}"
EXPECTED_UPSTREAM_REPO_LOWER="${EXPECTED_UPSTREAM_REPO,,}"
if [[ "${ALLOW_NON_OFFICIAL_UPSTREAM}" != "1" ]]; then
  if [[ "${UPSTREAM_URL_LOWER}" != *"${EXPECTED_UPSTREAM_REPO_LOWER}"* ]]; then
    die "Upstream remote '${UPSTREAM_REMOTE}' URL '${UPSTREAM_URL}' is not '${EXPECTED_UPSTREAM_REPO}'. Set ALLOW_NON_OFFICIAL_UPSTREAM=1 to bypass."
  fi
fi

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
TIMESTAMP="$(date -u +%Y%m%d-%H%M%S)"
SYNC_BRANCH="chore/upstream-sync-${TIMESTAMP}"
ROLLBACK_TAG="rollback/upstream-sync-${TIMESTAMP}"
REPORT_FILE="${REPORT_DIR}/upstream-sync-${TIMESTAMP}.txt"

append_report() {
  printf '%s\n' "$*" >> "${REPORT_FILE}"
}

create_report_header() {
  : > "${REPORT_FILE}"
  append_report "# Upstream Sync Report"
  append_report ""
  append_report "timestamp_utc: ${TIMESTAMP}"
  append_report "repo_root: ${REPO_ROOT}"
  append_report "mode: ${MODE}"
  append_report "strategy: ${SYNC_STRATEGY}"
  append_report "strict_risk_gate: ${STRICT_RISK_GATE}"
  append_report "run_redteam_gate: ${RUN_REDTEAM_GATE}"
  append_report "redteam_cmd: ${REDTEAM_CMD}"
  append_report "run_elite_gate: ${RUN_ELITE_GATE}"
  append_report "elite_cmd: ${ELITE_CMD}"
  append_report "elite_rollback_cmd: ${ELITE_ROLLBACK_CMD}"
  append_report "draft_pr: ${PR_DRAFT}"
  append_report "allow_risk_paths: ${ALLOW_RISK_PATHS}"
  append_report "pr_labels: ${PR_LABELS}"
  append_report "risk_paths_file: ${RISK_PATHS_FILE}"
  append_report "upstream_url: ${UPSTREAM_URL}"
  append_report "expected_upstream_repo: ${EXPECTED_UPSTREAM_REPO}"
  append_report "allow_non_official_upstream: ${ALLOW_NON_OFFICIAL_UPSTREAM}"
  append_report "origin_ref: ${ORIGIN_REF}"
  append_report "upstream_ref: ${UPSTREAM_REF}"
  append_report "origin_sha: ${ORIGIN_SHA}"
  append_report "upstream_sha: ${UPSTREAM_SHA}"
  append_report ""
}

create_report_header

if git merge-base --is-ancestor "${UPSTREAM_REF}" "${ORIGIN_REF}"; then
  append_report "status: no-updates"
  log "No upstream updates to apply. ${ORIGIN_REF} already contains ${UPSTREAM_REF}."
  log "Report: ${REPORT_FILE}"
  exit 0
fi

COMMITS_TO_SYNC="$(git log --oneline --no-decorate "${ORIGIN_REF}..${UPSTREAM_REF}" || true)"
if [[ -z "${COMMITS_TO_SYNC}" ]]; then
  COMMITS_TO_SYNC="(origin and upstream diverged; merge strategy required)"
fi

DIFF_STAT="$(git diff --stat "${ORIGIN_REF}..${UPSTREAM_REF}" || true)"
DIFF_NAMES="$(git diff --name-status "${ORIGIN_REF}..${UPSTREAM_REF}" || true)"
DIFF_FILES="$(git diff --name-only "${ORIGIN_REF}..${UPSTREAM_REF}" || true)"

append_report "## Pending Upstream Commits"
append_report '```'
append_report "${COMMITS_TO_SYNC}"
append_report '```'
append_report ""
append_report "## Diff Stat"
append_report '```'
append_report "${DIFF_STAT}"
append_report '```'
append_report ""
append_report "## Files"
append_report '```'
append_report "${DIFF_NAMES}"
append_report '```'
append_report ""

publish_risk_issue() {
  local risk_report="$1"
  if [[ "${CREATE_CONFLICT_ISSUE}" != "1" ]]; then
    return 0
  fi

  gh label create "${CONFLICT_LABEL}" --color EAB308 --description "Automated upstream sync risk gate" >/dev/null 2>&1 || true

  local title="upstream sync blocked by strict risk gate (${BASE_BRANCH}, ${TIMESTAMP})"
  local body_file
  body_file="$(mktemp)"
  {
    echo "Automated upstream sync was blocked by strict risk gating."
    echo
    echo "- mode: \`${MODE}\`"
    echo "- strategy: \`${SYNC_STRATEGY}\`"
    echo "- strict risk gate: \`${STRICT_RISK_GATE}\`"
    echo "- allow risk paths: \`${ALLOW_RISK_PATHS}\`"
    echo "- risk file: \`${RISK_PATHS_FILE}\`"
    echo "- report: \`${risk_report}\`"
    echo
    echo "Review the matched files and rerun with explicit approval if intended."
  } > "${body_file}"

  if gh issue create --title "${title}" --label "${CONFLICT_LABEL}" --body-file "${body_file}" >/dev/null 2>&1; then
    log "Strict-risk issue created with label '${CONFLICT_LABEL}'."
  else
    log "Failed to create strict-risk issue automatically; review report ${risk_report}."
  fi
  rm -f "${body_file}"
}

evaluate_risk_gate() {
  if [[ "${STRICT_RISK_GATE}" != "1" ]]; then
    return 0
  fi

  if [[ ! -f "${RISK_PATHS_FILE}" ]]; then
    die "Strict risk gate enabled but pattern file not found: ${RISK_PATHS_FILE}"
  fi

  mapfile -t changed_files < <(printf '%s\n' "${DIFF_FILES}" | sed '/^\s*$/d')
  if [[ "${#changed_files[@]}" -eq 0 ]]; then
    return 0
  fi

  local matches=()
  while IFS= read -r pattern; do
    [[ -z "${pattern}" || "${pattern}" =~ ^[[:space:]]*# ]] && continue
    for path in "${changed_files[@]}"; do
      if [[ "${path}" == ${pattern} ]]; then
        matches+=("${pattern} -> ${path}")
      fi
    done
  done < "${RISK_PATHS_FILE}"

  if [[ "${#matches[@]}" -eq 0 ]]; then
    append_report "risk_gate_status: pass"
    RISK_MATCH_COUNT="0"
    return 0
  fi
  RISK_MATCH_COUNT="${#matches[@]}"

  {
    echo "## Strict Risk Gate Matches"
    echo '```'
    printf '%s\n' "${matches[@]}"
    echo '```'
    echo
  } >> "${REPORT_FILE}"

  if [[ "${ALLOW_RISK_PATHS}" == "1" ]]; then
    append_report "risk_gate_status: bypassed"
    append_report "risk_gate_bypass: --allow-risk-paths"
    log "Strict risk gate matched paths, but bypass is enabled for this run."
    return 0
  fi

  append_report "risk_gate_status: blocked"
  if [[ "${DRY_RUN}" == "1" ]]; then
    append_report "risk_gate_status_detail: blocked_in_dry_run"
    log "Strict risk gate would block sync (dry-run)."
    return 1
  fi
  publish_risk_issue "${REPORT_FILE}"
  die "Strict risk gate blocked sync due to high-risk path changes. Review ${REPORT_FILE}."
}

evaluate_risk_gate

if [[ "${DRY_RUN}" == "1" ]]; then
  append_report "status: dry-run"
  log "Dry run complete."
  log "Report: ${REPORT_FILE}"
  exit 0
fi

publish_conflict_issue() {
  local conflict_report="$1"
  local reason="$2"
  local failed_commit="$3"
  if [[ "${CREATE_CONFLICT_ISSUE}" != "1" ]]; then
    return 0
  fi

  gh label create "${CONFLICT_LABEL}" --color E11D48 --description "Automated upstream sync conflict" >/dev/null 2>&1 || true

  local title="upstream sync conflict (${BASE_BRANCH}, ${TIMESTAMP})"
  local body_file
  body_file="$(mktemp)"
  {
    echo "Automated upstream sync hit a conflict."
    echo
    echo "- mode: \\`${MODE}\\`"
    echo "- strategy: \\`${SYNC_STRATEGY}\\`"
    echo "- reason: \\`${reason}\\`"
    if [[ -n "${failed_commit}" ]]; then
      echo "- failed commit: \\`${failed_commit}\\`"
    fi
    if [[ "${SYNC_STRATEGY}" == "cherry-pick" ]]; then
      echo "- rollback tag: \\`${ROLLBACK_TAG}\\`"
    fi
    echo "- report: \\`${conflict_report}\\`"
    echo
    echo "See local report for conflicted files and recovery commands."
  } > "${body_file}"

  if gh issue create --title "${title}" --label "${CONFLICT_LABEL}" --body-file "${body_file}" >/dev/null 2>&1; then
    log "Conflict issue created with label '${CONFLICT_LABEL}'."
  else
    log "Failed to create conflict issue automatically; review report ${conflict_report}."
  fi
  rm -f "${body_file}"
}

handle_conflict() {
  local reason="$1"
  local failed_commit="${2:-}"
  local conflict_report="${REPORT_DIR}/upstream-sync-${TIMESTAMP}-conflict.txt"
  local conflicted_files
  conflicted_files="$(git diff --name-only --diff-filter=U || true)"

  if [[ "${SYNC_STRATEGY}" == "cherry-pick" ]]; then
    git tag -f "${ROLLBACK_TAG}" HEAD >/dev/null 2>&1 || true
    git cherry-pick --abort >/dev/null 2>&1 || true
  else
    git merge --abort >/dev/null 2>&1 || true
  fi

  {
    echo "# Upstream Sync Conflict"
    echo
    echo "timestamp_utc: ${TIMESTAMP}"
    echo "mode: ${MODE}"
    echo "strategy: ${SYNC_STRATEGY}"
    echo "reason: ${reason}"
    if [[ -n "${failed_commit}" ]]; then
      echo "failed_commit: ${failed_commit}"
    fi
    echo "origin_sha: ${ORIGIN_SHA}"
    echo "upstream_sha: ${UPSTREAM_SHA}"
    if [[ "${SYNC_STRATEGY}" == "cherry-pick" ]]; then
      echo "rollback_tag: ${ROLLBACK_TAG}"
      echo "rollback_hint: git checkout ${SYNC_BRANCH} && git reset --hard ${ROLLBACK_TAG}"
    fi
    echo
    echo "## Conflicted files"
    echo '```'
    echo "${conflicted_files}"
    echo '```'
  } > "${conflict_report}"

  append_report "status: conflict"
  append_report "conflict_reason: ${reason}"
  append_report "conflict_report: ${conflict_report}"
  if [[ "${SYNC_STRATEGY}" == "cherry-pick" ]]; then
    append_report "rollback_tag: ${ROLLBACK_TAG}"
  fi

  publish_conflict_issue "${conflict_report}" "${reason}" "${failed_commit}"

  die "${reason}. See ${conflict_report}"
}

log "Checking out ${BASE_BRANCH} and updating from ${ORIGIN_REMOTE}..."
git checkout "${BASE_BRANCH}"
git pull --ff-only "${ORIGIN_REMOTE}" "${BASE_BRANCH}"

log "Creating sync branch ${SYNC_BRANCH}..."
git checkout -b "${SYNC_BRANCH}"

if [[ "${SYNC_STRATEGY}" == "merge" ]]; then
  log "Merging ${UPSTREAM_REF} into ${SYNC_BRANCH}..."
  if ! git merge --no-edit "${UPSTREAM_REF}"; then
    handle_conflict "merge conflict"
  fi
else
  mapfile -t SHAS < <(git rev-list --reverse "${ORIGIN_REF}..${UPSTREAM_REF}")
  if [[ "${#SHAS[@]}" -eq 0 ]]; then
    die "No linear commits to cherry-pick; rerun with --strategy merge."
  fi

  for sha in "${SHAS[@]}"; do
    log "Cherry-picking ${sha}..."
    if ! git cherry-pick "${sha}"; then
      handle_conflict "cherry-pick conflict" "${sha}"
    fi
  done
fi

if [[ "${RUN_TESTS}" == "1" ]]; then
  log "Running verification: ${TEST_CMD}"
  bash -lc "${TEST_CMD}"
fi

if [[ "${RUN_REDTEAM_GATE}" == "1" ]]; then
  log "Running red-team gate: ${REDTEAM_CMD}"
  REDTEAM_OUTPUT="$(bash -lc "${REDTEAM_CMD}" 2>&1)" || {
    append_report "status: redteam-gate-failed"
    append_report "redteam_output: ${REDTEAM_OUTPUT//$'\n'/\\n}"
    printf '%s\n' "${REDTEAM_OUTPUT}"
    die "Red-team gate failed"
  }
  printf '%s\n' "${REDTEAM_OUTPUT}"
  if [[ "${REDTEAM_OUTPUT}" == *"[redteam-gate] Report: "* ]]; then
    REDTEAM_REPORT_LINE="$(printf '%s\n' "${REDTEAM_OUTPUT}" | awk '/\[redteam-gate\] Report: /{line=$0} END{print line}')"
    REDTEAM_REPORT_PATH="${REDTEAM_REPORT_LINE#*Report: }"
    append_report "redteam_report: ${REDTEAM_REPORT_PATH}"
  fi
fi

if [[ "${RUN_ELITE_GATE}" == "1" ]]; then
  ELITE_GATE_CMD="${ELITE_CMD}"
  if [[ -n "${ELITE_ROLLBACK_CMD}" ]]; then
    ELITE_GATE_CMD="${ELITE_GATE_CMD} --rollback-cmd $(printf '%q' "${ELITE_ROLLBACK_CMD}")"
  fi
  log "Running elite gate: ${ELITE_GATE_CMD}"
  ELITE_OUTPUT="$(bash -lc "${ELITE_GATE_CMD}" 2>&1)" || {
    append_report "status: elite-gate-failed"
    append_report "elite_output: ${ELITE_OUTPUT//$'\n'/\\n}"
    printf '%s\n' "${ELITE_OUTPUT}"
    die "Elite gate failed"
  }
  printf '%s\n' "${ELITE_OUTPUT}"
  if [[ "${ELITE_OUTPUT}" == *"[elite-sync-gate] Report: "* ]]; then
    ELITE_REPORT_LINE="$(printf '%s\n' "${ELITE_OUTPUT}" | awk '/\[elite-sync-gate\] Report: /{line=$0} END{print line}')"
    ELITE_REPORT_PATH="${ELITE_REPORT_LINE#*Report: }"
    append_report "elite_report: ${ELITE_REPORT_PATH}"
  fi
fi

if [[ "${RUN_DOC_REFRESH}" == "1" ]]; then
  log "Refreshing generated parity/readme docs..."
  DOC_REFRESH_OUTPUT="$(bash -lc "${DOC_REFRESH_CMD}" 2>&1)" || {
    append_report "status: doc-refresh-failed"
    append_report "doc_refresh_output: ${DOC_REFRESH_OUTPUT//$'\n'/\\n}"
    printf '%s\n' "${DOC_REFRESH_OUTPUT}"
    die "Generated doc refresh failed"
  }
  printf '%s\n' "${DOC_REFRESH_OUTPUT}"
  DOC_CHANGED="$(git status --porcelain -- README.md docs/parity/PARITY_DASHBOARD.md || true)"
  if [[ -n "${DOC_CHANGED}" ]]; then
    git add README.md docs/parity/PARITY_DASHBOARD.md
    git commit -m "chore(sync): refresh parity dashboard and README sync status"
    append_report "doc_refresh_commit: $(git rev-parse HEAD)"
  fi
fi

log "Pushing ${SYNC_BRANCH} to ${ORIGIN_REMOTE}..."
git push -u "${ORIGIN_REMOTE}" "${SYNC_BRANCH}"

if [[ "${MODE}" == "direct-main" ]]; then
  log "Fast-forwarding ${BASE_BRANCH} to ${SYNC_BRANCH} and pushing..."
  git checkout "${BASE_BRANCH}"
  git merge --ff-only "${SYNC_BRANCH}"
  git push "${ORIGIN_REMOTE}" "${BASE_BRANCH}"
  append_report "status: synced-direct-main"
  log "Direct-main sync complete."
  log "Report: ${REPORT_FILE}"
  exit 0
fi

if [[ "${CREATE_PR}" == "1" ]]; then
  PARITY_QUEUE_SUMMARY="$(
    python3 - <<'PY'
import json, pathlib
p = pathlib.Path("docs/parity/upstream-missing-queue.json")
if not p.exists():
    print("unavailable")
else:
    try:
        data = json.loads(p.read_text())
    except Exception:
        print("unavailable")
    else:
        if isinstance(data, list):
            items = data
        elif isinstance(data, dict):
            items = data.get("items") or data.get("entries") or []
        else:
            items = []
        total = len(items)
        queued = 0
        actionable = 0
        for item in items:
            if not isinstance(item, dict):
                continue
            state = str(item.get("state") or item.get("status") or "").lower()
            if state in {"queued", "todo", "pending", "open", "in_progress"}:
                queued += 1
            cls = str(item.get("classification") or item.get("class") or "").lower()
            if cls in {"actionable", "functional", "required"}:
                actionable += 1
        print(f"total={total}, queued={queued}, actionable={actionable}")
PY
  )"
  DRIFT_ARTIFACTS=(
    "docs/parity/upstream-missing-queue.json"
    "docs/parity/upstream-missing-queue.md"
    "docs/parity/global-parity-proof.json"
    "${REPORT_FILE}"
  )
  TITLE="chore: sync upstream ${BASE_BRANCH} (${TIMESTAMP})"
  BODY_FILE="$(mktemp)"
  trap 'rm -f "${BODY_FILE}"' EXIT
  cat > "${BODY_FILE}" <<PRBODY
Automated upstream sync.

- strategy: \`${SYNC_STRATEGY}\`
- upstream ref: \`${UPSTREAM_REF}\`
- upstream sha: \`${UPSTREAM_SHA}\`
- origin sha before sync: \`${ORIGIN_SHA}\`
- verification: \`${TEST_CMD}\`
- report: \`${REPORT_FILE}\`
- parity queue summary: \`${PARITY_QUEUE_SUMMARY}\`

Drift artifacts:
$(for p in "${DRIFT_ARTIFACTS[@]}"; do printf -- "- \`%s\`\n" "$p"; done)

Test guidance:
- Run \`cargo test -p hermes-gateway\` (or the configured \`--test-cmd\`) at minimum.
- If parity drift is detected, regenerate queue/proof artifacts before merge.

Commits pending from upstream at sync start:
\`\`\`
${COMMITS_TO_SYNC}
\`\`\`
PRBODY
  PR_ARGS=(--base "${BASE_BRANCH}" --head "${SYNC_BRANCH}" --title "${TITLE}" --body-file "${BODY_FILE}")
  if [[ "${PR_DRAFT}" == "1" ]]; then
    PR_ARGS+=(--draft)
  fi
  if PR_URL="$(gh pr create "${PR_ARGS[@]}")"; then
    log "PR created successfully: ${PR_URL}"
    IFS=',' read -r -a PR_LABEL_ARRAY <<< "${PR_LABELS}"
    if [[ "${RISK_MATCH_COUNT}" -gt 0 ]]; then
      PR_LABEL_ARRAY+=("risk-paths-reviewed")
    fi
    for raw_label in "${PR_LABEL_ARRAY[@]}"; do
      label="$(printf '%s' "${raw_label}" | xargs)"
      [[ -z "${label}" ]] && continue
      gh label create "${label}" --color "1D76DB" --description "Automated upstream sync metadata" >/dev/null 2>&1 || true
      gh pr edit "${PR_URL}" --add-label "${label}" >/dev/null 2>&1 || true
    done
  else
    log "PR creation failed (branch was pushed). Create PR manually if needed."
  fi
fi

append_report "status: synced-branch-pr"
append_report "sync_branch: ${SYNC_BRANCH}"
log "Sync branch prepared: ${SYNC_BRANCH}"
log "Report: ${REPORT_FILE}"
