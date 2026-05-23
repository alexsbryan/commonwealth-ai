#!/usr/bin/env bash
# build-bench.sh — measure sovereign-cli build performance.
#
# Phase 1 deliverable: get a repeatable baseline before changing
# anything. Per ARCH_PRINCIPLES §0.4 (don't whack moles), measure
# first, intervene second. Each run appends a JSONL row to
# `target/build-bench/log.jsonl` so trend lines survive across PRs.
#
# Scenarios:
#   noop      — `cargo build` with zero source changes. Measures
#               cargo's dependency-graph walk + relink check overhead.
#   leaf      — touch a leaf CLI module (notes_cmd.rs). Tests
#               "I edited one subcommand" cost. Should be link-only.
#   cli-dep   — touch sovereign-core::runtime. Tests "I edited a
#               transitively-used dep" cost — recompiles sovereign-core,
#               sovereign-tools, sovereign-inference, sovereign-cli.
#   cli-cold  — `cargo clean -p sovereign-cli` then build. Tests the
#               relink-only floor (deps cached, CLI fresh).
#   workspace-cold — `cargo clean` then build sovereign-cli. The full
#               5-minute pain. Opt-in (very destructive — wipes the
#               entire target/ tree, lance + llama-cpp-sys included).
#
# JSONL row shape:
#   {
#     "ts": "2026-05-22T15:40:00Z",
#     "scenario": "leaf",
#     "profile": "release",
#     "secs": 87.42,
#     "bin_bytes": 291504128,
#     "rustc": "1.94.1",
#     "linker": "lld",
#     "sccache": false,
#     "features": "corpus-engine/treesitter",
#     "host": "Darwin"
#   }
#
# Usage:
#   scripts/build-bench.sh baseline           # noop + leaf + cli-dep + cli-cold
#   scripts/build-bench.sh quick              # noop + leaf only
#   scripts/build-bench.sh cold               # cli-cold only
#   scripts/build-bench.sh full               # adds workspace-cold (destructive)
#   scripts/build-bench.sh report             # print log as table
#   scripts/build-bench.sh proposed-profiles  # print proposed Cargo.toml profile changes
#
# Environment knobs:
#   PROFILE=release           # or dev
#   PACKAGE=sovereign-cli     # target package
#   RUSTC_WRAPPER=sccache     # opt in to sccache (per-shell)
#   BUILD_BENCH_FEATURES=     # override default feature set

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$REPO_ROOT"

PROFILE="${PROFILE:-release}"
PACKAGE="${PACKAGE:-sovereign-cli}"
FEATURES="${BUILD_BENCH_FEATURES:-corpus-engine/treesitter}"
LOG_DIR="${REPO_ROOT}/target/build-bench"
LOG_FILE="${LOG_DIR}/log.jsonl"
TIMINGS_DIR="${LOG_DIR}/timings"
mkdir -p "$LOG_DIR" "$TIMINGS_DIR"

LEAF_FILE="sovereign/crates/sovereign-cli/src/notes_cmd.rs"
DEP_FILE="sovereign/crates/sovereign-core/src/runtime.rs"

# ── Helpers ───────────────────────────────────────────────────────────────

iso_ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }

bin_path() {
    case "$PROFILE" in
        release) echo "$REPO_ROOT/target/release/$PACKAGE" ;;
        dev|*)   echo "$REPO_ROOT/target/debug/$PACKAGE" ;;
    esac
}

bin_size() {
    local p; p="$(bin_path)"
    [[ -f "$p" ]] && stat -f%z "$p" 2>/dev/null || echo 0
}

detect_linker() {
    if grep -q 'fuse-ld=lld' .cargo/config.toml 2>/dev/null; then echo lld
    elif grep -q 'fuse-ld=mold' .cargo/config.toml 2>/dev/null; then echo mold
    else echo default; fi
}

sccache_active() {
    if [[ -n "${RUSTC_WRAPPER:-}" ]] && command -v "${RUSTC_WRAPPER}" >/dev/null 2>&1; then
        echo true
    else echo false; fi
}

# ── Pre-flight ────────────────────────────────────────────────────────────

preflight() {
    # Bail if daemon's lint watcher is mid-run (it holds Cargo's file
    # lock). User can stop it with `sovereign daemon stop` or wait.
    if pgrep -f 'cargo check --workspace' >/dev/null 2>&1; then
        echo "build-bench: lint watcher active — pause it first" >&2
        echo "   sovereign daemon stop   # or wait for current check to finish" >&2
        exit 1
    fi
    [[ -f "$LEAF_FILE" ]] || { echo "build-bench: $LEAF_FILE missing" >&2; exit 1; }
    [[ -f "$DEP_FILE" ]]  || { echo "build-bench: $DEP_FILE missing" >&2; exit 1; }
}

# ── Run one scenario ──────────────────────────────────────────────────────
# args: scenario_name pre_hook
run_scenario() {
    local scenario="$1"; shift
    local pre_hook="${1:-}"; shift || true

    [[ -n "$pre_hook" ]] && eval "$pre_hook"

    local timings_html="${TIMINGS_DIR}/${scenario}-$(date -u +%Y%m%d-%H%M%S).html"
    local start_ns end_ns secs

    # Build with --timings to drop HTML; gives per-crate codegen graph.
    start_ns=$(date +%s)
    cargo build -p "$PACKAGE" --profile "$PROFILE" \
        --features "$FEATURES" \
        --timings 2>&1 \
        | tail -50
    local exit_code=${PIPESTATUS[0]}
    end_ns=$(date +%s)
    secs=$(( end_ns - start_ns ))

    # cargo-timings drops HTML under target/cargo-timings/cargo-timing-*.html
    # Move the latest one into our scenario-tagged path.
    local latest_html
    latest_html=$(ls -t target/cargo-timings/cargo-timing-*.html 2>/dev/null | head -1)
    [[ -n "$latest_html" ]] && cp "$latest_html" "$timings_html"

    if [[ $exit_code -ne 0 ]]; then
        echo "build-bench: scenario '$scenario' FAILED (exit $exit_code)" >&2
        return $exit_code
    fi

    # Emit JSONL row.
    local row
    row=$(jq -nc \
        --arg ts "$(iso_ts)" \
        --arg scenario "$scenario" \
        --arg profile "$PROFILE" \
        --argjson secs "$secs" \
        --argjson bin_bytes "$(bin_size)" \
        --arg rustc "$(rustc --version | awk '{print $2}')" \
        --arg linker "$(detect_linker)" \
        --argjson sccache "$(sccache_active)" \
        --arg features "$FEATURES" \
        --arg host "$(uname -s)" \
        --arg timings "$timings_html" \
        '{ts:$ts, scenario:$scenario, profile:$profile, secs:$secs,
          bin_bytes:$bin_bytes, rustc:$rustc, linker:$linker,
          sccache:$sccache, features:$features, host:$host,
          timings_html:$timings}')
    echo "$row" | tee -a "$LOG_FILE"
}

# ── Scenarios ─────────────────────────────────────────────────────────────

scenario_noop() {
    run_scenario "noop" ""
}

scenario_leaf() {
    # Append + truncate a real byte. `touch` alone bumps mtime but
    # sccache content-hashes the file → cache hit → measurement
    # collapses to link-only. Appending a comment forces a real
    # recompile of the leaf crate.
    run_scenario "leaf" "_real_edit '$LEAF_FILE'"
}

scenario_cli_dep() {
    # Real edit to sovereign-core::runtime — cascades through
    # sovereign-tools, sovereign-inference, sovereign-cli for code
    # that actually rebuilds (not just fingerprint flip).
    run_scenario "cli-dep" "_real_edit '$DEP_FILE'"
}

# _real_edit appends a unique trailing comment + traps removal so
# the file returns to a byte-identical state on script exit. Forces
# rustc fingerprint AND sccache content-hash miss.
_real_edit() {
    local file="$1"
    local marker="// build-bench: $(date +%s%N)"
    printf '\n%s\n' "$marker" >> "$file"
    # On EXIT, strip the marker line back out. Trap stacks per
    # scenario invocation; we overwrite each time.
    trap "sed -i.bak '/build-bench: /d' '$file' && rm -f '${file}.bak'" EXIT
}

scenario_cli_cold() {
    run_scenario "cli-cold" "cargo clean -p '$PACKAGE' >/dev/null 2>&1"
}

scenario_workspace_cold() {
    echo "WARNING: scenario 'workspace-cold' wipes target/ entirely." >&2
    echo "  This will rebuild lance (~10min) + llama-cpp-sys (~5min) + ~700 crates." >&2
    echo "  Continue? (yes/N): " >&2
    read -r confirm
    [[ "$confirm" == "yes" ]] || { echo "aborted" >&2; exit 1; }
    run_scenario "workspace-cold" "cargo clean >/dev/null 2>&1"
}

# ── Report ────────────────────────────────────────────────────────────────

report() {
    local log="${1:-$LOG_FILE}"
    [[ -f "$log" ]] || { echo "no log at $log" >&2; exit 1; }
    printf "%-22s %-16s %-9s %8s %10s %8s %8s\n" \
        TS SCENARIO PROFILE SECS BIN_MB LINKER SCCACHE
    printf "%-22s %-16s %-9s %8s %10s %8s %8s\n" \
        ---- -------- ------- ---- ------ ------ -------
    jq -r '[
        (.ts | sub("T"; " ") | sub("Z"; "")),
        .scenario, .profile,
        (.secs | tostring),
        ((.bin_bytes / 1048576) | floor | tostring),
        .linker,
        (.sccache | tostring)
    ] | @tsv' "$log" \
        | awk -F'\t' '{printf "%-22s %-16s %-9s %8s %10s %8s %8s\n",$1,$2,$3,$4,$5,$6,$7}'
}

# ── Proposed profile changes (Phase 1b, not applied yet) ──────────────────

proposed_profiles() {
    cat <<'EOF'
# ── Proposed Cargo.toml additions for Phase 1b ────────────────────────────
# Apply AFTER baseline numbers exist; re-run `build-bench.sh baseline`
# and compare. Don't apply blindly — each lever has a cost.
#
# Rationale per lever:
#
# 1. `[profile.dev-release]` — new profile for the iteration loop
#    (currently we use `release` for daemon + CLI binaries, which
#    bakes lto=false codegen-units=16 into every dev compile).
#    Inherits release, dials codegen-units up + opt-level down for
#    sovereign-* + commonwealth-* code. Heavy third-party deps
#    (lance, llama-cpp-sys, arrow) keep opt-level=3 via per-package
#    overrides — they're rebuilt rarely and their runtime cost
#    dominates the daemon's hot path.
#
# 2. `incremental = true` on release — Cargo defaults to off for
#    release. The daemon eats the 30-60s incremental link cost gladly
#    if it saves 90s recompile cost. The on-disk size cost
#    (~2-4 GB extra fingerprint state) is bounded; we already carry
#    ~10 GB of target/ artifacts.
#
# 3. `strip = "debuginfo"` on release — line tables stay (panics
#    still resolve to file:line). Drops ~40-80 MB off the 278 MB
#    binary. Cheaper to link, cheaper to copy across the mesh.
#
# 4. Per-package opt-level pins. Without these, dropping the workspace
#    opt-level also softens lance + llama-cpp, which would tank
#    runtime perf. Pin them to 3.
#
# Apply by adding to root Cargo.toml after the existing [profile.dev]:

[profile.release]
incremental = true
strip = "debuginfo"

[profile.dev-release]
inherits = "release"
opt-level = 2
codegen-units = 256
lto = false
incremental = true
debug = "line-tables-only"

# Heavy deps stay at -O3 even under dev-release. These names must
# match `cargo tree -p sovereign-cli` output exactly.
[profile.dev-release.package."lance"]
opt-level = 3
[profile.dev-release.package."lance-encoding"]
opt-level = 3
[profile.dev-release.package."lance-index"]
opt-level = 3
[profile.dev-release.package."llama-cpp-sys-4"]
opt-level = 3
[profile.dev-release.package."arrow"]
opt-level = 3

# Then iterate with:
#   PROFILE=dev-release scripts/build-bench.sh baseline

# ── sccache opt-in (per-shell, not in repo) ──────────────────────────────
# Add to ~/.zshrc or ~/.bashrc:
#   export RUSTC_WRAPPER=sccache
#   export SCCACHE_DIR=~/.cache/sccache
#   export SCCACHE_CACHE_SIZE=50G
# Validate cache hits with:
#   sccache --show-stats
EOF
}

# ── Entry point ──────────────────────────────────────────────────────────

cmd="${1:-baseline}"
shift || true
case "$cmd" in
    baseline)
        preflight
        scenario_noop
        scenario_leaf
        scenario_cli_dep
        scenario_cli_cold
        echo ""
        report
        ;;
    quick)
        preflight
        scenario_noop
        scenario_leaf
        ;;
    cold)        preflight; scenario_cli_cold ;;
    full)        preflight; scenario_noop; scenario_leaf; scenario_cli_dep; scenario_cli_cold; scenario_workspace_cold ;;
    noop)        preflight; scenario_noop ;;
    leaf)        preflight; scenario_leaf ;;
    cli-dep)     preflight; scenario_cli_dep ;;
    report)      report "${1:-}" ;;
    proposed-profiles) proposed_profiles ;;
    *)
        echo "unknown command: $cmd" >&2
        echo "usage: $0 {baseline|quick|cold|full|noop|leaf|cli-dep|report|proposed-profiles}" >&2
        exit 1
        ;;
esac
