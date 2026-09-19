#!/usr/bin/env bash
# score.sh — THE decider for quality/campaigns/build-latency.toml.
#
# Success is a per-file table (see the SUCCESS block in the toml). For every
# [[probe]] row (file, crate, test), after a one-statement edit inserted into
# the first function body of the file:
#
#   warm   edit → scoped lint (absorb) · edit → scoped lint (MEASURE)
#          edit → focused test (absorb) · edit → focused test (MEASURE) · restore
#   fresh  on target/fresh-probe, wiped first, then the three full gates from
#          cold; then per crate: edit → focused test (first run) · restore
#   build  settle · edit → cargo build --workspace · restore · settle
#
# Sums over files are the numbers; --compare prints before → after per file.
# Runs inside the sovereign-vulkan toolbox, through scripts/with-cargo-lock.sh.
#
#   score.sh              warm    → target/build-probe/score.json
#   score.sh --fresh      fresh   → score.fresh.json   (~40 min; cold build)
#   score.sh --build      build   → score.build.json
#   score.sh --aa         warm twice, prints the band (bl-noise)
#   score.sh --noop       no-op full check, three runs, max (bl-noop-floor)
#   score.sh --predicate  the toml predicate, exit 0 = true
#   score.sh --snapshot <dir>   copy score*.json to <dir> (a before/after point)
#   score.sh --compare <before-dir> <after-dir>
set -uo pipefail
cd "$(git rev-parse --show-toplevel)"
OUT="${BUILD_PROBE_OUT:-$PWD/target/build-probe}"; mkdir -p "$OUT/logs" "$OUT/timings"
LOCK=scripts/with-cargo-lock.sh
GATE="corpus-engine/treesitter,sovereign-cli/dev-tools,sovereign-cli/code-intel,sovereign-cli/awareness,sovereign-mesh/mesh-sim,sovereign-mesh/dst,sovereign-turn-client/bundled-backend"
BUILDF="corpus-engine/treesitter,sovereign-cli/dev-tools"
MODE="${1:-warm}"
N=$RANDOM
TOUCHED=""
restore() { [[ -n "$TOUCHED" ]] && git checkout -- "$TOUCHED" 2>/dev/null; TOUCHED=""; }
trap 'restore' INT TERM EXIT
edit() { TOUCHED="$1"; N=$((N+1)); python3 - "$1" "$N" <<'PY' || exit 3
import re, sys
f, n = sys.argv[1], int(sys.argv[2]); L = open(f).read().split("\n")
pat = re.compile(r"^\s*(pub(\([^)]*\))?\s+)?(async\s+)?(unsafe\s+)?fn\s+[a-z_0-9]+[^;]*\{\s*$")
for i, l in enumerate(L):
    if pat.match(l):
        L.insert(i + 1, re.match(r"\s*", l).group(0) + f"    let __score_probe_{n}: u32 = {n}; let _ = __score_probe_{n};"); break
else:
    sys.exit("score.sh: no function body in " + f + " — the probe row is wrong, fix the toml")
open(f, "w").write("\n".join(L))
PY
}
wall() { local s e; s=$(date +%s.%N); "$@" >"$OUT/logs/last.log" 2>&1; local rc=$?; e=$(date +%s.%N); python3 -c "print(round($e-$s,1))"; return $rc; }
settle() { $LOCK cargo check --workspace --all-targets --features "$GATE" >/dev/null 2>&1; $LOCK cargo build --workspace --features "$BUILDF" >/dev/null 2>&1; }
probes() { python3 - <<'PY'
import tomllib
for p in tomllib.load(open("quality/campaigns/build-latency.toml","rb"))["probe"]:
    print(p["file"], p["crate"], p["test"], ",".join(p.get("shapes", ["lint","test"])))
PY
}
sum_json() { # key rows...
  local key=$1; shift
  printf '%s\n' "$@" | python3 -c "
import json, sys
rows=[json.loads(l) for l in sys.stdin if l.strip()]
tot=round(sum((r.get('lint_s') or 0)+(r.get('test_s') or 0)+(r.get('fresh_s') or 0)+(r.get('build_s') or 0) for r in rows),1)
print(json.dumps({'$key':tot,'rows':rows,'failed':[r['file'] for r in rows if r.get('rc',0)]}))"
}

warm() {
  local rows=() f c t shapes lint test rc
  settle
  while read -r f c t shapes; do
    lint=null; test=null; rc=0
    if [[ "$shapes" == *lint* ]]; then
      edit "$f"; wall $LOCK ./scripts/sovereign-lint.sh --human >/dev/null
      edit "$f"; lint=$(wall $LOCK ./scripts/sovereign-lint.sh --human) || rc=1
    fi
    if [[ "$shapes" == *test* ]]; then
      edit "$f"; wall $LOCK ./scripts/sovereign-test.sh --human --package "$c" --filter "$t" >/dev/null
      edit "$f"; test=$(wall $LOCK ./scripts/sovereign-test.sh --human --package "$c" --filter "$t") || rc=1
    fi
    restore
    rows+=("{\"file\":\"$f\",\"crate\":\"$c\",\"lint_s\":$lint,\"test_s\":$test,\"rc\":$rc}")
    echo "  $f lint=${lint}s test=${test}s rc=$rc" >&2
  done < <(probes)
  sum_json warm_s "${rows[@]}"
}

fresh() {
  local rows=() f c t shapes s rc
  export CARGO_TARGET_DIR="$PWD/target/fresh-probe"
  rm -rf "$CARGO_TARGET_DIR"
  echo "  fresh: cold full gates into target/fresh-probe $(date +%T)" >&2
  local g1 g2 g3
  g1=$(wall $LOCK ./scripts/sovereign-lint.sh --human --full); g2=$(wall $LOCK ./scripts/sovereign-test.sh --human); g3=$(wall $LOCK cargo build --workspace --features "$BUILDF")
  echo "  fresh: full gates cold lint=${g1}s test=${g2}s build=${g3}s" >&2
  local seen=""
  while read -r f c t shapes; do
    [[ "$seen" == *"|$c|"* ]] && continue; seen="$seen|$c|"
    rc=0; edit "$f"; s=$(wall $LOCK ./scripts/sovereign-test.sh --human --package "$c" --filter "$t") || rc=1; restore
    rows+=("{\"file\":\"$f\",\"crate\":\"$c\",\"fresh_s\":$s,\"rc\":$rc}")
    echo "  $f fresh=${s}s rc=$rc" >&2
  done < <(probes)
  unset CARGO_TARGET_DIR
  sum_json fresh_s "${rows[@]}" | python3 -c "import json,sys;d=json.load(sys.stdin);d['cold_full_gates_s']={'lint_full':$g1,'test_full':$g2,'build_ws':$g3};print(json.dumps(d))"
}

build() {
  local rows=() f c t shapes b rc
  settle
  while read -r f c t shapes; do
    [[ "$shapes" == *lint* ]] || continue
    rc=0; edit "$f"; b=$(wall $LOCK cargo build --workspace --features "$BUILDF" --timings) || rc=1
    cp target/cargo-timings/cargo-timing.html "$OUT/timings/$(echo "$f" | tr / _).build.html" 2>/dev/null
    restore; settle
    rows+=("{\"file\":\"$f\",\"crate\":\"$c\",\"build_s\":$b,\"rc\":$rc}")
    echo "  $f build=${b}s rc=$rc" >&2
  done < <(probes)
  sum_json build_s "${rows[@]}"
}

case "$MODE" in
  warm)     W=$(warm); echo "$W" > "$OUT/score.json"; echo "warm_s=$(echo "$W" | python3 -c 'import json,sys;print(json.load(sys.stdin)["warm_s"])')  ($OUT/score.json)";;
  --fresh)  F=$(fresh); echo "$F" > "$OUT/score.fresh.json"; echo "fresh_s=$(echo "$F" | python3 -c 'import json,sys;print(json.load(sys.stdin)["fresh_s"])')  ($OUT/score.fresh.json)";;
  --build)  B=$(build); echo "$B" > "$OUT/score.build.json"; echo "build_s=$(echo "$B" | python3 -c 'import json,sys;print(json.load(sys.stdin)["build_s"])')  ($OUT/score.build.json)";;
  --noop)   settle; m=0; for i in 1 2 3; do w=$(wall $LOCK cargo check --workspace --all-targets --features "$GATE"); m=$(python3 -c "print(max($m,$w))"); done; echo "noop_check_max_s=$m";;
  --aa)     A=$(warm); B=$(warm); python3 - "$A" "$B" <<'PY' | tee "$OUT/score.aa.json"
import json, sys
a, b = json.loads(sys.argv[1]), json.loads(sys.argv[2]); pa, pb = a["warm_s"], b["warm_s"]
band = abs(pa - pb) / ((pa + pb) / 2)
per = max(abs((x.get("lint_s") or 0) + (x.get("test_s") or 0) - (y.get("lint_s") or 0) - (y.get("test_s") or 0)) for x, y in zip(a["rows"], b["rows"]))
print(json.dumps({"warm_1": pa, "warm_2": pb, "band": round(band, 4), "max_row_delta_s": round(per, 1), "verdict": "passed" if band <= 0.03 else "failed", "runs": [a, b]}))
PY
    ;;
  --predicate)
    settle; edit sovereign/crates/sovereign-mesh/src/daemon.rs; wall $LOCK cargo build --workspace --features "$BUILDF" --timings >/dev/null
    cp target/cargo-timings/cargo-timing.html "$OUT/timings/predicate.mesh.build.html"; restore
    edit sovereign/crates/sovereign-core/src/runtime/mod.rs; wall $LOCK cargo build --workspace --features "$BUILDF" --timings >/dev/null
    cp target/cargo-timings/cargo-timing.html "$OUT/timings/predicate.core.build.html"; restore; settle
    m=$(python3 scripts/build-probe/critpath.py "$OUT/timings/predicate.mesh.build.html" | sed -n '/critical path/,/workspace units/p' | grep -c 'sovereign-cli-llm \[')
    c=$(python3 scripts/build-probe/critpath.py "$OUT/timings/predicate.core.build.html" | sed -n '/critical path/,/workspace units/p' | grep -c 'sovereign-tools \[')
    python3 scripts/build-probe/scope-drift.py >/dev/null; d=$?
    echo "predicate: cli-llm-on-mesh-chain=$m tools-on-core-chain=$c scope-drift-nonzero=$d"; [[ $m -eq 0 && $c -eq 0 && $d -eq 0 ]];;
  --snapshot) d="${2:?dir}"; mkdir -p "$d"; cp "$OUT"/score*.json "$d"/ 2>/dev/null; git rev-parse --short HEAD > "$d/HEAD"; ls "$d";;
  --compare)
    python3 - "${2:?before-dir}" "${3:?after-dir}" <<'PY'
import json, os, sys
b, a = sys.argv[1], sys.argv[2]
def load(d, n):
    p = os.path.join(d, n)
    return json.load(open(p)) if os.path.exists(p) else None
cols = [("warm", "score.json", lambda r: (r.get("lint_s") or 0) + (r.get("test_s") or 0)),
        ("fresh", "score.fresh.json", lambda r: r.get("fresh_s") or 0),
        ("build", "score.build.json", lambda r: r.get("build_s") or 0)]
hb, ha = open(os.path.join(b, "HEAD")).read().strip(), open(os.path.join(a, "HEAD")).read().strip()
print(f"before {hb}  →  after {ha}   (seconds; × = before/after)\n")
print(f"{'file':64s} " + " ".join(f"{c:>20s}" for c, _, _ in cols))
files = {}
for c, n, f in cols:
    for tag, d in (("b", b), ("a", a)):
        j = load(d, n)
        if j:
            for r in j["rows"]:
                files.setdefault(r["file"], {})[(c, tag)] = f(r)
for fl in files:
    cells = []
    for c, _, _ in cols:
        x, y = files[fl].get((c, "b")), files[fl].get((c, "a"))
        cells.append(f"{x:6.1f} → {y:6.1f} {x / y if x and y else 0:4.1f}×" if x is not None and y is not None else " " * 20)
    print(f"{fl:64s} " + " ".join(cells))
print()
for c, n, f in cols:
    jb, ja = load(b, n), load(a, n)
    if jb and ja:
        sb, sa = sum(f(r) for r in jb["rows"]), sum(f(r) for r in ja["rows"])
        print(f"{c:6s} sum: {sb:7.1f} → {sa:7.1f}   speed-up {sb / sa if sa else 0:4.2f}×   ({(1 - sa / sb) * 100 if sb else 0:+.0f}% time)")
PY
    ;;
  *) echo "usage: score.sh [--fresh|--build|--aa|--noop|--predicate|--snapshot <dir>|--compare <before> <after>]" >&2; exit 2;;
esac
