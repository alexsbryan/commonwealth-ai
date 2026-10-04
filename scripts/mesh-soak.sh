#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# mesh-soak.sh — Layer-2 multi-process mesh soak: the "real bytes" layer of the
# mesh QA stack (Layer 1 = in-process DST, `sovereign-mesh --features dst`;
# Layer 3 = the SLO gate, `sovereign mesh soak-gate`). It boots N real nodes,
# each a `cw-rails` (the node's mesh endpoint: membership, gossip, iroh) and a
# `sovereign daemon` that dials it through `[daemon] rails_base`, forms one mesh
# over iroh, then drives real faults (SIGKILL crash + churn/restart) in repeated
# cycles, asserting the HTTP-observable invariant pack via `sovereign mesh
# check-invariants` (the unit-tested assertion engine —
# cmnwlth/crates/sovereign-cli-mesh/src/mesh_soak.rs) against each node's
# cw-rails at every checkpoint. Findings stream to mesh-soak-findings.jsonl for
# the SLO gate.
#
# Since pb-mesh-exit-transport svrn serves no mesh membership: its /v1/mesh/*
# answer 410 naming cw-rails, and a daemon with no cw-rails beside it is in no
# mesh at all. So a node here is the pair, a crash kills both, and a restart
# brings cw-rails up before the daemon that dials it.
#
# ── What it exercises that the in-process DST suite cannot ────────────────────
#   Real process crashes (SIGKILL) + real wall-clock offline-decay + real churn
#   across actual OS-process and TCP boundaries — the fix-A decay path holding
#   under a genuine kill -9, not a simulated `down` flag.
#
# ── Isolation (this is load-bearing) ──────────────────────────────────────────
#   The `local` backend re-execs the whole soak inside a ROOTLESS NETWORK
#   NAMESPACE (`unshare -rn`, loopback-only). Why: every port here has a default
#   on the operator's live node — svrn :9741, cw-rails :9747, serve :9748 — and
#   any dial that fell back to one (a config key missed, a client that reads
#   the default) would reach the real house mesh. Every node is given its own
#   rails_base and serve port below, and the netns is the backstop for the one
#   nobody thought of: test nodes see only `lo`, self-advertise 127.0.0.1, and
#   form their mesh entirely on localhost.
#
# ── Models by workload ────────────────────────────────────────────────────────
#   crash lane: the mesh is cw-rails'; daemons only boot and dial it (no chat),
#   so primary == embed == a small embedding GGUF (~600MB/node) — N fit in RAM.
#   ingest lane: a REAL generative primary (so chat runs) + the 0.6B embed.
#
# Usage:
#   scripts/mesh-soak.sh [--nodes N] [--minutes M] [--seed S]
#                        [--workload crash|ingest|corrupt|offload] [--keep] [--gate]
#                        [--with-desktops] [--driver-minutes M] [--iroh]
#
#   --workload offload is the cross-node SERVICEABILITY lane: it proves a request
#     can actually be served by another node, which no other assertion here does.
#     Needs >=2 nodes and a generative primary (forced, like the ingest lane).
#     Fast — one warm-up per node, one settle, then N concurrent named turns.
#     Knobs: MESH_SOAK_OFFLOAD_CONCURRENCY (3), MESH_SOAK_OFFLOAD_SETTLE_SECS (14).
#     Also runs at the tail of --workload ingest, where the preconditions already hold.
#
#   --iroh (SOAK_IROH) is accepted and changes nothing: iroh is cw-rails' only
#     mesh transport, so every run joins over the founder's dial-by-key invite
#     and asserts each node carried gossip over iroh. cw-rails runs local-only
#     (`[relay] discovery = "none"`): the netns has no route to n0, and nodes
#     dial by key over gossiped direct addrs — the LAN-without-internet path.
#
#   --with-desktops (P2) hangs a headless desktop (attach-mode) + an app-user
#     persona driver on EACH node, in the netns, so user-visible TURN invariants
#     (stream integrity, intent, finish_reason, citation resolution, post-chaos
#     recovery) are asserted WHILE the soak kills/restarts the node underneath.
#     Forces a generative primary (set MESH_SOAK_MODEL). Pairs with --workload
#     crash (users on the app while the mesh is savaged). Needs a built desktop
#     binary (cargo build -p sovereign-desktop) at target/debug/sovereign-desktop.
#
#   --workload corrupt pre-writes garbage into a node's cw-rails mesh.json then
#     restarts it — cw-rails must refuse the store by name with its node_id
#     untouched (identity.rs: a corrupt store is never read as an empty roster),
#     and the node rejoins once the file is moved aside, under the same id.
#     A container-free OS-fault (the OS-fault tier — cgroup-OOM / disk-full /
#     partition — is rootless, no podman; see MESH_QA.md).
#   --workload ingest drives a daemon corpus ingest concurrently with chat and
#     asserts both progress (IngestProgress + ForegroundLiveness). Needs the chaos
#     corpus cached once (online) via scripts/setup-chaos-corpus.sh, a generative
#     MESH_SOAK_MODEL (default models/Qwen3.5-2B.Q6_K.gguf), and yield<30s.
#     ~3GB/node — stop the production 35B daemon first; fits a workstation at N=3.
#
# Prereq: a built sovereign-cli and cw-rails (cargo build --bins; debug is
# fine), the model(s) below, and `ip` + `unshare` for the netns.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# ── Args (parsed pre-reexec so they survive into the namespace) ────────────────
NODES="${NODES:-3}"; MINUTES="${MINUTES:-5}"; SEED="${SEED:-1}"; KEEP="${KEEP:-0}"; GATE="${GATE:-0}"
BACKEND="${MESH_SOAK_BACKEND:-local}"
# Workload: `crash` (default — kill-9/churn/decay) or `ingest` (ingest×inference
# contention lane: a real generative primary + concurrent corpus ingest + chat).
WORKLOAD="${WORKLOAD:-crash}"
# --with-desktops (P2): hang a headless desktop (attach-mode) + an app-user
# persona driver on EACH node, in the shared netns, so user-visible TURN
# invariants are asserted WHILE the node is killed/restarted underneath. Forces a
# generative primary (chat must work). DRIVER_MINUTES defaults to MINUTES.
DESKTOPS="${DESKTOPS:-0}"; DRIVER_MINUTES="${DRIVER_MINUTES:-}"
# --iroh / SOAK_IROH used to pick the transport. cw-rails has one, so the flag
# is parsed for old invocations and read by nothing.
# --reachability-chaos (SOAK_REACH_CHAOS): the founder-reachability self-heal
# axis (Track W). Each node's cw-rails boots with a fast watchdog + the periodic
# chaos hook (SOVEREIGN_MESH_WATCHDOG_CHAOS_DROP_SECS, read by
# commonwealth-rails iroh_watchdog.rs), injecting reachability wedges; the
# invariant checker records each node's `self_reachability.degraded` and the
# `founder_degraded_rate` SLI (baseline-gated) asserts self-heal keeps
# recovering them.
SOAK_REACH_CHAOS="${SOAK_REACH_CHAOS:-0}"
while [ $# -gt 0 ]; do
  case "$1" in
    --nodes)    NODES="$2"; shift 2;;
    --minutes)  MINUTES="$2"; shift 2;;
    --seed)     SEED="$2"; shift 2;;
    --workload) WORKLOAD="$2"; shift 2;;
    --with-desktops) DESKTOPS=1; shift;;
    --iroh)     shift;;   # iroh is the only transport; see SOAK_IROH above
    --reachability-chaos) SOAK_REACH_CHAOS=1; shift;;
    --driver-minutes) DRIVER_MINUTES="$2"; shift 2;;
    --keep)     KEEP=1; shift;;
    --gate)     GATE=1; shift;;
    -h|--help)  awk 'NR >= 3 && /^#/ { print; next } NR >= 3 { exit }' "$0"; exit 0;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done
case "$WORKLOAD" in crash|ingest|corrupt|offload) ;; *) echo "bad --workload: $WORKLOAD (crash|ingest|corrupt|offload)" >&2; exit 2;; esac
case "$SOAK_REACH_CHAOS" in 1|true|yes|on) REACH_CHAOS=1;; *) REACH_CHAOS=0;; esac

# ── Re-exec into a fresh rootless netns (loopback up) for the local backend ────
if [ "$BACKEND" = "local" ] && [ -z "${MESH_SOAK_IN_NETNS:-}" ]; then
  exec unshare -rn env MESH_SOAK_IN_NETNS=1 \
    NODES="$NODES" MINUTES="$MINUTES" SEED="$SEED" KEEP="$KEEP" GATE="$GATE" \
    MESH_SOAK_BACKEND="$BACKEND" WORKLOAD="$WORKLOAD" \
    DESKTOPS="$DESKTOPS" DRIVER_MINUTES="$DRIVER_MINUTES" \
    SOAK_REACH_CHAOS="$SOAK_REACH_CHAOS" bash "$0"
fi
[ "$BACKEND" = "local" ] && ip link set lo up

CLI="${SOVEREIGN_CLI:-$ROOT/target/debug/sovereign-cli}"
[ -x "$CLI" ] || CLI="$ROOT/target/release/sovereign-cli"
[ -x "$CLI" ] || { echo "sovereign-cli not built (cargo build --bins)"; exit 1; }
# The node's mesh endpoint. CW_RAILS_BIN is the name `svrn mesh up` reads too
# (rails_up.rs locate_rails).
RAILS="${CW_RAILS_BIN:-$ROOT/target/debug/cw-rails}"
[ -x "$RAILS" ] || { echo "cw-rails not built at $RAILS (cargo build --bins, or set CW_RAILS_BIN)"; exit 1; }
# Model profile by workload. The crash lane only needs daemons that boot + gossip,
# so primary == embed == a tiny embedding GGUF (N fit in RAM, no chat is made).
# The ingest lane needs a REAL generative primary (so chat actually runs) plus the
# small embed model (so corpus ingest's embed pipeline is cheap), and a yield
# window < 30s (mandatory — else the 30s health-ping starves ingest; see
# setup-chaos-corpus.sh / chaos_monkey README).
EMBED_DEFAULT="$ROOT/models/qwen-embedding-0.6b.gguf/Qwen3-Embedding-0.6B-Q8_0.gguf"
if [ "$WORKLOAD" = "ingest" ] || [ "$WORKLOAD" = "offload" ] || [ "$DESKTOPS" = 1 ]; then
  # A generative primary is required: the ingest lane chats, and --with-desktops
  # runs real app users that chat. The embed model stays the small 0.6B (for the
  # embeddings the knowledge path needs).
  PRIMARY_MODEL="${MESH_SOAK_MODEL:-$ROOT/models/Qwen3.5-2B.Q6_K.gguf}"
  EMBED_MODEL="${MESH_SOAK_EMBED_MODEL:-$EMBED_DEFAULT}"
  if [ "$WORKLOAD" = "ingest" ]; then
    YIELD_SECS="${MESH_SOAK_YIELD_SECS:-5}"
    case "$YIELD_SECS" in ''|*[!0-9]*) echo "MESH_SOAK_YIELD_SECS must be an integer"; exit 2;; esac
    [ "$YIELD_SECS" -lt 30 ] || { echo "yield_to_foreground_secs=$YIELD_SECS must be < 30 (else ingest starves)"; exit 2; }
  else
    YIELD_SECS=""   # crash + desktops: no ingest contention, default yield is fine
  fi
else
  PRIMARY_MODEL="${MESH_SOAK_MODEL:-$EMBED_DEFAULT}"
  EMBED_MODEL="$PRIMARY_MODEL"
  YIELD_SECS=""
fi
for _m in "$PRIMARY_MODEL" "$EMBED_MODEL"; do
  [ -f "$_m" ] || { echo "model not found: $_m (set MESH_SOAK_MODEL / MESH_SOAK_EMBED_MODEL)"; exit 1; }
done
# TOML line spliced into [daemon]; empty for the crash lane (default applies).
YIELD_TOML=""; [ -n "$YIELD_SECS" ] && YIELD_TOML="yield_to_foreground_secs = $YIELD_SECS"
# Ingest lane: point the daemon's recipe resolver at the override dir so it can
# fetch chaos-secret-agent. The soak daemon's engine has no local overrides_dir
# and the recipe isn't in the bundled catalog, so without this the install 200s
# with spawned:false ("No registry entry"). Step 1b of registry resolution reads
# $SOVEREIGN_RECIPES_DIR/<id>/recipe.toml — exactly the override setup_ingest_recipe
# writes (with the $HOME-correct cached-source path).
[ "$WORKLOAD" = "ingest" ] && export SOVEREIGN_RECIPES_DIR="$HOME/.svrnmesh/recipes"

WORK="$(mktemp -d -t mesh-soak.XXXXXX)"
FINDINGS="$ROOT/mesh-soak-findings.jsonl"; : > "$FINDINGS"
# cw-rails' offline_threshold_secs defaults to 60 (commonwealth-rails
# config.rs); the soak writes no override, so wait past it.
DECAY_WAIT="${DECAY_WAIT:-72}"
RANDOM=$SEED                        # seed bash PRNG → reproducible victim picks
declare -a PIDS RPIDS NODE_IDS      # PIDS: the svrn daemons; RPIDS: their cw-rails
FAILS=0; CYCLE=0

# Per node: svrn client · svrn internal · (svrn's ring rail, which the daemon
# binds at client + 2 itself — guest_pages.rs rail_port — so svrn's ports step
# by 4: at a step of 2, node N's rail took node N+1's client port) · cw-rails
# API · serve (hosted in the daemon process, which would otherwise take 9748 on
# every node).
cport() { echo $((19741 + 4 * $1)); }
iport() { echo $((19742 + 4 * $1)); }
rport() { echo $((20741 + 2 * $1)); }
sport() { echo $((20742 + 2 * $1)); }
# svrn's data dir and cw-rails' root, side by side: a cw-rails root nested in
# svrn's [data] dir would be walked by anything that scans it.
rdir()  { echo "$WORK/rails$1"; }
log()   { printf '\n\033[1m# [soak] %s\033[0m\n' "$*"; }
finding() { printf '%s\n' "$1" >> "$FINDINGS"; }
jget() { curl -s -m 4 "$1" 2>/dev/null | python3 -c "import sys,json
try:
    d=json.load(sys.stdin); print(eval(sys.argv[1]))
except Exception: pass" "$2"; }
# A node's cw-rails logs, ANSI stripped: tracing colours the key=value pairs,
# so `via=iroh` is not a substring of the raw line.
rails_log() { cat "$(rdir "$1")"/rails.*.log 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g'; }
# Does node <i>'s cw-rails log hold a line matching <ERE>? Never `rails_log |
# grep -q`: under pipefail, grep -q exits at the first match, sed dies of
# SIGPIPE, and the pipeline reports a miss for a line that is there (it read 0
# on every node of the first ported run, 2026-10-03).
rails_logged() { rails_log "$1" | grep -E "$2" > /dev/null; }

# The mesh view is cw-rails'. svrn's /v1/mesh/status answers 410.
status_url() { echo "http://127.0.0.1:$(rport $1)/v1/mesh/status"; }
rails_url()  { echo "http://127.0.0.1:$(rport $1)"; }

# One node identity, as `svrn mesh up`'s handover leaves it
# (identity_handover.rs): the same node_id in svrn's data dir and cw-rails'
# root. Seeded once, before the first boot of either; a restart finds it.
seed_identity() {  # seed_identity <i>
  local i="$1"; local d="$WORK/node$i" r; r=$(rdir "$i")
  mkdir -p "$d" "$r"
  [ -f "$r/node_id" ] && return 0
  head -c 16 /dev/urandom > "$r/node_id"
  cp "$r/node_id" "$d/node_id"
}

boot_rails() {  # boot_rails <i>
  local i="$1"; local r; r=$(rdir "$i")
  seed_identity "$i"
  # name: the member name peers see, and what /v1/mesh/join's node_name must
  # equal (membership.rs check_node_name). discovery "none" severs n0 — the
  # netns has no route to it, so asking would only add timeouts.
  [ -f "$r/rails.toml" ] || cat > "$r/rails.toml" <<EOF
name = "node$i"
listen = $(rport "$i")

[relay]
discovery = "none"
EOF
  # The reachability axis's watchdog is cw-rails' now (iroh_watchdog.rs reads
  # these names): a fast poll + the periodic chaos wedge, so the
  # founder_degraded_rate SLI observes real detect→escalate→rebuild→recover.
  local reach_env=""
  [ "$REACH_CHAOS" = 1 ] && reach_env="SOVEREIGN_MESH_WATCHDOG_POLL_SECS=5 SOVEREIGN_MESH_WATCHDOG_GRACE_SECS=8 SOVEREIGN_MESH_WATCHDOG_COOLDOWN_SECS=20 SOVEREIGN_MESH_WATCHDOG_CHAOS_DROP_SECS=45"
  env $reach_env "$RAILS" run --data-dir "$r" > "$r/rails.$RANDOM.log" 2>&1 &
  RPIDS[$i]=$!
}

boot_daemon() {  # boot_daemon <i>
  # NB: assign `i` on its own line first. A same-line `local i="$1" d="$WORK/node$i"`
  # expands $i in d= BEFORE `local i` is bound, so it captures a LEAKED outer loop
  # var (the survivor loop leaves `i`=NODES-1) — which cross-wired a restarted
  # node's data dir to a peer's on restart and looked like an id collision.
  local i="$1"; local d="$WORK/node$i"; mkdir -p "$d"
  cat > "$d/config.toml" <<EOF
[models]
primary = "$PRIMARY_MODEL"
embed = "$EMBED_MODEL"
context_size = 4096
[daemon]
client_port = $(cport $i)
internal_port = $(iport $i)
autostart = false
primary_idle_secs = 1800
extras_idle_secs = 0
freshness_watchers_enabled = false
client_bind = "127.0.0.1"
rails_base = "$(rails_url "$i")"
$YIELD_TOML
[data]
dir = "$d"
EOF
  # Every node gets its own `[data] dir = $d` above, and the run lock is keyed
  # on the DATA ROOT, so all three nodes claim independently. It was keyed on
  # $HOME until 2026-08-24, which meant node0 took the lock and EVERY other
  # node exited with "another daemon already holds the run lock" — and the
  # failure was near-invisible, because the surviving one-node mesh still
  # passed the invariant pack (convergence and pairwise liveness are trivially
  # true over a single reachable node). A 3-node soak was really a 1-node soak
  # reporting green. The SOVEREIGN_ALLOW_MULTIPLE_DAEMONS escape hatch that
  # papered over it is deleted with the re-key. See the bind assertion at the
  # bring-up loop.
  #
  # The daemon process hosts serve, which binds SOVEREIGN_SERVE_PORT (venue.rs)
  # and would otherwise race every node for 9748. CW_RAILS_DIR points svrn-side
  # readers of cw-rails' root (commonwealth_media::rails_data_dir) at this
  # node's, as collaborate_e2e.rs does.
  SOVEREIGN_SERVE_PORT="$(sport "$i")" CW_RAILS_DIR="$(rdir "$i")" \
    "$CLI" daemon run --config "$d/config.toml" > "$d/daemon.$RANDOM.log" 2>&1 &
  PIDS[$i]=$!
}

# A node is the pair; cw-rails first, so the daemon's first dial finds it.
boot_node() {  # boot_node <i>
  local i="$1"
  boot_rails "$i"
  boot_daemon "$i"
}
kill_node() {  # kill_node <i> — SIGKILL both halves, as a power cut would
  local i="$1"
  kill -9 "${PIDS[$i]:-0}" "${RPIDS[$i]:-0}" 2>/dev/null
}

# Up = both halves answer 200: cw-rails' status and svrn's /status. `-f`, so a
# 410 or a 5xx is not "up". Without it a 410 from svrn's retired
# /v1/mesh/status passed this wait, and the run failed later as empty node ids.
wait_port() { local i="$1" _; for _ in $(seq 1 60); do
  curl -sf -m 2 -o /dev/null "$(status_url $i)" 2>/dev/null \
    && curl -sf -m 2 -o /dev/null "http://127.0.0.1:$(cport $i)/status" 2>/dev/null && return 0
  kill -0 "${RPIDS[$i]}" 2>/dev/null || return 1
  kill -0 "${PIDS[$i]}" 2>/dev/null || return 1; sleep 0.5; done; return 1; }

# kill-9-startup-window torture: boot the node, then kill -9 it again WHILE it is
# still inside its startup window (before wait_port would succeed), then boot it
# clean. The clean restart must resume the SAME identity and take back cw-rails'
# root lock from the dead holder — a regression net for startup-window
# durability. Stability is asserted by the following healed checkpoint
# (UniqueIds + the unchanged self_id).
torture_restart() {  # torture_restart <i>
  local v="$1"
  boot_node "$v"                     # first boot
  sleep "0.$(( (RANDOM % 8) + 1 ))"  # 0.1–0.8s — land inside the startup window
  kill_node "$v"                     # kill mid-startup
  finding "{\"kind\":\"fault\",\"action\":\"kill-9-startup-window\",\"node\":$v,\"cycle\":${CYCLE:-0}}"
  boot_node "$v"                     # clean restart — must resume the same id
}

# Found the mesh on node0's cw-rails (`POST /v1/mesh/create`, what `svrn mesh
# create` sends after its bring-up — which this script must not run: it would
# install a cw-rails user unit). Sets FKEY and FLINK, the invite with node0's
# iroh dial. cw-rails' invite carries it as `iroh=`, and a join with no iroh
# dial is refused by name (join.rs NoIrohDial).
found_mesh() {
  local out code body
  out=$(curl -s -m 20 -w $'\n%{http_code}' -X POST "$(rails_url 0)/v1/mesh/create" \
    -H 'content-type: application/json' -d '{"name":"mesh-soak","node_name":"node0"}' 2>&1)
  code="${out##*$'\n'}"; body="${out%$'\n'*}"
  FKEY=$(printf '%s' "$body" | python3 -c 'import sys,json; print(json.load(sys.stdin).get("join_key") or "")' 2>/dev/null)
  FLINK=$(printf '%s' "$body" | python3 -c 'import sys,json; print(json.load(sys.stdin).get("join_link") or "")' 2>/dev/null)
  case "$code:$FLINK" in
    200:*iroh=*) return 0;;
  esac
  echo "  founding on node0 failed: HTTP ${code:-000} ${body:0:300}"
  finding "{\"phase\":\"found\",\"ok\":false,\"violations\":[{\"invariant\":\"mesh_founded\",\"detail\":\"node0 /v1/mesh/create answered ${code:-000} with no iroh= invite\"}]}"
  FAILS=$((FAILS+1)); return 1
}

# Join node <i> to the founder by its invite, through <i>'s own cw-rails
# (`POST /v1/mesh/join`, the door `svrn mesh join` dials). A refusal is printed
# and recorded where it happens, rather than surfacing later as a node missing
# from convergence.
join_to_founder() {  # join_to_founder <i> <founder_link>
  local i="$1" link="$2" body out code
  body=$(python3 -c 'import json,sys; print(json.dumps({"key_or_url": sys.argv[1], "node_name": "node"+sys.argv[2]}))' "$link" "$i")
  out=$(curl -s -m 60 -w $'\n%{http_code}' -X POST "$(rails_url "$i")/v1/mesh/join" \
    -H 'content-type: application/json' -d "$body" 2>&1)
  code="${out##*$'\n'}"
  [ "$code" = 200 ] && return 0
  out="${out%$'\n'*}"
  echo "  node$i join refused: HTTP ${code:-000} ${out:0:300}"
  local detail; detail=$(printf '%s' "${out:0:300}" | python3 -c 'import sys,json;print(json.dumps(sys.stdin.read()))')
  finding "{\"kind\":\"join\",\"node\":$i,\"ok\":false,\"http\":\"${code:-000}\",\"detail\":$detail}"
  return 1
}

self_id() { jget "$(status_url $1)" '[m["node_id"] for m in d["members"] if m["is_self"]][0]'; }
# Robust id capture: retry until non-empty. A transient status hiccup must NEVER
# leave an empty entry in NODE_IDS — that drops the node from --expect-live and
# flags a perfectly healthy peer as a "ghost" on every subsequent check.
robust_self_id() { local i="$1" id _; for _ in $(seq 1 15); do
  id=$(self_id "$i"); [ -n "$id" ] && { printf '%s' "$id"; return 0; }; sleep 0.5; done; printf ''; }
online_count() { jget "$(status_url $1)" 'd["members_online"]'; }
sees_status() { jget "$(status_url $1)" "[m['status'] for m in d['members'] if m['node_id']=='$2'][0]"; }
# Quiesce-then-assert: wait until EVERY node (except an optional excluded index)
# reports <target> members online, so a check runs against a converged mesh and
# not mid-gossip-propagation. Without this, a transient liveness/no-ghost lag on
# a slow peer reads as a violation even though the mesh converges a beat later.
# Bounded — if it never converges, the check still runs and flags a REAL failure.
wait_online_eq() { local target="$1" excl="${2:-x}" i; for _ in $(seq 1 90); do local ok=1
  for i in $(seq 0 $((NODES-1))); do [ "$i" = "$excl" ] && continue
    [ "$(online_count $i)" = "$target" ] || ok=0; done
  [ "$ok" = 1 ] && return 0; sleep 1; done; return 1; }

# Iroh: prove each node's cw-rails actually carried gossip over iroh — not
# merely that the endpoint bound. Two signals per node's cw-rails log (both at
# info, cw-rails' default level):
#   1. install: "rails: endpoint bound … dial=Some(" — the endpoint is up with
#      a dial string to hand peers (lib.rs).
#   2. carried: "gossip: round complete … via=iroh:… outcome=\"reached\"" — a
#      gossip round reached a peer over an iroh connection (gossip.rs).
# These replace the daemon's "routing classes over iroh" / "transport:
# resolved" lines, whose emitters left with the transport.
# A node missing either signal fails the run. Findings stream to the verdict.
# Called once after initial convergence. `install` is immediate; `carried`
# needs a round in each direction (the founder dials a joiner only once it has
# merged that joiner's self-stamped dial info), so the caller POLLS for it.
iroh_installed() { rails_logged "$1" 'rails: endpoint bound.*dial=Some\('; }
iroh_carried()   { rails_logged "$1" 'gossip: round complete.*via=iroh:.*outcome="reached"'; }

assert_iroh_carried_traffic() {
  log "iroh — asserting each node's cw-rails carried gossip over iroh"
  # Poll up to ~40s: install is immediate, but carried-over-iroh needs the
  # founder↔joiner gossip round that merges dial info (10s cadence). Bounded —
  # if a node never routes over iroh, the check below still runs and FAILS.
  local i deadline=$(( $(date +%s) + 40 ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    local all=1
    for i in $(seq 0 $((NODES-1))); do
      { iroh_installed "$i" && iroh_carried "$i"; } || all=0
    done
    [ "$all" = 1 ] && break
    sleep 2
  done
  local installed carried
  for i in $(seq 0 $((NODES-1))); do
    iroh_installed "$i" && installed=1 || installed=0
    iroh_carried "$i" && carried=1 || carried=0
    finding "{\"kind\":\"iroh\",\"check\":\"install\",\"node\":$i,\"ok\":$([ $installed = 1 ] && echo true || echo false)}"
    finding "{\"kind\":\"iroh\",\"check\":\"carried_over_iroh\",\"node\":$i,\"ok\":$([ $carried = 1 ] && echo true || echo false)}"
    if [ "$installed" = 1 ] && [ "$carried" = 1 ]; then
      echo "  node$i: iroh install ✓  carried-over-iroh ✓"
    else
      echo "  node$i: iroh install=$installed carried-over-iroh=$carried  ✗"
      FAILS=$((FAILS+1))
    fi
  done
  # The operator surface: node0's cw-rails status must name an iroh dial (a
  # relay or direct addresses) for every peer, or a member it lists is one it
  # cannot reach. The daemon's `iroh_transport` path view has no cw-rails
  # counterpart; `members[].dial` is what cw-rails reports (api.rs).
  local undialable
  undialable=$(jget "$(status_url 0)" '",".join(m["name"] for m in d["members"] if not m.get("is_self") and not ((m.get("dial") or {}).get("relay_url") or (m.get("dial") or {}).get("iroh_direct_addrs")))')
  local peers; peers=$(jget "$(status_url 0)" 'sum(1 for m in d["members"] if not m.get("is_self"))')
  if [ -n "$peers" ] && [ "$peers" -gt 0 ] && [ -z "$undialable" ]; then
    echo "  node0 status: an iroh dial for each of $peers peer(s)"
    finding '{"kind":"iroh","check":"status_surface","node":0,"ok":true}'
  else
    echo "  node0 status: peers=${peers:-unread} without an iroh dial: ${undialable:-none} ✗"
    FAILS=$((FAILS+1)); finding '{"kind":"iroh","check":"status_surface","node":0,"ok":false}'
  fi
}

# Forensic capture — dump the DURABLE identity state + daemon identity events at
# the moment of a violation, and copy node_id/mesh.json/logs into a stable bundle
# so the issue can be re-inspected (and replayed) offline without re-running the
# whole soak. This is what makes an intermittent failure efficient to root-cause:
# a UniqueIds/no_ghost hit tells you WHICH id collided; the bundle tells you which
# durable file carries the wrong id and what cw-rails logged when it took it.
# cw-rails' mesh.json names no self id (identity.rs reads node_id alone), so the
# second column is svrn's copy: the two must agree, as the handover leaves them.
REPRO_DIR="$ROOT/mesh-soak-repro"
fhex() { python3 -c "
try: print(open('$(rdir "$1")/node_id','rb').read().hex())
except Exception: print('NO-FILE')" 2>/dev/null; }
mhex() { python3 -c "
try: print(open('$WORK/node$1/node_id','rb').read().hex())
except Exception: print('NO-FILE')" 2>/dev/null; }
capture_forensics() {  # capture_forensics <label>
  local label="$1" i
  local cyc="${CYCLE:-0}"
  local bundle="$REPRO_DIR/seed${SEED}-cycle${cyc}-${label}"
  mkdir -p "$bundle"
  {
    echo "# mesh-soak forensics — seed=$SEED cycle=$cyc phase=$label nodes=$NODES backend=$BACKEND"
    echo "# durable identity state at the violation (live id vs cw-rails node_id vs svrn's copy):"
    for i in $(seq 0 $((NODES-1))); do
      printf '  node%s  live=%-32s  rails_node_id=%-32s  svrn_node_id=%s\n' \
        "$i" "$(self_id $i 2>/dev/null || echo DEAD)" "$(fhex $i)" "$(mhex $i)"
    done
    echo "# harness expect-live tracking (a healthy node missing here = a FALSE ghost):"
    echo "  NODE_IDS[]=${NODE_IDS[*]:-<unset>}"
    echo "  ALL_IDS=${ALL_IDS:-<unset>}"
    echo "# cw-rails identity events (per node):"
    for i in $(seq 0 $((NODES-1))); do echo "  node$i:"
      rails_log "$i" | grep -E 'identity: (minted|mesh loaded)|join: admitted|endpoint bound|is not a mesh' \
        | tail -6 | sed 's/^/    /'; done
  } | tee "$bundle/forensics.txt"
  for i in $(seq 0 $((NODES-1))); do local nd="$bundle/node$i" r; r=$(rdir "$i"); mkdir -p "$nd"
    cp "$r/node_id" "$r/mesh.json" "$r/rails.toml" "$nd/" 2>/dev/null
    cp "$r"/rails.*.log "$WORK/node$i"/daemon.*.log "$nd/" 2>/dev/null; done
  echo "  ↳ forensic bundle: $bundle"
  finding "{\"kind\":\"forensics\",\"phase\":\"$label\",\"cycle\":$cyc,\"bundle\":\"$bundle\"}"
}

check() {  # check <label> <nodes-csv> <expect-live-csv> ; appends a finding, bumps FAILS
  local label="$1" nodes="$2" live="$3" out rc rec
  # `--json` is load-bearing, not cosmetic. The human branch prints prose, so the
  # FAILING finding used to carry `detail` and no `violations` key at all — and
  # `soak_slis` counts a checkpoint only when `violations` is present
  # (mesh_soak.rs:307). Every failing checkpoint was therefore dropped on the
  # floor, and `invariant_violation_rate` could never rise above 0.0: the gate
  # reported perfect invariant health on a run that had just failed. The same
  # omission hid `founder_degraded`, pinning `founder_degraded_rate` at 0.0 too —
  # and that SLI is the ONLY assertion --reachability-chaos makes. One flag
  # revives both.
  # stderr is kept OUT of the parsed capture. The CLI writes advisories there
  # (e.g. the SOVEREIGN_*→SVRNMESH_* deprecation bridge), and folding them into
  # the JSON with 2>&1 makes every parse fail — which turned PASSING checkpoints
  # into failures on the first run of this rewiring.
  local errf="$WORK/check-${label//\//_}.err"
  out=$("$CLI" mesh check-invariants --nodes "$nodes" --expect-live "$live" --json 2>"$errf"); rc=$?
  # Stamp the checkpoint label into the CLI's own JSON and use it verbatim as the
  # finding: it already carries violations / ok / founder_degraded / unreachable
  # in exactly the shape the extractor wants, so there is no second ad-hoc shape
  # to keep in sync. `phase` is what the coverage accounting counts.
  # Scan stdout backwards for the last parseable JSON object rather than
  # assuming the whole stream is JSON — robust to any future preamble.
  rec=$(printf '%s' "$out" | python3 -c 'import sys,json
raw=sys.stdin.read()
d=None
for line in reversed(raw.splitlines()):
    line=line.strip()
    if not line.startswith("{"): continue
    try:
        d=json.loads(line); break
    except Exception: continue
if d is None:
    err=open(sys.argv[2]).read() if len(sys.argv)>2 else ""
    d={"ok":False,"violations":[{"invariant":"check_invariants",
       "detail":("unparseable check-invariants output: "+raw[:1000]+" | stderr: "+err[:1000])}]}
d["phase"]=sys.argv[1]
print(json.dumps(d))' "$label" "$errf")
  finding "$rec"
  printf '%s' "$rec" | python3 -c 'import sys,json
d=json.load(sys.stdin)
un=d.get("unreachable") or []
print("  [%s] ok=%s nodes=%s unreachable=%d" % (d.get("phase"), d.get("ok"), d.get("nodes","?"), len(un)))
for v in (d.get("violations") or []): print("    x %s: %s" % (v.get("invariant"), v.get("detail")))
for a in (d.get("founder_degraded") or []): print("    ~ founder self-heal degraded: %s" % a)'
  if [ "$rc" != 0 ]; then
    FAILS=$((FAILS+1))
    capture_forensics "$label"
  fi
}

# ── ingest × inference contention lane (--workload ingest) ────────────────────
# Drive a real daemon-side corpus ingest CONCURRENTLY with interactive chat on the
# same node, then assert both make progress. The ingest is POSTed to the node's
# INTERNAL port — the daemon owns the ingest task, so it genuinely competes for
# the engine (the contention the cheap embed-only crash lane structurally cannot
# reach). Two contention verdicts, plus the base invariant pack each cycle:
#   IngestProgress     — the per-corpus progress phase advances across polls
#                        (forward progress / non-stalling) while chat runs; a
#                        frozen progress phase under load is the failure (not
#                        non-completion — heavy chat correctly throttles ingest).
#   ForegroundLiveness — interactive chat keeps returning within an SLO while
#                        ingest runs (the advisory foreground-yield lets chat win
#                        the slot). Asserted on outcome CLASS, not absolute ms.
setup_ingest_recipe() {  # mirror the committed recipe to the live override dir
  local canonical="$ROOT/ingest/crates/sovereign-recipes/chaos-secret-agent/recipe.toml"
  local override="$HOME/.svrnmesh/recipes/chaos-secret-agent/recipe.toml"
  local src="$HOME/.svrnmesh/bench-corpora/chaos-secret-agent/secret-agent.txt"
  [ -f "$canonical" ] || { echo "  canonical recipe missing: $canonical"; return 1; }
  [ -f "$src" ] || { echo "  chaos source not cached: $src — run scripts/setup-chaos-corpus.sh once (online) first"; return 1; }
  mkdir -p "$(dirname "$override")"
  sed "s#^path = .*#path = \"$src\"#" "$canonical" > "$override"
  echo "  recipe override: $override (→ $(basename "$src"))"
}
chat_once() {  # chat_once <node> <slo_ms> → echoes "<http_code> <elapsed_ms>"
  local i="$1" slo="$2" t0 t1 code
  t0=$(date +%s%3N)
  code=$(curl -s -m $(( slo/1000 + 10 )) -o /dev/null -w '%{http_code}' \
    -X POST "http://127.0.0.1:$(cport "$i")/v1/chat/completions" \
    -H 'content-type: application/json' \
    -d '{"model":"primary","stream":false,"max_tokens":32,"messages":[{"role":"user","content":"Reply in one short sentence: who is Mr Verloc?"}]}' 2>/dev/null)
  t1=$(date +%s%3N); echo "${code:-000} $(( t1 - t0 ))"
}

# chat_capture <node> <slo_ms> <outfile> <max_tokens> → echoes "<http_code> <elapsed_ms>"
#
# Same call as chat_once but KEEPS the response body. The offload probe needs
# it: the only client-visible attribution of who actually served a turn is the
# response's `model` field, which `MeshInferenceProvider::annotate`
# (peer_inference.rs:1386-1389) rewrites to "<model> @ peer <name>" on a peer
# serve. chat_once sends the body to /dev/null, which is why this harness could
# never tell a locally-served turn from an offloaded one.
chat_capture() {
  local i="$1" slo="$2" out="$3" mt="${4:-32}" t0 t1 code
  t0=$(date +%s%3N)
  code=$(curl -s -m $(( slo/1000 + 10 )) -o "$out" -w '%{http_code}' \
    -X POST "http://127.0.0.1:$(cport "$i")/v1/chat/completions" \
    -H 'content-type: application/json' \
    -d "{\"model\":\"primary\",\"stream\":false,\"max_tokens\":$mt,\"messages\":[{\"role\":\"user\",\"content\":\"Explain in a few sentences why the sky appears blue.\"}]}" 2>/dev/null)
  t1=$(date +%s%3N); echo "${code:-000} $(( t1 - t0 ))"
}

# served_model <bodyfile> → echoes the response's `model` field ('' on any error)
served_model() {
  python3 -c "import sys,json
try:
    print(json.load(open(sys.argv[1])).get('model',''))
except Exception: print('')" "$1"
}

# ── grounding oracle (ingest lane) ────────────────────────────────────────────
# The contention lane proves chat STAYS LIVE under ingest; this proves the
# knowledge path stays CORRECT under it. There is no one-shot RAG route on the
# daemon, so a grounded turn is a 3-call path on the client port — embed →
# /v1/knowledge/search (returns chunk TEXT) → /v1/chat/completions (synthesize
# from ONLY those chunks). scripts/grounded-turn.py does that and writes the
# {question,answer,chunks} triple; `bench chaos-monkey score-answer` then judges
# it with the SAME gold-free primitive the live grounding gate uses. Two signals:
#   GroundingIntegrity — the deterministic backbone: after a completed ingest the
#                        corpus is actually queryable (chunks come back, corpus is
#                        in corpora_searched, not corpora_unavailable). A node that
#                        ingested under load but can't serve its own corpus is a
#                        real failure the progress-only check cannot see.
#   GroundingVerdict   — a conservative confabulation red-line: verdict ==
#                        "hallucination" (the answer asserts a value ABSENT from
#                        the retrieved evidence). The judge runs on the node's own
#                        primary — weak on a 2B, so Integrity is the backbone and
#                        the verdict the spice. "grounded" is NOT required: hedged/
#                        discursive answers score honest_abstention, which is fine.
GQUESTION="${MESH_SOAK_GQUESTION:-who is Mr Verloc?}"
GCORPUS="${MESH_SOAK_GCORPUS:-chaos-secret-agent}"
GTURN="$ROOT/scripts/grounded-turn.py"

# grounded_retrieval <node> — A→B only (embed + knowledge/search, NO generation,
# so it never competes with foreground chat for the primary slot), echoes n_chunks.
grounded_retrieval() {
  python3 "$GTURN" --base-url "http://127.0.0.1:$(cport "$1")" --corpus "$GCORPUS" \
    --question "$GQUESTION" --limit 6 2>/dev/null \
    | python3 -c 'import sys,json;print(json.load(sys.stdin).get("n_chunks",0))' 2>/dev/null
}

# grounding_verdict <node> <hard|soft> — hard mode (post-completed-ingest) emits
# counted phase checkpoints that can FAIL; soft mode (ingest still in flight, so a
# partial index is legitimate) records only observational findings, never fails.
grounding_verdict() {
  local i="$1" mode="$2" base si js n searched unavail err verdict t
  base="http://127.0.0.1:$(cport "$i")"; si="$WORK/score-input-node$i.json"
  # Retrieval can lag a beat behind ingest-complete (index open) — retry briefly.
  for t in 1 2 3 4 5; do
    js=$(python3 "$GTURN" --base-url "$base" --corpus "$GCORPUS" --question "$GQUESTION" \
           --synthesize --score-input "$si" --limit 6 2>/dev/null)
    n=$(printf '%s' "$js" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("n_chunks",0))' 2>/dev/null)
    [ "${n:-0}" -gt 0 ] && break; sleep 2
  done
  searched=$(printf '%s' "$js" | python3 -c "import sys,json;d=json.load(sys.stdin);print('yes' if '$GCORPUS' in (d.get('corpora_searched') or []) else 'no')" 2>/dev/null)
  unavail=$(printf '%s' "$js" | python3 -c "import sys,json;d=json.load(sys.stdin);print('yes' if '$GCORPUS' in (d.get('corpora_unavailable') or []) else 'no')" 2>/dev/null)
  err=$(printf '%s' "$js" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("error") or "")' 2>/dev/null)

  # ── retrieval integrity (deterministic; no judge) ──
  if [ "${n:-0}" -le 0 ] || [ "$searched" != "yes" ] || [ "$unavail" = "yes" ]; then
    if [ "$mode" = hard ]; then
      FAILS=$((FAILS+1))
      finding "{\"phase\":\"grounding-integrity\",\"ok\":false,\"detail\":\"corpus '$GCORPUS' not queryable after ingest (n_chunks=${n:-0} searched=$searched unavailable=$unavail err=${err:-none})\"}"
      echo "  ✗ GroundingIntegrity: $GCORPUS not queryable post-ingest (n_chunks=${n:-0} err=${err:-none})"
    else
      finding "{\"kind\":\"grounding-probe\",\"node\":$i,\"soft\":true,\"n_chunks\":${n:-0},\"detail\":\"ingest incomplete — retrieval not asserted\"}"
      echo "  ~ GroundingIntegrity (soft): n_chunks=${n:-0} (ingest incomplete — not asserted)"
    fi
    return
  fi
  finding "{\"phase\":\"grounding-integrity\",\"ok\":true,\"detail\":\"corpus queryable (n_chunks=$n)\"}"
  echo "  ✓ GroundingIntegrity: $GCORPUS queryable (n_chunks=$n)"

  # ── confabulation red-line (bench gold-free primitive; judge = node primary) ──
  verdict=$(SOVEREIGN_NO_STALE_WARN=1 "$CLI" bench chaos-monkey score-answer --input "$si" \
              --base-url "$base" --judge-model primary --critic-model primary 2>/dev/null \
              | python3 -c 'import sys,json;print(json.load(sys.stdin).get("verdict","?"))' 2>/dev/null)
  verdict="${verdict:-?}"
  if [ "$verdict" = "hallucination" ]; then
    FAILS=$((FAILS+1))
    finding "{\"phase\":\"grounding-verdict\",\"ok\":false,\"verdict\":\"$verdict\",\"detail\":\"confabulation — answer asserted a value absent from the retrieved evidence\"}"
    echo "  ✗ GroundingVerdict: HALLUCINATION (confabulated beyond the evidence)"
  else
    finding "{\"phase\":\"grounding-verdict\",\"ok\":true,\"verdict\":\"$verdict\"}"
    echo "  ✓ GroundingVerdict: $verdict (no confabulation)"
  fi
}

# ── cross-node offload probe: the serviceability gate (--workload offload) ────
#
# The gap this closes. Every other assertion in this harness checks that the
# mesh AGREES about membership; none checks that a request can actually be
# SERVED by another node. Nothing in the 8250-test suite would fail if peer
# offload were 100% broken — and for weeks it was: `provider_for_peer` put the
# placeholder id "mesh-peer" on the wire for every unnamed Normal/Extended
# dispatch, the receiver resolved it to nobody and 503'd, and the origin
# quarantined a healthy peer after three strikes. This lane makes that class of
# outage a red run.
#
# How it forces an offload with no new config knob. `locate_named_model`
# (peer_inference.rs:1489-1522) is the ONE place local in-flight moves the
# local-vs-peer decision:
#
#     if local_inflight <= peer_inflight { Local } else { Peer }   // :1501
#
# With an idle peer gossiping current_in_flight=0, a peer wins as soon as the
# origin has one in-flight turn for the SAME name (the counter is keyed on the
# requested name at both the write, :1713, and the read, :1493). So N
# concurrent {"model":"primary"} turns against one node must push at least one
# onto a peer. `primary` is a mesh-advertised alias (slot_aliases.rs:46-82),
# emitted as a real manifest row off the Slow slot (oicp_synthesis.rs:149-195).
#
# Two preconditions, both load-bearing:
#   * the PEER's Slow slot must be loaded, or it advertises no `primary` row and
#     the origin correctly stays local (autostart=false here, so warm it);
#   * the peer's gossiped in-flight must have settled back to 0, else
#     local_inflight(1) <= peer_inflight(1) keeps the turn local. The gossip
#     interval is 10s (gossip.rs:57), so wait past one round.
#
# NB the ranked-ANONYMOUS class is deliberately not probed here: it cannot be
# forced on a homogeneous fleet, because the scorer reads local load from
# `local_observations`, which nothing on the dispatch path writes
# (`record_dispatch(None)` has zero callers). Local is therefore scored
# permanently idle and wins every tie (scheduler_core.rs:117-123). That class is
# gated in-process instead — sovereign-mesh/tests/chat_completion_e2e.rs,
# against a mock peer that RESOLVES `model` rather than accepting any body.
OFFLOAD_CONCURRENCY="${MESH_SOAK_OFFLOAD_CONCURRENCY:-3}"
OFFLOAD_SETTLE_SECS="${MESH_SOAK_OFFLOAD_SETTLE_SECS:-14}"

run_offload_probe() {
  local origin=0 peer=1 slo="${MESH_SOAK_CHAT_SLO_MS:-90000}"
  if [ "$NODES" -lt 2 ]; then
    # Not applicable, and NOT a pass: a single-node run verifies nothing about
    # cross-node serving. Recorded so the SLI denominator stays 0 instead of
    # silently reading as green.
    finding '{"kind":"offload","applicable":false,"detail":"needs >=2 nodes"}'
    echo "  ⊘ offload probe: skipped (NODES=$NODES < 2) — asserts nothing"
    return
  fi

  log "cross-node offload probe: warm node$peer, then $OFFLOAD_CONCURRENCY concurrent named turns on node$origin"

  # 1. Warm the peer so its Slow slot loads and it advertises `primary`.
  local w; w=$(chat_capture "$peer" "$slo" "$WORK/warm-peer.json" 16)
  echo "  warm node$peer → $w  (served by: $(served_model "$WORK/warm-peer.json"))"
  case "$w" in
    200\ *) ;;
    *) finding "{\"phase\":\"offload-serviceable\",\"ok\":false,\"violations\":[{\"invariant\":\"peer_offload_serviceable\",\"detail\":\"peer node$peer cannot serve its own chat ($w) — probe invalid\"}]}"
       FAILS=$((FAILS+1)); echo "  ✗ offload probe: peer cannot serve locally — nothing to offload TO"; return;;
  esac

  # 2. Warm the origin too, so slot-load time is not mistaken for queueing and
  #    the concurrent turns genuinely overlap.
  local w0; w0=$(chat_capture "$origin" "$slo" "$WORK/warm-origin.json" 16)
  echo "  warm node$origin → $w0  (served by: $(served_model "$WORK/warm-origin.json"))"

  # 3. Let the peer's gossiped in-flight settle back to 0.
  echo "  settling ${OFFLOAD_SETTLE_SECS}s for a fresh gossip round (interval 10s)"
  sleep "$OFFLOAD_SETTLE_SECS"

  # 4. Fire the concurrent turns. Longer max_tokens than chat_once so they
  #    actually overlap — a turn that finishes before the next starts leaves
  #    local_inflight at 0 and the probe proves nothing.
  local pids=() n
  for n in $(seq 1 "$OFFLOAD_CONCURRENCY"); do
    ( chat_capture "$origin" "$slo" "$WORK/offload-$n.json" 160 > "$WORK/offload-$n.code" ) &
    pids+=($!)
  done
  for n in "${pids[@]}"; do wait "$n" || true; done

  # 5. Attribute each turn from the response `model` field — the only
  #    client-visible statement of who served it.
  local peer_served=0 local_served=0 failed=0 models="" codes="" code m
  for n in $(seq 1 "$OFFLOAD_CONCURRENCY"); do
    code=$(cut -d' ' -f1 < "$WORK/offload-$n.code" 2>/dev/null); code="${code:-000}"
    m=$(served_model "$WORK/offload-$n.json")
    codes="$codes${codes:+,}$code"
    models="$models${models:+ | }${m:-<none>}"
    if [ "$code" != "200" ]; then failed=$((failed+1))
    elif [ "${m#*@ peer }" != "$m" ]; then peer_served=$((peer_served+1))
    else local_served=$((local_served+1)); fi
    echo "  turn $n → HTTP $code  served-by: ${m:-<none>}"
  done

  finding "{\"kind\":\"offload\",\"applicable\":true,\"origin\":$origin,\"attempts\":$OFFLOAD_CONCURRENCY,\"peer_served\":$peer_served,\"local_served\":$local_served,\"failed\":$failed,\"codes\":\"$codes\"}"

  # Both branches carry `violations` — pass as `[]`, fail as a real entry — so the
  # verdict feeds the EXISTING `invariant_violation_rate` SLI (mesh_soak.rs:307)
  # rather than needing a new one. A new SLI would have been the wrong move here:
  # the rate is only comparable across runs that actually ran the probe, so a
  # crash-lane baseline would false-alarm every offload run. The hard `ok:false`
  # (which bumps FAILS and fails the script) is the real gate; the SLI is the trend.
  local models_json; models_json=$(printf '%s' "$models" | python3 -c 'import sys,json;print(json.dumps(sys.stdin.read()))')
  if [ "$peer_served" -gt 0 ]; then
    finding "{\"phase\":\"offload-serviceable\",\"ok\":true,\"violations\":[],\"detail\":\"$peer_served/$OFFLOAD_CONCURRENCY turns served by a peer (local=$local_served fail=$failed)\",\"models\":$models_json}"
    echo "  ✓ PeerOffloadServiceable: $peer_served/$OFFLOAD_CONCURRENCY served cross-node"
  else
    finding "{\"phase\":\"offload-serviceable\",\"ok\":false,\"violations\":[{\"invariant\":\"peer_offload_serviceable\",\"detail\":\"NO turn was served by a peer (local=$local_served fail=$failed codes=$codes)\"}],\"models\":$models_json}"
    FAILS=$((FAILS+1))
    echo "  ✗ PeerOffloadServiceable: every turn stayed local — cross-node serving is broken or never selected"
    capture_forensics "offload-serviceable"
  fi
}

run_ingest_workload() {
  local target=0
  setup_ingest_recipe || { FAILS=$((FAILS+1)); finding '{"phase":"ingest-setup","ok":false,"detail":"recipe/source unavailable"}'; return; }
  local SLO_MS="${MESH_SOAK_CHAT_SLO_MS:-90000}"
  log "ingest×inference contention on node$target — chat under load (SLO ${SLO_MS}ms)"

  # Warm the primary slot once so first-token cost isn't charged to the window,
  # and confirm chat works at all before judging liveness (fail fast otherwise).
  local warm; warm=$(chat_once "$target" "$SLO_MS"); echo "  warm chat → $warm"
  case "$warm" in
    200\ *) ;;
    *) FAILS=$((FAILS+1)); finding "{\"phase\":\"ingest-warmup\",\"ok\":false,\"detail\":\"chat unavailable pre-ingest: $warm\"}"; return;;
  esac

  # Kick the daemon-side ingest (non-blocking — the daemon spawns the task).
  # Capture the HTTP response so a failed trigger is visible, not silent.
  local inst inst_code inst_body
  inst=$(curl -s -m 20 -w $'\n%{http_code}' -X POST "http://127.0.0.1:$(iport "$target")/internal/corpus/install" \
    -H 'content-type: application/json' \
    -d '{"corpus_id":"chaos-secret-agent","parameters":{}}' 2>&1)
  inst_code="${inst##*$'\n'}"; inst_body="${inst%$'\n'*}"
  echo "  ingest install → HTTP ${inst_code:-000}: ${inst_body:0:200}"
  finding "{\"kind\":\"fault\",\"action\":\"ingest-start\",\"node\":$target,\"http\":\"${inst_code:-000}\"}"

  local purl="http://127.0.0.1:$(iport "$target")/internal/corpus/progress"
  local DEADLINE; DEADLINE=$(( $(date +%s) + MINUTES*60 ))
  local ing_seen=0 ing_done=0 prog_changes=0 prev_prog="∅" ing_unread=0 ing_ok=0
  local chat_ok=0 chat_slow=0 chat_fail=0
  while [ "$(date +%s)" -lt "$DEADLINE" ]; do
    local res code ms ing prog; res=$(chat_once "$target" "$SLO_MS"); code="${res%% *}"; ms="${res##* }"
    # Active ingest tasks, from the daemon that owns them: /internal/corpus/status
    # (corpus_ingest.rs corpus_status, one entry per corpus with `active`). It was
    # `active_corpus_ingests` on svrn's /v1/mesh/status, which cw-rails' status
    # does not carry. An unread poll is not "none active": it is counted, and it
    # can neither latch completion nor make the grounding check authoritative.
    ing=$(jget "http://127.0.0.1:$(iport "$target")/internal/corpus/status" 'sum(1 for e in d["entries"] if e.get("active"))')
    if [ -z "$ing" ]; then ing_unread=$((ing_unread+1)); ing_ok=0; ing=0; else ing_ok=1; fi
    # Forward-progress signal: the per-corpus IngestProgress phase/percent. A
    # CHANGING value across polls is forward progress even while active stays 1
    # (ingest correctly throttled by — not starved by — foreground chat).
    prog=$(jget "$purl" 'json.dumps(d.get("progress",{}).get("chaos-secret-agent"))'); prog="${prog:-null}"
    { [ "$ing" -gt 0 ] || [ "$prog" != "null" ]; } && ing_seen=1
    [ "$prog" != "null" ] && [ "$prog" != "$prev_prog" ] && prog_changes=$((prog_changes+1))
    prev_prog="$prog"
    [ "$ing_seen" = 1 ] && [ "$ing_ok" = 1 ] && [ "$ing" = 0 ] && [ "$prog" = "null" ] && ing_done=1
    case "$code" in
      200) [ "$ms" -le "$SLO_MS" ] && chat_ok=$((chat_ok+1)) || chat_slow=$((chat_slow+1));;
      *)   chat_fail=$((chat_fail+1));;
    esac
    local ing_json=null; [ "$ing_ok" = 1 ] && ing_json="$ing"
    finding "{\"kind\":\"contention\",\"node\":$target,\"active_ingests\":$ing_json,\"prog_changes\":$prog_changes,\"chat_code\":\"$code\",\"chat_ms\":$ms}"
    echo "  ingest=$ing_json prog_advances=$prog_changes chat=$code ${ms}ms (ok=$chat_ok slow=$chat_slow fail=$chat_fail unread=$ing_unread)"
    # Observational retrieval probe (embed + knowledge/search only, no generation):
    # watch the corpus become queryable as ingest advances. Never fails here — a
    # partial index mid-ingest is legitimate; the post-ingest check is the gate.
    gp_n=$(grounded_retrieval "$target")
    finding "{\"kind\":\"grounding-probe\",\"node\":$target,\"n_chunks\":${gp_n:-0},\"ingest_active\":$ing}"
    echo "  retrieval probe: corpus chunks queryable = ${gp_n:-0}"
    wait_online_eq "$NODES" >/dev/null 2>&1 || true
    check "ingest-cycle" "$ALL_NODES" "$ALL_IDS"     # base invariants must hold DURING ingest
    [ "$ing_done" = 1 ] && { log "ingest completed (active→0, progress cleared)"; break; }
    sleep "${MESH_SOAK_CHAT_GAP_SECS:-8}"            # leave a slot > yield window so ingest progresses
  done

  # IngestProgress verdict — NON-STALLING (forward progress), not necessarily
  # completion: under heavy foreground chat the embed pipeline is correctly
  # throttled, so "advanced while chat stayed live" is the property. A frozen
  # progress phase (started, 0 advances) is the real stall failure.
  if [ "$ing_seen" = 0 ]; then
    finding '{"phase":"ingest-progress","ok":false,"detail":"ingest never observed (active + progress both absent) — install did not start"}'
    FAILS=$((FAILS+1)); echo "  ✗ IngestProgress: never observed"
  elif [ "$ing_done" = 0 ] && [ "$prog_changes" -lt 2 ]; then
    finding "{\"phase\":\"ingest-progress\",\"ok\":false,\"detail\":\"ingest started but progress froze (<2 advances, never completed) — stalled under chat load\"}"
    FAILS=$((FAILS+1)); echo "  ✗ IngestProgress: stalled (froze after start)"
  else
    finding "{\"phase\":\"ingest-progress\",\"ok\":true,\"detail\":\"forward progress ($prog_changes advances; completed=$ing_done)\"}"
    echo "  ✓ IngestProgress: non-stalling ($prog_changes advances, completed=$ing_done)"
  fi
  # ForegroundLiveness verdict (outcome class, not absolute latency).
  local total=$((chat_ok + chat_slow + chat_fail))
  if [ "$total" = 0 ] || [ "$chat_fail" -gt 0 ] || [ "$chat_ok" -lt $(( (total + 1) / 2 )) ]; then
    finding "{\"phase\":\"foreground-liveness\",\"ok\":false,\"detail\":\"ok=$chat_ok slow=$chat_slow fail=$chat_fail of $total under ingest\"}"
    FAILS=$((FAILS+1)); echo "  ✗ ForegroundLiveness: ok=$chat_ok slow=$chat_slow fail=$chat_fail"
  else
    finding "{\"phase\":\"foreground-liveness\",\"ok\":true,\"detail\":\"ok=$chat_ok slow=$chat_slow fail=$chat_fail of $total\"}"
    echo "  ✓ ForegroundLiveness: ok=$chat_ok slow=$chat_slow fail=$chat_fail"
  fi

  # ── grounding under contention: did the corpus ingested under load stay correct? ──
  # Hard-assert once the ingest TASK is idle (no `active` entry on a READ
  # /internal/corpus/status poll) — the true
  # completion signal. NB the lane's ing_done ALSO requires the per-corpus progress
  # entry to go null, but the daemon leaves a terminal (non-null) progress record
  # after a completed ingest, so ing_done under-reports completion (a finished,
  # fully-queryable corpus never latches it). active==0 is the robust signal, and
  # keying the hard gate on it makes "ingest finished but corpus NOT queryable" a
  # real, catchable failure. Still-active (ing>0) at loop exit ⇒ soft: the index is
  # legitimately partial (chat throttled it), so don't false-fail on it.
  if [ "$ing_seen" = 1 ]; then
    if [ "$ing_ok" = 1 ] && [ "$ing" = 0 ]; then
      log "grounding check (ingest idle — authoritative) on node$target"
      grounding_verdict "$target" hard
    else
      log "grounding probe (ingest still active — soft) on node$target"
      grounding_verdict "$target" soft
    fi
  fi

  # Free extra coverage: this lane already has a generative primary loaded on
  # every node, which is the only precondition the offload probe needs.
  run_offload_probe
}

# ── corrupt-persisted-state OS-fault (--workload corrupt) ─────────────────────
# An OS-level fault that needs NO container: pre-write garbage into a node's
# cw-rails mesh.json (the store `identity::load_mesh` reads at start), then
# restart. Two distinct properties: (1) cw-rails FAILS SAFE — it refuses the
# store by name and leaves the separate node_id file alone, so it never boots
# with an empty roster or a garbage id; (2) with the store moved aside the node
# has no MEMBERSHIP, and in the netns (no mDNS) it can't re-discover peers on
# its own, so the loss is repaired by a RE-JOIN under the same id — after which
# the mesh reconverges. UniqueIds + NoGhost + convergence are the net. (The crash lane
# bare-resumes because its mesh.json is intact; only the corrupt lane re-joins.
# cgroup-OOM and disk-full are the other OS-faults in this tier; see MESH_QA.md —
# all rootless, no podman, per the toolbox decision.)
run_corrupt_state_workload() {
  local victim=$(( NODES > 1 ? 1 : 0 ))
  local r; r=$(rdir "$victim")
  local mj="$r/mesh.json" id_before refused=0 rpid _
  log "corrupt-persisted-state on node$victim — kill, corrupt cw-rails' mesh.json, expect a named refusal, then recover + re-join"
  kill_node "$victim"
  finding "{\"kind\":\"fault\",\"action\":\"kill-9\",\"node\":$victim,\"cycle\":0}"
  echo "  waiting ${DECAY_WAIT}s for offline-decay…"; sleep "$DECAY_WAIT"
  id_before=$(fhex "$victim")
  echo "  corrupting durable state: $mj"
  printf '{ this is not valid mesh json :: %s' "$RANDOM" > "$mj"
  finding "{\"kind\":\"fault\",\"action\":\"corrupt-mesh-json\",\"node\":$victim}"

  # (1) Fail-safe: cw-rails refuses a store that is not a mesh rather than
  # booting with an empty roster it would gossip to peers (identity.rs,
  # a_corrupt_mesh_file_is_refused_rather_than_read_as_empty). Measured
  # 2026-10-03: exit 1 inside a second, naming the file — "<path> is not a
  # mesh: …". Serving over the garbage, or exiting without naming the file, or
  # touching node_id, is the failure.
  boot_rails "$victim"; rpid="${RPIDS[$victim]}"
  for _ in $(seq 1 30); do kill -0 "$rpid" 2>/dev/null || break; sleep 0.5; done
  if kill -0 "$rpid" 2>/dev/null; then
    kill -9 "$rpid" 2>/dev/null
    echo "  ✗ cw-rails on node$victim kept running over a corrupt mesh.json"
  elif rails_logged "$victim" 'mesh\.json is not a mesh'; then
    refused=1
  else
    echo "  ✗ cw-rails on node$victim exited without naming mesh.json: $(rails_log "$victim" | tail -1)"
  fi
  if [ "$refused" = 1 ] && [ "$(fhex "$victim")" = "$id_before" ]; then
    finding "{\"phase\":\"corrupt-state-refused\",\"ok\":true,\"detail\":\"node$victim cw-rails refused the corrupt mesh.json by name; node_id unchanged\"}"
    echo "  ✓ node$victim cw-rails refused the corrupt store by name (node_id unchanged)"
  else
    FAILS=$((FAILS+1)); capture_forensics "corrupt-state-refused"
    finding "{\"phase\":\"corrupt-state-refused\",\"ok\":false,\"detail\":\"node$victim: refused_by_name=$refused node_id_before=$id_before after=$(fhex "$victim")\"}"
  fi

  # (2) Recovery, the operator's: move the store aside (kept, never deleted),
  # bring the node up solo, and re-join — in the netns (no mDNS) nothing
  # re-discovers peers on its own. Same name, so the founder keeps its id
  # (membership.rs join refuses a reassigned id).
  mv "$mj" "$mj.corrupt"
  boot_node "$victim"
  if wait_port "$victim"; then
    finding "{\"phase\":\"corrupt-state-recover\",\"ok\":true,\"detail\":\"node$victim up solo with its store moved aside\"}"
    log "node$victim re-joining the founder (membership lost with the store)"
    join_to_founder "$victim" "$FLINK"
    finding "{\"kind\":\"fault\",\"action\":\"corrupt-rejoin\",\"node\":$victim}"
  else
    FAILS=$((FAILS+1)); capture_forensics "corrupt-state-recover"
    finding "{\"phase\":\"corrupt-state-recover\",\"ok\":false,\"detail\":\"node$victim did not come back up with its store moved aside\"}"
    echo "  ✗ node$victim did NOT come back up after the store was moved aside"
  fi
  wait_online_eq "$NODES" || true
  NODE_IDS[$victim]=$(robust_self_id "$victim"); ALL_IDS=$(IFS=,; echo "${NODE_IDS[*]}")
  check "corrupt-state-healed" "$ALL_NODES" "$ALL_IDS"   # UniqueIds: no garbage/colliding id adopted
}

# ── P2: app-user desktops on nodes (--with-desktops) ──────────────────────────
# Each node also runs a headless desktop (attach-mode, in this netns) + an app-
# user persona driver, so user-visible TURN invariants are asserted WHILE the
# soak kills/restarts the node underneath it. The desktop attaches to its node
# via a baked SetupConfig whose [daemon] ports ARE the node's: detect() probes
# the client port, finds the live node, and Attaches; internal_port then flows to
# /internal/* calls through the desktop's AppState accessors (P2.1). The driver
# speaks ONLY the command bridge (the production webview.on_message dispatch
# path) and emits findings in THIS script's JSONL schema, so the verdict folds
# them in. Headline cross-layer assertion: a user's turns survive a peer-daemon
# kill (graceful incomplete, then recovery), and completed turns never violate a
# turn invariant.
DESKTOP_BIN="${SOVEREIGN_DESKTOP_BIN:-$ROOT/target/debug/sovereign-desktop}"
declare -a DESK_PIDS DRIVER_PIDS
bridge_port() { echo $((9745 + $1)); }

bake_desktop_config() {  # <i> — write the desktop's SetupConfig, echo its HOME
  local i="$1" home="$WORK/desktop$i/home"
  mkdir -p "$home/.svrnmesh" "$WORK/desktop$i/data"
  cat > "$home/.svrnmesh/config.toml" <<EOF
[models]
primary = "$PRIMARY_MODEL"
embed = "$EMBED_MODEL"
context_size = 4096
[daemon]
client_port = $(cport "$i")
internal_port = $(iport "$i")
client_bind = "127.0.0.1"
rails_base = "$(rails_url "$i")"
[data]
dir = "$WORK/desktop$i/data"
EOF
  # The desktop ALSO reads a DesktopConfig at $XDG_CONFIG_HOME/sovereign/desktop.toml.
  # bootstrap_with_progress() requires config.model_path to EXIST and loads it
  # even in attach mode (state.rs:309) — the default "models/fast.gguf" doesn't
  # exist in a scratch profile, so without this bootstrap returns Err early and
  # the chat Runtime never builds ("Backend is still loading" on every turn).
  mkdir -p "$home/.config/sovereign"
  cat > "$home/.config/sovereign/desktop.toml" <<EOF
model_path = "$PRIMARY_MODEL"
embed_model_path = "$EMBED_MODEL"
data_dir = "$WORK/desktop$i/data"
setup_complete = true
EOF
  printf '%s' "$home"
}

spawn_desktop_for_node() {  # <i>
  local i="$1" home bp up=0 _; home=$(bake_desktop_config "$i"); bp=$(bridge_port "$i")
  # setsid → own process group so teardown can group-kill the desktop + children.
  # Display env carries through the netns re-exec (Wayland pathname socket
  # survives — verified by the Phase-0 probe). No SOVEREIGN_USE_SUPERVISOR:
  # detect() finds the live node on its client port and pure-Attaches.
  setsid env \
    HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
    XDG_CACHE_HOME="$home/.cache" \
    DISPLAY="${DISPLAY:-}" WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-}" \
    XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-}" XDG_SESSION_TYPE="${XDG_SESSION_TYPE:-}" \
    DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-}" \
    SOVEREIGN_COMMAND_BRIDGE=1 SOVEREIGN_COMMAND_BRIDGE_PORT="$bp" \
    SOVEREIGN_SERVE_PORT="$(sport "$i")" \
    "$DESKTOP_BIN" > "$WORK/desktop$i/desktop.log" 2>&1 &
  DESK_PIDS[$i]=$!
  for _ in $(seq 1 60); do
    curl -s -m 2 -o /dev/null "http://127.0.0.1:$bp/healthz" 2>/dev/null && { up=1; break; }
    kill -0 "${DESK_PIDS[$i]}" 2>/dev/null || break; sleep 1; done
  if [ "$up" = 1 ]; then
    echo "  desktop$i bridge up :$bp (attached to node$i client :$(cport "$i") internal :$(iport "$i"))"
    return 0
  fi
  echo "  desktop$i FAILED to bring up its bridge on :$bp (see $WORK/desktop$i/desktop.log)"
  finding "{\"phase\":\"app-desktop-spawn\",\"ok\":false,\"node\":$i,\"detail\":\"bridge never came up on :$bp\"}"
  FAILS=$((FAILS+1)); return 1
}

spawn_driver_for_node() {  # <i>
  local i="$1" bp; bp=$(bridge_port "$i")
  : > "$WORK/driver$i-findings.jsonl"
  SOVEREIGN_BRIDGE_URL="http://127.0.0.1:$bp" \
  SOVEREIGN_DRIVER_FINDINGS="$WORK/driver$i-findings.jsonl" \
  SOVEREIGN_DRIVER_NODE="$i" \
  SOVEREIGN_DRIVER_MINUTES="${DRIVER_MINUTES:-$MINUTES}" \
  SOVEREIGN_DRIVER_CORPUS="${MESH_SOAK_GCORPUS:-chaos-secret-agent}" \
  SOVEREIGN_DRIVER_TRANSCRIPT="$REPRO_DIR/seed${SEED}-app-node$i" \
    node "$ROOT/scripts/mesh-app-driver.mjs" > "$WORK/driver$i.log" 2>&1 &
  DRIVER_PIDS[$i]=$!
  echo "  driver$i → bridge :$bp (findings $WORK/driver$i-findings.jsonl)"
}

wait_drivers() {  # block until every app driver exits (they run ~DRIVER_MINUTES)
  local i
  for i in $(seq 0 $((NODES-1))); do
    [ -n "${DRIVER_PIDS[$i]:-}" ] && wait "${DRIVER_PIDS[$i]}" 2>/dev/null
  done
}

# Let the app surface ESTABLISH before chaos: each driver's warm turn triggers a
# cold model load (~tens of seconds) on its node. If the crash loop's first kill
# lands during a victim's warm load, app-warm would false-fail. Barrier on each
# driver having logged its app-warm finding (capped) before we start killing.
_has_warm() { python3 -c "
import sys
try: sys.exit(0 if any('\"phase\": \"app-warm\"' in l or '\"phase\":\"app-warm\"' in l for l in open(sys.argv[1])) else 1)
except Exception: sys.exit(1)" "$1"; }
wait_drivers_warm() {
  local i ready _
  echo "  waiting for app drivers to warm (establish the surface before chaos)…"
  for _ in $(seq 1 80); do ready=1
    for i in $(seq 0 $((NODES-1))); do
      [ -n "${DRIVER_PIDS[$i]:-}" ] || continue
      _has_warm "$WORK/driver$i-findings.jsonl" || ready=0
    done
    [ "$ready" = 1 ] && { echo "  ✓ all app drivers warmed — starting chaos"; return 0; }
    sleep 3
  done
  echo "  (warm-up barrier timed out after ~240s; proceeding to chaos anyway)"
}

# P2.3 — fold each driver's turn findings into the unified stream + verdict. A
# `phase` finding with ok:false is a counted checkpoint failure (a real turn-
# invariant violation, or the post-chaos recovery turn failing); `kind` findings
# (chaos-incomplete turns, summaries) are observational and never fail the run.
fold_driver_findings() {
  local i f fails
  for i in $(seq 0 $((NODES-1))); do
    f="$WORK/driver$i-findings.jsonl"; [ -f "$f" ] || continue
    cat "$f" >> "$FINDINGS"
    fails=$(python3 -c "
import json,sys
n=0
for line in open('$f'):
    try: d=json.loads(line)
    except Exception: continue
    if 'phase' in d and d.get('ok') is False: n+=1
print(n)" 2>/dev/null || echo 0)
    if [ "${fails:-0}" -gt 0 ]; then
      FAILS=$((FAILS+fails))
      echo "  ✗ node$i app-driver: $fails turn-invariant/recovery failure(s)"
      capture_forensics "app-node$i"
    else
      echo "  ✓ node$i app-driver: turn invariants held"
    fi
  done
}

# P3 — controlled cross-layer probe at an orchestrator-chosen moment (the victim's
# node was JUST killed, or JUST healed). Runs ONE turn against the victim's
# desktop bridge and folds the verdict directly into $FINDINGS + FAILS. expect:
#   fail-fast — outage: the turn must error FAST (not hang on the dead daemon).
#   complete  — recovery: a fresh turn must complete cleanly after restart.
# This is the HARD cross-layer assertion (the autonomous driver is the backdrop).
probe_user() {  # probe_user <node> <label> <fail-fast|complete> <timeout_secs>
  local i="$1" label="$2" expect="$3" tmo="$4" out ok detail bp; bp=$(bridge_port "$i")
  out=$(SOVEREIGN_BRIDGE_URL="http://127.0.0.1:$bp" SOVEREIGN_DRIVER_NODE="$i" \
        node "$ROOT/scripts/mesh-app-driver.mjs" --probe --label "$label" --expect "$expect" --timeout "$tmo" 2>/dev/null)
  [ -n "$out" ] && printf '%s\n' "$out" >> "$FINDINGS"
  ok=$(printf '%s' "$out" | python3 -c 'import sys,json
try: print("yes" if json.load(sys.stdin).get("ok") else "no")
except Exception: print("err")' 2>/dev/null)
  detail=$(printf '%s' "$out" | python3 -c 'import sys,json
try: print(json.load(sys.stdin).get("detail",""))
except Exception: print("no probe output")' 2>/dev/null)
  if [ "$ok" = "yes" ]; then
    echo "  ✓ $label node$i: $detail"
  else
    FAILS=$((FAILS+1)); echo "  ✗ $label node$i: $detail"; capture_forensics "$label-node$i"
  fi
}

teardown() { log "teardown"
  for i in $(seq 0 $((NODES-1))); do
    [ -n "${DRIVER_PIDS[$i]:-}" ] && kill "${DRIVER_PIDS[$i]}" 2>/dev/null
    [ -n "${DESK_PIDS[$i]:-}" ] && kill -- "-${DESK_PIDS[$i]}" 2>/dev/null  # group-kill the setsid desktop
    kill_node "$i"
  done
  [ "$KEEP" = 0 ] && rm -rf "$WORK"; }
trap teardown EXIT

# ── bring up the mesh ─────────────────────────────────────────────────────────
log "backend=$BACKEND workload=$WORKLOAD nodes=$NODES minutes=$MINUTES seed=$SEED primary=$(basename "$PRIMARY_MODEL") embed=$(basename "$EMBED_MODEL")"
for i in $(seq 0 $((NODES-1))); do boot_node "$i"; done
for i in $(seq 0 $((NODES-1))); do
  if wait_port "$i"; then
    echo "  node$i up"
  else
    # A node that never bound must FAIL the run, not shrink it. Previously this
    # printed a line and carried on: the remaining nodes formed a smaller mesh
    # and every invariant passed over it (convergence and liveness are vacuous
    # on one reachable node), so a totally failed bring-up looked green.
    echo "  node$i FAILED to come up — see $(rdir "$i")/rails.*.log and $WORK/node$i/daemon.*.log"
    finding "{\"phase\":\"boot\",\"ok\":false,\"violations\":[{\"invariant\":\"all_nodes_booted\",\"detail\":\"node$i: cw-rails status or svrn /status never answered 200\"}]}"
    FAILS=$((FAILS+1))
  fi
done

# Every cw-rails starts solo; node0 founds, the rest join by its invite.
FKEY=""; FLINK=""
if found_mesh; then
  log "founder key=$FKEY — joining $((NODES-1)) peers over iroh (dial-by-key)"
  for i in $(seq 1 $((NODES-1))); do join_to_founder "$i" "$FLINK"; done
fi

log "waiting for convergence to $NODES members"
for _ in $(seq 1 45); do conv=1
  for i in $(seq 0 $((NODES-1))); do [ "$(jget "$(status_url $i)" 'd["members_total"]')" = "$NODES" ] || conv=0; done
  [ "$conv" = 1 ] && break; sleep 1; done
for i in $(seq 0 $((NODES-1))); do NODE_IDS[$i]=$(robust_self_id "$i"); done
# check-invariants polls each node's cw-rails (mesh_cmd.rs cmd_check_invariants).
ALL_NODES=$(for i in $(seq 0 $((NODES-1))); do printf '127.0.0.1:%s,' "$(rport $i)"; done | sed 's/,$//')
ALL_IDS=$(IFS=,; echo "${NODE_IDS[*]}")
echo "  converged: node0 online=$(online_count 0)/$NODES"
check "healthy" "$ALL_NODES" "$ALL_IDS"
# Iroh axis: the mesh converged — now prove it converged OVER iroh.
assert_iroh_carried_traffic

# ── P2: bring up app-user desktops + persona drivers on every node, BEFORE the
# chaos starts, so real users are operating the app while the mesh is savaged. ──
if [ "$DESKTOPS" = 1 ]; then
  [ -x "$DESKTOP_BIN" ] || { echo "  --with-desktops: desktop binary missing at $DESKTOP_BIN (build it or set SOVEREIGN_DESKTOP_BIN)"; FAILS=$((FAILS+1)); }
  log "P2: spawning $NODES app desktops + persona drivers (attach-mode, in-netns)"
  for i in $(seq 0 $((NODES-1))); do
    spawn_desktop_for_node "$i" && spawn_driver_for_node "$i"
  done
  wait_drivers_warm   # barrier: surface established before chaos starts
fi

# ── workload: ingest×inference contention, or repeated crash/churn cycles ─────
if [ "$WORKLOAD" = "ingest" ]; then
  run_ingest_workload
elif [ "$WORKLOAD" = "corrupt" ]; then
  run_corrupt_state_workload
elif [ "$WORKLOAD" = "offload" ]; then
  run_offload_probe
else
DEADLINE=$(( $(date +%s) + MINUTES*60 )); CYCLE=0
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  CYCLE=$((CYCLE+1))
  # With desktops attached, ROTATE the victim so every non-founder user gets
  # disrupted over the run (a random seed can otherwise hit the same node every
  # cycle — seed=1 picked node2 all 3 times, leaving node1's user untested).
  # Plain crash lane keeps the seeded-random pick for victim-choice fuzzing.
  if [ "$DESKTOPS" = 1 ]; then victim=$(( (CYCLE-1) % (NODES-1) + 1 )); else victim=$(( RANDOM % (NODES-1) + 1 )); fi
  log "cycle $CYCLE — crash node$victim (kill -9, cw-rails and daemon)"
  kill_node "$victim"
  finding "{\"kind\":\"fault\",\"action\":\"kill-9\",\"node\":$victim,\"cycle\":$CYCLE}"

  # P3 cross-layer assertion — node$victim's daemon is DOWN: its user's turn must
  # fail FAST (graceful error), not hang on the dead daemon. (Runs ≤30s inside the
  # decay window below.)
  [ "$DESKTOPS" = 1 ] && probe_user "$victim" "app-outage-graceful" "fail-fast" 30

  echo "  waiting ${DECAY_WAIT}s for offline-decay…"; sleep "$DECAY_WAIT"
  # survivors must now show the victim OFFLINE (decayed, not a live ghost)
  surv_nodes=""; surv_ids=""
  for i in $(seq 0 $((NODES-1))); do [ "$i" = "$victim" ] && continue
    surv_nodes+="127.0.0.1:$(rport $i),"; surv_ids+="${NODE_IDS[$i]},"; done
  surv_nodes="${surv_nodes%,}"; surv_ids="${surv_ids%,}"
  echo "  node0 sees node$victim as: $(sees_status 0 "${NODE_IDS[$victim]}")"
  wait_online_eq "$((NODES-1))" "$victim" || true   # all survivors must see the victim decayed
  check "post-crash-decay" "$surv_nodes" "$surv_ids"

  # churn: restart — a production restart RESUMES its identity + mesh from
  # cw-rails' root (node_id + mesh.json) and gossip-reconverges to online. We
  # deliberately do NOT call join_to_founder: that would exercise an explicit
  # re-join rather than the normal restart path. (The id-collision the 8h soak
  # first surfaced was a harness bug in boot_node — a leaked-loop-var data-dir
  # cross-wire on restart, since fixed — not a daemon bug. UniqueIds guards it.)
  if [ $((CYCLE % 2)) -eq 0 ]; then
    log "cycle $CYCLE — restart node$victim (resume, no re-join) [kill-9-startup-window torture]"
    torture_restart "$victim"
  else
    log "cycle $CYCLE — restart node$victim (resume, no re-join)"
    boot_node "$victim"
  fi
  wait_port "$victim"
  wait_online_eq "$NODES" || true     # ALL nodes must see full reconvergence, not just node0
  NODE_IDS[$victim]=$(robust_self_id "$victim"); ALL_IDS=$(IFS=,; echo "${NODE_IDS[*]}")
  echo "  reconverged: node0 online=$(online_count 0)/$NODES"
  check "healed" "$ALL_NODES" "$ALL_IDS"

  # P3 cross-layer assertion — node$victim is back: its user's surface must
  # RECOVER (a fresh turn completes cleanly; the daemon reloads its model cold on
  # the first request, so allow a generous window).
  [ "$DESKTOPS" = 1 ] && probe_user "$victim" "app-outage-recovered" "complete" 180

  # load: timed /v1/mesh/status queries against node0's cw-rails (latency SLIs
  # for the gate)
  for _ in $(seq 1 20); do
    ms=$(curl -s -m 4 -o /dev/null -w '%{time_total}' "$(status_url 0)" 2>/dev/null)
    finding "{\"kind\":\"load\",\"latency_ms\":$(python3 -c "print(round(float('${ms:-0}')*1000,2))" 2>/dev/null || echo 0),\"ok\":$([ -n "$ms" ] && echo true || echo false)}"
  done
done
fi

# ── P2: collect the app-user drivers + fold their turn findings into the verdict ──
if [ "$DESKTOPS" = 1 ]; then
  log "P2: waiting for app drivers to finish; folding turn findings into the verdict"
  wait_drivers
  fold_driver_findings
fi

# ── verdict ───────────────────────────────────────────────────────────────────
log "VERDICT — $CYCLE cycle(s), $FAILS checkpoint failure(s); findings=$FINDINGS"
# Coverage accounting: fold the findings into a fault × invariant grid so a run
# self-documents what it actually exercised — gaps visible, not assumed covered.
python3 - "$FINDINGS" "$WORKLOAD" <<'PY'
import sys, json
from collections import Counter
faults, phases, pfail = Counter(), Counter(), Counter()
runs = 0
for line in open(sys.argv[1]):
    try: d = json.loads(line)
    except Exception: continue
    if d.get("kind") == "fault": faults[d.get("action", "?")] += 1
    # A checkpoint is a `phase` record with no `kind`. The forensics marker
    # carries BOTH (kind=forensics, phase=<the failing label>), so counting on
    # `phase` alone credited every failure with a phantom extra checkpoint —
    # and, having no `ok` field, it defaulted to a PASS. A single failed
    # checkpoint therefore printed as "1/2✓". The SLI was always right (the
    # forensics record has no `violations` key, so soak_slis excludes it); only
    # this summary misread.
    if "phase" in d and "kind" not in d:
        phases[d["phase"]] += 1; runs += 1
        if not d.get("ok", True): pfail[d["phase"]] += 1
workload = sys.argv[2] if len(sys.argv) > 2 else "crash"
INV = ["convergence", "no_ghost_members", "liveness", "unique_ids",
       "admission_safety", "bounded_fan_out", "shared_model_single_host"]
print("  ── coverage accounting ──────────────────────────────────")
print(f"  workload        : {workload}")
print("  faults injected : " + (", ".join(f"{k}×{v}" for k, v in sorted(faults.items())) or "none"))
print("  checkpoints     : " + (", ".join(f"{k} {v-pfail[k]}/{v}✓" for k, v in sorted(phases.items())) or "none"))
print(f"  invariant pack  : {len(INV)} invariants × {runs} checkpoints = {len(INV)*runs} cell-checks")
print("                    " + ", ".join(INV))
if workload in ("ingest", "offload"):
    print("  live this lane  : PeerOffloadServiceable — a named turn actually served by")
    print("                    ANOTHER node, attributed from the response's own `model`")
    print("                    field ('@ peer <name>'). The one assertion that fails on a")
    print("                    total peer-offload outage.")
if workload == "ingest":
    print("  live this lane  : IngestProgress + ForegroundLiveness +")
    print("                    GroundingIntegrity/GroundingVerdict (real generative")
    print("                    primary under ingest; grounded RAG turn).")
# The checker reads these three from the /v1/mesh/status it polls, which is
# cw-rails' since pb-mesh-exit-transport, and no program emits the fields it
# reads there (peer_inflight_current/_ceiling, fanout_inflight_current,
# shared_model_host): svrn's counters left with its own status, and
# `EmbeddedDaemon::glassbox_signals` has no caller. Every lane passes them
# vacuously until a producer exists, so they are named, never counted as live.
print("  inert, all lanes: admission_safety + bounded_fan_out + shared_model_single_host")
print("                    (no status the checker polls carries their fields since the")
print("                    rails flip) — exercised only by the in-process DST suite.")
PY
# grep -c prints "0" AND exits 1 on no-match — a trailing `|| echo 0` would
# double it. Take grep's own count, default empty (missing file) to 0.
ok=$(grep -c '"kind":"iroh".*"ok":true' "$FINDINGS" 2>/dev/null); ok=${ok:-0}
bad=$(grep -c '"kind":"iroh".*"ok":false' "$FINDINGS" 2>/dev/null); bad=${bad:-0}
echo "  ── iroh ─────────────────────────────────────────────────"
echo "  transport       : iroh, cw-rails' only mesh transport (local-only: direct addrs)"
echo "  join path       : dial-by-key over the founder's iroh= invite"
echo "  iroh checks     : ${ok} ok / ${bad} failed (install + carried-over-iroh per node + status dials)"
if [ "$GATE" = 1 ]; then
  log "SLO gate"
  "$CLI" mesh soak-gate "$FINDINGS" --baseline "$ROOT/mesh-soak-baseline.json" || true
fi
[ "$FAILS" = 0 ] && { echo "  PASS ✓ — invariants held across all checkpoints"; exit 0; } \
                 || { echo "  FAIL ✗ — $FAILS checkpoint(s) violated"; exit 1; }
