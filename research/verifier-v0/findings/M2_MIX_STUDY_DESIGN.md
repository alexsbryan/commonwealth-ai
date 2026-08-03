# M2 mix study — why 400 iterations, and how to scale up without redoing it

**The study answers one binary question:** does Stream B belong in the ~2-week
M3 4B training run? Nothing here is a quality claim about the 0.8B.

## The decision: 400 iters/arm, staged

| iters/arm | arm A | arm AB | + scoring | total |
|---|---|---|---|---|
| **400** | 6.1 h | ~5.6 h | 1.4 h | **~13 h — one overnight** |
| 800 | 12.2 h | ~11.1 h | 1.4 h | ~25 h |
| 2,334 (1.00 epoch of A) | 35.5 h | ~32.4 h | 1.4 h | ~69 h |

At 54.79 s/it measured for A and ~50 s/it estimated for AB (B's rows are
shorter: p50 985 vs 1,816 tokens). Effective batch 32, so 400 iters = 12,800
preference pairs seen per arm.

400 wins on ROI **because it is resumable** (below), and because of the failure
mode it protects against: if the recipe turns out not to learn past the M0
probe, an 800-iter pair burns 12 extra hours discovering exactly the same
thing. Going short first costs nothing when it works and saves half a day when
it doesn't.

## Yes, you can scale up — with two conditions

`--resume-adapter-file` works, but it is a **warm start, not a checkpoint
restore**. Verified in `mlx_lm_lora/train.py`:

- `:546` — `model.load_weights(file, strict=False)` loads **adapter weights
  only**. No optimizer state (Adam moments reset to zero), no step counter.
- `:1129` — `np.random.seed(args.seed)` runs at startup, and
  `orpo_trainer.py:116` re-permutes from that seeded RNG. **Resuming with the
  same seed replays the same batch order from index 0** — the continuation
  would re-train on batches it has already seen.
- `:550` — LR is constant (`lr_schedule` is None, 1e-4 flat). This is the one
  thing that would have made resume invalid, and it is fine.

So the two conditions for extending:

1. **Change the seed on each continuation leg** (e.g. `SEED=18` for 400→800),
   so the second leg draws fresh batches instead of replaying leg 1.
2. **Extend both arms identically.** The Adam-moment reset and the sampling
   change are then *common-mode* — they hit A and AB the same way and cancel
   out of the A-vs-AB contrast, which is the only quantity this study reports.

A resumed 400+400 checkpoint is not bit-identical to a clean 800-iter run. That
does not matter here: the study reports a **difference between two arms**, not
an absolute score. It would matter if you tried to publish the checkpoint's
BAcc as a quality number — don't.

To extend:

```sh
# leg 2: 400 -> 800, both arms, fresh seed
ITERS=400 SEED=18 RESUME=1 ./scripts/run_mix_study.sh
```

(`RESUME` is not wired yet — leg 2 needs `--resume-adapter-file
runs/mix-study/<arm>/adapters/adapters.safetensors` added to `train_arm`, and
the arm dirs renamed so leg 1's results aren't overwritten. Deliberately left
out: wiring an untested resume path into a script that has to survive an
unattended night is how you lose the night.)

## Stage 0 is not optional

The run scores the **existing 100-iter probe GGUF** on the same full 2,200-item
card under the same protocol first (~27 min).

Without it, "both arms scored the same" has two readings that cannot be told
apart: Stream B doesn't help, or nothing trained at all. With it, the A-arm's
lift over the 100-iter line separates them. `summarize.py` prints that lift and
refuses to call a null interpretable when the lift is under 1 BAcc.

## What is already de-risked

- **Truncation is not a confound.** At `max_seq_length` 4096: A exceeds it on
  0.017% of rows, B on 0.000% (`findings/truncation_report.json`). The mix
  study is not secretly measuring damaged targets.
- **Batch composition is safe.** `iterate_orpo_batches` length-sorts, so B
  concentrates into its own micro-batches — but batches are re-permuted each
  epoch and one optimizer step averages 8 of them, so every gradient step
  mixes both streams (`M2_MAC_MIGRATION_OUTCOME.md §4`).
- **The scoring chain is proven end to end**: train → `fuse_lora_manual.py` →
  `convert_hf_to_gguf` q8_0 → llama-server → `eval_grounding.py`. The M0 probe
  produced `probe-orpo-0.8b-q8.gguf` this way and the eval-leg probe scored it.
- **`--no-think` is mandatory, not a tuning choice.** Thinking-on yields zero
  parseable verdicts on the 0.8B (55/55 items hit the token cap).
- **Q8_0 quantization noise is common-mode** across both arms, so it cancels
  from the contrast. That is why the proven GGUF path was kept over serving
  fused MLX directly.

## Reading the result

Headline is `macro_avg_bacc_tolerant`; strict is reported alongside as the
floor production actually experiences. A strict BAcc below 50 means **no
measurement**, not "worse than chance" — parse failures score as the wrong
label, so they drive strict toward 0 rather than 50.

## Files

Tracked (in git):

- `scripts/run_mix_study.sh` — the orchestrator. Serial, RSS-tripwired at
  40 GB, skips any stage whose output already exists (safe to re-run after a
  crash).
- `scripts/summarize_mix_study.py` — verdict table. Safe to run mid-flight.

Untracked (`runs/` is gitignored — these are artifacts):

- `runs/mix-study/orchestrator.log` — the single narrative log; per-stage logs
  (`train.log`, `fuse.log`, `convert.log`, `*.server.log`, `*.eval.log`) sit
  beside it.
- `runs/mix-study/{ref-probe100,A,AB}/` — adapters, fused models, GGUFs, and
  `eval/summary.json` per arm.
