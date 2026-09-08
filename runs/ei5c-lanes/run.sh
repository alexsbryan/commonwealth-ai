#!/usr/bin/env bash
# ei-5c-seed-race — the four lanes of the order's item 5, as one unit.
#
# Sequential on purpose: (a) and (b) want the embed slot, (c) wants the 35B,
# and one GPU lane at a time is the box rule. Every leg writes its own rc
# marker; the DONE marker is written on SIGTERM too, so a killed run is still a
# verdict.
#
# BINARY. Every leg runs THIS worktree's `target/debug/sovereign-cli-llm` and
# `target/debug/corpus-mcp`, built by runs/ei5c-gates. A leg that ran main's
# binary would measure main, so leg 0 refuses rather than assuming.
set -uo pipefail
WT=/home/alexbryan/dev/ei5c-wt
OUT="$WT/runs/ei5c-lanes/out"
CLI="$WT/target/debug/sovereign-cli-llm"
mkdir -p "$OUT"
rm -f "$OUT"/*.rc "$OUT"/DONE
cd "$WT" || exit 1

finish() { echo "$(date -Is)" > "$OUT/DONE"; }
trap finish EXIT TERM INT

box() { { echo "== box $1 $(date -Is)"; free -g | sed -n '1,2p'; df -h /home | tail -1;
          pgrep -af 'cargo|rustc' | grep -v pgrep | head -3; } > "$OUT/box-$1.txt" 2>&1; }

leg() { local name="$1"; shift
  echo "== $name start $(date -Is)" >> "$OUT/log.txt"
  ( "$@" ) > "$OUT/$name.txt" 2>&1; local rc=$?
  echo "$rc" > "$OUT/$name.rc"
  echo "== $name rc=$rc $(date -Is)" >> "$OUT/log.txt"; return 0; }

box before

# ── PREFLIGHT, before any long step ────────────────────────────────────────
# Four things that make every number below meaningless if absent. Each is a
# named line, never an assumption (ARCH §18.3).
{ echo "tip:        $(git -C "$WT" rev-parse HEAD)"
  echo "branch:     $(git -C "$WT" branch --show-current)"
  echo "dirty:      $(git -C "$WT" status --porcelain | wc -l) path(s)"
  for b in "$CLI" "$WT/target/debug/corpus-mcp"; do
    if [[ -x "$b" ]]; then echo "binary OK:  $b  ($(stat -c %y "$b" | cut -c1-19))"
    else echo "binary MISSING: $b — run runs/ei5c-gates first"; fi
  done
  echo "daemon:     $(curl -s -m 5 -o /dev/null -w '%{http_code}' http://127.0.0.1:9741/v1/models)"
  echo "-- the fixture's byte-identity, read off disk before it is measured:"
  for c in raptor-subset-off raptor-subset-on brothers-karamazov-book-1; do
    d="$HOME/.svrnmesh/indexes/$c/atlas"
    echo "   $c: population=[$(head -2 "$d/atoms_ann.population" 2>/dev/null | tr '\n' ' ')] \
derivation=[$(head -1 "$d/edges.csr.derivation" 2>/dev/null || echo ABSENT)] \
embedded=$(python3 -c "import json;print(json.load(open('$d/_summary.json'))['ann']['embedded_atoms'])" 2>/dev/null)"
  done
} > "$OUT/preflight.txt" 2>&1
[[ -x "$CLI" ]] || { echo "ABORT: no worktree binary" >> "$OUT/log.txt"; echo 90 > "$OUT/preflight.rc"; exit 90; }
echo 0 > "$OUT/preflight.rc"

# ── leg 0: rebuild brothers-karamazov-book-1's DERIVED set ─────────────────
# Seat-authorised, reversible, and BOTH halves are needed:
#
#   - the SEED TABLE, because its marker reads schema 1 (the pre-ei-7a
#     derivation), so `ann_table_is_fresh` is already false and the daemon
#     would rebuild it silently at next boot anyway; and
#   - the STORE, because the `Configures` edges are derived at the store write
#     and this corpus has none. A seed table alone would put the walk on a
#     Configuration it cannot leave — seeds on a terminus, which is half of
#     what bar (d) is asking about.
#
# `migrate-all <corpus>` does both through the one writer. The whole derived
# set is copied aside first: `atoms.json`, `edges.json` and every evidence path
# are untouched, so one `mv` restores the pre-ei-5c state and nothing this leg
# writes is content.
reseed() {
  local d="$HOME/.svrnmesh/indexes/brothers-karamazov-book-1/atlas"
  local bak="$d/derived.pre-ei5c.$(date +%Y%m%d)"
  echo "-- BEFORE"
  echo "   population: [$(tr '\n' ' ' < "$d/atoms_ann.population" 2>/dev/null)]"
  echo "   derivation: [$(tr '\n' ' ' < "$d/edges.csr.derivation" 2>/dev/null || echo ABSENT)]"
  python3 -c "import json;print('   embedded_atoms =', json.load(open('$d/_summary.json'))['ann']['embedded_atoms'])"
  python3 - "$d/edges.json" <<'PYEOF'
import json, sys
from collections import Counter
try:
    e = json.load(open(sys.argv[1]))["edges"]
    print("   edges.json:", len(e), Counter(x["edge_type"] for x in e).most_common())
except Exception as ex:
    print("   edges.json:", ex)
PYEOF
  mkdir -p "$bak" || return 1
  for f in atoms.lance edges.csr atoms_ann.lance atoms_ann.population edges.csr.derivation; do
    [[ -e "$d/$f" ]] && { cp -a "$d/$f" "$bak/" || return 1; }
  done
  echo "-- backup at $bak (restore: mv \"$bak\"/* \"$d\"/)"
  # `--no-flip`: `.read_v2` is already set on this corpus and flipping is not
  # this leg's business.
  "$CLI" atlas migrate-all --no-flip brothers-karamazov-book-1 || return 1
  echo "-- AFTER"
  echo "   population: [$(tr '\n' ' ' < "$d/atoms_ann.population" 2>/dev/null)]"
  echo "   derivation: [$(tr '\n' ' ' < "$d/edges.csr.derivation" 2>/dev/null || echo ABSENT)]"
  python3 -c "import json;print('   embedded_atoms =', json.load(open('$d/_summary.json'))['ann']['embedded_atoms'])"
  # The Configures edges are DERIVED into edges.csr and never into edges.json,
  # so the count that matters is the CSR's. Read it out of the header rather
  # than inferred from the atoms.
  python3 - "$d/edges.csr" <<'PYEOF'
import struct, sys
try:
    b = open(sys.argv[1], "rb").read(16)
    magic, ver, n_atoms, n_edges = struct.unpack("<IIII", b)
    print(f"   edges.csr: version={ver} atoms={n_atoms} edges={n_edges}")
except Exception as ex:
    print("   edges.csr:", ex)
PYEOF
}
leg 0-reseed-bk reseed

# ── leg a: EI2 — bare sep, walk sole, injector gone ────────────────────────
# `--limit 30` is the BASELINE's limit, never the default 10 (note b0cf07e8:
# a limit-10 run against a limit-30 baseline read a 21-fact regression that
# did not exist). `--prod-pipeline` is the only mode that reaches
# `apply_atlas_grounding`; the run announces its mode and leg-a greps it back.
sep_run() { local ix=$1 tag="$OUT/a-sep-run$ix"
  RUST_LOG="warn,retrieval_audit=debug" \
  "$CLI" eval run --bank sovereign/bench/sep/questions.toml \
    --prod-pipeline --isolate --limit 30 --format json --output "${tag}.json" \
    > "${tag}.log" 2>&1
  local rc=$?
  grep -q "prod-pipeline mode" "${tag}.log" || echo "INSTRUMENT_FAIL_a_$ix: not prod-pipeline mode"
  grep -q "limit.*30\|\"limit\": *30" "${tag}.json" || echo "NOTE_a_$ix: limit not echoed in artifact — check by hand"
  python3 "$WT/runs/ei5c-lanes/score.py" "${tag}.json" "a-sep-run$ix"
  python3 "$WT/runs/ei5c-lanes/yield.py" "${tag}.log" "a-sep-run$ix"
  return $rc
}
leg a-sep-run1 sep_run 1
leg a-sep-run2 sep_run 2

# ── leg b: the ei-7a displacement fixture, both arms, both directions ──────
# NO REBUILD: the per-kind budget is a WALK-time policy and changes no seed
# table, and both corpora are intact (checked in preflight — 23,120 vs 21,116
# embedded, a difference of exactly the 2,004 Summary atoms). So these arms are
# byte-identical to the ones ei-7a measured at OFF 47/66 / ON 40,39/66, and the
# only thing that moved between then and now is the walk.
subset_run() { local arm=$1 ix=$2 corpus=$3 tag="$OUT/b-${arm}-run${ix}"
  local bank="$OUT/questions-$corpus.toml"
  # DERIVED from the control bank with only `corpus` swapped — a checked-in
  # copy would be a second decider for the question set (ei-7a's rule, kept).
  sed "s/^corpus = \"sep\"/corpus = \"$corpus\"/" sovereign/bench/sep/questions.toml > "$bank"
  grep -q "^corpus = \"$corpus\"" "$bank" || { echo "FAIL: bank corpus not swapped"; return 1; }
  RUST_LOG="warn,retrieval_audit=debug" \
  "$CLI" eval run --bank "$bank" --prod-pipeline --isolate --limit 30 \
    --format json --output "${tag}.json" > "${tag}.log" 2>&1
  local rc=$?
  grep -q "prod-pipeline mode" "${tag}.log" || echo "INSTRUMENT_FAIL_b_${arm}_$ix: not prod-pipeline mode"
  python3 "$WT/runs/ei5c-lanes/score.py" "${tag}.json" "b-${arm}-run$ix"
  python3 "$WT/runs/ei5c-lanes/yield.py" "${tag}.log" "b-${arm}-run$ix"
  return $rc
}
leg b-off-run1 subset_run off 1 raptor-subset-off
leg b-on-run1  subset_run on  1 raptor-subset-on
leg b-on-run2  subset_run on  2 raptor-subset-on
leg b-off-run2 subset_run off 2 raptor-subset-off

# ── leg c: EI1 — the thematic synth lane, n=3, on the 35B ──────────────────
# Same flags as the floor (16f121a46: `--synth --isolate`, no --limit). n=3
# because the floor is n=2 and the bar is read against the synth band.
ei1_run() { local ix=$1 tag="$OUT/c-ei1-run$ix"
  RUST_LOG="warn,retrieval_audit=debug" \
  "$CLI" eval run --bank sovereign/bench/literary/thematic-bk-book-1.toml \
    --synth --isolate --format json --output "${tag}.json" > "${tag}.log" 2>&1
  local rc=$?
  grep -q "synth" "${tag}.log" || echo "INSTRUMENT_FAIL_c_$ix: mode line does not say synth"
  python3 "$WT/runs/ei5c-lanes/ei1.py" "${tag}.json" "c-ei1-run$ix"
  python3 "$WT/runs/ei5c-lanes/yield.py" "${tag}.log" "c-ei1-run$ix"
  return $rc
}
leg c-ei1-run1 ei1_run 1
leg c-ei1-run2 ei1_run 2
leg c-ei1-run3 ei1_run 3

# ── leg d: the acceptance theme bar ────────────────────────────────────────
# Runs the whole script, because its own preamble is what decides whether the
# bar is judgeable and the mechanism assertions above it are the context that
# makes a bar row readable. Its llama-server is the embed gguf only.
leg d-acceptance ./corpus-mcp/acceptance.sh

box after
