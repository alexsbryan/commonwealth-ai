#!/usr/bin/env bash
# install-git-hooks.sh — point this clone's hooks at the version-controlled
# .githooks/ directory.
#
# Why core.hooksPath rather than copying files into .git/hooks/: hooks under
# .git/ are per-clone, invisible to review, and drift silently between
# machines. This repo now treats the pre-push gate as the PRIMARY correctness
# gate (see scripts/pre-push.sh and docs/CI_ECONOMY.md), so it needs to be a
# reviewed, shared artifact — one `git pull` updates the gate for everyone.
#
# Idempotent. Safe to re-run; scripts/bootstrap.sh calls it.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

if ! git rev-parse --git-dir >/dev/null 2>&1; then
    echo "install-git-hooks: not inside a git repository — nothing to do" >&2
    exit 0
fi

current="$(git config --local --get core.hooksPath || true)"

# Respect a developer who has deliberately pointed hooksPath somewhere else
# (a personal hook manager, a monorepo wrapper). Tell them what they are
# missing rather than silently overwriting their setup.
if [[ -n "$current" && "$current" != ".githooks" ]]; then
    echo "install-git-hooks: core.hooksPath is already set to '$current' — leaving it alone." >&2
    echo "                   To adopt the repo's shared gate, run:" >&2
    echo "                     git config core.hooksPath .githooks" >&2
    exit 0
fi

chmod +x .githooks/* scripts/pre-push.sh 2>/dev/null || true
git config core.hooksPath .githooks

echo "install-git-hooks: core.hooksPath -> .githooks"
echo
echo "  The pre-push gate now runs on every push. It scopes to what you"
echo "  changed, and is held to a ONE-MINUTE budget (~22s for a full push,"
echo "  about a second for docs only). It checks that the workspace COMPILES,"
echo "  but runs no tests — run ./scripts/sovereign-test.sh --human when you"
echo "  mean to; CI runs it on every push."
echo
echo "    ./scripts/pre-push.sh            run the gate by hand, no push"
echo "    SOVEREIGN_PREPUSH_QUICK=1 ...    skip the desktop node gates"
echo "    git push --no-verify             bypass entirely, one push"
