#!/usr/bin/env bash
# cargo-scope.sh — shared crate-scoping helpers for the test runners.
#
# Sourced by scripts/sovereign-test.sh (the daemon-facing regression gate) and
# scripts/nextest.sh (the fast-path dev runner). Both answer the same two
# questions — "which crates does this run cover?" and "which <pkg>/<feature>
# flags are legal for that selection?" — and they MUST answer them identically:
# the repo's standing rule is that when one runner passes and the other fails,
# the discrepancy is the bug. Keeping the logic here makes that true by
# construction rather than by vigilance.
#
# Requires: REPO_ROOT set by the caller. Uses cargo metadata (~40ms) + python3.

# ── crate_for_path <repo-relative-path> ────────────────────────────────────
# Echo the name of the crate that owns the given file, or nothing.
#
# "Owns" = nearest ancestor directory holding a Cargo.toml with a `[package]`
# section. The virtual workspace-root manifest has only `[workspace]`, so a
# change to a non-crate path (scripts/, README) resolves to no crate and is
# reported by the caller rather than silently swallowed.
crate_for_path() {
    local dir="$REPO_ROOT/$1"
    dir="$(dirname "$dir")"
    while :; do
        local manifest="$dir/Cargo.toml"
        if [[ -f "$manifest" ]] && grep -q '^\[package\]' "$manifest"; then
            awk '
                /^\[package\]/ { inpkg=1; next }
                /^\[/          { inpkg=0 }
                inpkg && /^name[[:space:]]*=/ {
                    gsub(/^name[[:space:]]*=[[:space:]]*"/, "")
                    gsub(/".*$/, "")
                    print; exit
                }
            ' "$manifest"
            return 0
        fi
        [[ "$dir" == "$REPO_ROOT" || "$dir" == "/" ]] && return 0
        dir="$(dirname "$dir")"
    done
}

# ── _workspace_members ─────────────────────────────────────────────────────
# Echo every cargo workspace member name, one per line. Empty on failure.
_workspace_members() {
    cargo metadata --no-deps --format-version 1 \
        --manifest-path "$REPO_ROOT/Cargo.toml" 2>/dev/null \
    | python3 -c 'import json, sys
print("\n".join(p["name"] for p in json.load(sys.stdin)["packages"]))' 2>/dev/null
}

# ── keep_members  (stdin: crate names, stdout: the ones cargo accepts) ─────
# Drop crate names that aren't cargo WORKSPACE MEMBERS. A directory can hold a
# `[package]` manifest and still sit outside the workspace — sovereign-mobile
# is a standalone Tauri app — so crate_for_path can legitimately resolve a name
# that cargo refuses:
#   error: package ID specification `sovereign-mobile` did not match any packages
# and that error fails the ENTIRE run, not just the offending crate.
#
# Rejections are reported, never silent. If the member list can't be resolved,
# everything passes through: a loud cargo error beats a quietly narrowed run.
keep_members() {
    local members c
    members="$(_workspace_members)"
    while IFS= read -r c; do
        [[ -z "$c" ]] && continue
        if [[ -z "$members" ]] || grep -qxF -- "$c" <<< "$members"; then
            printf '%s\n' "$c"
        else
            echo "  skipping '${c}' — has a [package] manifest but is not a workspace member" >&2
        fi
    done
}

# ── resolve_features [<selected-pkg>...] ───────────────────────────────────
# Echo the comma-separated `<pkg>/<feature>` list that is BOTH needed for full
# coverage AND legal for the given selection. No args = full workspace.
#
# Two flags keep coverage from silently shrinking:
#   corpus-engine/treesitter  — corpus-engine was tested with treesitter on
#                               before the 2026-05-10 monorepo collapse.
#   sovereign-cli/dev-tools   — re-enables the feature-gated dev-verb suites
#                               (aliases, phase3 serve/init, phase6 retirement).
#   sovereign-cli/code-intel  — `svrn code index` / `svrn refresh`, which the
#                               release build ships. Added 2026-08-06 with the
#                               port; without it the gate compiles neither.
#
# `--features <pkg>/<feat>` is a hard ERROR (not a no-op) when <pkg> is neither
# in the `-p` selection nor a dependency of something in it. Passing
# corpus-engine/treesitter unconditionally therefore broke every scoped run on
# a crate outside corpus-engine's dependents — `--package oicp-types` died in
# 39ms with "the package 'oicp-types' does not contain this feature", cargo
# exit 101, zero tests run.
#
# So decide from the selection: emit a flag only when its package is in the
# selection's WORKSPACE-INTERNAL dependency closure. corpus-engine and
# sovereign-cli are both workspace members and no external crate can depend on
# them, so the workspace-internal closure is the whole answer.
#
# For an unscoped run every member is selected, so both flags apply. And because
# nothing depends on the leaf crate sovereign-cli, "in the closure" reduces to
# "explicitly selected" for dev-tools — matching the hand-rolled special case
# this replaced.
resolve_features() {
    if [[ $# -eq 0 ]]; then
        echo "corpus-engine/treesitter,sovereign-cli/dev-tools,sovereign-cli/code-intel"
        return 0
    fi

    local meta_json
    meta_json="$(cargo metadata --no-deps --format-version 1 \
                     --manifest-path "$REPO_ROOT/Cargo.toml" 2>/dev/null)"
    if [[ -z "$meta_json" ]]; then
        # Never guess: dropping a feature silently shrinks coverage, and keeping
        # one that's out of scope is a hard cargo error.
        echo "  cargo metadata failed — cannot resolve feature scope." >&2
        echo "  omitting pkg/feature flags; THIS RUN MAY UNDER-COVER." >&2
        return 0
    fi

    printf '%s' "$meta_json" | python3 -c '
import json, sys

meta = json.load(sys.stdin)
members = {p["name"] for p in meta["packages"]}
edges = {p["name"]: {d["name"] for d in p.get("dependencies", []) if d["name"] in members}
         for p in meta["packages"]}

seen, stack = set(), list(sys.argv[1:])
while stack:
    pkg = stack.pop()
    if pkg in seen or pkg not in edges:
        continue
    seen.add(pkg)
    stack.extend(edges[pkg])

want = []
if "corpus-engine" in seen:
    want.append("corpus-engine/treesitter")
if "sovereign-cli" in seen:
    want.append("sovereign-cli/dev-tools")
    # `code-intel` ships in the release binary (scripts/release-cli-local.sh),
    # so the gate must compile it. Omitting it would leave `svrn code index`
    # and `svrn refresh` — code real users run — never built by any check.
    want.append("sovereign-cli/code-intel")
print(",".join(want))
' "$@"
}

# ── nextest_install_hint ───────────────────────────────────────────────────
# Platform-correct precompiled-binary URL for cargo-nextest. get.nexte.st keys
# its tarballs by platform, so a hardcoded /mac hint hands Linux developers a
# macOS binary.
nextest_install_hint() {
    local platform
    case "$(uname -s)" in
        Darwin) platform="mac" ;;
        Linux)  platform="linux" ;;
        *)      platform="linux" ;;
    esac
    echo "  Install:     cargo install cargo-nextest --locked"
    echo "  Or faster:   curl -LsSf https://get.nexte.st/latest/${platform} | tar zxf - -C \${CARGO_HOME:-\$HOME/.cargo}/bin"
    echo "  Or one-shot: ${REPO_ROOT}/scripts/bootstrap.sh"
}
