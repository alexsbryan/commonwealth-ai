# Handoff: stand up the Strix Halo training stack (verifier-v0)

**For an agent running inside the `sovereign-rocm-7.2.4` toolbox.**
Written 2026-08-01 from a session in `sovereign-vulkan`, which cannot reach
your container (nested toolbox calls fail on this host) — that is the only
reason this is a handoff and not finished work.

Spec: `sovereign/docs/specs/VERIFIER_V0.md`. Read §7 (milestones) and §3
(data). Everything below is measured, not assumed, unless it says otherwise.

---

## Why you exist

Spec §7 **M0** asks for three ORPO probes: Unsloth-patched **on the Strix
Halo**, vanilla-TRL **on the Strix Halo**, and mlx-lm-lora on the M2 Max. Its
gate is *"wall-clock table re-derived from measured tok/s on **both** boxes."*

`findings/M0_PROBE.md:3` covers **only the M2 Max** — its whole table says
"measured on M2 Max, 64 GB". The Halo half was never done. Every downstream
sizing decision therefore inherits a Mac extrapolation, and the project has
been routing all training to the Mac by default.

Your job: close that gap. Stand up the stack, run the Halo probe, publish real
s/it numbers.

---

## Step 0 — confirm where you are (30 seconds)

```bash
cat /run/.containerenv | head -3     # expect name="sovereign-rocm-7.2.4"
ls /dev/kfd /dev/dri                 # both MUST be present or the GPU is invisible
```

If `name=` says `sovereign-vulkan`, stop: that container has no ROCm at all and
ships Python 3.14, for which no torch wheel exists. That is the wrong box.

Hardware you are targeting: **gfx1151**, AMD Radeon 8060S (Strix Halo),
PCI `1002:1586`, unified memory, 125 GB total with ~48 GB free. Needs
**ROCm 6.4+**; 7.2.4 is why you are in this container.

---

## Step 1 — run the setup script

```bash
/home/alexbryan/dev/train-env/setup_training_stack.sh
```

It is written, executable, syntax-checked, and **has never been executed** —
treat its success as unproven. It will:

1. reuse the Python **3.12** venv already created at
   `/home/alexbryan/dev/train-env/.venv` (toolboxes share `$HOME`, so it is
   already there),
2. install torch from the ROCm wheel index, trying `rocm7.0` then `rocm6.4`,
3. **hard-gate on `torch.cuda.is_available()` plus a real 2048² matmul** —
   this is the only check that matters; a torch that imports but sees no GPU is
   worthless,
4. install `trl peft transformers datasets accelerate`,
5. attempt `unsloth`, and continue without it if it fails (ROCm support there is
   community/experimental — vanilla TRL's `ORPOTrainer` is the fallback, and
   §7 M0 wants both probes anyway).

**CORRECTION (2026-08-02, from the ROCm-box session that executed this).** The
script *was* run. It got through the torch install and then **segfaulted** on
the GPU gate. Three things below were wrong or missing; all are now fixed in
the script itself, and all are measured, not guessed.

1. **`HSA_OVERRIDE_GFX_VERSION=11.0.0` is NOT the fix.** It does nothing here.
   This is not an arch-detection problem — the wheel already ships real gfx1151
   hipblaslt kernels. Nine env configurations were tested (SDMA, XNACK,
   GPU_MAX_HW_QUEUES, HIP_VISIBLE_DEVICES and pairs); every one segfaults. The
   harness is kept at `train-env/hip_env_matrix.sh`.

2. **The actual fix is `export LD_PRELOAD=/opt/rocm/lib/libhsa-runtime64.so.1`.**
   torch 2.10.0+rocm7.0 bundles its own ROCm 7.0 HSA runtime, which is
   incompatible with this container's ROCm 7.2.4 stack. `LD_LIBRARY_PATH` does
   **not** work — torch's RPATH is searched first. Only LD_PRELOAD wins.
   See note `b18dacf9`.

3. **The gate itself is too weak to trust.** Detection lies on this hardware:
   `torch.cuda.is_available()` returns True, `get_device_properties` reports
   gfx1151 and 124 GB, and even `torch.empty(device="cuda")` succeeds — while
   every real copy and kernel launch dies. Use
   `train-env/gpu_capability_probe.py` instead: it measures the true allocation
   ceiling, bf16 TFLOP/s, and an autograd round-trip, and exits non-zero if the
   box cannot actually train. Measured result here: **48 GB ceiling, 30.4
   TFLOP/s bf16, autograd clean.**

Also missing from the original script: `numpy` (torch's ROCm wheel does not
pull it) and `flash-linear-attention` + a C compiler — see the config notes in
`train-env/launch_m0_probe.sh`.

Do not proceed to training on CPU — the whole point is the wall-clock number.

---

## Step 2 — the M0 Halo probe

Mirror the M2 Max probe so the numbers are comparable. Its config, from
`findings/M0_PROBE.md`:

> Qwen/Qwen3.5-0.8B, ORPO, LoRA r=32/α=64 (scale 2.0), lr 1e-4, batch 4 ×
> grad-accum 8 (effective 32), seq 4096, 100 iters, `data/orpo-probe`.

Reference to beat — **M2 Max, 64 GB**: ~53 s/it at effective batch 32, seq
4096 ⇒ 76,708 rows ≈ 2,397 iters/epoch ≈ **35 h/epoch**. For A+B (~96k rows)
that is ~3,000 iters/epoch ≈ **44 h/epoch**.

**CORRECTION (2026-08-02): the Mac's batch config does not fit here, and the
mirror had to change in one specific way.** Micro-batch 4 is SIGKILLed by the
host OOM killer at 106 GB — the cause is Qwen3.5's 248,320-token vocab, since
ORPO scores chosen *and* rejected, making the logits tensor
`micro*2 x 4096 x 248320`. **Effective batch stays 32** (that is what sets
iters/epoch and therefore the wall-clock table); the micro-batch/grad-accum
split is a framework detail and is allowed to differ from mlx_lm_lora's.
Measured, all at effective batch 32 / seq 4096:

| micro | grad-ckpt | s/it | peak |
|---|---|---|---|
| 4 | off | SIGKILL | 106 GB |
| 2 | off | SIGKILL | — |
| 1 | off | 231.5 | 69.1 GB |
| 1 | **on** | 231.8 | **24.0 GB** |
| 2 | on | 313.3 | 46.1 GB |
| 1 | on + **fla** | **177.1** | 24.0 GB |

Two counter-intuitive results worth carrying forward: gradient checkpointing is
**free** on this box (same wall clock, a third of the memory), and a **bigger
micro-batch is slower** — sequences span ~2k–5k tokens and get padded to the
longest in the batch, so batching buys padding waste rather than parallelism.

Also note TRL moved ORPO: `from trl import ORPOConfig` raises on trl 1.9.2 (it
suggests `GRPOConfig`). It now lives at `trl.experimental.orpo`, and
`max_prompt_length` was replaced by `max_completion_length`.

Run it **twice** — Unsloth-patched and vanilla-TRL — and record s/it for each.
Quality is explicitly *not* a goal at 100 iters; the M2 Max probe's adapter
barely moved and that was expected. You are measuring plumbing and speed.

Then write `findings/M0_PROBE_HALO.md` and update M0_PROBE.md's wall-clock
table so it covers both boxes. That closes the M0 gate.

---

## Data — ready, decontaminated, do not rebuild

All under `research/verifier-v0/data/` (gitignored, deterministic from seed 17):

| dir | contents | use |
|---|---|---|
| `orpo-probe` | train 2,000 / valid 200 / test 200 | **your probe** |
| `orpo-76k` | train 74,674 / valid 1,000 / test 1,000 | Stream A only, §7 M1 |
| `orpo-ab` | A+B, ~76,674 A + ~19k B (~20% B) | §7 M3 |

Format is flat `{"prompt": str, "chosen": str, "rejected": str}` — what
`mlx_lm_lora`'s ORPODataset wants, and what TRL's `ORPOTrainer` accepts.

**Stream A is contaminated and has already been cleaned.** 34 rows share a
13-gram with LLM-AggreFact test docs, in subsets we report on (ClaimVerify 14,
Wice 13, ExpertQA 3, Reveal 2, AggreFact-XSum 2 — all inside the 11-subset
average §1 targets). They are dropped from every dir above; the droplist is
`findings/streamA_contaminated_rows.json`, independently verified 34/34. If you
regenerate data, use `scripts/prepare_orpo_data.py`, which applies the droplist
automatically and refuses to run without a contamination report.

---

## Do not touch: work in flight on the other box

A teacher-labeling run and its supervising driver are running under
`sovereign-vulkan` right now, writing to
`research/verifier-v0/data/stream_b/all/`:

- labeler **pid 2332400** — ~21,300/21,436 cases, finishing shortly
- driver **pid 2476684** — `on_labeling_complete.sh`, auto-resumes on crash,
  then validates pairs and **rebuilds `data/orpo-ab`**

So `data/orpo-ab` may be rewritten while you work. Check
`data/stream_b/all/STAGE_REPORT.txt` — when it exists, the run is finished and
`orpo-ab` is final. Until then, use `orpo-probe` (stable, unaffected).

Do not kill either process, and do not start a second driver.

---

## Gotchas that already cost time

- **`uv venv` does not install pip.** Use `uv pip install`, never
  `.venv/bin/pip` — it does not exist.
- **Never test an install through a pipe.** `cmd | tail -25` returns *tail's*
  exit status, so a hard failure reports as success. This bug printed
  "INSTALL OK" over a completely failed torch install on 2026-08-01.
- **Do not edit a bash script while it is executing** — bash re-reads by byte
  offset and can corrupt the run. Kill and relaunch.
- **Finding the driver:** use
  `ps -eo pid,cmd | awk '$2=="bash" && /on_labeling_complete/'`.
  `pgrep -f on_labeling_complete.sh` also matches your own watcher process, and
  a kill loop over it will kill your own shell.
- **`setsid` reparents**, so the pid printed at launch is *not* the surviving
  process's pid. Always re-derive it.
- **ROCm's bad reputation on this host is about llama.cpp/ggml**, not PyTorch —
  the A3B SEGV that moved the daemon to Vulkan is a different stack. It does
  not predict PyTorch+ROCm behaviour either way.

---

## Background worth having

- `findings/M2_STREAM_B_VOLUME.md` — how the 21,831-case Stream B bank was
  built, why volume is capped at `2 × grounded`, and the known weaknesses that
  belong on the eval card.
- `findings/M0_PROBE.md` — the M2 Max probe, including two real GGUF-conversion
  defects (`mlx_lm fuse` drops Qwen3.5's MTP layer and corrupts the
  hybrid-attention merge) and the `fuse_lora_manual.py` fix. Those are
  Mac-path bugs; whether the Halo path has analogues is unknown.
- Notes `72b3ab47` (contamination discipline), `b2db92b9` (Stream A droplist),
  `1eb7ec59` (Stream B volume identity).

Nothing in this arc is committed. Do not commit without an explicit ask.
