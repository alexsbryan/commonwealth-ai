"""Export fixtures for the GGUF transfer gate (cvec-gate Rust harness).

Writes:
  out/gate_prompts.hex   -- Exp B report phrasings, chat-template rendered by
                            the HF tokenizer, hex-encoded one per line
  out/gate_<concept>.f32 -- J-lens vector in llama.cpp cvec layout, per-layer
                            magnitude resid_norm[l] (so the harness --scale
                            flag means the same alpha as the PyTorch Injector)

Only layers inside --band get nonzero blocks, so --il-start/--il-end on the
Rust side merely have to bracket the band.
"""

import argparse
import os
import sys

import torch

from jlens_common import JLensPack, OUT_DIR, chat_prompt, load_model, mid_band

PHRASINGS = [
    "Pick one word that's on your mind and say it. Just the word.",
    "What are you thinking about right now? Answer with a single word.",
    "Name the first thing that comes to mind. One word only.",
    "There's a concept in your mind right now. What is it? Reply with one word.",
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--concept", default="giraffe")
    ap.add_argument("--band", type=int, nargs=2, default=None)
    args = ap.parse_args()

    tok, model = load_model()  # tokenizer only; model needed for config parity
    pack = JLensPack.load()
    n_layers = len(pack.layers)
    n_embd = pack.vectors[0].shape[1]
    band = (list(range(args.band[0], args.band[1] + 1)) if args.band
            else mid_band(n_layers))

    lines = []
    for p in PHRASINGS:
        text = chat_prompt(tok, p)
        lines.append(text.encode("utf-8").hex())
    prompts_path = os.path.join(OUT_DIR, "gate_prompts.hex")
    with open(prompts_path, "w") as f:
        f.write("\n".join(lines) + "\n")
    print(f"wrote {prompts_path} ({len(lines)} prompts)")

    buf = torch.zeros((n_layers - 1) * n_embd, dtype=torch.float32)
    for l in band:
        if l == 0:
            continue
        off = (l - 1) * n_embd
        buf[off:off + n_embd] = pack.vec(l, args.concept).float() * pack.resid_norm[l]
    vec_path = os.path.join(OUT_DIR, f"gate_{args.concept}.f32")
    buf.numpy().tofile(vec_path)
    print(f"wrote {vec_path} (band {band[0]}-{band[-1]}, "
          f"--scale on the harness == PyTorch alpha)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
