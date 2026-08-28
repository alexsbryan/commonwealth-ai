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
#   {"t":"summary","pass":<N>,"fail":<N>,"warn":<N>,"ms":<N>,"scope":"<what>"}
#
# ## Flags
#
#   --human   Human-readable banner instead of raw JSONL. This is the
#             interactive/gate form documented in CLAUDE.md; until 2026-07-28
#             the flag did not exist and was silently ignored, so the
#             documented gate invocation printed raw JSONL and any typo'd flag
#             was swallowed too. Unknown flags are now a usage error.
#   --full    Force a workspace check (same as SOVEREIGN_LINT_FULL=1).
#
# ## Exit codes
#
#   0   checked clean
#   1   errors found (or the build failed)
#   2   usage error
#   *   cargo's own exit code when it failed without attributable diagnostics

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADAPTER="${SCRIPT_DIR}/../sovereign/crates/sovereign-tools/src/code/test_adapters/sovereign-cargo-check-adapter"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"

# resolve_cargo_jobs — the concurrency budget, shared with sovereign-test.sh
# so both gates throttle by the same rule. See lib/cargo-jobs.sh.
# shellcheck source=lib/cargo-jobs.sh
source "${SCRIPT_DIR}/lib/cargo-jobs.sh"

HUMAN=0
# Empty ⇒ derived from cores + free memory. `cargo check` is lighter than
# the test gate's build+link+run, but it is the same unbounded fan against
# the same RAM, so it obeys the same budget rather than a second rule.
JOBS_REQUEST="${SOVEREIGN_LINT_JOBS:-}"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --human) HUMAN=1 ;;
        --full)  SOVEREIGN_LINT_FULL=1 ;;
        --jobs)
            shift
            [[ $# -gt 0 ]] || { echo "sovereign-lint: --jobs needs a value" >&2; exit 2; }
            JOBS_REQUEST="$1"
            ;;
        --jobs=*) JOBS_REQUEST="${1#--jobs=}" ;;
        -h|--help)
            cat <<'USAGE'
sovereign-lint.sh — cargo check, scoped to what you changed.

  --human   Human-readable banner instead of Tier 2 JSONL.
  --full    Check the whole workspace (same as SOVEREIGN_LINT_FULL=1).
  --jobs N  Cap build concurrency at N (0 = uncapped, the pre-2026-08-07
            behaviour). Also SOVEREIGN_LINT_JOBS; the flag wins. Default
            is derived: half the cores, capped by free memory at 4GB/job,
            because an unbounded fan against RAM a resident model already
            holds can wedge the machine. The banner names what it chose.

Scope defaults to the crates owning your uncommitted changes plus their
direct workspace dependents. For a pre-push gate use --full.

Runs with --all-targets, so #[cfg(test)] code is compiled too: a test
module that does not build fails HERE, not minutes later in the test
run. Measured warm cost of that coverage on this repo: +5.2s.

Exit: 0 clean · 1 errors/build failed · 2 usage · else cargo's own code.
USAGE
            exit 0
            ;;
        *)
            echo "sovereign-lint: unknown argument '$1'" >&2
            echo "usage: sovereign-lint.sh [--human] [--full] [--jobs N]" >&2
            exit 2
            ;;
    esac
    shift
done

# ── Concurrency budget ─────────────────────────────────────────────────────
# Resolved once; applied to the cargo check below (both the adapter-present
# and adapter-absent paths). A malformed request is a usage error, not a
# silent fall back to "all cores" — that fallback is the hazard.
if ! resolve_cargo_jobs "$JOBS_REQUEST"; then
    exit 2
fi

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

# Fold the concurrency budget into cargo_args rather than carrying a second
# array: both call sites below already expand this one, and an array that
# is empty in the uncapped case would break `set -u` on bash 3.2 (still
# `/bin/bash` on the macOS peers).
[[ "$CARGO_JOBS" -gt 0 ]] && cargo_args+=(-j "$CARGO_JOBS")

# ── --all-targets: the gate compiles TEST code too ─────────────────────────
#
# A bare `cargo check` compiles lib and bin targets only, so a `#[cfg(test)]`
# module that does not compile passes this gate. Proven on this tree
# (2026-08-07): a deliberate `let _: u32 = "not a u32";` inside a #[cfg(test)]
# mod produced `errors: 0` and `✓ Workspace checks clean` from this script,
# while the same tree under --all-targets gave
#     error[E0308]: mismatched types
#     error: could not compile `sovereign-cli-shared` (lib test)
# The breakage then surfaces minutes later in sovereign-test.sh, from a gate
# that had already said green — the "plausible exit-0 that is wrong" shape
# ARCH_PRINCIPLES §18 exists for.
#
# It is here because it was MEASURED, not because it is obviously right.
# Warm workspace, 12-core M-series, `-j 6` pinned, `--message-format json`,
# one lib file touched before every run, four consecutive runs per block
# (first discarded — see the methodology note below):
#     plain        21.6s  21.6s  21.3s
#     all-targets  26.6s  26.6s  26.8s
#     delta        +5.2s (+24%)
# 5.2s against the ~10s budget this loop is allowed. Building the test
# targets the first time costs ~5m on a cold target dir, and is paid once.
#
# METHODOLOGY, because two earlier protocols gave wrong answers here and the
# next person to re-measure will otherwise repeat them:
#   * Do NOT alternate plain/--all-targets run-by-run. Changing the target
#     selection invalidates fingerprints, so each flip pays a rebuild and
#     BOTH series come out inflated by an amount that depends on which one
#     you happened to run second. Measure in same-flag blocks.
#   * Pin --jobs. This script derives -j from FREE MEMORY, so back-to-back
#     runs of the same command legitimately resolve 6 jobs and then 2 as a
#     peer build takes RAM — observed here as 24s and 140s for identical
#     work. The banner prints the number it chose; read it before believing
#     any timing.
cargo_args+=(--all-targets)

# ── 5. Run cargo check ─────────────────────────────────────────────────────
#
# `--features corpus-engine/treesitter` matches the test runner's feature
# set so lint and test stay aligned. Cargo's `pkg/feature` syntax is a
# no-op only when `pkg` is in the dependency closure of the selection;
# for a package OUTSIDE the selection it is an error ("does not contain
# this feature"). corpus-engine sits in every crate's closure, so the
# treesitter flag is safe under any `-p` scoping. sovereign-cli is a
# leaf crate, so its dev-tools flag (which re-enables the gated dev-verb
# surface the test runner exercises) is only added when sovereign-cli is
# part of the selection.
#
# `sovereign-mesh/mesh-sim` (the Tier-1 scheduler simulator,
# SCHEDULER_QUALITY.md §5) rides along on the same rule. It is
# off-by-default so production never links a measurement harness, but
# a harness nothing compiles is a harness that silently rots — and
# this one is pure compute with no extra dependencies, so checking it
# costs a few seconds. Same leaf-crate conditional as dev-tools.
#
# `sovereign-cli/code-intel` (2026-08-06) is the `svrn code index` / `svrn
# refresh` surface that ships in the release binary
# (scripts/release-cli-local.sh passes it). Without it here the gate would
# never COMPILE ~1,500 lines that real users run — a worse failure than a
# gate that goes red, because nothing ever goes red. Same leaf-crate rule.
#
# `sovereign-cli/awareness` (2026-08-21, nc-26) is here for that exact reason,
# and it is the closure loop for the bug that put it here. `awareness_cmd`
# imported `crate::enrich_cmd::inference_client` from a crate that does not
# contain `enrich_cmd`; `--features awareness` failed with two E0433 from the
# 2026-05-22 slice-5 split until nc-26 found it — THREE MONTHS — because no
# gate anywhere built the feature. The repair moved the module to the crate
# that owns the import, which fixes today's break; this line is what stops the
# next crate split from re-opening it silently. A feature nothing compiles is a
# feature that rots, and the rot is invisible precisely because it is green.
#
# Cost measured when it was added, macOS peer, `cargo check --workspace
# --all-targets` alternated A/A/B/B on one warm tree: 37s, 31s WITHOUT the
# flag; 31s, 31s WITH it. Warm steady-state delta is ZERO to the second, and
# the flip between feature sets costs nothing either — cargo fingerprints the
# two configurations separately and keeps both. (Do not read the wall-clock of
# the whole script the same way: `jobs:` is derived from FREE MEMORY, so two
# runs minutes apart can differ by 2x for reasons that have nothing to do with
# the feature list.)
#
# The reason it is nearly free: enabling it links sovereign-cli-llm into the
# dispatcher — llama.cpp, the grammars, arrow — but this same workspace run
# already builds every one of those for the sibling binary. What is genuinely
# new is awareness_cmd's 7,526 lines, and it is paid once.
features="corpus-engine/treesitter"
if (( escalate_to_workspace )) || [[ ${#crates[@]} -eq 0 ]]; then
    features+=",sovereign-cli/dev-tools,sovereign-cli/code-intel,sovereign-cli/awareness,sovereign-mesh/mesh-sim"
else
    for c in "${crates[@]}"; do
        if [[ "$c" == "sovereign-cli" ]]; then
            features+=",sovereign-cli/dev-tools,sovereign-cli/code-intel,sovereign-cli/awareness"
        fi
        if [[ "$c" == "sovereign-mesh" ]]; then
            features+=",sovereign-mesh/mesh-sim"
        fi
    done
fi
if [[ ! -x "$ADAPTER" ]]; then
    echo "sovereign-lint: adapter not found at $ADAPTER — running raw cargo check ($label)" >&2
    (cd "$REPO_ROOT" && cargo check "${cargo_args[@]}" --features "$features" 2>&1)
    exit $?
fi

# ── 6. Run, then decide the verdict HERE — not in the adapter ──────────────
#
# The script owns the final answer because it is the only party that sees both
# cargo's exit code and the event stream. Until 2026-07-28 it exited
# PIPESTATUS[0] but let the adapter's JSONL speak for itself, so a build-script
# failure on the host (no clang → cargo exit 101) shipped `{"t":"pass"}` /
# `pass:1 fail:0` alongside that 101. The exit code was right and every human-
# and machine-readable surface said green (note 73bb9404).
#
# The adapter now reports build failures itself, so the cross-check below
# should never fire. It stays anyway: a disagreement between cargo's exit code
# and the event stream is precisely the bug we just fixed, and a gate that can
# only fail CLOSED is worth the six lines. `tee` keeps the raw cargo output for
# triage so diagnosing a failure never costs a second full check.
RUN_DIR="${REPO_ROOT}/target/sovereign-lint/latest"
mkdir -p "$RUN_DIR"
raw_log="${RUN_DIR}/cargo.raw.log"
out_jsonl="${RUN_DIR}/lint.jsonl"

start_ns=$(date +%s%N)
(cd "$REPO_ROOT" && cargo check "${cargo_args[@]}" --features "$features" --message-format json 2>&1) \
    | tee "$raw_log" \
    | "$ADAPTER" "$label" > "$out_jsonl"
cargo_exit="${PIPESTATUS[0]}"
elapsed_ms=$(( ($(date +%s%N) - start_ns) / 1000000 ))

# Counts come from the adapter's summary record — one python pass, no
# fork-per-line (that pattern cost 38s on a 6s workspace check; see the
# adapter header and scripts/sovereign-test.sh's aggregator).
counts="$(python3 -c '
import json, sys
p = f = w = 0
for line in open(sys.argv[1], errors="replace"):
    line = line.strip()
    if not line.startswith("{"):
        continue
    try:
        d = json.loads(line)
    except ValueError:
        continue
    if d.get("t") == "summary":
        p += d.get("pass", 0); f += d.get("fail", 0); w += d.get("warn", 0)
print("%d %d %d" % (p, f, w))
' "$out_jsonl" 2>/dev/null || echo "0 0 0")"
read -r total_pass total_fail total_warn <<< "$counts"

final_exit=0
disagreement=""
if [[ "$total_fail" -gt 0 ]]; then
    final_exit=1
elif [[ "$cargo_exit" != "0" ]]; then
    # Fail closed: cargo said no, the stream showed nothing to blame.
    final_exit="$cargo_exit"
    disagreement="cargo exited ${cargo_exit} but no failure was attributed to any file"
fi

if [[ $HUMAN -eq 0 ]]; then
    # Daemon mode: stream the events, replacing the adapter's ms=0 summary
    # with the real elapsed time and the scope that was actually checked.
    grep -v '"t":"summary"' "$out_jsonl" || true
    printf '{"t":"summary","pass":%d,"fail":%d,"warn":%d,"ms":%d,"scope":"%s"}\n' \
        "$total_pass" "$total_fail" "$total_warn" "$elapsed_ms" "$label"
    exit "$final_exit"
fi

echo
echo "═══════════════════════════════════════════════════════════════"
echo " sovereign-lint — cargo check"
echo "═══════════════════════════════════════════════════════════════"
# Name the scope every time. A change-scoped run and a workspace gate are
# different guarantees, and the caller cannot tell them apart from a ✓.
if (( escalate_to_workspace )) || [[ ${#crates[@]} -eq 0 ]]; then
    printf " %-12s  %s\n" "scope:" "WORKSPACE (all crates)"
else
    printf " %-12s  %s\n" "scope:" "${#crates[@]} crate(s) — $label"
fi
printf " %-12s  %s\n" "features:" "$features"
# Say that test code is in scope. A reader who does not know this gate covers
# #[cfg(test)] would reasonably assume it does not — bare `cargo check` never
# has — and would read a ✓ as narrower than it is.
printf " %-12s  %s\n" "targets:" "lib + bin + test + bench (--all-targets)"
# Same reason as the test gate: the default is derived from free memory, so
# it legitimately differs between two runs on one box. An unexplained slow
# run is indistinguishable from a broken one.
if [[ "$CARGO_JOBS" -gt 0 ]]; then
    printf " %-12s  %s\n" "jobs:" "$CARGO_JOBS — $CARGO_JOBS_REASON"
else
    printf " %-12s  %s\n" "jobs:" "UNCAPPED — $CARGO_JOBS_REASON"
fi
printf " %-12s  %s\n" "errors:" "$total_fail"
printf " %-12s  %s\n" "warnings:" "$total_warn"
printf " %-12s  %s\n" "elapsed:" "${elapsed_ms}ms"
printf " %-12s  %s\n" "cargo exit:" "$cargo_exit"
echo

if [[ -n "$disagreement" ]]; then
    echo " ✘ BUILD FAILED — $disagreement."
    echo
    echo "   This is a build/toolchain failure, not a code diagnostic:"
    echo "   a failed build script, a bad feature flag, or a missing linker."
    echo "   Running on the Fedora HOST rather than inside the toolbox is the"
    echo "   usual cause — llama-cpp-sys-4 cannot link without clang:"
    echo
    echo "     toolbox run -c sovereign-vulkan ./scripts/sovereign-lint.sh --human"
    echo
    echo "   Raw cargo output: $raw_log"
    echo
elif [[ "$total_fail" -gt 0 ]]; then
    echo " ✘ $total_fail error(s):"
    echo
    python3 -c '
import json, sys
for line in open(sys.argv[1], errors="replace"):
    line = line.strip()
    if not line.startswith("{"):
        continue
    try:
        d = json.loads(line)
    except ValueError:
        continue
    if d.get("t") != "fail":
        continue
    loc = d.get("n", "?")
    if d.get("line") is not None:
        loc += ":%s" % d["line"]
        if d.get("col") is not None:
            loc += ":%s" % d["col"]
    print("   %s" % loc)
    for l in (d.get("out") or "").splitlines()[:12]:
        print("     %s" % l)
    print()
' "$out_jsonl" 2>/dev/null || true
    # A `<cargo>` attribution means the toolchain failed, not your code. On this
    # host that is almost always "ran outside the toolbox" — llama-cpp-sys-4
    # needs clang and the C headers to build. Say so, rather than leaving the
    # reader to infer it from a build-script backtrace.
    if grep -q '"n":"<cargo>"' "$out_jsonl" 2>/dev/null; then
        echo "   ── This is a BUILD/TOOLCHAIN failure, not a code diagnostic."
        echo "      Most likely you are on the Fedora host. Use the toolbox:"
        echo
        echo "        toolbox run -c sovereign-vulkan ./scripts/sovereign-lint.sh --human"
        echo
    fi
    echo "   Raw cargo output: $raw_log"
    echo
else
    if (( escalate_to_workspace )) || [[ ${#crates[@]} -eq 0 ]]; then
        echo " ✓ Workspace checks clean."
    else
        # Never let a scoped pass read as a whole-repo guarantee.
        echo " ✓ Clean — but only for $label."
        echo "   Cross-crate breakage outside that set is NOT covered."
        echo "   Pre-push: ./scripts/sovereign-lint.sh --human --full"
    fi
    echo
fi

exit "$final_exit"
