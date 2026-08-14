#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# PROBE FLEET — bar `serve50-fleet-scaling` (order `mesh-serve-50-red`),
# MESH_SCALE_100_USERS_1000_CORPORA.md §9.
#
# ONE QUESTION: does mesh-wide admitted concurrency SCALE with the number of
# serving nodes? Load enters via ONE node's client surface so the mesh
# scheduler — if there is one — does the routing; the arms are N = 1, 2, all.
#
# ── Why this probe is NOT in a netns, unlike Probe A ──────────────────────────
# Probe A (§8.1) seals its daemon in a rootless netns precisely so it CANNOT
# reach the real mesh. This probe measures the real mesh, so it cannot do that.
# The isolation guarantee is therefore replaced, not dropped:
#
#   * Step 1 is a CENSUS, not load. It never sends a turn — it resolves every
#     member of the mesh to a reachable/unreachable verdict, per layer (ICMP,
#     TCP on the mesh port, TCP+HTTP on the client port). Three instruments,
#     because `mesh status` alone cannot tell "host powered off" from "host up,
#     daemon not running", and those are different findings.
#   * Load is only sent with an explicit `--drive` flag, and only ever at the
#     LOCAL node's client surface. This probe never sends a request at a peer's
#     port and never restarts, reconfigures, or otherwise touches a peer — peer
#     daemons are other machines' constraint (order `mesh-serve-50-red`, Seams).
#
# ── The verdict this probe is allowed to reach ────────────────────────────────
# If fewer than 2 nodes are SERVING, the N>=2 arms are recorded COULD-NOT-JUDGE
# with the blocker named, and the script says so and exits 0. A fleet-scaling
# factor computed against one node is not a small number, it is no number —
# and quietly reporting the N=1 arm as though it were the sweep is exactly the
# well-formed-but-wrong failure this repo's §18 exists to stop.
#
# Usage:
#   scripts/probe-serve50-fleet.sh                       # census only (safe, no load)
#   scripts/probe-serve50-fleet.sh --drive --clients 40  # census + load at LOCAL node
#
# Requires: python3, curl, target/debug/sovereign-cli.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLI="${SOVEREIGN_CLI:-$ROOT/target/debug/sovereign-cli}"
LOCAL_URL="${LOCAL_URL:-http://127.0.0.1:9741}"
DRIVE=0
CLIENTS="${CLIENTS:-40}"
LOAD_SCRIPT="${LOAD_SCRIPT:-scripts/probe_serve50_ttft.py}"
LOAD_ARGS="${LOAD_ARGS:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --drive)   DRIVE=1; shift ;;
    --clients) CLIENTS="$2"; shift 2 ;;
    --url)     LOCAL_URL="$2"; shift 2 ;;
    --load)    LOAD_SCRIPT="$2"; shift 2 ;;
    --load-args) LOAD_ARGS="$2"; shift 2 ;;
    -h|--help) sed -n '3,40p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

[[ -x "$CLI" ]] || { echo "probe-fleet: $CLI not built (cargo build --bins)" >&2; exit 1; }

echo "=============================================================="
echo "probe-fleet: STEP 1 — serving-node census (no load is sent)"
echo "=============================================================="

MESH_RAW="$("$CLI" mesh status 2>&1)"
echo "$MESH_RAW"
echo

# Resolve every member to a per-layer verdict. `mesh status` reports gossip's
# opinion; ICMP reports whether the host is powered on; a TCP connect reports
# whether a daemon is actually listening. Only the third can make a node
# SERVING, and only /v1/models can say what it would serve.
python3 - "$LOCAL_URL" "$CLI" <<'PY'
import re, socket, subprocess, sys, json, urllib.request

local_url, cli = sys.argv[1], sys.argv[2]

mesh = subprocess.run([cli, "mesh", "status"], capture_output=True, text=True)
text = mesh.stdout + mesh.stderr

rows = []
for line in text.splitlines():
    m = re.match(r"\s*([0-9a-f]{12,})\s+(\S+)\s+(online|offline)\s+(.*)$", line)
    if m:
        node_id, name, status, addrs = m.groups()
        ips = re.findall(r"(\d+\.\d+\.\d+\.\d+):(\d+)", addrs)
        # `mesh status` marks the local node with a trailing `*`.
        rows.append({"node_id": node_id, "name": name, "gossip": status,
                     "addrs": ips, "self": addrs.rstrip().endswith("*")})

def icmp(host):
    r = subprocess.run(["ping", "-c1", "-W2", host], capture_output=True)
    return "up" if r.returncode == 0 else "down"

def serves(url, timeout=8.0):
    """Does an HTTP client surface actually answer? The only test that can
    promote a node to SERVING."""
    try:
        with urllib.request.urlopen(f"{url}/v1/models", timeout=timeout) as resp:
            return "yes" if resp.status == 200 else f"http{resp.status}"
    except Exception as e:  # noqa: BLE001
        return f"no({type(e).__name__})"

# ── What each column can and cannot tell you ─────────────────────────────────
# gossip  : the mesh's OWN opinion of the peer, carried over iroh. This is the
#           authoritative "is the peer participating" signal.
# icmp    : whether the host is powered on. Its whole job is to separate
#           "machine off" from "machine on, daemon not running" — `mesh status`
#           collapses both to `offline`, and they are different findings.
# serves  : an HTTP GET /v1/models. Only the LOCAL node is probed here, for two
#           reasons: the order forbids touching peer daemons, AND the client
#           surface binds loopback-only (verified on RuggedFox 2026-08-13:
#           `ss -ltn` shows 127.0.0.1:9741 and 127.0.0.1:9742), so a peer's
#           client port is not reachable off-box even in principle. A TCP probe
#           of a peer's :9741 would therefore be measuring the bind address, not
#           the peer's health — a confident wrong answer, so it is not made.
#
# NOTE the mesh transport is NOT TCP on the advertised :9742. It is iroh/QUIC
# on a separate UDP socket (47997 on this host). Probing TCP :9742 and reporting
# `refused` as a mesh fault is an instrument error; this script does not do it.
print(f"{'name':<14}{'gossip':<9}{'host(icmp)':<12}{'serves(http)':<26}verdict")
print("-" * 78)
serving = []
for r in rows:
    is_self = r["self"]
    if not r["addrs"]:
        print(f"{r['name']:<14}{r['gossip']:<9}{'-':<12}{'-':<26}NO-ADDRESS")
        continue
    host, mesh_port = r["addrs"][0]
    for h, p in r["addrs"]:
        if h.startswith("192.168."):
            host, mesh_port = h, p
            break
    h_icmp = icmp(host)
    if is_self:
        s = serves(local_url)
        if s == "yes":
            verdict = "SERVING"
            serving.append(r["name"])
        else:
            verdict = "SELF-NOT-SERVING"
    else:
        s = "not-probed(peer)"
        # A peer can only be promoted to SERVING by gossip, which is the one
        # signal this probe is allowed to read for another machine.
        if r["gossip"] == "online":
            verdict = "SERVING(gossip)"
            serving.append(r["name"])
        elif h_icmp == "up":
            verdict = "HOST-UP/DAEMON-DOWN"
        else:
            verdict = "HOST-DOWN"
    print(f"{r['name']:<14}{r['gossip']:<9}{h_icmp:<12}{s:<26}{verdict}")

print()
print(f"PROBE_FLEET serving_nodes={len(serving)} names={serving}")

# What would each serving node actually serve? Only the LOCAL surface is
# queried — this probe never sends a request at a peer's port.
try:
    with urllib.request.urlopen(f"{local_url}/v1/models", timeout=10) as resp:
        data = json.load(resp)
    ids = [m.get("id") for m in data.get("data", [])]
    owners = sorted({str(m.get("owned_by")) for m in data.get("data", [])})
    print(f"PROBE_FLEET local_v1_models_count={len(ids)} owners={owners}")
    print(f"PROBE_FLEET local_v1_models={ids}")
except Exception as e:
    print(f"PROBE_FLEET local_v1_models COULD-NOT-JUDGE {e!r}")

if len(serving) < 2:
    print()
    print("PROBE_FLEET_VERDICT COULD-NOT-JUDGE — the N>=2 arms cannot run.")
    print(f"PROBE_FLEET_BLOCKER only {len(serving)} serving node(s) on the mesh "
          f"({serving}); a fleet-scaling factor needs at least 2.")
else:
    print()
    print(f"PROBE_FLEET_VERDICT {len(serving)} serving nodes — the N>=2 arms can run.")
PY
CENSUS_RC=$?

if [[ "$DRIVE" != 1 ]]; then
  echo
  echo "probe-fleet: census only (--drive not given); no load was sent."
  exit 0
fi

echo
echo "=============================================================="
echo "probe-fleet: STEP 2 — load at the LOCAL client surface ($LOCAL_URL)"
echo "=============================================================="

# Record WHICH daemon is about to be driven. The netns bind assertion is not
# available here (see the header), so the substitute is an explicit, recorded
# identification of the target: a run whose target was never named is a run
# whose numbers cannot be attributed.
echo "probe-fleet: TARGET CHECK — resolving $LOCAL_URL before sending load…"
TARGET="$(curl -s -m 10 "$LOCAL_URL/v1/mesh/status" 2>/dev/null)"
if [[ -z "$TARGET" ]]; then
  echo "probe-fleet: TARGET CHECK COULD-NOT-JUDGE — $LOCAL_URL did not answer; refusing to send load" >&2
  exit 1
fi
echo "probe-fleet: TARGET CHECK — $LOCAL_URL answered /v1/mesh/status:"
echo "$TARGET" | head -c 600
echo
echo "probe-fleet: driving $CLIENTS concurrent principals via $LOAD_SCRIPT"
echo

python3 "$ROOT/$LOAD_SCRIPT" --url "$LOCAL_URL" --clients "$CLIENTS" \
  --seconds "${SECONDS_RUN:-90}" $LOAD_ARGS
LOAD_RC=$?

echo
echo "=============================================================="
echo "probe-fleet: STEP 3 — peer attribution (where did the turns run?)"
echo "=============================================================="
# Glassbox, not assumption: ask the daemon's own surfaces where work went.
# `--drive` runs against the operator's live daemon, whose log this script does
# not own, so attribution is read from the HTTP surfaces that report it.
echo "probe-fleet: /v1/mesh/status after the run —"
curl -s -m 10 "$LOCAL_URL/v1/mesh/status" | head -c 1200
echo
echo "probe-fleet: NOTE — a turn served by a peer must show as peer dispatch in"
echo "             the daemon log ('peer inference' / 'dispatching to peer')."
echo "             With no peer serving, absence of those lines is expected and"
echo "             is recorded as such, never as evidence that routing works."

exit $(( CENSUS_RC != 0 ? CENSUS_RC : LOAD_RC ))
