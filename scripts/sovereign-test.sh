#!/usr/bin/env bash
# sovereign-test.sh — repo-wide test runner for the sovereign daemon's
# `test_status` watcher and the agent's pre-merge regression gate.
#
# Two faces, one truth:
#
# - **Daemon mode** (default, no flags): emits Tier 2 JSONL events
#   that `test_results.db` consumes; the daemon turns those into
#   `sovereign tools call test_status` (`fresh_passing` / `fresh_failing`).
# - **Human/agent mode** (`--human`): emits a compact summary, lists
#   every failing test by name, and points at the saved adapter logs
#   for failure-output triage.
#
# Coverage. One workspace-wide run. Pre-monorepo this script fanned out
# across three independent cargo workspaces; the 2026-05-10 monorepo
# collapse means a single root workspace covers every crate, so one
# invocation does the job a fan would — this script has NOT added any
# parallelism of its own since then, and never claimed the speed the
# fan-out once gave (see --engine for where the parallelism actually
# comes from now).
#
# Two executors, one coverage contract. `--engine nextest` (the default
# where cargo-nextest is installed) runs test binaries in parallel and
# reports via JUnit; `--engine cargo` runs them serially and reports via
# libtest. Both are translated to the SAME Tier 2 JSONL by their
# respective adapters, and nextest's inability to run doctests is covered
# by an unconditional `cargo test --doc` pass — so switching engines
# changes the clock, never the coverage.
#
# The `<pkg>/<feature>` flags are chosen per-selection by resolve_features
# in lib/cargo-scope.sh, not hardcoded: treesitter because sovereign-test
# ran corpus-engine with --features treesitter before the merge and we
# don't want coverage to silently shrink, and `sovereign-cli/dev-tools`
# for the same reason — the dev verbs (and their integration suites:
# aliases, phase3 serve/init, phase6 retirement) are feature-gated out of
# the default end-user build, and this script tests the developer build.
# The default build's intercept contract is covered separately by
# `sovereign-cli/tests/default_build_gate.rs` under plain
# `cargo test -p sovereign-cli`.
#
# Definition-of-done. Every feature push expects:
#   `./scripts/sovereign-test.sh --human` → "all green" (or
#   `sovereign tools call test_status` → `fresh_passing`)
# before merge. The daemon's watcher polls this script on debounce;
# the operator/agent invokes it on demand.
#
# Flags:
#   --human                 Compact human-readable summary on stderr.
#                           Tier 2 JSONL still written to logs; stdout
#                           becomes the summary.
#   --engine <auto|nextest|cargo>
#                           Test executor. Default `auto` = nextest when
#                           cargo-nextest is installed, else cargo.
#                           nextest runs the workspace's ~178 test binaries
#                           in PARALLEL where `cargo test` runs them one at a
#                           time: measured 2026-07-24, test execution 19.0s
#                           vs ~104s, full run 58.9s vs 126s. Both engines
#                           emit the identical Tier 2 JSONL contract, so the
#                           daemon's test_status cannot tell them apart.
#                           `cargo` forces the old path (and is the automatic
#                           fallback on a machine without nextest, so the gate
#                           still works everywhere).
#                           NOTE nextest does not run DOCTESTS — this script
#                           always appends a separate `cargo test --doc` pass
#                           so switching engines can never shrink coverage.
#   --package <name>        Run only the named package (e.g.
#                           `--package sovereign-cli`). Repeatable or
#                           comma-separated. Maps to cargo's `-p` flag.
#                           SCOPES BUILD + RUN — the real lean-run lever.
#   --changed               Auto-scope to the crates that own git-changed
#                           .rs / Cargo.toml files (vs HEAD, plus
#                           untracked). Expands to `-p <crate>` for each,
#                           so cargo builds + runs ONLY the touched crates
#                           and their dependents' tests — "just the
#                           packages we touched." Unions with any explicit
#                           --package. Falls back to the full workspace
#                           (with a loud note) when no crate is detected,
#                           so the gate never silently under-covers.
#                           Scoped runs (--changed/--package) build into an
#                           ISOLATED target dir (target/sovereign-test-scoped)
#                           ONLY when sccache is actually wired up — see the
#                           "Target-dir isolation" note below for why that
#                           precondition is load-bearing. Set CARGO_TARGET_DIR
#                           to override either way.
#   --filter <pattern>      Pass <pattern> to cargo test as a libtest
#                           NAME filter, AND auto-scope the build to the
#                           crates whose sources contain <pattern>.
#                           A libtest filter is a substring match on the
#                           test's full path, so any test it can match must
#                           have that substring somewhere in its crate's
#                           .rs sources — grepping for it therefore
#                           OVER-approximates the owning crates and can
#                           never under-cover. A broad pattern selects
#                           broadly (degrading to the full workspace),
#                           which is exactly the old behaviour.
#                           Explicit --package / --changed win over this.
#   --filter-workspace      Keep --filter workspace-scoped (compile every
#                           crate, run the filtered tests). The pre-2026-07
#                           behaviour; use it when you suspect the grep
#                           heuristic is missing a crate.
#   --jobs <N>              Cap concurrency at N for BOTH phases — the
#                           cargo build and the test run. `--jobs 0` lifts
#                           the cap entirely (cargo's and nextest's own
#                           "all cores" defaults, the pre-2026-08-07
#                           behaviour). Also settable as
#                           SOVEREIGN_TEST_JOBS; the flag wins.
#                           DEFAULT is derived, not unbounded: half the
#                           cores, further capped by free memory at 4GB
#                           per job. An unbounded run wedged this
#                           workstation on 2026-08-07 — 32 rustc
#                           processes and then 32 test binaries, against
#                           RAM a resident model was already holding (on
#                           the Halo, GPU memory IS system memory). The
#                           banner always names the number it chose and
#                           which term bound it, so a slow run is never a
#                           mystery. See scripts/lib/cargo-jobs.sh.
#   --no-default-features   Skip the corpus-engine treesitter feature
#                           (and any others). Default off.
#   --keep-logs             Preserve adapter logs even on success
#                           (failures always preserve).
#   --allow-empty           Treat a zero-test run as green. OFF by default:
#                           a gate that renders "green" from an empty run
#                           tells the caller it verified something when it
#                           verified nothing (see "Empty-run guard" below).
#                           Pass this only when you genuinely expect a scope
#                           with no tests.
#   --doctests              Also run `cargo test --doc`. OFF by default: it
#                           costs 17.4s of a 63s warm workspace run (measured
#                           2026-07-28) and this workspace collects ZERO
#                           runnable doctests, so locally it is pure tax.
#                           nextest cannot run doctests, so this pass is the
#                           only thing that would execute one — CI passes it
#                           (.github/workflows/ci.yml). When it is off the
#                           banner and the JSONL summary say so explicitly.
#   --no-doctests           Explicit form of the default.
#   -h, --help              This message.
#
# Outputs Tier 2 JSONL events on stdout (one per line):
#   {"t":"pass","n":"<test_name>"}
#   {"t":"fail","n":"<test_name>","out":"<captured output>"}
#   {"t":"summary","pass":<N>,"fail":<N>,"warn":0,"ms":<elapsed_ms>,"empty":<bool>}
#
# Exit code: 0 iff cargo test exits 0 AND no `fail` events were
# emitted AND at least one test actually ran. Non-zero on any failure
# or build error; 4 specifically for "no tests matched" (see the
# empty-run guard near the summary).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADAPTER="${SCRIPT_DIR}/../sovereign/crates/sovereign-tools/src/code/test_adapters/sovereign-cargo-test-adapter"
NEXTEST_ADAPTER="${SCRIPT_DIR}/../sovereign/crates/sovereign-tools/src/code/test_adapters/sovereign-nextest-junit-adapter"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"

# crate_for_path / keep_members / resolve_features — shared with nextest.sh so
# the two runners cover identically. See scripts/lib/cargo-scope.sh.
# shellcheck source=lib/cargo-scope.sh
source "${SCRIPT_DIR}/lib/cargo-scope.sh"
# resolve_cargo_jobs — the concurrency budget. See lib/cargo-jobs.sh for why
# an unbounded run can wedge a machine that holds model weights resident.
# shellcheck source=lib/cargo-jobs.sh
source "${SCRIPT_DIR}/lib/cargo-jobs.sh"
LOG_DIR="${REPO_ROOT}/target/sovereign-test"

PACKAGES=()
HUMAN=0
KEEP_LOGS=0
CHANGED=0
ALLOW_EMPTY=0
FILTER=""
# Set when --filter derived the package scope itself (see "--filter → owning
# crates"). The empty-run banner reads it to name the right culprit.
FILTER_AUTOSCOPED=0
# Set when the junit report we translated belongs to a DIFFERENT nextest run
# (see the attribution check after the nextest invocation).
JUNIT_MISMATCH=0
our_run_id=""
junit_run_id=""
FILTER_WORKSPACE=0
ENGINE="auto"
# Concurrency budget. Empty ⇒ resolve_cargo_jobs decides from cores + free
# memory; see lib/cargo-jobs.sh. The env var is the per-machine lever (a
# box that always holds a big model can pin itself low in a shell profile)
# and --jobs overrides it per run.
JOBS_REQUEST="${SOVEREIGN_TEST_JOBS:-}"
# nextest writes its JUnit report under <target>/nextest/<profile>/. Pinned to
# the `default` profile, whose fail-fast=false matches the gate's --no-fail-fast
# intent; .config/nextest.toml defines the junit path for it.
NEXTEST_PROFILE="default"
# Whether to pass the `<pkg>/<feature>` flags at all. WHICH flags is decided
# later, from the resolved package selection — see "Feature selection".
WANT_FEATURES=1
EXTRA_FEATURES=""
# Whether to run the `cargo test --doc` pass alongside nextest.
#
# nextest cannot run doctests, so this pass is the only thing that would
# execute one. It used to be unconditional and was described as "~4s of pure
# insurance" — but MEASURED 2026-07-28 it is 17.4s of a 63s warm workspace
# gate (28%), and it runs ZERO doctests because the workspace has none
# collectable. That is a lot of iteration tax for insurance against a file
# nobody has written yet.
#
# It cannot be auto-detected cheaply: static scanning finds 25 candidate
# fences (vendored crates, `rust,ignore` blocks) while cargo itself collects
# 0 runnable doctests, so any grep-based guess just runs the pass anyway.
# So it is an explicit choice — OFF for local iteration, ON in CI (see
# .github/workflows/ci.yml) — and, critically, the run REPORTS which it did.
# A gate that quietly verifies less than you think is the exact failure class
# this script's empty-run guard exists to prevent.
DOCTESTS=0

print_help() {
    sed -n '2,/^$/p' "${BASH_SOURCE[0]}" | sed 's/^# \?//'
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --human) HUMAN=1; shift ;;
        --package)
            shift
            IFS=',' read -ra parts <<< "$1"
            for p in "${parts[@]}"; do PACKAGES+=("$p"); done
            shift
            ;;
        --changed) CHANGED=1; shift ;;
        --filter)
            shift
            FILTER="$1"
            shift
            ;;
        --filter-workspace) FILTER_WORKSPACE=1; shift ;;
        --engine)
            shift
            ENGINE="$1"
            case "$ENGINE" in
                auto|nextest|cargo) ;;
                *) echo "sovereign-test: --engine must be auto|nextest|cargo (got '$ENGINE')" >&2; exit 2 ;;
            esac
            shift
            ;;
        --jobs)
            shift
            JOBS_REQUEST="$1"
            shift
            ;;
        --no-default-features)
            WANT_FEATURES=0
            shift
            ;;
        --keep-logs) KEEP_LOGS=1; shift ;;
        --allow-empty) ALLOW_EMPTY=1; shift ;;
        --doctests) DOCTESTS=1; shift ;;
        --no-doctests) DOCTESTS=0; shift ;;
        -h|--help) print_help; exit 0 ;;
        *)
            echo "sovereign-test: unknown arg '$1' (use --help)" >&2
            exit 2
            ;;
    esac
done

# ── Concurrency budget ─────────────────────────────────────────────────────
# Resolved once, here, and applied to both the build and the run below.
# Exits rather than guessing on a malformed request: a typo'd --jobs that
# silently fell back to "all cores" would reintroduce the exact hazard.
if ! resolve_cargo_jobs "$JOBS_REQUEST"; then
    exit 2
fi

# ── --changed → owning crates ──────────────────────────────────────────────
# Map each git-changed .rs / Cargo.toml file to the crate that owns it,
# then feed those crate names into PACKAGES so the existing `-p` plumbing
# builds + runs ONLY the touched crates (and their dependents' tests).
# This is a genuine INPUT filter: cargo never compiles an untouched crate.
#
# crate_for_path and keep_members come from lib/cargo-scope.sh (sourced above),
# so this runner and nextest.sh resolve crate ownership identically.

if [[ $CHANGED -eq 1 ]]; then
    changed_crates=()
    skipped_paths=()
    # Tracked changes vs HEAD + untracked files, restricted to Rust build
    # inputs. A Cargo.toml change means dependency/feature churn — include
    # its crate too.
    while IFS= read -r f; do
        [[ -z "$f" ]] && continue
        case "$f" in
            *.rs|*/Cargo.toml|Cargo.toml) ;;
            *) continue ;;
        esac
        c="$(crate_for_path "$f")"
        if [[ -n "$c" ]]; then
            changed_crates+=("$c")
        else
            skipped_paths+=("$f")
        fi
    done < <(
        { git -C "$REPO_ROOT" diff --name-only HEAD 2>/dev/null
          git -C "$REPO_ROOT" ls-files --others --exclude-standard 2>/dev/null
        } | sort -u
    )

    # De-dup crate names into PACKAGES (union with any explicit --package).
    if [[ ${#changed_crates[@]} -gt 0 ]]; then
        while IFS= read -r c; do
            [[ -z "$c" ]] && continue
            already=0
            for p in ${PACKAGES[@]+"${PACKAGES[@]}"}; do
                [[ "$p" == "$c" ]] && { already=1; break; }
            done
            [[ $already -eq 0 ]] && PACKAGES+=("$c")
        done < <(printf '%s\n' "${changed_crates[@]}" | sort -u | keep_members)
    fi

    if [[ ${#PACKAGES[@]} -gt 0 ]]; then
        echo "sovereign-test: --changed scoped to: ${PACKAGES[*]}" >&2
        [[ ${#skipped_paths[@]} -gt 0 ]] && \
            echo "sovereign-test: --changed ignored ${#skipped_paths[@]} non-crate path(s) (e.g. ${skipped_paths[0]})" >&2
    else
        echo "sovereign-test: --changed found no touched crate — running FULL workspace (never under-cover)" >&2
    fi
fi

# ── --filter → owning crates ───────────────────────────────────────────────
# A libtest name filter narrows which tests RUN but tells cargo nothing about
# which crates to COMPILE, so `--filter one_test` used to pay the full
# workspace build: measured 2026-07-24 at 36s (22s cargo staleness-checking 52
# crates + 14s launching all 229 test binaries so each could filter everything
# out) against 1s for the same single test under `cargo test -p <crate>`.
#
# Close that gap by deriving the crate scope from the pattern itself. libtest
# matches the filter as a SUBSTRING of each test's full path (module path +
# fn name), so every test the filter can match necessarily has that substring
# somewhere in its own crate's .rs sources. Grepping for it therefore
# OVER-approximates the owning crates — it can select a crate that turns out
# to have no matching test (harmless: that crate just reports 0 tests run),
# but it cannot miss one that does. A broad pattern selects broadly and
# degrades gracefully to the full workspace, i.e. to the old behaviour.
#
# `git grep --untracked` (not `grep -r`) is load-bearing: it honours
# .gitignore, so it never descends into target/ — which is 474 GB on a
# working machine — while still seeing new, not-yet-committed test files.
#
# Explicit --package/--changed win: if the caller already scoped the build,
# --filter goes back to being a pure run-narrower within that scope.
if [[ -n "$FILTER" && ${#PACKAGES[@]} -eq 0 && $FILTER_WORKSPACE -eq 0 ]]; then
    filter_crates=()
    while IFS= read -r f; do
        [[ -z "$f" ]] && continue
        c="$(crate_for_path "$f")"
        [[ -n "$c" ]] && filter_crates+=("$c")
    done < <(git -C "$REPO_ROOT" grep -l --untracked -F -e "$FILTER" -- '*.rs' 2>/dev/null)

    if [[ ${#filter_crates[@]} -gt 0 ]]; then
        while IFS= read -r c; do
            [[ -z "$c" ]] && continue
            PACKAGES+=("$c")
        done < <(printf '%s\n' "${filter_crates[@]}" | sort -u | keep_members)
    fi

    if [[ ${#PACKAGES[@]} -gt 0 ]]; then
        # Remember that the SCOPE was inferred, not asked for. If this run then
        # matches zero tests, the empty-run banner needs to know whether to
        # blame the heuristic or the caller.
        FILTER_AUTOSCOPED=1
        echo "sovereign-test: --filter '${FILTER}' scoped to ${#PACKAGES[@]} crate(s): ${PACKAGES[*]}" >&2
        echo "sovereign-test:   (--filter-workspace keeps the old compile-everything behaviour)" >&2
    else
        echo "sovereign-test: --filter '${FILTER}' matched no workspace crate — running FULL workspace (never under-cover)" >&2
    fi
fi

# ── Feature selection — scope-aware ────────────────────────────────────────
# resolve_features (lib/cargo-scope.sh) picks the `<pkg>/<feature>` flags that
# are both needed for coverage and LEGAL for this selection — passing one whose
# package is outside the `-p` set is a hard cargo error, not a no-op. nextest.sh
# calls the same helper, so the two runners cover identically.
if [[ $WANT_FEATURES -eq 1 ]]; then
    feature_list="$(resolve_features ${PACKAGES[@]+"${PACKAGES[@]}"})"
    if [[ -n "$feature_list" ]]; then
        EXTRA_FEATURES="--features $feature_list"
        [[ ${#PACKAGES[@]} -gt 0 ]] && \
            echo "sovereign-test: features in scope: ${feature_list}" >&2
    else
        EXTRA_FEATURES=""
    fi
fi

# ── Engine resolution ──────────────────────────────────────────────────────
# `auto` prefers nextest and silently falls back to cargo, so a machine that
# never ran bootstrap still has a working gate — nextest is a speed win, not a
# correctness dependency.
if [[ "$ENGINE" == "auto" ]]; then
    if command -v cargo-nextest >/dev/null 2>&1; then
        ENGINE="nextest"
    else
        ENGINE="cargo"
    fi
elif [[ "$ENGINE" == "nextest" ]] && ! command -v cargo-nextest >/dev/null 2>&1; then
    # Explicitly requested but absent — do NOT silently downgrade. The caller
    # asked for a specific executor; quietly running a different one is how a
    # "nextest is green" claim ends up meaning nothing.
    echo "sovereign-test: --engine nextest requested but cargo-nextest is not installed." >&2
    echo "sovereign-test:   Install: cargo install cargo-nextest --locked" >&2
    echo "sovereign-test:   Or:      ${REPO_ROOT}/scripts/bootstrap.sh" >&2
    echo "sovereign-test:   Or run with --engine cargo." >&2
    exit 2
fi

# Scope flags are engine-independent: `--workspace` covers every member, `-p`
# filters stack so `--package foo --package bar` runs only those.
scope_argv=()
if [[ ${#PACKAGES[@]} -eq 0 ]]; then
    scope_argv+=(--workspace)
else
    for p in "${PACKAGES[@]}"; do scope_argv+=(-p "$p"); done
fi

# ── Target-dir isolation for scoped runs ───────────────────────────────────
# A scoped `-p` build resolves feature UNIFICATION over a smaller crate set
# than `--workspace` does. On a SHARED target dir that flip changes the rustc
# inputs for corpus-engine + its ~17 dependents, so every alternation between
# a `--changed`/`--package` run and a full `--workspace` run (the daemon
# watcher, the pre-merge gate) misses the sccache cache key and triggers a
# full recompile of that closure — the observed 14-minute "build" cost.
#
# sccache is keyed by compiler inputs, NOT by target dir, so a dedicated
# CARGO_TARGET_DIR for scoped runs (a) stops them poisoning the workspace
# cache the watcher keeps warm, and (b) still builds fast because the shared
# sccache serves every unchanged crate. Full-workspace runs keep the default
# target dir so the daemon watcher and the pre-merge gate share one warm cache.
#
# (b) IS LOAD-BEARING, AND IT IS A PRECONDITION — NOT A GIVEN. Without a wired
# sccache the isolated dir is simply a second, permanently-cold build tree: on
# RuggedFox 2026-07-24 it had grown to 37 GB and gone a week stale, so reaching
# for --package/--changed — the lever meant to make a run LEAN — bought a cold
# rebuild instead. That inverts the whole point of the flag, so only redirect
# when sccache is genuinely in play.
#
# The fallback (sharing the workspace target dir) re-accepts the feature-
# unification thrash this isolation was introduced to prevent: a scoped build
# resolves corpus-engine's features over a smaller crate set, so alternating
# scoped and --workspace runs can invalidate corpus-engine + its ~17 dependents.
# That costs a recompile of one closure. The isolated-and-cold alternative costs
# a from-scratch build of everything, every time. Cheaper disease than cure.
#
# Respect an explicit CARGO_TARGET_DIR from the environment (CI / operator
# override) — only redirect when the caller hasn't pinned one.
if [[ ${#PACKAGES[@]} -gt 0 && -z "${CARGO_TARGET_DIR:-}" ]]; then
    if command -v sccache >/dev/null 2>&1 && [[ -n "${RUSTC_WRAPPER:-}" ]]; then
        export CARGO_TARGET_DIR="${REPO_ROOT}/target/sovereign-test-scoped"
        echo "sovereign-test: scoped run → isolated target dir ${CARGO_TARGET_DIR#$REPO_ROOT/} (sccache wired; keeps the --workspace cache warm)" >&2
    else
        echo "sovereign-test: scoped run → sharing the workspace target dir (no wired sccache;" >&2
        echo "sovereign-test:   an isolated dir would be a guaranteed cold rebuild). Set" >&2
        echo "sovereign-test:   RUSTC_WRAPPER=sccache to get isolation back." >&2
    fi
fi
# ── Journal isolation ──────────────────────────────────────────────────────
# Tests that exercise the grounding gate (or any journaling code path) must
# not write into the operator's real ~/.svrnmesh/journal — discovered live
# 2026-08-07 when one workspace run left 17 stub-gate decision lines in the
# production grounding journal. Redirect the whole layer to a throwaway dir
# via its own escape hatch (SOVEREIGN_JOURNAL_DIR, read by journal_dir());
# the journal unit tests are unaffected because they pass explicit tempdir
# paths and never resolve through journal_dir(). Not =off: that would flip
# JournalStream::enabled globally and fail the layer's own append tests.
if [[ -z "${SOVEREIGN_JOURNAL_DIR:-}" ]]; then
    export SOVEREIGN_JOURNAL_DIR="$(mktemp -d "${TMPDIR:-/tmp}/sovereign-test-journal.XXXXXX")"
fi

# ── Engine argv ────────────────────────────────────────────────────────────
# Both engines take a trailing positional as a SUBSTRING name filter, so
# --filter means the same thing either way (nextest's richer -E expression
# language is available in scripts/nextest.sh, not here — the gate keeps one
# filter semantics so its results are comparable across engines).
doc_argv=()
if [[ "$ENGINE" == "nextest" ]]; then
    # --no-tests=pass restores cargo's semantics: a filter that matches nothing
    # is 0 tests and exit 0, not a failure. Load-bearing because --filter
    # auto-scoping deliberately OVER-approximates the owning crates, so
    # selecting a crate with no matching test is an expected, benign outcome.
    # nextest's default for this is exit 4.
    # shellcheck disable=SC2206
    # `--no-tests=fail` (was `pass`) is the ROOT of the fail-open in note
    # 8def98d7: `pass` tells nextest to exit 0 SILENTLY when a scope contains
    # no matching tests, which is how `--filter <a real test name>` produced
    # "✓ All green" off a zero-test run. `fail` makes nextest say so itself
    # (exit 4); the empty-run guard near the summary turns that into a banner
    # naming the scope, and `--allow-empty` restores the old tolerance for
    # callers that genuinely expect an empty scope.
    cargo_argv=(nextest run "${scope_argv[@]}" $EXTRA_FEATURES
                --profile "$NEXTEST_PROFILE" --no-fail-fast --no-tests=fail)
    # Two flags, not one: `--build-jobs` bounds the cargo build,
    # `--test-threads` bounds the run. Separate knobs in nextest, but ONE
    # decision here — the phases never overlap and draw on the same
    # memory, so a second budget would be a second way to say one thing.
    # Appended conditionally rather than expanded from a possibly-empty
    # array: `"${empty[@]}"` under `set -u` is an error on bash 3.2, which
    # is what `/bin/bash` still is on the macOS peers.
    if [[ "$CARGO_JOBS" -gt 0 ]]; then
        cargo_argv+=(--build-jobs "$CARGO_JOBS" --test-threads "$CARGO_JOBS")
    fi
    [[ -n "$FILTER" ]] && cargo_argv+=(-- "$FILTER")

    # nextest CANNOT run doctests (upstream limitation, not a config choice), so
    # the gate appends a plain cargo doctest pass. The workspace has 43 doctest
    # targets and 0 runnable doctests today, making this ~4s of pure insurance —
    # but without it, the first doctest anyone writes would silently never run.
    # shellcheck disable=SC2206
    doc_argv=(test --doc "${scope_argv[@]}" $EXTRA_FEATURES --no-fail-fast)
    [[ "$CARGO_JOBS" -gt 0 ]] && doc_argv+=(-j "$CARGO_JOBS")
    doc_libtest_argv=()
    [[ "$CARGO_JOBS" -gt 0 ]] && doc_libtest_argv+=(--test-threads "$CARGO_JOBS")
    [[ -n "$FILTER" ]] && doc_libtest_argv+=("$FILTER")
    [[ ${#doc_libtest_argv[@]} -gt 0 ]] && doc_argv+=(-- "${doc_libtest_argv[@]}")
else
    # Plain cargo splits the budget across the `--` boundary: `-j` is the
    # BUILD parallelism (a cargo flag), `--test-threads` is the RUN
    # parallelism (a libtest flag). Same one number, two places it has to
    # be said — cargo has no single knob for it the way nextest does.
    # shellcheck disable=SC2206
    cargo_argv=(test "${scope_argv[@]}" $EXTRA_FEATURES --no-fail-fast)
    [[ "$CARGO_JOBS" -gt 0 ]] && cargo_argv+=(-j "$CARGO_JOBS")
    libtest_argv=()
    [[ "$CARGO_JOBS" -gt 0 ]] && libtest_argv+=(--test-threads "$CARGO_JOBS")
    [[ -n "$FILTER" ]] && libtest_argv+=("$FILTER")
    [[ ${#libtest_argv[@]} -gt 0 ]] && cargo_argv+=(-- "${libtest_argv[@]}")
fi

# ── Adapter-absent fallback ────────────────────────────────────────────────
if [[ ! -x "$ADAPTER" ]]; then
    echo "sovereign-test: adapter not found at $ADAPTER — running raw cargo test" >&2
    # stdin from /dev/null: test binaries must never inherit an interactive
    # terminal. A prompt helper that guards on `stdin().is_terminal()` (e.g.
    # sovereign-cli-shared::confirm) sees a TTY when run from a shell and
    # blocks in read_line forever — hanging the whole workspace run with no
    # output. /dev/null makes that non-tty EOF path structurally guaranteed.
    (cd "$REPO_ROOT" && cargo "${cargo_argv[@]}" </dev/null 2>&1)
    exit $?
fi

# ── Run cargo test --workspace ─────────────────────────────────────────────
# Per-invocation scratch dir so concurrent runs (e.g. daemon watcher
# + manual run) don't collide on the log files. Promoted to
# LOG_DIR/latest at the end.
mkdir -p "$LOG_DIR"
RUN_DIR="${LOG_DIR}/.runs/$$-$(date +%s)"
mkdir -p "$RUN_DIR"

raw_log="${RUN_DIR}/cargo.raw.log"
out_jsonl="${RUN_DIR}/cargo.jsonl"
exit_file="${RUN_DIR}/cargo.exit"

start_ms=$(($(date +%s%N) / 1000000))

# stdin from /dev/null throughout: the pipes below only redirect cargo's
# STDOUT, so without this the test binaries inherit the caller's interactive
# terminal as stdin. A prompt helper that guards on `stdin().is_terminal()`
# then sees a TTY, skips its non-tty EOF fast-path, and blocks in read_line
# forever (observed: prompts::confirm hangs the entire --workspace run with
# zero output under --human). /dev/null forces the non-tty path everywhere.
if [[ "$ENGINE" == "nextest" ]]; then
    # nextest reports via a JUnit file rather than parseable stdout. That file
    # is written at the END of a run, so a run that dies during COMPILATION
    # leaves the PREVIOUS run's report in place — and translating that would
    # report a stale green as if it were current, the exact orphaned-results
    # failure this repo treats as unforgivable. Delete it first: absent report
    # then unambiguously means "this run produced no results".
    # WHERE nextest writes the report is NOT simply <CARGO_TARGET_DIR>/nextest.
    # Measured 2026-07-26: with sccache wired, a scoped run exports
    # CARGO_TARGET_DIR=target/sovereign-test-scoped, but nextest still wrote
    # its junit under the WORKSPACE target dir. The adapter was then handed a
    # path that did not exist, exited 1, and the gate reported `pass: 0
    # fail: 0` for a run with 2 real failures — results silently destroyed on
    # EVERY scoped nextest run on an sccache-wired machine.
    #
    # So don't predict the path — search the candidates and identify the
    # report by RUN ID. nextest prints `Nextest run ID <uuid>` and stamps the
    # same uuid on the report, which resolves the path AND rules out adopting
    # another run's results (the daemon's watcher runs continuously against
    # the workspace dir) in one check.
    junit_candidates=()
    [[ -n "${CARGO_TARGET_DIR:-}" ]] && \
        junit_candidates+=("${CARGO_TARGET_DIR}/nextest/${NEXTEST_PROFILE}/junit.xml")
    junit_candidates+=("${REPO_ROOT}/target/nextest/${NEXTEST_PROFILE}/junit.xml")

    # Delete ours-to-be first: an absent report must mean "this run produced
    # no results", never "a previous run's results are still lying here". The
    # uuid check below is what makes adoption impossible, but deleting keeps
    # the failure mode simple when nextest dies during compilation.
    for c in "${junit_candidates[@]}"; do rm -f "$c"; done

    (
        cd "$REPO_ROOT"
        cargo "${cargo_argv[@]}" </dev/null 2>&1 | tee "$raw_log"
        echo "${PIPESTATUS[0]}" > "$exit_file"
    )

    our_run_id="$(grep -o 'Nextest run ID [0-9a-f-]\{36\}' "$raw_log" 2>/dev/null | tail -1 | awk '{print $4}')"
    junit_path=""
    for c in "${junit_candidates[@]}"; do
        [[ -f "$c" ]] || continue
        junit_run_id="$(grep -o 'uuid="[0-9a-f-]\{36\}"' "$c" 2>/dev/null | head -1 | cut -d'"' -f2)"
        if [[ -z "$our_run_id" || "$junit_run_id" == "$our_run_id" ]]; then
            junit_path="$c"
            break
        fi
        # A report exists but belongs to someone else — remember it for the
        # diagnostic, and keep looking.
        junit_other="$c"
    done

    nextest_rc="$(cat "$exit_file" 2>/dev/null || echo 1)"
    if [[ -n "$junit_path" ]]; then
        "$NEXTEST_ADAPTER" "$junit_path" > "$out_jsonl" 2>>"$raw_log"
    elif [[ "$nextest_rc" == "4" ]]; then
        # nextest's own "no tests to run". A missing report is EXPECTED here,
        # not a lost one — the empty-run guard names this outcome.
        : > "$out_jsonl"
    else
        # No report we can attribute to this run. Never fall through to the
        # adapter's empty 0/0 summary — that is indistinguishable from a
        # genuinely green run, which is the whole failure class this gate is
        # being hardened against.
        JUNIT_MISMATCH=1
        : > "$out_jsonl"
    fi

    # Doctest pass — appended to the same log and JSONL stream, through the
    # libtest adapter (cargo test --doc still speaks libtest). Opt-in; see
    # the DOCTESTS default above for why.
    doc_exit_file="${RUN_DIR}/doc.exit"
    if [[ $DOCTESTS -eq 1 ]]; then
        (
            cd "$REPO_ROOT"
            cargo "${doc_argv[@]}" </dev/null 2>&1 | tee -a "$raw_log" | "$ADAPTER" "monorepo" >> "$out_jsonl"
            echo "${PIPESTATUS[0]}" > "$doc_exit_file"
        )
    fi

    elapsed_ms=$(( $(date +%s%N) / 1000000 - start_ms ))
    exit_val=$(cat "$exit_file" 2>/dev/null || echo 1)
    if [[ $DOCTESTS -eq 1 ]]; then
        doc_exit=$(cat "$doc_exit_file" 2>/dev/null || echo 1)
        # First non-zero wins, so a green nextest run can't mask a red doctest.
        [[ "$exit_val" == "0" ]] && exit_val="$doc_exit"
    fi
else
    (
        cd "$REPO_ROOT"
        cargo "${cargo_argv[@]}" </dev/null 2>&1 | tee "$raw_log" | "$ADAPTER" "monorepo" > "$out_jsonl"
        echo "${PIPESTATUS[0]}" > "$exit_file"
    )
    elapsed_ms=$(( $(date +%s%N) / 1000000 - start_ms ))
    exit_val=$(cat "$exit_file" 2>/dev/null || echo 1)
fi

# ── Build-vs-run split (glassbox) ──────────────────────────────────────────
# cargo prints a `Finished ... target(s) in <Xm >Ys` line the moment compilation
# ends and test execution begins. Parse it so the summary can say whether a slow
# run was COMPILE cost (cache thrash / cold build) or genuinely slow tests — the
# distinction that turns "it's slow" into an actionable lead.
#
# SUM every such line rather than taking the last: the nextest path runs two
# cargo invocations (the nextest build, then the doctest build) and so emits
# two markers. Taking `tail -1` there would report only the doctest build —
# seconds — and make a cold 20-minute compile look like a slow test suite. The
# cargo path emits exactly one line, so summing is identical to the old
# behaviour for it.
build_secs=""
if grep -qE 'Finished .* target\(s\) in ' "$raw_log" 2>/dev/null; then
    # Forms: "in 13m 56s", "in 2m 03s", "in 8.42s".
    build_secs="$(grep -E 'Finished .* target\(s\) in ' "$raw_log" 2>/dev/null | awk '
        {
            m = 0; s = 0
            if (match($0, / in [0-9]+m /)) {
                mstr = substr($0, RSTART + 4, RLENGTH - 5); m = mstr + 0
            }
            if (match($0, /[0-9]+(\.[0-9]+)?s/)) {
                s = substr($0, RSTART, RLENGTH - 1) + 0
            }
            total += m * 60 + s
        }
        END { printf "%.0f", total }
    ')"
fi

# ── Aggregate ───────────────────────────────────────────────────────────────
# ONE python pass over the adapter JSONL — not three-forks-per-line.
#
# The prior implementation spawned up to three `python3` processes for
# EVERY record just to pull one field: on a full ~7.7k-test run that is
# ~20k process spawns at ~30-80ms each on macOS — minutes of pure fork
# overhead to recompute two counters the adapter already emits in its
# trailing `summary` record. This single invocation:
#   - reads the authoritative pass/fail counts from that summary record,
#   - collects failing test names into a sidecar file, and
#   - in daemon mode (HUMAN=0), streams every non-summary record straight
#     to stdout (our own `final_summary` below, with the real elapsed_ms,
#     replaces the adapter's ms=0 one).
# Counts come ONLY from the summary record, matching the prior behaviour:
# a build error with no summary leaves both at 0 (and exit_val carries the
# failure signal downstream).
total_pass=0
total_fail=0
total_warn=0
failed_names=""
fails_file="${RUN_DIR}/failed_names.txt"
counts_file="${RUN_DIR}/counts.env"

HUMAN="$HUMAN" python3 - "$out_jsonl" "$counts_file" "$fails_file" <<'PY'
import sys, json, os

in_path, counts_path, fails_path = sys.argv[1], sys.argv[2], sys.argv[3]
human = os.environ.get("HUMAN", "0") == "1"
emit = not human  # daemon mode streams the JSONL through to stdout

total_pass = 0
total_fail = 0
total_warn = 0   # skipped/ignored tests — nextest reports these explicitly
failed = []
out = sys.stdout

with open(in_path, "r", errors="replace") as fh:
    for line in fh:
        s = line.strip()
        if not s:
            continue
        try:
            d = json.loads(s)
        except Exception:
            # Non-JSON noise: pass through in daemon mode, drop otherwise.
            if emit:
                out.write(line if line.endswith("\n") else line + "\n")
            continue
        kind = d.get("t", "")
        if kind == "summary":
            # Authoritative counts; our final_summary replaces these records.
            #
            # ACCUMULATE, don't assign. The nextest path concatenates two
            # adapter outputs (JUnit translation + the doctest pass), so there
            # are TWO summary records; assigning would let the trailing doctest
            # summary (0/0 on a workspace with no doctests) silently overwrite
            # a 7993-test result and report an empty run as green. The cargo
            # path emits exactly one summary, so summing is identical there.
            total_pass += d.get("pass", 0)
            total_fail += d.get("fail", 0)
            total_warn += d.get("warn", 0)
            continue
        if kind == "fail":
            n = d.get("n", "")
            if n:
                failed.append(n)
        if emit:
            out.write(line if line.endswith("\n") else line + "\n")

with open(counts_path, "w") as cf:
    cf.write("total_pass=%d\n" % int(total_pass))
    cf.write("total_fail=%d\n" % int(total_fail))
    cf.write("total_warn=%d\n" % int(total_warn))
with open(fails_path, "w") as ff:
    ff.write("\n".join(failed))
PY

if [[ -f "$counts_file" ]]; then
    # shellcheck disable=SC1090
    source "$counts_file"
fi
failed_names="$(cat "$fails_file" 2>/dev/null || true)"

# ── Empty-run guard — a gate must NEVER render "green" from zero tests ─────
#
# cargo exits 0 when a scope contains no matching tests, so the exit code
# alone cannot tell "everything passed" from "nothing ran". Before this guard
# the summary read the exit code and printed `pass: 0  fail: 0  ✓ All green`.
#
# Measured 2026-07-26 (note 8def98d7): triaging a red test with
# `--filter cross_view_digest_surfaces_resonance` built for 252s, ran ZERO
# tests, and reported All green — while `cargo nextest run -p sovereign-tools
# --test main knowledge_view_e2e cross_view_digest` ran 2 and passed 2. The
# --filter → owning-crate auto-scoping (see above) had picked a scope where
# the target test binary never ran. An agent triaging that way gets a green
# light having verified nothing: the same fail-open class as note d4a08e0d.
#
# So pass==0 && fail==0 on a clean cargo exit is its OWN outcome — NO TESTS
# MATCHED — with its own exit code (4: distinct from 1=failures, 2=usage,
# 101=cargo panic) and a banner that names the resolved scope, because the
# scope is what the caller got wrong. `--allow-empty` opts out.
#
# A dirty cargo exit is NOT an empty run: it is a build error, which the
# existing branch below already reports correctly. The ONE exception is
# nextest's own exit 4 ("no tests to run") — same outcome, already named
# honestly by the runner, so it takes the empty-run banner rather than the
# "likely a build error" one. nextest's `--no-tests` defaults to `auto`,
# which downgrades to a WARNING (exit 0) when the caller passed a filter —
# which is exactly the case this guard exists to catch.
#
# An unattributable run (JUNIT_MISMATCH) is NOT empty — its counts are simply
# someone else's. It gets its own outcome below so a lost race can never be
# laundered into either "green" or "no tests matched".
EMPTY_RUN=0
if [[ $JUNIT_MISMATCH -eq 0 ]] \
   && [[ "$total_pass" -eq 0 && "$total_fail" -eq 0 ]] \
   && { [[ "$exit_val" == "0" ]] || [[ "$exit_val" == "4" ]]; }; then
    EMPTY_RUN=1
fi
final_exit="$exit_val"
if [[ $JUNIT_MISMATCH -eq 1 ]]; then
    final_exit=5
elif [[ $EMPTY_RUN -eq 1 ]]; then
    # --allow-empty means the caller has decided an empty scope is acceptable,
    # so it must also clear nextest's native exit 4 — otherwise the opt-out
    # would only work under the cargo engine.
    if [[ $ALLOW_EMPTY -eq 1 ]]; then final_exit=0; else final_exit=4; fi
fi

# `empty` rides the summary record so daemon-mode consumers (which never see
# the --human banner) can tell an empty run from a green one too.
empty_json=false
[[ $EMPTY_RUN -eq 1 ]] && empty_json=true
# `doctests` rides the summary for the same reason `empty` does: a daemon-mode
# consumer never sees the --human banner and must still be able to tell what
# this run actually covered.
doctests_json=false
[[ $DOCTESTS -eq 1 ]] && doctests_json=true
final_summary="{\"t\":\"summary\",\"pass\":${total_pass},\"fail\":${total_fail},\"warn\":${total_warn},\"ms\":${elapsed_ms},\"empty\":${empty_json},\"doctests\":${doctests_json}}"

if [[ $HUMAN -eq 1 ]]; then
    {
        echo
        echo "═══════════════════════════════════════════════════════════════"
        echo " sovereign-test — repo-wide regression gate"
        echo "═══════════════════════════════════════════════════════════════"
        # Name the executor: two engines that agree are a stronger signal than
        # one, but only if you can tell which one produced the number.
        if [[ "$ENGINE" == "nextest" ]]; then
            if [[ $DOCTESTS -eq 1 ]]; then
                printf " %-12s  %s\n" "engine:" "nextest (+ cargo doctest pass)"
            else
                # Named explicitly: nextest cannot run doctests, so with the
                # pass off this run verified none. Say it rather than let a
                # green banner imply coverage it does not have.
                printf " %-12s  %s\n" "engine:" "nextest"
                printf " %-12s  %s\n" "doctests:" "SKIPPED (--doctests to include; CI runs them)"
            fi
        else
            printf " %-12s  %s\n" "engine:" "cargo"
        fi
        # Name the concurrency and WHY. A run that is slower than the
        # reader remembers is otherwise indistinguishable from a run that
        # is slow because something is wrong, and the memory-derived
        # default legitimately varies between two runs on the same box.
        if [[ "$CARGO_JOBS" -gt 0 ]]; then
            printf " %-12s  %s\n" "jobs:" "$CARGO_JOBS — $CARGO_JOBS_REASON"
        else
            printf " %-12s  %s\n" "jobs:" "UNCAPPED — $CARGO_JOBS_REASON"
        fi
        printf " %-12s  %s\n" "pass:" "$total_pass"
        printf " %-12s  %s\n" "fail:" "$total_fail"
        # Skipped tests are invisible in cargo's output but explicit in
        # nextest's — surfacing the count keeps a newly-#[ignore]d test from
        # quietly leaving the suite.
        [[ "${total_warn:-0}" -gt 0 ]] && \
            printf " %-12s  %s\n" "skipped:" "$total_warn"
        printf " %-12s  %s\n" "elapsed:" "${elapsed_ms}ms"
        if [[ -n "$build_secs" ]]; then
            # Clamp: cargo's build marker and our wall-clock are measured a
            # beat apart, so a fast build can round just above total — never
            # show a negative "tests" figure.
            run_secs=$(awk -v e="$elapsed_ms" -v b="$build_secs" 'BEGIN{r=e/1000-b; printf "%.0f", (r<0?0:r)}')
            printf " %-12s  %s\n" "  build:" "${build_secs}s"
            if [[ "$run_secs" -lt 1 ]]; then
                printf " %-12s  %s\n" "  tests:" "<1s"
            else
                printf " %-12s  %s\n" "  tests:" "~${run_secs}s"
            fi
            # A build that dominates a multi-minute run is the cache-thrash tell.
            if [[ "$build_secs" -gt 300 ]]; then
                printf " %-12s  %s\n" "  ⚠ note:" "build > 5min — likely a cold/thrashed cache, not slow tests."
                printf " %-12s  %s\n" "" "sccache hit-rate: sccache --show-stats | grep 'hits rate'"
            fi
        fi
        printf " %-12s  %s\n" "cargo exit:" "$exit_val"
        echo

        if [[ $JUNIT_MISMATCH -eq 1 ]]; then
            echo " ✘ NOT GREEN — no results could be attributed to this run."
            echo
            echo "   The tests may well have run; their report could not be"
            echo "   found, so the counts above mean nothing and are not"
            echo "   reported as green."
            echo
            printf "   %-14s %s\n" "our run ID:" "${our_run_id:-<not printed>}"
            for c in "${junit_candidates[@]}"; do
                if [[ -f "$c" ]]; then
                    printf "   %-14s %s\n" "found:" "$c"
                    printf "   %-14s %s\n" "  its run ID:" \
                        "$(grep -o 'uuid="[0-9a-f-]\{36\}"' "$c" 2>/dev/null | head -1 | cut -d'"' -f2)"
                else
                    printf "   %-14s %s\n" "absent:" "$c"
                fi
            done
            echo
            echo "   Two known causes: (a) a concurrent nextest run (the daemon's"
            echo "   test watcher) overwrote the shared report; (b) nextest wrote"
            echo "   its report somewhere neither candidate covers. Re-run with"
            echo "   --engine cargo to bypass the junit path entirely."
            echo
            echo "   Exit 5 = unattributable results."
            echo
        elif [[ $EMPTY_RUN -eq 1 && $ALLOW_EMPTY -eq 0 ]]; then
            # Deliberately louder than a pass and deliberately NOT "✘ Failures":
            # nothing failed, nothing ran. The scope is the actionable fact.
            scope_desc="--workspace (all crates)"
            [[ ${#PACKAGES[@]} -gt 0 ]] && scope_desc="${PACKAGES[*]}"
            echo " ✘ NOT GREEN — no tests matched. 0 passed, 0 failed."
            echo
            echo "   A zero-test run verifies NOTHING. cargo exited 0 because"
            echo "   nothing ran, not because everything passed."
            echo
            printf "   %-10s %s\n" "scope:" "$scope_desc"
            printf "   %-10s %s\n" "filter:" "${FILTER:-<none>}"
            echo
            if [[ -n "$FILTER" ]] && [[ $FILTER_AUTOSCOPED -eq 1 ]]; then
                echo "   Most likely: --filter auto-scoped the build to crates whose"
                echo "   sources mention the pattern, and the test binary that owns"
                echo "   your test is not among them. Retry unscoped:"
                echo "     $0 --human --filter-workspace --filter '$FILTER'"
                echo "   Or name the crate directly:"
                echo "     $0 --human --package <crate> --filter '$FILTER'"
            elif [[ -n "$FILTER" ]]; then
                echo "   The filter matched no test name in this scope. libtest"
                echo "   matches a SUBSTRING of the full test path — check spelling,"
                echo "   or widen the scope you asked for."
            else
                echo "   This scope contains no tests at all. Widen it, or pass"
                echo "   --allow-empty if that is genuinely expected."
            fi
            echo
            echo "   Exit 4 = no tests matched (1 = failures, 2 = usage)."
            echo
        elif [[ "$total_fail" -gt 0 ]] || [[ "$exit_val" != "0" ]]; then
            if [[ -n "$failed_names" ]]; then
                echo " ✘ Failures:"
                while IFS= read -r failed; do
                    [[ -z "$failed" ]] && continue
                    echo "    $failed"
                done <<< "$failed_names"
            fi
            if [[ "$exit_val" != "0" ]] && [[ "$total_fail" == "0" ]]; then
                echo " ✘ Cargo exited $exit_val with no test failures parsed —"
                echo "    likely a build error. See raw log:"
                echo "      ${LOG_DIR}/latest/cargo.raw.log"
            fi
            echo
            echo " Triage:"
            echo "   - Raw cargo output:  ${LOG_DIR}/latest/cargo.raw.log"
            echo "   - Adapter JSONL:     ${LOG_DIR}/latest/cargo.jsonl"
            echo "   - Rerun a name filter: $0 --human --filter <pattern>"
            echo "   - Rerun one package:   $0 --human --package <crate>"
            echo "   - Rerun touched crates: $0 --human --changed"
            echo
        elif [[ $EMPTY_RUN -eq 1 ]]; then
            # --allow-empty was passed: still say what happened. "Green" and
            # "nothing ran" are different facts even when the caller has
            # decided the second one is acceptable.
            echo " ○ No tests matched — treated as green (--allow-empty)."
            echo
        else
            echo " ✓ All green."
            echo
        fi
    } >&2
fi

echo "$final_summary"

# ── Promote scratch run → latest ───────────────────────────────────────────
if [[ -d "$RUN_DIR" ]]; then
    rm -rf "${LOG_DIR}/latest" 2>/dev/null || true
    mv "$RUN_DIR" "${LOG_DIR}/latest" 2>/dev/null || true
fi
if [[ -d "${LOG_DIR}/.runs" ]]; then
    # shellcheck disable=SC2012
    ls -1t "${LOG_DIR}/.runs" 2>/dev/null | tail -n +6 | while IFS= read -r old; do
        rm -rf "${LOG_DIR}/.runs/${old}" 2>/dev/null || true
    done
fi

exit "$final_exit"
