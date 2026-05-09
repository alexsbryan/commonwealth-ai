#!/usr/bin/env bash
# cloud-peer-smoke.sh — validate a fresh cloud peer end-to-end.
#
# Runs in order, stopping at the first failure:
#
#   1. Pod is on the tailnet (tailscale status shows it).
#   2. Pod's daemon answers on :9741 over the tailnet.
#   3. Pod advertises the expected slots (primary / fast / embed).
#   4. Local mesh has fused the cloud peer's slots into /v1/models.
#   5. A direct chat-completion against the pod returns text.
#   6. (optional) A small enrich extract completes against the mesh.
#
# Usage:
#     ./scripts/cloud-peer-smoke.sh                # tests 1-5
#     ./scripts/cloud-peer-smoke.sh --with-extract # adds test 6
#
# Pre-reqs:
#   - Pod is up and entrypoint has finished (watch in RunPod UI Logs).
#   - Local sovereign-cli daemon is running on this laptop.
#   - tailscale, jq, curl on PATH.

set -euo pipefail

WITH_EXTRACT=0
SMOKE_SLUG="${SMOKE_SLUG:-sep-hegel}"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --with-extract) WITH_EXTRACT=1; shift ;;
        --slug)         SMOKE_SLUG="$2"; shift 2 ;;
        -h|--help)
            sed -n '2,/^set -/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//;/^set -/d'
            exit 0 ;;
        *) echo "unknown flag: $1" >&2; exit 1 ;;
    esac
done

PASS=$'\033[32m✓\033[0m'
FAIL=$'\033[31m✗\033[0m'
INFO=$'\033[34m·\033[0m'

step() { echo -e "\n== $* =="; }
ok()   { echo "  $PASS $*"; }
bad()  { echo "  $FAIL $*"; exit 1; }
note() { echo "  $INFO $*"; }

for tool in jq curl; do
    if ! command -v "$tool" >/dev/null; then
        case "$tool" in
            jq)   bad "jq not found. Install: 'sudo dnf install -y jq' (Fedora/toolbox) or 'sudo apt-get install -y jq' (Ubuntu)" ;;
            curl) bad "curl not found. Install: 'sudo dnf install -y curl' or 'sudo apt-get install -y curl'" ;;
        esac
    fi
done

# tailscale CLI lookup. From a toolbox, the host's tailscaled.sock
# isn't bind-mounted at the default path, but kyuz0 toolboxes expose
# the host filesystem at /run/host/, so the host's socket is reachable
# at /run/host/var/run/tailscale/tailscaled.sock. We auto-detect and
# pass --socket=<path> to whichever tailscale CLI is installed (in
# the toolbox or on the host).
#
# Escape hatch: if neither path works, set POD_TS_IP=100.x.y.z to
# skip tailscale lookup entirely.
HAVE_TAILSCALE=0
TAILSCALE_SOCKET_FLAG=()
if command -v tailscale >/dev/null; then
    HAVE_TAILSCALE=1
    if [[ -S /run/host/var/run/tailscale/tailscaled.sock ]]; then
        # We're in a toolbox; talk to the host's daemon explicitly.
        TAILSCALE_SOCKET_FLAG=(--socket=/run/host/var/run/tailscale/tailscaled.sock)
    fi
fi
ts() { tailscale "${TAILSCALE_SOCKET_FLAG[@]}" "$@"; }

# ── 1. Pod on tailnet ───────────────────────────────────────────────
step "1. Pod on tailnet"
# Priority: explicit POD_TS_IP override > tailscale CLI lookup > fail.
# The override path matters when running from a toolbox that has the
# CLI on PATH but can't reach the host's tailscaled.sock (no bind mount).
if [[ -n "${POD_TS_IP:-}" ]]; then
    note "using POD_TS_IP=$POD_TS_IP from env (skipping tailscale CLI lookup)"
    POD_TS_HOST="(unknown — POD_TS_IP override)"
    ok "pod IP: $POD_TS_IP"
elif (( HAVE_TAILSCALE )); then
    # Try tailscale status. If the CLI is installed but can't reach
    # its daemon, the call fails with a specific error we want to
    # surface clearly rather than swallow.
    if ! TS_OUT="$(ts status 2>&1)"; then
        bad "tailscale CLI present but can't reach tailscaled:
  $TS_OUT

  Either run this from the host shell, or pass the pod's IP directly:
      POD_TS_IP=100.x.y.z $0"
    fi
    TS_LINE="$(grep -E 'sovereign-' <<<"$TS_OUT" | head -1 || true)"
    if [[ -z "$TS_LINE" ]]; then
        bad "no 'sovereign-*' host in 'tailscale status'.

  Wait another minute for the pod's tailscale up to complete; if it
  still doesn't appear, check the RunPod UI Logs for tailscale errors
  (likely a bad TS_AUTHKEY or expired key)."
    fi
    POD_TS_IP="$(awk '{print $1}' <<<"$TS_LINE")"
    POD_TS_HOST="$(awk '{print $2}' <<<"$TS_LINE")"
    # Surface "offline, last seen Xm ago" status — the entry stays
    # in 'tailscale status' for a while after the pod actually went
    # away, so we want to flag that condition explicitly.
    if grep -q 'offline' <<<"$TS_LINE"; then
        bad "$POD_TS_HOST registered at $POD_TS_IP but is OFFLINE.
  Pod likely crashed and was torn down. Check RunPod UI for pod state.
  Full status line:
      $TS_LINE"
    fi
    ok "found $POD_TS_HOST at $POD_TS_IP"
else
    bad "tailscale CLI not on PATH and POD_TS_IP not set.

  Either install tailscale in this toolbox/host:
      sudo dnf install -y tailscale
  Or pass the pod's tailnet IP directly:
      POD_TS_IP=100.x.y.z $0"
fi

# ── 2. Pod's daemon reachable ───────────────────────────────────────
step "2. Pod daemon answers on :9741 (tailnet)"
if ! curl -fsS --max-time 5 "http://$POD_TS_IP:9741/v1/models" >/tmp/pod_models.json; then
    bad "GET http://$POD_TS_IP:9741/v1/models failed.

  Pod likely still loading slots. Check RunPod UI Logs for
  'launching sovereign-cli daemon'. Slot loads take 2-4 min after
  rclone sync."
fi
ok "got /v1/models from pod"

# ── 3. Pod's advertised slots ───────────────────────────────────────
step "3. Pod advertises expected slots"
POD_SLOTS="$(jq -r '.data[].id' /tmp/pod_models.json | sort -u)"
note "pod slots: $(echo "$POD_SLOTS" | tr '\n' ' ')"
for required in primary fast embed; do
    # PRIMARY_COPIES=N produces primary_0..primary_N-1; either form
    # of the primary slot ID counts as "primary present".
    if grep -q "^${required}\(_\|$\)" <<<"$POD_SLOTS"; then
        ok "$required: present"
    else
        bad "$required: missing from pod's /v1/models"
    fi
done

# ── 4. Mesh fusion — laptop sees the pod's slots ────────────────────
step "4. Mesh has fused pod slots into local /v1/models"
if ! curl -fsS --max-time 5 "http://localhost:9741/v1/models" >/tmp/local_models.json; then
    bad "local daemon (localhost:9741) not responding. Is it running?
  Start it: 'sovereign-cli daemon run' (or systemctl, however you run it)"
fi
LOCAL_SLOTS="$(jq -r '.data[].id' /tmp/local_models.json | sort -u)"
note "local-aggregated slots: $(echo "$LOCAL_SLOTS" | tr '\n' ' ')"
# Heuristic: at least one slot ID that's on the pod should also show
# up locally. With a typical setup the pod's primary will appear.
SHARED="$(comm -12 <(echo "$POD_SLOTS") <(echo "$LOCAL_SLOTS") || true)"
if [[ -z "$SHARED" ]]; then
    bad "no slot IDs in common between pod and laptop /v1/models.

  Mesh gossip from pod isn't reaching the laptop. Check:
    - Pod's MESH_SEED_ADDR matches laptop's 'tailscale ip -4'
    - Laptop's daemon has [mesh] configured and is listening on 9742
    - Tailscale ACL allows tag:cloud-peer → tag:laptop:9742"
fi
ok "mesh fused — $(echo "$SHARED" | wc -l | tr -d ' ') slot id(s) shared"

# ── 5. Direct chat completion against the pod ───────────────────────
step "5. Pod serves a chat completion"
RESP="$(curl -fsS --max-time 30 \
    -H 'content-type: application/json' \
    -d '{"model":"primary","messages":[{"role":"user","content":"Reply with exactly the word ready"}],"max_tokens":8}' \
    "http://$POD_TS_IP:9741/v1/chat/completions" \
    || echo '{}')"
TEXT="$(jq -r '.choices[0].message.content // ""' <<<"$RESP")"
if [[ -z "$TEXT" ]]; then
    bad "pod chat completion returned no content.
  raw: $(echo "$RESP" | head -c 400)"
fi
note "pod replied: $(echo "$TEXT" | tr -d '\n' | head -c 80)"
ok "chat round-trip works"

# ── 6. (optional) End-to-end enrich extract ─────────────────────────
if (( WITH_EXTRACT )); then
    step "6. Enrich extract via mesh ($SMOKE_SLUG)"
    # Repo binary is `sovereign-cli`; some setups alias it to `sovereign`.
    SOV_BIN=""
    for cand in sovereign sovereign-cli; do
        if command -v "$cand" >/dev/null 2>&1; then
            SOV_BIN="$cand"
            break
        fi
    done
    if [[ -z "$SOV_BIN" ]]; then
        bad "neither 'sovereign' nor 'sovereign-cli' on PATH; can't run enrich. Skip by omitting --with-extract."
    fi
    note "running: $SOV_BIN enrich extract $SMOKE_SLUG --full"
    note "(this routes Phase 1 to whichever peer has capacity — should be the pod)"
    if $SOV_BIN enrich extract "$SMOKE_SLUG" --full; then
        ok "extract completed"
    else
        bad "extract failed. Check the extract log + '$SOV_BIN enrich status $SMOKE_SLUG'."
    fi
fi

echo
echo "=================================================================="
echo "  Smoke OK. Cloud peer is wired up and serving."
echo "=================================================================="
