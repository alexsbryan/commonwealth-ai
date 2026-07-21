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
# Coverage. One `cargo test --workspace` invocation. Pre-monorepo this
# script fanned out across three independent cargo workspaces; the
# 2026-05-10 monorepo collapse means a single root workspace covers
# every crate, and one cargo invocation does the job a fan would.
# Treesitter is enabled explicitly (`-F corpus-engine/treesitter`)
# because sovereign-test ran corpus-engine with --features treesitter
# before the merge and we don't want test coverage to silently shrink.
# `sovereign-cli/dev-tools` is enabled for the same reason: the dev
# verbs (and their integration suites — aliases, phase3 serve/init,
# phase6 retirement) are feature-gated out of the default end-user
# build, and this script tests the developer build. The default
# build's intercept contract is covered separately by
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
#                           so their smaller feature-unification set can't
#                           invalidate the --workspace cache the watcher and
#                           pre-merge gate keep warm. sccache (keyed by
#                           compiler inputs, not target dir) keeps the isolated
#                           build fast. Set CARGO_TARGET_DIR to override.
#   --filter <pattern>      Pass <pattern> to cargo test as a libtest
#                           NAME filter. This narrows which tests RUN, not
#                           which crates COMPILE: a name filter can't tell
#                           cargo to skip building a crate, so the whole
#                           selected scope still compiles. For a lean
#                           BUILD, reach for --changed / --package; use
#                           --filter to focus the run within that scope.
#   --no-default-features   Skip the corpus-engine treesitter feature
#                           (and any others). Default off.
#   --keep-logs             Preserve adapter logs even on success
#                           (failures always preserve).
#   -h, --help              This message.
#
# Outputs Tier 2 JSONL events on stdout (one per line):
#   {"t":"pass","n":"<test_name>"}
#   {"t":"fail","n":"<test_name>","out":"<captured output>"}
#   {"t":"summary","pass":<N>,"fail":<N>,"warn":0,"ms":<elapsed_ms>}
#
# Exit code: 0 iff cargo test exits 0 AND no `fail` events were
# emitted. Non-zero on any failure or build error.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADAPTER="${SCRIPT_DIR}/../sovereign/crates/sovereign-tools/src/code/test_adapters/sovereign-cargo-test-adapter"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
LOG_DIR="${REPO_ROOT}/target/sovereign-test"

PACKAGES=()
HUMAN=0
KEEP_LOGS=0
CHANGED=0
FILTER=""
EXTRA_FEATURES="--features corpus-engine/treesitter"

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
        --no-default-features)
            EXTRA_FEATURES=""
            shift
            ;;
        --keep-logs) KEEP_LOGS=1; shift ;;
        -h|--help) print_help; exit 0 ;;
        *)
            echo "sovereign-test: unknown arg '$1' (use --help)" >&2
            exit 2
            ;;
    esac
done

# ── --changed → owning crates ──────────────────────────────────────────────
# Map each git-changed .rs / Cargo.toml file to the crate that owns it,
# then feed those crate names into PACKAGES so the existing `-p` plumbing
# builds + runs ONLY the touched crates (and their dependents' tests).
# This is a genuine INPUT filter: cargo never compiles an untouched crate.
#
# "Owns" = nearest ancestor directory holding a Cargo.toml with a
# `[package]` section (the virtual workspace-root manifest has only
# `[workspace]`, so it's skipped — a change to a non-crate path like
# scripts/ resolves to no crate and is reported, not silently swallowed).
crate_for_path() {
    # Walk up from the file's directory to REPO_ROOT looking for the
    # nearest crate manifest; echo its package name, or nothing.
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
        done < <(printf '%s\n' "${changed_crates[@]}" | sort -u)
    fi

    if [[ ${#PACKAGES[@]} -gt 0 ]]; then
        echo "sovereign-test: --changed scoped to: ${PACKAGES[*]}" >&2
        [[ ${#skipped_paths[@]} -gt 0 ]] && \
            echo "sovereign-test: --changed ignored ${#skipped_paths[@]} non-crate path(s) (e.g. ${skipped_paths[0]})" >&2
    else
        echo "sovereign-test: --changed found no touched crate — running FULL workspace (never under-cover)" >&2
    fi
fi

# `sovereign-cli/dev-tools` re-enables the feature-gated dev-verb
# suites (aliases, phase3 serve/init, phase6 retirement). The
# `pkg/feature` syntax is an ERROR (not a no-op) when the package is
# outside the `-p` selection, and sovereign-cli is a leaf crate no one
# depends on — so only add it when sovereign-cli is actually selected.
if [[ -n "$EXTRA_FEATURES" ]]; then
    if [[ ${#PACKAGES[@]} -eq 0 ]]; then
        EXTRA_FEATURES+=",sovereign-cli/dev-tools"
    else
        for p in "${PACKAGES[@]}"; do
            if [[ "$p" == "sovereign-cli" ]]; then
                EXTRA_FEATURES+=",sovereign-cli/dev-tools"
                break
            fi
        done
    fi
fi

# Build cargo argv. `--workspace` covers every member; `-p` filters
# stack on top so `--package foo --package bar` runs only those.
cargo_argv=(test)
if [[ ${#PACKAGES[@]} -eq 0 ]]; then
    cargo_argv+=(--workspace)
else
    for p in "${PACKAGES[@]}"; do cargo_argv+=(-p "$p"); done
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
# Respect an explicit CARGO_TARGET_DIR from the environment (CI / operator
# override) — only redirect when the caller hasn't pinned one.
if [[ ${#PACKAGES[@]} -gt 0 && -z "${CARGO_TARGET_DIR:-}" ]]; then
    export CARGO_TARGET_DIR="${REPO_ROOT}/target/sovereign-test-scoped"
    echo "sovereign-test: scoped run → isolated target dir ${CARGO_TARGET_DIR#$REPO_ROOT/} (keeps the --workspace cache warm)" >&2
fi
# shellcheck disable=SC2206
cargo_argv+=($EXTRA_FEATURES --no-fail-fast)
if [[ -n "$FILTER" ]]; then
    cargo_argv+=(-- "$FILTER")
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

(
    cd "$REPO_ROOT"
    # stdin from /dev/null: the pipe above only redirects cargo's STDOUT, so
    # without this the test binaries inherit the caller's interactive terminal
    # as stdin. A prompt helper that guards on `stdin().is_terminal()` then
    # sees a TTY, skips its non-tty EOF fast-path, and blocks in read_line
    # forever (observed: prompts::confirm hangs the entire --workspace run with
    # zero output under --human). /dev/null forces the non-tty path everywhere.
    cargo "${cargo_argv[@]}" </dev/null 2>&1 | tee "$raw_log" | "$ADAPTER" "monorepo" > "$out_jsonl"
    echo "${PIPESTATUS[0]}" > "$exit_file"
)

elapsed_ms=$(( $(date +%s%N) / 1000000 - start_ms ))
exit_val=$(cat "$exit_file" 2>/dev/null || echo 1)

# ── Build-vs-run split (glassbox) ──────────────────────────────────────────
# cargo prints exactly one `Finished ... target(s) in <Xm >Ys` line the moment
# compilation ends and test execution begins. Parse it so the summary can say
# whether a slow run was COMPILE cost (cache thrash / cold build) or genuinely
# slow tests — the distinction that turns "it's slow" into an actionable lead.
build_secs=""
build_line="$(grep -E 'Finished .* target\(s\) in ' "$raw_log" 2>/dev/null | tail -1)"
if [[ -n "$build_line" ]]; then
    # Forms: "in 13m 56s", "in 2m 03s", "in 8.42s".
    bmin=$(sed -nE 's/.* in ([0-9]+)m .*/\1/p' <<< "$build_line")
    bsec=$(sed -nE 's/.* in ([0-9]+m )?([0-9]+(\.[0-9]+)?)s.*/\2/p' <<< "$build_line")
    build_secs=$(awk -v m="${bmin:-0}" -v s="${bsec:-0}" 'BEGIN{printf "%.0f", m*60+s}')
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
            # Authoritative counts; our final_summary replaces this record.
            total_pass = d.get("pass", 0)
            total_fail = d.get("fail", 0)
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
with open(fails_path, "w") as ff:
    ff.write("\n".join(failed))
PY

if [[ -f "$counts_file" ]]; then
    # shellcheck disable=SC1090
    source "$counts_file"
fi
failed_names="$(cat "$fails_file" 2>/dev/null || true)"

final_summary="{\"t\":\"summary\",\"pass\":${total_pass},\"fail\":${total_fail},\"warn\":0,\"ms\":${elapsed_ms}}"

if [[ $HUMAN -eq 1 ]]; then
    {
        echo
        echo "═══════════════════════════════════════════════════════════════"
        echo " sovereign-test — repo-wide regression gate"
        echo "═══════════════════════════════════════════════════════════════"
        printf " %-12s  %s\n" "pass:" "$total_pass"
        printf " %-12s  %s\n" "fail:" "$total_fail"
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

        if [[ "$total_fail" -gt 0 ]] || [[ "$exit_val" != "0" ]]; then
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

exit "$exit_val"
