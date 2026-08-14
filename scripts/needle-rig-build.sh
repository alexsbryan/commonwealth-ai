#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# NEEDLE RIG — build. Order `mesh-scale-t2-needle-rig`, bar
# `t2-selection-quality`. Spec: MESH_SCALE_100_USERS_1000_CORPORA.md §8.6.
#
# WHAT IT BUILDS: a HYBRID 1000-corpus rig — k REAL, genuinely distinct
# needle corpora (ingested for real, so their embeddings and selection
# signals differ) scattered among (n-k) clone stubs that supply the bulk.
# Needles are signal; clones are the load. This closes the §8.4 rig caveat:
# the Tier-0/Tier-1 rig is `cp -r` of one index, so every corpus scores
# identically and no wrong pick is detectable.
#
# WHAT IT REUSES (ARCH §19 — inventory outranks plan):
#   - the clone-stub recipe from `scripts/probe-b-index-residency.sh`
#     (cp -r one tiny real index, stamp a unique corpus_id per clone). The
#     recipe is reproduced here rather than shelled out to because probe-b
#     is a MEASUREMENT script — it builds its rig, runs two arms, and
#     deletes it. Its rig is not addressable from outside. The stamping
#     loop is the same one-python-pass shape and is the only duplicated
#     part; if a third caller appears, lift it then, not now.
#   - the production ingest path (`svrn corpus ingest`, the shipped
#     `notebook` workflow) — no bespoke index writer.
#   - `scripts/needle_rig.py generate` for the corpora + bank + manifest.
#
# COST DISCIPLINE (order budget: <=30 min ingest wall at k=100): run this
# at --count 5 FIRST and read the EXTRAPOLATION line before committing to
# the full k. The generator seeds per-corpus RNG from "<seed>:<index>", so
# the first five corpora of a k=5 run are byte-identical to the first five
# of a k=100 run — the small probe measures the same work.
#
# SAFETY, and one honest caveat: everything WRITTEN lands under --out, which
# must not be the operator's index dir. Ingest runs under a throwaway $HOME so
# `tool:corpus_store` writes into <out>/home/.svrnmesh/indexes and never into
# ~/.svrnmesh. But the ingest READS an embedding from a daemon, and
# `corpus ingest` hardcodes `DEFAULT_DAEMON = "http://localhost:9741"`
# (corpus_cmd/ingest.rs:19) with no --daemon flag — so on this host the
# EMBEDDING calls go to the OPERATOR'S LIVE DAEMON. That is named here rather
# than worked around: it is the same embed model the stub index was built with
# (Qwen3-Embedding-0.6B-Q8_0, 1024 dims), it is read-only from the rig's point
# of view, and re-pointing it would need a production code change this order
# is not permitted to make. If the operator's daemon is down or holds a
# different embed model, ingest fails loudly rather than writing a rig whose
# vectors live in a different space.
#
# Usage:
#   scripts/needle-rig-build.sh --out DIR [--count K] [--seed S]
#                               [--total N] [--stub-source DIR] [--skip-ingest]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT=""
COUNT=100
SEED="mesh-scale-t2"
TOTAL=1000
STUB_SOURCE="$HOME/.svrnmesh/indexes/folder-df-fix-drive-4769b5117dd2"
SKIP_INGEST=0
# Prefer the main checkout's already-built debug binaries: this order
# changes no production code, so a cold worktree target buys nothing.
LLM="${NEEDLE_RIG_LLM:-$ROOT/target/debug/sovereign-cli-llm}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out)          OUT="$2"; shift 2 ;;
    --count)        COUNT="$2"; shift 2 ;;
    --seed)         SEED="$2"; shift 2 ;;
    --total)        TOTAL="$2"; shift 2 ;;
    --stub-source)  STUB_SOURCE="$2"; shift 2 ;;
    --skip-ingest)  SKIP_INGEST=1; shift ;;
    -h|--help)      sed -n '3,45p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "needle-rig-build: unknown flag: $1" >&2; exit 2 ;;
  esac
done

[[ -n "$OUT" ]] || { echo "needle-rig-build: --out is required" >&2; exit 2; }
case "$OUT" in
  */.svrnmesh/indexes|*/.svrnmesh/indexes/|"$HOME/.svrnmesh"*)
    echo "needle-rig-build: refusing to build inside the operator's data dir ($OUT)" >&2
    exit 2 ;;
esac
(( TOTAL >= COUNT )) || { echo "needle-rig-build: --total ($TOTAL) < --count ($COUNT)" >&2; exit 2; }
[[ -x "$LLM" ]] || { echo "needle-rig-build: $LLM not built" >&2; exit 1; }
[[ -d "$STUB_SOURCE/chunks.lance" ]] || {
  echo "needle-rig-build: --stub-source must be an installed index dir with chunks.lance (got $STUB_SOURCE)" >&2
  exit 2; }

SPEC="$OUT/spec"
FAKE_HOME="$OUT/home"
NEEDLE_INDEXES="$FAKE_HOME/.svrnmesh/indexes"
RIG="$OUT/rig"
mkdir -p "$SPEC" "$NEEDLE_INDEXES" "$RIG"

echo "needle-rig-build: out          $OUT"
echo "needle-rig-build: count/total  $COUNT needles + $((TOTAL - COUNT)) stubs = $TOTAL corpora"
echo "needle-rig-build: seed         $SEED"
echo "needle-rig-build: stub source  $STUB_SOURCE"

# ── 1. Generate ──────────────────────────────────────────────────────────────
python3 "$ROOT/scripts/needle_rig.py" generate --out "$SPEC" --count "$COUNT" --seed "$SEED"

# ── 2. Ingest, for real, one corpus at a time ───────────────────────────────
# Per-corpus wall is the number that prices every future rig change, so it is
# reported per corpus AND as a bracket, never as a single mean (ARCH §18.5).
if [[ "$SKIP_INGEST" == 0 ]]; then
  echo
  echo "needle-rig-build: ingesting $COUNT corpora via the production notebook workflow…"
  TIMINGS="$OUT/ingest-timings.txt"
  : > "$TIMINGS"
  FAILED=0
  IDX=0
  for DOCDIR in "$SPEC"/docs/*/; do
    CID="$(basename "$DOCDIR")"
    IDX=$((IDX + 1))
    T0=$(date +%s.%N)
    set +e
    env HOME="$FAKE_HOME" "$LLM" corpus ingest "${DOCDIR%/}" --corpus "$CID" \
      > "$OUT/ingest-$CID.log" 2>&1
    RC=$?
    set -e
    T1=$(date +%s.%N)
    W=$(python3 -c "print(f'{$T1-$T0:.2f}')")
    echo "$W $RC $CID" >> "$TIMINGS"
    if [[ "$RC" != 0 ]]; then
      FAILED=$((FAILED + 1))
      echo "needle-rig-build: INGEST FAILED rc=$RC $CID (see $OUT/ingest-$CID.log)" >&2
    fi
    printf 'needle-rig-build: [%3d/%3d] %-38s %6ss rc=%s\n' "$IDX" "$COUNT" "$CID" "$W" "$RC"
  done

  # §18.3 — absence is reported, never defaulted. A rig with missing corpora
  # would silently lower every hit rate downstream and read as a selection
  # defect. Refuse instead.
  if (( FAILED > 0 )); then
    echo "needle-rig-build: $FAILED/$COUNT ingests failed — the rig is INCOMPLETE, refusing to assemble" >&2
    exit 1
  fi
  python3 - "$TIMINGS" "$COUNT" "$TOTAL" <<'PY'
import statistics, sys
rows = [l.split() for l in open(sys.argv[1]) if l.strip()]
w = sorted(float(r[0]) for r in rows)
k, total = int(sys.argv[2]), int(sys.argv[3])
med = statistics.median(w)
print()
print(f"NEEDLE_RIG_INGEST corpora={len(w)} total_s={sum(w):.1f} "
      f"median_s={med:.2f} min_s={w[0]:.2f} max_s={w[-1]:.2f}")
# Bracket, not a point: the honest extrapolation spans the observed min and
# max per-corpus cost, and the median is the planning number (§bounds over
# point measurements).
for target in (100, 1000):
    print(f"NEEDLE_RIG_INGEST extrapolate k={target}: "
          f"[{w[0]*target/60:.1f}, {w[-1]*target/60:.1f}] min "
          f"(median {med*target/60:.1f} min)")
PY
else
  echo "needle-rig-build: --skip-ingest — reusing whatever is already in $NEEDLE_INDEXES"
fi

# ── 3. Verify the ingested corpora exist before assembling ──────────────────
MISSING=0
while read -r CID; do
  [[ -d "$NEEDLE_INDEXES/$CID/chunks.lance" ]] || { echo "needle-rig-build: missing index for $CID" >&2; MISSING=$((MISSING+1)); }
done < <(python3 -c "
import json,sys
m=json.load(open(sys.argv[1]))
[print(n['corpus_id']) for n in m['needles']]
" "$SPEC/manifest.json")
if (( MISSING > 0 )); then
  echo "needle-rig-build: $MISSING needle index(es) absent — refusing to assemble" >&2
  exit 1
fi

# ── 4. Assemble the hybrid rig ───────────────────────────────────────────────
# Needles are SYMLINKED (they are the artefact worth keeping and re-linking
# costs nothing); stubs are CLONED because each needs its own stamped
# corpus_id — the probe-b recipe. `probe-t1-expansion-fanout.sh` already
# counts both dirs and symlinks-to-dirs, so a mixed farm is a shape it
# understands.
echo
echo "needle-rig-build: assembling hybrid rig at $RIG"
find "$RIG" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
while read -r CID; do
  ln -s "$NEEDLE_INDEXES/$CID" "$RIG/$CID"
done < <(python3 -c "
import json,sys
m=json.load(open(sys.argv[1]))
[print(n['corpus_id']) for n in m['needles']]
" "$SPEC/manifest.json")

STUBS=$((TOTAL - COUNT))
for ((i = 0; i < STUBS; i++)); do
  cp -r "$STUB_SOURCE" "$RIG/stub-$(printf '%04d' "$i")"
done
if (( STUBS > 0 )); then
  python3 - "$RIG" <<'PY'
import json, os, sys
root = sys.argv[1]
for name in sorted(os.listdir(root)):
    if not name.startswith("stub-"):
        continue           # needles keep the corpus_id their real ingest wrote
    meta = os.path.join(root, name, "_corpus_meta.json")
    if not os.path.exists(meta):
        continue
    m = json.load(open(meta))
    m["corpus_id"] = name
    json.dump(m, open(meta, "w"))
PY
fi

N_DIRS="$(find "$RIG" -maxdepth 1 -mindepth 1 \( -type d -o -type l \) | wc -l)"
# `du -sh` on the farm follows only the real dirs (the needles are symlinks,
# which du does not traverse), so this is the STUB bulk plus the farm itself.
echo "needle-rig-build: rig has $N_DIRS corpora ($COUNT needles + $STUBS stubs); stub bulk on-disk $(du -sh "$RIG" 2>/dev/null | cut -f1)"
if [[ "$N_DIRS" != "$TOTAL" ]]; then
  echo "needle-rig-build: expected $TOTAL corpora, found $N_DIRS — refusing to report a rig that is not the size asked for" >&2
  exit 1
fi
echo "needle-rig-build: rig      $RIG"
echo "needle-rig-build: bank     $SPEC/bank.toml"
echo "needle-rig-build: manifest $SPEC/manifest.json"
echo "needle-rig-build: next     scripts/needle-rig-baseline.sh --rig-root $OUT"
