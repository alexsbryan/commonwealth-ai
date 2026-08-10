#!/usr/bin/env bash
# cargo-jobs.sh — one decider for "how much of this machine may a cargo
# run take?", shared by the gate scripts.
#
# WHY THIS EXISTS
#
# Neither gate script used to set any concurrency at all, so both phases
# inherited cargo's and nextest's defaults, and both defaults are "all
# cores". On a 32-core box that is 32 concurrent rustc processes during
# the build and then 32 concurrent test binaries during the run.
#
# That is fine on an idle machine and catastrophic on a busy one. This
# fleet's workstations hold LLM weights resident, and on the Strix Halo
# the GPU's memory is the SAME physical RAM as the system's — a resident
# 70 GB model leaves ~50 GB for everything else. Thirty-two rustc
# processes (the heavy crates here peak well over 1 GB each) plus test
# binaries that link llama and in some cases load models will exceed
# that, and Linux does not OOM-kill cleanly under this shape: it
# thrashes, and the desktop locks up. Observed 2026-08-07 — a full
# `sovereign-test.sh --human` run wedged the machine hard enough to kill
# the session that started it. It is the same failure that took the
# watchers out on 2026-05-31 ("the parallel cargo fan OOM'd the daemon
# under a resident big model"), which is why the watchers are still off.
#
# THE RULE
#
# Take half the cores, but never more than the free memory can hold. The
# memory term is what makes this adaptive: on an idle box it is slack and
# the core cap decides, so you keep most of the speed; with a big model
# resident it binds and the run gets quieter instead of fatal.
#
# The budget is deliberately ONE number feeding BOTH phases (build
# parallelism and test parallelism) rather than two knobs. They draw on
# the same physical memory and never overlap in time, so a second knob
# would be two ways to say one thing — and the caller would have to get
# both right to be safe.

# Physical cores, portably. Echoes a positive integer; falls back to 4
# rather than 0 on an unrecognised platform, so a bad probe degrades to a
# conservative run instead of an unbounded one.
cargo_jobs_cores() {
    local n=""
    if command -v nproc >/dev/null 2>&1; then
        n="$(nproc 2>/dev/null)"
    elif command -v sysctl >/dev/null 2>&1; then
        n="$(sysctl -n hw.ncpu 2>/dev/null)"
    fi
    [[ "$n" =~ ^[0-9]+$ && "$n" -gt 0 ]] || n=4
    echo "$n"
}

# Memory available for new work, in whole GB. Echoes empty when the
# platform can't be probed — callers must treat empty as "unknown", NOT
# as zero, or an unrecognised platform would clamp every run to the floor.
#
# Linux: MemAvailable is the kernel's own estimate of what a new
# allocation can get without swapping, which is exactly the question.
# Do NOT use MemFree — page cache counts as reclaimable and MemFree
# would under-report it, throttling every run on a warm build tree.
#
# macOS: no direct equivalent, so sum the page classes vm_stat reports as
# reclaimable (free + inactive + speculative). Approximate, and that is
# fine: this is a safety margin, not an accounting.
cargo_jobs_available_gb() {
    if [[ -r /proc/meminfo ]]; then
        awk '/^MemAvailable:/ {printf "%d", $2/1048576; found=1}
             END {if (!found) print ""}' /proc/meminfo
        return
    fi
    if command -v vm_stat >/dev/null 2>&1; then
        vm_stat 2>/dev/null | awk '
            /page size of/ {
                for (i = 1; i <= NF; i++) if ($i ~ /^[0-9]+$/) { ps = $i; break }
            }
            /^Pages free:/         {gsub(/\./,"",$3); free = $3}
            /^Pages inactive:/     {gsub(/\./,"",$3); inact = $3}
            /^Pages speculative:/  {gsub(/\./,"",$3); spec = $3}
            END {
                if (ps > 0) printf "%d", (free + inact + spec) * ps / 1073741824
                else print ""
            }'
        return
    fi
    echo ""
}

# Resolve the budget. Sets two globals:
#   CARGO_JOBS         — integer job count, or 0 meaning "unlimited"
#   CARGO_JOBS_REASON  — short human string naming which term bound it
#
# $1 (optional): an explicit override. A positive integer pins the count;
# `0` restores the old unbounded behaviour; empty/unset means "decide".
# The override is honoured verbatim and labelled as such — an operator who
# names a number gets that number, because the machine they are protecting
# is one they can see and this heuristic cannot.
#
# GB_PER_JOB is the memory a single job may claim. Four is conservative
# for rustc alone but this budget also governs the TEST phase, where a
# binary that loads a model dwarfs any compile. Cheap insurance: on an
# idle box the core cap binds first and this term costs nothing.
resolve_cargo_jobs() {
    local override="${1:-}"
    local cores avail cap_cores cap_mem
    local -r GB_PER_JOB=4
    local -r CEILING=16
    local -r FLOOR=2

    if [[ -n "$override" ]]; then
        if [[ ! "$override" =~ ^[0-9]+$ ]]; then
            echo "cargo-jobs: job count must be a non-negative integer (got '$override')" >&2
            return 2
        fi
        CARGO_JOBS="$override"
        if [[ "$CARGO_JOBS" -eq 0 ]]; then
            CARGO_JOBS_REASON="unlimited (explicitly requested)"
        else
            CARGO_JOBS_REASON="explicitly requested"
        fi
        return 0
    fi

    cores="$(cargo_jobs_cores)"
    avail="$(cargo_jobs_available_gb)"

    # Half the cores. The other half is not idle — it absorbs mold's own
    # threads, the daemon, and whatever the operator is doing.
    cap_cores=$(( cores / 2 ))
    [[ "$cap_cores" -lt "$FLOOR" ]] && cap_cores="$FLOOR"

    if [[ -n "$avail" && "$avail" -gt 0 ]]; then
        cap_mem=$(( avail / GB_PER_JOB ))
        [[ "$cap_mem" -lt "$FLOOR" ]] && cap_mem="$FLOOR"
    else
        # Unknown memory ⇒ the core cap is the only cap. Say so in the
        # reason rather than silently pretending the check happened.
        cap_mem="$cap_cores"
        avail=""
    fi

    if [[ "$cap_mem" -lt "$cap_cores" ]]; then
        CARGO_JOBS="$cap_mem"
        CARGO_JOBS_REASON="memory-capped: ${avail}GB available, ${GB_PER_JOB}GB/job"
    else
        CARGO_JOBS="$cap_cores"
        if [[ -n "$avail" ]]; then
            CARGO_JOBS_REASON="half of ${cores} cores (${avail}GB available)"
        else
            CARGO_JOBS_REASON="half of ${cores} cores (memory unknown on this platform)"
        fi
    fi

    if [[ "$CARGO_JOBS" -gt "$CEILING" ]]; then
        CARGO_JOBS="$CEILING"
        CARGO_JOBS_REASON="capped at ${CEILING} (${cores} cores)"
    fi

    return 0
}
