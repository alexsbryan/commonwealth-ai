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
set -u
cd /home/alexbryan/dev/commonwealth-ai/research/verifier-v0

ARM=${ARM:-A}
case "$ARM" in
  A)  DATA=data/orpo-76k ;;
  AB) DATA=data/orpo-ab ;;
  *)  echo "FATAL: ARM must be A or AB, got '$ARM'" >&2; exit 2 ;;
esac

# See launch_gradcheck.sh for why this is detected and not hardcoded.
for cand in /opt/rocm/lib/libhsa-runtime64.so.1 \
            /run/host/usr/lib64/libhsa-runtime64.so.1; do
  if [ -e "$cand" ]; then export LD_PRELOAD="$cand"; break; fi
done
[ -n "${LD_PRELOAD:-}" ] || { echo "FATAL: no libhsa-runtime64.so.1" >&2; exit 2; }

export HF_DATASETS_DISABLE_PROGRESS_BARS=1

OUT=${OUT:-/home/alexbryan/dev/train-env/runs/mix-$ARM}
mkdir -p "$OUT"

# Serial by construction. Two arms at once is what locked the machine on
# 2026-07-29, and GPU co-tenancy is the leading explanation for M0's death at
# step 63 (note f1e96c88).
if pgrep -f "[t]rain_orpo_trl" >/dev/null; then
  echo "FATAL: a trainer is already running — arms are serial." >&2; exit 3
fi

echo "arm=$ARM data=$DATA iters=${ITERS:-400} out=$OUT model=$(basename "${MODEL:-Qwen3.5-0.8B}")"
GTT_AT_LAUNCH_MIB=$(( $(cat /sys/class/drm/card1/device/mem_info_gtt_used) / 1048576 ))
echo "box GTT at launch: ${GTT_AT_LAUNCH_MIB} MiB"

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
if [ -n "${NO_BUCKET:-}" ] && [ "$GTT_AT_LAUNCH_MIB" -gt "${GTT_BASELINE_MAX_MIB:-2048}" ]; then
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
/home/alexbryan/dev/train-env/.venv/bin/python scripts/train_orpo_trl.py \
  --model "${MODEL:-/home/alexbryan/dev/train-env/models/Qwen3.5-0.8B}" \
  --data "$DATA" \
  --out "$OUT" \
  --iters "${ITERS:-400}" \
  --batch-size "${MICRO:-1}" --grad-accum "${ACCUM:-32}" --seq-len 4096 \
  --lr 1e-4 --lora-r 32 --lora-alpha 64 --beta 0.1 --seed "${SEED:-17}" \
  --grad-checkpointing \
  ${GTT_LIMIT:+--gtt-limit-gb "$GTT_LIMIT"} \
  ${SAVE_EVERY:+--save-every "$SAVE_EVERY"} \
  ${STOP_AT:+--stop-at-step "$STOP_AT"} \
  ${NO_BUCKET:+--no-group-by-length} \
  ${RESUME:+--resume} \
  >>"$OUT/train.log" 2>&1
RC=$?
echo "ARM_RC=$RC" >>"$OUT/train.log"

# The gate, independently of the trainer's own verdict.
/home/alexbryan/dev/train-env/.venv/bin/python \
  scripts/check_adapter_trained.py "$OUT/adapter"
GATE=$?
echo "arm=$ARM train_rc=$RC gate_rc=$GATE"
exit $(( RC != 0 ? RC : GATE ))
