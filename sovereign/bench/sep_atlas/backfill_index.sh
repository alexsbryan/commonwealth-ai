#!/usr/bin/env bash
# SEP (and any) atlas INDEX backfill — v2 store + `atoms_ann.lance`, with a
# per-corpus JSONL ledger and resume.
#
# ei-3-index made `atoms_ann.lance` a mandatory artifact of the atlas WRITE, so
# every atlas built from that commit forward carries one. This driver is for the
# atlases that already exist: on this host, 1,770 `sep-*` per-article atlases of
# which 22 had a seed table and 662 had a v2 store.
#
# It is a driver, not an engine. Both steps are existing verbs:
#   `svrn atlas migrate-all <id>`     -> atoms.lance + edges.csr  (no embedding)
#   `svrn atlas backfill-ann <ids...>` -> atoms_ann.lance         (embeds)
# The one writer stays `context_loader::backfill_ann`; nothing here re-derives
# a filter, a threshold or a table (ARCH 19, 10.6).
#
# Usage:
#   backfill_index.sh                          # every sep-* atlas on this host
#   backfill_index.sh brothers-karamazov-book-1 corpus-b
#   backfill_index.sh --prefix wiki- --batch 50
#   backfill_index.sh --dry-run                # print the worklist + the price
#
#   --prefix <s>    corpus-id prefix to sweep (default `sep-`). Ignored when
#                   corpus ids are given positionally.
#   --batch <n>     corpora per `backfill-ann` invocation (default 100). This is
#                   an AMORTISATION knob, not a parallelism one: each CLI start
#                   pays ~40 s of session bootstrap (wiki graph + meta-atlas),
#                   and the embed inside is strictly serial either way.
#   --limit <n>     stop after n corpora. Use for a smoke run.
#   --force         re-do corpora the ledger already records as done.
#   --dry-run       print the worklist and the projected cost, run nothing.
#
# SELF-THROTTLING, and why it is not a flag. `load_atlas_context` embeds one
# atom at a time, awaiting each call, so this job has no fan to widen or
# narrow. That matters here: the parallel embed fan is what OOM-killed the
# daemon on 2026-05-31, and this host killed it four more times on 2026-09-04.
# The only load knob that exists is "stop", and the ledger makes stopping free.
#
# RESUME. Every corpus lands one JSONL line the moment its batch reports. A
# re-run reads the ledger and skips any corpus whose last line is `built`,
# `had` or `no-seedable`; `failed` is retried. Kill it at any point.
set -uo pipefail

HERE=$(cd -- "$(dirname -- "$0")" && pwd)
REPO=$(cd -- "$HERE/../../.." && pwd)
# The per-user root through the SSOT, never a literal ~/.svrnmesh.
# shellcheck source=../../../scripts/lib/svrn-root.sh
. "$REPO/scripts/lib/svrn-root.sh"
SVRN_ROOT=$(svrn_root)

SCLI=${SCLI:-$REPO/target/debug/sovereign-cli}
LOG_DIR=${LOG_DIR:-$HERE/logs}
# The ledger is EVIDENCE and lives beside the driver, in git. The verbose run
# log is noise and lives in `logs/`, which `sovereign/.gitignore` excludes --
# putting the ledger there would have quietly un-committed the one artifact
# this job exists to produce.
LEDGER=${LEDGER:-$HERE/backfill-index.jsonl}

PREFIX="sep-"
BATCH=100
LIMIT=""
FORCE=0
DRY=0
declare -a EXPLICIT=()

while (( $# > 0 )); do
  case "$1" in
    --prefix)  PREFIX=$2; shift 2 ;;
    --batch)   BATCH=$2;  shift 2 ;;
    --limit)   LIMIT=$2;  shift 2 ;;
    --force)   FORCE=1;   shift 1 ;;
    --dry-run) DRY=1;     shift 1 ;;
    -h|--help) sed -n '2,50p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) echo "unknown flag: $1" >&2; exit 2 ;;
    *)  EXPLICIT+=("$1"); shift 1 ;;
  esac
done

[[ -x "$SCLI" ]] || { echo "no sovereign-cli at $SCLI — build it, or set SCLI" >&2; exit 2; }
mkdir -p "$LOG_DIR"
ts=$(date +%Y%m%d-%H%M%S)
RUN_LOG="$LOG_DIR/backfill-index-$ts.run.log"

# ── worklist ────────────────────────────────────────────────────────────────
# A corpus is in scope when it has `atlas/atoms.json`. Anything else is not an
# atlas and is not silently counted as done.
declare -a CANDIDATES=()
if (( ${#EXPLICIT[@]} > 0 )); then
  CANDIDATES=("${EXPLICIT[@]}")
else
  while IFS= read -r d; do
    CANDIDATES+=("$(basename "$(dirname "$(dirname "$d")")")")
  done < <(find "$SVRN_ROOT/indexes" -maxdepth 3 -path "*/${PREFIX}*/atlas/atoms.json" 2>/dev/null | sort)
fi

# Ledger states that mean "do not redo".
done_states=$(
  [[ -f "$LEDGER" ]] && python3 - "$LEDGER" <<'PY'
import json,sys
done=set()
for line in open(sys.argv[1]):
    line=line.strip()
    if not line: continue
    try: r=json.loads(line)
    except Exception: continue
    if r.get("state") in ("built","had","no-seedable"): done.add(r["corpus"])
    else: done.discard(r["corpus"])
print("\n".join(sorted(done)))
PY
)

declare -a WORK=() SKIPPED=()
for c in "${CANDIDATES[@]}"; do
  if (( ! FORCE )) && grep -qxF "$c" <<<"$done_states"; then SKIPPED+=("$c"); continue; fi
  WORK+=("$c")
  [[ -n "$LIMIT" && ${#WORK[@]} -ge $LIMIT ]] && break
done

# ── the price, before the run (a projection is not a result) ────────────────
# `tier2_count` in `_summary.json` is the same population the production
# grounding filter admits (extracted-depth entities), so it is the honest
# denominator for "how many embed calls is this".
atoms=$(python3 - "$SVRN_ROOT" "${WORK[@]}" <<'PY'
import json,os,sys
root=sys.argv[1]; tot=0
for c in sys.argv[2:]:
    p=os.path.join(root,"indexes",c,"atlas","_summary.json")
    try: tot+=json.load(open(p)).get("tier2_count",0)
    except Exception: pass
print(tot)
PY
)
need_store=0
for c in "${WORK[@]}"; do
  [[ -d "$SVRN_ROOT/indexes/$c/atlas/atoms.lance" ]] || need_store=$((need_store+1))
done
# 14.0 embeds/s, measured 2026-09-04 against the resident 1024-d slot (40 calls,
# 71.2 ms each). Re-measure rather than trusting this line on another host.
rate=${EMBED_RATE:-14.0}
echo "worklist: ${#WORK[@]} corpus(es) (${#SKIPPED[@]} already in the ledger)"
echo "price:    $atoms atoms to embed at ${rate}/s ≈ $(python3 -c "print(f'{$atoms/$rate/60:.0f}')") min, plus $need_store v2 store build(s)"
echo "ledger:   $LEDGER"
echo "run log:  $RUN_LOG"
(( DRY )) && { printf '%s\n' "${WORK[@]}"; exit 0; }
(( ${#WORK[@]} )) || { echo "nothing to do"; exit 0; }

t_start=$(date +%s)

ledger_line() { # corpus state store rows of seconds
  python3 - "$LEDGER" "$@" <<'PY'
import json,sys,time
path,corpus,state,store,rows,of,secs=sys.argv[1:8]
rec={"corpus":corpus,"state":state,"store":store,"at":int(time.time()),"seconds":float(secs)}
if rows!="-": rec["rows"]=int(rows); rec["of"]=int(of)
open(path,"a").write(json.dumps(rec)+"\n")
PY
}

# ── step 1: the v2 store, per corpus, no embedding ──────────────────────────
declare -A STORE=()
for c in "${WORK[@]}"; do
  if [[ -d "$SVRN_ROOT/indexes/$c/atlas/atoms.lance" ]]; then STORE[$c]=had; continue; fi
  if "$SCLI" atlas migrate-all "$c" >>"$RUN_LOG" 2>&1; then STORE[$c]=built; else STORE[$c]=failed; fi
done

# ── step 2: the seed table, in batches, one ledger line per corpus ──────────
i=0; total=${#WORK[@]}
while (( i < total )); do
  chunk=("${WORK[@]:i:BATCH}")
  ids=$(IFS=,; echo "${chunk[*]}")
  t0=$(date +%s)
  out=$("$SCLI" atlas backfill-ann "$ids" 2>&1)
  dt=$(( $(date +%s) - t0 ))
  per=$(python3 -c "print(f'{$dt/max(1,${#chunk[@]}):.2f}')")
  printf '%s\n' "$out" >>"$RUN_LOG"
  for c in "${chunk[@]}"; do
    # Parse THIS corpus's own line out of the batch's output. A corpus with no
    # line at all is `failed`, never counted as done -- absence is reported.
    line=$(grep -F "backfill-ann $c:" <<<"$out" | head -1)
    if [[ "$line" == *"wrote"* ]]; then
      rows=$(sed -E 's/.* ([0-9]+)\/([0-9]+) bag entries.*/\1/' <<<"$line")
      of=$(sed -E 's/.* ([0-9]+)\/([0-9]+) bag entries.*/\2/' <<<"$line")
      ledger_line "$c" built "${STORE[$c]}" "$rows" "$of" "$per"
      echo "  ✓ $c — $rows/$of"
    elif [[ "$line" == *"filter excluded every atom"* ]]; then
      ledger_line "$c" no-seedable "${STORE[$c]}" - - "$per"
      echo "  · $c — no seedable atoms under the grounding filter"
    else
      ledger_line "$c" failed "${STORE[$c]}" - - "$per"
      echo "  ✗ $c — ${line:-no line in the batch output; see $RUN_LOG}"
    fi
  done
  i=$(( i + BATCH ))
  echo "progress: $(( i < total ? i : total ))/$total  (${dt}s for ${#chunk[@]})"
done

# ── the table: had / built / failed BY NAME ─────────────────────────────────
python3 - "$LEDGER" "$t_start" <<'PY'
import json,sys,time,collections
ledger,t0=sys.argv[1],int(sys.argv[2])
last={}
for line in open(ledger):
    line=line.strip()
    if not line: continue
    try: r=json.loads(line)
    except Exception: continue
    if r.get("at",0)>=t0: last[r["corpus"]]=r
by=collections.defaultdict(list)
for c,r in sorted(last.items()): by[r["state"]].append(r)
print()
print(f"{'state':<14}{'corpora':>9}{'atoms embedded':>17}")
print("-"*40)
for state in ("built","no-seedable","failed"):
    rs=by.get(state,[])
    print(f"{state:<14}{len(rs):>9}{sum(r.get('rows',0) for r in rs):>17}")
stores=collections.Counter(r.get("store") for r in last.values())
print(f"\nv2 store: had {stores['had']}, built {stores['built']}, failed {stores['failed']}")
print(f"wall: {int(time.time())-t0}s over {len(last)} corpus(es)")
for state in ("failed","no-seedable"):
    rs=by.get(state,[])
    if rs: print(f"\n{state} BY NAME:\n  " + "\n  ".join(r["corpus"] for r in rs))
PY
