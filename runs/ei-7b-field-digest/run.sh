#!/usr/bin/env bash
# ei-7b lane: the ambient "Field guide" digest, rendered from the atlas.
#
# WHAT THIS MEASURES, AND WHAT IT CANNOT.
# `context.knowledge_view_digests` has exactly ONE reader —
# `sovereign-core/src/runtime/system_message.rs:233`, prompt assembly. Nothing
# in retrieval reads it. So this change moves the PROMPT and structurally
# cannot move retrieval's source recall. The `retrieval-prod` rows below are
# therefore a does-not-regress check on a path the change cannot touch, and
# they are reported as such rather than as evidence the digest is good. The
# evidence that the digest is good is leg 1: the two digest texts, side by
# side, rendered by the SAME `render_landscape` from the two sources.
#
# THE FIXTURE, and why its id does not start with `sep-`.
# `sep` and every `sep-<slug>` atlas is a declared CONTROL this campaign may
# not overwrite, and the atoms arm has to WRITE into the atlas the digest
# reads. `EvidenceSite::derive`'s table is `[("sep-", "sep")]`, so a fixture
# called `sep-fieldguide` would declare its parent to be the control corpus
# and fetch its evidence out of it (ei-7a paid for this lesson;
# `runs/sep-raptor-subset/build_subset.py` records it). `ei7b-fieldguide`
# derives self-hosted, which is what it is.
#
# NOTHING IS RE-EMBEDDED AND NOTHING IS RE-INDEXED. The fixture is a
# filesystem reflink copy of the whole `sep` index, so it carries the SAME
# chunks, the same vectors and — unlike a Lance `write_dataset` copy — the
# same three indices. A delta here cannot be an embedding or an index
# difference.
#
# THE ARMS are the two SOURCES `field_atoms::load_field_model` can serve from,
# on byte-identical corpora:
#   ARM legacy : corpus `ei7b-legacy`     — empty atlas, v1 `field_skeleton.json`
#                kept; the un-migrated shape `sep` is in TODAY
#   ARM atoms  : corpus `ei7b-fieldguide` — same bytes, v1 file REMOVED,
#                `enrich field-atoms` run; the migrated shape
#
# So the comparison is "the same digest from the two sources", which is the
# port's actual question. A third "no field model at all" arm was dropped: it
# measures nothing this change decides, and it would cost two more model runs.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

OUT=runs/ei-7b-field-digest/out
mkdir -p "$OUT"
CLI=./target/debug/sovereign-cli-llm
INDEXES="${SOVEREIGN_INDEXES:-$HOME/.svrnmesh/indexes}"
SRC="$INDEXES/sep"

box () { # label -> one line of box state, before and after (§18.5)
  printf 'BOX_%s: gtt_used=%s mem_avail_gb=%s df_home=%s\n' "$1" \
    "$(cat /sys/class/drm/card1/device/mem_info_gtt_used 2>/dev/null || echo NA)" \
    "$(awk '/MemAvailable/{printf "%.0f", $2/1048576}' /proc/meminfo)" \
    "$(df -h --output=avail /home | tail -1 | tr -d ' ')"
}
box before

# ── Refuse a busy box, with an explicit override ────────────────────────────
#
# MATCH THE BINARY, NOT THE STRING "cargo". `pgrep -af 'cargo|rustc'` matches
# on the full command line, and `~/.cargo/bin/lspmux` — the rust-analyzer proxy
# the daemon keeps running PERMANENTLY — contains `.cargo/` in its path. So the
# obvious pattern refuses on a completely idle box, every time, and the seat's
# launch window is spent on an exit 3 that looks like a real busy-box refusal.
# Verified on a quiet box 2026-09-05: the loose pattern matched 1 process
# (lspmux), the exact-name form matched 0.
builds_running () { pgrep -x rustc >/dev/null 2>&1 || pgrep -x cargo >/dev/null 2>&1; }
if [ "${EI7B_FORCE:-0}" != "1" ]; then
  if builds_running; then
    # Name what was seen — a refusal that does not say what it refused on
    # cannot be told from a broken check (ARCH §18.3).
    echo "REFUSED: a build is running; set EI7B_FORCE=1 to override"
    pgrep -ax rustc; pgrep -ax cargo
    exit 3
  fi
  avail=$(awk '/MemAvailable/{printf "%.0f", $2/1048576}' /proc/meminfo)
  if [ "$avail" -lt 40 ]; then
    echo "REFUSED: MemAvailable ${avail}G < 40G; set EI7B_FORCE=1 to override"; exit 3
  fi
fi

[ -d "$SRC" ] || { echo "RC_FIXTURE=90 (no $SRC)"; exit 90; }
[ -x "$CLI" ] || { echo "RC_FIXTURE=91 (no $CLI — build sovereign-cli-llm)"; exit 91; }

# ── Leg 0: the fixtures ─────────────────────────────────────────────────────
# Reflink, so two 1.1 GB copies cost ~0 bytes until they diverge. `cp` is used
# rather than a Lance copy precisely because it brings the indices along.
for id in ei7b-legacy ei7b-fieldguide; do
  if [ ! -d "$INDEXES/$id" ]; then
    cp --reflink=auto -r "$SRC" "$INDEXES/$id" || { echo "RC_FIXTURE=92"; exit 92; }
    # Both start from the same bytes with an EMPTY atlas (which is what `sep`
    # has). They then diverge in exactly one thing.
    python3 -c "import json,sys; json.dump({'atoms':[],'schema_version':'2.0'},open(sys.argv[1],'w'))" \
      "$INDEXES/$id/atlas/atoms.json"
    rm -f "$INDEXES/$id/atlas/_summary.json"
  fi
done
# The atoms arm drops the v1 file, so a digest it serves CANNOT have come from
# the fallback. Without this the arms would not be separable and a green run
# would prove nothing about the atlas path.
rm -f "$INDEXES/ei7b-fieldguide/field_skeleton.json"
[ -f "$INDEXES/ei7b-legacy/field_skeleton.json" ] \
  || { echo "RC_FIXTURE=93 (legacy arm lost its v1 file)"; exit 93; }
echo "RC_FIXTURE=0"

# ── Leg 1: the digests, side by side (the actual evidence) ──────────────────
# `--into` writes into the fixture, never into `sep`. `--show-digest` prints
# BOTH: the one v1 renders from `field_skeleton.json` and the one the atoms
# render, through the same `render_landscape`.
"$CLI" enrich field-atoms sep --into ei7b-fieldguide --show-digest \
  > "$OUT/digest-before-after.txt" 2>&1
echo "RC_DIGEST=$?"
cat "$OUT/digest-before-after.txt"

# ── Leg 2: retrieval-prod, two runs per arm, both directions ────────────────
bank_for () { # corpus_id -> derived bank path
  local corpus=$1 out="$OUT/questions-$1.toml"
  # DERIVED from the control bank with only `corpus` swapped; a checked-in
  # copy would be a second decider for the question set (reused verbatim from
  # ei-7a's lane.sh).
  sed "s/^corpus = \"sep\"/corpus = \"$corpus\"/" sovereign/bench/sep/questions.toml > "$out"
  grep -q "^corpus = \"$corpus\"" "$out" || { echo "FAIL: bank corpus not swapped"; exit 1; }
  echo "$out"
}

run () { # arm run_ix corpus
  local arm=$1 ix=$2 corpus=$3
  local tag="$OUT/${arm}-run${ix}"
  local bank; bank=$(bank_for "$corpus")
  echo "=== arm=$arm run=$ix corpus=$corpus ==="
  # `retrieval_audit` is a CUSTOM tracing target: dark unless named in the
  # filter, however high the level. The ambient field_model step logs its
  # `field_digests` count there, and that count is the ONLY direct evidence
  # the digest fired for these turns.
  RUST_LOG="warn,retrieval_audit=debug" \
    "$CLI" eval run --bank "$bank" --prod-pipeline --isolate \
      --format json --output "${tag}.json" > "${tag}.log" 2>&1
  echo "RC_${arm}_${ix}=$?"
  # The instrument, checked before the result (§18.4): a run whose log does not
  # say `prod-pipeline mode` never reached the prod path and its number means
  # nothing here.
  grep -q "prod-pipeline mode" "${tag}.log" \
    || echo "INSTRUMENT_FAIL_${arm}_${ix}: not prod-pipeline mode"
  python3 - "${tag}.log" "${arm}" "${ix}" <<'PYEOF'
import re, sys
# STRIP ANSI FIRST. `tracing` writes escapes BETWEEN the field name and the
# `=`, so a naive `field_digests=[0-9]+` matches nothing and returns a clean,
# plausible ZERO — indistinguishable from "no digest was spliced", which is
# exactly what this leg has to tell apart (ei-7a paid for this one).
ansi = re.compile(r"\x1b\[[0-9;]*m")
txt = ansi.sub("", open(sys.argv[1], errors="replace").read())
lines = [l for l in txt.splitlines() if "ambient field_model" in l]
spliced = sum(int(m) for l in lines for m in re.findall(r"\bfield_digests=(\d+)", l))
skipped = sum(1 for l in lines if "census says no Question atoms" in l
                              or "no canonical questions" in l)
print(f"DIGEST_{sys.argv[2]}_{sys.argv[3]}: lines={len(lines)} "
      f"field_digests={spliced} skipped={skipped}")
if not lines:
    print(f"INSTRUMENT_NOTE_{sys.argv[2]}_{sys.argv[3]}: no 'ambient field_model' line at all "
          f"— the step did not run, or retrieval_audit was not in the filter")
PYEOF
  python3 - "$tag" <<'PYEOF'
import json, sys
try:
    d = json.load(open(sys.argv[1] + ".json"))
except Exception as e:
    print(f"SCORE_{sys.argv[1]}: unreadable ({e})"); raise SystemExit(0)
rs = d.get("results") or []
m = sum(len(r["source_score"].get("matched", [])) for r in rs)
e = sum(r["source_score"].get("total_expected", 0) for r in rs)
print(f"SCORE {sys.argv[1]}: sources {m}/{e} ({100*m/max(e,1):.1f}%) over {len(rs)} questions")
PYEOF
}

# Both directions, so an ordering effect cannot hide inside a per-arm spread.
run atoms  1 ei7b-fieldguide
run legacy 1 ei7b-legacy
run legacy 2 ei7b-legacy
run atoms  2 ei7b-fieldguide

box after
echo DONE
