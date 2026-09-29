#!/usr/bin/env bash
# =============================================================================
# cc switch live -- upstream sync that keeps this fork's removals and branding
#
# Usage:
#   scripts/upstream-sync.sh                 # sync onto the default sync branch
#   scripts/upstream-sync.sh --dry-run       # plan only, touch nothing
#   scripts/upstream-sync.sh --no-fetch      # use already-fetched refs
##   scripts/upstream-sync.sh --base upstream/main --branch upstream-sync
#   scripts/upstream-sync.sh --ours main     # where our customizations live
#
# Steps:
#   1. fetch upstream, list not-yet-synced upstream commits
#   2. create the sync branch from our customization branch (default main)
#   3. merge upstream; conflicts are resolved by .github/upstream-sync.conf:
#        action=del / keep-deleted  -> keep this fork's deletion
#        action=redact / brand      -> keep this fork's version
#   4. iteratively auto-strip reintroduced removed-feature code until the
#      scan reports no residual integration points (max --max-cleanup-passes)
#   5. scan the result for leftover MCP/Skills/Prompt/auth/sponsor code
#   6. write .upstream-sync/report.md
#
# Exit codes: 0 = clean, 2 = needs human review, 1 = usage/environment error
# =============================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

CONF=".github/upstream-sync.conf"
BASE_REF="upstream/main"
BRANCH="upstream-sync"
OURS_REF="main"
DO_FETCH=1
DRY_RUN=0
REPORT_DIR=".upstream-sync"
CLEANUP_PASSES=5

while [ $# -gt 0 ]; do
  case "$1" in
    --base) BASE_REF="${2:?--base needs a value}"; shift 2 ;;
    --branch) BRANCH="${2:?--branch needs a value}"; shift 2 ;;
    --ours) OURS_REF="${2:?--ours needs a value}"; shift 2 ;;
    --no-fetch) DO_FETCH=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --max-cleanup-passes) CLEANUP_PASSES="${2:?--max-cleanup-passes needs a value}"; shift 2 ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
done

[ -f "$CONF" ] || { echo "missing filter manifest: $CONF" >&2; exit 1; }
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 1; }

if [ "$DRY_RUN" -eq 0 ] && [ -n "$(git status --porcelain)" ]; then
  echo "working tree is dirty; commit or stash first: git status" >&2
  exit 1
fi

if [ "$DO_FETCH" -eq 1 ]; then
  echo "==> fetch upstream"
  git fetch --prune upstream
fi

if git rev-parse --verify --quiet "refs/remotes/$BASE_REF" >/dev/null; then
  BASE_REF="refs/remotes/$BASE_REF"
fi
git rev-parse --verify --quiet "$BASE_REF^{commit}" >/dev/null \
  || { echo "unknown base ref: $BASE_REF" >&2; exit 1; }
git rev-parse --verify --quiet "$OURS_REF^{commit}" >/dev/null \
  || { echo "unknown fork ref: $OURS_REF" >&2; exit 1; }

mkdir -p "$REPORT_DIR"
REPORT="$REPORT_DIR/report.md"
PLAN="$REPORT_DIR/plan.json"

echo "==> parse filter manifest"
python3 "$REPO_ROOT/scripts/upstream_sync_plan.py" "$CONF" > "$PLAN"

echo "==> list unsynced upstream commits"
UPSTREAM_LOG="$(git log --oneline --reverse "$OURS_REF..$BASE_REF" || true)"
UPSTREAM_COUNT="$(printf '%s\n' "$UPSTREAM_LOG" | grep -c . || true)"

if [ "$UPSTREAM_COUNT" -eq 0 ]; then
  echo "already up to date: $OURS_REF already contains $BASE_REF"
  python3 "$REPO_ROOT/scripts/upstream_sync_uptodate_report.py" \
    --repo "$REPO_ROOT" --base "$BASE_REF" --ours "$OURS_REF" > "$REPORT"
  echo "report: $REPORT"
  exit 0
fi
printf '%s\n' "$UPSTREAM_LOG" | sed 's/^/    /'

if [ "$DRY_RUN" -eq 1 ]; then
  echo
  echo "==> [dry-run] filter manifest:"
  python3 - "$PLAN" <<'PY'
import json, sys
plan = json.load(open(sys.argv[1], encoding="utf-8"))
for glob in plan["delete_globs"]:
    print("    del      ", glob)
for glob in plan["ours_globs"]:
    print("    ours     ", glob)
PY
  echo
  echo "policy: conflicts keep this fork's deletion/customization, then reintroduced"
  echo "        removed-feature code is auto-stripped in a loop; a clean result is"
  echo "        pushed to main, anything unfixable falls back to a review PR."
  exit 0
fi

echo "==> create sync branch $BRANCH from $OURS_REF"
git switch -C "$BRANCH" "$OURS_REF"

echo "==> merge $BASE_REF"
set +e
git merge --no-ff --no-commit "$BASE_REF"
MERGE_STATUS=$?
set -e
if [ "$MERGE_STATUS" -ne 0 ] && [ ! -f .git/MERGE_HEAD ]; then
  echo "merge failed before creating a merge state (exit $MERGE_STATUS)" >&2
  exit 1
fi

# Only real unmerged files count as conflicts; git also writes MERGE_HEAD for a clean merge
CONFLICTED=0
if git diff --name-only --diff-filter=U | grep -q .; then
  CONFLICTED=1
fi

echo "==> apply filter manifest"
python3 "$REPO_ROOT/scripts/upstream_sync_apply.py" "$PLAN" > "$REPORT_DIR/apply.log" 2>&1 || {
  APPLY_STATUS=$?
  echo "filter step failed (exit $APPLY_STATUS); see $REPORT_DIR/apply.log" >&2
  exit 1
}

if [ -f .git/MERGE_HEAD ]; then
  echo "==> finish merge commit"
  git add -A
  git commit --no-verify --no-edit -m "chore(sync): merge $BASE_REF into cc switch live" >/dev/null
fi

echo "==> auto-strip re-introduced code (iterative, max $CLEANUP_PASSES passes)"
SCAN="$REPORT_DIR/scan.json"
CLEANUP_LOG="$REPORT_DIR/cleanup.log"
: > "$CLEANUP_LOG"
for pass in $(seq 1 "$CLEANUP_PASSES"); do
  python3 "$REPO_ROOT/scripts/upstream_sync_scan.py" "$REPO_ROOT" > "$SCAN" || true
  REVIEW="$(python3 -c "import json,sys;print(json.load(open(sys.argv[1],encoding='utf-8'))['review_count'])" "$SCAN")"
  if [ "$REVIEW" -eq 0 ]; then
    echo "    pass $pass: no residual findings"
    break
  fi
  echo "    pass $pass: $REVIEW findings, auto-stripping"
  # first drop newly-added files matched by del rules, then strip code lines,
  # finally drop brand-new files whose contents still reference removed features
  python3 "$REPO_ROOT/scripts/upstream_sync_apply.py" "$PLAN" --only-new >> "$CLEANUP_LOG" 2>&1 || true
  python3 "$REPO_ROOT/scripts/upstream_sync_cleanup.py" "$REPO_ROOT" --apply >> "$CLEANUP_LOG" 2>&1 || true
  python3 "$REPO_ROOT/scripts/upstream_sync_cleanup.py" "$REPO_ROOT" \
    --remove-new-offenders "$OURS_REF..$BASE_REF" --scan "$SCAN" \
    --apply-log "$REPORT_DIR/apply.log" >> "$CLEANUP_LOG" 2>&1 || true
done
python3 "$REPO_ROOT/scripts/upstream_sync_scan.py" "$REPO_ROOT" > "$SCAN" || true
FINAL_REVIEW="$(python3 -c "import json,sys;print(json.load(open(sys.argv[1],encoding='utf-8'))['review_count'])" "$SCAN")"
FINAL_MANUAL="$(grep -c '^  ! ' "$REPORT_DIR/apply.log" || true)"
echo "    final residuals: $FINAL_REVIEW, unresolved conflicts: $FINAL_MANUAL"

if [ -n "$(git status --porcelain)" ]; then
  echo "==> commit auto-strip results"
  git add -A
  git commit --no-verify -m "chore(sync): auto-strip reintroduced removed-feature code" >/dev/null
fi

echo "==> write report"
python3 "$REPO_ROOT/scripts/upstream_sync_report.py" \
  --plan "$PLAN" --repo "$REPO_ROOT" \
  --base "$BASE_REF" --ours "$OURS_REF" \
  --apply-log "$REPORT_DIR/apply.log" --scan "$REPORT_DIR/scan.json" \
  --conflicted "$CONFLICTED" > "$REPORT"

echo
echo "report: $REPORT"

if [ -n "$(git status --porcelain)" ]; then
  echo "next steps:"
  echo "  1. read $REPORT and handle anything under manual review"
  echo "  2. git add -A && git commit -m 'chore(sync): finish upstream sync'"
  echo "  3. push and open a PR: git push -u origin $BRANCH"
else
  echo "merge and auto-strip committed on $BRANCH; working tree clean."
fi

if python3 "$REPO_ROOT/scripts/upstream_sync_needs_review.py" \
     "$SCAN" "$REPORT_DIR/apply.log"; then
  echo "auto-filter complete: no residual integration points."
  echo "result left on $BRANCH; the GitHub workflow validates it before publishing main."
  exit 0
fi

echo
echo "items remain that cannot be auto-fixed; manual review needed (exit 2)."
exit 2
