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
# floors and directions READ from the three campaign files (the-link,
# ring-guest, ring-room).
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
# The room's own six are the REGRESSION set and are read from the file above,
# never edited. `ring-guest`'s five and `the-link`'s three are read from their
# own files: three campaigns, one run, one `verdict all`.
GUEST_CAMPAIGN="$REPO/quality/campaigns/ring-guest.toml"
LINK_CAMPAIGN="$REPO/quality/campaigns/the-link.toml"
# The commit the ring-guest campaign starts from. Two bars read a diff against
# it — the scaffold's, which must be empty, and the rail's, which is the one
# row the operator opened (D1).
GUEST_BASE=f51b66112
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
# The eighth column is the ELEMENT that took the string — the door's own
# prompt, a field on an app's page, or nothing when the instrument typed it.
typed() { printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$STAGE" "$1" "$2" "${5:-}" "$3" "${4:-}" "${6:-}" "${7:-}" >> "$TYPED"; }

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
  # The wall's guest door: its own bind on the room's WiFi. Inserted INTO the
  # `[daemon]` table ring_doc_mkcfg wrote — a second `[daemon]` header would be
  # a duplicate key and the daemon would refuse the file. `client_bind` is
  # untouched and stays loopback.
  #
  # `guest_page_dir` is DELIBERATELY unset. It is the one-app spelling — the
  # page at the bare `/ring/` — and with it set the door would serve an app
  # there instead of the wall's index, and one QR could reach only that one.
  # What the wall holds is the REGISTRY: the owner declaring, per rail
  # namespace, which apps admit guests and what they may do there.
  if [ "$1" = beefy ]; then
    sed -i "/^\[daemon\]$/a guest_bind = \"${IP[beefy]}:$GUEST_PORT\"" "$D/$1/config.toml"
    {
      echo
      echo '[daemon.guest_pages]'
      echo "$RING = \"$PAGE\""
      echo "$RING2 = \"$APP2\""
      # The narrowing, spelled the way an operator would: an app the room reads
      # and does not write to.
      echo "$RING3 = { dir = \"$APP3\", guests = \"read\" }"
    } >> "$D/$1/config.toml"
  fi
  return 0
}

# The second app, scaffolded at run time and hashed against the templates it
# came from — the campaign's falsifier, made a measurement rather than a claim.
#
# `svrn ring new` substitutes `{{NAME}}` into `index.html` at the title and the
# heading and copies the other three files through, so `index.html` is compared
# against the template WITH that substitution applied and the rest byte for
# byte. Hashing the raw template against a patched-in-no-way scaffold would
# read mismatch on a correct run and turn the bar's own falsifier into a false
# alarm (inventory 0f2bba316 (v)).
room_scaffold() { # dir name
  local dir=$1 name=$2
  "$CLI" ring new "$dir" --name "$name" > "$D/scaffold-$(basename "$dir").out" 2>&1 || return 1
  # The falsifier's falsifier. With this set the instrument serves a scaffold
  # it PATCHED by one line, and `rg-second-app-zero-lines` must read 0.0
  # naming the file whose hash moved — a bar that stays green here is a bar
  # measuring nothing.
  [ -n "${RING_ROOM_PLANT_SCAFFOLD:-}" ] && echo "// planted: one line the templates do not have" >> "$dir/app.js"
  return 0
}

# Per-file sha256 of a scaffolded directory against the templates, plus the
# `grep -ci guest` the bar's clause (b) reads. Writes ONE json object.
room_scaffold_proof() { # dir name out.json
  python3 - "$1" "$2" "$REPO/sovereign/crates/sovereign-cli-llm/src/ring_cmd/templates" > "$3" <<'PY'
import hashlib, json, os, re, sys
scaffold, name, templates = sys.argv[1:4]
def sha(b): return hashlib.sha256(b).hexdigest()[:16]
files, equal = [], True
for f in ("index.html", "app.js", "expenses.js", "expenses.test.mjs"):
    tpl = open(os.path.join(templates, f), "rb").read()
    # The ONE substitution the verb makes, applied to the template so the two
    # sides are comparable. Every other file is compared as it was written.
    if f == "index.html":
        tpl = tpl.replace(b"{{NAME}}", name.encode())
    try:
        got = open(os.path.join(scaffold, f), "rb").read()
    except OSError as e:
        files.append({"file": f, "error": str(e)}); equal = False; continue
    same = got == tpl
    equal = equal and same
    files.append({"file": f, "scaffold_sha": sha(got), "template_sha": sha(tpl), "equal": same,
                  "guest_mentions": len(re.findall(rb"(?i)guest", got))})
json.dump({"dir": scaffold, "name": name, "files": files,
           "equal_to_templates": equal,
           "guest_mentions": sum(f.get("guest_mentions", 0) for f in files)}, sys.stdout)
PY
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
  # The venue's WiFi reaches the wall and nothing else. Without `--internal` a
  # podman bridge NATs to the internet, beefy keeps its relay through the room
  # while "the uplink is cut", and the offline bar measures nothing.
  #
  # `--internal` does the whole job here, and NOT by the mechanism its name
  # suggests. It is usually described as route-based — no default route in the
  # container, no masquerade — which would leave the host free to forward the
  # uplink bridge INTO the room bridge. What netavark 1.17.2 also does is set
  # `net.ipv4.conf.<room bridge>.forwarding = 0` in the rootless netns, and
  # that single bit is what closes the path. Measured on this host 2026-09-20
  # in the live topology, keeper (uplink) → the wall's ROOM address, with the
  # phone (room) → the same address as the control, every reading 5 tries:
  #   room forwarding=0, no seal ....... UDP 0/5, TCP timeout   (phone 5/5, 200)
  #   room forwarding=1, no seal ....... UDP 5/5, TCP 200
  #   room forwarding=1, seal installed  UDP 0/5, TCP no answer
  # `--opt isolate=true` was carried here until 2026-09-20 for this job and
  # installs no rule at all: netavark writes isolation chains for non-internal
  # networks only, so `nft list ruleset` in that netns reads
  # `chain NETAVARK-ISOLATION-1 { }` — empty — with the room isolated, and
  # `strict` reads the same. FORWARD is `policy accept` carrying only the
  # UPLINK's own `ip daddr 10.89.61.0/24 ct state established,related accept` /
  # `ip saddr 10.89.61.0/24 accept`; the room bridge is never named there. So
  # the flag went, and `room_seal` below installs the rule pair that the third
  # row shows is what actually holds if that sysctl is ever 1 — while
  # `room_seal_prove` reads the wire itself, whichever of the two is carrying
  # the guarantee.
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
  # The wall's SECOND screen. Two apps on the wall means two `ring show`
  # proxies on beefy — one namespace each — and the host-side watcher reads
  # both, so this port is published beside the page port.
  # NOT client_port + 2: that is the daemon's own rail port, which `ring show`
  # proxies to (`commonwealth_core::config::rail_port`), and binding there
  # fails with "Address already in use" at bring-up.
  DPORT2=19948
  XPUB=([beefy]=$DPORT2)
  CPORT=([beefy]=19941 [halo]=19951 [little]=19961 [phone]=19971)
  IPORT=([beefy]=19942 [halo]=19952 [little]=19962 [phone]=19972)
  DPORT=([beefy]=19949 [halo]=19959 [little]=19969 [phone]=19979)
  MESHNAME=([beefy]=BeefyMac [halo]=RuggedFox [little]=LittleMac)
  PERSON=([beefy]=beefy [halo]=halo [little]=little [phone]=phone)
  # The guest door's own bind, on the room's WiFi. Nothing else on beefy
  # leaves loopback: the client API stays shut, as it must on an encrypted
  # mesh (O2 (ii)).
  GUEST_PORT=19947
  # ONE door, ONE QR, and the scan lands on the door's INDEX of the apps the
  # wall's owner registered (A55): `guest_page_dir` is left UNSET, so the bare
  # prefix is that index rather than one app's page.
  DOOR="http://${IP[beefy]}:$GUEST_PORT/ring/"
  PAGE="$D/page"
  # The app a person would make SECOND: `svrn ring new`, unmodified, on the
  # same wall through the same door. And a THIRD, the same scaffold registered
  # `guests = "read"` — an app the room may look at and not write to, which is
  # the third of the three refusals `rg-one-person-across-apps` (e) asks for.
  RING2=house-expenses
  RING3=house-ledger
  APP2="$D/$RING2"
  APP3="$D/$RING3"
  # The QR lives beside the apps, never inside one: a file the instrument drops
  # into a served bundle would be a line nobody scaffolded, and the second
  # app's whole claim is that its directory is the templates.
  WALL_QR="$D/wall-qr.svg"
  WALL_LOG="$D-wall.ndjson"
  WALL2_LOG="$D-wall2.ndjson"
  PHONE_PKG="$REPO/scripts/ring-room-phone"
  # The guests, named by themselves. None of them is ever a member. There are
  # MORE PHONES THAN APPS on purpose — three registered apps, four phones —
  # so one grant for the wall cannot be mistaken for a grant per phone
  # (C `rg-one-person-across-apps` clause (d)).
  # Every one distinct, and that is now load-bearing: the name binds to a
  # door-issued session and the door refuses a name another live session
  # already holds, so two phones sharing one would deadlock the shim's ask.
  GUEST_ONE=Wren
  GUEST_TWO=Fen
  GUEST_CUT=Juno
  GUEST_NARROW_ONE=Pike
  GUEST_NARROW_TWO=Rowan
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
room_journal() { # node [namespace]
  sv "$1" ring log "${2:-$RING}" --json 2>/dev/null \
    | python3 -c "
import hashlib, json, sys
try: log = json.load(sys.stdin)
except Exception: print(''); raise SystemExit
ops = sorted((o.get('id') or '') + json.dumps(o.get('payload'), sort_keys=True) for o in log.get('ops') or [])
print(f\"{len(ops)} {hashlib.sha256(''.join(ops).encode()).hexdigest()[:16]}\")"
}

# A namespace this daemon owns, read from the constant that DECLARES it rather
# than typed here — a rename on the writing side then moves this leg with it
# instead of leaving it probing a namespace nobody owns any more.
room_daemon_owned_ns() {
  sed -n 's/^pub const MEASUREMENTS_APP_ID: &str = "\(.*\)";$/\1/p' \
    "$REPO/sovereign/crates/sovereign-core/src/mesh_measurements.rs" | head -1
}

# The model the wall will grant, read from the daemon's own dispatchable list
# so no model id is written here.
room_model() {
  node_curl beefy -s --max-time 20 "$(at "${CPORT[beefy]}")/v1/models" 2>/dev/null \
    | python3 -c "import sys,json; print(((json.load(sys.stdin).get('data') or [{}])[0]).get('id') or '')" 2>/dev/null
}

# A grant and its QR. `--url` is the page the door serves; the bearer rides the
# FRAGMENT the builder puts it in, and the QR carries that link.
#
# The scope is the CALLER's, because the wall has two kinds now and they are
# two different things: `--all-apps` is the one code the room scans and it reaches
# every app the owner registered, and `--app <ns>` is the narrowing knob —
# one link, one app. The default is the wall.
room_grant() { # label svg-path [scope-flag scope-arg]
  local label=$1 svg=$2; shift 2
  local scope=(--all-apps); [ $# -gt 0 ] && scope=("$@")
  typed beefy wall "mesh grant --model $MODEL ${scope[*]} --ttl 2h --label $label --url $DOOR --qr-svg $svg" \
    "the wall's own grant, minted at the wall by the member standing there"
  node_exec beefy "$CLI" mesh grant --model "$MODEL" "${scope[@]}" --ttl 2h --label "$label" \
    --url "$DOOR" --qr-svg "$svg" > "$D/grant-$label.out" 2>&1
}

# A phone, in the phone container: nothing but node and the repo it reads the
# page's adapter out of. Its whole input is the SVG it scanned.
room_phone() { # label svg name apps edits ask collide liar probes
  node_exec phone env REPO="$REPO" QR_SVG="$2" NAME="$3" APPS="$4" EDITS="${5:-1}" \
    ASK="${6:-}" COLLIDE="${7:-}" LIAR="${8:-}" PROBES="${9:-}" LABEL="$1" \
    node "$PHONE_PKG/phone.mjs" > "$D/phone-$1.json" 2> "$D/phone-$1.err"
  # Everything it typed goes into the ONE census, under its own node label —
  # with the ELEMENT that took the string, so "which field asked for the name"
  # is read off the record rather than assumed.
  python3 - "$D/phone-$1.json" "$1" <<'PY' >> "$TYPED"
import json, sys
try: p = json.load(open(sys.argv[1]))
except Exception: raise SystemExit
for t in p.get("typed") or []:
    print("\t".join(["walk", sys.argv[2], t.get("leg", ""), "", t.get("string", ""), t.get("note", ""),
                     sys.argv[1], t.get("element", "")]))
PY
}

mesh_json() { sv "$1" mesh status --json; }

# How many members a node's own `mesh status --json` names — read once and,
# when it prints nothing, once more 2 s later. Third recurrence of an empty
# read (A27, A40), and an empty read is a reading the instrument FAILED to
# make, never a count and never a false leg (ARCH 6): the exit code and the
# first line of stderr come back beside it so the bar can name what happened.
# Prints `<count>|<exit code>|<stderr>`; count is empty when both tries were.
mesh_members() { # node
  local n=$1 try out rc="" count=""
  for try in 1 2; do
    out=$(mesh_json "$n" 2>"$D/room-$n-members.err"); rc=$?
    count=$(printf '%s' "$out" \
      | python3 -c "import sys,json; print(len(json.load(sys.stdin)['members']))" 2>/dev/null)
    [ -n "$count" ] && break
    [ "$try" = 1 ] && sleep 2
  done
  printf '%s|%s|%s\n' "$count" "$rc" "$(tr '\n\t' '  ' < "$D/room-$n-members.err" | cut -c1-200)"
}

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
  for n in a b c; do opened "$n" doc "$D/$n/dev.out" "the ring-doc page, as ring show printed it"; done
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
  n_before=$(mesh_members a)
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
  n_after=$(mesh_members a)

  # (a) the doc: its page names it, and its edit is attributed on a.
  start_proxy d 2> "$D/room-join-proxy.err"
  opened d join "$D/d/dev.out" "the ring-doc page, as ring show printed it"
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
# An empty `mesh status --json` from a is recorded as a null count and named
# WITH the exit code and stderr of the second try, never an int('') crash that
# erases the doc and library legs with it and never a count the read never
# made. `mesh_members` hands each reading over as `<count>|<rc>|<stderr>`.
def read(s):
    c, rc, err = (s.split("|", 2) + ["", ""])[:3]
    return (int(c) if c else None, rc, err.strip())
nb, nb_rc, nb_err = read(nb)
na, na_rc, na_err = read(na)
unread = [f"a's mesh status --json printed nothing ({k}: exit {rc}" + (f"; {err}" if err else "") + ")"
          for k, v, rc, err in (("n_before", nb, nb_rc, nb_err), ("n_after", na, na_rc, na_err)) if v is None]
json.dump({"name": name, "n_before": nb, "n_after": na, "n_unread": unread, "doc": doc,
           "corpus": cid, "ingest_rc": int(rc), "shared_meta": bool(meta),
           "answered_s": float(answered) if answered else None, "asks": int(asks) + (1 if answered else 0),
           "listed_s": float(listed) if listed else None,
           "library": None if backend == "podman" else "backend local: Jellyfin needs a node netns to sit in"}, sys.stdout)
PY
}

# TRUE when the join that just failed failed the ONE way waiting can fix: the
# joiner's iroh tunnel timed out (`join.rs:412`) while the relay was still
# holding a torn-down node's endpoint id from a previous run ("Another endpoint
# connected with the same endpoint id"). Two of four joins on 2026-09-19 went
# this way. This instrument reuses no key — `room_up` does `rm -rf "$D"` before
# any node has a data dir, so every run's endpoint key is minted fresh; the
# wait is the relay's own, not ours. Any OTHER failure is the run's verdict and
# is never retried.
room_join_lost_to_the_relay() { # node
  local n=$1
  grep -qs "iroh tunnel" "$D/join-$n.json" \
    && grep -qs "Another endpoint connected with the same endpoint id" \
         "$D/$n/daemon.err" "$D/beefy/daemon.err"
}

# ── the seal: neither bridge forwards to the other ──────────────────────────
# One drop each way between the room's bridge and the uplink's, which is what
# a venue's WiFi says. It is the SECOND thing holding that guarantee — the
# room bridge's `forwarding = 0` is the first, and the table at NET_FLAGS
# above is the run where the rules alone closed the path with that bit set to
# 1. Kept for the day a podman or netavark stops setting it, which is a bit
# nobody would notice changing. netavark rewrites the netns ruleset on every
# container setup, so this runs AFTER every node is attached — and again
# after a heal, which is a setup like any other.
room_nft() { "${PODMAN[@]}" unshare --rootless-netns nft "$@"; }
# Reading the chain is NOT `room_nft list …`: from inside the toolbox podman is
# reached through `flatpak-spawn --host`, and nft's listing arrives empty there
# unless it is piped on the far side (measured 2026-09-20: `podman unshare
# --rootless-netns nft list ruleset | wc -l` → 0, `… sh -c 'nft list ruleset |
# cat' | wc -l` → 68). An empty read here would make the seal insert twice and
# then report itself missing, so the pipe lives inside the host command.
room_forward_chain() { "${PODMAN[@]}" unshare --rootless-netns sh -c 'nft list chain inet netavark FORWARD | cat' 2>/dev/null; }
# The interface podman gave a network, asked rather than guessed: they are
# podman1/podman2 here in creation order, which is the kind of fact that holds
# until something else on this host creates a network first.
room_net_if() { "${PODMAN[@]}" network inspect "$1" --format '{{.NetworkInterface}}' 2>/dev/null; }
room_seal() {
  [ "$BACKEND" = podman ] || return 0
  local rootless up_if room_if have a b
  rootless=$("${PODMAN[@]}" info --format '{{.Host.Security.Rootless}}' 2>/dev/null)
  [ "$rootless" = true ] || {
    echo "room: podman reports Rootless=${rootless:-<nothing>} — the seal is installed in the ROOTLESS network namespace and this host has none, so the uplink bridge stays forwarded into the room and the offline bar would measure a cut that is not a cut" >&2
    return 3
  }
  up_if=$(room_net_if "$UPLINK"); room_if=$(room_net_if "$ROOM_NET")
  [ -n "$up_if" ] && [ -n "$room_if" ] || {
    echo "room: podman names no bridge interface for $UPLINK/$ROOM_NET (got '${up_if:-}'/'${room_if:-}') — nothing to seal between" >&2
    return 3
  }
  a="iifname \"$up_if\" oifname \"$room_if\" drop"
  b="iifname \"$room_if\" oifname \"$up_if\" drop"
  have=$(room_forward_chain)
  case "$have" in *"$a"*) ;; *) room_nft insert rule inet netavark FORWARD \
      iifname "$up_if" oifname "$room_if" drop || return 3 ;; esac
  case "$have" in *"$b"*) ;; *) room_nft insert rule inet netavark FORWARD \
      iifname "$room_if" oifname "$up_if" drop || return 3 ;; esac
  # Read back, because an `nft insert` that exits 0 having matched nothing is
  # the shape of a seal nobody would notice was missing.
  have=$(room_forward_chain)
  case "$have" in
    *"$a"*) case "$have" in *"$b"*) ;; *) echo "room: the $room_if→$up_if drop is not in the ruleset after insert" >&2; return 3 ;; esac ;;
    *) echo "room: the $up_if→$room_if drop is not in the ruleset after insert" >&2; return 3 ;;
  esac
  echo "room: sealed $up_if↛$room_if and $room_if↛$up_if"
}

# The phone has no curl: a handset image carries none of this repo's
# toolchain, so it reads a status the way its browser would.
room_fetch_status() { # node url
  node_exec "$1" node -e \
    'fetch(process.argv[1], {signal: AbortSignal.timeout(5000)}).then(r => console.log(r.status)).catch(() => console.log("000"))' \
    "$2" 2>/dev/null
}

# The seal read on the wire, both ways round, before any leg runs: from the
# keeper on the uplink the wall's ROOM address must not answer, and from the
# phone in the room it must. Only the pair separates a sealed room from a
# broken one — a run where nothing answers anything would pass the first
# clause alone.
#
# Watched RED on 2026-09-20 the only way it can go red on this host: with the
# room bridge's `forwarding` set to 1 and no rules installed, the keeper read
# HTTP 200 here and 5 of 5 UDP replies at the wall's room address.
#
# The target is the wall's page port at its room address, not the guest door
# the bar's people use. The door is not listening yet: it binds only while a
# rail grant is live (`sovereign-daemon/src/guest_door.rs:117`) and the first
# grant is minted inside the wall leg (`room_grant wall`), so at `up` both
# ends would read "no answer" and the phone's clause could never pass. The
# page port is the same uplink→room wire and the wall's only room-address
# listener at this point in the run.
room_seal_prove() {
  local url="http://${IP[beefy]}:${DPORT[beefy]}/" from_keeper from_phone
  from_keeper=$(node_curl halo -s --max-time 5 -o /dev/null -w '%{http_code}' "$url" 2>/dev/null)
  from_phone=$(room_fetch_status phone "$url")
  printf 'halo (uplink) -> %s : %s\nphone (room)  -> %s : %s\n' \
    "$url" "${from_keeper:-no-answer}" "$url" "${from_phone:-no-answer}" > "$D/room-seal.out"
  case "${from_keeper:-000}" in
    000) ;;
    *) echo "room: the seal does not hold — halo, on the uplink, fetched the wall's page at its ROOM address $url and read HTTP $from_keeper. The uplink bridge is still forwarded into the room, so a cut of beefy's uplink would leave that path up and the offline bar would measure nothing." >&2; return 3 ;;
  esac
  case "${from_phone:-000}" in
    2??) ;;
    *) echo "room: the seal cuts too much — the phone is on the room's own WiFi and its fetch of $url read '${from_phone:-no answer}'. The room must reach the wall; only the uplink may not." >&2; return 3 ;;
  esac
  echo "room: seal proven — halo→wall's room address ${from_keeper:-no answer}, phone→wall's room address $from_phone"
}

# The wall's second screen: `ring show` for the scaffolded app, on beefy, at
# the published second port. `start_proxy`'s shape, with the namespace and the
# bundle it serves as the only difference — a wall with two apps runs one per
# app because a dev server holds one grant for one namespace.
room_proxy2() {
  local deadline
  [ -f "$D/beefy/dev2.pid" ] && node_kill beefy "$D/beefy/dev2.pid" && sleep 1
  node_bg beefy "$D/beefy/dev2.pid" "$D/beefy/dev2.out" "$D/beefy/dev2.out" \
    "$CLI" ring show "$RING2" --dir "$APP2" --port "$DPORT2"
  deadline=$(( $(date +%s) + 60 ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    node_curl beefy -s --max-time 2 -o /dev/null -w '%{http_code}' -X POST "$(at "$DPORT2")/__ring/log" -d '{}' 2>/dev/null | grep -q 200 && return 0
    sleep 1
  done
  echo "ring show for $RING2 never served /__ring/log" >&2
  return 1
}

# ── the room's bring-up ─────────────────────────────────────────────────────
# Three daemons and one daemon-less phone. The library's wizard runs HERE, at
# install: the walk that follows types no credential, which is what the film
# bar's clause (c) reads.
room_up() {
  need_binaries
  rm -rf "$D"; mkdir -p "$D"
  local n
  # The apps BEFORE the configs that register them: the door canonicalizes a
  # bundle directory when it serves it, and a registry pointing at nothing is
  # a 404 the run would have to explain.
  mkdir -p "$PAGE"; cp -R "$APP/." "$PAGE/"
  room_scaffold "$APP2" "House Expenses" || return 3
  room_scaffold "$APP3" "House Ledger" || return 3
  room_scaffold_proof "$APP2" "House Expenses" "$D/scaffold.json"
  room_scaffold_proof "$APP3" "House Ledger" "$D/scaffold-readonly.json"
  for n in beefy halo little; do mkcfg "$n"; done
  containers_up || return 3
  room_seal || return 3
  for n in beefy halo little; do room_start_daemon "$n"; done
  wait_all_up beefy halo little || return 3
  wait_homed beefy halo little || return 3
  local retries=0
  for n in halo little; do
    if ! { join_one "$n" && wait_online "${MESHNAME[$n]}"; }; then
      room_join_lost_to_the_relay "$n" || return 3
      echo "room: $n's join lost its iroh tunnel while the relay still held a previous endpoint id — one retry, 30 s" >&2
      sleep 30; retries=$(( retries + 1 ))
      join_one "$n" && wait_online "${MESHNAME[$n]}" || return 3
    fi
  done
  python3 -c "import json,sys; json.dump({'join_retries': int(sys.argv[1])}, sys.stdout)" \
    "$retries" > "$D/room-up.json"
  sleep 12
  members_from_mesh 3 || return 3
  # The wall's own screens: the member's page, loopback, exactly as rr-1 runs
  # it — and one per app, because a `ring show` serves ONE namespace and the
  # wall now holds two. The second is the scaffold's, unmodified.
  start_proxy beefy || return 3
  room_proxy2 || return 3
  room_seal_prove || return 3
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
  node_kill beefy "$D/beefy/dev2.pid"
  local n
  for n in beefy halo little; do node_kill "$n" "$D/$n/pid"; done
  sleep 1
  [ "$BACKEND" = podman ] && containers_down
  return 0
}

# ── the narrowing knob ──────────────────────────────────────────────────────
# `--app <ns>` is the other grant an owner can mint: one link, one app. It
# GATES NOTHING here — the wall is one grant and one QR, and clause (d)'s
# census was taken before this ran — but two things are worth reading off it.
#
# The reach: a link narrowed to one app reaches it and is refused the other BY
# NAME, so "the wall grant reaches every declared app" is a property of the
# wall grant rather than of the rail being open.
#
# And the collision, per app: under the wall grant the door checks a claimed
# name against every roster the bearer reaches, so ONE refusal covers the whole
# wall and "refused on both apps" cannot be read off it. A grant narrowed to
# each app asks the question one app at a time, which is the only way that
# clause can be measured at all.
leg_room_narrowing() { # member
  local member=$1
  room_grant narrow-expenses "$D/qr-narrow-expenses.svg" --app "$RING2"
  room_grant narrow-doc "$D/qr-narrow-doc.svg" --app "$RING"
  room_phone narrow-expenses "$D/qr-narrow-expenses.svg" "$GUEST_NARROW_ONE" \
    "$RING2:expenses:$APP2" 0 "" "$member" "" "other_app=$RING"
  room_phone narrow-doc "$D/qr-narrow-doc.svg" "$GUEST_NARROW_TWO" \
    "$RING:doc" 0 "" "$member" "" "other_app=$RING2"
  sv beefy mesh grant --list > "$D/wall-grants-after.txt" 2>&1
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
  # ONE grant, ONE QR, for the whole wall (A55). rr-2 minted one per phone;
  # under the operator's model the resource declares and the credential
  # identifies, so the owner widens the wall by REGISTERING an app, not by
  # minting another link.
  room_grant wall "$WALL_QR"
  # The wall watches its own rail while the phones are in the room — one
  # watcher per app, because the wall shows two screens now.
  : > "$WALL_LOG"; : > "$WALL2_LOG"
  REPO="$REPO" PA="$(tab_url beefy)" OUT="$WALL_LOG" POLL_MS=250 WATCH_S=1200 \
    node "$PHONE_PKG/wall.mjs" > "$D/wall-watch.out" 2>&1 &
  wallpid=$!
  REPO="$REPO" PA="http://127.0.0.1:$DPORT2" KIND=expenses APP_DIR="$APP2" \
    OUT="$WALL2_LOG" POLL_MS=250 WATCH_S=1200 \
    node "$PHONE_PKG/wall.mjs" > "$D/wall2-watch.out" 2>&1 &
  wall2pid=$!
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
  # The person: one scan, one name — typed into the DOOR's prompt, once — an
  # expense in the app nobody wrote a line of, then the doc, which must not ask
  # again. Three act kinds in the expenses app: a record, a correction that
  # restates, and a retraction that carries no replacement at all.
  room_phone phone "$WALL_QR" "$GUEST_ONE" "$RING2:expenses:$APP2,$RING:doc" 1 "$q" "" 1 \
    "undeclared=ring-not-granted,daemon_owned=$(room_daemon_owned_ns),read_only=$RING3"
  # The second person, who tries the wall's own member name first and is
  # refused, then takes a name of their own — on the SAME code as the first.
  room_phone phone2 "$WALL_QR" "$GUEST_TWO" "$RING2:expenses:$APP2" 1 "" "$member"
  kill "$wallpid" "$wall2pid" 2>/dev/null
  # The grant census for `rg-one-person-across-apps` (d) is taken HERE, while
  # the only grant that exists is the wall's. The narrowing leg below mints
  # two more on purpose and its own list is recorded apart.
  sv beefy mesh grant --list > "$D/wall-grants.txt" 2>&1
  leg_room_narrowing "$member"
  mesh_json beefy > "$D/wall-beefy-after.json"; mesh_json halo > "$D/wall-halo-after.json"
  # The peer path, from the daemon's EXISTING observation — nothing added for
  # the bar (A36): `/v1/mesh/status` already publishes `iroh_transport`.
  node_curl beefy -s --max-time 10 "$(at "${CPORT[beefy]}")/v1/mesh/status" > "$D/wall-transport.json" 2>/dev/null
  grep -E 'routing outcome|routing decision|guest_ask: accepted|turn stage attribution' "$D/beefy/daemon.err" \
    > "$D/wall-decisions.txt" 2>/dev/null
  # A MEMBER's own loopback append carrying a forged `on_behalf_of`. Nobody
  # authenticated a guest here, so the door has nothing to stamp: the name must
  # not reach the signature, and the daemon must say which of the two happened
  # rather than dropping it quietly (C `rg-guest-stamped-by-the-door` (c)).
  node_curl beefy -s --max-time 20 -X POST \
    "$(at "${CPORT[beefy]}")/v1/rail/append?namespace=$RING2" \
    -H 'content-type: application/json' \
    -d "{\"op\":\"record\",\"on_behalf_of\":\"Not A Guest\",\"payload\":{\"kind\":\"expense\",\"payer\":\"$member\",\"amount_cents\":100,\"description\":\"a member's own append, carrying a name it was not given\",\"participants\":[\"$member\"]}}" \
    > "$D/wall-forged.json" 2>/dev/null
  grep -E 'dropped an on_behalf_of|stamped an act with the name' "$D/beefy/daemon.err" \
    > "$D/wall-stamp-lines.txt" 2>/dev/null
  # The keeper's replica: the guests' acts, byte for byte, after its next sync.
  # BOTH rings — the doc's, which rr-2 reads, and the scaffold's, which is
  # where this campaign's guest acts are.
  local jb jh="" sync="" t1 jb2 jh2="" sync2=""
  t1=$(date +%s)
  while [ $(( $(date +%s) - t1 )) -lt 120 ]; do
    jb=$(room_journal beefy); jh=$(room_journal halo)
    [ -n "$jh" ] && [ "$jh" = "$jb" ] && { sync=$(( $(date +%s) - t1 )); break; }
    sleep 3
  done
  t1=$(date +%s)
  while [ $(( $(date +%s) - t1 )) -lt 120 ]; do
    jb2=$(room_journal beefy "$RING2"); jh2=$(room_journal halo "$RING2")
    [ -n "$jh2" ] && [ "$jh2" = "$jb2" ] && { sync2=$(( $(date +%s) - t1 )); break; }
    sleep 3
  done
  printf '%s\n%s\n%s\n' "${jb:-}" "${jh:-}" "${sync:-}" > "$D/wall-replica.txt"
  printf '%s\n%s\n%s\n' "${jb2:-}" "${jh2:-}" "${sync2:-}" > "$D/wall-replica-2.txt"
  # Demo step 6: the Halo's own `svrn ring log` after the sync, which is the
  # one route `op.person` is composed on — so the stamp is read on a replica
  # the wall did not render.
  sv halo ring log "$RING2" --json > "$D/wall-halo-expenses.json" 2>/dev/null
  sv beefy ring log "$RING2" --json > "$D/wall-beefy-expenses.json" 2>/dev/null
  python3 - "$D" "$member" "$id" "$rc" "${meta:-}" "${seen:-}" "$MODEL" "$q" "$(self_name halo)" \
    "$RING" "$RING2" "$RING3" > "$out" <<'PY'
import json, os, sys
d, member, cid, rc, meta, seen, model, q, keeper, doc_ns, exp_ns, ro_ns = sys.argv[1:13]
listed = open(os.path.join(d, "wall-beefy-corpora.txt")).read()
def members(p):
    try: return sorted(m["name"] for m in json.load(open(os.path.join(d, p)))["members"])
    except Exception: return None
def read(p):
    try: return open(os.path.join(d, p)).read()
    except OSError: return ""
def js(p):
    try: return json.load(open(os.path.join(d, p)))
    except Exception: return None
# The stamp as a REPLICA holds it: every op on the scaffold's ring, with the
# name the log route composed. Read off `svrn ring log --json`, which is the
# same route the page reads — never re-derived here.
def stamped(p):
    v = js(p) or {}
    return [{"id": o.get("id"), "person": o.get("person"), "guest": o.get("guest"),
             "corrects": o.get("corrects"), "voided": o.get("voided"),
             "has_payload": o.get("payload") is not None}
            for o in (v.get("ops") or [])]
json.dump({"member": member, "keeper": keeper, "model": model, "question": q, "corpus": cid,
           "ingest_rc": int(rc), "shared_meta": bool(meta),
           "absent_on_beefy": cid not in listed,
           "beefy_heard_s": int(seen) if seen else None,
           "namespaces": {"doc": doc_ns, "expenses": exp_ns, "read_only": ro_ns},
           "members_before": {"beefy": members("wall-beefy-before.json"), "halo": members("wall-halo-before.json")},
           "members_after": {"beefy": members("wall-beefy-after.json"), "halo": members("wall-halo-after.json")},
           "grants": read("wall-grants.txt"),
           "grants_after_narrowing": read("wall-grants-after.txt"),
           "scaffold": js("scaffold.json"), "scaffold_read_only": js("scaffold-readonly.json"),
           # A member's own append carrying a name nobody gave it.
           "forged": js("wall-forged.json"),
           "stamp_lines": [l for l in read("wall-stamp-lines.txt").splitlines()][-6:],
           "on_beefy": stamped("wall-beefy-expenses.json"),
           "on_halo": stamped("wall-halo-expenses.json"),
           "replica_2": (read("wall-replica-2.txt").splitlines() + ["", "", ""])[:3]},
          sys.stdout)
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

# Every address beefy's OWN log says it reached this peer at — the daemon's
# citation rather than the instrument's guess (ARCH 4). Every internal port
# binds loopback here, so a peer is reachable only over iroh, and iroh's local
# mouth is a bridge gateway on beefy's loopback: the `url=` this list collects
# is `http://127.0.0.1:<bridge-port>/oicp/v1/capabilities`, not a container
# address. That bridge port is what survived the cut in run 4 of 2026-09-19
# (`transport: resolved … first=iroh:127.0.0.1:32519→63cc94f6`, then `fetched
# peer manifest peer=RuggedFox url=http://127.0.0.1:32519/… rtt_ms=3
# locality=Local`). It is the DAEMON's, not the instrument's forwarder — the
# forwarder listens on DPORT and only ever carries a node's own page to that
# node's own loopback, never a peer.
room_peer_urls() { # peer-mesh-name
  python3 -c "
import re, sys
ansi = re.compile(r'\x1b\[[0-9;]*m')
want, urls = sys.argv[2], []
try: f = open(sys.argv[1], errors='replace')
except OSError: raise SystemExit
for line in f:
    m = re.search(r'peer=(\S+) url=(\S+)', ansi.sub('', line))
    if m and m.group(1) == want and m.group(2) not in urls: urls.append(m.group(2))
print('\n'.join(urls))" "$D/beefy/daemon.err" "$1" 2>/dev/null
}

# The cut must BE a cut before the leg may judge what the room lost (ARCH 5):
# a bar reading "the answer named nobody it could not reach" while the keeper
# was still reachable measures nothing at all. Two readings on beefy, both
# inside the cut and before the phone types anything:
#   (1) `mesh status --json` stops calling the two uplink members online,
#       within one gossip round — RECORDED into `$D/room-cut-status.out` and
#       NOT gating (A42): `online` is the roster's own liveness clock, which a
#       member keeps until its last-seen window closes; a roster that has not
#       caught up yet is not an uplink that is still carrying bytes, and the
#       bar was reading COULD-NOT-JUDGE on it while the cut was real.
#   (2) every address in `room_peer_urls` refuses an OICP capabilities fetch.
#   (3) the fan-out the bar actually judges still refuses — see below.
# Echoes the surviving address, or nothing. The leg reads COULD-NOT-JUDGE on a
# survivor — never FAILED, never PASSED.
#
# (2) alone used to decide, and it decided the wrong thing. It probes a FRESH
# dial, which proves new dials fail and says nothing about a connection the
# daemon already holds — and an already-held connection is exactly what served
# the keeper's corpus 52 s into the cut of 2026-09-19 while (2) was writing
# "the cut is a cut". A guard that asserts on a different path from the one
# the bar judges is ARCH 5's check with no failing input you can name. So (3)
# drives the fan-out itself: an OICP knowledge search on beefy for the
# keeper's corpus, which beefy does not host, over whatever path the transport
# already has. The reading is the daemon's OWN `fan-out served` line (ARCH 4),
# which fires whether or not the search had hits — a response body would not
# distinguish "the peer answered with nothing" from "the peer never answered".
room_cut_fanout_survivor() { # keeper-mesh-name
  local before
  [ -n "${ROOM_CORPUS:-}" ] \
    || { echo "the keeper's corpus id is not known in this leg set, so the fan-out path was NOT probed"; return 0; }
  before=$(wc -l < "$D/beefy/daemon.err" 2>/dev/null || echo 0)
  node_curl beefy -s --max-time 30 -X POST "$(at "${CPORT[beefy]}")/v1/knowledge/search" \
    -H 'content-type: application/json' \
    -d "{\"query\":\"what the keeper holds\",\"corpora\":[\"$ROOM_CORPUS\"],\"limit\":5}" \
    > "$D/room-cut-fanout.json" 2>/dev/null
  tail -n "+$(( before + 1 ))" "$D/beefy/daemon.err" 2>/dev/null | python3 -c "
import re, sys
ansi = re.compile(r'\x1b\[[0-9;]*m')
want = sys.argv[1]
for line in sys.stdin:
    m = re.search(r'fan-out served .*peer_name=(\S+) addr=(\S+)', ansi.sub('', line))
    if m and m.group(1) == want:
        print(m.group(2) + ' still served a knowledge fan-out to ' + want + ' from beefy')
        break" "$1"
}
# The addresses beefy's own endpoint calls ACTIVE inside the cut, read from
# the snapshot `room_assert_cut` just wrote. `mesh status --json` published a
# COUNT until 2026-09-20 (`PeerPathSnapshot.active_direct_addrs`), which is
# why the run of that morning could record a surviving path and still not name
# the wire it rode. Empty when no direct address is active, which is what a
# sealed room reads.
room_cut_direct_addrs() {
  python3 -c "
import json, sys
try: peers = json.load(open(sys.argv[1])).get('iroh_transport') or []
except Exception: raise SystemExit
print(' '.join(str(p.get('name', '?')) + '@' + a
                for p in peers
                for a in ((p.get('path') or {}).get('active_direct_socket_addrs') or [])))" \
    "$D/room-cut-paths.json" 2>/dev/null
}
room_assert_cut() {
  local deadline=$(( $(date +%s) + 30 )) online="" u survivor="" code="" wire=""
  while :; do
    online=$(mesh_json beefy | python3 -c "
import json, sys
try: ms = json.load(sys.stdin).get('members') or []
except Exception: raise SystemExit
print(' '.join(m.get('name','') for m in ms
                if m.get('status') == 'online' and m.get('name') in ('${MESHNAME[halo]}', '${MESHNAME[little]}')))" 2>/dev/null)
    [ -z "$online" ] && break
    [ "$(date +%s)" -ge "$deadline" ] && break
    sleep 3
  done
  printf '%s' "$online" > "$D/room-cut-status.out"
  # Recorded, gating nothing: beefy's own `peer_paths` INSIDE the cut. The
  # endpoint is the only thing that can name the address a surviving path
  # actually uses — `mesh status` names the roster, `room_peer_urls` names the
  # local bridge mouth, and neither is the wire. Run 3 of 2026-09-20 needs
  # this: with the uplink disconnected, gossip rounds to both peers read
  # `unreachable` while an OICP capabilities fetch to the keeper still
  # returned HTTP 200. Whatever carries that is in here — and since
  # 2026-09-20 `active_direct_socket_addrs` makes it an ADDRESS rather than
  # the count this surface used to publish. (This comment credited the room's
  # `isolate=true` with stopping the host forwarding uplink→room. It stopped
  # nothing: the flag installs no rule on an internal network, and the
  # forwarding was the wire. `room_seal` is what closes it.)
  mesh_json beefy > "$D/room-cut-paths.json" 2>/dev/null
  wire=$(room_cut_direct_addrs)
  # (3) first, because it is the path the bar judges: a survivor here is the
  # finding, and a survivor (2) alone reports is the same cut read one hop
  # further from what the bar actually measures. A survivor is reported WITH
  # the addresses the endpoint calls active, so the next reader is told which
  # wire carried it rather than being sent to reproduce the run.
  survivor=$(room_cut_fanout_survivor "${MESHNAME[halo]}")
  [ -n "$survivor" ] && { printf '%s' "$survivor${wire:+ [active direct addresses on beefy inside the cut: $wire]}"; return 0; }
  # A STATUS, not merely a response. `curl -s -o /dev/null` exits 0 on any
  # HTTP reply, and the local end of an iroh bridge answers a dead peer with
  # its own 5xx — so the un-statused form reported a survivor in the plant run
  # of 2026-09-20 05:35 while (3)'s fan-out to the same peer had just failed,
  # gossip rounds to it were FAILED, and the watchdog called the endpoint
  # unhealthy. That is a false COULD-NOT-JUDGE, which withholds a verdict the
  # leg had earned. Only a 2xx is the keeper answering; the code is echoed
  # either way so the next reader sees which it was.
  for u in $(room_peer_urls "${MESHNAME[halo]}"); do
    code=$(node_curl beefy -s --max-time 5 -o /dev/null -w '%{http_code}' "$u" 2>/dev/null)
    case "$code" in
      2??) echo "$u still answers an OICP capabilities fetch from beefy (HTTP $code)${wire:+ [active direct addresses on beefy inside the cut: $wire]}"; return 0 ;;
    esac
  done
  return 0
}

# One forgery's verdict line, echoed for the leg log; the bar's judgment is
# the report's, re-derived from the recorded exits and sentences.
ck_refusal() { # label rc outfile needle
  if [ "$2" -eq 1 ] && grep -q "checkpoint verify: step" "$3" 2>/dev/null && grep -q "$4" "$3"; then
    echo "checkpoint: the $1 forgery was refused by name"
  else
    echo "checkpoint: the $1 forgery was NOT refused as expected (exit=$2, wanted: $4)" >&2
  fi
}

# The checkpoint probe (the-link): the keeper verifies the wall's mid-cut
# export COLD — nobody asked it, it reads the file — and then three forgeries
# are refused by name, in the same run (C tl-checkpoint-verifies (a)-(d)).
# Two are file-level: a flipped signature byte (the line still parses, the id
# still derives, only the signature breaks) and a truncated ops tail (the
# completeness claim fails, naming the actor). The third cannot be typed into
# a file: a same-seq pair admit will judge needs both sides SIGNED, and only
# the key holder can equivocate. So the fork is grown on the scaffold's ring —
# the ring the wall leg already appends probes to: a real act; the journal's
# line rolled back under the live daemon (append takes its sequence number
# from disk); a second real act, different body, re-using the number.
# Nothing measures that ring after this leg, and the containers are the run's
# own throwaways.
room_checkpoint_probe() { # doc-export cut_at heal_at
  local doc=$1 cut=$2 heal=$3
  local flipped="$D/room-checkpoint-flipped.json" truncated="$D/room-checkpoint-truncated.json"
  local forkdoc="$D/room-checkpoint-fork.json" fork2="$D/room-checkpoint-fork2.json"
  local rc_v rc_b rc_t rc_f
  [ -n "${RING2:-}" ] || { echo "checkpoint: no probe ring outside the room topology" >&2; return 0; }
  # (a) cold, on the machine that was never asked.
  node_exec halo "$CLI" ring checkpoint --verify "$doc" > "$D/room-checkpoint-verify.out" 2>&1
  rc_v=$?
  [ $rc_v -eq 0 ] && echo "checkpoint: the keeper verified the mid-cut export cold" \
    || echo "checkpoint: the keeper did NOT verify the honest export (exit=$rc_v)" >&2
  # (b) one flipped signature byte.
  python3 - "$doc" "$flipped" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
op = json.loads(doc["ops"][0])
op["sig"] = ("1" if op["sig"][0] == "0" else "0") + op["sig"][1:]
doc["ops"][0] = json.dumps(op, separators=(",", ":"), ensure_ascii=False)
json.dump(doc, open(sys.argv[2], "w"), ensure_ascii=False)
PY
  node_exec halo "$CLI" ring checkpoint --verify "$flipped" > "$D/room-checkpoint-flipped.out" 2>&1
  rc_b=$?; ck_refusal "flipped-signature" "$rc_b" "$D/room-checkpoint-flipped.out" "signature that does not verify"
  # (c) the last ops line removed.
  python3 - "$doc" "$truncated" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
doc["ops"].pop()
json.dump(doc, open(sys.argv[2], "w"), ensure_ascii=False)
PY
  node_exec halo "$CLI" ring checkpoint --verify "$truncated" > "$D/room-checkpoint-truncated.out" 2>&1
  rc_t=$?; ck_refusal "truncated-tail" "$rc_t" "$D/room-checkpoint-truncated.out" "marks disagree"
  # (d) the planted same-seq pair, grown not typed (see the header note).
  local probe=$RING2 journal="$D/beefy/rings/$RING2/ring_oplog.jsonl"
  local body='{"op":"record","payload":{"kind":"expense","payer":"'"$FOUNDER"'","amount_cents":1,"description":"checkpoint fork probe: SEQUENCE NUMBER PLANT %s","participants":[]}}'
  node_curl beefy -s --max-time 20 -X POST \
    "$(at "${CPORT[beefy]}")/v1/rail/append?namespace=$probe" \
    -H 'content-type: application/json' \
    -d "$(printf "$body" first)" > "$D/room-fork-x.out" 2>&1
  node_exec beefy "$CLI" ring checkpoint "$probe" --out "$forkdoc" \
    > "$D/room-checkpoint-fork-export.out" 2>&1
  python3 - "$journal" <<'PY'
import sys
p = sys.argv[1]
lines = open(p).readlines()
kept = [l for l in lines if "SEQUENCE NUMBER PLANT first" not in l]
if len(kept) == len(lines):
    sys.exit(f"checkpoint: the plant's line is not in {p}; the journal rolled back nothing")
open(p, "w").writelines(kept)
PY
  [ $? -eq 0 ] || echo "checkpoint: the journal rollback failed; the planted pair would not be a fork" >&2
  node_curl beefy -s --max-time 20 -X POST \
    "$(at "${CPORT[beefy]}")/v1/rail/append?namespace=$probe" \
    -H 'content-type: application/json' \
    -d "$(printf "$body" second)" > "$D/room-fork-y.out" 2>&1
  node_exec beefy "$CLI" ring checkpoint "$probe" --out "$fork2" \
    > "$D/room-checkpoint-fork2-export.out" 2>&1
  python3 - "$forkdoc" "$fork2" <<'PY'
import json, sys
f = json.load(open(sys.argv[1]))
g = json.load(open(sys.argv[2]))
f["ops"].append(g["ops"][-1])
json.dump(f, open(sys.argv[1], "w"), ensure_ascii=False)
PY
  node_exec halo "$CLI" ring checkpoint --verify "$forkdoc" > "$D/room-checkpoint-fork.out" 2>&1
  rc_f=$?; ck_refusal "same-seq-pair" "$rc_f" "$D/room-checkpoint-fork.out" "the document forks"
  # One summary artifact the verdict printer reads.
  python3 - "$D" "$cut" "$heal" "$rc_v" "$rc_b" "$rc_t" "$rc_f" > "$D/room-checkpoint.json" <<'PY'
import json, os, sys
d, cut, heal, v, b, t, f = sys.argv[1:8]
def js(p):
    try: return json.load(open(os.path.join(d, p)))
    except Exception: return None
def out(p):
    try: return open(os.path.join(d, p)).read().strip()
    except OSError: return ""
doc = js("room-checkpoint-doc.json") or {}
json.dump({"ns": doc.get("ns"), "created_unix": doc.get("created_unix"),
           "acts": len(doc.get("ops") or []), "cut_at": int(cut), "heal_at": int(heal),
           "export_out": out("room-checkpoint-export.out"),
           "verify": {"exit": int(v), "output": out("room-checkpoint-verify.out")},
           "flipped": {"exit": int(b), "output": out("room-checkpoint-flipped.out")},
           "truncated": {"exit": int(t), "output": out("room-checkpoint-truncated.out")},
           "fork": {"exit": int(f), "output": out("room-checkpoint-fork.out"),
                    "ns": (js("room-checkpoint-fork.json") or {}).get("ns")}}, sys.stdout)
PY
}

leg_room_offline() {
  local out="$D/room-offline.json" q wallpid before_beefy after_beefy after_halo t0 conv="" survivor="" status_online="" cut_at heal_at cp_doc
  q=$(BANK="$BANK" python3 -c "import json,os; print(json.loads(os.environ['BANK'])[1][1])")
  : > "$D-wall-cut.ndjson"
  REPO="$REPO" PA="$(tab_url beefy)" OUT="$D-wall-cut.ndjson" POLL_MS=250 WATCH_S=900 \
    node "$PHONE_PKG/wall.mjs" > "$D/wall-watch-cut.out" 2>&1 &
  wallpid=$!
  # The cut's own length, recorded rather than inferred from the logs: a cut
  # longer than the offline threshold decays the roster, and then convergence
  # waits on the peer coming back rather than on the write (room run 2).
  cut_at=$(date +%s)
  cut_node beefy "$UPLINK" > "$D/room-cut.out" 2>&1
  survivor=$(room_assert_cut)
  status_online=$(cat "$D/room-cut-status.out" 2>/dev/null)
  echo "${survivor:-the cut is a cut: no logged peer address answers an OICP fetch from beefy}${status_online:+ [recorded, not gating: mesh status on beefy still calls $status_online online]}" > "$D/room-cut-assert.out"
  # The checkpoint addendum: the wall freezes the doc's record WHILE the room
  # is cut. Taken here, at the window's start, so the document's created_unix
  # lands strictly inside it — that is what makes "made while the room was
  # cut" checkable rather than asserted (C tl-checkpoint-verifies (a)). The
  # route is the wall daemon's own loopback; the cut took the uplink, never
  # the room.
  cp_doc="$D/room-checkpoint-doc.json"
  node_exec beefy "$CLI" ring checkpoint "$RING" --out "$cp_doc" \
    > "$D/room-checkpoint-export.out" 2>&1
  sleep 10
  room_phone phone-cut "$WALL_QR" "$GUEST_CUT" "$RING:doc" 1 "$q" ""
  kill "$wallpid" 2>/dev/null
  before_beefy=$(room_journal beefy)
  heal_at=$(date +%s)
  heal_node beefy "$UPLINK" > "$D/room-heal.out" 2>&1
  # A `network connect` is a container setup and netavark rewrites the netns
  # ruleset on each one, so the seal is re-asserted here or the healed room
  # stops being a room for everything that follows. Reported, never swallowed.
  room_seal >> "$D/room-heal.out" 2>&1 \
    || echo "room: the seal did not survive the heal (see $D/room-heal.out)" >&2
  t0=$(date +%s)
  while [ $(( $(date +%s) - t0 )) -lt 120 ]; do
    after_halo=$(room_journal halo); after_beefy=$(room_journal beefy)
    [ -n "$after_halo" ] && [ "$after_halo" = "$after_beefy" ] && { conv=$(( $(date +%s) - t0 )); break; }
    sleep 3
  done
  python3 - "${before_beefy:-}" "${after_beefy:-}" "${after_halo:-}" "${conv:-}" "$q" "$survivor" "$status_online" "${cut_at:-}" "${heal_at:-}" > "$out" <<'PY'
import json, sys
before, after_b, after_h, conv, q, survivor, status_online, cut_at, heal_at = sys.argv[1:10]
json.dump({"question": q, "beefy_at_cut": before, "beefy_after": after_b, "halo_after": after_h,
           "converged_s": int(conv) if conv else None,
           # The cut's wall-clock bounds. Read with the offline threshold (60 s):
           # a cut longer than it decays the roster on both sides, and the return
           # is what has to carry the catch-up.
           "cut_at": int(cut_at) if cut_at else None,
           "heal_at": int(heal_at) if heal_at else None,
           "byte_equal": bool(after_h) and after_h == after_b,
           # The roster reading, kept because it is worth seeing and gating on
           # nothing: only `cut_not_a_cut` — a peer address that still answers
           # — can withhold this bar's verdict.
           "status_online_at_cut": status_online or None,
           "cut_not_a_cut": survivor or None}, sys.stdout)
PY
  room_checkpoint_probe "$cp_doc" "$cut_at" "$heal_at"
}

# ── the census + the five rows ───────────────────────────────────────────────
# The bars this topology is judged on, in order. ONE reader, so the verdict
# loop and the could-not-judge short-circuit cannot disagree about the set.
# The ROOM topology reports fourteen bars: `the-link`'s three, which are what
# this campaign is proving, then `ring-guest`'s five and `ring-room`'s six,
# which are the regression it must not move. Read from the three campaign
# files, in that order.
room_bar_ids() {
  if [ "$TOPOLOGY" = room ]; then
    python3 -c "
import sys, tomllib
link  = [b['id'] for b in tomllib.load(open(sys.argv[1],'rb'))['bar']]
guest = [b['id'] for b in tomllib.load(open(sys.argv[2],'rb'))['bar']]
room  = [b['id'] for b in tomllib.load(open(sys.argv[3],'rb'))['bar'] if b.get('rung')=='rr-2']
print(' '.join(link + guest + room))" "$LINK_CAMPAIGN" "$GUEST_CAMPAIGN" "$ROOM_CAMPAIGN"
  else
    echo "ra-room-answer-names-the-machine ra-room-doc-name-from-membership ra-room-film-from-the-library-rail ra-room-plug-in-live ra-room-nothing-typed"
  fi
}

# Every bar reads COULD-NOT-JUDGE for one named reason, without a walk — the
# shape a refusal takes when the instrument cannot legitimately run at all.
# The third argument is the id set, defaulting to this script's own: a second
# instrument sourcing this one (threat-gaps-demo.sh) refuses with the same
# sentence over its own campaign's bars rather than copying the shape.
room_cnj_rows() { # reason bar|all [bar-ids]
  python3 -c "
import json, sys
reason, want, ids = sys.argv[1], sys.argv[2], sys.argv[3].split()
for b in (ids if want == 'all' else [want]):
    print(json.dumps({'bar': b, 'value': None, 'reason': reason, 'verdict': 'COULD-NOT-JUDGE'}))" \
    "$1" "$2" "${3:-$(room_bar_ids)}"
}

report() { # bar|all
  python3 - "$D" "$ROOM_CAMPAIGN" "$1" "$TYPED" "$CMDLOG" "$ROOM_SCRIPT" "${CPORT[*]} ${IPORT[*]} ${DPORT[*]} ${DPORT2:-}" "${IP[*]}" "$TOPOLOGY" "${IP[phone]:-}" "$(room_bar_ids)" "$GUEST_CAMPAIGN" "$GUEST_BASE" "$LINK_CAMPAIGN" <<'PY'
import json, os, re, subprocess, sys, tomllib
d, campaign, want, typed_p, cmdlog_p, script_p, ports, ips, topology, phone_ip, bar_ids, guest_campaign, guest_base, link_campaign = sys.argv[1:15]
# Three campaigns, one table. Ids are unique across the files, so one map is
# the right shape — and a collision would be a bar defined twice, which is the
# thing that map would hide, so it is refused here.
bars = {b["id"]: b for b in tomllib.load(open(campaign, "rb"))["bar"]}
for f in (guest_campaign, link_campaign):
    for b in tomllib.load(open(f, "rb"))["bar"]:
        if b["id"] in bars:
            raise SystemExit(f"{b['id']} is declared in both campaign files; one bar, one home")
        bars[b["id"]] = b
REPO = os.path.dirname(script_p) + "/.."
def git(*args):
    return subprocess.run(["git", *args], capture_output=True, text=True, cwd=REPO).stdout.strip()
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
    stage, node, leg, secret, s, note, prov, element = (line.split("\t") + [""] * 8)[:8]
    # `opened`: the string came VERBATIM from a tool's stdout line, and that
    # line is re-read here rather than trusted. Assembled, or no such line: counts.
    src = next((l.strip() for l in (open(prov).read().splitlines() if prov and os.path.exists(prov) else [])
                if s and s in l.split()), "")
    entries.append(dict(stage=stage, node=node, leg=leg, string=s, note=note, element=element,
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
    # WHICH element took the string. "the app still asks the name" and "the
    # door asks it" are the same census line without this.
    if e.get("element"): print(f"        taken by: {e['element']}")
    if e["src"]: print(f"        from the tool's stdout, verbatim: {e['src']}")
print(f"  walk count: {walk_count}")
print(f"== census: this script names {len(hits)} of {len(needles)} member names/ports/addresses/corpus ids: {hits} ==")

sys.path.insert(0, os.path.join(os.path.dirname(script_p), "lib"))
import demo_verdicts
rows, row = demo_verdicts.new_rows()
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
    elif j.get("n_unread"):
        # ARCH 6: a member count the instrument could not read is not a count
        # of zero and not a leg that is false. The exit code is the finding.
        row("ra-room-plug-in-live", None, "; ".join(j["n_unread"]), n_unread=j["n_unread"])
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
        # RE-READ 2026-09-20, and the reading is named here so it can be argued
        # with. Clause (a) says the QR "parses as a guest grant minted by
        # BeefyMac (`Scope::Rails(ring-doc)` + `Scope::Models`)". That
        # parenthetical describes the grant rr-2 MINTED — one per app. Under
        # the operator's A55 model the wall is ONE grant reaching every app the
        # owner registered, and its summary reads "the wall" rather than
        # "rail:ring-doc". What clause (a) is about — a real, expiring,
        # BeefyMac-minted grant that reaches this app, and members unchanged —
        # is unchanged, so the test is "does the summary name a scope that
        # reaches ring-doc", spelled either way. ring-room.toml is NOT edited.
        doc_ns = (w.get("namespaces") or {}).get("doc") or ""
        reaches_the_doc = f"rail:{doc_ns}" in (p1.get("summary") or "") \
            or "the wall" in (p1.get("summary") or "")
        legs = {"a_guest_grant_and_members_unchanged":
                    bool(p1.get("link")) and not p1.get("token_in_query")
                    and reaches_the_doc
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
            quiet_zone=(p1.get("qr") or {}).get("quiet_zone"), page=p1.get("apps"),
            clause_a_reading="A55: the wall is ONE grant reaching every registered app, so its "
                             f"summary reads 'the wall' where rr-2's read 'rail:{doc_ns}'. Clause (a) "
                             "reads 'a scope that reaches this app' — corrected in the bar's own "
                             "text 2026-09-20 (ledger A60), after data, by the operator.",
            phone_typed=[f"{e['leg']}: {e['string']}" for e in walk if e["node"].startswith("phone")],
            log_mentions_of_the_phone=guest_mentions, members=w.get("members_after"))

    # 2 — the guest's edit, and never mistakable for a member
    b = bars["ra-room-guest-edit-attributed"]
    edit_w = num(r"within ([\d.]+) s", b["floor_basis"])
    # RE-READ 2026-09-22, after the same mechanism broke a third way: the
    # D1-approved rg-1 rail diff and 6bda3417a's gate-hygiene file reorg
    # (+824/−796, tests_sealing/journal.rs splits, zero semantics) were
    # PUSHED to origin/main, so `origin/main..<guest_base>` stopped being
    # "what others did" and became the approved diff itself — the clause
    # read red on a the-link run that touched zero rail files (measured,
    # 2026-09-22, target/ralph/demo.log). No single pin can serve both
    # this clause and the shed bar's `guest_base..HEAD` range: a base at
    # or past 6bda3417a empties the shed range and the rg bar abstains;
    # anything earlier leaves the reorg in the diff. The clause therefore
    # returns to the registered floor's literal reading (ring-room.toml,
    # ra-room-guest-edit-attributed, clause c): `git diff --stat origin/main
    # -- <rail>`, origin/main against the WORKING TREE. That is green for
    # any run whose tree equals origin/main on the rail whatever history
    # did, and it re-arms as a tripwire the moment an unpushed campaign
    # edits rail — which is the standing rule D1 revised for one row only.
    # Operator direction 2026-09-22 (option a on the same handoff).
    RAIL = ["commonwealth/crates/commonwealth-rail",
            "commonwealth/crates/commonwealth-rail-core"]
    rail_diff = git("diff", "--stat", "origin/main", "--", *RAIL)
    rail_diff_ring_guest = git("diff", "--stat", guest_base, "HEAD", "--", *RAIL)
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
            clause_c_reading="rr-2's rail diff, evaluated at rr-2's own tip. The rail diff ring-guest "
                             "added under operator decision D1 is beside it and is not judged here: "
                             + (rail_diff_ring_guest.splitlines()[-1] if rail_diff_ring_guest else "empty"),
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
    elif o.get("cut_not_a_cut"):
        # ARCH 5: the keeper was still reachable, so neither PASSED nor FAILED
        # is a reading this leg earned. The surviving address is the finding.
        row("ra-room-offline-room-says-so", None, f"cut-not-a-cut: {o['cut_not_a_cut']}",
            cut_not_a_cut=o["cut_not_a_cut"])
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
            cited_during_the_cut=cut_cites, status_online_at_cut=o.get("status_online_at_cut"),
            gaps=len(cut_es.get("gaps") or []),
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
        # RE-READ 2026-09-20. The clause is "BeefyMac's grant list shows the
        # guests with their expiry"; `>= 2` was how many grants rr-2 minted —
        # one per phone — not a property the clause names. Under A55 the wall
        # is one grant however many phones scan it, so the threshold is "at
        # least one live grant listed with its expiry". The per-phone count is
        # what `rg-one-person-across-apps` (d) now reads, and it reads it the
        # other way round: more than one would be the failure.
        legs = {"a_member_sets_unchanged": bool(w.get("members_before")) and w["members_before"] == w["members_after"],
                "b_the_grants_are_listed_with_their_expiry":
                    len([l for l in grants.splitlines() if re.search(r"\blive\b.*\d+[hm]", l)]) >= 1,
                "c_no_other_door": all(refused(v) for r in refusals.values() for v in r.values())}
        row("ra-room-member-only-by-vouch", 1.0 if all(legs.values()) else 0.0, "", legs=legs,
            scans=len(scans), members=w.get("members_after"), refusals=refusals,
            grants=grants.strip().splitlines()[-6:],
            # ARCH 5: the half that cannot be judged says so rather than
            # riding on the half that can.
            affirmative_half="COULD-NOT-JUDGE: the phone runs no node here, so `introduce` cannot be "
                             "observed making one a member (ring-apps ra-11's measurement first)")

# ── ring-guest: the wall's second app ───────────────────────────────────────
# The falsifier for "did we build the class or the demo": the app a person
# would make SECOND, `svrn ring new`, UNMODIFIED, on the same wall through the
# same door with the same phones. Every clause below is a sentence from a
# `floor_basis` in quality/campaigns/ring-guest.toml.
if topology == "room":
    ns = w.get("namespaces") or {}
    scaffold = w.get("scaffold") or {}
    wall2_rows = sightings(d + "-wall2.ndjson")
    guest_one = next((t["string"] for t in (p1.get("typed") or []) if t.get("leg") == "wall"), None)
    on_beefy = {o["id"]: o for o in (w.get("on_beefy") or []) if o.get("id")}
    on_halo = {o["id"]: o for o in (w.get("on_halo") or []) if o.get("id")}
    # The sentence the door composes for a guest's act, built ONCE here from
    # the two names the run read rather than spelled per clause.
    stamped_as = f"{guest_one}, guest of {member}" if guest_one and member else None

    def names_the_guest(op):
        """Does this op, as a replica holds it, name the guest AND the member?"""
        if not op or not guest_one or not member:
            return False
        person = op.get("person") or ""
        return guest_one in person and "guest" in person and member in person

    def everywhere(op_id):
        return names_the_guest(on_beefy.get(op_id)) and names_the_guest(on_halo.get(op_id))

    # 1 — an unmodified scaffold names a guest correctly
    b = bars["rg-second-app-zero-lines"]
    row_w = num(r"within ([\d.]+) s", b["floor_basis"])
    tpl_diff = git("diff", "--stat", f"{guest_base}..HEAD", "--",
                   "sovereign/crates/sovereign-cli-llm/src/ring_cmd/templates")
    spend = act_at(p1, "expense")
    spend_s, spend_row = seen_after(wall2_rows, spend)
    if not scaffold or not p1:
        row("rg-second-app-zero-lines", None,
            "the instrument scaffolded no second app" if not scaffold else "no phone reached the wall")
    else:
        legs = {"a_served_directory_is_the_templates": bool(scaffold.get("equal_to_templates")),
                "b_the_word_guest_appears_zero_times": scaffold.get("guest_mentions") == 0,
                "c_the_wall_row_names_the_guest": spend_s is not None and row_w is not None
                                                  and spend_s <= row_w and bool(spend_row)
                                                  and bool(guest_one) and guest_one in spend_row
                                                  and "guest" in spend_row and bool(member)
                                                  and member in spend_row,
                "d_the_templates_are_untouched": tpl_diff == ""}
        row("rg-second-app-zero-lines", 1.0 if all(legs.values()) else 0.0, "", legs=legs,
            window_s=row_w, wall_row_seen_s=spend_s, wall_showed=spend_row,
            scaffold=scaffold, templates_diff=tpl_diff,
            # `grep -ci guest` counts LINES; this counts OCCURRENCES, which is
            # the stricter of the two and zero exactly when the other is.
            guest_mentions_are_occurrences=True)

    # 2 — the door writes whose words an act was
    liar = p1.get("liar") or {}
    forged = w.get("forged") or {}
    forged_id = forged.get("id")
    stamp_lines = w.get("stamp_lines") or []
    if not liar or not w.get("on_beefy"):
        row("rg-guest-stamped-by-the-door", None,
            "the liar pages did not run" if not liar else "no replica of the scaffold's ring was read")
    else:
        silent, claims = liar.get("silent") or {}, liar.get("claims_another") or {}
        legs = {"a_a_page_that_names_nobody": bool(silent.get("id")) and everywhere(silent["id"]),
                "b_a_page_that_claims_another_name": bool(claims.get("id")) and everywhere(claims["id"])
                                                     and "Somebody Else" not in ((on_beefy.get(claims["id"]) or {}).get("person") or ""),
                # A member's own loopback append carrying a name nobody gave
                # it: refused, or stripped and SAID so. Silence is the failure.
                "c_a_members_forged_name_is_dropped_and_said":
                    (forged_id is None
                     or ((on_beefy.get(forged_id) or {}).get("person") == member
                         and not (on_beefy.get(forged_id) or {}).get("guest")))
                    and any("dropped an on_behalf_of" in l for l in stamp_lines),
                # The keeper verified the signature or the op would not be in
                # its journal at all, and the two journals hash the same.
                "d_it_verifies_on_the_keepers_replica":
                    bool(silent.get("id")) and names_the_guest(on_halo.get(silent["id"]))
                    and bool(w.get("replica_2", [""])[1])
                    and w["replica_2"][0] == w["replica_2"][1]}
        row("rg-guest-stamped-by-the-door", 1.0 if all(legs.values()) else 0.0, "", legs=legs,
            stamped_as=stamped_as, liar=liar,
            what_the_wall_shows_for_each={k: (on_beefy.get((v or {}).get("id")) or {}).get("person")
                                          for k, v in liar.items()},
            forged=forged, forged_on_the_log=on_beefy.get(forged_id),
            stamp_lines=stamp_lines[-3:], replica=w.get("replica_2"))

    # 3 — every act kind names the guest, including the one with no payload
    kinds = ["record", "correct-with-replacement", "correct-with-none"]
    made = {k: next((a for a in (p1.get("acts") or []) if a.get("kind") == k and a.get("ns") == ns.get("expenses")), None)
            for k in kinds}
    if not any(made.values()):
        row("rg-every-act-names-the-guest", None, "the guest made none of the three act kinds")
    else:
        named = {k: bool(a) and everywhere(a["id"]) for k, a in made.items()}
        row("rg-every-act-names-the-guest", 1.0 if all(named.values()) else 0.0, "",
            legs=named, act_kinds_made=len([a for a in made.values() if a]), act_kinds_required=3,
            stamped_as=stamped_as,
            # The retraction is the act kind a reserved payload key could never
            # have stamped: there is no payload to put a name in.
            on_the_wall={k: {"beefy": (on_beefy.get((a or {}).get("id")) or {}).get("person"),
                             "halo": (on_halo.get((a or {}).get("id")) or {}).get("person"),
                             "has_payload": (on_beefy.get((a or {}).get("id")) or {}).get("has_payload")}
                         for k, a in made.items()})

    # 4 — one person, every app, one code
    narrow_x, narrow_d = phone("narrow-expenses"), phone("narrow-doc")
    scans = [x for x in (p1, p2, pc, narrow_x, narrow_d) if x.get("link")]
    on_the_wall_qr = [x for x in (p1, p2, pc) if x.get("link")]
    declared = [v for v in (ns.get("doc"), ns.get("expenses"), ns.get("read_only")) if v]
    # The GUEST-facing grants. `svrn ring show` mints a rail grant of its own
    # for the member's screen — one per app the wall shows — and those are the
    # wall's own page talking to its own daemon on loopback, not a link anybody
    # scanned. Counting them would read "a grant per app" off the member's
    # side of the room (measured 2026-09-20, run 2: two `ring show:` rows).
    live = [l for l in (w.get("grants") or "").splitlines()
            if re.search(r"\blive\b", l) and "ring show:" not in l]
    probes = p1.get("probes") or {}
    # Every element that took this phone's name — the goodhart's own question,
    # answered with the record rather than with an assumption. Exactly one of
    # them may be asking WHO THIS IS, and it must be the door's own prompt.
    # The scaffold's `payer` field also takes the string, and that is a money
    # question ("who paid"), not an identity one: it is listed here so the
    # reading can be argued with rather than quietly excluded.
    took_the_name = [{"where": f"{e['node']}/{e['leg']}", "element": e.get("element") or "unrecorded",
                      "note": e.get("note") or ""}
                     for e in walk if e["string"] == guest_one]
    asked_who_this_is = [t for t in took_the_name if "window.prompt" in t["element"]]
    second_app = (p1.get("apps") or [])[1] if len(p1.get("apps") or []) > 1 else {}
    doc_act = act_at(p1, "wall")
    if len(on_the_wall_qr) < 2 or not w.get("grants"):
        row("rg-one-person-across-apps", None,
            f"the bar needs two phones on one code; {len(on_the_wall_qr)} scanned it")
    else:
        legs = {"a_two_phones_one_code_two_names":
                    p1.get("link") == p2.get("link")
                    and bool(p1.get("apps")) and bool(p2.get("apps"))
                    and (p1["apps"][0].get("guest") or "") != (p2["apps"][0].get("guest") or "")
                    and bool(p1["apps"][0].get("guest")) and bool(p2["apps"][0].get("guest")),
                # The doc's act, as the DOOR stamped it — `op.person` off the
                # log route, which the wall watcher records beside what
                # ring-doc's own page renders from its payload.
                "b_the_same_person_on_the_second_app":
                    second_app.get("asked_a_name") == 0
                    and bool(doc_act)
                    and names_the_guest({"person": next((r.get("person") for r in wall_rows
                                                         if r.get("id") == doc_act["id"]), "")})
                    and len(asked_who_this_is) == 1
                    and len({a.get("guest") for a in (p1.get("apps") or [])}) == 1,
                # 409 is the only status the shim alerts on, so an alert IS the
                # refusal. Under the wall grant one claim covers every app the
                # bearer reaches; the two narrowed grants ask it one app at a
                # time, which is the only way "on both apps" is measurable.
                "c_a_members_name_is_refused_on_both_apps":
                    bool((p2.get("collision") or {}).get("refused"))
                    and bool((narrow_x.get("collision") or {}).get("refused"))
                    and bool((narrow_d.get("collision") or {}).get("refused")),
                "d_one_grant_for_the_wall_and_more_phones_than_apps":
                    len([l for l in live if "the wall" in l]) == 1
                    and not [l for l in live if "rail:" in l]
                    and len(scans) > len(declared)
                    and w.get("members_before") == w.get("members_after"),
                "e_three_refusals_each_naming_the_namespace":
                    len(probes) == 3
                    and all(isinstance(v.get("status"), int) and v["status"] in (401, 403, 404)
                            and v.get("names_it") for v in probes.values())}
        row("rg-one-person-across-apps", 1.0 if all(legs.values()) else 0.0, "", legs=legs,
            phones=len(scans), phones_on_the_wall_code=len(on_the_wall_qr), apps_declared=declared,
            grants_live=live, grants_after_narrowing=(w.get("grants_after_narrowing") or "").strip().splitlines()[-6:],
            names={x.get("label"): [a.get("guest") for a in (x.get("apps") or [])] for x in scans},
            second_app_asked_a_name=second_app.get("asked_a_name"),
            elements_that_took_the_name=took_the_name,
            elements_that_asked_who_this_is=asked_who_this_is,
            doc_act_stamped_as=next((r.get("person") for r in wall_rows
                                     if doc_act and r.get("id") == doc_act["id"]), None),
            collisions={"wall": p2.get("collision"), "narrowed_to_the_expenses_app": narrow_x.get("collision"),
                        "narrowed_to_the_doc": narrow_d.get("collision")},
            refusals=probes,
            # The narrowing knob, recorded and gating nothing: a link scoped to
            # one app reaches it and is refused the other BY NAME.
            narrowing={"reached": [a.get("namespace") for a in (narrow_x.get("apps") or [])],
                       "refused_the_other": (narrow_x.get("probes") or {}).get("other_app")},
            index_the_scan_landed_on=(p1.get("index") or {}).get("apps"))

    # 5 — ring-doc sheds its own guest code
    #
    # The work this bar measures is a LATER row. Until that row lands the bar
    # has nothing to read, and a bar whose work has not happened is not a bar
    # that failed (ARCH 5) — it says which row it is waiting for, by name, and
    # judges itself the moment that row's commit is in the range.
    # By SUBJECT, not by `--grep`: every row's id appears in the bodies of the
    # rows that cite it, and the inventory commit cites this one (run 2 read
    # the deletion as landed off `0f2bba316`).
    ROW = "rg-2-ring-doc-sheds-its-guest-code"
    shed = [l for l in git("log", "--format=%h %s", f"{guest_base}..HEAD").splitlines()
            if l.split(" ", 1)[-1].startswith(ROW + ":")]
    # Leg (a) as corrected in ledger A57, before any datum: non-comment lines
    # outside the ask panel. ONE counter, `scripts/ring-doc-guest-lines.py`,
    # which carries its own self-test; this script does not re-derive it. A
    # counter that cannot run is could-not-judge, never a 0 (ARCH 6).
    doc_files = [os.path.join(REPO, f) for f in
                 ("sovereign/apps/ring-doc/app.js", "sovereign/apps/ring-doc/adapter.js")]
    try:
        counted = json.loads(subprocess.run(
            [sys.executable, os.path.join(REPO, "scripts/ring-doc-guest-lines.py"), *doc_files],
            capture_output=True, text=True, check=True, timeout=30).stdout)
        doc_guest_lines, doc_guest_where = counted["count"], counted["lines"][:8]
        doc_guest_exempt = counted.get("exempt", {})
    except Exception as exc:
        doc_guest_lines, doc_guest_where = None, [f"counter failed: {exc}"]
        doc_guest_exempt = {}
    if doc_guest_lines is None:
        row("rg-ring-doc-sheds-its-guest-code", None, "instrument-missing",
            counter=doc_guest_where)
    elif not shed:
        row("rg-ring-doc-sheds-its-guest-code", None,
            f"the deletion is `{ROW}`, which has not landed in "
            f"{guest_base}..HEAD; ring-doc's app.js and adapter.js still handle a guest on "
            f"{doc_guest_lines} code line(s) outside the ask panel",
            guest_lines_in_ring_doc=doc_guest_lines)
    else:
        rr2 = [rows[x]["value"] for x in
               ("ra-room-scan-to-name", "ra-room-guest-edit-attributed",
                "ra-room-guest-ask-served-by-the-room", "ra-room-film-from-littlemac",
                "ra-room-offline-room-says-so", "ra-room-member-only-by-vouch") if x in rows]
        legs = {"a_no_guest_outside_the_ask_panel": doc_guest_lines == 0,
                "b_the_six_rr2_bars_are_green": len(rr2) == 6 and all(v == 1.0 for v in rr2),
                "c_the_deleted_line_count_is_reported": True}
        row("rg-ring-doc-sheds-its-guest-code", 1.0 if all(legs.values()) else 0.0, "", legs=legs,
            guest_lines_in_ring_doc=doc_guest_lines, guest_lines_where=doc_guest_where,
            ask_panel_exempted=doc_guest_exempt,
            shed_by=shed[:2],
            rr2_values=rr2)

# ── the-link: the checkpoint, frozen mid-cut and verified cold ──────────────
# Every clause is a sentence from tl-checkpoint-verifies' floor_basis in
# quality/campaigns/the-link.toml, read off the offline leg's probe. The two
# bars this instrument cannot measure say so rather than reading 1.0 on
# kindness (ARCH 5): their proof lives in the test suite and the decisions
# appendix, not in this run.
if topology == "room":
    ck = load("room-checkpoint.json") or {}
    if not ck:
        row("tl-checkpoint-verifies", None, "the offline leg ran no checkpoint probe")
    else:
        verify, flipped, truncated, fork = (ck.get(k) or {} for k in
                                            ("verify", "flipped", "truncated", "fork"))
        created, cut, heal = ck.get("created_unix"), ck.get("cut_at"), ck.get("heal_at")
        v_out = verify.get("output") or ""
        legs = {
            # (a) the export's own created_unix sitting strictly inside the cut
            # window is what makes "made while the room was cut" checkable; the
            # keeper's exit 0 printing the marks is the cold verification.
            "a_made_inside_the_cut_verified_cold":
                None not in (created, cut, heal) and cut < created < heal
                and verify.get("exit") == 0 and "verified" in v_out and "mark" in v_out,
            "b_flipped_signature_refused_naming_the_step":
                flipped.get("exit") == 1 and "step 2 (admit)" in (flipped.get("output") or "")
                and "signature" in (flipped.get("output") or ""),
            "c_truncated_tail_refused_naming_the_actor":
                truncated.get("exit") == 1 and "step 3 (digest)" in (truncated.get("output") or "")
                and "marks disagree" in (truncated.get("output") or "")
                and "actor " in (truncated.get("output") or ""),
            "d_same_seq_pair_refused_as_a_fork":
                fork.get("exit") == 1 and "the document forks" in (fork.get("output") or "")}
        row("tl-checkpoint-verifies", 1.0 if all(legs.values()) else 0.0, "", legs=legs,
            ns=ck.get("ns"), created_unix=created, cut_window={"cut_at": cut, "heal_at": heal},
            acts=ck.get("acts"), export_out=ck.get("export_out"),
            verify=v_out[:400], flipped=(flipped.get("output") or "")[:300],
            truncated=(truncated.get("output") or "")[:300], fork=(fork.get("output") or "")[:300],
            fork_ns=fork.get("ns"))
    row("tl-link-carries-its-couriers", None,
        "its proof is the test suite's — builder/parser round-trip, byte-identity, the "
        "fragment refusals and the QR are cargo tests (tl-2-link-carries-its-couriers); "
        "this run mints no at=/iroh= link and exercises none of it")
    row("tl-dial-measured", None,
        "the three numbers land in ralph/DECISIONS.md under tl-3-dial-measured; this "
        "instrument measures none of them")

order = bar_ids.split()
# The table, the floor comparison and the exit rule are scripts/lib/demo_verdicts.py:
# threat-gaps-demo.sh prints the same table, and two copies of one threshold
# rule is the thing ARCH §8 forbids.
sys.exit(demo_verdicts.emit(
    rows, bars, order if want == "all" else [want], artifact=d,
    topology=("four podman nodes on two networks, one host: the wall in the room, "
              "the keeper and the holder behind an uplink, the phone on the room's WiFi only"
              if topology == "room" else
              f"three {os.environ.get('RING_DOC_BACKEND')} nodes, one host")))
PY
}

room_down() { # Jellyfin first: podman will not remove b while a container shares its netns
  [ "$BACKEND" = podman ] && "${PODMAN[@]}" rm -f -t 2 "$JELLY" > /dev/null 2>&1
  [ "$BACKEND" = podman ] && "${PODMAN[@]}" rm -f -t 2 "$JELLY-d" > /dev/null 2>&1
  local n=d; node_kill $n "$D/$n/dev.pid"; node_kill $n "$D/$n/pid"
  [ "$BACKEND" = podman ] && "${PODMAN[@]}" rm -f -t 2 "ring-doc-$n" > /dev/null 2>&1
  cmd_down
}

# Sourced (threat-gaps-demo.sh reuses the node door, room_start_daemon,
# mesh_json and room_cnj_rows): define only, exactly as ring-doc-demo.sh does.
[[ "${BASH_SOURCE[0]}" == "$0" ]] || return 0
case "${1:-}" in
  up)   : > "$CMDLOG"; : > "$TYPED"; if [ "$TOPOLOGY" = room ]; then room_up; else cmd_up; fi ;;
  down) if [ "$TOPOLOGY" = room ]; then room_topology_down; else room_down; fi; echo "stopped" ;;
  verdict)
    # A bar id from ANY of the three campaigns: the-link's three, ring-guest's
    # five, or the room's regression six. One run reports all fourteen.
    python3 -c "
import sys, tomllib
ids = [b['id'] for f in sys.argv[1:4] for b in tomllib.load(open(f,'rb'))['bar']]
sys.exit(0 if sys.argv[4] in ids + ['all'] else 1)" \
      "$LINK_CAMPAIGN" "$GUEST_CAMPAIGN" "$ROOM_CAMPAIGN" "${2:-}" \
      || { echo "verdict: name a bar or all — see quality/campaigns/the-link.toml, ring-guest.toml and ring-room.toml" >&2; exit 2; }
    # The demo builds what it measures, or it says so and judges nothing.
    stale=$(stale_binaries)
    [ -z "$stale" ] || { room_cnj_rows "$stale" "$2"; echo "$stale" >&2; exit 3; }
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
