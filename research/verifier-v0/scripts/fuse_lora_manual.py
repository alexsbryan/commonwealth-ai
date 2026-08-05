#!/usr/bin/env python3
"""Manual LoRA fuse: base HF snapshot + mlx-lm-lora adapter -> fused HF dir.

Why this exists (M0 finding, 2026-07-29): `mlx_lm fuse` silently corrupts
Qwen3.5 hybrid models — the fused output is token salad in MLX itself, while
base + adapter (unfused) generates correctly. Qwen3.5 mixes `linear_attn`
(gated-deltanet) and `self_attn` layers; fuse mishandles the merge and also
drops the `mtp.*` (multi-token-prediction) tensors llama.cpp's qwen35 arch
requires (block_count = decoder layers + 1).

This script does the merge the boring, verifiable way:
  1. Copy every file of the original HF snapshot (config, tokenizer, index)
     so the output dir is layout-identical to what convert_hf_to_gguf.py has
     already converted successfully. Nothing is dropped, vision + mtp included.
  2. For each adapter module, W += scale * (lora_b.T @ lora_a.T) in f32,
     cast back to the weight's dtype. Same math as mlx_lm's LoRALinear.fuse,
     applied by explicit key mapping: adapter `language_model.model.<path>`
     -> snapshot `model.language_model.<path>.weight`.
  3. Write with safetensors.torch (the writer HF tooling reads), preserving
     the original shard filename so the index stays valid.

Usage:
  fuse_lora_manual.py --snapshot <hf snapshot dir> \
      --adapter runs/probe-0.8b-orpo/adapters --out runs/probe-0.8b-orpo/fused-manual
"""

import argparse
import glob
import json
import os
import shutil

import torch
from safetensors.torch import load_file, save_file


def load_adapter(adapter_dir: str):
    """(modules, scale, rank, flavour) normalised to ONE convention.

    TWO PRODUCERS, ONE FUSE (ARCH_PRINCIPLES §10.6). The 0.8B probes came from
    `mlx-lm-lora` on the Mac; every 4B run comes from TRL/PEFT on a rented CUDA
    box. They disagree on all four things that matter, and the mismatch is
    total — this is not a filename tweak:

        | | mlx-lm-lora | PEFT / TRL |
        |---|---|---|
        | file    | adapters.safetensors | adapter_model.safetensors |
        | scale   | lora_parameters.scale | lora_alpha / r |
        | key     | <module>.lora_a | base_model.model.<module>.lora_A.weight |
        | A shape | [in, r] | **[r, in]** — transposed |
        | B shape | [r, out] | **[out, r]** — transposed |

    Getting the transpose wrong does not raise: `b.T @ a.T` and `b @ a` are both
    well-formed for square-ish LoRA shapes at r=32, and the result is a silently
    wrong model that still generates fluent text. The shape assertion in main()
    is what makes that impossible, so do not relax it.

    Normal form returned here: delta = scale * (b @ a), with a [r, in] and
    b [out, r] — PEFT's layout, because it is the one every future run produces.
    """
    peft = os.path.join(adapter_dir, "adapter_model.safetensors")
    mlx = os.path.join(adapter_dir, "adapters.safetensors")
    with open(os.path.join(adapter_dir, "adapter_config.json")) as f:
        acfg = json.load(f)

    if os.path.exists(peft):
        raw = load_file(peft)
        rank = acfg["r"]
        # PEFT stores alpha, not the scale; the scale IS alpha/r and there is
        # no second place that knows it. (r=32, alpha=64 -> 2.0 on every arm.)
        scale = acfg["lora_alpha"] / rank
        mods = {}
        for k in raw:
            if ".lora_A.weight" not in k and ".lora_B.weight" not in k:
                continue
            base, side = k.rsplit(".lora_", 1)
            base = base[len("base_model.model."):] if base.startswith(
                "base_model.model.") else base
            a, b = mods.setdefault(base, [None, None])
            if side.startswith("A"):
                mods[base][0] = raw[k]
            else:
                mods[base][1] = raw[k]
        missing = [m for m, (a, b) in mods.items() if a is None or b is None]
        if missing:
            raise SystemExit(f"PEFT adapter has half a pair for: {missing[:5]}")
        return ({m: (a, b) for m, (a, b) in mods.items()}, scale, rank, "peft")

    if os.path.exists(mlx):
        raw = load_file(mlx)
        lp = acfg["lora_parameters"]
        scale, rank = lp["scale"], lp["rank"]
        mods = {}
        for m in sorted({k.rsplit(".lora_", 1)[0] for k in raw}):
            # [in, r] -> [r, in] and [r, out] -> [out, r]; see the table above.
            mods[m] = (raw[m + ".lora_a"].T, raw[m + ".lora_b"].T)
        return (mods, scale, rank, "mlx")

    raise SystemExit(
        f"{adapter_dir}: no adapter_model.safetensors (PEFT) and no "
        f"adapters.safetensors (mlx-lm-lora) — nothing to fuse")


def resolve_weight_key(module: str, weights: dict) -> str:
    """Adapter module path -> snapshot tensor key, or die naming what was tried.

    The two trainers see different module trees for the SAME checkpoint. The
    snapshot stores `model.language_model.layers.N…` (Qwen3.5-4B is a
    multimodal checkpoint, so the text tower sits under `language_model`),
    while PEFT recorded `model.layers.N…` because the trainer loaded the text
    model directly, and mlx recorded `language_model.model.layers.N…`.

    Candidates are tried in order and the FIRST PRESENT one wins. A module that
    matches none is fatal: silently skipping it would fuse a partial adapter and
    report success (§18.3), which is indistinguishable from a model that trained
    badly.
    """
    stem = module[:-len(".weight")] if module.endswith(".weight") else module
    candidates = [
        stem + ".weight",
        stem.replace("language_model.model.", "model.language_model.") + ".weight",
    ]
    if stem.startswith("model.") and not stem.startswith("model.language_model."):
        candidates.append("model.language_model." + stem[len("model."):] + ".weight")
    for c in candidates:
        if c in weights:
            return c
    raise SystemExit(
        f"adapter module {module}: no snapshot weight. Tried:\n  "
        + "\n  ".join(candidates))


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--snapshot", required=True, help="HF snapshot dir (base model)")
    ap.add_argument("--adapter", required=True,
                    help="adapter dir — PEFT/TRL or mlx-lm-lora, auto-detected")
    ap.add_argument("--out", required=True, help="output dir for the fused model")
    args = ap.parse_args()

    adapters, scale, rank, flavour = load_adapter(args.adapter)
    modules = sorted(adapters)
    print(f"adapter flavour={flavour}  modules={len(modules)}  "
          f"rank={rank}  scale={scale}")

    os.makedirs(args.out, exist_ok=True)
    shards = []
    for src in sorted(glob.glob(os.path.join(args.snapshot, "*"))):
        name = os.path.basename(src)
        if os.path.isdir(src):
            continue
        if name.endswith(".safetensors"):
            shards.append(name)
        else:
            shutil.copy2(src, os.path.join(args.out, name))

    weights = {}
    shard_of = {}
    for name in shards:
        shard = load_file(os.path.join(args.snapshot, name))
        weights.update(shard)
        for k in shard:
            shard_of[k] = name

    fused_count = 0
    for m in modules:
        wkey = resolve_weight_key(m, weights)
        a, b = adapters[m]
        a = a.to(torch.float32)  # [r, in]   -- normal form, see load_adapter
        b = b.to(torch.float32)  # [out, r]
        w = weights[wkey]
        # THE ASSERTION THAT MAKES A TRANSPOSE BUG LOUD. Without it, mixing the
        # two adapter layouts produces a well-formed, fluent, WRONG model.
        if a.shape[0] != rank or b.shape[1] != rank or w.shape != (b.shape[0], a.shape[1]):
            raise SystemExit(
                f"shape mismatch at {m}: W{tuple(w.shape)} "
                f"a{tuple(a.shape)} b{tuple(b.shape)} rank={rank}")
        delta = scale * (b @ a)
        weights[wkey] = (w.to(torch.float32) + delta).to(w.dtype)
        fused_count += 1

    for name in shards:
        save_file({k: v for k, v in weights.items() if shard_of[k] == name},
                  os.path.join(args.out, name), metadata={"format": "pt"})

    print(f"fused {fused_count}/{len(modules)} modules (scale={scale}, rank={rank}) "
          f"into {len(shards)} shard(s) at {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
