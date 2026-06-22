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
while [ $# -gt 0 ]; do
  case "$1" in
    --nodes)    NODES="$2"; shift 2;;
    --minutes)  MINUTES="$2"; shift 2;;
    --seed)     SEED="$2"; shift 2;;
    --workload) WORKLOAD="$2"; shift 2;;
    --keep)     KEEP=1; shift;;
    --gate)     GATE=1; shift;;
    -h|--help)  sed -n '3,44p' "$0"; exit 0;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done
case "$WORKLOAD" in crash|ingest|corrupt) ;; *) echo "bad --workload: $WORKLOAD (crash|ingest|corrupt)" >&2; exit 2;; esac

# ── Re-exec into a fresh rootless netns (loopback up) for the local backend ────
if [ "$BACKEND" = "local" ] && [ -z "${MESH_SOAK_IN_NETNS:-}" ]; then
  exec unshare -rn env MESH_SOAK_IN_NETNS=1 \
    NODES="$NODES" MINUTES="$MINUTES" SEED="$SEED" KEEP="$KEEP" GATE="$GATE" \
    MESH_SOAK_BACKEND="$BACKEND" WORKLOAD="$WORKLOAD" bash "$0"
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
if [ "$WORKLOAD" = "ingest" ]; then
  PRIMARY_MODEL="${MESH_SOAK_MODEL:-$ROOT/models/Qwen3.5-2B.Q6_K.gguf}"
  EMBED_MODEL="${MESH_SOAK_EMBED_MODEL:-$EMBED_DEFAULT}"
  YIELD_SECS="${MESH_SOAK_YIELD_SECS:-5}"
  case "$YIELD_SECS" in ''|*[!0-9]*) echo "MESH_SOAK_YIELD_SECS must be an integer"; exit 2;; esac
  [ "$YIELD_SECS" -lt 30 ] || { echo "yield_to_foreground_secs=$YIELD_SECS must be < 30 (else ingest starves)"; exit 2; }
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
EOF
  "$CLI" daemon run --config "$d/config.toml" > "$d/daemon.$RANDOM.log" 2>&1 &
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

join_to_founder() {  # join_to_founder <i> <founder_key>
  local i="$1" key="$2"
  local body; body=$(printf '{"key_or_url":"sovereign://join/%s?relay=127.0.0.1:%s","node_name":"node%s"}' \
    "$key" "$(iport 0)" "$i")
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

teardown() { log "teardown"; for i in $(seq 0 $((NODES-1))); do kill -9 "${PIDS[$i]:-0}" 2>/dev/null; done
  [ "$KEEP" = 0 ] && rm -rf "$WORK"; }
trap teardown EXIT

# ── bring up the mesh ─────────────────────────────────────────────────────────
log "backend=$BACKEND workload=$WORKLOAD nodes=$NODES minutes=$MINUTES seed=$SEED primary=$(basename "$PRIMARY_MODEL") embed=$(basename "$EMBED_MODEL")"
for i in $(seq 0 $((NODES-1))); do boot_node "$i"; done
for i in $(seq 0 $((NODES-1))); do wait_port "$i" && echo "  node$i up" || echo "  node$i FAILED to bind"; done

FKEY=$(jget "$(status_url 0)" 'd["join_key"]')
log "founder key=$FKEY — joining $((NODES-1)) peers over localhost relay"
for i in $(seq 1 $((NODES-1))); do join_to_founder "$i" "$FKEY"; done

log "waiting for convergence to $NODES members"
for _ in $(seq 1 45); do conv=1
  for i in $(seq 0 $((NODES-1))); do [ "$(jget "$(status_url $i)" 'd["members_total"]')" = "$NODES" ] || conv=0; done
  [ "$conv" = 1 ] && break; sleep 1; done
for i in $(seq 0 $((NODES-1))); do NODE_IDS[$i]=$(robust_self_id "$i"); done
ALL_NODES=$(for i in $(seq 0 $((NODES-1))); do printf '127.0.0.1:%s,' "$(cport $i)"; done | sed 's/,$//')
ALL_IDS=$(IFS=,; echo "${NODE_IDS[*]}")
echo "  converged: node0 online=$(online_count 0)/$NODES"
check "healthy" "$ALL_NODES" "$ALL_IDS"

# ── workload: ingest×inference contention, or repeated crash/churn cycles ─────
if [ "$WORKLOAD" = "ingest" ]; then
  run_ingest_workload
elif [ "$WORKLOAD" = "corrupt" ]; then
  run_corrupt_state_workload
else
DEADLINE=$(( $(date +%s) + MINUTES*60 )); CYCLE=0
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  CYCLE=$((CYCLE+1))
  victim=$(( RANDOM % (NODES-1) + 1 ))         # never the founder (node0)
  log "cycle $CYCLE — crash node$victim (kill -9)"
  kill -9 "${PIDS[$victim]}" 2>/dev/null
  finding "{\"kind\":\"fault\",\"action\":\"kill-9\",\"node\":$victim,\"cycle\":$CYCLE}"

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

  # load: timed /v1/mesh/status queries (latency SLIs for the gate)
  for _ in $(seq 1 20); do
    ms=$(curl -s -m 4 -o /dev/null -w '%{time_total}' "$(status_url 0)" 2>/dev/null)
    finding "{\"kind\":\"load\",\"latency_ms\":$(python3 -c "print(round(float('${ms:-0}')*1000,2))" 2>/dev/null || echo 0),\"ok\":$([ -n "$ms" ] && echo true || echo false)}"
  done
done
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
    print("                    ForegroundLiveness (real generative primary under ingest).")
else:
    print("  inert here      : admission_safety + bounded_fan_out + shared_model_single_host")
    print("                    (cheap embed-only daemons take no peer-inference / run no")
    print("                    shared model) — exercised by --workload ingest + the DST suite.")
PY
if [ "$GATE" = 1 ]; then
  log "SLO gate"
  "$CLI" mesh soak-gate "$FINDINGS" --baseline "$ROOT/mesh-soak-baseline.json" || true
fi
[ "$FAILS" = 0 ] && { echo "  PASS ✓ — invariants held across all checkpoints"; exit 0; } \
                 || { echo "  FAIL ✗ — $FAILS checkpoint(s) violated"; exit 1; }
