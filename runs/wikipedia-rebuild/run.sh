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

# LEG 3 — the new store answers both faces on real articles. Read-only, and it
# is the first evidence that the rebuild is usable at all.
leg probe bash -c "
  for t in 'Roman Empire' 'Albert Einstein' 'Byzantine Empire'; do
    toolbox run -c sovereign-vulkan '$CLI' atlas wikipedia neighbors wikipedia \"\$t\" --limit 10 \
      2>/dev/null || exit 1
  done | tee '$OUT/probe.txt'"

date -Is > "$M/DONE"
echo "DONE — markers in $M, measurements in $OUT/measurements.txt" >&2
