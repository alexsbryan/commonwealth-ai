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

# A FAILED RUN'S ARTIFACTS MUST NOT SURVIVE INTO THE NEXT ONE.
# Every leg writes to a fixed filename, so before this the 11:17 failure's
# `atoms-run2.json` sat beside the 11:39 run's fresh `atoms-run1.json`, and
# nothing in the file told them apart. Reading the directory as a set while a
# run was still in flight reported INSTRUMENT_FAIL on arms that had simply not
# been reached yet — correctly, for the stale data it was actually reading.
# That is a plausible, well-formed, WRONG verdict from a green process, which
# is the failure this whole lane exists to avoid making.
#
# The previous run is MOVED, not deleted — a single overwritten slot, so the
# evidence survives one generation without unbounded growth.
rm -rf "$OUT.prev"
[ -d "$OUT" ] && mv "$OUT" "$OUT.prev"
mkdir -p "$OUT"
# The launch stamp every artifact is checked against. An artifact older than
# this file did not come from this run.
date -u +%s > "$OUT/RUN_ID"
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
  # THE COPY BRINGS THE PARENT'S IDENTITY WITH IT, AND THAT IS FATAL AND SILENT.
  # `_corpus_meta.json` carries `corpus_id`, so a straight `cp -r` of `sep`
  # yields a directory named `ei7b-legacy` that ADVERTISES itself as `sep`.
  # `installed_indexes()` dedups on the advertised id, keeps the real `sep`, and
  # DROPS the fixture: invisible to search, no error, exit 0, every question
  # retrieving 0 chunks in BOTH arms. That is exactly what the 11:17 run did.
  # ei-7a's `build_subset.py:210` does this rewrite; the reflink shortcut here
  # skipped the one line that mattered.
  python3 - "$INDEXES/$id/_corpus_meta.json" "$id" <<'META'
import json, sys
p, cid = sys.argv[1], sys.argv[2]
m = json.load(open(p))
m["corpus_id"] = cid
if isinstance(m.get("corpus_name"), str):
    m["corpus_name"] = f"SEP field-guide fixture ({cid}, ei-7b)"
json.dump(m, open(p, "w"), indent=2)
META
done
# Asserted, not assumed. A fixture still advertising `sep` produces a run that
# cannot mean anything, and it has to stop HERE rather than at a plausible zero
# forty minutes later.
for id in ei7b-legacy ei7b-fieldguide; do
  adv=$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))['corpus_id'])" "$INDEXES/$id/_corpus_meta.json")
  [ "$adv" = "$id" ] || { echo "RC_FIXTURE=94 ($id advertises corpus_id '$adv' — it would be deduped away)"; exit 94; }
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
  # Freshness, asserted per arm: a log older than the launch stamp is a
  # leftover, and reading one is how a stale failure becomes this run's verdict.
  if [ "${tag}.log" -ot "$OUT/RUN_ID" ]; then
    echo "INSTRUMENT_FAIL_${arm}_${ix}: ${tag}.log predates this run's RUN_ID — STALE, not a result"
  fi
  python3 - "${tag}.log" "${arm}" "${ix}" <<'PYEOF'
import re, sys
# STRIP ANSI FIRST. `tracing` writes escapes BETWEEN the field name and the
# `=`, so a naive `field=[0-9]+` grep matches nothing and returns a clean,
# plausible ZERO (ei-7a paid for this one).
ansi = re.compile(r"\x1b\[[0-9;]*m")
txt = ansi.sub("", open(sys.argv[1], errors="replace").read())
arm, ix = sys.argv[2], sys.argv[3]

# THE CORPUS MUST ACTUALLY HAVE BEEN SEARCHED. This is the check that would
# have caught the 11:17 run, where both fixtures advertised `corpus_id = sep`,
# were deduped away by `installed_indexes()`, and every question retrieved
# nothing while the lane still exited 0.
searched = [int(m) for m in re.findall(r"\bcorpora_searched=(\d+)", txt)]
finals = [int(m) for m in re.findall(r"\bfinal_chunks=(\d+)", txt)]
collided = "corpus_id collision" in txt
print(f"YIELD_{arm}_{ix}: merged_events={len(searched)} "
      f"corpora_searched_max={max(searched or [0])} final_chunks_total={sum(finals)}")
if collided:
    print(f"INSTRUMENT_FAIL_{arm}_{ix}: 'corpus_id collision' in the log — a fixture was "
          f"deduped away and this arm searched nothing. COULD-NOT-JUDGE.")
if max(searched or [0]) == 0 or sum(finals) == 0:
    print(f"INSTRUMENT_FAIL_{arm}_{ix}: the target corpus was never searched "
          f"(corpora_searched_max=0 or final_chunks_total=0). COULD-NOT-JUDGE, not a regression.")

# THE DIGEST IS NOT OBSERVABLE FROM THIS MODE, and saying so beats printing a
# zero. `--prod-pipeline` drives `Runtime::retrieve_evidence` — context build,
# `kq_pipeline()`, merge, truncate — and does NO synthesis, so it never
# assembles a system message. `splice_ambient_field_digests` has exactly two
# callers, `turn.rs:593` and `streaming.rs:4440`, and this mode enters neither.
# The digest's evidence is leg 1 (the two renderings, byte-identical) and the
# unit tests; it was never going to be this counter. Kept as a STATEMENT so the
# next person does not design the same instrument again.
lines = [l for l in txt.splitlines() if "ambient field_model" in l]
print(f"DIGEST_{arm}_{ix}: ambient_field_model_lines={len(lines)} "
      f"(EXPECTED 0 — --prod-pipeline does no synthesis and never reaches the step)")
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
