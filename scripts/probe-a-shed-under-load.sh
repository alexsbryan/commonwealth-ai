#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# PROBE A — order `mesh-scale-t0`, MESH_SCALE_100_USERS_1000_CORPORA.md §8.
#
# ONE QUESTION: *does the shed hold the line?* ~100 concurrent mixed clients
# against one node — every request must be served or 503'd fast, nothing
# parked — with the two adversaries §7 says win today in the mix:
#   • one STALLED-SSE client (opens a stream, reads nothing, never disconnects)
#   • one TIGHT-RETRY client (re-fires the instant it is shed, ignoring the hint)
# and the measured admitted concurrency checked against the architecture's
# prediction, `1 + floor(shed_window / avg_turn)`.
#
# ── Isolation (load-bearing; do not "simplify" this away) ─────────────────────
# The daemon is booted inside a ROOTLESS NETWORK NAMESPACE (`unshare -rn`,
# loopback only) — the same mechanism `scripts/mesh-soak.sh` uses, and for the
# same reason. A daemon that LOSES a port bind logs a warning and keeps
# running, so on the bare host a probe can silently drive the operator's live
# daemon and the client side cannot tell. Inside the netns, loopback is
# namespace-private: the operator's :9741 is not reachable at all, so the
# mistake is structurally impossible rather than merely avoided.
#
# Belt and braces on top of that: before ANY load is sent, the probe asserts
# that the process listening on its client port is the daemon it just started,
# and prints that assertion. A probe whose bind check never ran is a gate that
# never ran.
#
# Usage:
#   scripts/probe-a-shed-under-load.sh [--clients N] [--seconds S] [--keep]
#
# Requires: `unshare`, `ip`, python3, and target/debug/sovereign-cli.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLIENTS="${CLIENTS:-100}"
SECONDS_RUN="${SECONDS_RUN:-45}"
KEEP="${KEEP:-0}"

# `--load` swaps the load generator; `--daemon-env K=V` (repeatable) adds env
# to the DAEMON process only. Both were added by order `mesh-scale-t1-red` so
# the Tier-1 red-baseline probes reuse this script's netns + bind assertion
# instead of copying them (one netns decider, one bind check). Defaults leave
# the t0 Probe A behaviour byte-identical.
LOAD_SCRIPT="${LOAD_SCRIPT:-scripts/probe_a_load.py}"
LOAD_ARGS="${LOAD_ARGS:-}"   # extra args appended to the load generator
declare -a DAEMON_ENV=()
if [[ -n "${PROBE_A_DAEMON_ENV:-}" ]]; then
  while IFS= read -r line; do [[ -n "$line" ]] && DAEMON_ENV+=("$line"); done <<< "$PROBE_A_DAEMON_ENV"
fi

while [[ $# -gt 0 ]]; do
  case "$1" in
    --clients) CLIENTS="$2"; shift 2 ;;
    --seconds) SECONDS_RUN="$2"; shift 2 ;;
    --load)    LOAD_SCRIPT="$2"; shift 2 ;;
    --load-args) LOAD_ARGS="$2"; shift 2 ;;
    --daemon-env) DAEMON_ENV+=("$2"); shift 2 ;;
    --keep)    KEEP=1; shift ;;
    -h|--help) sed -n '3,30p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

# ── Re-exec into a sealed rootless netns ──────────────────────────────────────
if [[ -z "${PROBE_A_IN_NETNS:-}" ]]; then
  DENV_JOINED=""
  ((${#DAEMON_ENV[@]})) && DENV_JOINED="$(printf '%s\n' "${DAEMON_ENV[@]}")"
  exec unshare -rn env PROBE_A_IN_NETNS=1 CLIENTS="$CLIENTS" \
    SECONDS_RUN="$SECONDS_RUN" KEEP="$KEEP" LOAD_SCRIPT="$LOAD_SCRIPT" \
    LOAD_ARGS="$LOAD_ARGS" \
    PROBE_A_DAEMON_ENV="$DENV_JOINED" bash "$0"
fi
ip link set lo up

# `SOVEREIGN_CLI` / `PROBE_A_MODELS_DIR` let this run from a git WORKTREE, whose
# `target/` is cold and which carries no (gitignored) model files, against the
# main checkout's warm debug binary. Added by order `mesh-serve-50-red`, which
# is measurement-only and must not pay a cold worktree build to run a probe.
# Both default to the repo-relative paths, so a normal checkout is unchanged.
CLI="${SOVEREIGN_CLI:-$ROOT/target/debug/sovereign-cli}"
[[ -x "$CLI" ]] || { echo "probe-a: $CLI not built (cargo build --bins), and SOVEREIGN_CLI did not point at one" >&2; exit 1; }
MODELS_DIR="${PROBE_A_MODELS_DIR:-$ROOT/sovereign/models}"
PRIMARY="$MODELS_DIR/gemma-4-E4B-it-Q4_K_M.gguf"
EMBED="$MODELS_DIR/Qwen3-Embedding-0.6B-Q8_0.gguf"
for m in "$PRIMARY" "$EMBED"; do
  [[ -f "$m" ]] || { echo "probe-a: model not found: $m" >&2; exit 1; }
done

CPORT=19741
IPORT=19742
WORK="${TMPDIR:-/tmp}/probe-a-$$"
mkdir -p "$WORK/node0"
cleanup() {
  [[ -n "${DPID:-}" ]] && kill "$DPID" 2>/dev/null || true
  [[ "$KEEP" == 1 ]] || rm -rf "$WORK"
}
trap cleanup EXIT

# `autostart = true` so the primary is resident before the load lands (the
# warm-up turn below still pays any residual cold cost, and is excluded from
# the measurement). Watchers off: the probe measures the serving path, not a
# background sweep. Ports are namespace-private, so 19741/19742 collide with
# nothing even if the host is busy.
cat > "$WORK/node0/config.toml" <<EOF
[models]
primary = "$PRIMARY"
embed = "$EMBED"
context_size = 4096
[daemon]
client_port = $CPORT
internal_port = $IPORT
autostart = true
primary_idle_secs = 1800
extras_idle_secs = 0
freshness_watchers_enabled = false
client_bind = "127.0.0.1"
[data]
dir = "$WORK/node0"
[iroh]
enabled = false
EOF

# The stream deadline is shortened for the probe so item 5's bounded
# release is OBSERVABLE inside a 45s window instead of 300s later. Normal
# turns here run ~1.3s, so the shorter deadline cannot abort a healthy one —
# it only brings the stalled-consumer release into the measurement window.
DEADLINE_SECS="${PROBE_A_DEADLINE_SECS:-20}"
echo "probe-a: netns sealed (loopback only). booting dev daemon on :$CPORT …"
echo "probe-a: SOVEREIGN_INFERENCE_TIMEOUT_SECS=$DEADLINE_SECS (shortened so the"
echo "         stalled-consumer release lands inside the probe window)"
((${#DAEMON_ENV[@]})) && echo "probe-a: extra daemon env: ${DAEMON_ENV[*]}"
env SOVEREIGN_INFERENCE_TIMEOUT_SECS="$DEADLINE_SECS" \
  "${DAEMON_ENV[@]}" \
  "$CLI" daemon run --config "$WORK/node0/config.toml" > "$WORK/node0/daemon.log" 2>&1 &
DPID=$!

for _ in $(seq 1 240); do
  curl -s -m 2 -o /dev/null "http://127.0.0.1:$CPORT/v1/mesh/status" 2>/dev/null && break
  kill -0 "$DPID" 2>/dev/null || { echo "probe-a: daemon exited during boot; see $WORK/node0/daemon.log" >&2; tail -20 "$WORK/node0/daemon.log"; exit 1; }
  sleep 1
done

# ── BIND ASSERTION (recorded, per the seat's instruction) ─────────────────────
# Resolve the listener on the client port back to a pid and require it to be
# our daemon or one of its children. Reported as a verdict, not assumed.
BIND_OWNER="$(python3 - "$CPORT" <<'PY'
import glob, os, socket, struct, sys
port = int(sys.argv[1])
# /proc/net/tcp: find the inode listening on 127.0.0.1:<port>
inode = None
for line in open("/proc/net/tcp").read().splitlines()[1:]:
    f = line.split()
    local, state = f[1], f[3]
    if state != "0A":
        continue
    if int(local.split(":")[1], 16) == port:
        inode = f[9]
        break
if inode is None:
    print("NO_LISTENER"); raise SystemExit
for fd in glob.glob("/proc/[0-9]*/fd/*"):
    try:
        if os.readlink(fd) == f"socket:[{inode}]":
            print(fd.split("/")[2]); raise SystemExit
    except OSError:
        continue
print("UNRESOLVED")
PY
)"
echo "probe-a: BIND CHECK — listener on :$CPORT is pid $BIND_OWNER; daemon pid is $DPID"
if [[ "$BIND_OWNER" == "NO_LISTENER" || "$BIND_OWNER" == "UNRESOLVED" ]]; then
  echo "probe-a: BIND CHECK COULD-NOT-JUDGE — refusing to send load" >&2
  exit 1
fi
# The daemon may exec/fork; accept the pid itself or a descendant of it.
python3 - "$BIND_OWNER" "$DPID" <<'PY' || { echo "probe-a: BIND CHECK FAILED — the listener is not our daemon; refusing to send load" >&2; exit 1; }
import sys
pid, want = int(sys.argv[1]), int(sys.argv[2])
seen = set()
while pid > 1 and pid not in seen:
    if pid == want:
        raise SystemExit(0)
    seen.add(pid)
    try:
        stat = open(f"/proc/{pid}/stat").read()
        pid = int(stat[stat.rindex(")") + 2:].split()[1])
    except OSError:
        break
raise SystemExit(1)
PY
echo "probe-a: BIND CHECK PASSED — the load below reaches this probe's daemon and nothing else."

python3 "$ROOT/$LOAD_SCRIPT" \
  --url "http://127.0.0.1:$CPORT" --clients "$CLIENTS" --seconds "$SECONDS_RUN" \
  --daemon-log "$WORK/node0/daemon.log" $LOAD_ARGS
