#!/usr/bin/env bash
# `.containerignore` must keep the desktop image build context lean — and must
# not trim so hard that it starves a Containerfile of a path it COPYs.
#
# WHY THIS EXISTS. Measured on RuggedFox 2026-08-10, the desktop build context
# was 36 GB: research/ 33 GB and models/ 2.1 GB, neither excluded. Both
# desktop Containerfiles COPY exactly ONE file each (their entrypoint script) —
# every other path reaches the build through the driver's runtime bind mount
# (`-v "$REPO_ROOT:/work:Z"`), never the context. So podman was tar-streaming
# 36 GB to copy two small shell scripts. Excluding both took it to 789 MB.
#
# The failure mode is why this is a test and not a comment: an un-ignored tree
# does not break the build, it just makes every container build open with a
# multi-minute "Sending build context" stall. Nothing reports it, so it accrues
# silently — `models/` was missed for however long its `sovereign/models/`
# sibling has been excluded, and research/ grew to 33 GB unnoticed.
#
# THREE checks, and the third is the one that matters:
#   1. The named bulky trees are excluded.        (regression control)
#   2. Every path a Containerfile COPYs is NOT excluded.  (over-reach control —
#      fails if someone "fixes" the context with `*`, which would break the
#      build rather than slow it.)
#   3. NO un-ignored top-level tree exceeds the size budget, whatever it is
#      called. 1 and 2 only know the names we already thought of; 3 is what
#      would have caught research/ without anyone naming it.
#
# No podman, no network, no containers — this reads the ignore file and the
# Containerfiles and walks the tree.
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)" || exit 2
cd "$ROOT" || exit 2

# Both libs: `run_capped` because macOS has no `timeout`, and release-host.sh
# for RELEASE_STAT_SIZE because BSD `find` has no `-printf`. Until 2026-09-03
# this suite used both GNU spellings, so on a Mac the walk produced nothing and
# the size budget was reported COULD-NOT-JUDGE on every run — honestly, which
# is why it was the only one of the six that did not read as a pass.
# shellcheck source=../lib/run-capped.sh
source "$ROOT/scripts/lib/run-capped.sh"
# shellcheck source=../lib/release-host.sh
source "$ROOT/scripts/lib/release-host.sh"

IGNORE_FILE=".containerignore"
[[ -f "$IGNORE_FILE" ]] || { echo "release-build-context: no $IGNORE_FILE at repo root"; exit 2; }

# Budget for the WHOLE effective context — the quantity that actually causes
# the harm, rather than a per-directory rule with an arbitrary cut. Measured
# 2026-08-10: 789 MB after the exclusions, 36 GB before. 2 GB leaves real
# headroom while still failing long before a 33 GB tree gets streamed again.
BUDGET_MB="${RELEASE_CONTEXT_BUDGET_MB:-2048}"
# Ceiling on the tree walk. A walk too slow to finish must be reported as
# could-not-judge, never as a pass (ARCH §18.1 — four verdicts, not two).
WALK_TIMEOUT="${RELEASE_CONTEXT_WALK_TIMEOUT:-60}"

rc=0
ok()   { echo "  ok    $1"; }
fail() { echo "  FAIL  $1"; rc=1; }
# could-not-judge is NOT a pass and NOT a failure: it exits non-zero at the end
# only if nothing else did, so it can never be mistaken for green.
unknown_count=0
declare -A unknown_seen=()
# Deduped: the matcher runs per candidate path, so one unreadable pattern would
# otherwise print once per path and bury the rest of the report.
unknown() {
    unknown_count=$((unknown_count + 1))
    [[ -n "${unknown_seen[$1]:-}" ]] && return 0
    unknown_seen[$1]=1
    echo "  ????  $1"
}

# ─── Pattern matching ─────────────────────────────────────────────────
# `.containerignore` is NOT gitignore. Patterns match the context-relative
# path, so `target/` matches the root `target` only — which is exactly why the
# file carries BOTH `target/` and `**/target/`, and why `**/target/` does NOT
# match `target-container-linux`.
#
# This implements the pattern FORMS the file actually uses. Any other form is
# reported as could-not-judge, never quietly treated as "does not match" — a
# matcher that shrugs at a pattern it cannot read is the silent-substitution
# failure this suite exists to prevent (ARCH §18.3).
# Evaluation is IN ORDER and the LAST matching pattern wins, so a `!` negation
# after a broad exclusion re-includes the path. That is not decoration: it is
# the only way `target/` can be excluded wholesale while
# Containerfile.local-test still COPYs target/release/sovereign-cli out of it.
# Verified against the real podman (5.8.4) before relying on it — `target/`
# alone fails that COPY with exit 125 and "1 filtered out using
# .containerignore"; adding the negation makes the same build exit 0.
path_is_ignored() {  # path_is_ignored <relative-path>  → 0 if ignored
    local path="${1%/}" pat base comp negate ignored=1
    while IFS= read -r pat; do
        pat="${pat%%#*}"                    # strip comments
        pat="${pat#"${pat%%[![:space:]]*}"}"  # ltrim
        pat="${pat%"${pat##*[![:space:]]}"}"  # rtrim
        [[ -z "$pat" ]] && continue
        negate=0
        if [[ "$pat" == '!'* ]]; then negate=1; pat="${pat#!}"; fi
        pat="${pat%/}"
        [[ -z "$pat" ]] && continue
        if [[ "$pat" == '**/'* ]]; then
            # Any-depth: matches if any component of the path matches the base.
            base="${pat#'**/'}"
            [[ "$base" == *'**'* || "$base" == */* ]] && { unknown "unsupported '**' pattern '$pat'"; continue; }
            local IFS='/'
            for comp in $path; do
                # shellcheck disable=SC2053  # glob match is intended
                [[ "$comp" == $base ]] && { ignored=$(( negate ? 1 : 0 )); break; }
            done
        elif [[ "$pat" == *'**'* ]]; then
            unknown "unsupported '**' pattern '$pat'"
        else
            # Root-anchored: the path itself, or anything beneath it.
            # shellcheck disable=SC2053  # glob match is intended
            [[ "$path" == $pat || "$path" == $pat/* ]] && ignored=$(( negate ? 1 : 0 ))
        fi
    done < "$IGNORE_FILE"
    return "$ignored"
}

echo "release-build-context:"

# ─── 1. The named bulky trees stay excluded ───────────────────────────
# Each of these was, or would be, multi-GB of context. Named individually so a
# deletion from .containerignore fails HERE with the reason attached.
#
# This list and the budget in check 3 are COMPLEMENTARY, and neither replaces
# the other. Check 3 catches trees nobody thought to name, but only once they
# exceed the budget — dropping `.cargo-container/` alone takes the context to
# 1487 MB, which is under budget and fires nothing. Naming it here makes each
# exclusion we have already paid for non-removable, whatever its current size.
# Every entry added to .containerignore for a size reason belongs here too.
for tree in \
    target \
    sovereign/target \
    sovereign/models \
    models \
    research \
    target-container-linux \
    target-container-windows \
    .cargo-container \
    .cargo-container-windows \
    .npm-container \
    .npm-container-modules \
    .npm-container-modules-windows \
    .tauri-cache-container \
    .xwin-container \
    .ort-cache-container \
    dist \
    .git
do
    if path_is_ignored "$tree"; then
        ok "excluded: $tree"
    else
        fail "'$tree' is NOT excluded by $IGNORE_FILE — it will be streamed into every container build"
    fi
done

# ─── 2. Nothing a Containerfile COPYs may be excluded ─────────────────
# Derived from the real Containerfiles, so this cannot drift: add a COPY of a
# path that .containerignore excludes and this fails. The Containerfile is the
# single decider of what the build genuinely needs (ARCH §10.6).
#
# EVERY Containerfile, not just the desktop pair. .containerignore lives at the
# repo root and applies to any build whose context is the root — which is all of
# them. Scoping this check to the two desktop files is how the local-test
# conflict below stayed invisible: that image COPYs target/release/sovereign-cli
# while `target/` is excluded, so it cannot build at all, and its own header
# asserts the opposite ("the build context is the workspace root so this path
# resolves"). Nothing caught it because its smoke tests are #[ignore]d
# (sovereign-mesh/tests/local_pod_smoke.rs:262,415).
#
# A COPY of a DIRECTORY is fine even when descendants are filtered — podman
# only fails when the named path itself resolves to nothing. That is why
# `COPY corpus-engine/ …` coexists happily with `**/target/`.
mapfile -t containerfiles < <(git ls-files | grep -iE '(^|/)(Containerfile|Dockerfile)([.-][A-Za-z0-9._-]+)?$')
if (( ${#containerfiles[@]} == 0 )); then
    fail "found no Containerfiles to check — the COPY-path control cannot run"
else
    copy_checked=0
    for cf in "${containerfiles[@]}"; do
        # COPY [--flags] <src>... <dest> — every arg but the last is a source.
        while read -r -a words; do
            local_srcs=()
            for w in "${words[@]:1}"; do
                [[ "$w" == --* ]] && continue
                local_srcs+=("$w")
            done
            (( ${#local_srcs[@]} < 2 )) && continue
            unset 'local_srcs[${#local_srcs[@]}-1]'   # drop <dest>
            for src in "${local_srcs[@]}"; do
                # Skip stage-scoped copies (--from=) and absolute in-image paths.
                [[ "$src" == /* ]] && continue
                copy_checked=$((copy_checked + 1))
                if path_is_ignored "$src"; then
                    fail "$cf COPYs '$src' but $IGNORE_FILE excludes it — the image build would fail on a missing file"
                else
                    ok "COPY source reachable: $src"
                fi
            done
        done < <(grep -hE '^[[:space:]]*(COPY|ADD)[[:space:]]' "$cf" | grep -v -- '--from=')
    done
    (( copy_checked > 0 )) || fail "parsed 0 COPY sources from ${#containerfiles[@]} Containerfile(s) — the control asserted nothing"
fi

# ─── 3. The effective context stays inside the budget ─────────────────
# The check that does not depend on having thought of the name — this is what
# would have caught research/ unprompted.
#
# It must measure what podman actually sends, which means pruning ignored
# subtrees rather than skipping ignored top-level NAMES. Getting that wrong is
# not academic: the first cut of this check ran `du -sm sovereign`, which
# descended into the excluded sovereign/models/ and reported the context as
# 775,636 MB. Pruning is also what keeps the walk cheap — the 537 GB target/
# and 757 GB sovereign/models/ are never entered.
#
# Ignore patterns are translated into find predicates: `**/base` prunes at any
# depth (-name), a root-anchored pattern prunes one exact path (-path).
prune=()
negated=()
while IFS= read -r pat; do
    pat="${pat%%#*}"
    pat="${pat#"${pat%%[![:space:]]*}"}"
    pat="${pat%"${pat##*[![:space:]]}"}"
    [[ -z "$pat" ]] && continue
    # A negated path is re-INCLUDED, and it may well sit inside a tree that the
    # prune list removes (target/release/sovereign-cli under target/). Pruning
    # would then undercount it, so collect these and add their bytes back.
    if [[ "$pat" == '!'* ]]; then
        negated+=( "${pat#!}" )
        continue
    fi
    pat="${pat%/}"
    if [[ "$pat" == '**/'* ]]; then
        base="${pat#'**/'}"
        [[ "$base" == *'**'* || "$base" == */* ]] && continue
        prune+=( -name "$base" -prune -o )
    elif [[ "$pat" == *'**'* ]]; then
        continue
    else
        prune+=( -path "./$pat" -prune -o )
    fi
done < "$IGNORE_FILE"

# Sum the sizes of every file that survives pruning. Sizes are apparent bytes
# (%s), which is what gets tar-streamed — not on-disk blocks.
total_mb=""
if walk_out=$(run_capped "$WALK_TIMEOUT" find . "${prune[@]}" -type f -exec stat "${RELEASE_STAT_SIZE[@]}" {} + 2>/dev/null); then
    # Add back anything a `!` pattern re-included from inside a pruned tree.
    for n in "${negated[@]:-}"; do
        [[ -z "$n" || ! -e "$n" ]] && continue
        walk_out+=$'\n'"$(find "$n" -type f -exec stat "${RELEASE_STAT_SIZE[@]}" {} + 2>/dev/null)"
    done
    total_mb=$(awk '{s+=$1} END {printf "%d", s/1048576}' <<<"$walk_out")
else
    unknown "context walk did not finish within ${WALK_TIMEOUT}s — size budget NOT verified"
fi

if [[ -n "$total_mb" ]]; then
    if (( total_mb > BUDGET_MB )); then
        fail "effective build context is ${total_mb} MB (budget ${BUDGET_MB} MB) — every container build tar-streams this. Largest un-ignored trees:"
        # Name the offenders, using the same pruning so the numbers agree with
        # the total above.
        while IFS= read -r entry; do
            path_is_ignored "$entry" && continue
            sz=$(run_capped "$WALK_TIMEOUT" find "./$entry" "${prune[@]}" -type f -exec stat "${RELEASE_STAT_SIZE[@]}" {} + 2>/dev/null \
                 | awk '{s+=$1} END {printf "%d", s/1048576}')
            [[ -n "$sz" ]] && printf '%8s MB  %s\n' "$sz" "$entry"
        done < <(ls -A 2>/dev/null) \
            | sort -rn | head -6 | sed 's/^/          /'
    else
        ok "effective context is ${total_mb} MB (budget ${BUDGET_MB} MB)"
    fi
fi

if (( unknown_count > 0 )) && (( rc == 0 )); then
    echo "  release-build-context: $unknown_count check(s) could not be judged — that is not a pass"
    rc=1
fi
exit "$rc"
