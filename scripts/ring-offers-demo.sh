#!/usr/bin/env bash
# ring-offers-demo.sh — the ra-4 demo, and the instrument behind its two bars.
#
# Four throwaway daemons on ONE host, one mesh, two of them publishing an offer
# origin and two publishing none. `svrn mesh offers` run from a fifth role (the
# caller) then shows what `ra-offers-catalogue-computed` claims: a SERVED row
# from a peer's offer origin and a NEVER_ASKED row naming a peer that publishes
# none, in the SAME response — and `--why` shows what
# `ra-seller-carries-their-vouch` claims.
#
# WHY FOUR DAEMONS AND NOT A UNIT TEST. `origin_fanout` never asks this node
# (origin_fanout.rs), so a catalogue can only come from a peer; and
# `hm-no-shared-credentials` records a run where a reachability probe passed
# while every real fetch 401'd. The verdict below reads the FANOUT ROWS.
#
# WHY THROWAWAY DAEMONS AND NOT THE OPERATOR'S. Every node here has its own
# `SOVEREIGN_DATA_DIR`, its own ports and its own node key. Nothing touches
# ~/.svrnmesh or the daemon on :9741, so this needs no restart of anything a
# person is using and no coordination with a peer session.
#
#   scripts/ring-offers-demo.sh up                 bring the four up and join them
#   scripts/ring-offers-demo.sh show               the demo: offers, then --why
#   scripts/ring-offers-demo.sh verdict <bar-id>   bring up, measure, tear down
#   scripts/ring-offers-demo.sh down               stop everything
#
# `verdict` prints a co-lineage measurement line as its LAST line
# (`{"value": N, "artifact": "…"}`) and is what `quality/campaigns/ring-apps.toml`
# names as the instrument for both bars.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
D="${RING_OFFERS_DIR:-${TMPDIR:-/tmp}/ring-offers-demo}"
DAEMON="$REPO/target/debug/sovereign-cli-daemon"
CLI="$REPO/target/debug/sovereign-cli"
export SOVEREIGN_NO_STALE_WARN=1

# Five ports per node, well clear of the daemon's own 9741/9742.
ADA_C=19741;   ADA_I=19742
MIRA_C=19751;  MIRA_I=19752;  MIRA_O=18711
JONAS_C=19761; JONAS_I=19762; JONAS_O=18712
SAM_C=19771;   SAM_I=19772

sv() { local n=$1; shift; SOVEREIGN_DATA_DIR="$D/$n" "$CLI" "$@" 2>/dev/null | grep -v '^svrnmesh: bridged'; }

need_binaries() {
  # Exit 3 is co-lineage's "artifact-absent": the instrument could not run, and
  # that is a could-not-judge rather than a failed bar (ARCH §18.2).
  [ -x "$DAEMON" ] && [ -x "$CLI" ] && return 0
  echo "ring-offers-demo: build first — cargo build --bins --features sovereign-cli/dev-tools" >&2
  exit 3
}

mkcfg() { # name client internal [offer-port]
  local dir="$D/$1"; mkdir -p "$dir"
  {
    echo 'mcp_servers = []'; echo
    echo '[daemon]'
    echo "client_port = $2"; echo "internal_port = $3"
    echo 'autostart = false'
    echo 'client_bind = "127.0.0.1"'; echo 'internal_bind = "127.0.0.1"'; echo
    echo '[data]'; echo "dir = \"$dir\""; echo
    # A terminal node: it holds no weights, which is all this demo needs and
    # keeps four daemons cheap (~190 MB RSS each, no model loaded).
    echo '[node]'; echo 'entry = "http://127.0.0.1:9741/v1"'; echo
    echo '[iroh]'; echo 'enabled = true'
    [ -n "${4:-}" ] && echo "offer_origin = \"127.0.0.1:$4\""
    echo
    # No mDNS: these four must not meet the real house on this LAN. They find
    # each other through the `?relay=` hint on the join link instead.
    echo '[discovery]'; echo 'mdns = false'; echo 'seed_addrs = []'
  } > "$dir/config.toml"
}

origin_py() {
  cat > "$D/origin.py" <<'PY'
#!/usr/bin/env python3
"""A house's offer origin: one file, served on loopback, over HTTP.

Deliberately the dumbest server that could work — the substrate defines no
item schema and merges nothing, so an offer origin is whatever HTTP server its
operator already runs. It logs the `X-Mesh-Member` the HOLDER'S acceptor added,
which is how the demo shows the seller learns who is asking while the caller
holds no credential of the seller's.
"""
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(sys.argv[1]); BODY = open(sys.argv[2], "rb").read()

class H(BaseHTTPRequestHandler):
    def do_GET(self):
        who = self.headers.get("X-Mesh-Member", "(nobody named)")
        sys.stderr.write(f"asked by {who} for {self.path}\n"); sys.stderr.flush()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(BODY)))
        self.end_headers()
        self.wfile.write(BODY)
    def log_message(self, *a): pass

HTTPServer(("127.0.0.1", PORT), H).serve_forever()
PY
  # One catalogue is a JSON ARRAY and one an OBJECT, on purpose: the renderer
  # counts array elements and says nothing about the object, which is the
  # no-item-schema rule visible in the output.
  cat > "$D/offers-mira.json" <<'J'
[
  {"thing": "cordless drill", "terms": "lend, back by Sunday", "where": "hall cupboard"},
  {"thing": "6 eggs", "terms": "free — the hens are ahead of us"},
  {"thing": "roof ladder", "terms": "lend, ask first"},
  {"thing": "sourdough starter", "terms": "free, bring a jar"}
]
J
  cat > "$D/offers-jonas.json" <<'J'
{"catalogue": "Jonas's shed", "updated": "today",
 "items": [{"name": "trailer", "terms": "£10/day"},
           {"name": "post-hole digger", "terms": "lend"}]}
J
}

start_daemon() { local n=$1; SOVEREIGN_DATA_DIR="$D/$n" "$DAEMON" daemon run > "$D/$n/daemon.out" 2> "$D/$n/daemon.err" & echo $! > "$D/$n/pid"; }

# ONE deadline for ALL four, not one each. Four debug daemons cold-booting at
# once on a loaded machine take well over a minute between them, and a
# per-node budget multiplies that by four for no gain — they are starting
# concurrently, so the only question is when the LAST one answers.
wait_all_up() {
  local deadline=$(( $(date +%s) + 240 )) p missing
  while [ "$(date +%s)" -lt "$deadline" ]; do
    missing=""
    for p in "$@"; do
      curl -s --max-time 2 "http://127.0.0.1:$p/status" >/dev/null 2>&1 || missing="$missing $p"
    done
    [ -z "$missing" ] && return 0
    sleep 3
  done
  echo "daemons on$missing never answered /status inside 240s" >&2
  return 1
}

join_one() { # display-name client-port
  # The name goes through the HTTP route because the CLI has no --name and all
  # four nodes share one hostname; the `?relay=` hint is how a joiner reaches
  # the founder with mDNS off. The key is rotated per joiner — `mesh rotate`
  # invalidates the previous one by design.
  local key
  key=$(sv ada mesh rotate | awk '/Join key:/{print $3}')
  curl -s --max-time 90 -X POST "http://127.0.0.1:$2/v1/mesh/join" \
    -H 'content-type: application/json' \
    -d "{\"key_or_url\":\"https://sovereign.dev/join/$key?relay=127.0.0.1:$ADA_I\",\"node_name\":\"$1\"}" \
    > "$D/join-$1.json"
}

cmd_up() {
  need_binaries
  mkdir -p "$D"; origin_py
  mkcfg ada   $ADA_C   $ADA_I
  mkcfg mira  $MIRA_C  $MIRA_I  $MIRA_O
  mkcfg jonas $JONAS_C $JONAS_I $JONAS_O
  mkcfg sam   $SAM_C   $SAM_I
  python3 "$D/origin.py" $MIRA_O  "$D/offers-mira.json"  > "$D/origin-mira.log"  2>&1 & echo $! > "$D/origin-mira.pid"
  python3 "$D/origin.py" $JONAS_O "$D/offers-jonas.json" > "$D/origin-jonas.log" 2>&1 & echo $! > "$D/origin-jonas.pid"
  for n in ada mira jonas sam; do start_daemon $n; done
  wait_all_up $ADA_C $MIRA_C $JONAS_C $SAM_C || exit 3
  join_one Mira  $MIRA_C
  join_one Jonas $JONAS_C
  join_one Sam   $SAM_C
  # One gossip round, so `origins` has crossed before anything is asked.
  sleep 12
  seed_ring
  echo "up: ada(caller) mira(offers,vouched) jonas(offers,no warrant) sam(no offer origin)"
}

seed_ring() {
  # The ring the `--why` join reads. Ada admits herself (a founder has nobody
  # to vouch for them), introduces Mira and admits her ON that act, and admits
  # Jonas with NO warrant — the row every roster written before ra-1 has.
  # The MIXED run is the instrument: a uniform one cannot tell a working join
  # from a stuck one.
  local mira jonas op
  mira=$(curl -s "http://127.0.0.1:$ADA_C/v1/mesh/status" | python3 -c "import sys,json;d=json.load(sys.stdin);print(next((m['node_pubkey'] for m in d['members'] if m['name']=='Mira'), ''))")
  jonas=$(curl -s "http://127.0.0.1:$ADA_C/v1/mesh/status" | python3 -c "import sys,json;d=json.load(sys.stdin);print(next((m['node_pubkey'] for m in d['members'] if m['name']=='Jonas'), ''))")
  [ -z "$mira" ] && { echo "Mira never reached the roster — nothing to vouch for" >&2; return; }
  sv ada ring roster add ada --self --ring house-things > /dev/null
  op=$(sv ada ring introduce Mira --key "$mira" --reason "sold me the drill in June; her eggs are good" --ring house-things | awk '/^  op:/{print $2}')
  sv ada ring roster add Mira --key "$mira" --on "$op" --ring house-things > /dev/null
  [ -n "$jonas" ] && sv ada ring roster add Jonas --key "$jonas" --ring house-things > /dev/null
}

cmd_down() {
  local n
  for n in ada mira jonas sam; do [ -f "$D/$n/pid" ] && kill "$(cat "$D/$n/pid")" 2>/dev/null; done
  for n in mira jonas; do [ -f "$D/origin-$n.pid" ] && kill "$(cat "$D/origin-$n.pid")" 2>/dev/null; done
  sleep 1
}

cmd_show() {
  echo "\$ svrn mesh offers"; sv ada mesh offers
  echo; echo "\$ svrn mesh offers --why"; sv ada mesh offers --why
}

# ── the verdicts ────────────────────────────────────────────────────────────

verdict_catalogue() {
  sv ada mesh offers --json > "$D/catalogue.json"
  python3 - "$D/catalogue.json" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
rows = doc.get("rows", [])
served = [r for r in rows if r.get("verdict") == "served" and 200 <= r.get("status", 0) < 300]
# A never_asked row is trivially produced by naming a peer that does not exist
# (goodhart). So the row must name a REAL member — a non-zero node id — and its
# reason must be about the OFFER origin, not about an unknown name.
never = [r for r in rows
         if r.get("verdict") == "never_asked"
         and "offer origin" in (r.get("reason") or "")
         and set((r.get("node_id") or "").replace("node-", "")) != {"0"}]
ok = 1.0 if served and never else 0.0
print(f"served rows: {[r['name'] for r in served]}")
print(f"never_asked rows naming an offer origin: {[r['name'] for r in never]}")
print(f"asked: {doc.get('asked')}  kind: {doc.get('kind')}")
print(json.dumps({"value": ok, "artifact": sys.argv[1]}))
PY
}

verdict_vouch() {
  sv ada mesh offers --why > "$D/why.txt"
  python3 - "$D/why.txt" <<'PY'
import json, re, sys
text = open(sys.argv[1]).read()
vouches = re.findall(r"^      vouch: (.*)$", text, re.M)
names = re.findall(r"^  (\S+)\s", text, re.M)
resolved = [v for v in vouches if v.startswith("introduced by") and " op " in v]
unknown  = [v for v in vouches if v.startswith("warrant ")]
blank    = [v for v in vouches if not v.strip()]
rows = len(names)
print(f"seller rows: {rows}   vouch lines: {len(vouches)}")
print(f"  resolved:      {len(resolved)}  {resolved}")
print(f"  warrant-known-unknown: {len(unknown)}")
print(f"  BLANK (a failure): {len(blank)}")
# The goodhart guard, and it is the whole reason this is not just a count:
# 1.0 with every row reading `unknown` is a perfect score for a join that never
# ran, so a run with no RESOLVED row scores zero however tidy it looks.
if rows == 0 or not resolved:
    value = 0.0
    print("  no row named a person and an op — the join is unproven, scoring 0")
else:
    value = (len(resolved) + len(unknown)) / rows
print(json.dumps({"value": value, "artifact": sys.argv[1]}))
PY
}

case "${1:-show}" in
  up)   cmd_up ;;
  down) cmd_down; echo "stopped" ;;
  show) cmd_show ;;
  verdict)
    need_binaries
    trap cmd_down EXIT
    cmd_up > "${D}-up.log" 2>&1 || { echo "bring-up failed, see ${D}-up.log" >&2; exit 3; }
    case "${2:-}" in
      ra-offers-catalogue-computed) verdict_catalogue ;;
      ra-seller-carries-their-vouch) verdict_vouch ;;
      *) echo "verdict: name a bar — ra-offers-catalogue-computed | ra-seller-carries-their-vouch" >&2; exit 2 ;;
    esac
    ;;
  *) sed -n '2,26p' "$0"; exit 2 ;;
esac
