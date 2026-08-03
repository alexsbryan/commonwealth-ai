#!/usr/bin/env python3
"""Did this LoRA adapter actually train? Exit 0 = yes.

    python3 scripts/check_adapter_trained.py <adapter-dir-or-file> [...]

    exit 0  TRAINED    B has finite nonzero values -- the adapter changes the model
    exit 1  NOT TRAINED  B is exactly zero -- the fused model IS the base model
    exit 3  DIVERGED   the weights are NaN/Inf -- the trainer worked and blew up
    exit 2  unusable input (no adapter found, no LoRA tensors)

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

WHY THERE ARE THREE VERDICTS AND NOT TWO. The first version of this script had
only TRAINED / NOT TRAINED, and on 2026-08-02 it called a NaN-diverged run
"NOT TRAINED -- B is exactly zero". It was not zero; it was NaN, and Python's
max() silently prefers the running value over NaN (`max(0.0, nan)` is 0.0,
because `nan > 0.0` is False). So a numerically unstable run -- a REAL trainer
doing REAL work at too high a learning rate -- was reported with the exact
words reserved for a structurally dead one. The two demand opposite responses:
NOT TRAINED means fix your framework, DIVERGED means fix your LR/stability.
Conflating them would send someone back down the MLX rabbit hole to debug a
gradient path that was never broken. NaN is checked FIRST, before any max().

Handles both naming conventions:
  MLX  (mlx-lm-lora):  ...lora_a / ...lora_b
  PEFT (HF/TRL):       ...lora_A.weight / ...lora_B.weight

FINGERPRINT WORTH KNOWING: if B is exactly 0 but A has drifted slightly from
its init, that is not partial learning -- it is AdamW's DECOUPLED weight decay
acting on a zero-gradient parameter (decay * 0 = 0 leaves B pinned). Seeing A
move while B stays exactly zero is the signature of that bug, not of progress.
"""
import pathlib
import sys

try:
    import numpy as np
    from safetensors import safe_open
except ImportError:
    sys.exit("need `safetensors` and `numpy` (pip install safetensors numpy)")

TRAINED = "trained"
NOT_TRAINED = "not_trained"
DIVERGED = "diverged"
UNUSABLE = "unusable"

EXIT = {TRAINED: 0, NOT_TRAINED: 1, UNUSABLE: 2, DIVERGED: 3}


def _strip_weight(k: str) -> str:
    return k[: -len(".weight")] if k.endswith(".weight") else k


def is_b_key(k: str) -> bool:
    return _strip_weight(k).endswith(("lora_b", "lora_B"))


def is_a_key(k: str) -> bool:
    return _strip_weight(k).endswith(("lora_a", "lora_A"))


def candidates(p: pathlib.Path) -> list[pathlib.Path]:
    if p.is_file():
        return [p]
    # prefer the canonical final adapter if present
    for name in ("adapters.safetensors", "adapter_model.safetensors"):
        f = p / name
        if f.exists():
            return [f]
    return sorted(p.glob("*.safetensors"))


def scan(path: pathlib.Path) -> dict:
    """The single source of truth for "did this adapter train?".

    Returns a verdict dict. `train_orpo_trl.py` calls this on its own output so
    a run states its verdict in its own summary; this file's __main__ is the
    standalone auditor for arbitrary directories. One rule, one place -- a
    second implementation is how the two drift apart and stop agreeing.
    """
    files = candidates(path)
    if not files:
        return {"dir": str(path), "verdict": UNUSABLE,
                "reason": "no .safetensors found"}

    amax = bmax = 0.0
    acount = bcount = bnonzero = 0
    nan_a = nan_b = 0
    for f in files:
        with safe_open(str(f), framework="np") as h:
            for k in h.keys():
                a, b = is_a_key(k), is_b_key(k)
                if not (a or b):
                    continue
                t = h.get_tensor(k)
                # NaN/Inf FIRST: max() would silently swallow it (see docstring).
                bad = not bool(np.isfinite(t).all())
                m = 0.0 if bad else float(np.abs(t).max())
                if b:
                    bcount += 1
                    nan_b += bad
                    bnonzero += m > 0
                    bmax = max(bmax, m)
                else:
                    acount += 1
                    nan_a += bad
                    amax = max(amax, m)

    if bcount == 0:
        return {"dir": str(path), "verdict": UNUSABLE,
                "reason": f"no lora_b/lora_B tensors ({acount} lora_a seen)"
                          " -- is this a LoRA adapter?"}

    if nan_a or nan_b:
        verdict = DIVERGED
    elif bmax > 0.0:
        verdict = TRAINED
    else:
        verdict = NOT_TRAINED

    return {
        "dir": str(path),
        "files": [f.name for f in files],
        "verdict": verdict,
        "trained": verdict == TRAINED,
        "lora_a_tensors": acount,
        "lora_b_tensors": bcount,
        "max_abs_a": amax,
        "max_abs_b": bmax,
        "b_nonzero": bnonzero,
        "nonfinite_a_tensors": nan_a,
        "nonfinite_b_tensors": nan_b,
    }


def report(v: dict) -> str:
    """Human-facing verdict. Each branch says what to DO, not just what is."""
    if v["verdict"] == UNUSABLE:
        return f"{v['dir']}: {v['reason']}"

    head = (f"{v['dir']}\n"
            f"  lora_A tensors {v['lora_a_tensors']:>4}   max|A| {v['max_abs_a']:.6e}\n"
            f"  lora_B tensors {v['lora_b_tensors']:>4}   max|B| {v['max_abs_b']:.6e}"
            f"   nonzero {v['b_nonzero']}/{v['lora_b_tensors']}")

    if v["verdict"] == TRAINED:
        return head + "\n  TRAINED -- the adapter changes the model"

    if v["verdict"] == DIVERGED:
        return head + (
            f"\n  non-finite: {v['nonfinite_a_tensors']}/{v['lora_a_tensors']} A,"
            f" {v['nonfinite_b_tensors']}/{v['lora_b_tensors']} B"
            "\n  DIVERGED -- the weights are NaN/Inf. This is NOT the zero-gradient"
            "\n  failure: the trainer computed real gradients and the run blew up"
            "\n  numerically. Do not debug the gradient path. Look at the loss and"
            "\n  grad_norm traces for the step they went to nan, then lower the LR,"
            "\n  add warmup, or clip. The reported max|B| of 0 is an artifact --"
            "\n  NaN loses every comparison, so it cannot be the running max.")

    out = head + (
        "\n  NOT TRAINED -- B is exactly zero, so W' == W and the fused"
        "\n  model IS the base model. Every metric from it is a base-model"
        "\n  metric. Do not score it, do not ship it, do not compare it.")
    if v["max_abs_a"] > 0:
        out += ("\n  (A is nonzero but that is just its random init, possibly"
                "\n   shrunk by AdamW weight decay -- it is not evidence of learning.)")
    return out


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        sys.exit(__doc__)
    worst = 0
    for arg in argv[1:]:
        v = scan(pathlib.Path(arg))
        print(report(v))
        worst = max(worst, EXIT[v["verdict"]])
    return worst


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
