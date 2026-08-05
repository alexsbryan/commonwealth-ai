#!/usr/bin/env bash
# Rent, provision, probe and destroy a training pod on Vast. RUNS LOCALLY.
#
#   cloud/pod.sh up --gpu RTX_PRO_6000_WS      # search, rent, provision
#   cloud/pod.sh sync  <id>                    # push scripts + data
#   cloud/pod.sh probe <id>                    # preflight + the 25-step probe
#   cloud/pod.sh fetch <id>                    # pull the run dir back
#   cloud/pod.sh down  <id>                    # destroy + close the ledger row
#   cloud/pod.sh list                          # what is running and what it costs
#
# WHY THIS IS A SHELL SCRIPT AND NOT A `pipeline pod` SUBCOMMAND (yet).
# `sovereign pipeline pod up` builds an EPHEMERAL INFERENCE WORKER: it mints a
# bootstrap blob, boots our sovereign-cuda image whose entrypoint ends in
# `daemon run --worker-mode`, and drives a job protocol whose only reverse flow
# is JSON unit results. A training pod needs a PyTorch image, an SSH session,
# and a way to bring FILES back — none of which that path has. Building the
# Rust surface first would mean guessing the shape; this script IS the spec for
# it, and Phase 2 lifts it once the probe has proven what the shape actually is.
#
# WHAT IS REUSED ANYWAY: the cost ledger. Rows land in
# ~/.sovereign/pipeline-pods.json in the exact schema sovereign-pipeline reads,
# so `sovereign pipeline pod list` shows a training pod's accruing cost next to
# every other pod. Forgetting a running pod is the real money risk here, and it
# is not worth a second, private accounting of it (§10.6).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(dirname "$HERE")"
# Not SOVEREIGN_LEDGER: the CLI bridges and deprecation-warns on SOVEREIGN_*
# now, so a var in that namespace reads as a stale mesh setting rather than a
# test hook.
LEDGER="${VERIFIER_LEDGER:-$HOME/.sovereign/pipeline-pods.json}"
RECIPE_ID="verifier-v0-probe"
IMAGE="${VERIFIER_TRAIN_IMAGE:-pytorch/pytorch:2.10.0-cuda12.8-cudnn9-devel}"
DISK_GB="${DISK_GB:-120}"
# ONE floor, shared with preflight.py's --vram-floor-gb (§10.6). Overriding it
# here without overriding it there re-creates the two-deciders bug this replaced.
VRAM_FLOOR_GB="${VRAM_FLOOR_GB:-46}"
REMOTE_WORK="/workspace"
REMOTE_REPO="$REMOTE_WORK/verifier"
REMOTE_ENV="$REMOTE_WORK/train-env"

die() { echo "FATAL: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null || die "$1 not on PATH"; }

# --------------------------------------------------------------------------
# ledger — same file and schema as sovereign-pipeline's ledger.rs
# --------------------------------------------------------------------------
ledger_append() { # vast_id label gpu image cost_per_hour
  python3 - "$LEDGER" "$1" "$2" "$RECIPE_ID" "$3" "$4" "$5" <<'PY'
import json, os, sys, tempfile, time
path, vid, label, recipe, gpu, image, cph = sys.argv[1:8]
os.makedirs(os.path.dirname(path), exist_ok=True)
try:
    with open(path) as fh: doc = json.load(fh)
except (OSError, ValueError):
    doc = {"pods": []}
pods = doc.setdefault("pods", [])
if any(p.get("vast_id") == vid and p.get("status") == "running" for p in pods):
    print(f"  ledger: {vid} already recorded as running"); sys.exit(0)
pods.append({"vast_id": vid, "label": label, "recipe_id": recipe,
             "gpu_name": gpu, "image": image, "started_at": int(time.time()),
             "ended_at": None, "cost_per_hour": float(cph), "status": "running"})
# Atomic, matching ledger.rs's write_atomic: a torn ledger loses the record of
# a pod that is still costing money.
fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), suffix=".tmp")
with os.fdopen(fd, "w") as fh:
    json.dump(doc, fh, indent=2); fh.flush(); os.fsync(fh.fileno())
os.replace(tmp, path)
print(f"  ledger: recorded {vid} at ${float(cph):.3f}/hr")
PY
}

ledger_close() { # vast_id
  python3 - "$LEDGER" "$1" <<'PY'
import json, os, sys, tempfile, time
path, vid = sys.argv[1:3]
try:
    with open(path) as fh: doc = json.load(fh)
except (OSError, ValueError):
    print("  ledger: no ledger file"); sys.exit(0)
now = int(time.time())
for p in doc.get("pods", []):
    if p.get("vast_id") == vid and p.get("status") == "running":
        p["status"] = "closed"; p["ended_at"] = now
        hrs = (now - p["started_at"]) / 3600.0
        print(f"  ledger: closed {vid} after {hrs:.2f}h "
              f"= ${hrs * p['cost_per_hour']:.2f}")
        break
else:
    print(f"  ledger: {vid} not open in the ledger")
fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), suffix=".tmp")
with os.fdopen(fd, "w") as fh:
    json.dump(doc, fh, indent=2); fh.flush(); os.fsync(fh.fileno())
os.replace(tmp, path)
PY
}

# --------------------------------------------------------------------------
ssh_target() { # vast_id -> "-p PORT root@HOST"
  local url
  url=$(vastai ssh-url "$1" 2>/dev/null) || die "no ssh-url for $1 (still booting?)"
  # ssh://root@host:port
  local hostport=${url#ssh://}
  echo "-p ${hostport##*:} ${hostport%:*}"
}

# Pin the identity. The account key Vast injects is ~/.ssh/id_ed25519 (key id
# 1147518); without IdentitiesOnly, ssh offers every key it can find and an
# agent holding several will hit "Too many authentication failures" before
# reaching the right one — which presents identically to a key that was never
# installed, and that ambiguity already cost one debugging round.
SSH_KEY="${VERIFIER_SSH_KEY:-$HOME/.ssh/id_ed25519}"
SSH_OPTS=(-i "$SSH_KEY" -o IdentitiesOnly=yes
          -o StrictHostKeyChecking=accept-new -o ConnectTimeout=20)

pod_ssh() { local id=$1; shift; read -ra T <<<"$(ssh_target "$id")"
  ssh "${SSH_OPTS[@]}" "${T[@]}" "$@"; }

# --------------------------------------------------------------------------
cmd_up() {
  need vastai; need python3
  local gpu="RTX_PRO_6000_WS" maxp="1.10" label="" vram_floor="$VRAM_FLOOR_GB"
  local skip_machines="${SKIP_MACHINES:-}"
  while [ $# -gt 0 ]; do case $1 in
    --gpu) gpu=$2; shift 2 ;;
    --max-price) maxp=$2; shift 2 ;;
    --label) label=$2; shift 2 ;;
    --vram-floor-gb) vram_floor=$2; shift 2 ;;
    # A MACHINE THAT REFUSES THE KEY IS A PERMANENT LOSS UNTIL YOU CAN SKIP IT.
    # The ranking below is DETERMINISTIC, so re-running `up` after a failure
    # rents the SAME machine and fails the same way — measured 2026-08-05 on
    # machine 51579, twice, ~$0.10 and 20 minutes. `vastai attach ssh` answers
    # "SSH key already associated" while sshd keeps refusing, so there is no
    # repair from this side; the only move is to pick a different machine.
    # Comma-separated MACHINE ids (not offer ids — offers are re-minted per
    # search, machines persist), or set SKIP_MACHINES in the environment.
    --skip-machines) skip_machines=$2; shift 2 ;;
    *) die "unknown flag $1" ;;
  esac; done
  label=${label:-verifier-probe-$(echo "$gpu" | tr 'A-Z_' 'a-z-')}

  # Mirrors pipeline_cmd.rs's query, minus direct_port_count (we need SSH, not
  # a mapped worker port) and plus the VRAM floor the 4B actually measured.
  # cuda_max_good>=12.8 is the sm_120 floor — it is what keeps a Blackwell box
  # from being rented with a driver that cannot run the wheels we pinned.
  #
  # VRAM_FLOOR_GB IS ONE DECIDER SHARED WITH preflight.py (§10.6). It was a
  # hardcoded 60 here against preflight's 52 — two numbers for one constraint,
  # and the search one silently dominated: preflight could never fail a card the
  # search had already refused to rent. Lowering only preflight would have
  # changed nothing. Keep these equal; `--vram-floor-gb` moves both.
  # 46 is "35.9 GiB of measured demand plus room for fragmentation" and is
  # probably TOO HIGH: the 44-45 GiB aborts it was set to refuse turned out to
  # be our own tripwire firing on its allocator cache, not OOMs. One completed
  # run on a 44-45 GiB card under the fixed guard lowers it — see
  # check_vram_floor() in preflight.py for the full provenance.
  #
  # VAST'S `gpu_ram` DOES NOT PREDICT torch's `total_memory`, AND THE ERROR IS
  # PER-CARD, NOT A UNIT CONVERSION. Measured on real pods, same day:
  #     RTX A6000     vast 48.0 -> torch 44.43 GiB   (-7.4%)
  #     RTX PRO 5000  vast 47.8 -> torch 47.27 GiB   (-1.1%)
  #     A100 SXM4     vast 80.0 -> torch 79.25 GiB   (-0.9%)
  # The A6000 is the outlier because Quadro-class parts reserve ~6.25% of VRAM
  # for ECC; the others do not. A GiB/GB conversion was tried and FALSIFIED by
  # the PRO 5000 — it would have demanded >=49.4 and excluded the one cheap card
  # that actually passes.
  # So the search floor stays a COARSE PRE-FILTER in Vast's own units, and
  # preflight.py on the pod is the AUTHORITATIVE gate. That split is deliberate:
  # only the pod can ask torch. Renting a card preflight then refuses costs
  # ~$0.02 (measured); trusting the listing and training on it costs the run.
  local query="gpu_name=$gpu num_gpus=1 rentable=true verified=true \
reliability>=0.95 cuda_max_good>=12.8 gpu_ram>=$vram_floor disk_space>=$DISK_GB \
inet_down>=200 dph_total<=$maxp"
  echo "searching: $query"
  local offer
  [ -n "$skip_machines" ] && echo "  skipping machines: $skip_machines"
  offer=$(vastai search offers "$query" -o 'dph+' --raw \
    | python3 -c "
import json,sys
o=json.load(sys.stdin)
if not o: sys.exit('no offers matched — raise --max-price or try --gpu A100_SXM4')
# SKIP FIRST, so the count in the refusal below is the count you can actually
# rent. Reported OUT LOUD rather than silently narrowing the pool (§18.3): a
# search that quietly dropped your only cheap machine looks identical to a
# market with no cheap machines.
skip={s.strip() for s in '$skip_machines'.split(',') if s.strip()}
if skip:
    kept=[x for x in o if str(x.get('machine_id')) not in skip]
    print(f'  {len(o)-len(kept)} of {len(o)} offers skipped by machine id',
          file=sys.stderr)
    o=kept
    if not o: sys.exit('every matching offer is on a skipped machine')
# Same ranking as pod.rs::pick_offer: verified, then reliability, then price.
o.sort(key=lambda x: (not (x.get('verification')=='verified'),
                      -x.get('reliability2',0), x['dph_total']))
b=o[0]
# Underscore EVERY field that can contain a space. gpu_name is 'A100 SXM4',
# which silently shifted every later field by one when this was read with a
# bare \`read -r\` — the price stayed right (it is field 2) but gpu_name reached
# the ledger as 'A100' and reliability printed as the VRAM. A whitespace-
# delimited protocol has to guarantee its own delimiters.
print(b['id'], f\"{b['dph_total']:.4f}\", b['gpu_name'].replace(' ','_'),
      f\"{b['gpu_ram']/1024:.0f}\", f\"{b.get('reliability2',0):.3f}\",
      str(b.get('geolocation','?')).replace(' ','_'))
") || die "offer search failed"
  read -r oid price gname vram rel loc <<<"$offer"
  echo "picked offer $oid: $gname ${vram}GB \$$price/hr rel=$rel $loc"

  local created
  # WE INSTALL OUR OWN KEY. Vast's account-level SSH key association is not
  # reliable here: key 1147518 is on the account, `vastai attach ssh <id>`
  # answers "SSH key already associated with instance", and sshd in the
  # container still refuses it — verified twice, on two instances, over both
  # the direct port and the ssh<N>.vast.ai proxy, with `ssh -v` showing the
  # correct key Offered and rejected. Whatever writes authorized_keys on their
  # side did not run.
  #
  # onstart is a channel we control, it runs in the container, and sshd runs
  # independently of it (a `sleep infinity` onstart still produced an OpenSSH
  # banner). So appending the key here removes the dependency entirely rather
  # than retrying an opaque association and hoping.
  #
  # The pubkey has no single quotes, so single-quoting the echo is safe.
  local pubkey; pubkey=$(cat "$SSH_KEY.pub") || die "no pubkey at $SSH_KEY.pub"
  local onstart="mkdir -p /root/.ssh; echo '$pubkey' >> /root/.ssh/authorized_keys; \
chmod 700 /root/.ssh; chmod 600 /root/.ssh/authorized_keys; \
echo verifier-onstart-ok > /root/.verifier-onstart"
  # --cancel-unavail matches pod.rs: fail loudly rather than leave a STOPPED
  # instance sitting in the account that nobody is watching.
  created=$(vastai create instance "$oid" --image "$IMAGE" --disk "$DISK_GB" \
      --onstart-cmd "$onstart" --label "$label" --ssh --direct \
      --cancel-unavail --raw) || die "create failed"
  local vid
  vid=$(echo "$created" | python3 -c "
import json,sys
d=json.load(sys.stdin)
if d.get('success') is False: sys.exit(d.get('error','create rejected'))
print(d.get('new_contract') or d.get('id'))")
  echo "instance $vid created (\$$price/hr)"
  ledger_append "$vid" "$label" "$gname" "$IMAGE" "$price"

  # WAIT ON THE REAL READINESS CONDITION, AND NAME WHICH STATE WE ARE IN.
  # A bare retry loop over `ssh true` cannot tell these apart, and the
  # difference is the entire diagnosis:
  #   ports {} / "Connection refused"  -> the container is not up yet. WAIT.
  #   ports mapped / "Permission denied (publickey)" -> sshd IS up. But this is
  #        NOT immediately fatal: Vast runs its own apt-based provisioning
  #        INSIDE the container after the image lands (observed live:
  #        status_msg cycling through "Setting up python3-cryptography"), and
  #        sshd can be listening before the key is installed. So a denial is
  #        tolerated for a bounded window and only then treated as auth.
  # Conflating these cost a full debugging round on 2026-08-04 — a "denied"
  # was read first as "still booting", then as a Vast-side gateway fault.
  # Reporting WHICH state we are in, and for how long, is the whole fix.
  echo "waiting for the container, then for sshd ..."
  local ready=0 denied=0
  local denied_max=${SSH_DENIED_TOLERANCE:-18}   # x10s = 3 min of denials
  for i in $(seq 1 90); do
    local j st ports
    j=$(vastai show instance "$vid" --raw 2>/dev/null) || true
    st=$(printf '%s' "$j" | python3 -c "
import json,sys
try: d=json.load(sys.stdin); print(d.get('actual_status') or 'unknown')
except Exception: print('unknown')" 2>/dev/null)
    ports=$(printf '%s' "$j" | python3 -c "
import json,sys
try: d=json.load(sys.stdin); print(len(d.get('ports') or {}))
except Exception: print(0)" 2>/dev/null)
    if [ "$st" = "running" ] && [ "${ports:-0}" -gt 0 ]; then
      local err
      err=$(pod_ssh "$vid" true 2>&1) && { ready=1; echo "  ssh up after $((i*10))s"; break; }
      case "$err" in
        *"Permission denied"*)
          denied=$((denied + 1))
          [ "$denied" = 1 ] && echo "  sshd up, key refused — tolerating for \
$((denied_max * 10))s while Vast finishes in-container provisioning"
          if [ "$denied" -ge "$denied_max" ]; then
            die "sshd REFUSED the key for $((denied * 10))s straight — this is \
AUTH, not boot, and waiting will not fix it. Check, in order: the key is \
registered (\`vastai show ssh-keys\`); it matches $SSH_KEY.pub; and it was \
registered BEFORE this instance was created — a key added to the account \
afterwards is not installed into an already-running container, which is the \
trap that cost a session on 2026-08-04. Then: cloud/pod.sh down $vid"
          fi ;;
      esac
    fi
    [ "$i" = 90 ] && die "container never became reachable in 15 min \
(last status=$st ports=${ports:-0}) — 'cloud/pod.sh down $vid' and retry"
    sleep 10
  done
  [ "$ready" = 1 ] || die "ssh never came up"
  echo
  echo "next:  cloud/pod.sh sync $vid"
}

cmd_sync() {
  local id=${1:?usage: pod.sh sync <vast-id>}
  local data=${DATA_DIR:-$REPO_DIR/data/orpo-76k}
  read -ra T <<<"$(ssh_target "$id")"
  local rsh="ssh ${SSH_OPTS[*]} ${T[0]} ${T[1]}"
  local host=${T[2]}
  # rsync must exist on BOTH ends. The pytorch base images ship neither rsync
  # nor much else beyond conda + torch, and rsync's failure mode over ssh is
  # the unhelpful "bash: rsync: command not found" followed by a protocol
  # error — which reads as a broken transport rather than a missing package.
  pod_ssh "$id" "mkdir -p $REMOTE_REPO/scripts $REMOTE_REPO/cloud $REMOTE_REPO/data; \
     command -v rsync >/dev/null || { \
       echo 'installing rsync on the pod'; \
       apt-get update -qq && apt-get install -y -qq rsync; }" \
    || die "could not prepare the pod for sync"

  echo "--- code ---"
  rsync -az --info=stats1 -e "$rsh" \
    "$REPO_DIR/scripts/" "$host:$REMOTE_REPO/scripts/"
  rsync -az --info=stats1 -e "$rsh" \
    "$HERE/" "$host:$REMOTE_REPO/cloud/"

  # -t IS LOAD-BEARING, not cosmetic. train_orpo_trl.py caches per-row token
  # lengths under a key of (train.jsonl size, mtime). Without preserved mtimes
  # the cache misses and the pod re-tokenizes 74,674 rows — ~2 minutes of paid
  # time, and worse, an unexplained gap before step 1 that looks like a hang.
  echo "--- data (mtimes preserved for the length cache) ---"
  rsync -azt --info=stats1 -e "$rsh" \
    "$data/" "$host:$REMOTE_REPO/data/$(basename "$data")/"

  echo
  echo "next:  cloud/pod.sh probe $id"
}

cmd_provision() {
  local id=${1:?usage: pod.sh provision <vast-id>}
  # VRAM_FLOOR_GB crosses the ssh boundary explicitly: `ssh` does not carry the
  # caller's environment, so without this the pod's preflight would silently use
  # its own default while the search used ours — the two-deciders bug again,
  # just distributed.
  pod_ssh "$id" "VRAM_FLOOR_GB=$VRAM_FLOOR_GB bash $REMOTE_REPO/cloud/provision.sh 2>&1" | tee \
    "$REPO_DIR/cloud/.last-provision-$id.log"
}

cmd_probe() {
  local id=${1:?usage: pod.sh probe <vast-id>}
  local iters=${ITERS:-25} micro=${MICRO:-1} accum=${ACCUM:-32}
  # THE RUN NAME MUST IDENTIFY THE HARDWARE, NOT JUST THE RECIPE. It was
  # `probe-4b-cloud-m$micro`, identical for every card — and `fetch` rsyncs into
  # a flat `runs/`, so probing a second GPU SILENTLY OVERWROTE the first one's
  # summary.json, steps.jsonl, adapter and train.log. Lost the A100's raw run
  # dir to exactly this on 2026-08-05; the numbers survived only because they
  # were already in a note. A comparison harness whose two arms collide on one
  # path cannot compare anything (§7.5: identity from essence).
  # gpu_name comes from the ledger row this pod already wrote; the vast id is
  # appended because it is unique by construction even if the lookup misses.
  local gpu_slug
  gpu_slug=$(python3 -c "
import json,sys
try: pods=json.load(open('$LEDGER')).get('pods',[])
except Exception: pods=[]
m=[p for p in pods if p.get('vast_id')=='$id']
n=(m[-1].get('gpu_name') if m else '') or 'gpu'
print(''.join(c if c.isalnum() else '-' for c in n.lower()).strip('-'))
" 2>/dev/null) || gpu_slug=gpu
  local name=${NAME:-probe-4b-m$micro-$gpu_slug-$id}
  local data=$(basename "${DATA_DIR:-orpo-76k}")

  # THE FULL GATE, with payload, immediately before spending. Separate from
  # provision's machine-only pass because the data arrives in between, and a
  # missing/half-synced dataset is discovered here for free rather than after
  # the model loads.
  echo "=== preflight (with payload) ==="
  pod_ssh "$id" "cd $REMOTE_REPO && python cloud/preflight.py \
      --data data/$data --model $REMOTE_ENV/models/Qwen3.5-4B \
      --vram-floor-gb $VRAM_FLOOR_GB \
      --json $REMOTE_ENV/runs/preflight-$name.json" \
    || die "preflight UNFIT — not starting a paid run. Fix it and re-probe."

  # IDENTICAL to the Halo's run_4b_sweep.sh arm: same ARM, same seq len, same
  # LR/LoRA/beta (fixed inside launch_arm.sh), same seed, same bucketing, same
  # effective batch. Only the accelerator differs — which is the entire point.
  echo "=== probe: $iters steps, micro $micro x accum $accum ==="
  pod_ssh "$id" "cd $REMOTE_REPO && \
      REPO_DIR=$REMOTE_REPO TRAIN_ENV=$REMOTE_ENV PY=python \
      ARM=A ITERS=$iters MICRO=$micro ACCUM=$accum \
      MODEL=$REMOTE_ENV/models/Qwen3.5-4B \
      OUT=$REMOTE_ENV/runs/$name \
      SAVE_EVERY=1000 \
      bash scripts/launch_arm.sh; \
      echo LAUNCH_RC=\$?; tail -c 4000 $REMOTE_ENV/runs/$name/train.log"
  echo
  echo "next:  cloud/pod.sh fetch $id   then   cloud/pod.sh down $id"
}

cmd_fetch() {
  local id=${1:?usage: pod.sh fetch <vast-id>}
  read -ra T <<<"$(ssh_target "$id")"
  local rsh="ssh ${SSH_OPTS[*]} ${T[0]} ${T[1]}"
  local dest=${FETCH_DEST:-$HOME/dev/train-env/runs}
  mkdir -p "$dest"
  # Everything except the HF checkpoint dirs — those are the bulk and are
  # reproducible from the adapter. summary.json / steps.jsonl / train.log ARE
  # the measurement; the adapter is the artifact.
  rsync -azt --info=stats1 --exclude 'hf/' -e "$rsh" \
    "${T[2]}:$REMOTE_ENV/runs/" "$dest/"
  echo "fetched into $dest"
}

cmd_down() {
  local id=${1:?usage: pod.sh down <vast-id>}
  vastai destroy instance "$id" -y || echo "  (destroy reported an error; \
verify with 'vastai show instances')"
  ledger_close "$id"
}

cmd_list() {
  echo "--- vast ---"; vastai show instances 2>/dev/null | head -20
  echo "--- ledger ---"
  python3 - "$LEDGER" <<'PY'
import json, sys, time
try:
    doc = json.load(open(sys.argv[1]))
except (OSError, ValueError):
    print("  (no ledger)"); raise SystemExit
now, total = time.time(), 0.0
for p in doc.get("pods", []):
    if p.get("status") != "running": continue
    hrs = (now - p["started_at"]) / 3600.0
    cost = hrs * p["cost_per_hour"]; total += cost
    print(f"  {p['vast_id']:>10}  {p['label']:<34} {p['gpu_name']:<18} "
          f"{hrs:6.2f}h  ${p['cost_per_hour']:.3f}/hr  ${cost:6.2f}")
print(f"  running total: ${total:.2f}")
PY
}

case "${1:-}" in
  up)        shift; cmd_up "$@" ;;
  sync)      shift; cmd_sync "$@" ;;
  provision) shift; cmd_provision "$@" ;;
  probe)     shift; cmd_probe "$@" ;;
  fetch)     shift; cmd_fetch "$@" ;;
  down)      shift; cmd_down "$@" ;;
  list)      shift; cmd_list "$@" ;;
  ssh)       shift; id=$1; shift; pod_ssh "$id" "$@" ;;
  *) sed -n '2,12p' "${BASH_SOURCE[0]}"; exit 2 ;;
esac
