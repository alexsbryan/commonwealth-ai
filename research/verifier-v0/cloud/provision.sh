#!/usr/bin/env bash
# Bring a rented GPU pod up to "fit to train". RUNS ON THE POD.
#
#   bash /workspace/verifier/cloud/provision.sh
#
# IDEMPOTENT BY CONSTRUCTION. Every step is safe to re-run, because the way
# this actually gets used is: ssh in, something failed, fix it, run again. A
# provisioner that only works on a clean box turns a five-minute repair into a
# destroy-and-recreate cycle at full price.
#
# WHAT THIS DOES NOT DO: upload the model. Qwen3.5-4B is 8.8 GB and public on
# HF; the pod pulls it in about a minute at datacenter bandwidth, against ~35
# minutes pushing it from the Halo's 4.2 MB/s uplink. Only the ~780 MB derived
# training set has to come from us, because it is neither in git nor on the
# Hub. See cloud/README.md "What crosses the wire".
set -euo pipefail

WORK=${WORK:-/workspace}
REPO_DIR=${REPO_DIR:-$WORK/verifier}
TRAIN_ENV=${TRAIN_ENV:-$WORK/train-env}
PY=${PY:-python}
MODEL_ID=${MODEL_ID:-Qwen/Qwen3.5-4B}
MODEL_DIR=${MODEL_DIR:-$TRAIN_ENV/models/$(basename "$MODEL_ID")}

echo "=== provision: $(hostname) ==="
echo "  work=$WORK repo=$REPO_DIR train_env=$TRAIN_ENV"
mkdir -p "$TRAIN_ENV/models" "$TRAIN_ENV/runs"

# --- 1. the GPU is real -----------------------------------------------------
# Before anything slow. A pod that came up without a usable GPU should cost us
# seconds, not a full provision cycle.
if command -v nvidia-smi >/dev/null; then
  nvidia-smi --query-gpu=name,memory.total,driver_version,compute_cap \
             --format=csv,noheader || true
else
  echo "WARN: no nvidia-smi on PATH" >&2
fi

# --- 2. the stack -----------------------------------------------------------
# --extra-index-url, not --index-url: the cu128 index carries torch and its
# CUDA runtime shims only, so everything else still has to resolve from PyPI.
# Using --index-url instead makes transformers/trl/peft unresolvable, which
# presents as a confusing "no matching distribution" for a package that
# obviously exists.
echo "--- installing the pinned stack ---"
# PEP 668. The pytorch base images ship a DISTRO-MANAGED python (Ubuntu 24.04
# marks it EXTERNALLY-MANAGED), so a bare `pip install` aborts with
# "error: externally-managed-environment" and a wall of advice about venvs.
#
# --break-system-packages is the RIGHT answer here and not a shortcut: this
# container is disposable, it exists solely to run one training job, the image's
# torch already lives in that same system python, and a venv would either
# shadow it or force a redundant 900 MB reinstall. The flag is applied ONLY
# when the marker is actually present, so a conda-based or venv-based image
# (older pytorch tags) is untouched.
PIP_FLAGS=""
if $PY -c 'import os,sys,sysconfig; sys.exit(0 if os.path.exists(os.path.join(sysconfig.get_path("stdlib"),"EXTERNALLY-MANAGED")) else 1)'; then
  PIP_FLAGS="--break-system-packages"
  echo "  PEP 668 externally-managed python detected -> --break-system-packages"
fi
$PY -m pip install --quiet $PIP_FLAGS --upgrade pip
$PY -m pip install --quiet $PIP_FLAGS \
  -r "$REPO_DIR/cloud/requirements-cu128.txt" \
  --extra-index-url https://download.pytorch.org/whl/cu128

# A compiler is REQUIRED and its absence is silent — Triton cannot build
# launcher stubs, fla degrades to eager-torch, and the only symptom is a
# warning and a ~1.3x worse s/it. The -devel base images carry gcc; install it
# only if we somehow landed on a runtime image.
if ! command -v gcc >/dev/null && ! command -v cc >/dev/null; then
  echo "--- no compiler; installing build-essential (Triton JIT needs it) ---"
  (apt-get update -qq && apt-get install -y -qq build-essential) || {
    echo "FATAL: no compiler and apt failed. fla cannot work here." >&2; exit 3; }
fi

# --- 3. the base model ------------------------------------------------------
# `hf download` is resumable and content-addressed, so re-running costs a HEAD
# request per file rather than 8.8 GB.
if [ -f "$MODEL_DIR/config.json" ]; then
  echo "--- model already present at $MODEL_DIR ---"
else
  echo "--- fetching $MODEL_ID ---"
  # `hf` is the hub>=1.0 command (the old name is `huggingface-cli`). It comes
  # with huggingface-hub 1.26.0, which requirements-cu128.txt already pins —
  # but only if the [cli] extra's deps resolved, so install on demand rather
  # than assume.
  command -v hf >/dev/null || $PY -m pip install --quiet $PIP_FLAGS "huggingface_hub[cli]==1.26.0"
  hf download "$MODEL_ID" --local-dir "$MODEL_DIR"
fi

# --- 4. the gate ------------------------------------------------------------
# --skip-payload: the data may not be synced yet. This answers "is the MACHINE
# fit", which is the question that decides whether to keep paying for it. The
# full check (with payload) runs again from pod.sh probe.
echo "--- preflight (machine only) ---"
# VRAM_FLOOR_GB is exported by pod.sh so the search floor and the check floor
# are one number (§10.6). Defaulted here too, because provision.sh is also run
# by hand on a box that pod.sh did not rent.
$PY "$REPO_DIR/cloud/preflight.py" \
    --skip-payload \
    --vram-floor-gb "${VRAM_FLOOR_GB:-46}" \
    --json "$TRAIN_ENV/runs/preflight-machine.json"

echo
echo "provisioned. Next: pod.sh sync <id>  then  pod.sh probe <id>"
