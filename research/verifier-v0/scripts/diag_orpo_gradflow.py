#!/usr/bin/env python3
"""Prove where the ORPO gradient goes. No trainer, no mx.compile.

Runs the SAME batch through two structures and reports the gradient norm that
reaches the LoRA parameters:

  BROKEN (mlx-lm-lora 3.0.0): forward runs OUTSIDE nn.value_and_grad, which is
      handed precomputed logps. Nothing in the differentiated function touches
      a model parameter.
  FIXED: forward runs INSIDE, mirroring sft_trainer.py:473.

Expected: broken -> 0.0 for every parameter; fixed -> nonzero.

    .venv/bin/python scripts/diag_orpo_gradflow.py
"""
import mlx.core as mx
import mlx.nn as nn
import numpy as np
from mlx.utils import tree_flatten

from mlx_lm.utils import load
from mlx_lm.tuner.utils import linear_to_lora_layers
from mlx_lm_lora.trainer.orpo_trainer import get_logps, orpo_loss

MODEL = "Qwen/Qwen3.5-0.8B"
SEQ = 64
BATCH = 1
BETA = 0.1


def grad_norms(grad):
    """Total |grad| over lora_a and lora_b separately."""
    a = b = 0.0
    for k, v in tree_flatten(grad):
        if not isinstance(v, mx.array):
            continue
        s = float(mx.abs(v).sum())
        if k.endswith("lora_a"):
            a += s
        elif k.endswith("lora_b"):
            b += s
    return a, b


def main():
    print(f"loading {MODEL} ...", flush=True)
    model, tokenizer = load(MODEL)
    model.freeze()
    linear_to_lora_layers(model, -1, {"rank": 32, "dropout": 0.0, "scale": 2.0})
    mx.eval(model.parameters())

    n_train = sum(v.size for _, v in tree_flatten(model.trainable_parameters()))
    print(f"trainable params: {n_train/1e6:.3f}M\n", flush=True)

    # A deterministic toy batch. Content is irrelevant -- we are measuring
    # whether the gradient reaches the parameters at all, not what it says.
    rng = np.random.default_rng(17)
    vocab = 30000  # comfortably inside any Qwen vocab; content is irrelevant here
    chosen = mx.array(rng.integers(0, vocab, (BATCH, SEQ)).astype(np.int32))
    rejected = mx.array(rng.integers(0, vocab, (BATCH, SEQ)).astype(np.int32))
    cmask = mx.ones((BATCH, SEQ), dtype=mx.bool_)
    rmask = mx.ones((BATCH, SEQ), dtype=mx.bool_)
    pref = mx.ones((BATCH,))

    # ---------------------------------------------------------- BROKEN shape
    def broken_wrapper(cl, clm, rl, rlm, cm, rm, ps):
        return orpo_loss(
            chosen_logps=cl, chosen_logits_mean=clm,
            rejected_logps=rl, rejected_logits_mean=rlm,
            chosen_masks=cm, rejected_masks=rm,
            preference_scores=ps, beta=BETA,
        )

    cl, clm = get_logps(model, chosen, cmask)
    rl, rlm = get_logps(model, rejected, rmask)
    _, gb = nn.value_and_grad(model, broken_wrapper)(
        cl, clm, rl, rlm, cmask, rmask, pref
    )
    mx.eval(gb)
    a_b, b_b = grad_norms(gb)
    print(f"BROKEN (forward outside): sum|grad lora_a|={a_b:.6e}  sum|grad lora_b|={b_b:.6e}")

    # ----------------------------------------------------------- FIXED shape
    def fixed_wrapper(model, chosen, rejected, cm, rm, ps):
        cl, clm = get_logps(model, chosen, cm)
        rl, rlm = get_logps(model, rejected, rm)
        return orpo_loss(
            chosen_logps=cl, chosen_logits_mean=clm,
            rejected_logps=rl, rejected_logits_mean=rlm,
            chosen_masks=cm, rejected_masks=rm,
            preference_scores=ps, beta=BETA,
        )

    _, gf = nn.value_and_grad(model, fixed_wrapper)(
        model, chosen, rejected, cmask, rmask, pref
    )
    mx.eval(gf)
    a_f, b_f = grad_norms(gf)
    print(f"FIXED  (forward inside) : sum|grad lora_a|={a_f:.6e}  sum|grad lora_b|={b_f:.6e}")

    print()
    ok = b_b == 0.0 and b_f > 0.0
    print("VERDICT:", "confirmed -- forward must be inside value_and_grad" if ok
          else "INCONCLUSIVE -- see numbers above")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
