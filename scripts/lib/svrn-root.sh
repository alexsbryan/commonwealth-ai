# SPDX-License-Identifier: AGPL-3.0-or-later
# svrn-root.sh — resolve the per-user svrnmesh root for shell callers.
#
# Source this; call `svrn_root`.
#
#   . "$(dirname "${BASH_SOURCE[0]}")/lib/svrn-root.sh"
#   root="$(svrn_root)"
#   mkdir -p "$root"
#
# ── Why this file exists ────────────────────────────────────────────────
# The Rust getters (`sovereign_contracts::rebrand`) prefer `~/.svrnmesh`
# but fall back to a POPULATED legacy `~/.sovereign`, so a machine that
# predates the rebrand keeps working. A shell script that hard-codes
# `~/.svrnmesh` does not have that fallback — and worse, `mkdir -p
# ~/.svrnmesh` on such a machine POPULATES the rebranded dir, after which
# `resolve_branded_dir()` prefers it and the real data root (models,
# indexes, notes.db) is silently orphaned. No error; the install just
# looks new. Reproduced 2026-08-10.
#
# ── Why the fallback is duplicated here, and only here ──────────────────
# One decider per path is the rule (ARCH_PRINCIPLES §10.6), so the FIRST
# thing this tries is the binary that owns the decision: `svrn path root`.
# The arm below it exists solely because bootstrap.sh must work on a
# machine where nothing has been built yet — there is no binary to ask.
# That makes this the ONE sanctioned shell copy of the preference order.
# Do not inline it anywhere else; source this file instead.

# Print the per-user svrnmesh root on stdout.
svrn_root() {
    # 1. Ask the SSOT, if a binary exists to ask. Prefer an installed
    #    `svrn`/`sovereign`, then this checkout's debug build.
    local candidate root
    for candidate in svrn sovereign sovereign-cli; do
        if command -v "$candidate" >/dev/null 2>&1; then
            if root="$("$candidate" path root 2>/dev/null)" && [ -n "$root" ]; then
                printf '%s\n' "$root"
                return 0
            fi
        fi
    done
    local repo_root
    repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
    for candidate in "$repo_root/target/debug/sovereign-cli" \
                     "$repo_root/target/release/sovereign-cli"; do
        if [ -x "$candidate" ]; then
            if root="$("$candidate" path root 2>/dev/null)" && [ -n "$root" ]; then
                printf '%s\n' "$root"
                return 0
            fi
        fi
    done

    # 2. Nothing built yet (fresh workstation, pre-bootstrap). Mirror
    #    resolve_branded_dir(): populated rebranded dir wins, else a
    #    legacy dir that exists, else the rebranded name.
    if [ -d "${HOME}/.svrnmesh" ] && [ -n "$(ls -A "${HOME}/.svrnmesh" 2>/dev/null)" ]; then
        printf '%s\n' "${HOME}/.svrnmesh"
    elif [ -d "${HOME}/.sovereign" ]; then
        printf '%s\n' "${HOME}/.sovereign"
    else
        printf '%s\n' "${HOME}/.svrnmesh"
    fi
}
