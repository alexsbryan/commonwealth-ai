#!/usr/bin/env bash
# ring-room-demo.sh — the rr-1 instrument: three machines, one sitting, four legs.
#
# REUSES ring-doc-demo.sh by sourcing it (its dispatcher is guarded): the node
# door `sv`/`node_exec`/`node_curl`, the podman backend, `up`/`down`, the
# 100-edit drive and the attribution leg (RING_DOC_PHASES=1,3). Nothing of it
# is copied here; what this adds is the four legs' measurement and the census.
#
#   1. answer  b ingests a small folder that a never installs; a answers a
#              5-question bank from it; each released citation's member is read.
#   2. doc     ring-doc's drive + attribution, names from mesh status, the
#              roster origin, and the command log grepped for `roster add`.
#   3. film    Jellyfin runs as a sibling container in b's network namespace;
#              b runs cw-media-demo.sh's holder-setup (wizard, key, the offer
#              verb); c polls GET /v1/mesh/media for the offer and its
#              `offered to`; b narrows to a and c; c times the first byte.
#   4. join    a fourth podman node joins by the link a's `mesh status` prints;
#              legs 1-3's checks re-run against the member mesh status names new.
#   5. census  every leg appends what it "typed" on the person's behalf to a
#              log; the verdict prints it classified, INSTALL (bring-up, before
#              the walk) and WALK apart. The bar's count is the WALK.
#
# No member name, port, host or corpus id is written here: names and N come
# from `svrn mesh status` on the nodes, ports from ring-doc-demo.sh's node
# door, the corpus id from a folder name generated at run time, the origin
# from cw-media-demo.sh. The census greps this file for each and prints hits.
#
#   scripts/ring-room-demo.sh up | down | verdict <bar|all>
#
# Weights (seat decision 2026-09-18 #4): b gets the 0.6B embedder, a gets the
# embedder and RING_ROOM_CHAT (default the smallest chat GGUF that released a
# citation here). Podman is the rehearsed topology and the default; leg 3
# needs it (the netns sibling), so on `--backend local` leg 3 cannot judge.
#
# `verdict` prints one co-lineage measurement line per bar as its LAST lines,
# floors and directions READ from quality/campaigns/ring-room.toml.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
export RING_DOC_BACKEND="${RING_DOC_BACKEND:-podman}"
[ "${1:-}" = --backend ] && { RING_DOC_BACKEND="${2:-}"; shift 2; }
export RING_DOC_DIR="${RING_DOC_DIR:-$REPO/target/ring-room-demo}"
export RING_DOC_PHASES=1,3
# shellcheck source=scripts/ring-doc-demo.sh
source "$REPO/scripts/ring-doc-demo.sh" _sourced

ROOM_CAMPAIGN="$REPO/quality/campaigns/ring-room.toml"
ROOM_SCRIPT="$REPO/scripts/ring-room-demo.sh"
MEDIA_SCRIPT="$REPO/scripts/cw-media-demo.sh"
EMBED_GGUF="$REPO/sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf"
CHAT_GGUF="${RING_ROOM_CHAT:-$REPO/sovereign/models/Qwen3.5-2B.Q6_K.gguf}"
# Beside $D, not in it: bring-up empties $D.
CMDLOG="$D-commands.log"
TYPED="$D-typed.tsv"
MEDIA_ROOT="$D-media"
JELLY="ring-room-jellyfin"
STAGE=install

# The PRE-REGISTRATION: the run's shape, fixed before any number exists.
POLL_S=2          # c's offers poll, the rail's cadence stand-in
ASK_TIMEOUT_S=300 # one question, one synthesis on a CPU node
JOIN_POLL_S=2     # join_poll_s: the join-to-visible window's poll (1–5 s, B §Tuning)
JOIN_WATCH_S=120  # how long each join check is watched; the bar's window is read in the report
# Which legs run (default all); a leg left out reads COULD-NOT-JUDGE.
RING_ROOM_LEGS="${RING_ROOM_LEGS:-answer,doc,film,join}"

# ── the command log: every command on every node goes through node_exec ─────
eval "ring_doc_$(declare -f node_exec)"
node_exec() { printf '%s\t%s\t%s\n' "$STAGE" "$1" "${*:2}" >> "$CMDLOG"; ring_doc_node_exec "$@"; }

# typed <node> <leg> <string> [note] [secret] — a string the person would have
# typed. The class is decided in ONE place, `classify` in the report, from the
# string itself; only a credential cannot be recognised by shape, so it says so.
typed() { printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$STAGE" "$1" "$2" "${5:-}" "$3" "${4:-}" >> "$TYPED"; }

# ── weights: ring-doc's config, then a [models] table on a and b ────────────
eval "ring_doc_$(declare -f mkcfg)"
# `[models]` requires `primary`, so b names the same chat GGUF; b is never asked.
mkcfg() {
  ring_doc_mkcfg "$1"
  case "$1" in
    a|b|d) printf '\n[models]\nprimary = "%s"\nembed = "%s"\n' "$CHAT_GGUF" "$EMBED_GGUF" >> "$D/$1/config.toml" ;;
  esac
}

if command -v podman >/dev/null; then HOSTRUN=(); else HOSTRUN=(flatpak-spawn --host); fi

# ── the fourth machine: the same node door, one node more ───────────────────
# Its ports and address continue the door's own stride past the third, so none
# is written here. Its container is ring-doc-demo.sh's `containers_up` body run
# for that one node (the network is up, the three must stay), so the podman
# line and its HOME guard stay in one place.
CPORT[d]=$(( CPORT[c] * 2 - CPORT[b] )); IPORT[d]=$(( IPORT[c] * 2 - IPORT[b] )); DPORT[d]=$(( DPORT[c] * 2 - DPORT[b] ))
IP[d]="${IP[c]%.*}.$(( ${IP[c]##*.} * 2 - ${IP[b]##*.} ))"
eval "$(declare -f containers_up | sed -e '1s/containers_up/container_up_one/' -e '/^ *containers_down;$/d' \
  -e '/network create/,/^ *};$/d' -e 's/for n in a b c;/for n in "$1";/')"
declare -f container_up_one | grep -q 'for n in "$1"' \
  || { echo "ring-room-demo: ring-doc-demo.sh's containers_up changed shape; the fourth node has no door" >&2; exit 3; }

mesh_json() { sv "$1" mesh status --json; }
self_name() { mesh_json "$1" | python3 -c "import sys,json; print(next((m['name'] for m in json.load(sys.stdin)['members'] if m.get('is_self')), ''))" 2>/dev/null; }

# ── leg 1: the answer names the machine ──────────────────────────────────────
# The bank: five facts that exist nowhere but this folder, and a question each.
BANK='[
 ["The Larkspur Lane cooperative keeps its bees in four hives painted teal, ochre, plum and slate.", "What colours are the four hives of the Larkspur Lane cooperative painted?", "ochre"],
 ["The Larkspur Lane cooperative weighed its 2026 honey harvest at thirty-seven kilograms.", "How much did the Larkspur Lane cooperative 2026 honey harvest weigh?", "thirty-seven"],
 ["Ottoline Marsh founded the Larkspur Lane cooperative and planted its quince tree by the north gate.", "Who founded the Larkspur Lane cooperative?", "Marsh"],
 ["At the Larkspur Lane cooperative the compost heaps are turned every second Thursday.", "How often are the Larkspur Lane cooperative compost heaps turned?", "Thursday"],
 ["The Larkspur Lane cooperative shares one cargo bike, named Pelican, kept in the blue shed.", "What is the Larkspur Lane cooperative cargo bike called?", "Pelican"]
]'

# share_folder <node> <leg> <bank-json> — the node ingests a folder holding the
# bank's facts and shares it; prints `<corpus-id> <ingest-rc> <meta-path>`.
share_folder() {
  local n=$1 leg=$2 folder id meta rc
  folder=$(mktemp -d "$D/room-XXXXXX") # its basename is the corpus id: generated, never written here
  id=$(basename "$folder")
  BANK="$3" python3 -c "
import json, os, sys
for i, (fact, _, _) in enumerate(json.loads(os.environ['BANK'])):
    open(os.path.join(sys.argv[1], f'note-{i}.md'), 'w').write(fact + '\n')" "$folder"
  typed "$n" "$leg" "corpus ingest $folder"
  node_exec "$n" "$CLI" corpus ingest "$folder" > "$D/room-ingest-$n.out" 2>&1
  rc=$?
  meta=$(find "$D/$n" -name _corpus_meta.json -path "*/$id/*" 2>/dev/null | head -1)
  # No verb turns query sharing on for an ingested folder (corpus_store.rs
  # creates it local-only): the driver writes the flag, and the census counts it.
  if [ -n "$meta" ]; then
    python3 -c "import json,sys; p=sys.argv[1]; m=json.load(open(p)); m['query_sharing']=True; json.dump(m,open(p,'w'))" "$meta"
    typed "$n" "$leg" '"query_sharing": true' "written into $meta — no verb shares an ingested folder"
  fi
  # The daemon resolves query_sharing at open: the script restarts it, as D does.
  stop_node "$n"; start_node "$n" > /dev/null && wait_homed "$n"
  echo "$id $rc $meta"
}

leg_answer() {
  local out="$D/room-answer.json" id meta bname i q ingest_rc
  bname=$(self_name b)
  read -r id ingest_rc meta < <(share_folder b answer "$BANK")
  sv a corpus list > "$D/room-a-corpora.txt"
  # a learns b's corpus from gossip: waited for on a's own fan-out (the route
  # the answer's retrieval uses), then asked regardless. Not scored.
  local seen="" t0; t0=$(date +%s)
  while [ $(( $(date +%s) - t0 )) -lt 120 ]; do
    node_curl a -s --max-time 10 -X POST "$(at "${CPORT[a]}")/v1/knowledge/search" -H 'content-type: application/json' \
      -d "{\"query\":\"Larkspur Lane cooperative\",\"corpora\":[\"$id\"]}" > "$D/room-fanout.json" 2>/dev/null
    grep -q '"peer_name"' "$D/room-fanout.json" \
      && { seen=$(( $(date +%s) - t0 )); break; }
    sleep 3
  done
  for i in 0 1 2 3 4; do
    q=$(BANK="$BANK" python3 -c "import json,os,sys; print(json.loads(os.environ['BANK'])[int(sys.argv[1])][1])" "$i")
    typed a answer "$q"
    node_exec a timeout "$ASK_TIMEOUT_S" "$CLI" chat ask --format json "$q" \
      > "$D/room-answer-$i.json" 2> "$D/room-answer-$i.err"
  done
  BANK="$BANK" python3 - "$D" "$id" "$bname" "$ingest_rc" "${meta:-}" "$seen" > "$out" <<'PY'
import json, os, sys
d, cid, bname, rc, meta, seen = sys.argv[1:7]
bank = json.loads(os.environ["BANK"])
listed = open(os.path.join(d, "room-a-corpora.txt")).read()
try:
    fan = json.load(open(os.path.join(d, "room-fanout.json")))
except Exception:
    fan = {}
# What a's /v1/knowledge/search itself returns for the corpus: the passages'
# members, one layer below the answer. Evidence beside the bar, never its value.
fanout_members = sorted({r.get("peer_name") for r in fan.get("results") or [] if r.get("peer_name")})
qs = []
for i, (_, q, key) in enumerate(bank):
    try:
        a = json.load(open(os.path.join(d, f"room-answer-{i}.json")))
    except Exception as e:
        qs.append({"q": q, "error": str(e)[:200]}); continue
    cites = (a.get("epistemic_state") or {}).get("citations") or []
    qs.append({"q": q, "released": len(cites), "members": [c.get("member") for c in cites],
               "names_b": any(c.get("member") == bname for c in cites),
               "answer_has": key.lower() in (a.get("visible") or "").lower(),
               "verdict": (a.get("epistemic_state") or {}).get("verdict")})
json.dump({"corpus": cid, "b_name": bname, "ingest_rc": int(rc), "shared_meta": bool(meta),
           "absent_on_a": cid not in listed, "a_heard_s": int(seen) if seen else None,
           "fanout_members": fanout_members, "questions": qs}, sys.stdout)
PY
}

# ── leg 2: the doc names you from membership ────────────────────────────────
leg_doc() {
  sv a ring roster "$RING" > "$D/room-roster-a.txt"
  local n
  for n in a b c; do typed "$n" doc "$(tab_url "$n")/" "the ring-doc page, as ring dev printed it"; done
  run_session
}

# ── leg 3: a film from the library rail ─────────────────────────────────────
offers_on_c() { node_curl c -s --max-time 5 "$(at "${CPORT[c]}")/v1/mesh/media"; }

# poll_offer <holder-name> <want-offered-to-json> <window-s> → seconds, or empty
poll_offer() {
  local t0 now; t0=$(date +%s.%N)
  while :; do
    now=$(date +%s.%N)
    python3 -c "import sys; sys.exit(0 if float(sys.argv[1]) - float(sys.argv[2]) <= float(sys.argv[3]) else 1)" "$now" "$t0" "$3" || return 0
    offers_on_c > "$D/room-offers.json" 2>/dev/null
    if python3 -c "
import json, sys
o = [x for x in json.load(open(sys.argv[1])).get('offering', []) if x.get('peer') == sys.argv[2]]
sys.exit(0 if o and sorted(o[0].get('offered_to') or []) == sorted(json.loads(sys.argv[3])) else 1)" "$D/room-offers.json" "$1" "$2" 2>/dev/null; then
      python3 -c "import sys; print(round(float(sys.argv[1]) - float(sys.argv[2]), 2))" "$(date +%s.%N)" "$t0"
      return 0
    fi
    sleep "$POLL_S"
  done
}

# media_holder <node> <leg> <container> <root> — the Jellyfin container, from
# the host, in the node's network namespace (it listens on the node's own
# loopback, where holder-setup and the node's daemon reach it), then
# holder-setup inside the node: wizard, the declared key, the offer verb.
media_holder() {
  local n=$1 leg=$2 origin
  origin=$(sed -n 's/^ORIGIN="\(.*\)"$/\1/p' "$MEDIA_SCRIPT")
  "${HOSTRUN[@]}" env CW_MEDIA_ROOT="$4" CW_MEDIA_NAME="$3" CW_MEDIA_NETWORK="container:ring-doc-$n" \
    bash "$MEDIA_SCRIPT" holder-up > "$D/room-holder-up-$n.out" 2>&1 || return 1
  typed "$n" "$leg" "demo / demo" "Jellyfin's own login (holder-setup's wizard, and the key it mints from it and declares) — the ONE excluded credential" secret
  typed "$n" "$leg" "mesh media offer $origin" "holder-setup's one verb"
  node_exec "$n" env SVRN="$CLI" CW_MEDIA_ROOT="$4" bash "$MEDIA_SCRIPT" holder-setup > "$D/room-holder-setup-$n.out" 2>&1
}

leg_film() {
  local out="$D/room-film.json"
  if [ "$BACKEND" != podman ]; then echo '{"skipped":"backend local: Jellyfin needs a node netns to sit in"}' > "$out"; return; fi
  local origin bname aname cname first narrowed url item fb
  origin=$(sed -n 's/^ORIGIN="\(.*\)"$/\1/p' "$MEDIA_SCRIPT")
  bname=$(self_name b); aname=$(self_name a); cname=$(self_name c)
  cp "$D/b/config.toml" "$D/room-b-config.before"; cp "$D/c/config.toml" "$D/room-c-config.before"
  rm -rf "$MEDIA_ROOT/config" "$MEDIA_ROOT/cache" # a cold holder each run; the generated title is kept
  media_holder b film "$JELLY" "$MEDIA_ROOT" \
    || { echo '{"fatal":"holder-up failed, see room-holder-up-b.out"}' > "$out"; return; }
  # The clock starts when the verb returns: its own restart of b's daemon, and
  # b's return to the mesh, are inside the window.
  first=$(poll_offer "$bname" '[]' 30)
  typed b film "mesh media offer $origin --admit $aname $cname" "the narrowing, one verb"
  node_exec b "$CLI" mesh media offer "$origin" --admit "$aname" "$cname" > "$D/room-narrow.out" 2>&1
  narrowed=$(poll_offer "$bname" "[\"$aname\",\"$cname\"]" 30)
  # The pick, as the rail makes it: the reach for that member (re-read every
  # poll, as the rail does — the narrowing just restarted b), then a title. The
  # clock starts at the pick, the moment the rail listed the narrowed offer, so
  # a holder the rail lists but cannot yet reach is inside the number.
  local pick_t0 deadline url=""
  pick_t0=$(date +%s.%N); deadline=$(( $(date +%s) + 120 ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    url=$(node_curl c -s --max-time 15 "$(at "${CPORT[c]}")/v1/mesh/media?peer=$bname" \
      | python3 -c "import sys,json; print(json.load(sys.stdin).get('url') or '')" 2>/dev/null)
    [ -n "$url" ] || { sleep 3; continue; }
    # Each attempt's status, beside the body: an empty body alone cannot tell a
    # 401 from a library still scanning from a shim that never answered.
    node_curl c -s --max-time 10 -o /dev/stdout -w '%{stderr}%{http_code} %{time_total}\n' \
      "$url/Items?Recursive=true&IncludeItemTypes=Movie" > "$D/room-items.json" 2>> "$D/room-items.codes"
    echo " $(date +%T) $url" >> "$D/room-items.codes"
    item=$(python3 -c "import sys,json; print((json.load(open(sys.argv[1])).get('Items') or [{}])[0].get('Id') or '')" "$D/room-items.json" 2>/dev/null)
    [ -n "$item" ] && break
    sleep 3
  done
  local pick_s=""
  if [ -n "${item:-}" ]; then
    fb=$(node_curl c -s -o /dev/null -r 0-65535 --max-time 30 -w '%{time_starttransfer} %{http_code}' \
      "$url/Videos/$item/stream?static=true")
    pick_s=$(python3 -c "import sys; print(round(float(sys.argv[1]) - float(sys.argv[2]), 2))" "$(date +%s.%N)" "$pick_t0")
  fi
  cp "$D/b/config.toml" "$D/room-b-config.after"; cp "$D/c/config.toml" "$D/room-c-config.after"
  python3 - "$D" "$bname" "${first:-}" "${narrowed:-}" "$url" "${item:-}" "${fb:-}" "$pick_s" > "$out" <<'PY'
import difflib, json, sys
d, bname, first, narrowed, url, item, fb, pick = sys.argv[1:9]
def diff(n):
    a = open(f"{d}/room-{n}-config.before").read().splitlines()
    b = open(f"{d}/room-{n}-config.after").read().splitlines()
    return [l for l in difflib.unified_diff(a, b, lineterm="", n=0) if l[:1] in "+-" and l[:3] not in ("+++", "---")]
t, code = (fb.split() + ["", ""])[:2] if fb else ("", "")
json.dump({"holder": bname, "listed_s": float(first) if first else None, "narrowed_s": float(narrowed) if narrowed else None,
           "player_url": bool(url), "item": bool(item), "stream_first_byte_s": float(t) if t else None,
           "pick_to_first_byte_s": float(pick) if pick else None, "http": code,
           "b_config_diff": diff("b"), "c_config_diff": diff("c")}, sys.stdout)
PY
}

# ── leg 4: a fourth member plugs in ─────────────────────────────────────────
# The fourth's corpus: a fact that exists nowhere but its folder.
JOIN_BANK='[
 ["The Hollowmere allotment society keeps its seed library in a tin trunk under the oak by the pond.", "Where does the Hollowmere allotment society keep its seed library?", "trunk"]
]'

# The fourth's page, headless: one word typed into the doc as its node holds
# it, then a's page polled until a holds that act and names its writer. The
# naming is the adapter's own `personFor` over each node's roster.
join_doc_js() {
  cat > "$D/join-doc.mjs" <<'JS'
const { REPO, PD, PA, POLL_S, WATCH_S } = process.env;
const A = await import(`file://${REPO}/sovereign/apps/ring-doc/adapter.js`);
const { Y } = await import(`file://${REPO}/sovereign/apps/ring-doc/vendor/ring-doc-bundle.js`);
// The SDK's fold, as ring-doc-demo.sh's driver carries it.
const fold = (log, reducer, initial) => {
  let acc = initial;
  for (const op of (log && log.ops) || []) {
    if (op.voided || op.payload == null) continue;
    acc = reducer(acc, op.payload, op);
  }
  return acc;
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const call = async (base, op, body) => {
  const r = await fetch(`${base}/__ring/${op}`, { method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify(body || {}), signal: AbortSignal.timeout(20000) });
  const v = await r.json().catch(() => null);
  if (!r.ok) throw new Error(JSON.stringify(v));
  return v;
};
const out = { errors: [] };
const done = () => { console.log(JSON.stringify(out)); process.exit(0); };
const ydoc = new Y.Doc(), applied = new Set(), frag = ydoc.getXmlFragment(A.PROSE_FRAGMENT);
let deadline = Date.now() + WATCH_S * 1000;
while (frag.length === 0 && Date.now() < deadline) {
  try { A.applyNew(ydoc, A.decodeActs(await call(PD, "log"), fold).acts, applied); }
  catch (e) { out.errors.push(`d log: ${e.message || e}`); }
  if (frag.length === 0) await sleep(POLL_S * 1000);
}
if (frag.length === 0) { out.fatal = "the doc never reached the fourth's node"; done(); }
let update = null;
ydoc.on("update", (u) => { update = u; });
const text = frag.get(0).get(0);
text.insert(text.length, " plugged-in");
let written;
try { written = await call(PD, "append", { op: "record", payload: A.changeAct(update) }); }
catch (e) { out.fatal = `the fourth's append was refused: ${e.message || e}`; done(); }
const t0 = Date.now();
out.act = written.id;
deadline = t0 + WATCH_S * 1000;
while (Date.now() < deadline && (out.a_s === undefined || !out.d_self)) {
  try {
    if (!out.d_self) out.d_self = A.personFor(((await call(PD, "log")).roster || {}).members, written.actor);
    const aLog = await call(PA, "log");
    out.a_roster = Object.keys((aLog.roster || {}).members || {});
    const act = A.decodeActs(aLog, fold).acts.find((x) => x.id === written.id);
    const name = act && A.personFor((aLog.roster || {}).members, act.actor);
    if (name && out.a_s === undefined) { out.a_name = name; out.a_s = (Date.now() - t0) / 1000; }
  } catch (e) { out.errors.push(String(e.message || e)); }
  await sleep(POLL_S * 1000);
}
out.errors = out.errors.slice(-3);
done();
JS
}

# Each clock starts when the fourth's own act returns (the append, the shared
# corpus back up, the offer verb) and stops when an observer sees it: a for the
# doc and the answer, c's rail for the library — on the host clock they share.
leg_join() {
  local out="$D/room-join.json" n_before n_after link="" dname t0
  local members='import sys,json; print(len(json.load(sys.stdin)["members"]))'
  n_before=$(mesh_json a | python3 -c "$members")
  mkcfg d
  fatal() { python3 -c "import json,sys; print(json.dumps({'fatal': sys.argv[1]}))" "$1" > "$out"; }
  if [ "$BACKEND" = podman ]; then container_up_one d || { fatal "the fourth container did not start"; return; }; fi
  start_daemon d
  { wait_all_up d && wait_homed d; } || { fatal "the fourth daemon never came up homed"; return; }
  # The invite, as a shows it: `mesh status`'s `join link:` line.
  t0=$(date +%s)
  while [ $(( $(date +%s) - t0 )) -lt 30 ]; do
    link=$(sv a mesh status | sed -n 's/^join link: //p'); [ -n "$link" ] && break; sleep 2
  done
  [ -n "$link" ] || { fatal "a's mesh status printed no join link inside 30 s"; return; }
  typed d join "mesh join $link" "the invite a shows — scanned as a QR in the room (no renderer yet: rr-2)"
  node_exec d "$CLI" mesh join "$link" > "$D/room-join.out" 2>&1
  dname=$(self_name d)
  [ -n "$dname" ] || { fatal "the fourth's mesh status names no self after the join, see room-join.out"; return; }
  wait_online "$dname" 2> "$D/room-join-online.err" || { fatal "a never saw the fourth online"; return; }
  n_after=$(mesh_json a | python3 -c "$members")

  # (a) the doc: its page names it, and its edit is attributed on a.
  start_proxy d 2> "$D/room-join-proxy.err"
  typed d join "$(tab_url d)/" "the ring-doc page, as ring dev printed it"
  join_doc_js
  REPO="$REPO" PD="$(tab_url d)" PA="$(tab_url a)" POLL_S="$JOIN_POLL_S" WATCH_S="$JOIN_WATCH_S" \
    node "$D/join-doc.mjs" > "$D/room-join-doc.json" 2> "$D/room-join-doc.err"

  # (c) the answer: a question only its corpus answers, answered on a with its
  # name. Before the library: the offer verb restarts d's daemon outside
  # stop_node's pidfile, and share_folder restarts it through that pidfile.
  local id rc meta q i=0 answered="" t1 el
  read -r id rc meta < <(share_folder d join "$JOIN_BANK")
  q=$(BANK="$JOIN_BANK" python3 -c "import json,os; print(json.loads(os.environ['BANK'])[0][1])")
  typed a join "$q"
  t1=$(date +%s.%N)
  while :; do
    el=$(python3 -c "import sys; print(int(float(sys.argv[1]) - float(sys.argv[2])))" "$(date +%s.%N)" "$t1")
    [ "$el" -lt "$JOIN_WATCH_S" ] || break
    node_exec a timeout "$JOIN_WATCH_S" "$CLI" chat ask --format json "$q" > "$D/room-join-answer-$i.json" 2> "$D/room-join-answer-$i.err"
    if python3 -c "
import json, sys
c = (json.load(open(sys.argv[1])).get('epistemic_state') or {}).get('citations') or []
sys.exit(0 if any(x.get('member') == sys.argv[2] for x in c) else 1)" "$D/room-join-answer-$i.json" "$dname" 2>/dev/null; then
      answered=$(python3 -c "import sys; print(round(float(sys.argv[1]) - float(sys.argv[2]), 2))" "$(date +%s.%N)" "$t1")
      break
    fi
    i=$(( i + 1 )); sleep "$JOIN_POLL_S"
  done

  # (b) the library: the one verb on the fourth, listed in c's rail.
  local listed="" root="$MEDIA_ROOT-d"
  if [ "$BACKEND" = podman ]; then
    rm -rf "$root/config" "$root/cache"; mkdir -p "$root/media"
    cp -n "$MEDIA_ROOT"/media/*.mp4 "$root/media/" 2>/dev/null # leg 3's generated title, not encoded twice
    media_holder d join "$JELLY-d" "$root" && listed=$(poll_offer "$dname" '[]' "$JOIN_WATCH_S")
  fi

  python3 - "$D" "$dname" "$n_before" "$n_after" "$id" "$rc" "${meta:-}" "$answered" "$i" "$listed" "$BACKEND" > "$out" <<'PY'
import json, sys
d, name, nb, na, cid, rc, meta, answered, asks, listed, backend = sys.argv[1:12]
try:
    doc = json.load(open(f"{d}/room-join-doc.json"))
except Exception as e:
    doc = {"fatal": f"the fourth's page wrote nothing: {e}"}
json.dump({"name": name, "n_before": int(nb), "n_after": int(na), "doc": doc,
           "corpus": cid, "ingest_rc": int(rc), "shared_meta": bool(meta),
           "answered_s": float(answered) if answered else None, "asks": int(asks) + (1 if answered else 0),
           "listed_s": float(listed) if listed else None,
           "library": None if backend == "podman" else "backend local: Jellyfin needs a node netns to sit in"}, sys.stdout)
PY
}

# ── the census + the five rows ───────────────────────────────────────────────
report() { # bar|all
  python3 - "$D" "$ROOM_CAMPAIGN" "$1" "$TYPED" "$CMDLOG" "$ROOM_SCRIPT" "${CPORT[*]} ${IPORT[*]} ${DPORT[*]}" "${IP[*]}" <<'PY'
import json, os, re, sys, tomllib
d, campaign, want, typed_p, cmdlog_p, script_p, ports, ips = sys.argv[1:9]
bars = {b["id"]: b for b in tomllib.load(open(campaign, "rb"))["bar"]}
COUNTED = ("address", "port", "URL", "config-line", "credential")

def classify(s, secret=""):
    """THE one decider for a typed string's class."""
    s = s.strip()
    if secret: return "credential"
    if re.search(r"\w+://", s): return "URL"
    if re.search(r"\b(\d{1,3}(\.\d{1,3}){3}|localhost)(:\d+)?\b", s): return "address"
    if re.fullmatch(r"\d{2,5}", s) or re.search(r"(?<![\w.])\d{4,5}(?![\w.])", s) and re.search(r"port", s): return "port"
    if re.match(r'^\s*(\[[\w.-]+\]|"?[\w.-]+"?\s*[:=]\s*\S)', s): return "config-line"
    return "other"

def load(name):
    p = os.path.join(d, name)
    try: return json.load(open(p))
    except Exception: return None

entries = []
for line in open(typed_p).read().splitlines() if os.path.exists(typed_p) else []:
    stage, node, leg, secret, s, note = (line.split("\t") + [""] * 6)[:6]
    entries.append(dict(stage=stage, node=node, leg=leg, string=s, note=note, cls=classify(s, secret),
                        excluded=bool(secret) and "Jellyfin" in note))
# INSTALL: what bring-up wrote where the daemons read it, before the walk.
install = []
for n in "abcd":
    p = os.path.join(d, n, "config.toml")
    if not os.path.exists(p): continue
    before = os.path.join(d, f"room-{n}-config.before")
    for l in open(before if os.path.exists(before) else p).read().splitlines():
        if not l.strip() or l.startswith("#"): continue
        c = classify(l)
        # Weights are named by path; a model path is not a member's anything.
        if re.search(r"\.gguf\"?\s*$", l): c = "other"
        install.append(dict(node=n, string=l, cls=c))
cmdlog = open(cmdlog_p).read() if os.path.exists(cmdlog_p) else ""
for m in re.finditer(r'"key_or_url":"([^"]+)"', cmdlog):
    install.append(dict(node="-", string=m.group(1), cls=classify(m.group(1)), note="the join link bring-up posted"))

walk = [e for e in entries if e["stage"] == "walk"]
walk_count = sum(1 for e in walk if e["cls"] in COUNTED and not e["excluded"])
install_count = sum(1 for e in install if e["cls"] in COUNTED)

# What this script itself names: every member name, node port, node address, corpus id.
ans = load("room-answer.json") or {}
j = load("room-join.json") or {}
members = load("members.json") or {}
needles = sorted(set(list(members.values()) + ports.split() + ips.split()
                     + [x for x in (ans.get("corpus"), j.get("name"), j.get("corpus")) if x]))
text = open(script_p).read()
hits = [n for n in needles if re.search(r"(?<![\w.])" + re.escape(n) + r"(?![\w.])", text)]

print("== census: INSTALL (bring-up, before the walk) — printed and classified; the bar's count is the WALK ==")
for e in install:
    if e["cls"] != "other" or "gguf" in e["string"]:
        print(f"  install  {e['node']}  {e['cls']:<11} {e['string']}" + (f"   ({e['note']})" if e.get("note") else ""))
print(f"  install counted-class entries: {install_count}")
print("== census: WALK (typed on the person's behalf during the walk) ==")
for e in walk:
    flag = "EXCLUDED" if e["excluded"] else ("COUNTS" if e["cls"] in COUNTED else "")
    print(f"  walk  {e['leg']:<6} {e['node']}  {e['cls']:<11} {flag:<8} {e['string']}" + (f"   ({e['note']})" if e["note"] else ""))
print(f"  walk count: {walk_count}")
print(f"== census: this script names {len(hits)} of {len(needles)} member names/ports/addresses/corpus ids: {hits} ==")

rows = {}
def row(bar, value, reason="", **extra): rows[bar] = dict(bar=bar, value=value, reason=reason, **extra)
num = lambda pat, s: float(re.search(pat, s).group(1)) if re.search(pat, s) else None

# 1 — the answer names the machine
if not ans or not ans.get("questions"):
    row("ra-room-answer-names-the-machine", None, "the answer leg did not run")
elif not ans["absent_on_a"]:
    row("ra-room-answer-names-the-machine", 0.0, "the corpus is installed on a: the positive control failed", **ans)
else:
    qs = ans["questions"]
    row("ra-room-answer-names-the-machine", sum(1 for q in qs if q.get("names_b")) / len(qs), "",
        b_name=ans["b_name"], absent_on_a=True, a_heard_s=ans["a_heard_s"], shared_meta=ans["shared_meta"],
        fanout_members=ans.get("fanout_members"),
        questions=[{k: q.get(k) for k in ("released", "members", "verdict", "answer_has", "error")} for q in qs])

# 2 — the doc names you from membership
s = load("session.json") or {"fatal": "the driver wrote no session.json"}
roster = open(os.path.join(d, "room-roster-a.txt")).read().strip() if os.path.exists(os.path.join(d, "room-roster-a.txt")) else ""
at, nu = s.get("attribution") or {}, s.get("nudge") or {}
ceiling = num(r"p99 propagation <= ([\d.]+) s", bars["ra-room-doc-name-from-membership"]["floor_basis"])
if s.get("fatal") or not at or not nu.get("samples") or ceiling is None:
    row("ra-room-doc-name-from-membership", None, s.get("fatal") or ("floor_basis names no p99 ceiling" if ceiling is None else "the drive did not run"))
else:
    forged_ok = at["forged_held_everywhere"] and at["forged_clients"] == [at["a_client"]]
    legs = {"a_names_from_mesh": forged_ok and at["lines"] > 0 and at["right"] == at["lines"],
            "b_roster_derived": "everyone in the mesh" in roster.splitlines(),
            "c_no_roster_add": "roster add" not in cmdlog,
            "d_p99": nu["p99"] is not None and nu["p99"] <= ceiling}
    row("ra-room-doc-name-from-membership", 1.0 if all(legs.values()) else 0.0, "", legs=legs, roster=roster,
        p50=nu["p50"], p99=nu["p99"], acts=nu["acts"], lines=at["lines"], right=at["right"], wrong=at["wrong"][:3])

# 3 — a film from the library rail
f = load("room-film.json") or {}
fb = bars["ra-room-film-from-the-library-rail"]["one_line"]
list_w, byte_w = num(r"within (\d+) s", fb), num(r"first byte <= ([\d.]+) s", fb)
if not f or f.get("skipped") or f.get("fatal"):
    row("ra-room-film-from-the-library-rail", None, f.get("skipped") or f.get("fatal") or "the film leg did not run")
else:
    verb_keys = re.compile(r'^[+-]\s*media_(origin|allow)\s*=')
    c_urls = [e for e in walk if e["leg"] == "film" and e["node"] == "c" and e["cls"] == "URL"]
    legs = {"a_one_verb_no_config_edit": all(verb_keys.match(l) for l in f["b_config_diff"]) and not f["c_config_diff"],
            "b_listed_and_narrowed": f["listed_s"] is not None and f["listed_s"] <= list_w
                                     and f["narrowed_s"] is not None and f["narrowed_s"] <= list_w,
            "c_first_byte": f["pick_to_first_byte_s"] is not None and f["http"] in ("200", "206")
                            and f["pick_to_first_byte_s"] <= byte_w,
            "d_viewer_typed_no_url": not c_urls}
    row("ra-room-film-from-the-library-rail", 1.0 if all(legs.values()) else 0.0, "", legs=legs,
        **{k: f[k] for k in ("holder", "listed_s", "narrowed_s", "pick_to_first_byte_s", "stream_first_byte_s", "http",
                            "b_config_diff", "c_config_diff")})

# 4 — a fourth member plugs in
win = num(r"within (\d+) s", bars["ra-room-plug-in-live"]["one_line"])
if not j:
    row("ra-room-plug-in-live", None, "phase-missing")
elif j.get("fatal"):
    row("ra-room-plug-in-live", None, j["fatal"])
else:
    within = lambda s: s is not None and win is not None and s <= win
    doc = j["doc"]
    legs = {"a_doc_names_and_attributes": doc.get("d_self") == j["name"] and j["name"] in (doc.get("a_roster") or [])
                                          and doc.get("a_name") == j["name"] and within(doc.get("a_s")),
            "b_library_listed": within(j["listed_s"]),
            "c_answer_names": within(j["answered_s"]),
            "d_n_from_mesh_only": j["n_after"] == j["n_before"] + 1 and not hits}
    row("ra-room-plug-in-live", 1.0 if all(legs.values()) else 0.0, "", legs=legs, window_s=win,
        **{k: j[k] for k in ("name", "n_before", "n_after", "listed_s", "answered_s", "asks", "library")},
        doc={k: doc.get(k) for k in ("d_self", "a_name", "a_s", "a_roster", "fatal", "errors")})

# 5 — nothing typed
row("ra-room-nothing-typed", walk_count if walk else None, "" if walk else "no leg typed anything: the walk did not run",
    walk=[f"{e['leg']}/{e['node']} {e['cls']}: {e['string']}" for e in walk if e["cls"] in COUNTED and not e["excluded"]],
    excluded=[f"{e['string']} — {e['note']}" for e in walk if e["excluded"]],
    install_counted=install_count, script_names=hits)

order = ["ra-room-answer-names-the-machine", "ra-room-doc-name-from-membership",
         "ra-room-film-from-the-library-rail", "ra-room-plug-in-live", "ra-room-nothing-typed"]
for bar in (order if want == "all" else [want]):
    r, b = rows[bar], bars[bar]
    v = r["value"]
    verdict = "COULD-NOT-JUDGE" if v is None else (
        ("PASSED" if v <= b["floor"] else "FAILED") if b["direction"] == "lower_is_better" else
        ("PASSED" if v >= b["floor"] else "FAILED"))
    r.update(floor=b["floor"], verdict=verdict, topology=f"three {os.environ.get('RING_DOC_BACKEND')} nodes, one host", artifact=d)
    print(json.dumps(r))
# Four verdicts, as ring-doc's: 1 any FAILED, 4 some COULD-NOT-JUDGE, 0 all PASSED.
vs = [rows[b]["verdict"] for b in (order if want == "all" else [want])]
sys.exit(1 if "FAILED" in vs else (4 if "COULD-NOT-JUDGE" in vs else 0))
PY
}

room_down() { # Jellyfin first: podman will not remove b while a container shares its netns
  [ "$BACKEND" = podman ] && "${PODMAN[@]}" rm -f -t 2 "$JELLY" > /dev/null 2>&1
  [ "$BACKEND" = podman ] && "${PODMAN[@]}" rm -f -t 2 "$JELLY-d" > /dev/null 2>&1
  local n=d; node_kill $n "$D/$n/dev.pid"; node_kill $n "$D/$n/pid"
  [ "$BACKEND" = podman ] && "${PODMAN[@]}" rm -f -t 2 "ring-doc-$n" > /dev/null 2>&1
  cmd_down
}

case "${1:-}" in
  up)   : > "$CMDLOG"; : > "$TYPED"; cmd_up ;;
  down) room_down; echo "stopped" ;;
  verdict)
    python3 -c "import sys,tomllib; ids=[b['id'] for b in tomllib.load(open(sys.argv[1],'rb'))['bar']]; sys.exit(0 if sys.argv[2] in ids+['all'] else 1)" \
      "$ROOM_CAMPAIGN" "${2:-}" || { echo "verdict: name a bar or all — see quality/campaigns/ring-room.toml" >&2; exit 2; }
    need_binaries
    : > "$CMDLOG"; : > "$TYPED"
    trap room_down EXIT
    cmd_up > "$D-up.log" 2>&1 || { echo "bring-up failed, see $D-up.log" >&2; exit 3; }
    STAGE=walk
    for leg in ${RING_ROOM_LEGS//,/ }; do "leg_$leg" > "$D-$leg.log" 2>&1; done
    report "$2"
    ;;
  *) sed -n '2,34p' "$0"; exit 2 ;;
esac
