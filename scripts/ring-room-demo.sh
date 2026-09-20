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
# door, the corpus id from a folder name generated at run time, the media origin
# found by the offer verb. The census greps this file for each and prints hits.
#
#   scripts/ring-room-demo.sh up | down | verdict <bar|all>
#
# TWO TOPOLOGIES, one instrument (RING_ROOM_TOPOLOGY, default `three`):
#
#   three  rr-1's, above and unchanged. It stays the regression gate.
#   room   rr-2's, the room as it will be. FOUR nodes on TWO podman networks
#          through ring-doc-demo.sh's own door — `room` is the venue's WiFi,
#          `uplink` is its internet:
#            beefy   the ONE member in the room, on both, with the host's
#                    render node; it drives the wall, mints the grants, serves
#                    the guest door and schedules every guest ask
#            halo    a member on the uplink only: it holds the corpus the ask
#                    must cite, and is the keeper the doc converges back to
#            little  a member on the uplink only, with Jellyfin in its netns
#            phone   the room's WiFi ONLY, a stock node image, NO daemon and
#                    no key: a guest, holding a bearer it read off a QR
#          Its legs are wall (the scan, the name, the edit, the ask), film
#          (offer, play, in-use, withdraw) and offline (the uplink cut), and
#          it reports the SIX rr-2 bars. Cutting `uplink` leaves the room's
#          WiFi up, which is what the offline bar's goodhart requires.
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
# TOPOLOGY. `three` is rr-1's, unchanged and still the regression gate.
# `room` is rr-2's: the room as it will be — ONE member in the room on the
# room's WiFi, the rest of the mesh elsewhere, and phones that are guests.
# It selects the node SET, the container prefix and the two podman networks
# through ring-doc-demo.sh's own door rather than beside it.
TOPOLOGY="${RING_ROOM_TOPOLOGY:-three}"
case "$TOPOLOGY" in three|room) ;; *) echo "ring-room-demo: topology is three or room, not '$TOPOLOGY'" >&2; exit 2 ;; esac
if [ "$TOPOLOGY" = room ]; then
  export RING_DOC_NODES="beefy halo little phone"
  export RING_DOC_CPREFIX=ring-room
  export RING_DOC_FOUNDER=beefy
  # The wall drives a model for every guest ask; it is the one machine in the
  # room and the only one here that gets the host's render node (O2 (vii)).
  export RING_DOC_GPU_NODES="${RING_DOC_GPU_NODES:-beefy}"
  export RING_DOC_DIR="${RING_DOC_DIR:-$REPO/target/ring-room-rr2-demo}"
  export RING_DOC_BACKEND=podman
fi
export RING_DOC_DIR="${RING_DOC_DIR:-$REPO/target/ring-room-demo}"
export RING_DOC_PHASES=1,3
# shellcheck source=scripts/ring-doc-demo.sh
source "$REPO/scripts/ring-doc-demo.sh" _sourced

ROOM_CAMPAIGN="$REPO/quality/campaigns/ring-room.toml"
ROOM_SCRIPT="$REPO/scripts/ring-room-demo.sh"
MEDIA_SCRIPT="$REPO/scripts/cw-media-demo.sh"
EMBED_GGUF="$REPO/sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf"
# The 2B is the bank's floor and the default; the answer bar is judged with
# RING_ROOM_CHAT=$REPO/sovereign/models/Qwen3.5-4B.Q6_K.gguf (seat A22).
CHAT_GGUF="${RING_ROOM_CHAT:-$REPO/sovereign/models/Qwen3.5-2B.Q6_K.gguf}"
# Beside $D, not in it: bring-up empties $D.
CMDLOG="$D-commands.log"
TYPED="$D-typed.tsv"
MEDIA_ROOT="$D-media"
JELLY="ring-room-jellyfin"
STAGE=install

# The PRE-REGISTRATION: the run's shape, fixed before any number exists.
POLL_S=2          # c's offers poll, the rail's cadence stand-in
ASK_TIMEOUT_S=600 # one question, one synthesis; a measurement budget - the room's machine is not a CPU node, this one is
JOIN_POLL_S=2     # join_poll_s: the join-to-visible window's poll (1–5 s, B §Tuning)
JOIN_WATCH_S=120  # how long each join check is watched; the bar's window is read in the report
# Which legs run (default all); a leg left out reads COULD-NOT-JUDGE.
RING_ROOM_LEGS="${RING_ROOM_LEGS:-answer,doc,film,join}"

# ── the command log: every command on every node goes through node_exec ─────
eval "ring_doc_$(declare -f node_exec)"
node_exec() { printf '%s\t%s\t%s\n' "$STAGE" "$1" "${*:2}" >> "$CMDLOG"; ring_doc_node_exec "$@"; }

# typed <node> <leg> <string> [note] [secret] [provenance] — a string the person would have
# typed. The class is decided in ONE place, `classify` in the report, from the
# string itself; only a credential cannot be recognised by shape, so it says so.
typed() { printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$STAGE" "$1" "$2" "${5:-}" "$3" "${4:-}" "${6:-}" >> "$TYPED"; }

# opened <node> <leg> <stdout-file> <label> — a URL the person OPENED: taken
# verbatim from the tool's `open   :` stdout line, never assembled here. The
# file rides along as provenance; the report re-reads it and counts the URL
# unless that line is there.
opened() {
  local url; url=$(sed -n 's/^ *open *: *\([^ ]*\).*/\1/p' "$3" | tail -1)
  typed "$1" "$2" "$url" "$4" "" "$3"
}

# ── weights: ring-doc's config, then a [models] table on a and b ────────────
eval "ring_doc_$(declare -f mkcfg)"
# `[models]` requires `primary`, so b names the same chat GGUF; b is never asked.
mkcfg() {
  ring_doc_mkcfg "$1"
  case "$1" in
    a|b|d|beefy|halo) printf '\n[models]\nprimary = "%s"\nembed = "%s"\n' "$CHAT_GGUF" "$EMBED_GGUF" >> "$D/$1/config.toml" ;;
  esac
  # The wall's guest door: its own bind on the room's WiFi, serving the ring
  # page out of a directory this instrument owns (the grant writes the QR into
  # it). Inserted INTO the `[daemon]` table ring_doc_mkcfg wrote — a second
  # `[daemon]` header would be a duplicate key and the daemon would refuse the
  # file. `client_bind` is untouched and stays loopback.
  [ "$1" = beefy ] && sed -i "/^\[daemon\]$/a guest_bind = \"${IP[beefy]}:$GUEST_PORT\"\nguest_page_dir = \"$PAGE\"" "$D/$1/config.toml"
  return 0
}

if command -v podman >/dev/null; then HOSTRUN=(); else HOSTRUN=(flatpak-spawn --host); fi

# ── the fourth machine: the same node door, one node more ───────────────────
# Its ports and address continue the door's own stride past the third, so none
# is written here. Its container is brought up by ring-doc-demo.sh's own
# `container_up_one` (the network is up, the three must stay), so the podman
# line and its HOME guard stay in one place.
if [ "$TOPOLOGY" = three ]; then
  CPORT[d]=$(( CPORT[c] * 2 - CPORT[b] )); IPORT[d]=$(( IPORT[c] * 2 - IPORT[b] )); DPORT[d]=$(( DPORT[c] * 2 - DPORT[b] ))
  IP[d]="${IP[c]%.*}.$(( ${IP[c]##*.} * 2 - ${IP[b]##*.} ))"
  PNET[d]=$NET
fi
declare -f container_up_one >/dev/null \
  || { echo "ring-room-demo: ring-doc-demo.sh has no container_up_one; the fourth node has no door" >&2; exit 3; }


# ── the room: four machines, two networks ───────────────────────────────────
# beefy  the ONE member in the room, on the room's WiFi AND the uplink: it
#        drives the wall, mints the grants, serves the guest door and is the
#        scheduler for every guest ask. It holds the host's render node.
# halo   a member elsewhere, uplink only: it holds the corpus the ask must
#        cite and is the keeper the doc converges back to.
# little a member elsewhere, uplink only: Jellyfin sits in its netns, exactly
#        as rr-1's holder does.
# phone  the room's WiFi ONLY, and NO daemon — curl and node. It is a guest:
#        no key, no membership, nothing installed.
# Cutting `uplink` from beefy is the venue's internet going down: the phones
# keep reaching the wall over `room`, and the mesh is gone.
if [ "$TOPOLOGY" = room ]; then
  ROOM_NET=room; UPLINK=uplink
  NETS=([$ROOM_NET]=10.89.60.0/24 [$UPLINK]=10.89.61.0/24)
  # The venue's WiFi reaches the wall and nothing else. Without this a podman
  # bridge NATs to the internet, beefy keeps its relay through the room while
  # "the uplink is cut", and the offline bar measures nothing.
  NET_FLAGS=([$ROOM_NET]="--internal")
  IP=([beefy]=10.89.60.11 [phone]=10.89.60.14 [halo]=10.89.61.12 [little]=10.89.61.13)
  PNET=([beefy]=$ROOM_NET [phone]=$ROOM_NET [halo]=$UPLINK [little]=$UPLINK)
  # The wall's second attachment. Its primary is the ROOM, so the forwarder
  # that stands in for the browser survives the cut the way the WiFi does.
  XNET=([beefy]="$UPLINK:ip=10.89.61.11")
  # The phone is a handset: a stock node image, none of this repo's toolchain,
  # no daemon, no forwarder, nothing to serve. Everything it does it does with
  # `fetch` against the door, exactly as a browser would.
  NODE_IMAGE=([phone]=docker.io/library/node:20-bookworm-slim)
  NOFWD=([phone]=1)
  CPORT=([beefy]=19941 [halo]=19951 [little]=19961 [phone]=19971)
  IPORT=([beefy]=19942 [halo]=19952 [little]=19962 [phone]=19972)
  DPORT=([beefy]=19949 [halo]=19959 [little]=19969 [phone]=19979)
  MESHNAME=([beefy]=BeefyMac [halo]=RuggedFox [little]=LittleMac)
  PERSON=([beefy]=beefy [halo]=halo [little]=little [phone]=phone)
  # The guest door's own bind, on the room's WiFi. Nothing else on beefy
  # leaves loopback: the client API stays shut, as it must on an encrypted
  # mesh (O2 (ii)).
  GUEST_PORT=19947
  DOOR="http://${IP[beefy]}:$GUEST_PORT/ring/"
  PAGE="$D/page"
  WALL_LOG="$D-wall.ndjson"
  PHONE_PKG="$REPO/scripts/ring-room-phone"
  # The two guests, named by themselves. Neither is ever a member.
  GUEST_ONE=Wren
  GUEST_TWO=Fen
  # The decision log and the stage ledger are custom tracing targets, so they
  # are dark unless named (glassbox allowlists). The ask bar reads served_by
  # off beefy's, which is why only beefy carries this.
  ROOM_RUST_LOG="info,mesh.decision=info,stage_attribution=info,transport=debug"
  # rr-2's legs; a leg left out reads COULD-NOT-JUDGE in its bars.
  ROOM_LEGS="${RING_ROOM_LEGS_ROOM:-wall,film,offline}"
  # The wall is the room's one machine and it is the machine with the GPU, so
  # it runs the model rr-1's answer bar was judged with (seat A22) rather than
  # the 2B floor: the ask bar reads a CITATION, and a released citation is
  # what the 2B did not produce here.
  CHAT_GGUF="${RING_ROOM_CHAT:-$REPO/sovereign/models/Qwen3.5-4B.Q6_K.gguf}"
fi

# A room daemon, with the decision log and the presence decision ON. Both sit
# on custom tracing targets, which are dark unless a filter names them — the
# ask bar reads served_by off one and the film bar's cause off the other.
# Everything else about it is ring-doc-demo.sh's `start_daemon`.
room_start_daemon() { # node
  node_bg "$1" "$D/$1/pid" "$D/$1/daemon.out" "$D/$1/daemon.err" env RUST_LOG="$ROOM_RUST_LOG" "$DAEMON" daemon run
}

# One node's whole ring journal as a content fingerprint, so two nodes'
# replicas can be compared without either one's paths appearing in it.
room_journal() { # node
  sv "$1" ring log "$RING" --json 2>/dev/null \
    | python3 -c "
import hashlib, json, sys
try: log = json.load(sys.stdin)
except Exception: print(''); raise SystemExit
ops = sorted((o.get('id') or '') + json.dumps(o.get('payload'), sort_keys=True) for o in log.get('ops') or [])
print(f\"{len(ops)} {hashlib.sha256(''.join(ops).encode()).hexdigest()[:16]}\")"
}

# The model the wall will grant, read from the daemon's own dispatchable list
# so no model id is written here.
room_model() {
  node_curl beefy -s --max-time 20 "$(at "${CPORT[beefy]}")/v1/models" 2>/dev/null \
    | python3 -c "import sys,json; print(((json.load(sys.stdin).get('data') or [{}])[0]).get('id') or '')" 2>/dev/null
}

# One wall grant and its QR. `--url` is the page the door serves; the bearer
# rides the FRAGMENT the builder puts it in, and the QR carries that link.
room_grant() { # label svg-path
  typed beefy wall "mesh grant --model $MODEL --rail $RING --ttl 2h --label $1 --url $DOOR --qr-svg $2" \
    "the wall's own grant, minted at the wall by the member standing there"
  node_exec beefy "$CLI" mesh grant --model "$MODEL" --rail "$RING" --ttl 2h --label "$1" \
    --url "$DOOR" --qr-svg "$2" > "$D/grant-$1.out" 2>&1
}

# A phone, in the phone container: nothing but node and the repo it reads the
# page's adapter out of. Its whole input is the SVG it scanned.
room_phone() { # label svg name edits ask collide
  node_exec phone env REPO="$REPO" QR_SVG="$2" NAME="$3" EDITS="$4" ASK="${5:-}" COLLIDE="${6:-}" LABEL="$1" \
    node "$PHONE_PKG/phone.mjs" > "$D/phone-$1.json" 2> "$D/phone-$1.err"
  # Everything it typed goes into the ONE census, under its own node label.
  python3 - "$D/phone-$1.json" "$1" <<'PY' >> "$TYPED"
import json, sys
try: p = json.load(open(sys.argv[1]))
except Exception: raise SystemExit
for t in p.get("typed") or []:
    print("\t".join(["walk", sys.argv[2], t.get("leg", ""), "", t.get("string", ""), t.get("note", ""), sys.argv[1]]))
PY
}

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
  # `--share` sets the corpus's query_sharing (corpus_store.rs creates it
  # local-only); the next gossip tick advertises it, no restart.
  typed "$n" "$leg" "corpus ingest --share $folder"
  node_exec "$n" "$CLI" corpus ingest --share "$folder" > "$D/room-ingest-$n.out" 2>&1
  rc=$?
  meta=$(find "$D/$n" -name _corpus_meta.json -path "*/$id/*" 2>/dev/null | head -1)
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
    es = a.get("epistemic_state") or {}
    cites = es.get("citations") or []
    # The per-claim path releases no citation; its evidence is a verified
    # holding's pool-level member. A release that checked no claim names nobody.
    checked = ((a.get("metadata") or {}).get("grounding_gate") or {}).get("claims_checked") or 0
    held = [((h.get("provenance") or {}).get("corpus") or {}).get("member")
            for h in es.get("holdings") or [] if h.get("verification") == "verified"] if checked else []
    qs.append({"q": q, "released": len(cites), "members": [c.get("member") for c in cites],
               "claims_checked": checked, "holding_members": held,
               "names_b": any(c.get("member") == bname for c in cites) or bname in held,
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
  for n in a b c; do opened "$n" doc "$D/$n/dev.out" "the ring-doc page, as ring dev printed it"; done
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
  local n=$1 leg=$2
  "${HOSTRUN[@]}" env CW_MEDIA_ROOT="$4" CW_MEDIA_NAME="$3" CW_MEDIA_NETWORK="container:$CPREFIX-$n" \
    bash "$MEDIA_SCRIPT" holder-up > "$D/room-holder-up-$n.out" 2>&1 || return 1
  typed "$n" "$leg" "demo / demo" "Jellyfin's own login (holder-setup's wizard, and the key it mints from it and declares) — the ONE excluded credential" secret
  typed "$n" "$leg" "mesh media offer" "holder-setup's one verb; it finds the origin itself"
  node_exec "$n" env SVRN="$CLI" CW_MEDIA_ROOT="$4" bash "$MEDIA_SCRIPT" holder-setup > "$D/room-holder-setup-$n.out" 2>&1
}

leg_film() {
  local out="$D/room-film.json"
  if [ "$BACKEND" != podman ]; then echo '{"skipped":"backend local: Jellyfin needs a node netns to sit in"}' > "$out"; return; fi
  local bname aname cname first narrowed url item fb
  bname=$(self_name b); aname=$(self_name a); cname=$(self_name c)
  cp "$D/b/config.toml" "$D/room-b-config.before"; cp "$D/c/config.toml" "$D/room-c-config.before"
  rm -rf "$MEDIA_ROOT/config" "$MEDIA_ROOT/cache" # a cold holder each run; the generated title is kept
  media_holder b film "$JELLY" "$MEDIA_ROOT" \
    || { echo '{"fatal":"holder-up failed, see room-holder-up-b.out"}' > "$out"; return; }
  # The clock starts when the verb returns: its own restart of b's daemon, and
  # b's return to the mesh, are inside the window.
  first=$(poll_offer "$bname" '[]' 30)
  typed b film "mesh media admit $aname $cname" "the narrowing, one verb; the origin is the stored one"
  node_exec b "$CLI" mesh media admit "$aname" "$cname" > "$D/room-narrow.out" 2>&1
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
  # The invite, as a shows it: `mesh status`'s `join link:` line, kept as the
  # provenance the census re-reads — the link is opened verbatim, the verb is typed.
  t0=$(date +%s)
  while [ $(( $(date +%s) - t0 )) -lt 30 ]; do
    sv a mesh status > "$D/room-a-status.out" 2>&1
    link=$(sed -n 's/^join link: //p' "$D/room-a-status.out"); [ -n "$link" ] && break; sleep 2
  done
  [ -n "$link" ] || { fatal "a's mesh status printed no join link inside 30 s"; return; }
  typed d join "mesh join" "the one verb; its argument is the invite below"
  typed d join "$link" "the invite a shows — scanned as a QR in the room (no renderer yet: rr-2)" "" "$D/room-a-status.out"
  node_exec d "$CLI" mesh join "$link" > "$D/room-join.out" 2>&1
  dname=$(self_name d)
  [ -n "$dname" ] || { fatal "the fourth's mesh status names no self after the join, see room-join.out"; return; }
  wait_online "$dname" 2> "$D/room-join-online.err" || { fatal "a never saw the fourth online"; return; }
  n_after=$(mesh_json a | python3 -c "$members")

  # (a) the doc: its page names it, and its edit is attributed on a.
  start_proxy d 2> "$D/room-join-proxy.err"
  opened d join "$D/d/dev.out" "the ring-doc page, as ring dev printed it"
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
# An empty `mesh status --json` from a is recorded as a null count and named,
# never an int('') crash that erases the doc and library legs with it.
unread = [f"a's mesh status --json printed nothing ({k})" for k, v in (("n_before", nb), ("n_after", na)) if not v]
json.dump({"name": name, "n_before": int(nb) if nb else None, "n_after": int(na) if na else None, "n_unread": unread, "doc": doc,
           "corpus": cid, "ingest_rc": int(rc), "shared_meta": bool(meta),
           "answered_s": float(answered) if answered else None, "asks": int(asks) + (1 if answered else 0),
           "listed_s": float(listed) if listed else None,
           "library": None if backend == "podman" else "backend local: Jellyfin needs a node netns to sit in"}, sys.stdout)
PY
}

# ── the room's bring-up ─────────────────────────────────────────────────────
# Three daemons and one daemon-less phone. The library's wizard runs HERE, at
# install: the walk that follows types no credential, which is what the film
# bar's clause (c) reads.
room_up() {
  need_binaries
  rm -rf "$D"; mkdir -p "$D"
  local n
  for n in beefy halo little; do mkcfg "$n"; done
  mkdir -p "$PAGE"; cp -R "$APP/." "$PAGE/"
  containers_up || return 3
  for n in beefy halo little; do room_start_daemon "$n"; done
  wait_all_up beefy halo little || return 3
  wait_homed beefy halo little || return 3
  for n in halo little; do join_one "$n" && wait_online "${MESHNAME[$n]}" || return 3; done
  sleep 12
  members_from_mesh 3 || return 3
  # The wall's own screen: the member's page, loopback, exactly as rr-1 runs it.
  start_proxy beefy || return 3
  case ",$ROOM_LEGS," in
    *,film,*)
      rm -rf "$MEDIA_ROOT/config" "$MEDIA_ROOT/cache"
      media_holder little film "$JELLY" "$MEDIA_ROOT" || return 3
      # holder-setup ends with its own `offer`; it is taken back here so the
      # WALK can be the verb the bar times.
      node_exec little "$CLI" mesh media withdraw > "$D/room-install-withdraw.out" 2>&1
      ;;
  esac
  echo "up (room): $(self_name beefy) in the room, $(self_name halo) and $(self_name little) elsewhere; wall page $(tab_url beefy)"
}

# The room's teardown. Named apart from rr-1's `room_down` below: two
# topologies, two node sets, and a teardown that reached for the wrong one
# would leave containers behind.
room_topology_down() {
  [ "$BACKEND" = podman ] && "${PODMAN[@]}" rm -f -t 2 "$JELLY" > /dev/null 2>&1
  node_kill beefy "$D/beefy/dev.pid"
  local n
  for n in beefy halo little; do node_kill "$n" "$D/$n/pid"; done
  sleep 1
  [ "$BACKEND" = podman ] && containers_down
  return 0
}

# ── the wall: one QR, two people, one question ──────────────────────────────
leg_room_wall() {
  local out="$D/room-wall.json" wallpid="" member q id rc meta seen="" t0
  mesh_json beefy > "$D/wall-beefy-before.json"; mesh_json halo > "$D/wall-halo-before.json"
  # The positive control, recorded BEFORE anything is ingested anywhere: the
  # wall does not hold this corpus, so an answer that cites it crossed the mesh.
  sv beefy corpus list > "$D/wall-beefy-corpora.txt"
  MODEL=$(room_model)
  [ -n "$MODEL" ] || { echo '{"fatal":"beefy lists no dispatchable model — nothing can be granted"}' > "$out"; return; }
  read -r id rc meta < <(share_folder halo wall "$BANK")
  ROOM_CORPUS=$id
  room_grant wall "$PAGE/wall-qr.svg"
  room_grant second "$D/wall-qr-2.svg"
  # The wall watches its own rail while the phones are in the room.
  : > "$WALL_LOG"
  REPO="$REPO" PA="$(tab_url beefy)" OUT="$WALL_LOG" POLL_MS=250 WATCH_S=1200 \
    node "$PHONE_PKG/wall.mjs" > "$D/wall-watch.out" 2>&1 &
  wallpid=$!
  member=$(self_name beefy)
  q=$(BANK="$BANK" python3 -c "import json,os; print(json.loads(os.environ['BANK'])[0][1])")
  # The wall must have heard that halo holds the corpus before the ask, or the
  # turn cannot route to it. Waited for, not scored (rr-1's rule).
  t0=$(date +%s)
  while [ $(( $(date +%s) - t0 )) -lt 180 ]; do
    node_curl beefy -s --max-time 10 -X POST "$(at "${CPORT[beefy]}")/v1/knowledge/search" \
      -H 'content-type: application/json' -d "{\"query\":\"Larkspur Lane cooperative\",\"corpora\":[\"$id\"]}" \
      > "$D/wall-fanout.json" 2>/dev/null
    grep -q '"peer_name"' "$D/wall-fanout.json" && { seen=$(( $(date +%s) - t0 )); break; }
    sleep 3
  done
  # The person: one scan, one name, one edit, one question.
  room_phone phone "$PAGE/wall-qr.svg" "$GUEST_ONE" 1 "$q" ""
  # The second person, who tries the wall's own member name first and is refused.
  room_phone phone2 "$D/wall-qr-2.svg" "$GUEST_TWO" 1 "" "$member"
  kill "$wallpid" 2>/dev/null
  sv beefy mesh grant --list > "$D/wall-grants.txt" 2>&1
  mesh_json beefy > "$D/wall-beefy-after.json"; mesh_json halo > "$D/wall-halo-after.json"
  # The peer path, from the daemon's EXISTING observation — nothing added for
  # the bar (A36): `/v1/mesh/status` already publishes `iroh_transport`.
  node_curl beefy -s --max-time 10 "$(at "${CPORT[beefy]}")/v1/mesh/status" > "$D/wall-transport.json" 2>/dev/null
  grep -E 'routing outcome|routing decision|guest_ask: accepted|turn stage attribution' "$D/beefy/daemon.err" \
    > "$D/wall-decisions.txt" 2>/dev/null
  # The keeper's replica: the guests' acts, byte for byte, after its next sync.
  local jb jh="" sync="" t1
  t1=$(date +%s)
  while [ $(( $(date +%s) - t1 )) -lt 120 ]; do
    jb=$(room_journal beefy); jh=$(room_journal halo)
    [ -n "$jh" ] && [ "$jh" = "$jb" ] && { sync=$(( $(date +%s) - t1 )); break; }
    sleep 3
  done
  printf '%s\n%s\n%s\n' "${jb:-}" "${jh:-}" "${sync:-}" > "$D/wall-replica.txt"
  python3 - "$D" "$member" "$id" "$rc" "${meta:-}" "${seen:-}" "$MODEL" "$q" "$(self_name halo)" > "$out" <<'PY'
import json, os, sys
d, member, cid, rc, meta, seen, model, q, keeper = sys.argv[1:10]
listed = open(os.path.join(d, "wall-beefy-corpora.txt")).read()
def members(p):
    try: return sorted(m["name"] for m in json.load(open(os.path.join(d, p)))["members"])
    except Exception: return None
json.dump({"member": member, "keeper": keeper, "model": model, "question": q, "corpus": cid,
           "ingest_rc": int(rc), "shared_meta": bool(meta),
           "absent_on_beefy": cid not in listed,
           "beefy_heard_s": int(seen) if seen else None,
           "members_before": {"beefy": members("wall-beefy-before.json"), "halo": members("wall-halo-before.json")},
           "members_after": {"beefy": members("wall-beefy-after.json"), "halo": members("wall-halo-after.json")},
           "grants": open(os.path.join(d, "wall-grants.txt")).read()}, sys.stdout)
PY
}

# ── the film: LittleMac's library, offered, watched, withdrawn ──────────────
# Jellyfin's own API is reached from inside little, where it listens on
# loopback — the same door the holder's daemon uses.
jelly() { # method path [body]  → stdout, on little
  local m=$1 p=$2 body="${3:-}" auth="MediaBrowser Client=\"ring-room-demo\", Device=\"cli\", DeviceId=\"ring-room-demo\", Version=\"1\""
  [ -n "${JELLY_TOKEN:-}" ] && auth="$auth, Token=\"$JELLY_TOKEN\""
  if [ -n "$body" ]; then
    node_curl little -sS --max-time 15 -X "$m" "http://127.0.0.1:8096$p" \
      -H 'Content-Type: application/json' -H "Authorization: $auth" -d "$body"
  else
    node_curl little -sS --max-time 15 -X "$m" "http://127.0.0.1:8096$p" -H "Authorization: $auth"
  fi
}

leg_room_film() {
  local out="$D/room-film.json"
  if [ "$BACKEND" != podman ]; then echo '{"skipped":"backend local: Jellyfin needs a node netns to sit in"}' > "$out"; return; fi
  local lname bname first="" url="" item="" fb="" pick_s="" gone="" inuse_s="" inuse_text=""
  lname=$(self_name little); bname=$(self_name beefy)
  cp "$D/little/config.toml" "$D/room-little-config.before"; cp "$D/beefy/config.toml" "$D/room-beefy-config.before"
  # (a) ONE verb on the holder, and the clock starts when it returns.
  typed little film "mesh media offer" "the holder's one verb; it finds the origin and mints the read-only viewer itself"
  node_exec little "$CLI" mesh media offer > "$D/room-offer.out" 2>&1
  OFFERS_NODE=beefy; first=$(room_poll_offer "$lname" '[]' 30)
  # (b) the pick, as the rail makes it.
  local pick_t0 deadline
  pick_t0=$(date +%s.%N); deadline=$(( $(date +%s) + 180 ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    url=$(node_curl beefy -s --max-time 15 "$(at "${CPORT[beefy]}")/v1/mesh/media?peer=$lname" \
      | python3 -c "import sys,json; print(json.load(sys.stdin).get('url') or '')" 2>/dev/null)
    [ -n "$url" ] || { sleep 3; continue; }
    node_curl beefy -s --max-time 20 -o /dev/stdout -w '%{stderr}%{http_code} %{time_total}\n' \
      "$url/Items?Recursive=true&IncludeItemTypes=Movie" > "$D/room-items.json" 2>> "$D/room-items.codes"
    item=$(python3 -c "import sys,json; print((json.load(open(sys.argv[1])).get('Items') or [{}])[0].get('Id') or '')" "$D/room-items.json" 2>/dev/null)
    [ -n "$item" ] && break
    sleep 3
  done
  if [ -n "$item" ]; then
    fb=$(node_curl beefy -s -o /dev/null -r 0-65535 --max-time 30 -w '%{time_starttransfer} %{http_code}' \
      "$url/Videos/$item/stream?static=true")
    pick_s=$(python3 -c "import sys; print(round(float(sys.argv[1]) - float(sys.argv[2]), 2))" "$(date +%s.%N)" "$pick_t0")
  fi
  # (f) the holder watches their own library. This is INSTALL stage on purpose:
  # it is the holder using their own machine, the CONDITION the bar measures,
  # not a step anyone walks — and the walk's census stays credential-free.
  local was=$STAGE; STAGE=install
  JELLY_TOKEN=$(jelly POST /Users/AuthenticateByName '{"Username":"demo","Pw":"demo"}' \
    | sed -n 's/.*"AccessToken":"\([^"]*\)".*/\1/p')
  typed little film "demo / demo" "the holder's own login to their own library, to press play" secret
  if [ -n "$JELLY_TOKEN" ] && [ -n "$item" ]; then
    jelly POST /Sessions/Playing "{\"ItemId\":\"$item\",\"PlayMethod\":\"DirectPlay\",\"CanSeek\":true,\"PositionTicks\":0}" \
      > "$D/room-playing.out" 2>&1
    jelly GET /Sessions > "$D/room-sessions.json" 2>&1
  fi
  STAGE=$was
  inuse_s=$(room_poll_available "$lname" 0 120)
  sv beefy mesh media > "$D/room-rail.txt" 2>&1
  inuse_text=$(grep -c 'in use right now' "$D/room-rail.txt")
  was=$STAGE; STAGE=install
  [ -n "$JELLY_TOKEN" ] && jelly POST /Sessions/Playing/Stopped "{\"ItemId\":\"$item\",\"PositionTicks\":0}" > /dev/null 2>&1
  # (e) the policy of the account the offer declared, read back from Jellyfin
  # itself rather than from the verb's own word for it.
  jelly GET /Users > "$D/room-users.json" 2>&1
  STAGE=$was
  # (g) the inverse verb.
  typed little film "mesh media withdraw" "the holder's other verb; the offer is gone within a gossip round"
  node_exec little "$CLI" mesh media withdraw > "$D/room-withdraw.out" 2>&1
  gone=$(room_poll_gone "$lname" 120)
  cp "$D/little/config.toml" "$D/room-little-config.after"; cp "$D/beefy/config.toml" "$D/room-beefy-config.after"
  python3 - "$D" "$lname" "${first:-}" "$url" "${item:-}" "${fb:-}" "${pick_s:-}" "${inuse_s:-}" "$inuse_text" "${gone:-}" > "$out" <<'PY'
import difflib, json, os, sys
d, lname, first, url, item, fb, pick, inuse, inuse_text, gone = sys.argv[1:11]
def diff(n):
    a = open(f"{d}/room-{n}-config.before").read().splitlines()
    b = open(f"{d}/room-{n}-config.after").read().splitlines()
    return [l for l in difflib.unified_diff(a, b, lineterm="", n=0) if l[:1] in "+-" and l[:3] not in ("+++", "---")]
t, code = (fb.split() + ["", ""])[:2] if fb else ("", "")
offer_out = open(f"{d}/room-offer.out").read()
# The declared account's policy, read back from Jellyfin. The offer verb names
# the user it created; every non-admin user with playback and no management is
# read-only, and the one the verb made is the one that must be.
try:
    users = json.load(open(f"{d}/room-users.json"))
except Exception:
    users = []
MANAGE = ("EnableContentDeletion", "EnableCollectionManagement", "EnableSubtitleManagement", "EnableLiveTvManagement")
policies = [{"name": u.get("Name"), "id": u.get("Id"),
             "admin": (u.get("Policy") or {}).get("IsAdministrator"),
             "manages": sorted(k for k in MANAGE if (u.get("Policy") or {}).get(k))}
            for u in (users if isinstance(users, list) else [])]
viewers = [p for p in policies if p["name"] != "demo"]
try:
    sessions = json.load(open(f"{d}/room-sessions.json"))
    holder_playing = any(s.get("NowPlayingItem") for s in sessions)
except Exception:
    holder_playing = None
json.dump({"holder": lname, "listed_s": float(first) if first else None,
           "player_url": bool(url), "item": bool(item),
           "stream_first_byte_s": float(t) if t else None,
           "pick_to_first_byte_s": float(pick) if pick else None, "http": code,
           "viewer_declared": "read-only account" in offer_out,
           "viewer_policies": viewers, "holder_playing": holder_playing,
           "in_use_s": float(inuse) if inuse else None, "in_use_lines": int(inuse_text or 0),
           "withdrawn_s": float(gone) if gone else None,
           "little_config_diff": diff("little"), "beefy_config_diff": diff("beefy")}, sys.stdout)
PY
}

# The rail as the viewer reads it. Three polls, one shape: `GET /v1/mesh/media`
# on the viewer, which is what `svrn mesh media` and the desktop Library rail
# both read.
room_offers() { node_curl "${OFFERS_NODE:-beefy}" -s --max-time 10 "$(at "${CPORT[${OFFERS_NODE:-beefy}]}")/v1/mesh/media"; }
room_poll_until() { # window-s predicate-python peer arg
  local t0 now
  t0=$(date +%s.%N)
  while :; do
    now=$(date +%s.%N)
    python3 -c "import sys; sys.exit(0 if float(sys.argv[1]) - float(sys.argv[2]) <= float(sys.argv[3]) else 1)" "$now" "$t0" "$1" || return 0
    room_offers > "$D/room-offers.json" 2>/dev/null
    if python3 -c "$2" "$D/room-offers.json" "$3" "${4:-}" 2>/dev/null; then
      python3 -c "import sys; print(round(float(sys.argv[1]) - float(sys.argv[2]), 2))" "$(date +%s.%N)" "$t0"
      return 0
    fi
    sleep "$POLL_S"
  done
}
room_poll_offer() { # holder offered-to-json window
  room_poll_until "$3" "
import json, sys
o = [x for x in json.load(open(sys.argv[1])).get('offering', []) if x.get('peer') == sys.argv[2]]
sys.exit(0 if o and sorted(o[0].get('offered_to') or []) == sorted(json.loads(sys.argv[3])) else 1)" "$1" "$2"
}
room_poll_available() { # holder value window
  room_poll_until "$3" "
import json, sys
o = [x for x in json.load(open(sys.argv[1])).get('offering', []) if x.get('peer') == sys.argv[2]]
sys.exit(0 if o and o[0].get('media_available') == float(sys.argv[3]) else 1)" "$1" "$2"
}
room_poll_gone() { # holder window
  room_poll_until "$2" "
import json, sys
sys.exit(0 if not [x for x in json.load(open(sys.argv[1])).get('offering', []) if x.get('peer') == sys.argv[2]] else 1)" "$1"
}

# ── the cut: the venue's internet goes down ─────────────────────────────────
# Only the uplink. The room's WiFi stays up, so the phones keep reaching the
# wall — which is exactly what the bar's goodhart demands be true.
leg_room_offline() {
  local out="$D/room-offline.json" q wallpid before_beefy after_beefy after_halo t0 conv=""
  q=$(BANK="$BANK" python3 -c "import json,os; print(json.loads(os.environ['BANK'])[1][1])")
  : > "$D-wall-cut.ndjson"
  REPO="$REPO" PA="$(tab_url beefy)" OUT="$D-wall-cut.ndjson" POLL_MS=250 WATCH_S=900 \
    node "$PHONE_PKG/wall.mjs" > "$D/wall-watch-cut.out" 2>&1 &
  wallpid=$!
  cut_node beefy "$UPLINK" > "$D/room-cut.out" 2>&1
  sleep 10
  room_phone phone-cut "$PAGE/wall-qr.svg" "$GUEST_ONE" 1 "$q" ""
  kill "$wallpid" 2>/dev/null
  before_beefy=$(room_journal beefy)
  heal_node beefy "$UPLINK" > "$D/room-heal.out" 2>&1
  t0=$(date +%s)
  while [ $(( $(date +%s) - t0 )) -lt 120 ]; do
    after_halo=$(room_journal halo); after_beefy=$(room_journal beefy)
    [ -n "$after_halo" ] && [ "$after_halo" = "$after_beefy" ] && { conv=$(( $(date +%s) - t0 )); break; }
    sleep 3
  done
  python3 - "${before_beefy:-}" "${after_beefy:-}" "${after_halo:-}" "${conv:-}" "$q" > "$out" <<'PY'
import json, sys
before, after_b, after_h, conv, q = sys.argv[1:6]
json.dump({"question": q, "beefy_at_cut": before, "beefy_after": after_b, "halo_after": after_h,
           "converged_s": int(conv) if conv else None,
           "byte_equal": bool(after_h) and after_h == after_b}, sys.stdout)
PY
}

# ── the census + the five rows ───────────────────────────────────────────────
report() { # bar|all
  python3 - "$D" "$ROOM_CAMPAIGN" "$1" "$TYPED" "$CMDLOG" "$ROOM_SCRIPT" "${CPORT[*]} ${IPORT[*]} ${DPORT[*]}" "${IP[*]}" "$TOPOLOGY" "${IP[phone]:-}" <<'PY'
import json, os, re, subprocess, sys, tomllib
d, campaign, want, typed_p, cmdlog_p, script_p, ports, ips, topology, phone_ip = sys.argv[1:11]
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
    stage, node, leg, secret, s, note, prov = (line.split("\t") + [""] * 7)[:7]
    # `opened`: the string came VERBATIM from a tool's stdout line, and that
    # line is re-read here rather than trusted. Assembled, or no such line: counts.
    src = next((l.strip() for l in (open(prov).read().splitlines() if prov and os.path.exists(prov) else [])
                if s and s in l.split()), "")
    entries.append(dict(stage=stage, node=node, leg=leg, string=s, note=note,
                        cls="opened" if src else classify(s, secret), src=src and f"{prov}: {src}",
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
    if e["src"]: print(f"        from the tool's stdout, verbatim: {e['src']}")
print(f"  walk count: {walk_count}")
print(f"== census: this script names {len(hits)} of {len(needles)} member names/ports/addresses/corpus ids: {hits} ==")

rows = {}
def row(bar, value, reason="", **extra): rows[bar] = dict(bar=bar, value=value, reason=reason, **extra)
num = lambda pat, s: float(re.search(pat, s).group(1)) if re.search(pat, s) else None

# rr-1's five. They read artifacts the ROOM topology does not write (and
# writes differently), so they are built only for the topology they measure —
# four verdicts, not a traceback (ARCH 5).
if topology != "room":
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
            questions=[{k: q.get(k) for k in ("released", "members", "claims_checked", "holding_members", "verdict", "answer_has", "error")} for q in qs])

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
        # Every key `svrn mesh media offer` writes, and nothing else. This
        # clause asks whether the HOLDER'S config moved under ONE VERB rather
        # than under a person's editor — so it tracks the verb's key set, and
        # the verb gained `media_viewer_user` in rr-2-media-posture
        # (ee1c6b388: the read-only account's id, which the presence poll
        # needs to tell the holder's own sessions from the house's). What the
        # clause discriminates is unchanged: any other key, and node c's
        # config moving at all (`c_config_diff`), still fail.
        verb_keys = re.compile(r'^[+-]\s*media_(origin|allow|viewer_user)\s*=')
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
                "d_n_from_mesh_only": None not in (j["n_before"], j["n_after"]) and j["n_after"] == j["n_before"] + 1 and not hits}
        row("ra-room-plug-in-live", 1.0 if all(legs.values()) else 0.0, "", legs=legs, window_s=win,
            **{k: j.get(k) for k in ("name", "n_before", "n_after", "n_unread", "listed_s", "answered_s", "asks", "library")},
            doc={k: doc.get(k) for k in ("d_self", "a_name", "a_s", "a_roster", "fatal", "errors")})

    # 5 — nothing typed
    row("ra-room-nothing-typed", walk_count if walk else None, "" if walk else "no leg typed anything: the walk did not run",
        walk=[f"{e['leg']}/{e['node']} {e['cls']}: {e['string']}" for e in walk if e["cls"] in COUNTED and not e["excluded"]],
        excluded=[f"{e['string']} — {e['note']}" for e in walk if e["excluded"]],
        install_counted=install_count, script_names=hits)

# ── rr-2: the room's six ────────────────────────────────────────────────────
# Every clause below is a sentence from the bar's `floor_basis` in
# quality/campaigns/ring-room.toml, judged from what the run recorded. Where a
# clause could only be read one way by a machine, the reading is named in the
# row so it can be argued with.
def phone(label):
    return load(f"phone-{label}.json") or {}

def sightings(path):
    """The wall's NDJSON sightings. `path` is absolute: the log sits BESIDE
    the artifact directory, which bring-up empties."""
    rows = []
    for line in (open(path).read().splitlines() if os.path.exists(path) else []):
        try: rows.append(json.loads(line))
        except Exception: pass
    return rows

def act_at(p, leg):
    return next((a for a in p.get("acts") or [] if a.get("leg") == leg), None)

def seen_after(rows, act):
    """When the wall first showed this act, and the name it showed."""
    if not act: return None, None
    hit = next((r for r in rows if r.get("id") == act["id"]), None)
    if not hit: return None, None
    return round(hit["at"] - act["at"], 3), hit.get("name")

def cited_members(es):
    es = es or {}
    out = [c.get("member") for c in es.get("citations") or []]
    out += [((h.get("provenance") or {}).get("corpus") or {}).get("member")
            for h in es.get("holdings") or [] if h.get("verification") == "verified"]
    return [m for m in out if m]

def logs_matching(node, pattern):
    """Lines in a node's daemon log matching `pattern`. Absent log: None, which
    is not zero — a check that never ran says so (ARCH 6)."""
    p = os.path.join(d, node, "daemon.err")
    if not os.path.exists(p): return None
    rx = re.compile(pattern)
    return sum(1 for l in open(p, errors="replace") if rx.search(l))

if topology == "room":
    w = load("room-wall.json") or {}
    p1, p2, pc = phone("phone"), phone("phone2"), phone("phone-cut")
    wall_rows = sightings(d + "-wall.ndjson")
    cut_rows = sightings(d + "-wall-cut.ndjson")
    member = w.get("member")
    # The keeper's name as IT reports itself, never a name written here.
    halo_name = w.get("keeper")
    # MEASURED 2026-09-19: no daemon logs a caller's address, and every node
    # logs `accepting GUEST_ALPN` for the ephemeral iroh guest surface, which
    # has nothing to do with the phone. So the needles are the DOOR's own
    # lines — the only ones a phone can produce.
    DOOR_LINE = r"guest door:|guest_ask:"
    guest_mentions = {n: {"door_lines": logs_matching(n, DOOR_LINE),
                          "accepted": logs_matching(n, r"guest_ask: accepted"),
                          "address": logs_matching(n, re.escape(phone_ip)) if phone_ip else None}
                      for n in ("beefy", "halo", "little")}

    # 1 — one scan, and a name on the wall
    b = bars["ra-room-scan-to-name"]
    name_w = num(r"within (\d+) s", b["floor_basis"])
    if w.get("fatal") or not p1:
        row("ra-room-scan-to-name", None, w.get("fatal") or "the wall leg did not run")
    else:
        named = act_at(p1, "wall")
        seen_s, seen_name = seen_after(wall_rows, named)
        # (d) reads the WALL leg's strings: the step this bar is about. The
        # doc words and the question belong to the bars that measure them,
        # and every one of the phone's strings is listed in the row.
        phone_wall = [e for e in walk if e["node"] == "phone" and e["leg"] == "wall"]
        phone_counted = [e for e in walk if e["node"].startswith("phone") and e["cls"] in COUNTED]
        legs = {"a_guest_grant_and_members_unchanged":
                    bool(p1.get("link")) and not p1.get("token_in_query")
                    and "rail:" in (p1.get("summary") or "")
                    and (w.get("model") or "\0") in (p1.get("summary") or "")
                    and (p1.get("expires_at") or 0) > 0
                    and w.get("members_before") == w.get("members_after"),
                # (b) reads the two things a log here can carry: the door's
                # own accepted-guest lines on the wall, and any trace of the
                # phone anywhere else. No daemon logs a caller's address, so
                # the address term is a negative check only.
                "b_only_the_wall_saw_the_phone":
                    (guest_mentions["beefy"]["accepted"] or 0) > 0
                    and all(guest_mentions[n]["door_lines"] == 0 and guest_mentions[n]["address"] == 0
                            for n in ("halo", "little")),
                "c_name_on_the_wall": seen_s is not None and name_w is not None and seen_s <= name_w,
                "d_phone_typed_one_string": len(phone_wall) == 1 and not phone_counted}
        row("ra-room-scan-to-name", 1.0 if all(legs.values()) else 0.0, "", legs=legs,
            window_s=name_w, name_seen_s=seen_s, wall_showed=seen_name, summary=p1.get("summary"),
            quiet_zone=(p1.get("qr") or {}).get("quiet_zone"), page=p1.get("page"),
            phone_typed=[f"{e['leg']}: {e['string']}" for e in walk if e["node"].startswith("phone")],
            log_mentions_of_the_phone=guest_mentions, members=w.get("members_after"))

    # 2 — the guest's edit, and never mistakable for a member
    b = bars["ra-room-guest-edit-attributed"]
    edit_w = num(r"within ([\d.]+) s", b["floor_basis"])
    rail_diff = subprocess.run(["git", "diff", "--stat", "origin/main", "--",
                                "commonwealth/crates/commonwealth-rail",
                                "commonwealth/crates/commonwealth-rail-core"],
                               capture_output=True, text=True, cwd=os.path.dirname(script_p) + "/..").stdout.strip()
    replica = (open(os.path.join(d, "wall-replica.txt")).read().splitlines() + ["", "", ""])[:3] \
        if os.path.exists(os.path.join(d, "wall-replica.txt")) else ["", "", ""]
    if not p1 or p1.get("fatal"):
        row("ra-room-guest-edit-attributed", None, (p1 or {}).get("fatal") or "no phone reached the wall")
    else:
        edit = act_at(p1, "edit")
        edit_s, line = seen_after(wall_rows, edit)
        legs = {"a_on_the_wall_in_time": edit_s is not None and edit_w is not None and edit_s <= edit_w,
                "b_named_a_guest_not_a_member": bool(line) and (p1.get("typed") or [{}])[0].get("string", "") in (line or "")
                                                and "guest" in (line or "") and bool(member) and member in (line or ""),
                "c_the_rail_is_untouched": rail_diff == "",
                "d_in_the_keepers_replica": bool(replica[1]) and replica[0] == replica[1]}
        row("ra-room-guest-edit-attributed", 1.0 if all(legs.values()) else 0.0, "", legs=legs,
            window_s=edit_w, edit_seen_s=edit_s, wall_showed=line, signer=member,
            rail_diff=rail_diff, replica={"beefy": replica[0], "halo": replica[1], "synced_s": replica[2]},
            collision=p2.get("collision"))

    # 3 — the room answered, and named the machine it came from
    b = bars["ra-room-guest-ask-served-by-the-room"]
    ask_w = num(r"within (\d+) s", b["floor_basis"])
    ask = (p1 or {}).get("ask") or {}
    es = ask.get("epistemic_state") or {}
    decisions = open(os.path.join(d, "wall-decisions.txt")).read().splitlines() \
        if os.path.exists(os.path.join(d, "wall-decisions.txt")) else []
    try:
        transport = json.load(open(os.path.join(d, "wall-transport.json"))).get("iroh_transport") or []
    except Exception:
        transport = []
    if not ask:
        row("ra-room-guest-ask-served-by-the-room", None, "the phone never asked (see phone-phone.json)")
    else:
        cites = cited_members(es)
        legs = {"a_the_wall_never_held_it": bool(w.get("absent_on_beefy")),
                "b_the_evidence_names_the_keeper": bool(halo_name) and halo_name in cites,
                "c_answered_in_time": ask.get("answered_s") is not None and ask_w is not None
                                      and ask["answered_s"] <= ask_w,
                "d_served_by_and_the_path": any("served_by" in l for l in decisions) and bool(transport)}
        row("ra-room-guest-ask-served-by-the-room", 1.0 if all(legs.values()) else 0.0, "", legs=legs,
            window_s=ask_w, answered_s=ask.get("answered_s"), corpus=w.get("corpus"),
            keeper=halo_name, cited_members=cites, sources=ask.get("sources"),
            verdict_of_turn=es.get("verdict"), error=ask.get("error"),
            # The answer's own words, so a run that read 1.0 on a decoration
            # can be argued with.
            answer=(ask.get("answer") or "")[:400],
            decision_lines=decisions[-4:], peer_paths=transport)

    # 4 — the film from LittleMac
    f = load("room-film.json") or {}
    fb_basis = bars["ra-room-film-from-littlemac"]["floor_basis"]
    list_w, byte_w = num(r"within (\d+) s", fb_basis), num(r"within ([\d.]+) s over MEDIA_ALPN", fb_basis)
    if not f or f.get("skipped") or f.get("fatal"):
        row("ra-room-film-from-littlemac", None, f.get("skipped") or f.get("fatal") or "the film leg did not run")
    else:
        verb_keys = re.compile(r'^[+-]\s*(media_(origin|allow|viewer\w*)|viewer\w*)\s*=')
        film_creds = [e for e in walk if e["leg"] == "film" and e["cls"] == "credential"]
        legs = {"a_listed_with_offered_to": f["listed_s"] is not None and list_w is not None and f["listed_s"] <= list_w,
                "b_first_byte": f["pick_to_first_byte_s"] is not None and f["http"] in ("200", "206")
                                and byte_w is not None and f["pick_to_first_byte_s"] <= byte_w,
                "c_no_credential_in_the_walk": not film_creds,
                "d_holder_config_untouched": all(verb_keys.match(l) for l in f["little_config_diff"])
                                             and not f["beefy_config_diff"],
                "e_the_declared_account_is_read_only": bool(f.get("viewer_policies"))
                                                      and all(p["admin"] is False and not p["manages"]
                                                              for p in f["viewer_policies"]),
                "f_in_use_and_nothing_started": f.get("in_use_s") is not None and f.get("in_use_lines", 0) > 0,
                "g_withdrawn_within_a_round": f.get("withdrawn_s") is not None}
        row("ra-room-film-from-littlemac", 1.0 if all(legs.values()) else 0.0, "", legs=legs,
            windows={"listed_s": list_w, "first_byte_s": byte_w},
            **{k: f.get(k) for k in ("holder", "listed_s", "pick_to_first_byte_s", "stream_first_byte_s", "http",
                                     "viewer_declared", "viewer_policies", "holder_playing", "in_use_s",
                                     "in_use_lines", "withdrawn_s", "little_config_diff", "beefy_config_diff")})

    # 5 — the room says what it lost
    o = load("room-offline.json") or {}
    off_w = num(r"within (\d+) s", bars["ra-room-offline-room-says-so"]["floor_basis"])
    cut_ask = (pc or {}).get("ask") or {}
    cut_es = cut_ask.get("epistemic_state") or {}
    if not o or not pc:
        row("ra-room-offline-room-says-so", None, "the offline leg did not run")
    elif pc.get("fatal"):
        row("ra-room-offline-room-says-so", 0.0, f"the phone could not reach the wall during the cut: {pc['fatal']}")
    else:
        cut_edit = act_at(pc, "edit")
        cut_s, cut_line = seen_after(cut_rows, cut_edit)
        cut_cites = cited_members(cut_es)
        # (b), read the only way a machine can: the release names nobody it
        # could not reach, and it says something is missing — a gap, or a
        # verdict that is not a plain grounded release.
        says_so = bool(cut_es.get("gaps")) or cut_es.get("verdict") not in (None, "released", "Released")
        legs = {"a_the_doc_kept_working": cut_s is not None and edit_w is not None and cut_s <= edit_w
                                          and bool(cut_line) and "guest" in (cut_line or ""),
                "b_no_citation_of_the_keeper_and_it_says_so":
                    (not halo_name or halo_name not in cut_cites) and says_so,
                "c_byte_equal_after_the_return": bool(o.get("byte_equal"))
                                                 and o.get("converged_s") is not None
                                                 and off_w is not None and o["converged_s"] <= off_w}
        row("ra-room-offline-room-says-so", 1.0 if all(legs.values()) else 0.0, "", legs=legs,
            window_s=off_w, edit_seen_s=cut_s, wall_showed=cut_line,
            cited_during_the_cut=cut_cites, gaps=len(cut_es.get("gaps") or []),
            turn_verdict=cut_es.get("verdict"), answer=(cut_ask.get("answer") or "")[:400],
            converged_s=o.get("converged_s"), journals={"beefy": o.get("beefy_after"), "halo": o.get("halo_after")})

    # 6 — nobody became a member by scanning
    scans = [x for x in (p1, p2, pc) if x.get("link")]
    grants = (w.get("grants") or "")
    refusals = {k: {"other_namespace": x.get("other_namespace_status"),
                    "conversations": x.get("conversations_status"),
                    "mesh_status": x.get("mesh_status_status")}
                for k, x in (("phone", p1), ("phone2", p2)) if x}
    refused = lambda v: isinstance(v, int) and v in (401, 403, 404)
    if len(scans) < 2:
        row("ra-room-member-only-by-vouch", None, f"the negative half needs two scans; {len(scans)} happened")
    else:
        legs = {"a_member_sets_unchanged": bool(w.get("members_before")) and w["members_before"] == w["members_after"],
                "b_the_grants_are_listed_with_their_expiry":
                    len([l for l in grants.splitlines() if re.search(r"\blive\b.*\d+[hm]", l)]) >= 2,
                "c_no_other_door": all(refused(v) for r in refusals.values() for v in r.values())}
        row("ra-room-member-only-by-vouch", 1.0 if all(legs.values()) else 0.0, "", legs=legs,
            scans=len(scans), members=w.get("members_after"), refusals=refusals,
            grants=grants.strip().splitlines()[-6:],
            # ARCH 5: the half that cannot be judged says so rather than
            # riding on the half that can.
            affirmative_half="COULD-NOT-JUDGE: the phone runs no node here, so `introduce` cannot be "
                             "observed making one a member (ring-apps ra-11's measurement first)")

order = ([b["id"] for b in tomllib.load(open(campaign, "rb"))["bar"] if b.get("rung") == "rr-2"]
         if topology == "room" else
         ["ra-room-answer-names-the-machine", "ra-room-doc-name-from-membership",
          "ra-room-film-from-the-library-rail", "ra-room-plug-in-live", "ra-room-nothing-typed"])
for bar in (order if want == "all" else [want]):
    r, b = rows[bar], bars[bar]
    v = r["value"]
    verdict = "COULD-NOT-JUDGE" if v is None else (
        ("PASSED" if v <= b["floor"] else "FAILED") if b["direction"] == "lower_is_better" else
        ("PASSED" if v >= b["floor"] else "FAILED"))
    r.update(floor=b["floor"], verdict=verdict, artifact=d,
             topology=("four podman nodes on two networks, one host: the wall in the room, "
                       "the keeper and the holder behind an uplink, the phone on the room's WiFi only"
                       if topology == "room" else
                       f"three {os.environ.get('RING_DOC_BACKEND')} nodes, one host"))
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
  up)   : > "$CMDLOG"; : > "$TYPED"; if [ "$TOPOLOGY" = room ]; then room_up; else cmd_up; fi ;;
  down) if [ "$TOPOLOGY" = room ]; then room_topology_down; else room_down; fi; echo "stopped" ;;
  verdict)
    python3 -c "import sys,tomllib; ids=[b['id'] for b in tomllib.load(open(sys.argv[1],'rb'))['bar']]; sys.exit(0 if sys.argv[2] in ids+['all'] else 1)" \
      "$ROOM_CAMPAIGN" "${2:-}" || { echo "verdict: name a bar or all — see quality/campaigns/ring-room.toml" >&2; exit 2; }
    need_binaries
    : > "$CMDLOG"; : > "$TYPED"
    if [ "$TOPOLOGY" = room ]; then
      trap room_topology_down EXIT
      room_up > "$D-up.log" 2>&1 || { echo "bring-up failed, see $D-up.log" >&2; exit 3; }
      STAGE=walk
      for leg in ${ROOM_LEGS//,/ }; do "leg_room_$leg" > "$D-$leg.log" 2>&1; done
    else
      trap room_down EXIT
      cmd_up > "$D-up.log" 2>&1 || { echo "bring-up failed, see $D-up.log" >&2; exit 3; }
      STAGE=walk
      for leg in ${RING_ROOM_LEGS//,/ }; do "leg_$leg" > "$D-$leg.log" 2>&1; done
    fi
    report "$2"
    ;;
  *) sed -n '2,53p' "$0"; exit 2 ;;
esac
