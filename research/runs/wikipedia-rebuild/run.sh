#!/usr/bin/env bash
# ei-7c — rebuild wikipedia's wiki-class columnar store at full scale.
#
# BESIDE the installed index, never in place: everything is written under
# $OUT, and the installed `~/.svrnmesh/indexes/wikipedia/atlas` is only READ.
# The cutover is a separate, seat-approved step.
#
# Per-leg exit markers land in $OUT/markers/<leg>.rc; the terminal marker is
# $OUT/markers/DONE. A leg that fails writes its rc and stops the run — an
# absent marker means the leg never ran, which is a different finding from a
# failed one (ARCH §18.3).
set -uo pipefail

WT=/home/alexbryan/dev/ei7c-wt
# $OUT lives inside the worker's own worktree under the gitignored `runs/`
# tree, not in the seat's checkout and not in `.wiki-rebuild-scratch` — the
# 2026-09-04 attempt wrote there and left markers behind (see below).
OUT="${OUT:-$WT/runs/wikipedia-rebuild/out}"
LIVE="$HOME/.svrnmesh/indexes/wikipedia/atlas"
CLI="$WT/target/debug/sovereign-cli"
M="$OUT/markers"

# A MARKER FROM A PREVIOUS RUN IS NOT A VERDICT ABOUT THIS ONE (ARCH §18.1:
# four verdicts, and "never ran" has to stay distinguishable from "failed").
# The first attempt left `rebuild.rc` = 1 in its output dir; if this run then
# died before leg 1, a reader would find a failure marker written by a
# different binary against different code. Clear them, and only then create
# the tree, so an absent marker means exactly one thing.
rm -rf "$M"
mkdir -p "$OUT/atlas" "$M"

# Attribution for the artifact this run produces (ARCH §18.5): which commit,
# which binaries, when they were built. Written before any leg so it survives
# a kill.
{
  echo "commit:      $(git -C "$WT" rev-parse HEAD)"
  echo "branch:      $(git -C "$WT" rev-parse --abbrev-ref HEAD)"
  echo "dirty:       $(git -C "$WT" status --porcelain | wc -l) uncommitted path(s)"
  for b in sovereign-cli sovereign-cli-llm; do
    echo "binary:      $WT/target/debug/$b  $(date -Is -r "$WT/target/debug/$b" 2>/dev/null || echo MISSING)"
  done
  # THE CAP THIS RUN ACTUALLY RAN UNDER, read from its own cgroup rather than
  # from the manifest. The launcher sizes MemoryMax from whatever the box has
  # free at launch, so the manifest cannot know it — and a kill is only
  # interpretable against the number that did the killing. Without this, an
  # OOM at 30G reads as "needs more than 40G", which is a different and wrong
  # finding (ARCH §18.3: name the constraint, never let it be inferred).
  echo "memory.max:  $(cat /sys/fs/cgroup/memory.max 2>/dev/null || echo 'unknown — not under a cgroup limit')"
  echo "mem_avail:   $(awk '/^MemAvailable:/{printf "%.1f GB", $2/1048576}' /proc/meminfo 2>/dev/null)"
} | tee "$OUT/provenance.txt"

# The `atlas` verb is dispatched into the `sovereign-cli-llm` sibling, so BOTH
# binaries have to exist or leg 1 fails in a way that looks like a bad corpus.
for b in sovereign-cli sovereign-cli-llm; do
  [[ -x "$WT/target/debug/$b" ]] || { echo "missing binary: $WT/target/debug/$b" >&2; exit 2; }
done

leg() {                       # leg <name> <cmd...>
  local name="$1"; shift
  echo "=== $name ===" >&2
  "$@"; local rc=$?
  echo "$rc" > "$M/$name.rc"
  [[ $rc -eq 0 ]] || { echo "FAILED $name rc=$rc" >&2; exit "$rc"; }
}

# Peak RSS from the kernel's own high-water mark. GNU `time` is not installed
# inside the sovereign-vulkan toolbox, and VmHWM is the same measurement.
measured() {                  # measured <label> <cmd...>
  local label="$1"; shift
  local t0 peak=0 pid v
  t0=$(date +%s.%N)
  "$@" > "$OUT/$label.out" 2> "$OUT/$label.err" &
  pid=$!
  while kill -0 "$pid" 2>/dev/null; do
    for p in $(pgrep -f 'sovereign-cli(-llm)? atlas wikipedia' 2>/dev/null) "$pid"; do
      v=$(awk '/^VmHWM:/{print $2}' "/proc/$p/status" 2>/dev/null)
      [[ -n "${v:-}" && "$v" -gt "$peak" ]] && peak=$v
    done
    sleep 0.5
  done
  wait "$pid"; local rc=$?
  printf '%s rc=%d wall_s=%.1f peak_rss_mb=%.0f\n' "$label" "$rc" \
    "$(echo "$(date +%s.%N) - $t0" | bc)" "$(echo "$peak / 1024" | bc)" \
    | tee -a "$OUT/measurements.txt"
  return $rc
}

# LEG 1 — the rebuild, measured, into $OUT/atlas.
leg rebuild measured rebuild \
  toolbox run -c sovereign-vulkan "$CLI" atlas wikipedia build-graph wikipedia \
    --atlas-dir "$OUT/atlas"

# LEG 2 — on-disk size of what was produced, against what it replaces.
leg sizes bash -c "
  { echo '--- new (this run) ---'; du -sh '$OUT/atlas'/* 2>/dev/null;
    echo '--- live (untouched) ---'; du -sh '$LIVE'/* 2>/dev/null;
    echo '--- live sqlite (retired by this order) ---';
    du -sh \"\$HOME/.svrnmesh/indexes/wikipedia/wikipedia_graph.db\" 2>/dev/null;
  } | tee '$OUT/sizes.txt'"

# LEG 3 — the NEW store answers on real articles. Read-only.
#
# `--atlas-dir "$OUT/atlas"` is the whole point and it is why this leg was
# worthless on 2026-09-04: `neighbors` had no such flag, dropped it silently,
# and read the INSTALLED store instead — so the probe reported on the store the
# rebuild was meant to replace and was cited as evidence for the rebuild. Fixed
# in the verb (it now honours the flag, REFUSES an unknown one, and prints the
# store it read on every line of output). Keep the flag here and keep the
# `store:` line in probe.txt: that line is what makes this leg checkable.
leg probe bash -c "
  for t in 'Roman Empire' 'Albert Einstein' 'Byzantine Empire' 'Jigsaw puzzle' 'Jigsaw Puzzle'; do
    toolbox run -c sovereign-vulkan '$CLI' atlas wikipedia neighbors wikipedia \"\$t\" --limit 10 \
      --atlas-dir '$OUT/atlas' || exit 1
  done | tee '$OUT/probe.txt'"

date -Is > "$M/DONE"
echo "DONE — markers in $M, measurements in $OUT/measurements.txt" >&2
