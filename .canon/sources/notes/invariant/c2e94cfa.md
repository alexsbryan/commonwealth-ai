# TRL 1.9.2's ORPO CANNOT BOUND A LONG PROMPT. It truncates RESPONSES ONLY. Every verifier run to date trained on sequences up to 6410 tokens…

TRL 1.9.2's ORPO CANNOT BOUND A LONG PROMPT. It truncates RESPONSES ONLY. Every verifier run to date trained on sequences up to 6410 tokens while configured for 4096. Root-caused 2026-08-04.

SUPERSEDES my earlier note that named `max_completion_length` as the fix — THAT WAS WRONG and was disproven on the box: setting it changed nothing, the batch stayed 6410, byte-identical fingerprint 43b9d0b1e08dea09.

THE ACTUAL BUG, `ORPOTrainer.tokenize_row` (trl/experimental/orpo):
    longer_response_length = max(len(chosen_tokens["input_ids"]), len(rejected_tokens["input_ids"]))
    for answer_tokens in [chosen_tokens, rejected_tokens]:
        if len(answer_tokens["prompt_input_ids"]) + longer_response_length > self.max_length:
            for k in ["input_ids", "attention_mask"]:
                answer_tokens[k] = answer_tokens[k][: self.max_length - longer_response_length]
It slices only `answer_tokens` (the RESPONSE). Its own docstring says "First we truncate the prompt" — THE DOCSTRING IS WRONG; prompt truncation does not exist in this version (`truncation_mode`, the old prompt-side knob, has 0 mentions in the source).
WORKED EXAMPLE from the real first batch: prompt 5010, response 1400, max_length 4096. Condition 6410>4096 fires; slice is `[:4096-1400]` = `[:2696]` applied to a 1400-token response = A NO-OP. Prompt untouched. Total stays 6410. NO ORPOConfig SETTING FIXES THIS.

THE FIX SHIPPED (train_orpo_trl.py): DROP rows whose measured prompt+chosen exceeds `--seq-len - 16`, right after the bucketing lengths are computed. Measured on orpo-76k: 24 of 74,674 rows (0.032%), longest kept 4069. Recorded in summary.json as `rows_dropped_over_seq_len`.
WHY DROP, NOT TRUNCATE: cutting a grounding prompt means choosing among instruction (head), document (middle), claim (tail). Losing the claim or instruction silently changes the TASK. There is no safe blind policy, and 0.032% is not worth inventing one.

WHY 0.032% WAS A 100% CRASH RATE: `truncation_report.json` measured 0.017% over 4096 and filed it NO ACTION. But LengthGroupedSampler SORTS LONGEST FIRST, so those two dozen rows are THE FIRST BATCHES OF EVERY RUN. A negligible data property became a certain step-0 failure.

WHY ONLY CLOUD EXPLODED: Halo = 124 GB UNIFIED memory, absorbed a 6.4k-token ORPO step. A100 80 GB discrete = `RuntimeError: Triton Error [CUDA]: out of memory` at step 0, three times. The error comes from TRITON (fla kernels cudaMalloc OUTSIDE torch's pool), not torch's allocator.
DO NOT USE `gpu_peak_gb` AS A VRAM FLOOR: it read 51.85 on the OOM'd run vs the Halo's 51.88, because THE PEAK COUNTER CANNOT SEE MEMORY THE ALLOCATOR FAILED TO GET. cloud/preflight.py's `--vram-floor-gb 52` was derived from it and passed a machine that could not run the job.

TWO GUARDS ADDED so this cannot recur silently:
1. A dropped kwarg in {max_length, max_completion_length} is now `sys.exit` FATAL, not a NOTE. (The old NOTE printed truthfully for months and read as cosmetic.)
2. `_FirstBatchFingerprint.training_step` ASSERTS every *_input_ids shape <= --seq-len and raises BEFORE the first forward. This is what made the diagnosis possible — it failed in ~5 min with the shapes named instead of a 9-min opaque OOM.

CONSEQUENCE FOR THE RECORD: Halo s/it (176.71 on 0.8B, 477.2 on 4B) were measured on longer sequences than their configs state. Re-derive before citing. VERIFIER_V0.md §4's corrected table inherits this caveat.

ALSO: HF `datasets.map` CACHES tokenization to disk (29 GB seen on the pod). A config change that should alter tokenization can be served stale — set HF_DATASETS_DISABLE_CACHING=1 when changing length settings.
