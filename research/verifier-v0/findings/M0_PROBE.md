# M0 probe: 0.8B ORPO pipeline shakeout (2026-07-29)

Purpose (VERIFIER_V0.md M0): prove the train → fuse → GGUF → llama.cpp path on
the M2 Max and measure wall-clock for sizing the real runs. Quality of this
100-iter probe is explicitly not a goal.

## Training

- Qwen/Qwen3.5-0.8B, ORPO, LoRA r=32/α=64 (scale 2.0), lr 1e-4, batch 4 ×
  grad-accum 8 (effective 32), seq 4096, 100 iters, `data/orpo-probe`.
- Loss 0.155 @ iter 10 → 0.056 @ iter 20, flat thereafter (0.057 @ 100).
- Timing: 1:31:19 for 100 iters ≈ **54.8 s/it** overall. Per-10-iter window
  means swing 28–74 s/it — dominated by per-batch sequence-length variance,
  not contention: iters 1–36 ran with a nice-19 contamination pass alongside
  (54.6 s/it), iters 37–100 solo (52.2 s/it).
- First attempt died at iter 40 in the 2026-07-29 machine lockup (four
  concurrent heavy jobs). Rerun used `runs/probe-0.8b-orpo/relaunch.sh` with a
  40 GB RSS tripwire; peak trainer RSS observed ~22 GB.
- Adapter moved barely at this scale: `lora_b` magnitudes ≈ 0, `lora_a` at
  init magnitude; fused output is token-identical to base on the smoke prompt.
  Expected for 100 iters — the probe validates plumbing, not learning.

## Wall-clock table (BOTH boxes — Halo half in `M0_PROBE_HALO.md`)

**Roles, assigned from these numbers (§7 M0 gate):** M2 Max = **primary trainer**,
including M3's 4B. Strix Halo = **inference / eval only** — 3.33x slower *and* it
cannot complete a 100-step probe (OOM-killed at step 63; GTT ratchets 25 → 103 GB).

**This overturns §4's plan, which schedules M3 on the Halo** (`VERIFIER_V0.md:245,462`).
§4 assumed 10–25 sustained TFLOPS on gfx1151; measured is **2.7** (Mac: 9.0), so
its estimates are 4–9x optimistic. A 4B epoch is ~24 days on the Halo (~7 weeks
for the planned 2 epochs) versus ~7 days on the Mac (~2 weeks). §4's line 235,
"memory is simply not the constraint on this box," is also falsified — memory is
what killed the run. Per §4 line 250, that table must be re-derived before M3 is
scheduled; the derivation is in `M0_PROBE_HALO.md`.

| Step | M2 Max (64 GB) | Strix Halo (gfx1151, 125 GB) |
|---|---|---|
| ORPO LoRA iter (eff. batch 32, seq 4096) | **~53 s/it** · 1,969 tok/s | **176.71 s/it** · 591 tok/s (n=61, CI [172.4, 178.5]) |
| → orpo-76k (74,674 rows, 2,334 it/epoch) | **~34 h/epoch** | 114.5 h/epoch (4.8 days) |
| → orpo-ab (93,693 rows, 2,928 it/epoch) | ~43 h/epoch | 143.7 h/epoch (6.0 days) |
| Completed the 100-step probe? | yes | **no — OOM at step 63, `PROBE_RC=137`** |
| Stack | `mlx-lm-lora` (MLX) | PyTorch 2.10+rocm7.0 / TRL 1.9.2 |

The Halo's h/epoch figures are what it *would* cost if it could finish. It cannot.
Both boxes ran effective batch 32 / seq 4096 so iters/epoch is identical; the
frameworks and numerics differ, which is what the spec asks for (each box native).

| Step (M2 Max only) | Measured | Extrapolation |
|---|---|---|
| mlx_lm fuse (manual script) | ~1 min | — |
| convert_hf_to_gguf q8_0 | ~2 min | — |
| llama-cli q8 generation (0.8B) | 172–177 t/s | — |
| Reference: HalluGuard paper recipe | 16 h on H100 (r=16/α=16) | our r=32/α=64 is a deliberate delta |

## GGUF conversion: two real defects found and fixed (the rehearsal's payoff)

1. **`mlx_lm fuse` drops Qwen3.5's MTP layer.** The HF snapshot carries
   `mtp.*` tensors (multi-token-prediction head); llama.cpp's `qwen35` arch
   maps them to `blk.24.nextn.*` and requires `block_count = 25` (24 decoder
   layers + 1). mlx-lm models only the decoder layers, so the fused export has
   320 tensors vs 335 and the converted GGUF dies at load with
   `missing tensor 'blk.24.attn_norm.weight'`.
2. **`mlx_lm fuse` corrupts the hybrid-attention merge outright.** Even with
   MTP grafted back, the fused model emits token salad *in MLX itself*, while
   base + adapter (unfused) generates correctly — isolating the corruption to
   fuse, not training and not conversion. Qwen3.5 interleaves `linear_attn`
   (gated deltanet) and `self_attn` layers; the adapter covers both
   (186 modules).

**Fix: `scripts/fuse_lora_manual.py`** — copies the original HF snapshot
(config, tokenizer, index, all 488 tensors incl. `mtp.*` and vision tower) and
applies `W += scale · (lora_b.T @ lora_a.T)` in f32 per adapter module, keyed
by explicit name mapping (`language_model.model.*` → `model.language_model.*`),
written with `safetensors.torch`. Verified: MLX generation from the manual
fuse is coherent and token-identical to base+adapter; converted q8_0 GGUF
loads and generates correctly under llama-cli (172 t/s).

Non-findings: no NaN/inf in any tensor (adapter, fused, grafted — scanned).
The `RuntimeWarning: invalid value` pairs from `gguf-py/quants.py` during
q8_0 conversion appear on known-good files too; benign converter quirk.

## Artifacts

- `runs/probe-0.8b-orpo/adapters/` — iter-50 + iter-100 checkpoints
- `runs/probe-0.8b-orpo/fused-manual/` — correct fused model (HF layout)
- `runs/probe-0.8b-orpo/probe-orpo-0.8b-q8.gguf` — working converted model
- `runs/probe-0.8b-orpo/fused/` — mlx_lm fuse output, kept as the bug repro
- `runs/probe-0.8b-orpo/train.log`, `train.log.iter40-crash`, `mem.log`
