#!/usr/bin/env bash
# dev-pod.sh — rent a Vast GPU and boot a sovereign daemon carrying the
# RuggedFox loadout (Qwen3.6-35B-A3B primary + Qwen3.5-4B-MTP fast), reachable
# on localhost through an SSH tunnel. Destroy it when the burst is over.
#
#   ./dev-pod.sh plan [vram-mib]        # what the loadout needs; which card holds it
#   ./dev-pod.sh offers                 # live eligible offers, cheapest first
#   ./dev-pod.sh up [--mesh] [--loadout FILE] [offer]
#                                       # rent + boot (prints the instance id)
#   ./dev-pod.sh up --dry-run ...       # render + validate the boot script, rent nothing
#   ./dev-pod.sh logs [instance-id]     # watch the boot (model pull + slot load)
#   ./dev-pod.sh tunnel [instance-id]   # forward pod :9741 -> local :9841
#   ./dev-pod.sh env                    # env exports that point a session at the pod
#   ./dev-pod.sh check                  # is the pod in the mode it was rented in?
#   ./dev-pod.sh status                 # what is billing right now, and how much so far
#   ./dev-pod.sh down [instance-id]     # leave the mesh, then destroy (stops billing)
#
# Full runbook — prerequisites, the two modes, what --mesh puts on third-party
# hardware, and what to do when it goes wrong: docs/CLOUD_PEER.md
#
# TWO MODES, AND THE INSTANCE LABEL IS WHICH. Without --mesh the pod is a SOLO
# ISLAND: it answers inference for whoever tunnels in and knows nothing about
# the operator's mesh. With --mesh it JOINS, which buys it federated retrieval
# — it can answer questions from corpora it does not hold, because a peer that
# does hold them serves the chunks (`routes_knowledge.rs`). It is still never a
# corpus of record: nothing lives only on rented hardware.
#
# The mode is recorded in the VAST LABEL, not in a local file (§10.6, one
# decider — same reason `resolve_id` reads the billing surface instead of
# /tmp). `check` and `down` read the mode back off the label, so neither can be
# run against the wrong expectation, and a `check` that contradicts the label
# EXITS NON-ZERO rather than printing a verdict nobody reads.
#
# WHAT --mesh SENDS TO A THIRD PARTY. The join link is an invite: it carries
# the mesh join key and the founder's iroh dial string, and it is written into
# the Vast onstart script, so Vast can read it. That is the trade --mesh makes.
# The blast radius is bounded — Meshsonics is `require_encryption`, so every
# peer is dialed BY KEY and a stranger holding the link can join but cannot
# read a corpus flagged `query_sharing = false`. After a mesh flight, rotating
# the invite is cheap hygiene: `svrn daemon stop && svrn mesh rotate && svrn
# daemon start` (rotate on a RUNNING daemon re-breaks — note in
# `project_mesh_rotate_clobber_and_resolver_split`). Existing members keep
# their membership; only new joins need the new key.
#
# IMAGE. Defaults to $SOVEREIGN_VAST_IMAGE. ghcr.io/alexsbryan/sovereign-cuda
# :latest was rebuilt 2026-08-28 from commit 1e7b66866 — it carries the
# vendored llama.cpp (including the four CUDA sources VENDOR_EXCLUDE used to
# omit, without which the image could not compile ggml-cuda at all) and
# openssh-server + aria2 baked into the runtime stage. The daemon logs its
# SOVEREIGN_GIT_SHA at startup, so check that against the commit you expect
# before trusting a boot. Whether the 35B-A3B LOADS on CUDA is still unproven
# — it is an MoE with an MTP head and the image's llama.cpp was only ever
# flown here with dense models, so treat the first boot as an instrument
# check (watch `logs` for the slot load) before trusting a long run to it.
set -euo pipefail

# Where this script lives, so the onstart render can read its siblings
# (pod-supervise.sh). Resolved before any cd.
SCRIPT_DIR_LOCAL="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGE="${SOVEREIGN_VAST_IMAGE:-ghcr.io/alexsbryan/sovereign-cuda:latest}"
# The instance disk. Left EMPTY here on purpose: `pod_plan` fills it from the
# loadout (weights + headroom for the image and the daemon's data dir) so the
# volume tracks the models. An explicit DISK= still wins.
#
# It must be the SAME number the offer search filters on, or the search can
# return a box with less free space than `create` then asks for.
DISK="${DISK:-}"
# Headroom over the model weights for the container image and the data dir.
POD_DISK_HEADROOM_GB="${POD_DISK_HEADROOM_GB:-35}"
LOCAL_PORT="${LOCAL_PORT:-9841}"
CTX="${CTX:-32768}"
# The label is BOTH the handle every verb resolves by AND the record of which
# mode the pod was rented in. Two labels, one prefix: `resolve_row` matches the
# prefix, so `logs`/`tunnel`/`status`/`down` keep working with no argument in
# either mode, and `pod_mode` reads the suffix back.
LABEL_PREFIX="sovereign-dev-daemon"
LABEL_SOLO="$LABEL_PREFIX"
LABEL_MESH="$LABEL_PREFIX-mesh"

# The operator's own mesh. For a SOLO pod `check` asserts the pod is NOT in it;
# for a MESH pod it asserts the pod IS. Override if this host's mesh is named
# something else (`svrn mesh status`).
HOME_MESH="${HOME_MESH:-Meshsonics}"

# The HOME daemon's client port. Deliberately NOT resolved through
# SOVEREIGN_DAEMON_URL: the whole point of the `env` verb is to point a session
# at the POD, and a shell that has done so must still be able to read the
# founder's invite and the founder's member list from the machine in front of
# it. Same reason `client_daemon_base` has a `client_daemon_base_for` twin for
# callers that manage the local daemon rather than talk to a daemon.
HOME_PORT="${HOME_PORT:-9741}"

# ── model loadout ────────────────────────────────────────────────────────────
# name | expected bytes | url.  Sizes are the gate: a truncated pull must fail
# loudly, not boot a daemon on half a GGUF.
#   35B  : 30,011,242,784 — byte-exact match for the local
#          Qwen3.6-35B-A3B-MTP-UD-Q6_K.gguf (local name is a rename; the
#          `out=` field below pulls HF's `-UD-Q6_K` under the LOCAL name so
#          the daemon registers the SAME model id and a bench run on the pod
#          stays comparable to baselines minted on the founder).
#   4B   : 4,261,908,800 — byte-exact match for the local
#          Qwen3.5-4B-UD-MTP-Q6_K_XL.gguf (local name is a rename).
#   embed: 639,150,592 — byte-exact match for the local copy.
#
# THE PRIMARY WAS THE 27B UNTIL 2026-09-03 AND THAT WAS NOT THIS HOST'S
# LOADOUT. RuggedFox's resident `primary` alias is the 35B-A3B (`/v1/models`);
# the 27B was a near-neighbour whose own comment here conceded it was "fine
# for dev, NOT for judge parity". A bench run is exactly the judge-parity
# case: every committed baseline was minted on the 35B, so a pod carrying the
# 27B reds HARD lanes for the MODEL and reports it as a regression in the
# change under test (ARCH §18.4 — validate the instrument before the result).
# All three slots are now byte-exact with the founder.
read -r -d '' DEFAULT_LOADOUT <<'MODELS' || true
primary Qwen3.6-35B-A3B-MTP-UD-Q6_K.gguf 30011242784 https://huggingface.co/unsloth/Qwen3.6-35B-A3B-MTP-GGUF/resolve/main/Qwen3.6-35B-A3B-UD-Q6_K.gguf
fast Qwen3.5-4B-UD-Q6_K_XL.gguf 4261908800 https://huggingface.co/unsloth/Qwen3.5-4B-MTP-GGUF/resolve/main/Qwen3.5-4B-UD-Q6_K_XL.gguf
embed Qwen3-Embedding-0.6B-Q8_0.gguf 639150592 https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF/resolve/main/Qwen3-Embedding-0.6B-Q8_0.gguf
MODELS

# THE LOADOUT IS CONFIGURABLE, and it is ONE declaration.
#
#   LOADOUT_FILE=my-loadout.txt ./dev-pod.sh up
#   ./dev-pod.sh up --loadout my-loadout.txt
#
# Four whitespace-separated fields per line: `slot name bytes url`. `slot` is
# the config.toml key the model lands under (`primary`, `fast`, `embed`), so
# the daemon config below is GENERATED from these lines rather than repeating
# the filenames — the two used to be typed separately, which is one loadout
# declared twice and a rename away from a pod that pulls one model and boots
# pointing at another (ARCH §10.6).
#
# The sizes are the gate and they are not optional: a truncated pull must fail
# loudly, not boot a daemon on half a GGUF. They are also what the offer
# search is sized from, so a wrong number here rents the wrong box.
LOADOUT="${DEFAULT_LOADOUT}"
load_loadout_file() {
  [ -r "$1" ] || { echo "[dev-pod] cannot read loadout file: $1" >&2; exit 2; }
  LOADOUT="$(grep -v '^[[:space:]]*\(#\|$\)' "$1")"
  [ -n "$LOADOUT" ] || { echo "[dev-pod] loadout file is empty: $1" >&2; exit 2; }
}
[ -n "${LOADOUT_FILE:-}" ] && load_loadout_file "$LOADOUT_FILE"

# Every line must carry all four fields, or the awk consumers below silently
# build a malformed url list / config. Refuse at parse time, on the ground,
# rather than at boot on a billing GPU.
printf '%s\n' "$LOADOUT" | awk 'NF != 4 {
  printf "[dev-pod] loadout line %d has %d fields, expected 4 (slot name bytes url): %s\n", NR, NF, $0 > "/dev/stderr"; bad=1
} END { exit bad+0 }' || exit 2

# $1 = join link, or "" for a solo island. Rendered BEFORE the instance exists,
# so nothing in here may depend on the instance id.
onstart_script() {
local join_link="${1:-}"
cat <<EOS
set -euo pipefail
echo "[dev-pod] boot \$(date -u +%FT%TZ)"
nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader || true
/usr/local/bin/cuda-preflight || { echo "[dev-pod] FATAL: cuda preflight failed"; exit 1; }

# aria2c + sshd are baked into the image's runtime stage as of 2026-08-28.
# Only reach for a mirror if one is genuinely missing — an apt round-trip on
# a GPU billing by the minute is worth skipping, and it is one more thing
# that can fail at boot on a host with flaky egress.
export DEBIAN_FRONTEND=noninteractive
missing=""
command -v aria2c >/dev/null || missing="\$missing aria2"
[ -x /usr/sbin/sshd ] || missing="\$missing openssh-server"
if [ -n "\$missing" ]; then
  echo "[dev-pod] apt: installing\$missing (not in image — is it stale?)"
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends \$missing >/dev/null
else
  echo "[dev-pod] aria2c + sshd present in image, skipping apt"
fi
mkdir -p /run/sshd && /usr/sbin/sshd || true

mkdir -p /workspace/models /workspace/data /root/.svrnmesh /root/.local/share
# CLI mesh verbs use XDG, the daemon uses ~/.svrnmesh — unify or they split-brain.
[ -e /root/.local/share/svrnmesh ] || ln -s /root/.svrnmesh /root/.local/share/svrnmesh

cd /workspace/models
# aria2 input format: a URL line, then INDENTED per-download options. The
# out= is not cosmetic and it is not optional. A HuggingFace resolve/main/
# URL redirects to a CDN whose path ends in the blob SHA256, and aria2 names
# the output from the FINAL url -- so without out= all three files land as
#
# (NO BACKTICKS ANYWHERE BELOW THIS LINE. Everything from here to the EOS is
# inside an UNQUOTED heredoc, so bash performs command substitution on it --
# comments included. A backticked phrase in a comment is EXECUTED at render
# time: this block once printed "resolve/main/...: No such file or directory"
# while rendering, and only luck kept it from corrupting the boot script.)
# 64-hex-character names, byte-perfect and unusable. Flown 2026-08-29: the
# download reported "(OK):download completed" and all three byte gates then
# read 0 bytes, because they stat the names the config.toml refers to. The
# gates caught it (that is them working), but it cost a rental.
cat > /tmp/urls <<'URLS'
$(printf '%s\n' "$LOADOUT" | awk '{print $4 "\n  out=" $2}')
URLS
echo "[dev-pod] pulling loadout (~${WEIGHTS_GB} GB) from HuggingFace"
time aria2c -x 8 -s 8 -j 3 --continue=true --auto-file-renaming=false \\
     --summary-interval=15 -i /tmp/urls

fail=0
$(printf '%s\n' "$LOADOUT" | awk '{printf "have=$(stat -c %%s %s 2>/dev/null || echo 0); [ \"$have\" = \"%s\" ] || { echo \"[dev-pod] FATAL: %s is \$have bytes, expected %s\"; fail=1; }\n", $2, $3, $2, $3}')
[ "\$fail" = 0 ] || { echo "[dev-pod] refusing to boot on an incomplete loadout"; exit 1; }

cat > /root/.svrnmesh/config.toml <<'CFG'
[models]
$(printf '%s\n' "$LOADOUT" | awk '{printf "%-7s = \"/workspace/models/%s\"\n", $1, $2}')
context_size = $CTX

# mDNS is OFF in both modes because a Vast box is a SHARED machine — leaving
# discovery on would let it see co-tenant containers on the same LAN segment.
# In mesh mode the pod finds the founder by the iroh dial string in the invite,
# never by broadcasting on the datacentre's LAN.
[discovery]
mdns = false
seed_addrs = []
CFG

# MTP (speculative decode off the model's own nextn head) is ON by default;
# SOVEREIGN_MTP_DISABLE=1 kills it, SOVEREIGN_MTP_DRAFT_MAX tunes depth (default 3).
export SOVEREIGN_DATA_DIR=/workspace/data
export SOVEREIGN_MODELS_DIR=/workspace/models
# Belt and braces on top of "no mesh join": even if a peer were ever learned,
# this daemon answers from its own GPU or not at all (peer_inference.rs).
export SOVEREIGN_DISABLE_PEER_INFERENCE=1

# ── Joining, when this pod was rented with --mesh ────────────────────────────
#
# A background waiter, not an inline step: the daemon is exec'd as this
# script's last act, so nothing after it would ever run. The waiter polls the
# daemon's OWN mesh status until the HTTP listener answers (well before the
# 35B finishes loading — join needs the listener, not the slots), then POSTs
# the invite to \`/v1/mesh/join\`, which is the same call \`svrn mesh join\`
# makes. Routing through the RUNNING daemon is load-bearing: a join performed
# in a separate CLI process updates that process's in-memory mesh and not the
# one that serves gossip (mesh_cmd.rs:2767-2782).
#
# It reports its own verdict and never pretends: no timeout, no non-2xx, no
# missing link is swallowed. \`tee\`, NOT \`>>\`: \`dev-pod.sh logs\` shows the
# CONTAINER's stdout, so a waiter that only appended to /workspace/daemon.log
# was invisible from the operator side — measured on flight 49188146, where the
# join SUCCEEDED and the only way to know was to ask the founder. A verdict
# nobody can read is not a verdict (ARCH §9.1). The daemon line below already
# tees for the same reason.
JOIN_LINK='$join_link'
if [ -n "\$JOIN_LINK" ]; then
  # Name the member after the Vast contract so a row in \`svrn mesh status\`
  # can be traced back to the thing that is billing. The \`vast-\` prefix is
  # ours, not Vast's — the founder side keys on it to find rented members.
  NODE_NAME="vast-\${VAST_CONTAINERLABEL#C.}"
  [ "\$NODE_NAME" = "vast-" ] && NODE_NAME="vast-\$(hostname)"
  (
    echo "[dev-pod] mesh mode: waiting for the local daemon before joining as \$NODE_NAME"
    joined=no
    for i in \$(seq 1 150); do
      if curl -sf --max-time 4 http://127.0.0.1:9741/v1/mesh/status >/dev/null 2>&1; then
        code=\$(curl -s -o /tmp/join-resp.json -w '%{http_code}' --max-time 30 \
          -X POST http://127.0.0.1:9741/v1/mesh/join \
          -H 'content-type: application/json' \
          --data "{\"key_or_url\":\"\$JOIN_LINK\",\"node_name\":\"\$NODE_NAME\"}" || echo 000)
        if [ "\$code" = "200" ] || [ "\$code" = "204" ]; then
          echo "[dev-pod] JOINED: \$(cat /tmp/join-resp.json)"
          joined=yes
          break
        fi
        # Do NOT give up on the first non-2xx. The listener answers before the
        # daemon has finished binding its iroh endpoint, and an encrypted mesh
        # cannot be joined until it has — so the first attempt can legitimately
        # fail on a pod that will join fine seconds later. Keep trying to the
        # loop bound; the FAILED line still prints each time, so a permanent
        # failure is loud rather than silent.
        echo "[dev-pod] join attempt \$i failed (HTTP \$code): \$(cat /tmp/join-resp.json 2>/dev/null)"
      fi
      sleep 4
    done
    if [ "\$joined" != yes ]; then
      echo "[dev-pod] JOIN DID NOT HAPPEN — this pod is a solo island despite --mesh."
      echo "[dev-pod] Federated retrieval will NOT work. Do not read a bench off it."
    fi
  ) 2>&1 | tee -a /workspace/daemon.log &
else
  echo "[dev-pod] solo mode: this daemon joins no mesh"
fi

echo "[dev-pod] launching daemon on 127.0.0.1:9741 — tunnel in to reach it"

# SUPERVISED, NOT EXEC'D — and the supervisor is a REAL FILE with a REAL TEST
# (scripts/pod-supervise.sh, scripts/tests/pod-supervise.sh), shipped here
# base64'd so nothing in it has to survive this heredoc's expansion.
#
# Using exec was terminal: a failed GGML_ASSERT in llama_decode calls abort(),
# so one over-long prompt killed the daemon and a four-hour bench reported
# "daemon unreachable" from that point on.
#
# The first fix was an inline while-loop piping the daemon into tee, and it
# WEDGED: bash waits for every member of a pipeline, tee only sees EOF when
# the last writer closes, and the daemon's surviving children hold that pipe.
# (No backticks in this comment on purpose — see the warning above: this is an
#  UNQUOTED heredoc, so a backticked phrase in a COMMENT is executed at render
#  time. Writing that exact loop in backticks here ran it locally, forever.)
# Flown 2026-09-03 on pod 49783403 — supervisor alive, no daemon, no restart.
# That is exactly why the loop is no longer inline: a heredoc cannot be tested,
# and this bug only appears when the supervised process leaves a child behind.
echo '$(base64 -w0 < "$SCRIPT_DIR_LOCAL/pod-supervise.sh")' | base64 -d > /workspace/pod-supervise.sh
chmod +x /workspace/pod-supervise.sh
exec /workspace/pod-supervise.sh /workspace/daemon.log sovereign-cli daemon run
EOS
}

# ── Which offers are even eligible ───────────────────────────────────────────
#
# THE ARCHITECTURE FILTER IS A SAFETY GUARD, NOT A PREFERENCE. The image is
# compiled for CUDA_ARCHITECTURES="80;86;89;90" (Containerfile.cuda). Rent a
# Turing card — the Quadro RTX 8000 is 45G and routinely the CHEAPEST offer
# that fits the loadout — and llama.cpp has no kernels for it, so the flight
# fails with a CUDA error that says nothing about whether the model loads. You
# pay for a could-not-judge.
#
# ALLOWLIST, NOT DENYLIST: an unrecognised GPU is EXCLUDED from auto-pick
# rather than assumed fine (ARCH §18.3 — refuse, do not substitute). `offers`
# still lists it, marked, so you can name it explicitly if you know better.
SUPPORTED_GPUS='A100|A6000|A40|A10|RTX 3090|RTX 4090|6000Ada|5880Ada|L40|H100|H200|RTX 5090|RTX PRO 6000'

# ── Sizing: the loadout decides which boxes are even eligible ────────────────
#
# The floor used to be a hardcoded `gpu_ram>=46` + `disk_space>=80`. Those two
# numbers were right for ONE loadout and became silently wrong the moment
# anybody configured a different model — the search would go on returning
# 48 GB boxes for a loadout that needs 70, and the mismatch would surface after
# the pull, the boot and the bill. They are now DERIVED from the loadout by the
# same estimator the daemon's own boot preflight uses
# (`sovereign_inference::capacity`, via `svrn daemon vram-plan`), so the box
# that gets rented tracks whatever models are configured above.
#
# There is no fallback number here on purpose. If the estimator cannot be
# reached the script REFUSES rather than quietly reverting to 46 — a rental
# sized by a stale constant is exactly the silent substitution ARCH §18.3
# forbids, and it is one that costs money.
svrn_bin() {
  # `svrn` and `sovereign` are the same binary under two names and not every
  # host has both (AGENTS.md). Take whichever is here.
  if [ -n "${SOVEREIGN_CLI:-}" ]; then echo "$SOVEREIGN_CLI"; return; fi
  for c in svrn sovereign; do command -v "$c" >/dev/null && { echo "$c"; return; }; done
  for c in target/debug/sovereign-cli ./target/debug/sovereign-cli; do
    [ -x "$c" ] && { echo "$c"; return; }
  done
  return 1
}

# Populates MIN_VRAM_MIB / MIN_VRAM_GB / DISK_GB / REQUIRED_MIB / WEIGHTS_GB
# from the LOADOUT. Memoised — several verbs want it and it costs a process.
POD_PLAN_DONE=""
pod_plan() {
  [ -n "$POD_PLAN_DONE" ] && return 0
  local bin slots line
  bin=$(svrn_bin) || {
    echo "[dev-pod] cannot find the sovereign CLI, so the loadout cannot be sized." >&2
    echo "[dev-pod] build it (cargo build -p sovereign-cli-daemon) or set SOVEREIGN_CLI." >&2
    echo "[dev-pod] refusing to rent against a guessed VRAM floor." >&2
    return 1
  }
  slots=()
  while read -r slot _name bytes _url; do
    [ -n "$slot" ] || continue
    slots+=(--slot "$slot:$bytes:$CTX")
  done <<< "$LOADOUT"

  line=$("$bin" daemon vram-plan "${slots[@]}" --json 2>/dev/null | tail -1) || true
  case "$line" in
    *'"min_total_vram_mb"'*) ;;
    *) echo "[dev-pod] \`$bin daemon vram-plan\` produced no plan — refusing to size by guess." >&2
       return 1 ;;
  esac
  eval "$(printf '%s' "$line" | python3 -c '
import sys, json, math
d = json.load(sys.stdin)
print("MIN_VRAM_MIB=%d" % d["min_total_vram_mb"])
print("MIN_VRAM_GB=%d" % d["min_total_vram_gb"])
print("WEIGHTS_GB_INT=%d" % d["weights_gb"])
print("REQUIRED_MIB=%d" % d["required_mb"])
print("WEIGHTS_GB=%.1f" % (d["weights_bytes"] / 1e9))
')"
  # One number for both the search floor and the volume actually requested.
  # The estimator reports the WEIGHTS; the headroom on top is this script's
  # own domain — the CUDA image (~12 GB), the daemon's data dir, and room for
  # a partial re-pull on a resume.
  DISK="${DISK:-$(( WEIGHTS_GB_INT + POD_DISK_HEADROOM_GB ))}"
  POD_PLAN_DONE=1
}

# NOTE ON UNITS, because they differ between the two halves of the same API:
# the SEARCH QUERY takes `gpu_ram` in GB (`gpu_ram>=46`), while the returned
# ROW carries `gpu_ram` in MiB (49140 for a 48 GB card). Verified 2026-09-03:
# `gpu_ram>=46000` returns zero rows. The query below is therefore in GB and
# the exact eligibility test in `offer_eligible` is in MiB.
offer_rows() {
  pod_plan || exit 1
  vastai search offers \
    "gpu_ram>=$MIN_VRAM_GB num_gpus=1 rentable=true disk_space>=$DISK inet_down>=1500 dph<1.30 reliability>0.97" \
    -o dph --raw
}

# The one eligibility predicate, shared by `offers` and `up`. It used to be
# spelled twice — the same regex inlined in two Python blocks — so a rule added
# to one silently did not apply to the other, and `offers` could show a box as
# fine that auto-pick would refuse (or worse, the reverse).
#
# Reads rows on stdin, writes the eligible ones back as JSON, each with an
# added `_skip` field naming why it was refused (empty when eligible).
offer_eligible() {
  pod_plan || exit 1
  SUPPORTED_GPUS="$SUPPORTED_GPUS" MIN_VRAM_MIB="$MIN_VRAM_MIB" python3 -c "
import sys, json, os, re
rows = json.load(sys.stdin)
gpus = re.compile(os.environ['SUPPORTED_GPUS'])
need = int(os.environ['MIN_VRAM_MIB'])
out = []
for r in rows:
    why = ''
    if not gpus.search(r.get('gpu_name') or ''):
        why = 'arch'
    elif (r.get('gpu_ram') or 0) < need:
        why = 'vram'
    r['_skip'] = why
    out.append(r)
json.dump(out, sys.stdout)
"
}

# ── Resolving WHICH pod, without making you carry a number ───────────────────
#
# The label is the source of truth, not a local state file (§10.6: one decider).
# A file in /tmp dies on reboot, a file in the repo is one more thing to sync,
# and either can disagree with Vast. `vastai show instances` cannot — it IS the
# billing surface. So every verb takes an OPTIONAL id and falls back to "the
# one instance whose label starts with $LABEL_PREFIX".
#
# Refuses on ambiguity rather than guessing: two dev pods and no id named is a
# question, not a default (ARCH §18.3 — never silently substitute).
#
# `resolve_row` prints "<id> <label>" so callers can have the MODE for free.
# When an id is passed explicitly we still look the row up, because the id
# alone does not say which mode the pod was rented in — and a `down` that
# guessed "solo" on a mesh pod would destroy it without leaving, stranding a
# live member row on every peer.
resolve_row() {
  vastai show instances --raw 2>/dev/null \
    | LABEL_PREFIX="$LABEL_PREFIX" WANT_ID="${1:-}" python3 -c '
import sys, json, os
prefix = os.environ["LABEL_PREFIX"]
want = os.environ.get("WANT_ID") or ""
try: rows = json.load(sys.stdin)
except Exception: rows = []
mine = [r for r in rows if (r.get("label") or "").startswith(prefix)]
if want:
    hit = [r for r in mine if str(r.get("id")) == want]
    if hit:
        print("%s %s" % (hit[0]["id"], hit[0].get("label") or ""))
        raise SystemExit(0)
    # An id we were handed that is not one of ours: report it rather than
    # inventing a mode for it.
    print(f"instance {want} is not labelled {prefix}* — refusing to guess its mode",
          file=sys.stderr)
    raise SystemExit(1)
if len(mine) == 1:
    print("%s %s" % (mine[0]["id"], mine[0].get("label") or ""))
    raise SystemExit(0)
if not mine:
    print(f"no instance labelled {prefix}* — nothing to act on", file=sys.stderr)
else:
    ids = ", ".join(str(r["id"]) for r in mine)
    print(f"{len(mine)} instances labelled {prefix}* ({ids}) — name one explicitly",
          file=sys.stderr)
raise SystemExit(1)
'
}

resolve_id() { resolve_row "${1:-}" | cut -d" " -f1; }

# "mesh" or "solo", read off the resolved label. Never inferred from what the
# pod is currently DOING — that is what `check` compares against.
pod_mode() {
  case "$1" in
    "$LABEL_MESH") printf 'mesh' ;;
    *)             printf 'solo' ;;
  esac
}

# ── The founder side ─────────────────────────────────────────────────────────
#
# One place that asks the HOME daemon anything, so "which daemon is home" is
# answered once (§10.6). Prints the raw /v1/mesh/status JSON; empty on failure.
home_status() {
  timeout 6 curl -s "http://127.0.0.1:$HOME_PORT/v1/mesh/status" 2>/dev/null || true
}

# The invite a joining pod needs, or a refusal. NOT the bare `join_key`: a bare
# key carries no way to REACH the founder, and Meshsonics is an encrypted mesh
# whose join is key-dialed and fail-closed. The `sovereign://` deep link
# carries `iroh=<pubkey>@<relay>,<addrs>` — that dial string is the whole
# reason a box in another datacentre can join at all (deep_link.rs:26-48).
home_join_link() {
  home_status | HOME_MESH="$HOME_MESH" HOME_PORT="$HOME_PORT" python3 -c '
import sys, json, os
home, port = os.environ["HOME_MESH"], os.environ["HOME_PORT"]
try: d = json.load(sys.stdin)
except Exception:
    print(f"no daemon answering /v1/mesh/status on 127.0.0.1:{port} — "
          "start it before renting a mesh pod", file=sys.stderr)
    raise SystemExit(1)
name = d.get("mesh_name") or "(none)"
if name != home:
    print(f"this host is in mesh {name!r}, not {home!r} — refusing to hand a pod "
          "an invite to a mesh you did not ask for (set HOME_MESH to override)",
          file=sys.stderr)
    raise SystemExit(1)
link = d.get("join_link")
if not link:
    print(f"mesh {name!r} publishes no join link — it is still a solo mesh. "
          "Run `svrn mesh create` to make it joinable, then retry.", file=sys.stderr)
    raise SystemExit(1)
# chr(39) rather than a literal apostrophe: this whole program is inside a
# single-quoted `python3 -c` argument, so an apostrophe here would end it.
if chr(39) in link:
    print("the join link contains a single quote and cannot be safely embedded "
          "in the boot script. Refusing.", file=sys.stderr)
    raise SystemExit(1)
if "iroh=" not in link:
    print("the join link carries no iroh dial string, so a pod outside this LAN "
          "could not reach the founder. Refusing to rent something that cannot "
          f"join. link={link}", file=sys.stderr)
    raise SystemExit(1)
print(link)
'
}

# Members of the HOME mesh that are rented pods (see NODE_NAME_PREFIX below),
# one "<status> <name>" per line. The observation `check` and `down` report
# against — a pod that joined should appear here, and a pod that left should
# not.
home_pod_members() {
  home_status | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: raise SystemExit(0)
for m in d.get("members") or []:
    if (m.get("name") or "").startswith("vast-") and m.get("active"):
        print("%s %s" % (m.get("status"), m.get("name")))
'
}

case "${1:-}" in
plan)
  # What does the configured loadout need, and would a given card hold it?
  # Read-only and free — the answer that decides which box `up` may rent.
  pod_plan || exit 1
  bin=$(svrn_bin) || exit 1
  slots=()
  while read -r slot _name bytes _url; do
    [ -n "$slot" ] || continue
    slots+=(--slot "$slot:$bytes:$CTX")
  done <<< "$LOADOUT"
  printf '%s\n' "$LOADOUT" | awk '{printf "  %-8s %s\n", $1, $2}'
  echo
  "$bin" daemon vram-plan "${slots[@]}" ${2:+--vram-mb "$2"}
  ;;
offers)
  pod_plan || exit 1
  echo "[dev-pod] loadout needs ${REQUIRED_MIB} MiB working set -> ${MIN_VRAM_GB} GB card, ${DISK} GB disk" >&2
  offer_rows | offer_eligible | python3 -c '
import sys, json
rows = json.load(sys.stdin)
print("%10s %-24s %6s %8s %8s %6s  %-4s %s" % ("offer","gpu","vram","$/hr","down","rel","","geo"))
for r in rows[:15]:
    print("%10s %-24s %5.0fG %8.3f %7.0fM %6.3f  %-4s %s" % (
        r["id"], r["gpu_name"][:24], r["gpu_ram"]/1024, r["dph_total"],
        r.get("inet_down") or 0, r.get("reliability2") or 0,
        r["_skip"].upper(),
        (r.get("geolocation") or "?")[:24]))
print("\nARCH = outside the compiled CUDA archs. VRAM = too small for this loadout.", file=sys.stderr)
print("Both are refused by auto-pick.", file=sys.stderr)'
  ;;
up)
  # An offer id is OPTIONAL and usually the wrong thing to type: offers churn
  # within hours (45241992 was live at 03:50 and gone by 21:24 the same day),
  # so a hand-copied id is stale by the time it is pasted and `create` fails.
  # With no argument, take the cheapest offer that passes the ARCHITECTURE
  # guard -- never merely the cheapest.
  #
  # --mesh may appear before or after the offer id; both read naturally and
  # neither should be a syntax lesson.
  mode=solo; label="$LABEL_SOLO"; join_link=""; dry=""
  offer=""
  shift || true
  while [ $# -gt 0 ]; do
    case "$1" in
      --mesh)      mode=mesh; label="$LABEL_MESH" ;;
      --dry-run)   dry=1 ;;
      --loadout)   [ -n "${2:-}" ] || { echo "[dev-pod] --loadout needs a file" >&2; exit 2; }
                   load_loadout_file "$2"; shift ;;
      --loadout=*) load_loadout_file "${1#--loadout=}" ;;
      -*)          echo "[dev-pod] unknown flag $1 (--mesh, --dry-run, --loadout FILE)" >&2; exit 2 ;;
      *)           offer="$1" ;;
    esac
    shift
  done
  # Size the loadout BEFORE anything is rented, and refuse if it cannot be
  # sized. Also runs on the explicit-offer path, where the auto-pick search
  # never happens but the boot script still needs the pull size.
  pod_plan || exit 1
  # RESOLVE THE INVITE BEFORE SPENDING A CENT. Every way this can fail — no
  # daemon, wrong mesh, still solo, a link with no iroh dial string — is a
  # refusal that happens while nothing is billing. Renting first and
  # discovering the pod cannot join costs a flight (ARCH §18.3).
  if [ "$mode" = mesh ]; then
    join_link=$(home_join_link) || { echo "[dev-pod] refusing to rent a mesh pod" >&2; exit 1; }
    echo "[dev-pod] mesh mode: pod will join \"$HOME_MESH\" using the founder's invite" >&2
  fi
  if [ -z "$offer" ]; then
    offer=$(offer_rows | offer_eligible | python3 -c '
import sys, json
rows = json.load(sys.stdin)
ok = [r for r in rows if not r["_skip"]]
if not ok:
    n_arch = sum(1 for r in rows if r["_skip"] == "arch")
    n_vram = sum(1 for r in rows if r["_skip"] == "vram")
    print("no eligible offer: %d rentable now, %d refused on CUDA arch, %d too "
          "small for this loadout" % (len(rows), n_arch, n_vram), file=sys.stderr)
    raise SystemExit(1)
best = min(ok, key=lambda r: r["dph_total"])
print("%s %s %.3f %s" % (best["id"], best["gpu_name"].replace(" ","_"), best["dph_total"], best.get("geolocation","?").replace(" ","_")), file=sys.stderr)
print(best["id"])
') || exit 1
  fi
  # Everything conversational goes to stderr; stdout carries the id ALONE, so
  # `id=$(dev-pod.sh up)` works.
  echo "[dev-pod] offer $offer  image: $IMAGE  disk: ${DISK}G  ctx: $CTX  mode: $mode" >&2
  boot="$(mktemp -d)/dev-pod-boot.sh"; onstart_script "$join_link" > "$boot"
  bash -n "$boot" || { echo "[dev-pod] refusing to rent: boot script does not parse"; exit 1; }
  # --dry-run: everything up to the point of spending. The rendered boot script
  # is the part nobody can review once the pod is billing — an unquoted-heredoc
  # render is where this script has historically gone wrong (see the aria2
  # `out=` and the backtick warnings above), and a mesh join adds an invite and
  # a POST body to that surface. Reviewing it costs nothing; discovering it on
  # a rented GPU costs a flight.
  if [ -n "$dry" ]; then
    echo "[dev-pod] DRY RUN — nothing rented, nothing billing." >&2
    echo "[dev-pod] would create offer=$offer label=$label mode=$mode" >&2
    echo "[dev-pod] rendered boot script ($boot):" >&2
    cat "$boot"
    exit 0
  fi
  # --raw so the instance id is CAPTURED, not eyeballed out of a printed dict.
  # Every other verb can then be run with no arguments at all.
  id=$(vastai create instance "$offer" \
    --image "$IMAGE" --disk "$DISK" --label "$label" \
    --ssh --direct --cancel-unavail \
    --onstart "$boot" --raw 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: raise SystemExit(1)
if not d.get("success"): 
    print(d.get("msg") or "create failed", file=sys.stderr); raise SystemExit(1)
print(d["new_contract"])
') || { echo "[dev-pod] create FAILED — nothing rented, nothing billing"; exit 1; }
  echo "$id"
  echo "[dev-pod] rented $id -- billing runs until 'dev-pod.sh down', which needs no id" >&2
  echo "[dev-pod] watch it:  ./dev-pod.sh logs    first boot ~4-6 min: image pull, 30 GB models, slot load" >&2
  if [ "$mode" = mesh ]; then
    echo "[dev-pod] the join is a BACKGROUND step inside the pod — grep the log for JOINED/JOIN FAILED," >&2
    echo "[dev-pod] then confirm from this side with:  ./dev-pod.sh check" >&2
  fi
  ;;
logs)
  id=$(resolve_id "${2:-}") || exit 1
  vastai logs "$id" --tail "${3:-120}"
  ;;
env)
  # Paste into the shell you launch the SECOND session from, BEFORE `claude`.
  # Bash tool calls inherit the session process env, so this scopes to that
  # session only — the local-daemon session is untouched.
  cat <<EOF
export SOVEREIGN_DAEMON_URL=http://127.0.0.1:$LOCAL_PORT
export SVRNMESH_DAEMON_URL=http://127.0.0.1:$LOCAL_PORT
export SOVEREIGN_DISABLE_PEER_INFERENCE=1
EOF
  ;;
check)
  # Which daemon is on each port, and is the pod in the mode it was RENTED in?
  # The loadout is the SAME on both, so model ids do not discriminate — mesh
  # membership does.
  #
  # THE EXPECTATION COMES FROM THE LABEL, NOT FROM A FLAG YOU REMEMBER TO TYPE.
  # A solo pod must not be a member of the operator's mesh; a --mesh pod must
  # be one. Same observation, opposite verdicts, and the instance label is what
  # decides which — so `check` cannot be run against the wrong expectation.
  #
  # EXITS NON-ZERO WHEN THE OBSERVATION CONTRADICTS THE LABEL. It printed a
  # verdict and returned 0 unconditionally until 2026-08-29, which is not a
  # gate (ARCH §18.1): "!! JOINED SOMETHING !!" scrolled past in a green run.
  #
  # The older bug this replaced is worth keeping in view: the predicate used to
  # assert mesh=(none) and could never have passed, because a fresh daemon
  # AUTO-CREATES its own single-node mesh ("<hostname>'s Mesh",
  # members_total=1). Isolation was holding the whole time and the check was
  # merely unsatisfiable — worse than no check, because it reads as a real
  # failure.
  mode=solo
  if row=$(resolve_row "${2:-}" 2>/dev/null); then
    mode=$(pod_mode "$(printf '%s' "$row" | cut -d' ' -f2)")
    echo "[dev-pod] instance $(printf '%s' "$row" | cut -d' ' -f1) was rented in $mode mode"
  else
    echo "[dev-pod] no dev pod listed; checking ports anyway, expecting $mode" >&2
  fi

  rc=0
  for port in "$HOME_PORT" "$LOCAL_PORT"; do
    # Port $HOME_PORT is always the founder and must always read HOME. The
    # tunnel port is the pod, and what it must read depends on the mode.
    if [ "$port" = "$HOME_PORT" ]; then want=home; else want="$mode"; fi
    printf "%5s  " "$port"
    out=$(timeout 6 curl -s "http://127.0.0.1:$port/v1/mesh/status" 2>/dev/null \
      | HOME_MESH="$HOME_MESH" WANT="$want" python3 -c '
import sys, json, os
home, want = os.environ.get("HOME_MESH", ""), os.environ["WANT"]
# Exit 3 = COULD NOT JUDGE (nothing answered / not JSON), which is a different
# verdict from exit 1 = the predicate failed. Collapsing the two made a
# tunnel-less `check` on a solo pod report red (ARCH §18.1: four verdicts, not
# two — passed, failed, could-not-judge, never-ran).
try: d = json.load(sys.stdin)
except Exception: raise SystemExit(3)
name = d.get("mesh_name") or "(none)"
total = d.get("members_total")
in_home = (name == home)
solo = (not in_home) and (total in (0, 1, None))
# What we SAW, named once.
seen = "HOME" if in_home else ("solo island" if solo else "JOINED SOMETHING ELSE")
# What we REQUIRED. A mesh pod and the founder must both read HOME; a solo pod
# must read solo island. Anything else is a contradiction, including a solo pod
# that has wandered into a third mesh.
ok = {"home": in_home, "mesh": in_home, "solo": solo}[want]
print("mesh=%-28s members=%s/%s peer_inflight=%s/%s  %s%s" % (
    name, d.get("members_online"), total,
    d.get("peer_inflight_current"), d.get("peer_inflight_ceiling"),
    seen, "" if ok else "   !! EXPECTED %s !!" % want.upper()))
raise SystemExit(0 if ok else 1)') || {
      case "$?" in
        1) rc=1 ;;
        *) out="no answer (tunnel down?)"
           # COULD NOT JUDGE. For a solo pod with no tunnel up that is simply
           # unobserved, and unobserved is not failed. For the founder port, or
           # for a pod that is supposed to be ON the mesh, silence IS the
           # finding: neither can be absent and the mode still hold.
           case "$want" in mesh|home) rc=1 ;; esac ;;
      esac
    }
    echo "${out:-no answer (tunnel down?)}"
  done

  # The founder's own view of rented members — the half the pod cannot
  # self-report. A mesh pod that never appears here did not really join,
  # whatever its own /v1/mesh/status says.
  pods=$(home_pod_members)
  if [ -n "$pods" ]; then
    echo "[dev-pod] rented members seen by $HOME_MESH:"
    # One member per line. `printf %s\n $(...)` word-split each row into its
    # own line ("online" then "vast-49188146"), which reads as two members.
    printf '%s\n' "$pods" | sed 's/^/  /' 
  else
    echo "[dev-pod] $HOME_MESH lists no rented (vast-*) members"
    [ "$mode" = mesh ] && { echo "[dev-pod] !! a --mesh pod is missing from the founder member list" >&2; rc=1; }
  fi
  exit "$rc"
  ;;
tunnel)
  id=$(resolve_id "${2:-}") || exit 1
  url=$(vastai ssh-url "$id")                         # ssh://root@host:port
  hostport="${url#ssh://root@}"
  echo "[dev-pod] http://127.0.0.1:$LOCAL_PORT  ->  pod 127.0.0.1:9741   (ctrl-c to drop)"
  # KEEPALIVES + RECONNECT. A bare `ssh -N` holds a TCP session open across a
  # WAN for as long as the run lasts, and a multi-hour bench is long enough
  # for it to go away: a NAT idle-timeout or a dropped route leaves the ssh
  # PROCESS alive with forwarding dead, so the port answers connection-refused
  # while `pgrep ssh` says everything is fine. Flown 2026-09-03 — the tunnel
  # process was still listed while every lane reported "daemon unreachable",
  # which sent the diagnosis down the wrong path for a while.
  #
  # ServerAliveInterval makes ssh notice a dead peer instead of hanging on it,
  # and the loop reconnects rather than ending the run. ExitOnForwardFailure
  # means a tunnel that cannot bind FAILS instead of sitting there forwarding
  # nothing (§18.3 — absence is reported, never defaulted).
  #
  # `exec` is gone on purpose: the point is to outlive one connection.
  attempt=0
  while :; do
    attempt=$((attempt + 1))
    [ "$attempt" -gt 1 ] && echo "[dev-pod] tunnel reconnect #$attempt at $(date -u +%FT%TZ)"
    ssh -N -o StrictHostKeyChecking=accept-new \
        -o ServerAliveInterval=15 -o ServerAliveCountMax=3 \
        -o ExitOnForwardFailure=yes -o ConnectTimeout=15 \
        -L "$LOCAL_PORT:127.0.0.1:9741" \
        -p "${hostport##*:}" "root@${hostport%%:*}"
    echo "[dev-pod] tunnel dropped (status $?) — retrying in 5s; ctrl-c to stop"
    sleep 5
  done
  ;;
down)
  # -y IS LOAD-BEARING. Without it vastai prompts "Are you sure? [y/N]" and,
  # with no tty answer, prints "Aborted." and exits — leaving the pod RUNNING
  # AND BILLING while the command looks like it did something. Observed
  # 2026-08-29 on the first flight. Billing stops on destroy and nothing else,
  # so a teardown that no-ops on the happy path is the worst bug this script
  # could carry.
  row=$(resolve_row "${2:-}") || exit 1
  id=$(printf '%s' "$row" | cut -d' ' -f1)
  mode=$(pod_mode "$(printf '%s' "$row" | cut -d' ' -f2)")

  # LEAVING IS PART OF TEARDOWN, AND IT HAPPENS FIRST. Destroying a joined pod
  # without leaving strands a LIVE member row on every peer: gossip keeps
  # re-learning a node that no longer exists, the fan-out keeps trying it for
  # three seconds a query, and the next rental — a fresh identity, same name
  # shape — accumulates beside it. `leave` stamps a `removed_at` tombstone and
  # pushes it to online peers before we go (gossip.rs:1003-1056), which is the
  # only moment the pod can still speak.
  #
  # Best-effort by design: a pod that is already unreachable cannot leave, and
  # refusing to destroy it would leave it BILLING. So a failed leave is
  # reported loudly and teardown continues — the repair is
  # `svrn mesh forget-member <node>` on this side.
  if [ "$mode" = mesh ]; then
    echo "[dev-pod] mesh pod — leaving $HOME_MESH before destroying"
    before=$(home_pod_members)
    if url=$(vastai ssh-url "$id" 2>/dev/null); then
      hostport="${url#ssh://root@}"
      code=$(timeout 40 ssh -n -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10 \
             -p "${hostport##*:}" "root@${hostport%%:*}" \
             "curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:9741/v1/mesh/leave" \
             2>/dev/null || echo 000)
      case "$code" in
        200|204) echo "[dev-pod] pod acknowledged leave (HTTP $code); waiting for the tombstone to land" ;;
        *)       echo "[dev-pod] WARNING: leave did not succeed (HTTP $code) — destroying anyway." >&2
                 echo "[dev-pod] If a vast-* member lingers: svrn mesh forget-member <node>" >&2 ;;
      esac
    else
      echo "[dev-pod] WARNING: no ssh url for $id — cannot leave; destroying anyway." >&2
    fi
    sleep 6
    after=$(home_pod_members)
    if [ -n "$before" ] && [ "$before" = "$after" ]; then
      echo "[dev-pod] WARNING: the founder still lists the same rented members after the leave:" >&2
      printf '%s\n' "$after" | sed 's/^/  /' >&2
    fi
  fi

  vastai destroy instance "$id" -y
  sleep 3
  # Report what IS, not what was asked for: confirm it is actually gone.
  if resolve_row "" >/dev/null 2>&1; then
    echo "[dev-pod] WARNING: an instance labelled $LABEL_PREFIX* is STILL LISTED -- check 'vastai show instances'" >&2
    exit 1
  fi
  echo "[dev-pod] $id destroyed; no instance labelled $LABEL_PREFIX* remains (billing stopped)"
  [ "$mode" = mesh ] && echo "[dev-pod] the invite went to a third party; rotating it is cheap: svrn daemon stop && svrn mesh rotate && svrn daemon start"
  true
  ;;
status)
  # "Is anything costing me money right now, and how much so far?"
  vastai show instances --raw 2>/dev/null | LABEL_PREFIX="$LABEL_PREFIX" python3 -c '
import sys, json, os, time
prefix = os.environ["LABEL_PREFIX"]
try: rows = json.load(sys.stdin)
except Exception: rows = []
mine = [r for r in rows if (r.get("label") or "").startswith(prefix)]
if not mine:
    print("no dev pod running (nothing billing)"); raise SystemExit(0)
for r in mine:
    dph = r.get("dph_total")
    start = r.get("start_date")
    # Age and cost are DERIVED from start_date. If Vast did not send it, say so
    # rather than printing a plausible zero (ARCH §18.3).
    if start:
        mins = (time.time() - float(start)) / 60
        age = "%dm%02ds" % (mins, (mins * 60) % 60)
        cost = "$%.2f" % (mins / 60 * dph) if dph else "unknown"
    else:
        age, cost = "unknown (no start_date)", "unknown (no start_date)"
    print("  id       %s" % r["id"])
    print("  mode     %s" % ("mesh" if (r.get("label") or "").endswith("-mesh") else "solo"))
    print("  status   %s / %s" % (r.get("actual_status"), r.get("intended_status")))
    print("  gpu      %s x%s" % (r.get("gpu_name"), r.get("num_gpus")))
    print("  rate     %s" % ("$%.4f/hr" % dph if dph else "unknown"))
    print("  age      %s" % age)
    print("  spent    %s   (billing stops only on: dev-pod.sh down)" % cost)
    msg = (r.get("status_msg") or "").strip()
    if msg: print("  last     %s" % msg.splitlines()[-1][:100])
'
  ;;
*) sed -n '2,18p' "$0"; exit 2 ;;
esac
