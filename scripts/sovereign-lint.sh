#!/usr/bin/env bash
# sovereign-lint.sh — fast cargo check for the sovereign daemon's
# `lint_status` watcher and for interactive "did I break it?" use.
#
# ## Why per-crate by default
#
# A workspace `cargo check` on this monorepo is ~30 s warm, ~2 min cold.
# A `cargo check -p <one-crate>` is 2–15 s for the same edit. Most edits
# touch one or two crates, so scoping to the actual delta is a 5–10×
# speedup with no loss in coverage for the file(s) you just changed.
#
# Cross-crate breakage (a lib change compiles in isolation but breaks a
# consumer) is the one case per-crate misses. The periodic sweep
# (SOVEREIGN_LINT_FULL=1) covers that — wire it into a pre-push hook or
# an hourly cron, not the per-keystroke path.
#
# ## Path discovery
#
# In priority order:
#   1. SOVEREIGN_CHANGED_PATHS=path1:path2:...   (set by LintWatcher;
#      colon-separated, repo-relative or absolute — both handled.)
#   2. SOVEREIGN_LINT_FULL=1                     (forces workspace check.)
#   3. `git status --porcelain` against the repo (interactive default.)
#   4. No paths discovered                       (workspace check.)
#
# Workspace-level files (root Cargo.toml, Cargo.lock, .cargo/config*,
# rust-toolchain) always escalate to a full workspace check — a dep
# version bump or feature-flag tweak can have non-local effects.
#
# ## Catching transitive regressions
#
# After resolving touched crates we automatically add every *direct*
# workspace dependent via `cargo metadata`. This catches the
# "lib compiles in isolation, breaks a consumer" failure mode without
# sliding back to workspace cost when the change is local — leaf
# crates have no dependents (zero overhead), heavy libs add their N
# direct consumers (and cargo's incremental cascade handles deeper
# layers naturally during the check itself).
#
# SOVEREIGN_LINT_NARROW=1 disables the dependent expansion for users
# who want raw "just the touched crate" timing (rare; mostly for
# debugging the script itself).
#
# ## Output
#
# Tier 2 JSONL events (one per stdout line), same schema the adapter
# has always produced:
#   {"t":"pass","n":"<crate-or-monorepo>"}
#   {"t":"fail","n":"<file>","out":"<error>","line":<N>,"col":<N>}
#   {"t":"warn","n":"<file>","out":"<warning>","line":<N>,"col":<N>}
#   {"t":"summary","pass":<N>,"fail":<N>,"warn":<N>,"ms":<N>}

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADAPTER="${SCRIPT_DIR}/../sovereign/crates/sovereign-tools/src/code/test_adapters/sovereign-cargo-check-adapter"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"

# ── 1. Discover changed paths ──────────────────────────────────────────────
if [[ -n "${SOVEREIGN_LINT_FULL:-}" ]]; then
    raw_paths=""
elif [[ -n "${SOVEREIGN_CHANGED_PATHS:-}" ]]; then
    raw_paths="$(echo "$SOVEREIGN_CHANGED_PATHS" | tr ':' '\n')"
else
    # `git status --porcelain` output (XY status, two-char column then path):
    #   ` M path/to/file`       (modified)
    #   `?? path/to/file`       (untracked)
    #   `R  old -> new`         (rename — take the new name)
    # Strip the XY status and take the path; for renames take the post `-> ` half.
    raw_paths="$(cd "$REPO_ROOT" && git status --porcelain 2>/dev/null \
        | sed -E 's/^...//' \
        | awk -F' -> ' '{ if (NF>1) print $NF; else print $1 }')"
fi

# ── 2. Map paths → owning crates ───────────────────────────────────────────
#
# Walk each path upward toward the repo root looking for the nearest
# Cargo.toml that has a `[package]` section (i.e. is itself a crate,
# not the workspace root). Files outside any crate are dropped on the
# floor; the workspace-level escalation below handles the workspace
# Cargo.toml case explicitly.
crates=()
contains() {
    local needle="$1"; shift
    local x
    for x in "$@"; do
        [[ "$x" == "$needle" ]] && return 0
    done
    return 1
}
escalate_to_workspace=0

if [[ -n "$raw_paths" ]]; then
    while IFS= read -r path; do
        [[ -z "$path" ]] && continue

        # Normalize absolute paths from SOVEREIGN_CHANGED_PATHS to
        # repo-relative so the per-file `case` patterns and the
        # upward walk both work.
        case "$path" in
            "$REPO_ROOT"/*) path="${path#$REPO_ROOT/}" ;;
        esac

        # Workspace-level files force a full check. A workspace Cargo.toml
        # edit could change resolver settings, feature defaults, or the
        # member list — none of which respect per-crate scoping.
        case "$path" in
            Cargo.toml|Cargo.lock|.cargo/*|rust-toolchain*)
                escalate_to_workspace=1
                continue
                ;;
        esac

        # Only Rust source or in-crate Cargo.toml affect cargo check.
        case "$path" in
            *.rs|*/Cargo.toml) ;;
            *) continue ;;
        esac

        abs="$REPO_ROOT/$path"
        dir="$(dirname "$abs")"
        # Walk upward; stop at REPO_ROOT or filesystem root.
        while [[ "$dir" != "$REPO_ROOT" && "$dir" != "/" && -n "$dir" ]]; do
            if [[ -f "$dir/Cargo.toml" ]] && grep -q '^\[package\]' "$dir/Cargo.toml"; then
                name="$(awk -F'=' '/^name[[:space:]]*=/ { gsub(/[[:space:]"]/, "", $2); print $2; exit }' "$dir/Cargo.toml")"
                if [[ -n "$name" ]]; then
                    if ! contains "$name" ${crates[@]+"${crates[@]}"}; then
                        crates+=("$name")
                    fi
                    break
                fi
            fi
            dir="$(dirname "$dir")"
        done
    done <<< "$raw_paths"
fi

# ── 3. Expand to direct dependents ─────────────────────────────────────────
#
# Per-crate scoping catches local breakage but misses "lib change compiles in
# isolation, breaks a consumer." Solve it by automatically adding every direct
# workspace dependent of each touched crate. Single-level only — cargo's
# incremental rebuild handles transitive cascades during the `cargo check`
# itself, and going deeper here just slides toward workspace cost.
#
# Skipped when:
#   - SOVEREIGN_LINT_NARROW=1 is set (explicit "just the touched crate" path).
#   - workspace escalation already engaged.
#   - no crates resolved (empty path list).
#
# `cargo metadata --no-deps` is ~20 ms here — cargo caches it. We filter to
# `kind is None` so dev-dependencies and build-dependencies don't pull in
# their consumers; cargo check skips test code, so a dev-only dep on a
# changed lib doesn't affect a consumer's check result.
if [[ ${#crates[@]} -gt 0 ]] \
    && (( ! escalate_to_workspace )) \
    && [[ -z "${SOVEREIGN_LINT_NARROW:-}" ]]; then
    # Python script via `-c` (not heredoc) so stdin is free for cargo
    # metadata — heredoc would override stdin and `json.load(sys.stdin)`
    # would silently parse the Python source itself.
    dependents="$(cd "$REPO_ROOT" && cargo metadata --no-deps --format-version 1 2>/dev/null \
        | python3 -c '
import json, sys
md = json.load(sys.stdin)
touched = set(sys.argv[1:])
direct = set()
for pkg in md.get("packages", []):
    if pkg["name"] in touched:
        continue
    for dep in pkg.get("dependencies", []):
        # Normal deps only — dev/build deps do not influence cargo check.
        if dep["name"] in touched and dep.get("kind") is None:
            direct.add(pkg["name"])
            break
for name in sorted(direct):
    print(name)
' "${crates[@]}")"
    while IFS= read -r d; do
        [[ -z "$d" ]] && continue
        if ! contains "$d" ${crates[@]+"${crates[@]}"}; then
            crates+=("$d")
        fi
    done <<< "$dependents"
fi

# ── 4. Build cargo args ────────────────────────────────────────────────────
if (( escalate_to_workspace )) || [[ ${#crates[@]} -eq 0 ]]; then
    cargo_args=(--workspace)
    label="monorepo"
else
    cargo_args=()
    for c in "${crates[@]}"; do
        cargo_args+=(-p "$c")
    done
    # Comma-joined crate list as the adapter's "workspace" label so the
    # Tier 2 `pass` event names what was actually checked.
    label="$(IFS=,; echo "${crates[*]}")"
fi

# ── 5. Run cargo check ─────────────────────────────────────────────────────
#
# `--features corpus-engine/treesitter` matches the test runner's feature
# set so lint and test stay aligned. Cargo treats a feature flag for a
# crate that isn't in the dependency closure as a no-op, so this is safe
# under any `-p` scoping.
if [[ ! -x "$ADAPTER" ]]; then
    echo "sovereign-lint: adapter not found at $ADAPTER — running raw cargo check ($label)" >&2
    (cd "$REPO_ROOT" && cargo check "${cargo_args[@]}" --features corpus-engine/treesitter 2>&1)
    exit $?
fi
(cd "$REPO_ROOT" && cargo check "${cargo_args[@]}" --features corpus-engine/treesitter --message-format json 2>&1) \
    | "$ADAPTER" "$label"
exit "${PIPESTATUS[0]}"
