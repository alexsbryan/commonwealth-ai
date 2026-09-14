# THE ADAPTER GATE CANNOT SEE NaN UNLESS IT CHECKS FOR IT FIRST — AND IT REPORTED A DIVERGED RUN AS "NOT TRAINED". Found and fixed…

THE ADAPTER GATE CANNOT SEE NaN UNLESS IT CHECKS FOR IT FIRST — AND IT REPORTED A DIVERGED RUN AS "NOT TRAINED". Found and fixed 2026-08-02, verifier-v0, `runs/ratchet-25`.

THE BUG. `check_adapter_trained.py` computed `max(bmax, float(abs(t).max()))`. When a tensor is NaN that inner max is NaN, and Python's `max()` KEEPS THE RUNNING VALUE because every comparison against NaN is False:
    max(0.0, float('nan')) == 0.0
    float('nan') > 0.0     == False   # so b_nonzero never increments
So an adapter whose 372 tensors were ALL NaN reported `max|B| 0.000000e+00, nonzero 0/186` and printed the verdict reserved for a structurally dead trainer: "NOT TRAINED -- B is exactly zero, so W' == W and the fused model IS the base model."

WHY THIS MATTERS MORE THAN A NORMAL BUG. The two failures demand OPPOSITE responses. NOT TRAINED means the framework never delivered a gradient — fix the framework (this is the MLX no-op, note 3d9a9ce4). DIVERGED means the trainer computed real gradients and the run blew up numerically — fix the LR/warmup/clipping. Reading one as the other sends the next session to debug a gradient path that was never broken. The gate is the single artifact this whole handoff rests on; it had a blind spot in the same shape as the failure it was built to catch.

THE FIX. Finiteness is checked BEFORE any max(). Three verdicts, distinct exit codes: 0 TRAINED, 1 NOT TRAINED, 3 DIVERGED, 2 unusable. `scan()` is now the single shared rule — `train_orpo_trl.py` imports it instead of carrying a second copy, because two implementations of a gate is how the two stop agreeing. Verified in all three directions: real trained adapter -> 0, the NaN adapter -> 3, a synthetic random-A/zero-B adapter reproducing the MLX fingerprint -> 1.

SECOND LESSON, INDEPENDENT OF THE BUG: A 5-STEP GATE IS NOT A STABILITY TEST. The 5-step run PASSED; the 25-step run at identical settings hit NaN at step 11 (loss 1.630 -> 0.687 healthy through step 10, grad_norm 2.3-4.4 with a 15.06 spike at step 4, then nan). Five steps proves B leaves zero. It does not prove the config is stable, and the handoff's §4 should not be read as if it did.
