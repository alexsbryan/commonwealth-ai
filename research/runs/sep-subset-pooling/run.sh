#!/usr/bin/env bash
# sep-subset-pooling — does the LAST-pooled stack actually retrieve better than
# the MEAN-pooled corpora it inherited? The decision input for the operator's
# re-embed + re-publish call on sep and wikipedia.
#
# TWO ARMS, one variable.
#   M = raptor-subset-off as it stands (mean-pooled vectors, copied from sep)
#   L = a NEW corpus id, the same 33,884 chunk rows with the same ids, titles
#       and content, and ONLY the chunks.lance vector column re-embedded
#       through the daemon's document path (current stack = last-pooled).
# The atlas and seed tables are carried over unchanged — they are already
# last-pooled (sep-al-farabi's seeds read 0.9610 at last, note 500f1229).
#
# NOTHING IS OVERWRITTEN. M stays as it is; L is a new id; the installed `sep`,
# every `sep-<slug>` and `wikipedia` are untouched and never opened for write.
#
# Env the CALLER sets: none required.
# Optional: ARM_L (default raptor-subset-pooled-last), EMBED_BATCH (128),
#           BANK_LIMIT (30), TRIALS (2), ALLOW_BUSY_BOX=1
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="$here/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$out"

ARM_M="${ARM_M:-raptor-subset-off}"
ARM_L="${ARM_L:-raptor-subset-pooled-last}"
BANK_LIMIT="${BANK_LIMIT:-30}"
TRIALS="${TRIALS:-2}"
INDEXES_ROOT="${INDEXES_ROOT:-$HOME/.svrnmesh/indexes}"
export INDEXES_ROOT

mark() { printf '%s rc=%s %s\n' "$1" "$2" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$out/markers.txt"; }
box()  { { date -u +%Y-%m-%dT%H:%M:%SZ; free -g | sed -n 2p; df -h /home | tail -1; } > "$out/box-$1.txt"; }
die()  { mark "$1" 1; echo "DONE rc=1" >> "$out/markers.txt"; box after 2>/dev/null || true; exit 1; }

# The terminal marker on EVERY exit path including a kill — without it "killed"
# is indistinguishable from "still running" (the gap that made ei-6's first run
# unreadable).
on_signal() {
  printf 'DONE rc=%s KILLED-BY=%s %s\n' "$2" "$1" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$out/markers.txt"
  box after 2>/dev/null || true
  exit "$2"
}
trap 'on_signal SIGTERM 143' TERM
trap 'on_signal SIGINT 130'  INT
trap 'on_signal SIGHUP 129'  HUP

# ── preflight, ALL of it before the 33-minute step ─────────────────────────
command -v svrn >/dev/null && SVRN=svrn || SVRN=sovereign
command -v "$SVRN" >/dev/null || die preflight-cli
python3 -c "import lance, pyarrow" 2>/dev/null || die preflight-python-lance
[[ -d "$INDEXES_ROOT/$ARM_M" ]] || die preflight-arm-m-missing
# REUSE_ARM_L: arm L is already built and installed, so run the BANK ONLY.
# One script, two invocations — the second limit is a second MEASUREMENT of the
# same two arms, not a second experiment, and forking the script would make the
# arms two things that merely look alike (ARCH §10.6).
if [[ -n "${REUSE_ARM_L:-}" ]]; then
  [[ -d "$INDEXES_ROOT/$ARM_L" ]] || die preflight-arm-l-missing-for-reuse
else
  [[ -e "$INDEXES_ROOT/$ARM_L" ]] && die preflight-arm-l-exists
fi
BANK="$repo/sovereign/bench/sep/questions.toml"
[[ -f "$BANK" ]] || die preflight-bank
# The daemon must answer an embed BEFORE we commit to the long leg.
curl -sf -m 120 -X POST http://127.0.0.1:9741/v1/embeddings \
  -H 'Content-Type: application/json' \
  -d '{"input":"preflight","model":"embed:default"}' \
  | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if len(d['data'][0]['embedding'])==1024 else 1)" \
  || die preflight-daemon-embed
mark preflight 0

box before
avail=$(awk '/MemAvailable/{print int($2/1048576)}' /proc/meminfo)
disk=$(df --output=avail -BG /home | tail -1 | tr -dc '0-9')
if [[ -z "${ALLOW_BUSY_BOX:-}" ]] && { (( avail < 20 )) || (( disk < 120 )); }; then
  echo "run.sh: REFUSED — MemAvailable=${avail}G (want >=20), disk=${disk}G (want >=120)" >&2
  die box-before
fi
mark box-before 0

# ── the two bank copies: same bank, `corpus=` rewritten per arm (ei-7a's pattern)
for arm in "$ARM_M" "$ARM_L"; do
  sed "s/^corpus = \"sep\"/corpus = \"$arm\"/" "$BANK" > "$out/bank-$arm.toml"
  grep -q "^corpus = \"$arm\"" "$out/bank-$arm.toml" || die "bank-rewrite-$arm"
done
mark bank-copies 0

# ── LEG 1: build arm L (the long one, ~33 min at 17 chunk/s) ───────────────
if [[ -n "${REUSE_ARM_L:-}" ]]; then
  echo "run.sh: reusing the installed $ARM_L — bank only, no re-embed" | tee "$out/build.log"
  mark build-arm-l-reused 0
else
  t0=$(date +%s)
  EMBED_BATCH="${EMBED_BATCH:-128}" python3 "$here/build_last_pooled.py" "$ARM_M" "$ARM_L" \
    > "$out/build.log" 2>&1
  rc=$?; mark build-arm-l "$rc"
  printf 'build wall=%ss\n' "$(( $(date +%s) - t0 ))" >> "$out/walls.txt"
  [[ $rc -eq 0 ]] || { tail -20 "$out/build.log" >&2; die build-arm-l; }
fi

# ── LEG 2: validate the instrument BEFORE any bench number ────────────────
python3 "$here/selfcheck.py" "$ARM_L" "$ARM_M" > "$out/selfcheck.txt" 2>&1
rc=$?; cat "$out/selfcheck.txt"; mark selfcheck "$rc"
[[ $rc -eq 0 ]] || die selfcheck

# ── LEG 3: the bank, n=TRIALS per arm, both arms the same day ─────────────
# retrieval-prod: the HARD lane, facts + sources recall, exact band.
for trial in $(seq 1 "$TRIALS"); do
  for arm in "$ARM_M" "$ARM_L"; do
    log="$out/bank-$arm-t$trial.txt"
    echo "=== $arm trial $trial — --prod-pipeline --isolate --limit $BANK_LIMIT ===" | tee "$log"
    "$SVRN" eval run --bank "$out/bank-$arm.toml" \
      --prod-pipeline --isolate --limit "$BANK_LIMIT" \
      --format json --output "$out/bank-$arm-t$trial.json" >> "$log" 2>&1
    mark "bank-$arm-t$trial" "$?"
  done
done

# ── the table, and the PRE-REGISTERED verdict (manifest.md, written first) ──
python3 "$here/score.py" "$out" "$ARM_M" "$ARM_L" "$TRIALS" > "$out/TABLE.txt" 2>&1
rc=$?; cat "$out/TABLE.txt"; mark score "$rc"
grep -E '^VERDICT-' "$out/TABLE.txt" | while read -r v; do mark "$v" 0; done

box after
echo "DONE rc=0" >> "$out/markers.txt"
echo "artifacts: $out"
