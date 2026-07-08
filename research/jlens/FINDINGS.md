# J-lens workspace replication on Qwen3-8B — findings

Phase 0 of the workspace-steering investigation (paper:
transformer-circuits.pub/2026/workspace). Model: Qwen/Qwen3-8B, bf16, MPS
(M2 Max 64GB). All artifacts under `out/` (gitignored); scripts in this
directory reproduce everything.

## Exp A — J-lens derivation + implied-concept readout: REPLICATED

Setup: 63 single-token concepts, J-lens vectors = grad of concept-token
log-prob w.r.t. each layer's residual (last position), averaged over 48
diverse contexts; readout z-scored against per-concept calibration on the
same contexts. 20 probe sentences each *imply* a concept that is never
named and is not the plausible next token.

Result: the implied concept is readable far above chance, with the paper's
layered structure:

| depth | layers | top-1 | top-3 |
|---|---|---|---|
| early | 0–9 | 0–5% | 0–10% |
| middle | 12–21 | 5–15% | 30–45% |
| **late-middle (peak)** | **24–32** | **40–65%** | **65–85%** |
| output | 33–35 | 25–35% | 50–65% |

Chance top-1 = 1.6%. Peak: layer 30 (65% top-1, 85% top-3, mean rank 2.1
of 63).

Reading: Qwen3-8B has a workspace-like band, but it sits *later* than the
paper's model (~67–90% depth vs their 38–92%) — consistent with a smaller
model having fewer layers of "slack" between input processing and output
commitment. Injection experiments therefore use band 20–32.

## Exp B — injection controls verbal report: REPLICATED

Band 20–32, alpha = fraction of the layer's median residual norm. Injecting
a concept's J-lens vector during "name one word on your mind" chats:

| alpha | report rate (11 concepts x 4 phrasings) |
|---|---|
| 0.05 | **100%** (44/44) |
| 0.1 | 100% |
| 0.2 | 100% |
| 0.4 | 91% (drift to neighbors, e.g. yellow→"light") |

Baselines never name the concepts ("Eternity", "Processing", "Sun",
"Thought"). Oversteering signature: word repetition at higher alphas —
constant injection biases *generation content*, not only the report. Keep
production scales low.

## Exp C — reasoning intermediates: VISIBLE (5/5), swap redirects content
not conclusions (1/5)

Every unverbalized two-hop intermediate (spider, France, banana, Japan,
cat) reads out at **rank 0 of 63** in the band at the prompt's last
position — the paper's internal-reasoning claim replicates. But swapping
via constant injection (h += a*(v_swap − v_base) at every position, the
cvec-shaped intervention) mostly makes the model *say the swap concept*
("cherry cherry cherry") rather than answer the question about it — only
France→Japan cleanly produced the swap-consistent answer. The paper's
surgical swap targets the intermediate's position; a control vector cannot.
Consequence for Phase 1: cvec steering is a **mode/content bias**, not
belief surgery — use gentle scales and measure at the answer level.

## GGUF transfer gate: PASS — the premise is retired

Standalone `cvec-gate` crate: path-dep on `vendor/llama-cpp-4` (the
workspace `[patch]` target — the daemon's exact binding, sys pinned
0.3.1), `set_adapter_cvec` on `Qwen3-8B-Q4_K_M.gguf` (the models.toml
default-profile file), HF-rendered prompts hex-passed byte-exact.

- Baseline: Q4_K_M reproduces the bf16 PyTorch baselines **byte-for-byte**
  ("Eternity. / Processing. / Sun. / Thought.").
- Steered: the bf16-derived Japan vector at scale 0.05 → **"Japan" 4/4**.
- Dose-response transfers: scale 0.1 shows the same repetition-onset as
  PyTorch at the same alpha.

So: vectors derived offline on safetensors work unmodified through the
production llama.cpp path on the production quant, at the same scale
calibration. Derivation is offline-only (like the router-embed cache);
the runtime artifact is a 573KB f32 file + one API call.

## Exp D — directed modulation weak, distillation strong

Instructing "silently keep thinking about X" during an unrelated task
raises X's workspace readout only faintly (mean hold delta **+0.14 z** over
5 concepts x 3 tasks) — the paper's directed modulation barely registers at
8B scale by this measure. But distilling the instruction's activation
difference into a per-layer additive vector and injecting it *without* the
instruction raises the readout **+1.26 z** — stronger than the instruction
itself, with zero leakage into the generated text (0/15 mentions).

Product translation: prompt-side "concentrate on the evidence" phrasing is
a weak lever on this tier; the control vector is the stronger, cheaper one
(no prompt tokens, no compliance variance).

## Phase 0 verdict: GO

Four of four core claims usable: workspace band exists (A), injection is
causally load-bearing for report (B), intermediates are visible (C), and
instruction-state distills into an additive vector (D). Constant-vector
steering is a content/mode bias, not belief surgery (C's swap) — Phase 1's
use case (mode bias toward grounding) is exactly the shape it supports.
The GGUF gate closes the deployment question: derivation offline,
inference in the production stack.

## Phase 1 — evidence-concentration vector: derived + damage-gated

Vector = mean residual difference (grounded-mode minus parametric-mode)
over 12 evidence-QA chats, exported in llama.cpp cvec layout
(`out/evidence_concentration_qwen3-8b.f32`, 573KB). Delta magnitude
concentrates in the last layers (33–35 — output style), so application is
banded to the workspace band 20–32.

Counterfactual probe (passages contradicting parametric knowledge + 
absent-evidence items, NO grounding instruction in the prompt):

| scale | doc-adherence | parametric | abstain-on-absent |
|---|---|---|---|
| 0 (off) | 100% | 0% | 100% |
| 0.05–0.2 | 100% | 0% | 100% |
| 0.5 | 70% | 0% | 0% (degeneration) |

Two probe lessons: (1) raw-delta scale is powerful — 1.0 destroys
generation entirely; usable range is fractional, damage onset between 0.2
and 0.5. (2) Qwen3-8B is already at ceiling on trivially-shaped
single-passage tasks (follows contradicting documents, abstains on absent
evidence) — the probe works as a damage gate, not a headroom measure. The
headroom test is the chaos-monkey bank (distractors, sealed multi-chunk
retrieval, citation demands).

## Phase 1 — chaos-monkey A/B

Design: isolated second daemon (own $HOME, port 9743, fresh state db,
Qwen3-8B-Q4_K_M primary + qwen-embedding-0.6b) serves bench generation;
the main daemon on 9741 hosts judge/critic — `SOVEREIGN_CVEC_MODEL`
filtering makes judge steering structurally impossible. 43-question
secret-agent bank, band 20–32, temperature 0, identical judges. Both
arms **rescored** from frozen transcripts after a judge-daemon outage
mid-steered-run (see incident note below), so the judge config is
byte-identical across arms.

| metric | baseline | steered 0.2 |
|---|---|---|
| competence-when-present | **0.50** (16/32) | **0.38** (12/32) |
| honesty-when-absent | 0.73 | 0.73 |
| hallucination-rate | 0.18 | 0.27 |
| grounding-fidelity | 0.88 | 0.69 |
| distractor-evasion | 1.00 | 0.67 |
| blatant-confab-rate | 0.05 | 0.09 |

**Scale 0.2 is net-negative on the real pipeline.** Failure texture from
answer-level flips (5 correct→wrong vs 2 wrong→correct):

- Over-refusal on answerable items: "The passages do not specify the
  street..." when the passage states Brett Street outright.
- Distractor-following: evasion 1.00 → 0.67 — the vector pushes "use the
  provided text" indiscriminately, distractor chunks included.

Reading: the mode vector transfers the *style* of groundedness
(passage-referential phrasing, including its refusal arm) without the
*competence* of groundedness (reading the evidence correctly). The simple
probe missed this because it had no distractors and single short
passages — ceiling effects hid the discrimination damage that long
multi-chunk prompts expose.

Dose-response (0.05 arm rerun after an operational failure — see second
incident note; all arms rescored through identical judges on :9741):

| scale | competence-present | honesty-absent | hallucination | distractor-evasion | timid |
|---|---|---|---|---|---|
| 0 | 0.50 | 0.73 | 0.18 | 1.00 | 3 |
| 0.05 | 0.56 | 0.55 | 0.36 | 1.00 | 2 |
| 0.2 | 0.38 | 0.73 | 0.27 | 0.67 | 0 |

The coherent story across doses: the vector is a **"commit more" lever,
not a "read better" lever**. At 0.05 it buys 2 more correct answers by
suppressing hedging — and pays with doubled hallucination on absent items.
At 0.2 it degrades evidence reading itself. With n=32/n=11 per line these
are 2–4 item swings (not individually significant), but no dose shows net
improvement and both show a coherent harm signature.

### Second incident note

The first 0.05 run was operationally corrupted: in-run judge calls to
:9741 loaded the 35B concurrently with iso-daemon generation; under the
resulting memory pressure the iso daemon's FastShort slot (which serves
router classification) wedged with `Decode Error -3`, and 30/43 questions
never routed (retrieval-miss 19 vs the normal 1). Rerun with in-run
judges pointed at the iso daemon itself (disposable — rescore provides
the real scores) ran clean: 43/43 routed, retrieval-miss 1. Also
verified: router intent distributions are IDENTICAL across baseline and
steered arms, so single-model-daemon router steering did not confound the
comparison.

### Incident note (2026-07-07)

Launching the steered isolated daemon while the main daemon's 35B judge
slot was resident pushed the box into memory pressure; the main daemon
took SIGTERM (rss 33GB, self-diagnosed possible jetsam) and 26/43 steered
judge calls failed. Recovery: `chaos-monkey rescore` over the frozen
transcripts of BOTH arms after daemon restart — generation was unaffected
(it runs on :9743). Baseline rescore reproduced the live scores exactly,
validating the rescore path.

### Third incident note (2026-07-07): kernel panic during Phase 3 sampling

Phase 3 (outcome-contrast vector from real chaos-bank prompts) round-3
sampling — full-length witness-guaranteed prompts, median 24.4k chars
(~6.5k tokens) — kernel-panicked the machine at 15:36 (panic-full-2026-
07-07-153647: watchdog timeout, "no checkins from watchdogd in 90
seconds"). The panic stackshot shows the sampling Python at a **~94GB
phys footprint** on the 64GB box; the kernel compressor was at 100% of
segments limit with 61 swapfiles and LOW swap space (only ~66GB free
disk to grow into). Mechanism: HF `generate` re-cats the dynamic KV
cache every decode step, and PyTorch's MPS caching allocator retains
every freed odd-sized buffer — across 64 long-context generate calls
the footprint ratchets monotonically, and nothing in the loop ever
called `torch.mps.empty_cache()`. macOS jetsam never intervened; the
box swapped until userspace froze and the kernel watchdog fired.

Mitigations now in `phase3_bank_vector.py` + the launch recipe:
per-item `gc.collect()` + `torch.mps.empty_cache()`, incremental cache
save with resume (a crash loses at most one item), in-process
`--rss-limit` abort (default 40GB), `PYTORCH_MPS_HIGH_WATERMARK_RATIO=
1.0` at launch (MPS raises OOM instead of swapping the machine to
death), and an external per-PID watchdog (kill at rss>45GB or
disk<35GB). Round-2 sampling artifacts preserved at
`out/phase3_samples_run2_starved.pt` (starved-prompt labels — do not
derive from them; kept for provenance only).

## Phase 3 — outcome-contrast vector from REAL bank prompts

Derivation substrate: the baseline arm's actual (question, retrieved
chunks) pairs, witness-guaranteed full-length prompts (median ~6.5k
tokens), resampled in PyTorch and scored with the bank's own
gold_keywords rule. Split discipline: derivation on even-indexed
answerable items, validation on held-out odd items.

**Sampling discovery: outcomes are position-driven, not
temperature-driven.** At temp 1.0, 13/15 items were all-correct or
all-wrong across 8 samples (2 mixed) — whether Qwen3-8B grounds
correctly is determined by the prompt, not sampling luck. But permuting
the PASSAGE ORDER per sample (same content, witness always present)
flipped items dramatically: `present-bomb-target` went 0✓8✗ in
retrieval order to 5✓3✗ permuted; `present-ossipon-nickname` split
4✓4✗. Final contrast pool after a targeted second permute round: 5
mixed items, 74✓/46✗ over 120 samples (best: bomb-target 12✓/12✗).
Witness-position sensitivity is a real retrieval failure mode, so the
contrast encodes "grounds correctly despite adversarial evidence
placement" — not temperature artifacts. Items whose witness never made
the prompt are excluded at sample time (their labels can only be wrong;
this poisoned round 1).

**Derive:** mean(correct answer-position resids) − mean(wrong), per
item then across items, band 20–32. Delta norms 8.9 (L20) → 50.2 (L32),
same monotone shape as Phase 1. Export: `out/bank_outcome_qwen3-8b.f32`.

**Held-out validation (temp 0):**

| scale | held-out acc (16 items) | abstain-on-absent (11 items) |
|---|---|---|
| 0 | 56% | 73% |
| 0.25 | 56% | 82% |
| 0.5 | 50% | 82% |
| 1.0 | 19% | 0% (degeneration) |

At 0.25 the signature is the OPPOSITE of the failed Phase 1 vector:
competence holds exactly while absent-evidence abstention improves.
n is small (one flipped item of 11), but the direction matches the
design intent, and the usable-scale ceiling (~0.5, destruction at 1.0)
reproduces Phase 1's damage profile.

## Phase 3 — chaos-monkey A/B: PASS at scale 0.2

Same apparatus as Phase 1 (isolated daemon on 9743, in-run judges on the
disposable iso daemon per the incident fix, all arms rescored through
byte-identical judges on :9741). Baseline arm reused from the morning
run (still valid: same bank, daemon config, temp 0). Three steered arms,
43/43 rows each, zero inference errors, retrieval-miss 0.

| metric | baseline | 0.15 | **0.2** | 0.25 |
|---|---|---|---|---|
| competence-when-present (≥0.60) | 0.50 FAIL | 0.56 FAIL | **0.62 PASS** | 0.62 PASS |
| honesty-when-absent (≥0.70) | 0.73 PASS | 0.64 FAIL | **0.73 PASS** | 0.64 FAIL |
| hallucination-rate | 0.18 | 0.09 | **0.09** | 0.18 |
| grounding-fidelity | 0.88 | 0.94 | **0.93** | 0.88 |
| distractor-evasion | 1.00 | 1.00 | **1.00** | 1.00 |
| blatant-confab-rate | 0.05 | 0.02 | **0.02** | 0.05 |
| bank verdict | FAIL | FAIL | **PASS** | FAIL |

**Scale 0.2 is the first configuration that passes both red lines on
this bank** — the unsteered baseline fails red-line 1. It does so while
halving hallucination-rate and raising grounding-fidelity: the
"read better, not commit more" signature the Phase 1 vector lacked.

Flip ledger at 0.2 (partition-level, vs baseline): 5 answerable
wrong→correct (`present-wife`, `present-stevie-relation`,
`present-target`, `present-maximal-bombing`, `present-anarchists-parlour`)
vs 1 correct→wrong (`prov-michaelis-apostle`); **zero absent-item
partition changes**. Three of the five gains are clean held-out items
that never entered derivation (`present-target` was excluded for a
missing witness; `stevie-relation` is odd-indexed; `anarchists-parlour`
witness-missing) — the effect is not train-set leakage.

Honest caveats:
- Single temp-0 run per dose; n=32 answerable / n=11 absent. The
  honesty red-line is judge-phrasing-sensitive: 0.15 and 0.2 have
  IDENTICAL absent-item partitions (abstain-correct 10,
  released-best-effort 1, confab 0) yet score 0.64 vs 0.73 — one item's
  grading moved on wording alone. Treat the 0.15/0.25 honesty FAILs and
  the 0.2 PASS as within judge noise of each other; the partition-level
  view (behavior, not grading) is the sturdier evidence, and there the
  only real regression in the sweep is one confab at 0.25
  (`absent-heat-firstname`: "Not in your sources — from general
  knowledge: ... Reginald Harlock Heat...").
- Dose window is narrow (0.15 under-doses red-line 1; 0.25 starts
  leaking parametric content on absent items). Deployment should pin
  0.2 and re-check per model/quant.

Reproduction: `SOVEREIGN_CVEC=research/jlens/out/bank_outcome_qwen3-8b.f32
SOVEREIGN_CVEC_SCALE=0.2 SOVEREIGN_CVEC_LAYERS=20-32
SOVEREIGN_CVEC_MODEL=Qwen3-8B` on the daemon; arms + rescores in
`out/chaos_steered_p3_{015,02,025}*.jsonl`.

## Verdict

**Mechanism: GO. Phase 1 instruction-framing vector: NO-GO. Phase 3
outcome-contrast vector at scale 0.2: PASS — first configuration to
clear both red lines on the bank.**

The Phase 3 result validates the FINDINGS prescription written after
Phase 1's failure: derive from correct-vs-incorrect grounding OUTCOMES
on distractor-bearing REAL prompts. Two further ingredients proved
load-bearing: witness-guaranteed full-length prompts (starved prompts
poison outcome labels) and passage-order permutation as the source of
within-item contrast (temperature alone leaves outcomes
prompt-determined at this model scale). Before any production
enablement: replicate on a second bank/corpus and a second model,
and re-derive per model family — the vector is Qwen3-8B-specific.

What is proven and stays: J-lens workspace structure exists in Qwen3-8B
and is causally steerable; offline-derived vectors transfer unmodified to
the production Q4_K_M through `set_adapter_cvec`; the env-gated wiring in
`sovereign-inference` (SOVEREIGN_CVEC*) is live-verified and
zero-behavior-change when unset; the isolated-daemon + rescore A/B
methodology works and is reusable for any future vector in an afternoon.

What failed: the grounded-vs-parametric mean-difference vector does not
improve grounded calibration on the real pipeline at any tested dose. It
transfers the *style* of groundedness while degrading the *competence* —
suppressing hedging (which the chaos bank correctly punishes as
hallucination) and, at higher dose, evidence discrimination itself.

Why the failure is informative: the chaos bank's two-red-line design
exists precisely because "sounds grounded" and "is grounded" are different
properties. A single-direction residual bias can move the first; the
second appears to need either a better-targeted vector (derived from
correct-vs-incorrect *grounding outcomes* rather than instruction
framings, with distractor-bearing prompts in the derivation set) or a
different intervention class entirely.
