# External baseline table — M0 (2026-07-30)

Seed of the §1 external table (VERIFIER_V0.md §6 card item 1). All numbers in
this file are **our harness's** measurements — `scripts/eval_grounding.py`
against a local llama-server — not transcriptions of published results. That
is deliberate: the bar our checkpoints must clear is the adopt candidate
*measured identically*, so harness-induced depression (parse rule, sampling,
quantization) applies to both sides of every future comparison.

## HalluGuard-Qwen3-4B (GGUF) on LLM-AggreFact

Setup: `lrsbrgrn/HalluGuard-Qwen3-4B-GGUF` (Q4-class, 2.5 GB) via llama-server
:8089; 200 rows/subset, seed 17; verdict parsed after last `</think>`;
**parse failure scores as forced-wrong**; transport errors excluded from n.
Run: `runs/baseline-halluguard-gguf-aggrefact/` (exit=0, 2026-07-30 00:26).

Two BAcc columns: **strict** scores parse failures as wrong (the deployment
floor — a gate cannot act on an unparseable verdict); **excl-pf** drops them
(the model-quality number, closest to published methodology).

| Subset | BAcc strict | BAcc excl-pf | TPR (supported) | TNR (hallucinated) | parse-fail | n |
|---|---|---|---|---|---|---|
| AggreFact-CNN | 54.75 | 58.88 | 90.21 | 19.30 | 16 | 200 |
| AggreFact-XSum | 66.35 | 73.31 | 63.00 | 69.70 | 19 | 199 |
| ClaimVerify | 69.79 | 81.59 | 79.17 | 60.42 | 28 | 192 |
| ExpertQA | 53.00 | 57.73 | 43.00 | 63.00 | 16 | 200 |
| FactCheck-GPT | 68.23 | 71.24 | 45.45 | 91.00 | 11 | 199 |
| Lfqa | 84.00 | 90.38 | 88.00 | 80.00 | 14 | 200 |
| RAGTruth | 81.00 | 86.61 | 84.00 | 78.00 | 13 | 200 |
| Reveal | 86.50 | 88.29 | 85.00 | 88.00 | 4 | 200 |
| TofuEval-MediaS | 70.50 | 73.81 | 86.00 | 55.00 | 8 | 200 |
| TofuEval-MeetB | 74.50 | 82.15 | 85.00 | 64.00 | 19 | 200 |
| Wice | 69.88 | 80.36 | 55.56 | 84.21 | 26 | 194 |
| **Macro avg** | **70.77** | **76.76** | | | 159 (7.6%) | 2073 scored |

TPR/TNR columns are the strict run's. Errors 16 (excluded from n).

## Measured vs published

| Metric | Strict | Excl-pf | Published | Delta (excl-pf) |
|---|---|---|---|---|
| LLM-AggreFact 11-subset avg | 70.77 | 76.76 | 75.7 | **+1.1** |
| RAGTruth | 81.00 | 86.61 | 84.0 | **+2.6** |
| RAGTruth (60-row smoke, ref) | 83.3 | — | 84.0 | — |

**The parse-fail policy accounts for essentially the entire headline gap.**
Excluding unparseable verdicts, the Q4 GGUF matches or slightly exceeds the
published fp16 numbers — so Q4 quantization loss on this task is ~nil, and
200-row sampling noise is minor. The strict column stays the headline for
gate-sizing purposes: in production an unparseable verdict is a failed
verification, so strict is the floor the fleet actually experiences.
(Strict is also *harsher* than production, which would map parse failures to
one consistent side rather than adversarially to the wrong label.)

> **AMENDED 2026-08-02 — most "parse failures" were correct verdicts.**
> `M2_MAC_MIGRATION_OUTCOME.md §7` traced the failure mode: `ANSWER_RE`
> demanded a fully well-formed `<answer>` block, so a right classification
> wrapped in a typo'd closing tag was discarded. On the 0.8B probe **only 19%
> of responses were strictly well-formed**, and a tolerant parser recovered
> 130 of 132 strict failures. The excl-pf column above therefore *excluded*
> rows that should have been *scored* — meaning 76.76 is a mix of "the model
> was right" and "the model was unreadable", and the true 4B number sits at
> or above it.
>
> **These 4B numbers were not re-derived**, because `results.jsonl` stored
> only parsed verdicts and not the raw text, so re-scoring needs a ~6 h
> re-run rather than an offline re-parse. `eval_grounding.py` now persists
> `responses.jsonl` by default so this is never true again. Treat the table
> above as a floor for the 4B, and do not compare it to any run made with
> `--no-think` — that is a different protocol, and `summary.json` now records
> which one a run used.

Not a contributor: contamination (0 canaries, 34 shared-source rows dropped
from Stream A — `findings/contamination_report.json`).

## HalluGuard-Qwen3-4B (GGUF) on FaithBench-750

Same server, same harness, auto-chained after the aggrefact run
(`runs/baseline-halluguard-gguf-faithbench/`, exit=0, 2026-07-30 03:0x).

| Metric | Strict | Excl-pf | Reference points |
|---|---|---|---|
| FaithBench BAcc | **49.57** | **56.88** | HHEM floor 52.6 · frontier-judge 68.8 |

TPR 82.96 / TNR 16.17 (strict) · parse-fail 124/750 (16.5%) · errors 0.

**The spec's §6 prediction is confirmed: the adopt candidate collapses on
FaithBench.** Strict is below the HHEM small-classifier floor; even excl-pf
(56.88) clears that floor by only 4 points and sits 12 under the
frontier-judge band. The failure signature is the same call-everything-
supported collapse as AggreFact-CNN (TNR 16). Two additional observations:

- The parse-fail rate on FaithBench (16.5%) is 2.2× the aggrefact rate
  (7.6%) — hard examples break the output format more often, so the strict
  penalty compounds exactly where the benchmark is hardest.
- This is the decisive argument against pure adoption (spec §0): the
  candidate's headline LLM-AggreFact avg hides a FaithBench cliff. Our M2/M3
  runs must report both, and the mix-study gate (spec: no checkpoint that
  trades FaithBench for LLM-AggreFact) is the right guard.

## Reading the per-subset shape

- **AggreFact-CNN (54.75) is a TNR collapse** — 90/19 split means the model
  calls nearly everything supported on CNN-style summaries. ExpertQA (53.00)
  fails the opposite way (TPR 43). These two subsets are where the published
  75.7-avg model must also be weak-ish (a 4B can't be >75 everywhere and
  average 75.7), but our harness likely amplifies it.
- The strong subsets (Reveal 86.5, Lfqa 84.0, RAGTruth 81.0) are the
  RAG-shaped ones — closest to our production distribution. Consistent with
  the spec's premise that the adopt candidate is credible *for the gate* even
  where headline-avg lags.
- Balanced-looking subsets with low BAcc (XSum, ClaimVerify) lose on both
  sides at once — genuinely hard, not a threshold artifact.

## MiniCheck-Flan-T5-Large (0.77B classifier)

`lytang/MiniCheck-Flan-T5-Large` via `scripts/eval_minicheck.py` —
transformers on MPS, fp32, replicating the upstream MiniCheck inference
path exactly (self-test reproduces the model-card demo probabilities to six
decimals; the script refuses to run the eval if that check fails). Same
`load_items` sampling as every other row (200/subset, seed 17; FaithBench
750). A classifier emits no free text, so there is no parse-fail channel —
one BAcc column, comparable to the *strict* column above (nothing is
forfeited to format).

| Subset | BAcc | TPR | TNR | n |
|---|---|---|---|---|
| AggreFact-CNN | 66.86 | 88.11 | 45.61 | 200 |
| AggreFact-XSum | 74.50 | 78.00 | 71.00 | 200 |
| ClaimVerify | 76.00 | 92.00 | 60.00 | 200 |
| ExpertQA | 54.00 | 40.00 | 68.00 | 200 |
| FactCheck-GPT | 71.50 | 61.00 | 82.00 | 200 |
| Lfqa | 85.50 | 91.00 | 80.00 | 200 |
| RAGTruth | 76.00 | 71.00 | 81.00 | 200 |
| Reveal | 88.00 | 93.00 | 83.00 | 200 |
| TofuEval-MediaS | 71.50 | 83.00 | 60.00 | 200 |
| TofuEval-MeetB | 76.50 | 86.00 | 67.00 | 200 |
| Wice | 78.50 | 69.00 | 88.00 | 200 |
| **Macro avg** | **74.44** | | | 2200 |
| FaithBench | **51.88** | 52.73 | 51.03 | 750 |

Published LLM-AggreFact avg for this model is ~73.4 — our 74.44 is within
sampling noise, a second confirmation the harness measures faithfully.

Cross-model reading:

- **On deployable-floor terms (strict), flan-t5 beats the 4B**: 74.44 vs
  70.77 macro — because it never forfeits a row to format. On pure model
  quality (excl-pf) the 4B is ahead (76.76), and decisively so on the
  RAG-shaped subsets (RAGTruth 86.61 vs 76.00, Lfqa 90.38 vs 85.50). The
  verdict-format tax is the 4B's whole deployment story: fix the format,
  keep the quality.
- **FaithBench kills both, differently.** HalluGuard skews
  everything-supported (TNR 16); flan-t5 is a balanced coin flip
  (52.7/51.0). Neither clears the HHEM floor on strict terms. The spec's
  §6 warning — headline LLM-AggreFact hides the FaithBench cliff — now has
  two independent local confirmations.

## Bespoke-MiniCheck-7B (CC-BY-NC — baseline-only, never a component)

`bespokelabs/Bespoke-MiniCheck-7B` via `scripts/eval_minicheck_bespoke.py`
— transformers fp16 on MPS under a **dedicated venv pinning
transformers==4.49.0**. Two environment traps, both caught by the self-test
gate (model-card demo probs must reproduce before any eval runs):

1. The model's InternLM2 remote code **silently computes near-uniform
   garbage under transformers 5.x** — no error, weights load clean, CPU and
   MPS identical. A published-looking number from that state would have been
   pure noise. Pin 4.x for anything InternLM2.
2. Eager attention (the remote code's default) materializes the full N²
   matrix and **segfaults (exit=139) on ~6k-token docs**; benchmark docs run
   to 21k tokens. Fix: `attn_implementation="sdpa"` + length-sorted
   token-budget batching.

Yes-probability is computed as softmax mass summed over all vocab tokens
decoding to "yes" (case-insensitive), the causal-LM equivalent of
upstream's vLLM logprob sum. Claims passed wholesale, no doc chunking
(upstream chunks only past ~32K tokens). Same sampling/metrics imports as
every other row.

| Subset | BAcc | TPR | TNR | n |
|---|---|---|---|---|
| AggreFact-CNN | 62.12 | 94.41 | 29.82 | 200 |
| AggreFact-XSum | 76.50 | 78.00 | 75.00 | 200 |
| ClaimVerify | 72.50 | 89.00 | 56.00 | 200 |
| ExpertQA | 57.50 | 49.00 | 66.00 | 200 |
| FactCheck-GPT | 76.00 | 64.00 | 88.00 | 200 |
| Lfqa | 83.00 | 93.00 | 73.00 | 200 |
| RAGTruth | 86.00 | 88.00 | 84.00 | 200 |
| Reveal | 86.00 | 97.00 | 75.00 | 200 |
| TofuEval-MediaS | 73.00 | 88.00 | 58.00 | 200 |
| TofuEval-MeetB | 78.50 | 92.00 | 65.00 | 200 |
| Wice | 84.50 | 82.00 | 87.00 | 200 |
| **Macro avg** | **75.97** | | | 2200 |
| FaithBench | **54.79** | 78.14 | 31.44 | 750 |

Published: 77.4 avg (−1.4 ours: fp16-vs-vLLM numerics + 200-row sampling),
84.0 RAGTruth (+2.0 ours). Fidelity consistent with the other rows.

FaithBench reading: **third independent collapse**, and the strongest
model of the three baselines is still 14 points under the frontier-judge
band (68.8), barely above the HHEM floor. Ranking on FaithBench:
Bespoke-7B 54.79 > flan-t5 51.88 ≈ HalluGuard strict 49.57 — size and
recipe quality buy a little, but no small classifier survives
example-anchored hard negatives. This is the gap Stream B exists to close.

## The table at a glance (M0 complete, 2026-07-30)

| Model | AggreFact avg | RAGTruth | FaithBench |
|---|---|---|---|
| HalluGuard-Qwen3-4B Q4 (strict / excl-pf) | 70.77 / 76.76 | 81.00 / 86.61 | 49.57 / 56.88 |
| MiniCheck-Flan-T5-Large (0.77B) | 74.44 | 76.00 | 51.88 |
| Bespoke-MiniCheck-7B | 75.97 | 86.00 | 54.79 |
| *Published bars (spec §1)* | *75.7 / 77.4* | *84.0* | *HHEM 52.6 · frontier 68.8* |

What M1+ must beat, in one line: **~76 avg / ~86 RAGTruth is table stakes at
any size; FaithBench ≥ 60 single-pass would already be best-in-class-local,
and the §10 campaign target (≥80 avg) requires closing the format tax AND
the FaithBench cliff simultaneously.**

## Pending rows
- Our M1 checkpoint, same harness, same seed — the comparison this table
  exists for.
