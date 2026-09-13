# MLX-LM-LORA'S --resume-adapter-file IS A WARM START, NOT A CHECKPOINT RESTORE — AND WITH THE SAME SEED IT REPLAYS THE BATCHES IT ALREADY…

MLX-LM-LORA'S `--resume-adapter-file` IS A WARM START, NOT A CHECKPOINT RESTORE — AND WITH THE SAME SEED IT REPLAYS THE BATCHES IT ALREADY TRAINED ON. Verified 2026-08-02 by reading the installed package (`.venv/lib/python3.13/site-packages/mlx_lm_lora/`), while sizing the verifier-v0 M2 mix study.

Three facts, each with the line that proves it:

- `train.py:546` — `model.load_weights(file, strict=False)` loads adapter weights only. No optimizer state (Adam m/v reset to zero), no step counter, no data cursor.
- `train.py:1129` — `np.random.seed(args.seed)` runs at startup, and `orpo_trainer.py:116` draws the batch order from `np.random.permutation` off that seeded RNG. So a resume with the SAME `--seed` regenerates the identical permutation from index 0: the continuation re-trains on batches already seen. Pass a DIFFERENT seed on each continuation leg.
- `train.py:550` — `lr = build_schedule(...) if args.lr_schedule else args.learning_rate`. With no schedule configured the LR is flat. This is the one thing that would have made resume outright invalid (resuming into the wrong point of a decay curve) and it is fine here.

WHY THIS STILL PERMITS A STAGED STUDY. A resumed 400+400 checkpoint is not bit-identical to a clean 800-iter run. That does not matter for an A/B contrast, because the Adam reset and the sampling change are COMMON-MODE — they hit both arms identically and cancel out of the difference. Two conditions: change the seed per leg, and extend BOTH arms or neither. It WOULD matter if the checkpoint's absolute BAcc were reported as a quality number.

`orpo_trainer.py:97` also length-sorts the dataset before cutting batches, so micro-batches are length-homogeneous and a short stream (verifier Stream B, p50 985 tok vs A's 1,816) concentrates into its own micro-batches. Not a curriculum confound: order is re-permuted each epoch and `--gradient-accumulation-steps 8` averages 8 micro-batches per optimizer step.
