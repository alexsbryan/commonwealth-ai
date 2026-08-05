#!/usr/bin/env bash
# One mix-study arm on the Halo. The Mac's scripts/run_mix_study.sh is zsh and
# assumes Mac paths (HF snapshot dir, .venv-bespoke, ~/dev/llama.cpp); this is
# the training half of it, ported. Scoring/GGUF is deliberately NOT here — that
# leg does not exist on this box yet and is being built while an arm trains.
#
#   ARM=A ./launch_arm.sh          # data/orpo-76k   (Stream A only)
#   ARM=AB ./launch_arm.sh         # data/orpo-ab    (A + B, B share .199)
#   ARM=A RESUME=1 ./launch_arm.sh # continue the newest checkpoint
#
# MATCHING AN ARM TO A PREVIOUS ONE. Arm A (runs/mix-A) was cut at step 118 of
# a 400-step schedule by the old 95 GB tripwire, and it trained BEFORE length
# bucketing existed. To compare a new arm against it, both differences have to
# be reproduced, not just the step count:
#
#   ARM=AB ITERS=400 STOP_AT=118 NO_BUCKET=1 ./launch_arm.sh
#
# 118, NOT 117. summary.json's `steps_timed` counts inter-step DURATIONS and is
# always one short (step 1 has no predecessor to time against). Arm A reported
# steps_timed 117 and had trained 118 optimizer steps; its trace ends at 118.
# Take the step from the last record in steps.jsonl, or from the
# `steps_completed` field added 2026-08-03 — not from steps_timed.
#
# STOP_AT stops at a named step without touching the LR schedule (lowering
# ITERS would re-fit the decay and compare two different schedules). NO_BUCKET
# restores the pre-2026-08-03 shuffled sampler — bucketing changes WHICH rows
# land in which step, so an arm trained with it is not comparable to one
# trained without it, whatever the step count says.
#
# PAUSE AND RESUME. At the measured 171 s/step a 400-step arm is ~19 hours, so
# it WILL be interrupted — the box is also a workstation. Ctrl-C (or SIGINT to
# the trainer) stops cleanly, keeps the adapter, and leaves a checkpoint;
# RESUME=1 continues from it with optimizer, scheduler, RNG and step count
# intact. train.log is APPENDED to across legs so the whole history survives.
# `--resume` with no checkpoint is a hard error, never a silent restart.
#
# Matched ITERS, not matched epochs: at effective batch 32 both arms see the
# same 12,800 examples and only the mixture differs (M2_MIX_STUDY_DESIGN.md).
#
# RUNS ON BOTH BOXES. Every path below is a knob with the Halo's value as its
# default, so a bare `ARM=A ./launch_arm.sh` on the Halo means exactly what it
# meant before 2026-08-04, and a rented CUDA pod sets four env vars instead of
# maintaining a forked copy of this file. Two launchers would be two deciders
# for one recipe (§10.6) — and the hyperparameters at the bottom are the
# recipe, so a fork is how a cloud run silently stops being comparable to the
# Halo runs it is supposed to extend.
#
#   REPO_DIR   where scripts/ and data/ live      (default: the Halo checkout)
#   TRAIN_ENV  where .venv, models/ and runs/ live (default: ~/dev/train-env)
#   PY         the interpreter                     (default: $TRAIN_ENV/.venv/bin/python)
#   MODEL/OUT  as before
set -u
REPO_DIR=${REPO_DIR:-/home/alexbryan/dev/commonwealth-ai/research/verifier-v0}
TRAIN_ENV=${TRAIN_ENV:-/home/alexbryan/dev/train-env}
PY=${PY:-$TRAIN_ENV/.venv/bin/python}
cd "$REPO_DIR"

ARM=${ARM:-A}
case "$ARM" in
  A)  DATA=data/orpo-76k ;;
  AB) DATA=data/orpo-ab ;;
  *)  echo "FATAL: ARM must be A or AB, got '$ARM'" >&2; exit 2 ;;
esac

# AMDGPU ONLY. On gfx1151 the HIP stack segfaults without this preload — the
# hip_env_matrix.sh sweep found it, and cloud/preflight.py reproduces the
# failure on demand (SIGSEGV compiling a trivial Triton kernel with it unset,
# clean with it set). See launch_gradcheck.sh for why it is detected and not
# hardcoded.
#
# The gate is the SYSFS PATH, not the absence of the library: "no amdgpu here"
# and "amdgpu here but the runtime is missing" are different failures and only
# the second one should stop the run. Before 2026-08-04 this block exited 2
# unconditionally, which meant the shared launcher could never start on a CUDA
# box at all.
IS_AMDGPU=0
[ -e /sys/class/drm/card1/device/mem_info_gtt_used ] && IS_AMDGPU=1
if [ "$IS_AMDGPU" = 1 ]; then
  for cand in /opt/rocm/lib/libhsa-runtime64.so.1 \
              /run/host/usr/lib64/libhsa-runtime64.so.1; do
    if [ -e "$cand" ]; then export LD_PRELOAD="$cand"; break; fi
  done
  [ -n "${LD_PRELOAD:-}" ] || { echo "FATAL: amdgpu box with no libhsa-runtime64.so.1" >&2; exit 2; }
fi

export HF_DATASETS_DISABLE_PROGRESS_BARS=1

OUT=${OUT:-$TRAIN_ENV/runs/mix-$ARM}
mkdir -p "$OUT"

# Serial by construction. Two arms at once is what locked the machine on
# 2026-07-29, and GPU co-tenancy is the leading explanation for M0's death at
# step 63 (note f1e96c88).
if pgrep -f "[t]rain_orpo_trl" >/dev/null; then
  echo "FATAL: a trainer is already running — arms are serial." >&2; exit 3
fi

echo "arm=$ARM data=$DATA iters=${ITERS:-400} out=$OUT model=$(basename "${MODEL:-Qwen3.5-0.8B}")"
# The launch-time memory baseline is an amdgpu concept (one unified pool shared
# with co-tenants). On a rented CUDA box the trainer's own arming line reports
# the equivalent, so this stays silent rather than inventing a number.
GTT_AT_LAUNCH_MIB=0
if [ "$IS_AMDGPU" = 1 ]; then
  GTT_AT_LAUNCH_MIB=$(( $(cat /sys/class/drm/card1/device/mem_info_gtt_used) / 1048576 ))
  echo "box GTT at launch: ${GTT_AT_LAUNCH_MIB} MiB"
fi

# THE BASELINE IS PART OF THE PROTOCOL, and until now it lived only in a
# findings doc. M2_HALO_GRADCHECK.md:449 records what arm A actually ran under:
# "an EMPTY BOX -- daemon stopped, GTT at launch 620 MiB". GTT here is one
# 124 GB pool shared with any resident model, so a co-tenant does not slow the
# run down, it eats the tripwire margin. Arm A's envelope reached 101.97 GB
# from a 0.6 GB floor; the same envelope on top of a daemon holding 8.5 GB is
# ~110 GB against a 112 GB limit, and the arm aborts a step or two short of its
# target looking like instability that is really a co-tenant.
#
# Only enforced for UNBUCKETED runs, which is the only case that reproduces
# that envelope -- length bucketing (default since 2026-08-03) keeps the peaks
# low enough that a resident daemon is affordable. Refuses rather than warns:
# the whole point of an unbucketed arm is to be comparable to a run that had
# the box to itself, and a warning at hour zero of a six-hour run is read by
# nobody.
# Guarded on IS_AMDGPU explicitly rather than relying on the baseline being 0
# off-amdgpu: a gate that passes because its input is a placeholder is a gate
# that has stopped gating without saying so (§18.3).
if [ "$IS_AMDGPU" = 1 ] && [ -n "${NO_BUCKET:-}" ] && [ "$GTT_AT_LAUNCH_MIB" -gt "${GTT_BASELINE_MAX_MIB:-2048}" ]; then
  echo "FATAL: box GTT is ${GTT_AT_LAUNCH_MIB} MiB at launch, over the" >&2
  echo "  ${GTT_BASELINE_MAX_MIB:-2048} MiB baseline an unbucketed arm needs." >&2
  echo "  Arm A ran at 620 MiB. Something else is holding the pool -- almost" >&2
  echo "  always the daemon with a model resident. Free it:" >&2
  echo "    sovereign daemon stop" >&2
  echo "  Override with GTT_BASELINE_MAX_MIB=<n> only if you have decided this" >&2
  echo "  arm does not need to be comparable to arm A." >&2
  exit 4
fi

# Effective batch 32 (micro 1 x accum 32) by default. MICRO/ACCUM override it —
# KEEP THE PRODUCT AT 32 or the run is not comparable to anything before it.
#
# WHY MICRO IS A KNOB AGAIN. M0_PROBE_HALO.md:222 measured micro 2 at 313.3 s/it
# against micro 1's 231.8 and concluded "bigger micro-batch is SLOWER" — with
# the stated cause being that batching two sequences pads both up to the longer.
# LENGTH BUCKETING LANDED AFTER THAT TABLE (2026-08-03) and takes consecutive
# pairs within 128 tokens from 15.2% to 92.2%, which is precisely the mechanism
# that made micro 2 slow. The conclusion may not survive its own premise, and at
# 4B scale a 1.3x step-time difference is a day per epoch. Re-measure; do not
# assume either way.
#
# Gradient checkpointing IS free here — measured, 231.5 off vs 231.8 on, at a
# 3x memory saving (M0_PROBE_HALO.md:221). That one needs no re-test.
#
# THE GTT LIMIT IS THE TRAINER'S TO DEFAULT, NOT THIS SCRIPT'S. Until
# 2026-08-03 this line read `--gtt-limit-gb "${GTT_LIMIT:-95}"`, so the
# launcher silently overrode the trainer and pinned every arm to 95 GB — below
# this workload's ordinary ~95-100 GB transient. That is what killed arm A at
# step 118. Two defaults for one threshold is how they stop agreeing
# (ARCH_PRINCIPLES §10.6); the flag is now passed ONLY when GTT_LIMIT is set.
# MODEL defaults to the 0.8B so every historical invocation still means what it
# did. VERIFIER_V0.md:116 names Qwen3.5-4B "the v0 model" and :115 gives the
# 0.8B its real job — pipeline shakeout — so the 4B needs to be a knob, not an
# edit. summary.json records `config.model`, so a run can never be mistaken for
# one at a different size.
# ALLOCATOR MODE — ON BY DEFAULT ON CUDA, MEASURED, NOT ASSUMED (note 8aad1dbb).
#
# Without this the RTX PRO 5000 OOMs at step 16 of 25 with 29.03 GiB allocated,
# 15.23 GiB reserved-but-unallocated and 2.41 GiB free: torch could not serve one
# 7.50 GiB contiguous request out of a fragmented cache. With it, the same recipe
# on the same pod completed all 25 steps. Expandable segments let a segment GROW
# rather than requiring a contiguous free block — visible in the trace as reserve
# rising 40.22 -> 41.17 GB at exactly the step that used to fail.
#
# IT IS NOT A SPEED TRADE. Paired runs, same seed, bit-identical losses: 34.60 vs
# 34.77, 41.14 vs 41.27, 46.37 vs 46.62 s/it at steps 3/10/15. It also LOWERS
# steady reserve by ~4.1 GB at identical demand.
#
# AMDGPU IS DELIBERATELY EXCLUDED. This was validated on CUDA only, and the Halo's
# unified-memory allocator behaves differently enough that adopting it there
# without a measurement would be exactly the substitution §18.3 forbids. The probe
# is the SAME sysfs path the trainer uses to decide platform (`device_pool_gb` ->
# `mem_reading`), so there is one way to ask "am I on the Halo", not two.
if [ -z "${PYTORCH_ALLOC_CONF:-}" ] && [ ! -r /sys/class/drm/card1/device/mem_info_gtt_used ]; then
  export PYTORCH_ALLOC_CONF="expandable_segments:True"
  export PYTORCH_CUDA_ALLOC_CONF="$PYTORCH_ALLOC_CONF"   # pre-2.9 spelling
  echo "  allocator: PYTORCH_ALLOC_CONF=$PYTORCH_ALLOC_CONF (CUDA default; note 8aad1dbb)"
fi

"$PY" scripts/train_orpo_trl.py \
  --model "${MODEL:-$TRAIN_ENV/models/Qwen3.5-0.8B}" \
  --data "$DATA" \
  --out "$OUT" \
  --iters "${ITERS:-400}" \
  --batch-size "${MICRO:-1}" --grad-accum "${ACCUM:-32}" --seq-len 4096 \
  --lr 1e-4 --lora-r 32 --lora-alpha 64 --beta 0.1 --seed "${SEED:-17}" \
  --grad-checkpointing \
  ${LIGER:+--liger} \
  ${GTT_LIMIT:+--gtt-limit-gb "$GTT_LIMIT"} \
  ${SAVE_EVERY:+--save-every "$SAVE_EVERY"} \
  ${SAVE_TOTAL_LIMIT:+--save-total-limit "$SAVE_TOTAL_LIMIT"} \
  ${STOP_AT:+--stop-at-step "$STOP_AT"} \
  ${NO_BUCKET:+--no-group-by-length} \
  ${RESUME:+--resume} \
  >>"$OUT/train.log" 2>&1
RC=$?
echo "ARM_RC=$RC" >>"$OUT/train.log"

# The gate, independently of the trainer's own verdict.
"$PY" scripts/check_adapter_trained.py "$OUT/adapter"
GATE=$?
echo "arm=$ARM train_rc=$RC gate_rc=$GATE"
exit $(( RC != 0 ? RC : GATE ))
