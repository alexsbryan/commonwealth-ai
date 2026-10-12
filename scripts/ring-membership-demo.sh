#!/usr/bin/env bash
# ring-membership-demo.sh — demo 5 (ra-5): undo a leaked invite in one act,
# and every node agrees who is in.
#
# THE STORY, three machines: Ada founds a ring with Mira; a link Ada minted
# leaks and a stranger redeems it (Ada's `Admit` names the key the link
# carried — "Sam"); Sam, now standing, lets one person of their own in
# ("Friendo"); ANY member — Mira, not the founder — voids Sam's `Admit` with
# one `Correct`; Sam and Friendo both fall away, on every node, and the
# roster files never move.
#
# WHY THREE DAEMONS AND NOT A UNIT TEST. The five legs of
# `ra-membership-is-order-free` prove the fold at unit level, permutations
# included (banked, 95e53c17b + c5e2b35a8). What only a sitting proves: real
# daemons, real iroh, real ring-sync — three nodes that arrived at the op
# set through different doors and different orders (sam's node authored the
# friend act locally; ada and mira received it after the void was already
# signed) still compute ONE membership. Plus the negative leg no fold can
# check: `roster.json` on every node is byte-identical start to finish
# (`ra-roster-stays-independent` at 0).
#
# THE ONE STAGED ACT, named not hidden. Every append door refuses a signer
# the roster FILE does not name (`RingJournal::append`'s `NotInRoster`), so
# Sam — standing through an `Admit`, exactly the amendment's "any admitted
# member may write either" — has no door yet (the join surface is ra-6's).
# And sync refuses the stranger's NODE the same way (a ring is served to
# its roster — recorded below as a leg), so the act is staged into MIRA'S
# journal — a node the product will carry it from — through
# `RingJournal::ingest`, the same entry sync uses for peer ops, signed by
# the crate's own signer over the crate's own digest. The SIGNATURE is the
# stranger's; only the first disk is Mira's. Everything else — Ada's
# Admit, Mira's void, every read — is the product's own path.
#
# THE FINDING this sitting names for ra-6: membership lives in the fold
# (seed + Admit/Remove acts), but the transport gate serves the roster
# FILE — a member by act cannot receive the ring, push to it, or write
# through any door. The gap between "any admitted member may write either"
# and what the doors and sync will carry is the join surface's work.
#
# TOPOLOGY, named because the goodhart demands it: three nodes on ONE host,
# one clock, one mesh (the join link — the leak — is ada's mesh key; mdns
# off, they meet only through it). A node is a `cw-rails` — its mesh
# endpoint and, since pb-mesh-exit-transport, the holder of its ring
# journals and the side that runs ring-sync — and the daemon that dials it
# through `[daemon] rails_base`, whose /v1/rail/* doors forward there. Each
# cw-rails is local-only (no relay, no --mdns): the three dial each other by
# the direct addresses the invite and gossip carry. Nothing here says
# anything about cross-host latency.
#
#   scripts/ring-membership-demo.sh sitting    up + the demo + down
#   scripts/ring-membership-demo.sh up         bring the three up, join, seed
#   scripts/ring-membership-demo.sh run        the sitting (needs `up`)
#   scripts/ring-membership-demo.sh down       stop everything
#
# Exit 0 only when every leg held; the artifact is $D/summary.json.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
D="${RING_MEMBERSHIP_DIR:-${TMPDIR:-/tmp}/ring-membership-demo}"
DAEMON="$REPO/target/debug/sovereign-cli-daemon"
CLI="$REPO/target/debug/sovereign-cli"
# CW_RAILS_BIN: the name `svrn mesh up` reads too (rails_up.rs locate_rails).
RAILS="${CW_RAILS_BIN:-$REPO/target/debug/cw-rails}"
STAGER="$REPO/target/debug/examples/stage_member_act"
RING=house-ring
export SOVEREIGN_NO_STALE_WARN=1

ADA_C=19941;  ADA_I=19942
MIRA_C=19951; MIRA_I=19952
SAM_C=19961;  SAM_I=19962
# svrn binds client, internal and its ring rail at client + 2 (guest_pages.rs
# rail_port); cw-rails' API is client + 5 and serve (hosted in the daemon)
# client + 6, so nothing falls back to the house's :9747/:9748.
declare -A CPORT=([ada]=$ADA_C [mira]=$MIRA_C [sam]=$SAM_C)
rport() { echo $(( CPORT[$1] + 5 )); }
sport() { echo $(( CPORT[$1] + 6 )); }
rails_url() { echo "http://127.0.0.1:$(rport "$1")"; }
# cw-rails' root, beside the daemon's data dir: where the ring journals and
# roster files live now.
rdir() { echo "$D/$1-rails"; }

# Deterministic on purpose: reruns reproduce the same actors and op ids.
SAM_SEED="a5$(printf 'd1%.0s' {1..31})"
FRIEND_SEED="b7$(printf 'e2%.0s' {1..31})"

# A CLI call on node n. CW_RAILS_DIR is load-bearing: `ring roster add`
# writes under it, and unset it is the house's ~/.commonwealth-rails.
sv() { local n=$1; shift; SOVEREIGN_DATA_DIR="$D/$n" SOVEREIGN_SERVE_PORT="$(sport "$n")" CW_RAILS_DIR="$(rdir "$n")" "$CLI" "$@" 2>/dev/null | grep -v '^svrnmesh: bridged'; }

need_binaries() {
  local missing=0
  for b in "$DAEMON" "$CLI" "$RAILS" "$STAGER"; do [ -x "$b" ] || { echo "missing: $b" >&2; missing=1; }; done
  [ "$missing" = 0 ] && return 0
  echo "build first:" >&2
  echo "  cargo build -p sovereign-cli-daemon -p sovereign-cli --features sovereign-cli/dev-tools -p commonwealth-rails -p commonwealth-rail --example stage_member_act" >&2
  exit 3
}

mkcfg() { # name client internal
  local dir="$D/$1" rd; rd=$(rdir "$1"); mkdir -p "$dir" "$rd"
  {
    echo 'mcp_servers = []'; echo
    echo '[daemon]'
    echo "client_port = $2"; echo "internal_port = $3"
    echo "rails_base = \"$(rails_url "$1")\""
    echo 'autostart = false'
    echo 'client_bind = "127.0.0.1"'; echo 'internal_bind = "127.0.0.1"'; echo
    echo '[data]'; echo "dir = \"$dir\""; echo
    echo '[node]'; echo 'entry = "http://127.0.0.1:9741"'; echo
    echo '[discovery]'; echo 'mdns = false'; echo 'seed_addrs = []'
  } > "$dir/config.toml"
  # cw-rails' own config: `name` is the member name the mesh shows and what
  # /v1/mesh/join's node_name must equal (membership.rs check_node_name).
  printf 'name = "%s"\nlisten = %s\n\n[relay]\ndiscovery = "none"\n' "${1^}" "$(rport "$1")" > "$rd/rails.toml"
  # One node identity in both roots, as `svrn mesh up`'s handover leaves it
  # (identity_handover.rs), before either boots.
  [ -f "$rd/node_id" ] || head -c 16 /dev/urandom > "$rd/node_id"
  cp "$rd/node_id" "$dir/node_id"
}

# Ring-sync is cw-rails' now, so its log is where the roster gate's refusal
# lands (ring_routes.rs, a warn on target `rails`, which info shows).
start_rails() { local n=$1; RUST_LOG="${RING_MEMBERSHIP_RUST_LOG:-info}" "$RAILS" run --data-dir "$(rdir "$n")" > "$D/$n/rails.out" 2> "$D/$n/rails.err" & echo $! > "$D/$n/rails.pid"; }
start_daemon() { local n=$1; SOVEREIGN_DATA_DIR="$D/$n" SOVEREIGN_SERVE_PORT="$(sport "$n")" CW_RAILS_DIR="$(rdir "$n")" RUST_LOG=warn "$DAEMON" daemon run > "$D/$n/daemon.out" 2> "$D/$n/daemon.err" & echo $! > "$D/$n/pid"; }

wait_rails_up() { # node…
  local deadline=$(( $(date +%s) + 60 )) n missing
  while [ "$(date +%s)" -lt "$deadline" ]; do
    missing=""
    for n in "$@"; do curl -sf --max-time 2 -o /dev/null "$(rails_url "$n")/v1/mesh/status" || missing="$missing $n"; done
    [ -z "$missing" ] && return 0
    sleep 1
  done
  echo "cw-rails on$missing never answered /v1/mesh/status inside 60s" >&2
  return 1
}

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

join_one() { # node-name node — through ada's join link, verbatim.
  # $JOIN_LINK is the invite ada's cw-rails answered `create` with: the
  # product's own link is the leak this demo undoes (it carries ada's iroh
  # dial — how a joiner on this host reaches the founder, mDNS off).
  local link="${JOIN_LINK:-}"
  [ -n "$link" ] || { echo "join: no join link was captured at up" >&2; return 1; }
  curl -s --max-time 90 -X POST "$(rails_url "$2")/v1/mesh/join" \
    -H 'content-type: application/json' \
    -d "{\"key_or_url\":\"$link\",\"node_name\":\"$1\"}" \
    > "$D/join-$1.json"
  if grep -q '"error"' "$D/join-$1.json"; then
    echo "join: $1 was refused: $(cat "$D/join-$1.json")" >&2; return 1
  fi
}

key_of() { # node name -> node_pubkey from the FOUNDER's mesh status (its cw-rails)
  curl -s --max-time 5 "$(rails_url "$1")/v1/mesh/status" \
    | python3 -c "import sys,json;d=json.load(sys.stdin);print(next((m['node_pubkey'] for m in d['members'] if m['name']=='$2'), ''))"
}

seed_rosters() {
  # The founder's row is --self (its key never crossed a wire); its member
  # name on every node's status is the `name` its rails.toml gives, "Ada",
  # which is how mira and sam find ada's key.
  local ada_key mira_key
  ada_key=$(key_of ada Ada)
  mira_key=$(key_of ada Mira)
  [ -n "$ada_key" ] && [ -n "$mira_key" ] || { echo "seed: the founder never saw both keys (ada='$ada_key' mira='$mira_key')" >&2; return 1; }
  # The SAME two lines on every node: the seed is the pre-amendment ring's
  # file, and the stranger is in NO seed anywhere — that is the point.
  sv ada  ring roster add Ada  --self           --ring "$RING" > /dev/null
  sv ada  ring roster add Mira --key "$mira_key" --ring "$RING" > /dev/null
  sv mira ring roster add Ada  --key "$ada_key"  --ring "$RING" > /dev/null
  sv mira ring roster add Mira --self           --ring "$RING" > /dev/null
  sv sam  ring roster add Ada  --key "$ada_key"  --ring "$RING" > /dev/null
  sv sam  ring roster add Mira --key "$mira_key" --ring "$RING" > /dev/null
}

wait_members() { # the founder's mesh status names all three WITH keys, or nothing starts
  # Counted, not name-matched: three members with keys is the state,
  # whatever they are called.
  local deadline=$(( $(date +%s) + 90 )) have=0
  while [ "$(date +%s)" -lt "$deadline" ]; do
    have=$(curl -s --max-time 5 "$(rails_url ada)/v1/mesh/status" \
      | python3 -c '
import sys, json
try: ms = json.load(sys.stdin)["members"]
except Exception: print(0); raise SystemExit
print(len([m for m in ms if m.get("node_pubkey")]))' 2>/dev/null)
    [ "${have:-0}" = 3 ] && return 0
    sleep 3
  done
  echo "seed: the founder saw ${have}/3 members with keys inside 90s" >&2
  return 1
}

# Ada's cw-rails founds the mesh (`POST /v1/mesh/create`, what `svrn mesh
# create` sends after its bring-up, which this script must not run: it would
# install a cw-rails user unit). Its answer carries the invite — the leak —
# with ada's iroh dial (`iroh=`); cw-rails refuses a join with none (join.rs
# NoIrohDial). The daemon-era wait for a relay home is gone with the relay:
# cw-rails stamps its own dial before a joiner can read it (69c023a45).
found_mesh() {
  local out code
  out=$(curl -s --max-time 20 -w $'\n%{http_code}' -X POST "$(rails_url ada)/v1/mesh/create" \
    -H 'content-type: application/json' -d '{"name":"house","node_name":"Ada"}')
  code="${out##*$'\n'}"; out="${out%$'\n'*}"
  JOIN_LINK=$(printf '%s' "$out" | python3 -c 'import sys,json; print(json.load(sys.stdin).get("join_link") or "")' 2>/dev/null)
  case "$code:$JOIN_LINK" in 200:*iroh=*) return 0;; esac
  echo "up: ada's cw-rails answered $code with no iroh= invite: ${out:0:300}" >&2
  return 1
}

cmd_up() {
  need_binaries
  mkdir -p "$D"
  sweep_ports
  # Throwaway nodes, wiped on every up: mesh state persists across daemon
  # restarts by design, and a rerun must not inherit the last sitting's
  # roster, join keys or members.
  for n in ada mira sam; do rm -rf "$D/$n" "$(rdir $n)"; done
  rm -f "$D"/join-*.json "$D"/admit-*.json "$D"/void-*.json "$D"/walk-*.json "$D"/summary.json
  mkcfg ada $ADA_C $ADA_I; mkcfg mira $MIRA_C $MIRA_I; mkcfg sam $SAM_C $SAM_I
  for n in ada mira sam; do start_rails $n; done
  wait_rails_up ada mira sam || exit 3
  for n in ada mira sam; do start_daemon $n; done
  wait_all_up $ADA_C $MIRA_C $SAM_C || exit 3
  found_mesh || exit 3
  join_one Mira mira || exit 3
  join_one Sam  sam || exit 3
  wait_members || exit 3
  seed_rosters || exit 3
  echo "up: ada(founder) mira(member) sam(the stranger's machine, on the mesh through ada's link)"
}

sweep_ports() {
  # This demo's own range only: a crashed run can leave daemons whose pid
  # file the next start overwrote, and `wait_all_up` then passes against a
  # ghost whose data dir was wiped underneath it (measured 2026-09-30:
  # mira's "join" answered from a daemon born two runs earlier).
  local p pid
  for p in $ADA_C $ADA_I $MIRA_C $MIRA_I $SAM_C $SAM_I \
           $(rport ada) $(sport ada) $(rport mira) $(sport mira) $(rport sam) $(sport sam); do
    pid=$(lsof -ti :$p 2>/dev/null)
    [ -n "$pid" ] && kill $pid 2>/dev/null
  done
  sleep 1
}

cmd_down() {
  local n
  for n in ada mira sam; do
    [ -f "$D/$n/pid" ] && kill "$(cat "$D/$n/pid")" 2>/dev/null
    [ -f "$D/$n/rails.pid" ] && kill "$(cat "$D/$n/rails.pid")" 2>/dev/null
  done
  sweep_ports
}

# ── the sitting's reads and waits ────────────────────────────────────────────

membership_json() { curl -s --max-time 10 "http://127.0.0.1:$1/v1/rail/membership?namespace=$RING"; }

log_json() { curl -s --max-time 10 "http://127.0.0.1:$1/v1/rail/log?namespace=$RING"; }

ops_held() { log_json "$1" | python3 -c 'import sys,json; d=json.load(sys.stdin); print(d.get("held", 0))' 2>/dev/null; }

# Wait until the nodes that HOLD the record agree: equal held lines and an
# equal membership walk. Two conditions, because either alone can lie: equal
# held with divergent membership is the A1-shaped failure, equal membership
# with unequal held is a sync still in flight. Sam's node is not in this
# wait by design — the roster gate refuses it (the leg below records the
# refusal), so it will never hold the lines.
wait_agrees() { # timeout-s then client-ports...
  local timeout=$1; shift
  local deadline=$(( $(date +%s) + timeout ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    if python3 - "$@" <<'PY'
import json, sys, urllib.request
ports = [int(p) for p in sys.argv[1:]]
held, walks = [], []
for p in ports:
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{p}/v1/rail/log?namespace=house-ring", timeout=8) as r:
            held.append(json.load(r).get("held"))
        with urllib.request.urlopen(f"http://127.0.0.1:{p}/v1/rail/membership?namespace=house-ring", timeout=8) as r:
            walks.append(json.dumps(json.load(r).get("membership"), sort_keys=True))
    except Exception:
        sys.exit(1)  # a node that will not answer is not agreement
sys.exit(0 if len(set(held)) == 1 and len(set(walks)) == 1 else 1)
PY
    then return 0; fi
    sleep 3
  done
  echo "the holders did not agree inside ${timeout}s:" >&2
  for p in "$@"; do
    echo "  :$p held=$(ops_held $p 2>/dev/null)" >&2
  done
  return 1
}

# Hashed by python3's hashlib, the judge's own `roster_sha`, so the before
# and after columns cannot differ by tool: `shasum` is absent on Fedora, and
# its empty output read as three blank hashes the judge could never match.
roster_hashes() {
  local n
  for n in ada mira sam; do
    python3 -c 'import hashlib,os,sys; p=sys.argv[1]; print(hashlib.sha256(open(p,"rb").read()).hexdigest() if os.path.exists(p) else "absent", end=" ")' \
      "$(rdir $n)/rings/$RING/roster.json"
  done
}

cmd_run() {
  need_binaries
  mkdir -p "$D"
  local t0 t_admit t_void sam_key friend_key admit_id void_id
  t0=$(date +%s)
  sam_key=$("$STAGER" --print-actor "$SAM_SEED")
  friend_key=$("$STAGER" --print-actor "$FRIEND_SEED")
  [ ${#sam_key} = 64 ] && [ ${#friend_key} = 64 ] || { echo "stager produced no actor: '$sam_key' / '$friend_key'" >&2; exit 3; }
  # The holders whose agreement the sitting asserts. Sam's node is outside
  # this set on purpose — see the header's finding.
  local HOLDERS="$ADA_C $MIRA_C"

  echo "== the seed: Ada and Mira, identical roster files on all three nodes"
  local roster_before; roster_before="$(roster_hashes)"
  echo "   $roster_before"

  echo "== the leak: Ada's Admit names the key the link carried (Sam, a stranger)"
  curl -s --max-time 20 -X POST "http://127.0.0.1:$ADA_C/v1/rail/append?namespace=$RING" \
    -H 'content-type: application/json' \
    -d "{\"op\":\"admit\",\"person\":\"Sam\",\"key\":\"$sam_key\"}" > "$D/admit-sam.json"
  admit_id=$(python3 -c 'import json;print(json.load(open("'"$D"'/admit-sam.json")).get("id",""))' 2>/dev/null)
  [ -n "$admit_id" ] || { echo "Ada's door refused the Admit: $(cat "$D/admit-sam.json")" >&2; exit 3; }
  echo "   $admit_id"
  wait_agrees 90 $HOLDERS || exit 1
  t_admit=$(( $(date +%s) - t0 ))

  echo "== the stranger lets one person in: Sam's OWN act (staged — no door for an"
  echo "   act-standing key yet), entering through Mira's journal: the stranger's"
  echo "   node is refused at the roster gate and cannot carry the ring."
  # Mira's node is quiescent here: it authored nothing since the sync above.
  "$STAGER" --root "$(rdir mira)" --namespace "$RING" --seed "$SAM_SEED" \
    --person Friendo --admit-key "$friend_key" > "$D/admit-friend.txt" || exit 3
  sed 's/^/   /' "$D/admit-friend.txt"
  wait_agrees 90 $HOLDERS || exit 1
  # Captured BEFORE the void, because "both fell" is indistinguishable from
  # "never in" without it — the goodhart guard the summary's first leg reads.
  membership_json $ADA_C > "$D/walk-before-void.json"

  echo "== the undo: MIRA (any member, not the founder) voids Sam's Admit in one act"
  curl -s --max-time 20 -X POST "http://127.0.0.1:$MIRA_C/v1/rail/append?namespace=$RING" \
    -H 'content-type: application/json' \
    -d "{\"op\":\"correct\",\"corrects\":\"$admit_id\"}" > "$D/void-sam.json"
  void_id=$(python3 -c 'import json;print(json.load(open("'"$D"'/void-sam.json")).get("id",""))' 2>/dev/null)
  [ -n "$void_id" ] || { echo "Mira's door refused the void: $(cat "$D/void-sam.json")" >&2; exit 3; }
  echo "   $void_id"
  wait_agrees 90 $HOLDERS || exit 1
  t_void=$(( $(date +%s) - t0 ))

  for n in ada mira sam; do
    echo "-- $n: $(sv $n ring membership $RING | tail -n +2 | tr '\n' '|' | sed 's/|$//')"
  done

  echo "== judging"
  python3 - "$D" "$sam_key" "$friend_key" "$roster_before" "$admit_id" "$t_admit" "$t_void" <<'PY'
import hashlib, json, os, sys, urllib.request
d, sam_key, friend_key = sys.argv[1:4]
roster_before = sys.argv[4].split()
admit_id, t_admit, t_void = sys.argv[5], int(sys.argv[6]), int(sys.argv[7])
ports = {"ada": 19941, "mira": 19951, "sam": 19961}
holders = ["ada", "mira"]

def get(node, path):
    with urllib.request.urlopen(f"http://127.0.0.1:{ports[node]}{path}", timeout=10) as r:
        return json.load(r)

def walk(node):
    return get(node, f"/v1/rail/membership?namespace=house-ring")["membership"]

def names_of(m):
    """Who is in: the person names standing keys resolve to, sorted."""
    return sorted(m["bindings"].get(k, "?") for k in m["standing"])

def roster_sha(node):
    p = os.path.join(d, node + "-rails", "rings", "house-ring", "roster.json")
    return hashlib.sha256(open(p, "rb").read()).hexdigest() if os.path.exists(p) else "absent"

def rails_log(node):
    # Ring-sync, and so the roster gate's refusal, is cw-rails'.
    p = os.path.join(d, node, "rails.err")
    return open(p, errors="replace").read() if os.path.exists(p) else ""

legs = {}
w = {n: walk(n) for n in ports}
# Leg 0 — the goodhart guard, read FIRST: before the void both were IN on
# every holder, so the fall below is a change the undo caused, not a constant.
before = json.load(open(os.path.join(d, "walk-before-void.json")))["membership"]
legs["stranger_and_friend_were_in_before_void"] = (
    sam_key in before["standing"] and friend_key in before["standing"])
# Leg 1 — the undo held on EVERY holder: the seed pair stands, nobody else,
# and the voided Admit left no binding (a voided admission never happened).
legs["undo_drops_stranger_and_friend"] = all(
    names_of(w[n]) == ["Ada", "Mira"]
    and sam_key not in w[n]["bindings"] and friend_key not in w[n]["bindings"]
    for n in holders)
# Leg 2 — the holders AGREE: one walk, byte-identical, from two arrival
# orders (mira held the stranger's act first; ada received it after the
# void was already signed).
legs["holders_agree_on_the_walk"] = \
    len({json.dumps(w[n], sort_keys=True) for n in holders}) == 1
# Leg 3 — the stranger's act is named, not silent. After a VOIDED Admit the
# key has no binding at all, so the gap is `unknown_signer` (`person_for`
# fails before the standing check); `not_a_member` is the Remove-cut's gap.
gaps = get("ada", "/v1/rail/log?namespace=house-ring")["gaps"]
legs["strangers_act_is_a_named_gap"] = any(
    g.get("gap") == "unknown_signer" and g.get("actor") == sam_key for g in gaps)
# Leg 4 — the finding, as a held fact: sync REFUSED the stranger's node by
# name, and that node carries none of the record — yet agrees on WHO IS IN
# from its seed (the floor every member accepted by joining).
refusal = "ring sync: refused — this ring's roster does not name the asker" in rails_log("ada")
sam_held = get("sam", "/v1/rail/log?namespace=house-ring")["held"]
legs["strangers_node_refused_and_agrees_on_names_only"] = (
    refusal and sam_held == 0 and names_of(w["sam"]) == ["Ada", "Mira"])
# Leg 5 — ra-roster-stays-independent at 0: same hashes, same files.
legs["roster_files_untouched"] = [roster_sha(n) for n in ports] == roster_before
# Leg 6 — the positive control: the members' own record survived the void.
ops = get("ada", "/v1/rail/log?namespace=house-ring")["ops"]
legs["members_record_intact"] = any(o.get("id") == admit_id and o.get("voided") for o in ops) \
    and any(o.get("person") == "Ada" for o in ops)

summary = {
    "demo": "ring-membership (ra-5 demo 5 — undo a leaked invite in one act)",
    "legs": legs,
    "held": {n: get(n, "/v1/rail/log?namespace=house-ring")["held"] for n in ports},
    "who_is_in": {n: names_of(w[n]) for n in ports},
    "converged_s": {"after_admit": t_admit, "after_void": t_void},
    "finding_ra6": "sync serves the roster FILE, not act-standing membership — the stranger's node was refused the ring it was a member of by act",
    "artifact": os.path.join(d, "summary.json"),
}
json.dump(summary, open(summary["artifact"], "w"), indent=1)
for k, v in legs.items():
    print(f"  {'PASS' if v else 'FAIL'}  {k}")
print(json.dumps(summary))
sys.exit(0 if all(legs.values()) else 1)
PY
}

case "${1:-}" in
  sitting) trap cmd_down EXIT; cmd_up && cmd_run ;;
  up)   cmd_up ;;
  down) cmd_down; echo "stopped" ;;
  run)  cmd_run ;;
  *) awk 'NR >= 2 && /^#/ { print; next } NR >= 2 { exit }' "$0"; exit 2 ;;
esac
