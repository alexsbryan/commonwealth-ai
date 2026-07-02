#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# mesh-soak.sh — Layer-2 multi-process mesh soak: the "real bytes" layer of the
# mesh QA stack (Layer 1 = in-process DST, `sovereign-mesh --features dst`;
# Layer 3 = the SLO gate, `sovereign mesh soak-gate`). It boots N real
# `sovereign daemon` processes, forms one mesh over real TCP gossip, then drives
# real faults (SIGKILL crash + churn/restart) in repeated cycles, asserting the
# HTTP-observable invariant pack via `sovereign mesh check-invariants` (the
# unit-tested assertion engine — sovereign-cli-llm/src/mesh_soak.rs) at every
# checkpoint. Findings stream to mesh-soak-findings.jsonl for the SLO gate.
#
# ── What it exercises that the in-process DST suite cannot ────────────────────
#   Real process crashes (SIGKILL) + real wall-clock offline-decay + real churn
#   across actual OS-process and TCP boundaries — the fix-A decay path holding
#   under a genuine kill -9, not a simulated `down` flag.
#
# ── Isolation (this is load-bearing) ──────────────────────────────────────────
#   The `local` backend re-execs the whole soak inside a ROOTLESS NETWORK
#   NAMESPACE (`unshare -rn`, loopback-only). Why: the daemon has no mDNS-disable
#   knob and the CLI `mesh join` is hardcoded to :9741 — run on the bare host it
#   would mDNS-advertise to (and try to join) the operator's real production
#   mesh. The netns seals it: test daemons see only `lo`, self-advertise
#   127.0.0.1, and form their mesh entirely on localhost. Zero production
#   cross-talk. (Verified: the host's real mesh member count is unchanged across
#   a full soak.)
#
# ── Models by workload ────────────────────────────────────────────────────────
#   crash lane: daemons only boot + gossip + serve /v1/mesh/status (no chat), so
#   primary == embed == a small embedding GGUF (~600MB/node) — N fit in RAM.
#   ingest lane: a REAL generative primary (so chat runs) + the 0.6B embed.
#
# Usage:
#   scripts/mesh-soak.sh [--nodes N] [--minutes M] [--seed S]
#                        [--workload crash|ingest|corrupt] [--keep] [--gate]
#                        [--with-desktops] [--driver-minutes M] [--iroh]
#
#   --iroh (SOAK_IROH) runs the transport-migration axis: every node boots with
#     `[iroh] enabled = true` (all traffic classes route iroh-first, IP fallback
#     retained), peers join over the founder's dial-by-key invite, and the run
#     asserts each node actually carried mesh traffic over iroh. The netns is
#     loopback-only (no internet → no relay); nodes dial by key over gossiped
#     direct addrs — the LAN-without-internet iroh path. See TRANSPORT_MIGRATION.md W3.
#
#   --with-desktops (P2) hangs a headless desktop (attach-mode) + an app-user
#     persona driver on EACH node, in the netns, so user-visible TURN invariants
#     (stream integrity, intent, finish_reason, citation resolution, post-chaos
#     recovery) are asserted WHILE the soak kills/restarts the node underneath.
#     Forces a generative primary (set MESH_SOAK_MODEL). Pairs with --workload
#     crash (users on the app while the mesh is savaged). Needs a built desktop
#     binary (cargo build -p sovereign-desktop) at target/debug/sovereign-desktop.
#
#   --workload corrupt pre-writes garbage into a node's durable mesh.json then
#     resumes it — the daemon must fail-safe (regenerate/reject, no id collision).
#     A container-free OS-fault (the OS-fault tier — cgroup-OOM / disk-full /
#     partition — is rootless, no podman; see MESH_QA.md).
#   --workload ingest drives a daemon corpus ingest concurrently with chat and
#     asserts both progress (IngestProgress + ForegroundLiveness). Needs the chaos
#     corpus cached once (online) via scripts/setup-chaos-corpus.sh, a generative
#     MESH_SOAK_MODEL (default models/Qwen3.5-2B.Q6_K.gguf), and yield<30s.
#     ~3GB/node — stop the production 35B daemon first; fits a workstation at N=3.
#
# Prereq: a built sovereign-cli (cargo build --bins; debug is fine), the model(s)
# below, and `ip` + `unshare` for the netns.
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
# --iroh (SOAK_IROH): the transport-migration axis. Boots every node with
# `[iroh] enabled = true` so all traffic classes route iroh-first (IP fallback
# retained), joins peers over the founder's dial-bearing invite (the `dial=`
# path), and asserts each node actually carried mesh traffic over iroh. The
# netns is loopback-only with no internet, so relays are unreachable — nodes
# dial by key over gossiped `iroh_direct_addrs` (127.0.0.1), which is exactly
# the LAN-without-internet iroh path. See TRANSPORT_MIGRATION.md W3.
SOAK_IROH="${SOAK_IROH:-0}"
while [ $# -gt 0 ]; do
  case "$1" in
    --nodes)    NODES="$2"; shift 2;;
    --minutes)  MINUTES="$2"; shift 2;;
    --seed)     SEED="$2"; shift 2;;
    --workload) WORKLOAD="$2"; shift 2;;
    --with-desktops) DESKTOPS=1; shift;;
    --iroh)     SOAK_IROH=1; shift;;
    --driver-minutes) DRIVER_MINUTES="$2"; shift 2;;
    --keep)     KEEP=1; shift;;
    --gate)     GATE=1; shift;;
    -h|--help)  sed -n '3,44p' "$0"; exit 0;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done
case "$WORKLOAD" in crash|ingest|corrupt) ;; *) echo "bad --workload: $WORKLOAD (crash|ingest|corrupt)" >&2; exit 2;; esac
# Normalise SOAK_IROH to a shell flag (0/1) + a TOML bool the config heredoc
# splices verbatim. Accept the usual truthy spellings so `SOAK_IROH=true` and
# `--iroh` and `SOAK_IROH=1` all mean the same thing.
case "$SOAK_IROH" in 1|true|yes|on) IROH_ON=1; IROH_TOML=true;; *) IROH_ON=0; IROH_TOML=false;; esac

# ── Re-exec into a fresh rootless netns (loopback up) for the local backend ────
if [ "$BACKEND" = "local" ] && [ -z "${MESH_SOAK_IN_NETNS:-}" ]; then
  exec unshare -rn env MESH_SOAK_IN_NETNS=1 \
    NODES="$NODES" MINUTES="$MINUTES" SEED="$SEED" KEEP="$KEEP" GATE="$GATE" \
    MESH_SOAK_BACKEND="$BACKEND" WORKLOAD="$WORKLOAD" \
    DESKTOPS="$DESKTOPS" DRIVER_MINUTES="$DRIVER_MINUTES" SOAK_IROH="$SOAK_IROH" bash "$0"
fi
[ "$BACKEND" = "local" ] && ip link set lo up

CLI="${SOVEREIGN_CLI:-$ROOT/target/debug/sovereign-cli}"
[ -x "$CLI" ] || CLI="$ROOT/target/release/sovereign-cli"
[ -x "$CLI" ] || { echo "sovereign-cli not built (cargo build --bins)"; exit 1; }
# Model profile by workload. The crash lane only needs daemons that boot + gossip,
# so primary == embed == a tiny embedding GGUF (N fit in RAM, no chat is made).
# The ingest lane needs a REAL generative primary (so chat actually runs) plus the
# small embed model (so corpus ingest's embed pipeline is cheap), and a yield
# window < 30s (mandatory — else the 30s health-ping starves ingest; see
# setup-chaos-corpus.sh / chaos_monkey README).
EMBED_DEFAULT="$ROOT/models/qwen-embedding-0.6b.gguf/Qwen3-Embedding-0.6B-Q8_0.gguf"
if [ "$WORKLOAD" = "ingest" ] || [ "$DESKTOPS" = 1 ]; then
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
[ "$WORKLOAD" = "ingest" ] && export SOVEREIGN_RECIPES_DIR="$HOME/.sovereign/recipes"

WORK="$(mktemp -d -t mesh-soak.XXXXXX)"
FINDINGS="$ROOT/mesh-soak-findings.jsonl"; : > "$FINDINGS"
DECAY_WAIT="${DECAY_WAIT:-72}"     # offline_threshold is 60s — wait past it
RANDOM=$SEED                        # seed bash PRNG → reproducible victim picks
declare -a PIDS NODE_IDS
FAILS=0; CYCLE=0

cport() { echo $((19741 + 2 * $1)); }
iport() { echo $((19742 + 2 * $1)); }
log()   { printf '\n\033[1m# [soak] %s\033[0m\n' "$*"; }
finding() { printf '%s\n' "$1" >> "$FINDINGS"; }
jget() { curl -s -m 4 "$1" 2>/dev/null | python3 -c "import sys,json
try:
    d=json.load(sys.stdin); print(eval(sys.argv[1]))
except Exception: pass" "$2"; }

status_url() { echo "http://127.0.0.1:$(cport $1)/v1/mesh/status"; }

boot_node() {  # boot_node <i>
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
$YIELD_TOML
[data]
dir = "$d"
[iroh]
# Pinned explicitly: soak nodes join via /v1/mesh/join, which writes the
# client-exposed marker — without this pin, auto-enable would silently point
# every soak node at public relay infrastructure. --iroh / SOAK_IROH is the
# transport-migration soak axis (see TRANSPORT_MIGRATION.md W3).
enabled = $IROH_TOML
EOF
  # Under the iroh axis, raise the `transport` tracing target to debug so each
  # node's log carries the per-dial `transport: resolved` lines (target:
  # "transport") the assertion greps for — proof iroh actually carried traffic,
  # not just that the endpoint bound. RUST_LOG is honoured by init_tracing's
  # EnvFilter. Left unset otherwise so the crash lane's log volume is unchanged.
  # `sovereign_mesh=info` surfaces the "routing classes over iroh" install
  # line (its target is the sovereign_mesh module, not `transport`);
  # `transport=debug` surfaces the per-dial "transport: resolved" lines.
  # Both targets are needed — an EnvFilter directive for one leaves the other
  # OFF, which is exactly what silently zeroed the install check on the first
  # smoke run.
  local rust_log=""
  [ "$IROH_ON" = 1 ] && rust_log="RUST_LOG=sovereign_cli_daemon=info,sovereign_mesh=info,transport=debug"
  env $rust_log "$CLI" daemon run --config "$d/config.toml" > "$d/daemon.$RANDOM.log" 2>&1 &
  PIDS[$i]=$!
}
wait_port() { local i="$1" _; for _ in $(seq 1 40); do
  curl -s -m 2 -o /dev/null "$(status_url $i)" 2>/dev/null && return 0
  kill -0 "${PIDS[$i]}" 2>/dev/null || return 1; sleep 0.5; done; return 1; }

# kill-9-startup-window torture: boot the node, then kill -9 it again WHILE it is
# still inside its startup window (before wait_port would succeed), then boot it
# clean. The daemon must persist its node_id synchronously early enough that the
# clean restart resumes the SAME identity — a regression net for the startup-
# window identity durability the whole restart-identity investigation hinged on.
# Stability is asserted by the following healed checkpoint (UniqueIds + the
# unchanged self_id). If a future daemon change defers node_id persistence past
# the bind, this fault will start flapping UniqueIds.
torture_restart() {  # torture_restart <i>
  local v="$1"
  boot_node "$v"                     # first boot
  sleep "0.$(( (RANDOM % 8) + 1 ))"  # 0.1–0.8s — land inside the startup window
  kill -9 "${PIDS[$v]}" 2>/dev/null  # kill mid-startup
  finding "{\"kind\":\"fault\",\"action\":\"kill-9-startup-window\",\"node\":$v,\"cycle\":${CYCLE:-0}}"
  boot_node "$v"                     # clean restart — must resume the same id
}

join_to_founder() {  # join_to_founder <i> <founder_key_or_link>
  local i="$1" key_or_link="$2" url
  # Under the iroh axis the caller passes the founder's FULL dial-bearing
  # invite link (`sovereign://join/<key>?...&dial=<hex>@127.0.0.1:<udp>`) read
  # live from node0's status — so the joiner dials the founder BY KEY over
  # iroh (the `dial=` plaintext path, W2c), IP/mDNS fallback intact. Otherwise
  # the legacy hand-built `?relay=127.0.0.1` IP hint.
  if [ "$IROH_ON" = 1 ]; then
    url="$key_or_link"
  else
    url="sovereign://join/${key_or_link}?relay=127.0.0.1:$(iport 0)"
  fi
  local body; body=$(python3 -c 'import json,sys; print(json.dumps({"key_or_url": sys.argv[1], "node_name": "node"+sys.argv[2]}))' "$url" "$i")
  curl -s -m 25 -X POST "http://127.0.0.1:$(cport $i)/v1/mesh/join" \
    -H 'content-type: application/json' -d "$body" >/dev/null 2>&1
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

# Iroh axis (--iroh / SOAK_IROH): prove each node actually routed mesh traffic
# over iroh — not merely that the endpoint bound. Two signals per node log:
#   1. install  (info): "routing classes over iroh" — the RoutedTransport with
#      iroh in the per-class map was installed at start_daemon.
#   2. carried  (debug, `transport` target, hence RUST_LOG=transport=debug in
#      boot_node): a "transport: resolved" line naming iroh — a real dial
#      candidate was produced over iroh, and since iroh candidates are listed
#      FIRST and loopback is reachable, that request rode iroh.
# A node missing either signal fails the run. Findings stream to the verdict.
# Called once after initial convergence — by then ≥1 gossip round has run, so
# every node has both dialed a peer and been dialed over iroh.
# Per-node log predicates. `install` is emitted at startup (immediate);
# `carried` needs a gossip round in each direction (the founder only dials a
# joiner over iroh once it has merged that joiner's self-stamped dial info),
# so the caller POLLS for it rather than asserting eagerly.
iroh_installed() { grep -qs "routing classes over iroh" "$WORK/node$1"/daemon.*.log; }
iroh_carried()   { grep -hs "transport: resolved" "$WORK/node$1"/daemon.*.log 2>/dev/null | grep -q iroh; }

assert_iroh_carried_traffic() {
  [ "$IROH_ON" = 1 ] || return 0
  log "iroh axis — asserting each node routed mesh traffic over iroh"
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
  # H2 observability: node0's /v1/mesh/status must expose iroh_transport with a
  # real path (direct/relayed/mixed) for its peers — the operator surface, on a
  # live daemon. In-netns peers are loopback ⇒ expect "direct".
  local paths
  paths=$(jget "$(status_url 0)" '",".join(p.get("path",{}).get("path","?") for p in d.get("iroh_transport",[]))')
  if [ -n "$paths" ]; then
    echo "  node0 iroh_transport paths: $paths"
    case "$paths" in
      *direct*|*relayed*|*mixed*) finding '{"kind":"iroh","check":"status_surface","node":0,"ok":true}';;
      *) echo "  node0: iroh_transport present but no active path ✗"; FAILS=$((FAILS+1)); finding '{"kind":"iroh","check":"status_surface","node":0,"ok":false}';;
    esac
  else
    echo "  node0: /v1/mesh/status exposed no iroh_transport ✗"
    FAILS=$((FAILS+1)); finding '{"kind":"iroh","check":"status_surface","node":0,"ok":false}'
  fi
}

# Forensic capture — dump the DURABLE identity state + daemon identity events at
# the moment of a violation, and copy node_id/mesh.json/logs into a stable bundle
# so the issue can be re-inspected (and replayed) offline without re-running the
# whole soak. This is what makes an intermittent failure efficient to root-cause:
# a UniqueIds/no_ghost hit tells you WHICH id collided; the bundle tells you which
# durable field (node_id file vs mesh.json self_node_id) carries the wrong id and
# what the daemon logged when it adopted it.
REPRO_DIR="$ROOT/mesh-soak-repro"
fhex() { python3 -c "
try: print(open('$WORK/node$1/node_id','rb').read().hex())
except Exception: print('NO-FILE')" 2>/dev/null; }
mhex() { python3 -c "import json
try:
    d=json.load(open('$WORK/node$1/mesh.json')); b=d.get('self_node_id'); print(bytes(b).hex() if isinstance(b,list) else str(b))
except Exception: print('NO-MESH')" 2>/dev/null; }
capture_forensics() {  # capture_forensics <label>
  local label="$1" i
  local cyc="${CYCLE:-0}"
  local bundle="$REPRO_DIR/seed${SEED}-cycle${cyc}-${label}"
  mkdir -p "$bundle"
  {
    echo "# mesh-soak forensics — seed=$SEED cycle=$cyc phase=$label nodes=$NODES backend=$BACKEND"
    echo "# durable identity state at the violation (live id vs node_id file vs mesh.json self):"
    for i in $(seq 0 $((NODES-1))); do
      printf '  node%s  live=%-32s  node_id_file=%-32s  mesh.json_self=%s\n' \
        "$i" "$(self_id $i 2>/dev/null || echo DEAD)" "$(fhex $i)" "$(mhex $i)"
    done
    echo "# harness expect-live tracking (a healthy node missing here = a FALSE ghost):"
    echo "  NODE_IDS[]=${NODE_IDS[*]:-<unset>}"
    echo "  ALL_IDS=${ALL_IDS:-<unset>}"
    echo "# daemon identity events (per node):"
    for i in $(seq 0 $((NODES-1))); do echo "  node$i:"
      grep -hE 'generated . persisted|resumed mesh|joined mesh|handshake_accepted|assigned_node_id' \
        "$WORK/node$i"/daemon.*.log 2>/dev/null | tail -6 | sed 's/^/    /'; done
  } | tee "$bundle/forensics.txt"
  for i in $(seq 0 $((NODES-1))); do local nd="$bundle/node$i"; mkdir -p "$nd"
    cp "$WORK/node$i/node_id" "$WORK/node$i/mesh.json" "$nd/" 2>/dev/null
    cp "$WORK/node$i"/daemon.*.log "$nd/" 2>/dev/null; done
  echo "  ↳ forensic bundle: $bundle"
  finding "{\"kind\":\"forensics\",\"phase\":\"$label\",\"cycle\":$cyc,\"bundle\":\"$bundle\"}"
}

check() {  # check <label> <nodes-csv> <expect-live-csv> ; appends a finding, bumps FAILS
  local label="$1" nodes="$2" live="$3" out rc
  out=$("$CLI" mesh check-invariants --nodes "$nodes" --expect-live "$live" 2>&1); rc=$?
  printf '%s\n' "$out"                       # echo for live visibility
  if [ "$rc" = 0 ]; then
    finding "{\"phase\":\"$label\",\"ok\":true,\"violations\":[]}"
  else
    # embed the ACTUAL violation text in the finding (not a 'see-stderr' stub)
    finding "{\"phase\":\"$label\",\"ok\":false,\"detail\":$(printf '%s' "$out" | python3 -c 'import sys,json;print(json.dumps(sys.stdin.read()))')}"
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
  local canonical="$ROOT/sovereign-recipes/chaos-secret-agent/recipe.toml"
  local override="$HOME/.sovereign/recipes/chaos-secret-agent/recipe.toml"
  local src="$HOME/.sovereign/bench-corpora/chaos-secret-agent/secret-agent.txt"
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
  local ing_seen=0 ing_done=0 prog_changes=0 prev_prog="∅"
  local chat_ok=0 chat_slow=0 chat_fail=0
  while [ "$(date +%s)" -lt "$DEADLINE" ]; do
    local res code ms ing prog; res=$(chat_once "$target" "$SLO_MS"); code="${res%% *}"; ms="${res##* }"
    ing=$(jget "$(status_url "$target")" 'd.get("active_corpus_ingests",0)'); ing="${ing:-0}"
    # Forward-progress signal: the per-corpus IngestProgress phase/percent. A
    # CHANGING value across polls is forward progress even while active stays 1
    # (ingest correctly throttled by — not starved by — foreground chat).
    prog=$(jget "$purl" 'json.dumps(d.get("progress",{}).get("chaos-secret-agent"))'); prog="${prog:-null}"
    { [ "$ing" -gt 0 ] || [ "$prog" != "null" ]; } && ing_seen=1
    [ "$prog" != "null" ] && [ "$prog" != "$prev_prog" ] && prog_changes=$((prog_changes+1))
    prev_prog="$prog"
    [ "$ing_seen" = 1 ] && [ "$ing" = 0 ] && [ "$prog" = "null" ] && ing_done=1
    case "$code" in
      200) [ "$ms" -le "$SLO_MS" ] && chat_ok=$((chat_ok+1)) || chat_slow=$((chat_slow+1));;
      *)   chat_fail=$((chat_fail+1));;
    esac
    finding "{\"kind\":\"contention\",\"node\":$target,\"active_ingests\":$ing,\"prog_changes\":$prog_changes,\"chat_code\":\"$code\",\"chat_ms\":$ms}"
    echo "  ingest=$ing prog_advances=$prog_changes chat=$code ${ms}ms (ok=$chat_ok slow=$chat_slow fail=$chat_fail)"
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
  # Hard-assert once the ingest TASK is idle (active_corpus_ingests==0) — the true
  # completion signal. NB the lane's ing_done ALSO requires the per-corpus progress
  # entry to go null, but the daemon leaves a terminal (non-null) progress record
  # after a completed ingest, so ing_done under-reports completion (a finished,
  # fully-queryable corpus never latches it). active==0 is the robust signal, and
  # keying the hard gate on it makes "ingest finished but corpus NOT queryable" a
  # real, catchable failure. Still-active (ing>0) at loop exit ⇒ soft: the index is
  # legitimately partial (chat throttled it), so don't false-fail on it.
  if [ "$ing_seen" = 1 ]; then
    if [ "${ing:-1}" = 0 ]; then
      log "grounding check (ingest idle — authoritative) on node$target"
      grounding_verdict "$target" hard
    else
      log "grounding probe (ingest still active — soft) on node$target"
      grounding_verdict "$target" soft
    fi
  fi
}

# ── corrupt-persisted-state OS-fault (--workload corrupt) ─────────────────────
# An OS-level fault that needs NO container: pre-write garbage into a node's
# durable mesh.json (the file `try_resume` loads on restart), then resume. Two
# distinct properties: (1) the daemon FAILS-SAFE on resume — identity survives in
# the separate node_id file, so it never adopts a colliding/garbage id; (2) the
# corrupt mesh.json wiped its MEMBERSHIP, and in the netns (no mDNS) it can't
# re-discover peers on its own, so catastrophic state loss must be repaired by a
# RE-JOIN (the path mDNS gives in a real deployment) — after which the mesh
# reconverges. UniqueIds + NoGhost + convergence are the net. (The crash lane
# bare-resumes because its mesh.json is intact; only the corrupt lane re-joins.
# cgroup-OOM and disk-full are the other OS-faults in this tier; see MESH_QA.md —
# all rootless, no podman, per the toolbox decision.)
run_corrupt_state_workload() {
  local victim=$(( NODES > 1 ? 1 : 0 ))
  local mj="$WORK/node$victim/mesh.json"
  log "corrupt-persisted-state on node$victim — kill, corrupt mesh.json, resume + re-join (fail-safe id + re-discover membership)"
  kill -9 "${PIDS[$victim]}" 2>/dev/null
  finding "{\"kind\":\"fault\",\"action\":\"kill-9\",\"node\":$victim,\"cycle\":0}"
  echo "  waiting ${DECAY_WAIT}s for offline-decay…"; sleep "$DECAY_WAIT"
  echo "  corrupting durable state: $mj"
  printf '{ this is not valid mesh json :: %s' "$RANDOM" > "$mj"
  finding "{\"kind\":\"fault\",\"action\":\"corrupt-mesh-json\",\"node\":$victim}"
  boot_node "$victim"                                   # resume: identity survives (node_id file), membership is gone
  if wait_port "$victim"; then
    finding "{\"phase\":\"corrupt-state-recover\",\"ok\":true,\"detail\":\"node$victim bound after corrupt mesh.json (identity intact)\"}"
    echo "  ✓ node$victim recovered from corrupt mesh.json (identity intact)"
    # The corruption wiped node$victim's member table; in the netns (no mDNS) it
    # can't re-discover peers on its own, so re-join the founder. Bare resume
    # leaves it isolated — the real finding the full-decay sweep surfaced.
    log "node$victim re-joining founder to re-discover peers (membership lost to corruption)"
    join_to_founder "$victim" "$FKEY"
    finding "{\"kind\":\"fault\",\"action\":\"corrupt-rejoin\",\"node\":$victim}"
  else
    FAILS=$((FAILS+1)); capture_forensics "corrupt-state-recover"
    finding "{\"phase\":\"corrupt-state-recover\",\"ok\":false,\"detail\":\"node$victim failed to bind after corrupt mesh.json (crash-loop?)\"}"
    echo "  ✗ node$victim did NOT recover from corrupt state"
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
  mkdir -p "$home/.sovereign" "$WORK/desktop$i/data"
  cat > "$home/.sovereign/config.toml" <<EOF
[models]
primary = "$PRIMARY_MODEL"
embed = "$EMBED_MODEL"
context_size = 4096
[daemon]
client_port = $(cport "$i")
internal_port = $(iport "$i")
client_bind = "127.0.0.1"
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
    kill -9 "${PIDS[$i]:-0}" 2>/dev/null
  done
  [ "$KEEP" = 0 ] && rm -rf "$WORK"; }
trap teardown EXIT

# ── bring up the mesh ─────────────────────────────────────────────────────────
log "backend=$BACKEND workload=$WORKLOAD nodes=$NODES minutes=$MINUTES seed=$SEED primary=$(basename "$PRIMARY_MODEL") embed=$(basename "$EMBED_MODEL")"
for i in $(seq 0 $((NODES-1))); do boot_node "$i"; done
for i in $(seq 0 $((NODES-1))); do wait_port "$i" && echo "  node$i up" || echo "  node$i FAILED to bind"; done

FKEY=$(jget "$(status_url 0)" 'd["join_key"]')
if [ "$IROH_ON" = 1 ]; then
  # Read the founder's dial-bearing invite live — current_invite stamps the
  # `dial=` connect code once the endpoint has an address (direct addrs are
  # immediate in-netns; no relay to wait for). Retry until it carries `dial=`.
  FLINK=""
  for _ in $(seq 1 20); do
    FLINK=$(jget "$(status_url 0)" 'd.get("join_link","")')
    case "$FLINK" in *dial=*) break;; esac
    sleep 0.5
  done
  case "$FLINK" in
    *dial=*) log "founder key=$FKEY — joining $((NODES-1)) peers over iroh (dial-by-key)";;
    *) log "founder key=$FKEY — WARNING: node0 invite carries no dial= yet; joining may fall back to IP"; FAILS=$((FAILS+1)); finding '{"kind":"iroh","check":"founder_dial_in_invite","ok":false}';;
  esac
  for i in $(seq 1 $((NODES-1))); do join_to_founder "$i" "$FLINK"; done
else
  log "founder key=$FKEY — joining $((NODES-1)) peers over localhost relay"
  for i in $(seq 1 $((NODES-1))); do join_to_founder "$i" "$FKEY"; done
fi

log "waiting for convergence to $NODES members"
for _ in $(seq 1 45); do conv=1
  for i in $(seq 0 $((NODES-1))); do [ "$(jget "$(status_url $i)" 'd["members_total"]')" = "$NODES" ] || conv=0; done
  [ "$conv" = 1 ] && break; sleep 1; done
for i in $(seq 0 $((NODES-1))); do NODE_IDS[$i]=$(robust_self_id "$i"); done
ALL_NODES=$(for i in $(seq 0 $((NODES-1))); do printf '127.0.0.1:%s,' "$(cport $i)"; done | sed 's/,$//')
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
else
DEADLINE=$(( $(date +%s) + MINUTES*60 )); CYCLE=0
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  CYCLE=$((CYCLE+1))
  # With desktops attached, ROTATE the victim so every non-founder user gets
  # disrupted over the run (a random seed can otherwise hit the same node every
  # cycle — seed=1 picked node2 all 3 times, leaving node1's user untested).
  # Plain crash lane keeps the seeded-random pick for victim-choice fuzzing.
  if [ "$DESKTOPS" = 1 ]; then victim=$(( (CYCLE-1) % (NODES-1) + 1 )); else victim=$(( RANDOM % (NODES-1) + 1 )); fi
  log "cycle $CYCLE — crash node$victim (kill -9)"
  kill -9 "${PIDS[$victim]}" 2>/dev/null
  finding "{\"kind\":\"fault\",\"action\":\"kill-9\",\"node\":$victim,\"cycle\":$CYCLE}"

  # P3 cross-layer assertion — node$victim's daemon is DOWN: its user's turn must
  # fail FAST (graceful error), not hang on the dead daemon. (Runs ≤30s inside the
  # decay window below.)
  [ "$DESKTOPS" = 1 ] && probe_user "$victim" "app-outage-graceful" "fail-fast" 30

  echo "  waiting ${DECAY_WAIT}s for offline-decay…"; sleep "$DECAY_WAIT"
  # survivors must now show the victim OFFLINE (decayed, not a live ghost)
  surv_nodes=""; surv_ids=""
  for i in $(seq 0 $((NODES-1))); do [ "$i" = "$victim" ] && continue
    surv_nodes+="127.0.0.1:$(cport $i),"; surv_ids+="${NODE_IDS[$i]},"; done
  surv_nodes="${surv_nodes%,}"; surv_ids="${surv_ids%,}"
  echo "  node0 sees node$victim as: $(sees_status 0 "${NODE_IDS[$victim]}")"
  wait_online_eq "$((NODES-1))" "$victim" || true   # all survivors must see the victim decayed
  check "post-crash-decay" "$surv_nodes" "$surv_ids"

  # churn: restart — a production restart RESUMES its identity + mesh from its
  # data_dir (try_resume loads mesh.json) and gossip-reconverges to online. We
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

  # load: timed /v1/mesh/status queries (latency SLIs for the gate)
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
    if "phase" in d:
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
if workload == "ingest":
    print("  live this lane  : admission_safety + bounded_fan_out (chat fan-out drives")
    print("                    peer_inflight / fanout_inflight > 0) + IngestProgress +")
    print("                    ForegroundLiveness + GroundingIntegrity/GroundingVerdict")
    print("                    (real generative primary under ingest; grounded RAG turn).")
else:
    print("  inert here      : admission_safety + bounded_fan_out + shared_model_single_host")
    print("                    (cheap embed-only daemons take no peer-inference / run no")
    print("                    shared model) — exercised by --workload ingest + the DST suite.")
PY
if [ "$IROH_ON" = 1 ]; then
  # grep -c prints "0" AND exits 1 on no-match — a trailing `|| echo 0` would
  # double it. Take grep's own count, default empty (missing file) to 0.
  ok=$(grep -c '"kind":"iroh".*"ok":true' "$FINDINGS" 2>/dev/null); ok=${ok:-0}
  bad=$(grep -c '"kind":"iroh".*"ok":false' "$FINDINGS" 2>/dev/null); bad=${bad:-0}
  echo "  ── iroh axis ────────────────────────────────────────────"
  echo "  transport       : iroh-first, all classes (IP fallback retained)"
  echo "  join path       : dial-by-key over the founder's dial= invite"
  echo "  iroh checks     : ${ok} ok / ${bad} failed (install + carried-over-iroh per node)"
fi
if [ "$GATE" = 1 ]; then
  log "SLO gate"
  "$CLI" mesh soak-gate "$FINDINGS" --baseline "$ROOT/mesh-soak-baseline.json" || true
fi
[ "$FAILS" = 0 ] && { echo "  PASS ✓ — invariants held across all checkpoints"; exit 0; } \
                 || { echo "  FAIL ✗ — $FAILS checkpoint(s) violated"; exit 1; }
