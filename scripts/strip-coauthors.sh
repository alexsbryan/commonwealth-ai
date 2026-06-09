#!/usr/bin/env bash
#
# strip-coauthors.sh — rewrite git history to remove every
# "Co-Authored-By:" trailer from commit messages.
#
# WHAT THIS DOES (and why it's dangerous):
#   This rewrites commit messages. Rewriting a message changes that
#   commit's SHA, and every descendant commit's SHA along with it. The
#   first commit that carries a Co-Authored-By trailer, plus everything
#   after it, gets a brand-new hash. That means:
#     - Anyone who has pulled these commits will diverge from you.
#     - You will have to force-push (`git push --force-with-lease`).
#     - Open PRs built on the old hashes may need to be rebased.
#
#   It is therefore IRREVERSIBLE in practice once force-pushed. This
#   script makes a full backup first so the LOCAL repo is recoverable.
#
# USAGE:
#   scripts/strip-coauthors.sh --dry-run     # show what would change, touch nothing
#   scripts/strip-coauthors.sh               # do it (prompts for confirmation)
#   scripts/strip-coauthors.sh --yes         # do it without the prompt
#
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

DRY_RUN=0
ASSUME_YES=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    --yes|-y)  ASSUME_YES=1 ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

# --- matcher: case-insensitive, leading whitespace tolerated ----------------
TRAILER_REGEX='^[[:space:]]*[Cc]o-[Aa]uthored-[Bb]y:'

# --- 1. report ---------------------------------------------------------------
MATCHED="$(git log --all --pretty=format:'%H' --grep='Co-Authored-By' -i | wc -l | tr -d ' ')"
echo "==> Commits containing a Co-Authored-By trailer: ${MATCHED}"

if [[ "$MATCHED" == "0" ]]; then
  echo "Nothing to do. History is already clean."
  exit 0
fi

if [[ "$DRY_RUN" == "1" ]]; then
  echo
  echo "==> DRY RUN. Commits that would be rewritten (subject lines):"
  git log --all --pretty=format:'  %h  %s' --grep='Co-Authored-By' -i
  echo
  echo "==> Trailer lines that would be removed:"
  git log --all --pretty=format:'%B' | grep -nE "$TRAILER_REGEX" | sed 's/^/  /' || true
  echo
  echo "Dry run complete. No changes made. Re-run without --dry-run to apply."
  exit 0
fi

# --- 2. preconditions --------------------------------------------------------
if [[ -n "$(git status --porcelain)" ]]; then
  echo "ERROR: working tree is dirty. Commit or stash first." >&2
  exit 1
fi

if ! command -v git-filter-repo >/dev/null 2>&1; then
  echo "ERROR: git-filter-repo not found." >&2
  echo "Install: brew install git-filter-repo   (or: pip install git-filter-repo)" >&2
  exit 1
fi

# --- 3. confirm --------------------------------------------------------------
CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
echo
echo "This will REWRITE ${MATCHED} commit(s) on ALL refs."
echo "Every descendant SHA changes. You will need to force-push afterward."
echo "Current branch: ${CURRENT_BRANCH}"
if [[ "$ASSUME_YES" != "1" ]]; then
  read -r -p "Type 'rewrite' to proceed: " CONFIRM
  [[ "$CONFIRM" == "rewrite" ]] || { echo "Aborted."; exit 1; }
fi

# --- 4. backup (local, recoverable) -----------------------------------------
STAMP="$(date +%Y%m%d-%H%M%S)"
BACKUP_BUNDLE="../$(basename "$REPO_ROOT")-pre-coauthor-strip-${STAMP}.bundle"
echo "==> Backing up all refs to bundle: ${BACKUP_BUNDLE}"
git bundle create "$BACKUP_BUNDLE" --all
git tag "backup/pre-coauthor-strip-${STAMP}"
echo "==> Tagged current HEAD as backup/pre-coauthor-strip-${STAMP}"

# --- 5. rewrite --------------------------------------------------------------
# git-filter-repo runs the callback once per commit message (bytes in/out).
# We drop any line that is a Co-Authored-By trailer, then collapse the
# trailing blank lines the removal can leave behind.
echo "==> Rewriting history..."
git filter-repo --force --message-callback '
import re
trailer = re.compile(br"^[ \t]*[Cc]o-[Aa]uthored-[Bb]y:")
kept = [ln for ln in message.split(b"\n") if not trailer.match(ln)]
out = b"\n".join(kept).rstrip(b"\n")
return out + b"\n"
'

# --- 6. report + next steps --------------------------------------------------
REMAINING="$(git log --all --pretty=format:'%H' --grep='Co-Authored-By' -i | wc -l | tr -d ' ')"
echo
echo "==> Done. Co-Authored-By commits remaining: ${REMAINING}"
echo
echo "NEXT STEPS:"
echo "  1. Inspect:        git log --pretty=full | less"
echo "  2. Re-add remote:  git remote add origin git@github.com:your-org/commonwealth-ai.git"
echo "     (filter-repo removes 'origin' on purpose to stop an accidental push.)"
echo "  3. Force-push:     git push --force-with-lease origin --all --tags"
echo
echo "RECOVERY if you need to undo (before force-pushing):"
echo "  git reset --hard backup/pre-coauthor-strip-${STAMP}"
echo "  # or restore the whole repo from: ${BACKUP_BUNDLE}"
