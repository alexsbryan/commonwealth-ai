# M0 probe, Halo half: 0.8B ORPO on gfx1151 (2026-08-02)

Purpose (VERIFIER_V0 §7 M0): the gate asks for a wall-clock table "re-derived
from measured tok/s on **both** boxes **and roles assigned**."
`findings/M0_PROBE.md:23` covered only the M2 Max, so every downstream M1/M3
sizing decision inherited a Mac extrapolation. This is the Strix Halo half.

## Bottom line

Two results, and the second matters more than the first.

1. **The Halo is 3.33x slower than the M2 Max** on the identical ORPO probe:
   **176.71 s/it** (n=61, 95% CI [172.4, 178.5]) vs ~53 s/it. That is
   **114.5 h — 4.8 days — per orpo-76k epoch**, against ~34 h on the Mac.
2. **The Halo cannot sustain this training run at all.** It was OOM-killed at
   step 63 of 100 (`PROBE_RC=137`). GTT ratcheted from ~25 GB to 103 GB and the
   kernel killed it. No config change is known to fix this.

So the objective's exit condition — "if torch+ROCm cannot allocate enough GTT to
train at seq 4096 on gfx1151, the Halo is not a training box, and that negative
result closes M0 just as well" — is **met**, in a sharper form than anticipated:
it trains fine for ~50 steps and then cannot keep going.

The speed gap is a **kernel-maturity gap on an unusual architecture, not a
silicon verdict** (~2.35 of a nominal 30.4 TFLOP/s). The sustainability failure
is likewise a software limit, not a capacity one — the box has 125 GB.

## Result

| | M2 Max (64 GB) | Strix Halo (gfx1151, 125 GB unified) |
|---|---|---|
| Stack | `mlx-lm-lora` (MLX) | PyTorch 2.10+rocm7.0 / TRL 1.9.2 `trl.experimental.orpo` |
| s/it (eff. batch 32, seq 4096) | ~53 | **176.71** (n=61, CI [172.4, 178.5], stdev 14.7) |
| tok/s | 1,969 | **591** |
| Config | micro 4 x accum 8 | micro 1 x accum 32, grad-checkpointing |
| Peak memory | ~22 GB RSS | 29.3 GB torch tensors / **103 GB GTT at death** |
| Completed the 100-step probe? | yes | **no — OOM-killed at step 63** |
| Ratio vs Mac | 1.0x | **3.33x slower** |

Effective batch (32) and seq (4096) are held identical across boxes because they
set iters/epoch and therefore the wall-clock table. The two boxes run different
frameworks on different numerics; that is apples-to-oranges by construction and
is what the spec asks for (each box on its native stack).

### The measurement is valid despite the death

- **61 timed steps used.** Step 63 (70.7 s) is excluded — the process was killed
  mid-step, so its time is partial.
- **No drift:** split-half medians are 176.71 (steps ≤32, n=31) and 176.67
  (steps >32, n=30).
- **No contention:** a rust-analyzer SCIP index ran 05:16:33–05:20:12Z. Step 5
  fell *entirely inside* that window and clocked 177.8 s against a 178.0/178.5
  baseline — zero measurable effect. Confirms this trainer is GPU-bound
  (~173% CPU) with CPU headroom to spare. What contaminated an earlier run was
  hipcc, a fully parallel multi-core compile — a different weight class.
- **Loss descended cleanly** 1.315 → 0.752 over 60 steps (epoch 0.944), with
  `rewards/accuracies` 1.0 and `rewards/margins` 0.689 at step 60. The pipeline
  learns; this is not a broken run.

## The sustainability failure (the load-bearing finding)

GTT over the run: ~25 GB (steps 1–50) → 51 GB (step 62) → 99 GB → 103 GB →
SIGKILL. On exit GTT fell to 21 GB, so the ~80 GB was the trainer's — only
pid 2741374 held GPU fds, verified via `/proc/*/fd`. At death: MemFree 19.8 GB
of 125.1 GB, and 4.6 GB of swap already consumed.

**Short probes cannot see this.** The config sweep measured 3–4 steps; the
longest prior run was 25. The ratchet does not become visible until ~step 50,
which is why the shipped config was believed stable at ~25 GB. **Any future
memory claim about this stack must state the step count it was measured over.**

**The tensor working set never grew.** torch's `gpu_peak_gb` sat flat at 29.34 GB
from step ~10 to death while GTT tripled. This is allocator/driver *reserve* and
fragmentation, not a tensor leak. `PYTORCH_ALLOC_CONF=expandable_segments:True`
is set in `launch_m0_probe.sh`, but torch logs `expandable_segments not supported
on this platform` at startup on ROCm/gfx1151 — so the allocator cannot compact
and ratchets as progressively longer sequences appear.

**Untested mitigation:** a `TrainerCallback` calling `torch.cuda.empty_cache()`
every ~10 steps, to cap reserve growth. Costs a ~5 h run to test. It would
**not** change the role assignment — at 3.33x slower the Halo is not the training
box either way — so it is only worth doing if someone specifically wants to train
on this box.

## tok/s (the unit the gate asks for)

Measured by tokenizing all 2,000 probe rows with the model's own tokenizer. ORPO
scores BOTH chosen and rejected, so a sample costs two forward/backward passes:

| | tokens |
|---|---|
| prompt (mean / median / max) | 869.5 / 840 / 2,529 |
| prompt+chosen (mean / max) | 1,848.9 / 4,096 |
| prompt+rejected (mean / max) | 1,412.9 / 3,501 |
| **per sample** | **3,261.8** |
| **per optimizer step** (x eff. batch 32) | **104,378** |

| box | s/it | tok/s |
|---|---|---|
| Strix Halo | 176.71 | **591** |
| M2 Max | ~53 | **1,969** |

**No padding waste at micro-batch 1.** TRL's `max_length` is a *truncation*
bound, not a pad target — its preference collator pads to the longest sequence in
the batch, and a batch of one has nothing to pad to. So 591 tok/s is real
throughput. This also explains the sweep result that micro-batch 2 was *slower*
(313.3 vs 231.8 s/it): batching two pads both up to the longer, and here that
penalty exceeds the batching gain.

s/it remains the load-bearing number — it is what sizes an epoch, and unlike
tok/s it does not move with the dataset's token distribution.

## Epoch sizing

| Dataset | Train rows | Iters/epoch | Halo | M2 Max |
|---|---|---|---|---|
| orpo-76k (Stream A) | 74,674 | 2,334 | **114.5 h (4.8 days)** | ~34 h |
| orpo-ab (A+B) | 93,693 | 2,928 | **143.7 h (6.0 days)** | ~43 h |

These are what the Halo *would* cost if it could run to completion. It cannot —
it dies at step 63 of 2,334.

**The extrapolation was verified, not assumed.** It presumes the real training
sets have the probe's token distribution. Checked on stratified ~500-row samples,
tokens/sample: probe 3,244.9, orpo-76k 3,222.7 (−0.7%), orpo-ab 3,094.1 (−4.6%).
The 76k projection holds; the orpo-ab one is mildly conservative. The probe
manifest confirms it is a seed-17 sample of the same HalluGuard-76k source.

## Roles (the half of the gate that is not a number)

- **M2 Max — primary trainer** for M1 and all 0.8B work. It is 3.33x faster and
  it is the only box that finishes.
- **Strix Halo — inference / eval / long-context serving.** What its 125 GB
  unified memory is actually good for, and where it already carries the fleet's
  big models. It is **not** a training box: too slow, and it cannot complete a
  100-step probe.
- **M3 (4B on A+B) — the Mac, at 2x the spec's budget; NOT the Halo.**

### §4's wall-clock table is falsified — re-derive it before scheduling M3

Spec §4 (`VERIFIER_V0.md:238-250`) sizes everything on "realistic sustained
training compute on gfx1151 is ~10–25 TFLOPS", cost ≈ 6·N·T with T ≈ 230M
tokens/epoch. Line 250 says M0 exists to re-derive that table. Running §4's own
model backwards through the measurement:

| | implied sustained TFLOPS |
|---|---|
| Strix Halo | **2.7** (§4 assumed 10–25 — a 4–9x optimism gap) |
| M2 Max | **9.0** |

That is self-consistent: 9.0 / 2.7 = 3.3x, exactly the measured s/it ratio.
Feeding measured throughput back into §4's formula:

| Run | §4 estimate | Measured-throughput estimate |
|---|---|---|
| 0.8B / epoch | ~0.5–1 day | **4.8 days** (measured, not modelled) |
| 4B / epoch, Halo | ~2.5–5 days | **~24 days** |
| 4B, 2 epochs, Halo | ~1 week | **~7 weeks** |
| 4B / epoch, Mac | not planned | **~7 days** |
| 4B, 2 epochs, Mac | — | **~2 weeks** |

**Halo: no**, for two independent reasons — it cannot sustain even the 0.8B past
step 63, and ~7 weeks badly exceeds the 1-week budget §4 assumed.
**Mac: plausibly yes**, at ~2 weeks for two epochs. Memory looks fine (4B bf16
weights ≈ 8 GB; the 0.8B run peaked ~22 GB of 64 GB), though that is unmeasured
at 4B.

Two caveats. 6·N·T is §4's model and treats compute as linear in parameters; the
0.8B's 248,320-token head is a large fraction of its params, so the 4B figure is
likely somewhat pessimistic. And the Mac 4B number is an extrapolation from a
0.8B measurement — the same class of inference that produced §4's 5x error.

**§4 also states, at line 235, "memory is simply not the constraint on this
box."** Memory is exactly what killed this run. That line needs correcting
alongside the table.

## Unsloth (spec asks for the probe "both Unsloth-patched and vanilla-TRL")

**Not run. It is viable, so no "cannot run on ROCm" reason can honestly be
recorded** — this is an open decision, not a negative result.

Measured non-destructively (`uv pip install unsloth --dry-run`): unsloth 2026.7.6
resolves on this platform, but installing it **downgrades transformers 5.14.1 →
5.5.0 and trl 1.9.2 → 0.24.0**, and adds triton 3.7.1, xformers 0.0.35,
torchvision and torchao.

The obvious worry — that a transformers downgrade drops Qwen3.5 — was tested in a
throwaway venv and is **false**: transformers 5.5.0 exports `qwen3_5` and
`qwen3_5_moe`. So the probe could run.

The cost is a destructive downgrade of the stack that produced the measurement
above, plus ~5 h, for a result that does not change the role assignment. Given
the Halo is disqualified as a training box on both speed and sustainability, the
recommendation is to **skip it** and record this section as M0's answer. If the
checkbox is wanted, `setup_training_stack.sh` rebuilds the vanilla stack
afterwards.

## What it took to get here — four blockers, all measured

1. **SEGV on every GPU op.** Bundled ROCm 7.0 HSA runtime vs the container's
   7.2.4. Fix: `LD_PRELOAD=/opt/rocm/lib/libhsa-runtime64.so.1` (note b18dacf9).
   Detection lies — `torch.cuda.is_available()` returns True while every kernel
   dies.
2. **OOM at micro-batch 4.** The 248,320-token vocab drives it: ORPO scores
   chosen AND rejected, so logits are `micro*2 x 4096 x 248320`.
3. **18 of 24 layers ran eager-torch deltanet.** `flash-linear-attention` fixed
   it (231.8 → 176.7 s/it) but needed `sudo dnf install gcc` FIRST — Triton had
   no C compiler and rolled back to CPU behind a misleading "Triton is not
   supported on current platform" warning that reads like an AMD support gap.
4. **The COMPLETE fast path is memory-infeasible** — note 12d363ea. fla +
   causal-conv1d = ~100 GB GTT at step 1; fla alone = ~25 GB for the first 50
   steps. causal-conv1d is uninstalled and must stay that way.

### Config sweep (all at effective batch 32 / seq 4096)

| micro | grad-ckpt | fla | s/it | peak | outcome |
|---|---|---|---|---|---|
| 4 | off | no | — | 106 GB | SIGKILL |
| 2 | off | no | — | — | SIGKILL |
| 1 | off | no | 231.5 | 69.1 GB | ok (3 steps) |
| 1 | **on** | no | 231.8 | 24.0 GB | **checkpointing is FREE here** |
| 2 | on | no | 313.3 | 46.1 GB | **bigger micro-batch is SLOWER** |
| 1 | on | **yes** | **176.7** | 25 GB → **103 GB** | best available; **still OOMs at step 63** |
| 1 | on | yes + ccc | (144.1 @ step 1) | ~100 GB | OOM at step 1 — faster per step, cannot fit |

Two counter-intuitive results worth carrying: gradient checkpointing costs
essentially nothing in time while cutting memory ~3x, and *increasing* the
micro-batch makes it slower. And one lesson: every "peak" in the rows above
except the last two was measured over 3–4 steps and is therefore not a
sustained-run number.

## Traps that cost runs

- **`torch.cuda.max_memory_allocated()` does not see GTT** reserved by the HIP
  runtime outside PyTorch's allocator. It read 29.34 GB while the process held
  103 GB. On unified memory ALWAYS cross-check
  `/sys/class/drm/card1/device/mem_info_gtt_used`.
- **A SIGKILL leaves no Python traceback**, and it **bypasses the trainer's
  try/except**, so `summary.json` is never written on an OOM even though the
  script writes it faithfully on error and interrupt. `steps.jsonl` survives
  (flushed per step) and is sufficient to reconstruct everything. Read
  `PROBE_RC` at the end of `train.log`.
- **The dying step's `step_s` is partial** (70.7 s vs a 176.7 median) and must be
  excluded from any statistic.

## This run saved no weights — by design

`save_strategy="no"` (`scripts/train_orpo_trl.py:359`) and no explicit save, so
the run persists timings and a loss curve and nothing else. Correct for a probe
whose purpose is a wall-clock number — checkpoint writes would consume wall-clock
and pollute the measurement — and it matches `M0_PROBE.md`'s "quality of this
100-iter probe is explicitly not a goal."

The gate's "fine-tuned-checkpoint → GGUF conversion proven on the probe
checkpoint" was already satisfied on the M2 Max (`M0_PROBE.md`, incl. the two
`mlx_lm fuse` defects and `scripts/fuse_lora_manual.py`). A ROCm-trained
checkpoint specifically would be a separate run — and per the finding above, one
that cannot currently reach step 100.

## Provenance — discarded runs kept as evidence

Each carries a `WHY_DISCARDED.txt` (`/home/alexbryan/dev/train-env/runs/`):

- `m0-halo-trl-CONTAMINATED-nocausalconv` — 25 steps, loss 1.315 → 0.782. Real
  learning evidence, invalid timing: hipcc was compiling on-box and drifted it
  168.6 → 187 s/it.
- `m0-halo-trl-OOMKILLED-attempt2` — rc=137 (SIGKILL), with causal-conv1d
  installed. Died after step 1.
- `m0-halo-trl-OOMKILLED-attempt3` — rc=**143** (SIGTERM). **The name is a
  misnomer**: a deliberate kill used as the GTT attribution measurement
  (102,892 → 21,574 MiB on kill). Its `steps.jsonl` is empty and it lived ~104 s,
  less than one step — so GTT hit ~102.9 GB *before a single step completed*.

Earlier notes recorded attempts 2 and 3 as "two rc=137 SIGKILLs." That was an
overcount, corrected in note 12d363ea (via e643e089). The
don't-install-causal-conv1d conclusion is unchanged.

**Which memory path did a run take?** `train_orpo_trl.py` now records
`deltanet_path` in its env dump, so this no longer depends on grepping for the
transformers warning string `"The fast path is not available ..."` (present =
causal-conv1d absent = sequential; absent = all four kernels = chunked).

## Artifacts

- `/home/alexbryan/dev/train-env/runs/m0-halo-trl/`
  - `steps.jsonl` — 62 per-step records, the primary evidence
  - `summary.json` — **reconstructed** by the successor session from
    `steps.jsonl`; the trainer's own writer never ran (SIGKILL). Carries a
    `reconstructed_by` field and a `validity` block.
  - `gtt_trace.tsv` — GTT sampled every 30 s through the death
  - `train.log` — ends `PROBE_RC=137`
- `/home/alexbryan/dev/train-env/launch_m0_probe.sh` — the shipped config
- `/home/alexbryan/dev/train-env/setup_training_stack.sh` — stack bring-up; warns
  if `causal_conv1d` is importable
- `scripts/train_orpo_trl.py` — the trainer
