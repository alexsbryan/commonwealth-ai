#!/usr/bin/env bash
# The repository's pre-push gate as eutropia's land check (eutropia design D10:
# a queue whose hook is a slow build gate skips the hook on eutropia's own
# pushes, so heartbeats never wait on a build, and declares that gate as its
# land check, run once on the merged tree before the landing push).
# Scoped to the row's changes when eutropia names the base, else the whole
# workspace: never a silent pass.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
svrn=""
for cand in "$root/target/debug/sovereign-cli" "$(command -v svrn || true)" "$(command -v sovereign || true)"; do
    [[ -n "$cand" && -x "$cand" ]] && { svrn=$cand; break; }
done
[[ -n "$svrn" ]] || { echo "land: no sovereign-cli to run the gates with: not gating, not passing" >&2; exit 1; }
if [[ -n "${EUTROPIA_BASE:-}" ]] && git cat-file -e "$EUTROPIA_BASE^{commit}" 2>/dev/null; then
    changed=$(git diff --name-only "$EUTROPIA_BASE"...HEAD | tr '\n' ':')
    echo "land: gating the row's $(git diff --name-only "$EUTROPIA_BASE"...HEAD | wc -l | tr -d ' ') changed file(s) since $EUTROPIA_BASE"
    export SOVEREIGN_CHANGED_PATHS="$changed"
else
    echo "land: no EUTROPIA_BASE in the environment: gating the whole workspace"
    export SOVEREIGN_LINT_FULL=1
fi
exec "$svrn" quality check --trigger prepush
