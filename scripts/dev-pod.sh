#!/usr/bin/env bash
# dev-pod.sh — rent a Vast GPU and boot a sovereign daemon carrying the
# RuggedFox loadout (Qwen3.8-27B primary + Qwen3.5-4B-MTP fast), reachable
# on localhost through an SSH tunnel. Destroy it when the burst is over.
#
#   ./dev-pod.sh offers                 # live 48GB-class offers, cheapest first
#   ./dev-pod.sh up <offer-id>          # rent + boot (prints the instance id)
#   ./dev-pod.sh logs <instance-id>     # watch the boot (model pull + slot load)
#   ./dev-pod.sh tunnel <instance-id>   # forward pod :9741 -> local :9841
#   ./dev-pod.sh down <instance-id>     # destroy (billing stops only on destroy)
#
# IMAGE. Defaults to $SOVEREIGN_VAST_IMAGE. ghcr.io/alexsbryan/sovereign-cuda
# :latest was rebuilt 2026-08-28 from commit 1e7b66866 — it carries the
# vendored llama.cpp (including the four CUDA sources VENDOR_EXCLUDE used to
# omit, without which the image could not compile ggml-cuda at all) and
# openssh-server + aria2 baked into the runtime stage. The daemon logs its
# SOVEREIGN_GIT_SHA at startup, so check that against the commit you expect
# before trusting a boot. Whether the 27B LOADS on CUDA is still unproven.
set -euo pipefail

IMAGE="${SOVEREIGN_VAST_IMAGE:-ghcr.io/alexsbryan/sovereign-cuda:latest}"
DISK="${DISK:-80}"
LOCAL_PORT="${LOCAL_PORT:-9841}"
CTX="${CTX:-32768}"
LABEL="sovereign-dev-daemon"
# The operator's own mesh. `check` asserts the pod is NOT in it. Override if
# this host's mesh is named something else (`svrn mesh status`).
HOME_MESH="${HOME_MESH:-Meshsonics}"

# ── model loadout ────────────────────────────────────────────────────────────
# name | expected bytes | url.  Sizes are the gate: a truncated pull must fail
# loudly, not boot a daemon on half a GGUF.
#   27B  : HF's CURRENT revision, 25,299,061,664 — NOT byte-identical to the
#          local /sovereign/models copy (25,924,152,384, an earlier unsloth
#          re-quant). Named substitution: fine for dev, NOT for judge parity.
#   4B   : 4,261,908,800 — byte-exact match for the local
#          Qwen3.5-4B-UD-MTP-Q6_K_XL.gguf (local name is a rename).
#   embed: 639,150,592 — byte-exact match for the local copy.
read -r -d '' LOADOUT <<'MODELS' || true
Qwen3.8-27B-UD-Q6_K_XL.gguf 25299061664 https://huggingface.co/unsloth/Qwen3.8-27B-GGUF/resolve/main/Qwen3.8-27B-UD-Q6_K_XL.gguf
Qwen3.5-4B-UD-Q6_K_XL.gguf 4261908800 https://huggingface.co/unsloth/Qwen3.5-4B-MTP-GGUF/resolve/main/Qwen3.5-4B-UD-Q6_K_XL.gguf
Qwen3-Embedding-0.6B-Q8_0.gguf 639150592 https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF/resolve/main/Qwen3-Embedding-0.6B-Q8_0.gguf
MODELS

onstart_script() {
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
$(printf '%s\n' "$LOADOUT" | awk '{print $3 "\n  out=" $1}')
URLS
echo "[dev-pod] pulling loadout (~30.2 GB) from HuggingFace"
time aria2c -x 8 -s 8 -j 3 --continue=true --auto-file-renaming=false \\
     --summary-interval=15 -i /tmp/urls

fail=0
$(printf '%s\n' "$LOADOUT" | awk '{printf "have=$(stat -c %%s %s 2>/dev/null || echo 0); [ \"$have\" = \"%s\" ] || { echo \"[dev-pod] FATAL: %s is \$have bytes, expected %s\"; fail=1; }\n", $1, $2, $1, $2}')
[ "\$fail" = 0 ] || { echo "[dev-pod] refusing to boot on an incomplete loadout"; exit 1; }

cat > /root/.svrnmesh/config.toml <<'CFG'
[models]
primary = "/workspace/models/Qwen3.8-27B-UD-Q6_K_XL.gguf"
fast    = "/workspace/models/Qwen3.5-4B-UD-Q6_K_XL.gguf"
embed   = "/workspace/models/Qwen3-Embedding-0.6B-Q8_0.gguf"
context_size = $CTX

# This daemon is a SOLO island. It never joins a mesh (no \`mesh join\` above),
# and mDNS is off because a Vast box is a SHARED machine — leaving discovery on
# would let it see co-tenant containers on the same LAN segment.
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
echo "[dev-pod] launching daemon on 127.0.0.1:9741 — tunnel in to reach it"
exec sovereign-cli daemon run 2>&1 | tee -a /workspace/daemon.log
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

offer_rows() {
  vastai search offers \
    'gpu_ram>=46 num_gpus=1 rentable=true disk_space>=80 inet_down>=1500 dph<1.30 reliability>0.97' \
    -o dph --raw
}

# ── Resolving WHICH pod, without making you carry a number ───────────────────
#
# The label is the source of truth, not a local state file (§10.6: one decider).
# A file in /tmp dies on reboot, a file in the repo is one more thing to sync,
# and either can disagree with Vast. `vastai show instances` cannot — it IS the
# billing surface. So every verb takes an OPTIONAL id and falls back to "the
# one instance labelled $LABEL".
#
# Refuses on ambiguity rather than guessing: two dev pods and no id named is a
# question, not a default (ARCH §18.3 — never silently substitute).
resolve_id() {
  if [ -n "${1:-}" ]; then printf '%s' "$1"; return 0; fi
  vastai show instances --raw 2>/dev/null | LABEL="$LABEL" python3 -c '
import sys, json, os
label = os.environ["LABEL"]
try: rows = json.load(sys.stdin)
except Exception: rows = []
mine = [r for r in rows if (r.get("label") or "") == label]
if len(mine) == 1:
    print(mine[0]["id"]); raise SystemExit(0)
if not mine:
    print(f"no instance labelled {label} — nothing to act on", file=sys.stderr)
else:
    ids = ", ".join(str(r["id"]) for r in mine)
    print(f"{len(mine)} instances labelled {label} ({ids}) — name one explicitly", file=sys.stderr)
raise SystemExit(1)
'
}

case "${1:-}" in
offers)
  offer_rows | SUPPORTED_GPUS="$SUPPORTED_GPUS" python3 -c '
import sys, json, os, re
rows = json.load(sys.stdin)
print("%10s %-24s %6s %8s %8s %6s  %-3s %s" % ("offer","gpu","vram","$/hr","down","rel","","geo"))
for r in rows[:15]:
    ok = bool(re.search(os.environ["SUPPORTED_GPUS"], r["gpu_name"]))
    print("%10s %-24s %5.0fG %8.3f %7.0fM %6.3f  %-3s %s" % (
        r["id"], r["gpu_name"][:24], r["gpu_ram"]/1024, r["dph_total"],
        r.get("inet_down") or 0, r.get("reliability2") or 0,
        "" if ok else "SKIP",
        (r.get("geolocation") or "?")[:24]))
print("\nSKIP = outside the compiled CUDA archs; auto-pick refuses these.", file=sys.stderr)'
  ;;
up)
  # An offer id is OPTIONAL and usually the wrong thing to type: offers churn
  # within hours (45241992 was live at 03:50 and gone by 21:24 the same day),
  # so a hand-copied id is stale by the time it is pasted and `create` fails.
  # With no argument, take the cheapest offer that passes the ARCHITECTURE
  # guard -- never merely the cheapest.
  offer="${2:-}"
  if [ -z "$offer" ]; then
    offer=$(offer_rows | SUPPORTED_GPUS="$SUPPORTED_GPUS" python3 -c '
import sys, json, re, os
rows = json.load(sys.stdin)
ok = [r for r in rows if re.search(os.environ["SUPPORTED_GPUS"], r["gpu_name"])]
if not ok:
    print("no eligible offer: nothing matching the compiled CUDA archs is rentable right now", file=sys.stderr)
    raise SystemExit(1)
best = min(ok, key=lambda r: r["dph_total"])
print("%s %s %.3f %s" % (best["id"], best["gpu_name"].replace(" ","_"), best["dph_total"], best.get("geolocation","?").replace(" ","_")), file=sys.stderr)
print(best["id"])
') || exit 1
  fi
  # Everything conversational goes to stderr; stdout carries the id ALONE, so
  # `id=$(dev-pod.sh up)` works.
  echo "[dev-pod] offer $offer  image: $IMAGE  disk: ${DISK}G  ctx: $CTX" >&2
  boot=$(mktemp /tmp/dev-pod-boot.XXXX.sh); onstart_script > "$boot"
  bash -n "$boot" || { echo "[dev-pod] refusing to rent: boot script does not parse"; exit 1; }
  # --raw so the instance id is CAPTURED, not eyeballed out of a printed dict.
  # Every other verb can then be run with no arguments at all.
  id=$(vastai create instance "$offer" \
    --image "$IMAGE" --disk "$DISK" --label "$LABEL" \
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
  # Which daemon is on each port, and is the pod contaminated? The loadout is
  # the SAME on both, so model ids do not discriminate — mesh membership does.
  #
  # THE PREDICATE IS "NOT $HOME_MESH", NOT "no mesh". This asserted
  # mesh=(none) until 2026-08-29 and could never have passed: a fresh daemon
  # AUTO-CREATES its own single-node mesh ("<hostname>'"'"'s Mesh",
  # members_total=1), measured on the first flight. Isolation was holding the
  # whole time; the check was just unsatisfiable, which is worse than no check
  # because it reads as a real failure (ARCH §18.1).
  #
  # What actually matters: the pod must not be a member of the OPERATOR'"'"'s
  # mesh, and must not be running peer inference for anyone.
  for port in 9741 "$LOCAL_PORT"; do
    printf "%5s  " "$port"
    timeout 6 curl -s "http://127.0.0.1:$port/v1/mesh/status" 2>/dev/null \
      | HOME_MESH="$HOME_MESH" python3 -c '
import sys, json, os
home = os.environ.get("HOME_MESH", "")
try: d = json.load(sys.stdin)
except Exception: raise SystemExit(1)
name = d.get("mesh_name") or "(none)"
total = d.get("members_total")
solo = (name != home) and (total in (0, 1, None))
verdict = "HOME" if name == home else ("solo island" if solo else "!! JOINED SOMETHING !!")
print("mesh=%-28s members=%s/%s peer_inflight=%s/%s  %s" % (
    name, d.get("members_online"), total,
    d.get("peer_inflight_current"), d.get("peer_inflight_ceiling"), verdict))' \
      2>/dev/null || echo "no answer (tunnel down?)"
  done
  true
  ;;
tunnel)
  id=$(resolve_id "${2:-}") || exit 1
  url=$(vastai ssh-url "$id")                         # ssh://root@host:port
  hostport="${url#ssh://root@}"
  echo "[dev-pod] http://127.0.0.1:$LOCAL_PORT  ->  pod 127.0.0.1:9741   (ctrl-c to drop)"
  exec ssh -N -o StrictHostKeyChecking=accept-new \
       -L "$LOCAL_PORT:127.0.0.1:9741" \
       -p "${hostport##*:}" "root@${hostport%%:*}"
  ;;
down)
  # -y IS LOAD-BEARING. Without it vastai prompts "Are you sure? [y/N]" and,
  # with no tty answer, prints "Aborted." and exits — leaving the pod RUNNING
  # AND BILLING while the command looks like it did something. Observed
  # 2026-08-29 on the first flight. Billing stops on destroy and nothing else,
  # so a teardown that no-ops on the happy path is the worst bug this script
  # could carry.
  id=$(resolve_id "${2:-}") || exit 1
  vastai destroy instance "$id" -y
  sleep 3
  # Report what IS, not what was asked for: confirm it is actually gone.
  if resolve_id "" >/dev/null 2>&1; then
    echo "[dev-pod] WARNING: an instance labelled $LABEL is STILL LISTED -- check 'vastai show instances'" >&2
    exit 1
  fi
  echo "[dev-pod] $id destroyed; no instance labelled $LABEL remains (billing stopped)"
  ;;
status)
  # "Is anything costing me money right now, and how much so far?"
  vastai show instances --raw 2>/dev/null | LABEL="$LABEL" python3 -c '
import sys, json, os, time
label = os.environ["LABEL"]
try: rows = json.load(sys.stdin)
except Exception: rows = []
mine = [r for r in rows if (r.get("label") or "") == label]
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
    print("  status   %s / %s" % (r.get("actual_status"), r.get("intended_status")))
    print("  gpu      %s x%s" % (r.get("gpu_name"), r.get("num_gpus")))
    print("  rate     %s" % ("$%.4f/hr" % dph if dph else "unknown"))
    print("  age      %s" % age)
    print("  spent    %s   (billing stops only on: dev-pod.sh down)" % cost)
    msg = (r.get("status_msg") or "").strip()
    if msg: print("  last     %s" % msg.splitlines()[-1][:100])
'
  ;;
*) sed -n '2,12p' "$0"; exit 2 ;;
esac
