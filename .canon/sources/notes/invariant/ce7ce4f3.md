# EVERY VERIFIER TRAINING RUN TO DATE TRAINED ON SEQUENCES UP TO 6410 TOKENS WHILE CONFIGURED FOR 4096. Root-caused 2026-08-04. This…

EVERY VERIFIER TRAINING RUN TO DATE TRAINED ON SEQUENCES UP TO 6410 TOKENS WHILE CONFIGURED FOR 4096. Root-caused 2026-08-04. This retro-qualifies the Halo timings, including the 477 s/it that redirected M3 to rented GPU.

THE EVIDENCE: `first batch this leg: chosen_input_ids(1, 6410) prompt_input_ids(1, 5010) rejected_input_ids(1, 5584)` on a run invoked with --seq-len 4096. That line has been printed by every run since the fingerprint landed and was read as informational.

THE MECHANISM — TRL 1.9.2 truncates in TWO places and needs TWO settings:
  responses: tokenizer(chosen/rejected, truncation=True, max_length=self.max_completion_length)
  prompt:    if len(prompt)+longer_response > self.max_length: prompt = prompt[:max_length-longer_response]
`train_orpo_trl.py` set `max_length` and `max_prompt_length`. **`max_prompt_length` DOES NOT EXIST in this TRL — 0 mentions in ORPOTrainer source.** `max_completion_length` (which does exist) was NEVER SET, so responses were unbounded and the prompt clamp had no finite response length to subtract. Net: no effective bound.

WHY IT WAS INVISIBLE FOR MONTHS: the signature filter (`train_orpo_trl.py`, `sig = inspect.signature(ORPOConfig.__init__).parameters`) silently drops unknown kwargs and prints `NOTE: ORPOConfig does not accept ['max_prompt_length'] — dropped.` A truthful one-line NOTE that nobody had reason to read as "your truncation policy does not exist here". The filter exists so a TRL rename does not crash a 19-hour run; the cost is that a LENGTH BOUND can vanish and the run still exits 0 having trained a different experiment.

WHY IT ONLY EXPLODED ON CLOUD: the Halo has 124 GB UNIFIED memory and absorbed a 6.4k-token ORPO step. An 80 GB A100 does not: `RuntimeError: Triton Error [CUDA]: out of memory` at step 0, THREE TIMES. Note the error is from TRITON (fla's gated-deltanet kernels cudaMalloc outside torch's pool), not torch's allocator — torch had reserved the card and left Triton nothing.
ALSO NOTE `gpu_peak_gb` read 51.85 on the OOM'd run vs the Halo's 51.88. THE PEAK COUNTER CANNOT SEE MEMORY THE ALLOCATOR FAILED TO GET, so it is NOT a VRAM floor for a discrete card. I set cloud/preflight.py's --vram-floor-gb 52 from that number and it passed a machine that could not run the job.

LENGTH BUCKETING MAKES STEP 1 THE WORST CASE: LengthGroupedSampler sorts longest-first, so the FIRST batch is the dataset maximum (6409 for orpo-76k). There is no warm-up into the failure — which is also why the first batch is the cheapest place to catch it.

THE FIXES (all in train_orpo_trl.py):
1. `max_completion_length=args.seq_len // 2` replaces the nonexistent `max_prompt_length`.
2. A dropped kwarg in {max_length, max_completion_length} is now `sys.exit` FATAL, not a NOTE. Everything else degrades gracefully; a lost length bound makes it a DIFFERENT run.
3. `_FirstBatchFingerprint.training_step` now ASSERTS every *_input_ids shape <= --seq-len and raises before the first forward.

CONSEQUENCE FOR THE RECORD: Halo s/it figures (176.71 on 0.8B, 477.2 on 4B) were measured on longer sequences than their configs state. Re-derive before citing them as the local baseline. VERIFIER_V0.md §4's corrected table inherits this caveat.
