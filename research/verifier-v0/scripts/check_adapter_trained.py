#!/usr/bin/env python3
"""Did this LoRA adapter actually train? Exit 0 = yes, 1 = no.

    python3 scripts/check_adapter_trained.py <adapter-dir-or-file> [...]

WHY THIS EXISTS. On 2026-08-02 the verifier-v0 project discovered that four
days of MLX ORPO runs had produced NOTHING: mlx-lm-lora 3.0.0 differentiates a
loss function that never touches a model parameter, so every gradient was
structurally zero. Every surface looked healthy -- "Trainable parameters:
2.877%" printed, loss varied per batch, val accuracy was plausible -- and the
fused checkpoints were byte-equivalent to the base model. Two independently
trained checkpoints scored +0.05 BAcc apart with 7 of 11 benchmark subsets
BIT-IDENTICAL before anyone looked at the weights.

THE CHECK. LoRA computes W' = W + scale*(B @ A) with B ZERO-INITIALISED and A
random. If B is still exactly zero after training, the adapter is a no-op and
the fused model IS the base model. Two seconds, reads one file, catches an
entire class of silent failure.

Handles both naming conventions:
  MLX  (mlx-lm-lora):  ...lora_a / ...lora_b
  PEFT (HF/TRL):       ...lora_A.weight / ...lora_B.weight

FINGERPRINT WORTH KNOWING: if B is exactly 0 but A has drifted slightly from
its init, that is not partial learning -- it is AdamW's DECOUPLED weight decay
acting on a zero-gradient parameter (decay * 0 = 0 leaves B pinned). Seeing A
move while B stays exactly zero is the signature of this bug, not of progress.
"""
import sys
import pathlib

try:
    from safetensors import safe_open
except ImportError:
    sys.exit("need `safetensors` (pip install safetensors)")

B_SUFFIXES = ("lora_b", "lora_b.weight", "lora_B", "lora_B.weight")


def is_b_key(k: str) -> bool:
    kl = k.rstrip(".weight") if k.endswith(".weight") else k
    return kl.endswith("lora_b") or kl.endswith("lora_B")


def is_a_key(k: str) -> bool:
    kl = k.rstrip(".weight") if k.endswith(".weight") else k
    return kl.endswith("lora_a") or kl.endswith("lora_A")


def candidates(p: pathlib.Path):
    if p.is_file():
        return [p]
    hits = sorted(p.glob("*.safetensors"))
    # prefer the canonical final adapter if present
    for name in ("adapters.safetensors", "adapter_model.safetensors"):
        f = p / name
        if f.exists():
            return [f]
    return hits


def check(path: pathlib.Path) -> bool:
    files = candidates(path)
    if not files:
        print(f"{path}: no .safetensors found")
        return False

    amax = bmax = 0.0
    acount = bcount = bnonzero = 0
    for f in files:
        with safe_open(str(f), framework="np") as h:
            for k in h.keys():
                if is_b_key(k):
                    m = float(abs(h.get_tensor(k)).max())
                    bmax = max(bmax, m)
                    bcount += 1
                    bnonzero += m > 0
                elif is_a_key(k):
                    amax = max(amax, float(abs(h.get_tensor(k)).max()))
                    acount += 1

    if bcount == 0:
        print(f"{path}: no lora_b/lora_B tensors found "
              f"({acount} lora_a seen) -- is this a LoRA adapter?")
        return False

    ok = bmax > 0.0
    print(f"{path}")
    print(f"  lora_A tensors {acount:>4}   max|A| {amax:.6e}")
    print(f"  lora_B tensors {bcount:>4}   max|B| {bmax:.6e}   nonzero {bnonzero}/{bcount}")
    if ok:
        print("  TRAINED -- the adapter changes the model")
    else:
        print("  NOT TRAINED -- B is exactly zero, so W' == W and the fused")
        print("  model IS the base model. Every metric from it is a base-model")
        print("  metric. Do not score it, do not ship it, do not compare it.")
        if amax > 0:
            print("  (A is nonzero but that is just its random init, possibly")
            print("   shrunk by AdamW weight decay -- it is not evidence of learning.)")
    return ok


def main(argv):
    if len(argv) < 2:
        sys.exit(__doc__)
    results = [check(pathlib.Path(a)) for a in argv[1:]]
    return 0 if all(results) else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
