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


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--snapshot", required=True, help="HF snapshot dir (base model)")
    ap.add_argument("--adapter", required=True, help="mlx-lm-lora adapter dir")
    ap.add_argument("--out", required=True, help="output dir for the fused model")
    args = ap.parse_args()

    with open(os.path.join(args.adapter, "adapter_config.json")) as f:
        acfg = json.load(f)
    lp = acfg["lora_parameters"]
    scale, rank = lp["scale"], lp["rank"]

    adapters = load_file(os.path.join(args.adapter, "adapters.safetensors"))
    modules = sorted({k.rsplit(".lora_", 1)[0] for k in adapters})

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
        wkey = m.replace("language_model.model.", "model.language_model.") + ".weight"
        if wkey not in weights:
            raise SystemExit(f"adapter module {m}: no snapshot weight {wkey}")
        a = adapters[m + ".lora_a"].to(torch.float32)  # [in, r]
        b = adapters[m + ".lora_b"].to(torch.float32)  # [r, out]
        w = weights[wkey]
        if a.shape[1] != rank or b.shape[0] != rank or w.shape != (b.shape[1], a.shape[0]):
            raise SystemExit(f"shape mismatch at {m}: W{tuple(w.shape)} a{tuple(a.shape)} b{tuple(b.shape)}")
        delta = scale * (b.T @ a.T)
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
