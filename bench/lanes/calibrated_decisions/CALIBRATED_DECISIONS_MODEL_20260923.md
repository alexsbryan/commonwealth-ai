# Calibrated decisions — the pipeline as a decision process, and one model for every client

Written 2026-09-23, against `main` @ 22a41304c, before any result is collected.
Revised the same day, before any result: the goal was set to one decision
model that loads on every client, the hardware facts were corrected against
`models.toml`, and F2 (distilling the base) was added. "Tradeoffs considered" records what the earlier drafts
proposed and why each was set aside, so a later reader can reopen any of them
with the reason in hand.

This is a plan, not a pre-registration. Every bar named here is a proposal.
Each item that runs gets its bars fixed in an appended pre-registration before
its first run, under the discipline of `bench/VERIFIER_ECONOMY_PREREG_20260819.md`
§"Shared discipline".

Sibling of `CALIBRATED_DECISIONS_PREREG_20260922.md`, which asks whether
replacing a label with a distribution makes one decision better. This document
asks what the pipeline looks like once most of its decisions are distributions,
and what model should produce them. Items R1–R6 in that document's 2026-09-23
addendum are the first concrete steps under this plan and are not repeated
here. Training plumbing, provenance rules and the eval card are
`docs/specs/VERIFIER_V0.md`'s, and this document amends that plan rather than
starting a second programme. What the model learns from and is judged against
is `CALIBRATED_DECISIONS_DATA_20260923.md`.

## The goal

**One decision model, the same weights on every client, good at this stack's
decisions.** Not a general-purpose model, and not best in class on public
leaderboards. The decisions are narrow and well defined, and nearly all of
them are answered by reading the evidence in view. What matters, in order:

1. It loads on every hardware profile, down to `cpu_only`.
2. Its probabilities are calibrated on this stack's distributions, per genre,
   including a genre it never trained on.
3. It is fast enough on the smallest profiles that a decision costs less than
   the step it saves.

### What clients run (`models.toml`)

| Profile (effective VRAM) | Fast | Primary | Embed |
|---|---|---|---|
| `cpu_only` (0) | Qwen3.5-0.8B Q4_K_M | Qwen3.5-4B Q4_K_M | Qwen3-Embedding-0.6B Q8_0 |
| `low_mem` (1–7 GB) | Qwen3.5-0.8B Q8_0 | Qwen3.5-4B Q4_K_M (MTP build) | same |
| `default` (8–19 GB) | Qwen3.5-2B Q4_K_M | Qwen3.5-9B Q4_K_M | same |
| `high` (20–23 GB) | Qwen3.5-2B Q8_0 | Qwen3.5-35B-A3B UD-IQ4_XS | same |
| `very_high` (24+ GB) | Qwen3.5-2B Q8_0 | Qwen3.5-35B-A3B Q4_K_M | same |

Profile thresholds: `sovereign-inference/src/hardware.rs:52`. Three facts
shape everything below:

- **The primary differs by profile**: a 4B, a 9B or a 35B-A3B. The shipped
  high-profile primary is Qwen3.5-35B-A3B. Much of the repo references
  `Qwen3.6-35B-A3B`, which is the development loadout, not what clients
  download.
- **The fast slot differs too**: the 0.8B on the two smallest profiles, the 2B
  above them.
- **The only model on every client is the 0.6B embedder.**

### Why same weights everywhere

- **One eval card, not five.** Every profile's decisions are certified once.
- **A probability means the same thing on every node.** In a mesh, p = 0.8 from
  a `default` node and p = 0.8 from a `high` node must be comparable, or any
  decision that pools evidence from peers compares numbers on different
  scales. Per-profile models would each need their own calibration, and their
  probabilities still would not share a scale.
- **It fits the smallest client**, which `VERIFIER_V0.md` §6.1 found a resident
  4B does not: "many target users run 16 GB VRAM; a resident 4B verifier
  beside the primary does not fly there."

### Why a small model can be good enough here

The decisions this stack needs are mostly **evidence-bound**: claim support,
which passage, sufficiency, span pointing, relevance. The answer is in the
passages. These are reading tasks, and size buys less on them than on
knowledge. The verifier arc measured it in one harness
(`research/verifier-v0/findings/BASELINES.md`, LLM-AggreFact, 11 subsets):

| Model | Size | Macro BAcc |
|---|---|---|
| MiniCheck-Flan-T5-Large | 0.77B classifier | 74.44 |
| HalluGuard-Qwen3-4B (GGUF) | 4B generative | 70.77 strict, 76.76 excluding parse failures |

The 2026-08-02 amendment in that file says most of the 4B's parse failures were
correct verdicts, so its true figure is at or above 76.76. A sub-1B classifier
therefore sits about two to three points behind a 4B on generic grounding. It
also cannot fail to parse. Those are generic benchmarks; trained on this
stack's own distribution, the gap should narrow further, and that is what the
eval card measures.

## The thesis

Jev (TypeSafe; architecture as reconstructed at archerhume.com, "Jev's
architecture unmasked") encodes a state once and reads many typed questions off
it as isolated branches, prefill-only, with probabilities trained against
outcomes by a proper scoring rule. The prereg took the interface. The larger
point is the shape of the pipeline:

1. **Everything that can be a typed decision is one.** A closed answer set, a
   distribution, a fixed rule on it.
2. **Generation comes last.** The model writes characters only when every
   choice it would otherwise make implicitly has already been decided, at
   synthesis and at Phase 1 extraction alike.
3. **Probabilities drive control, not just gating.** A label can only branch. A
   calibrated probability can be priced against the cost of the next step.

## What Jev probably is, and what we take from it

Jev's weights are not public. What we know comes from TypeSafe's launch post
("Introducing System One Models & Jev": a new architecture, a parallel sampler,
"a new stack entirely focused on automation", latency measured from West Coast
laptops), Archer Hume's black-box reconstruction (~10,000 calls), and a
follow-up study at mox.es ("Jev, 48 hours later"). None of it is verified here.
The reading we plan from, with its confidence:

| Claim | Confidence |
|---|---|
| Direct learned readout heads, not language-model decoding | very high |
| Heavily customised serving (kernels, batching, request shapes) | very high |
| Causal-transformer backbone | high |
| One shared state prefill, with isolated parallel question branches | high, from black-box behaviour |
| Sparse MoE backbone | plausible, perhaps the leading guess, and the least certain part of the reconstruction |
| Custom silicon | no evidence |

The speed does not require MoE. A conventional workflow request is roughly a
2,000-token prefill plus 500–5,000 generated tokens. Jev's is the same prefill,
a few dozen question tokens, and one readout. Removing generation alone makes
the same dense backbone look one to two orders of magnitude cheaper. MoE, if
present, multiplies that: a large parameter store for knowledge at small-model
active compute. That is the plausible explanation for 84.6% on MMLU-Pro
alongside 70–500 ms latency.

MMLU-Pro is a knowledge benchmark, and no model that loads on every client here
approaches that figure. That is Jev's product: a general decision service. It
is not this plan's goal. What we take is the computation graph (a shared
prefill, isolated question branches, a trained readout head, a proper scoring
rule), not the capacity.

One reported result supports the frozen-backbone reading. On FINAL-Bench, a
frozen Qwen3.5-27B dense representation with a small zero-token probe is
reported at 0.7289 verifier AUC, against Jev's 0.7350. That comes from an
outside analysis and is unverified here. If it holds, most of the gain is in
the readout and the objective, not in training the backbone. That supports
training a head over a small model's final state rather than chasing a larger
model.

## What probabilities buy: control over expected cost

With a calibrated p at each decision, each step can be chosen against its
expected cost. The policies this makes possible:

- **Early exit.** After the head steps, if p(evidence sufficient) ≥ τ, skip the
  18 core steps (`shared_core_steps()`, `retrieval_pipeline.rs:971`).
- **Value-of-information gating.** Run an injector only when p(it contributes
  a supporting chunk | query state) × value exceeds its measured cost. The
  obligations lane is the obvious first case: it costs 3.44 s at its join
  (`retrieval_pipeline.rs:1713`) and its contribution is unmeasured.
  Needs R1 (per-step time) and R2 (per-step contribution) first.
- **Speculation.** Begin synthesis while retrieval runs when p(sufficient) is
  high; cancel if a later step changes the pool.
- **Sequential testing in the gate.** Judge claims in descending order of
  prior p(violation) and stop once the answer-level verdict cannot flip.
- **Composition.** Estimate p(answer passes the gate) before paying for
  synthesis; retrieve more or abstain first.

The speed comes from the steps not run. A composed estimate is judged against
outcomes directly, never computed by multiplying stage probabilities, because
the stages are correlated. "Order", item 7, records why retrieval-control
policies wait for an oracle-headroom measurement.

## Cost is the number of states, not the number of decisions

Jev's measured asymmetry is that question tokens cost about twice what state
tokens cost, because the state is computed once. The same holds here. On the
development Halo, the primary restores a pinned ~6k-token prefix in 26–43 ms
and prefills one cold in about 7.7 s
(`bench/chaos_monkey/results/gate_call_census_20260814_landed.txt`). A decision
is nearly free when its state is already prefilled, and expensive when it is
not. So the pipeline is organised around a few explicit states, each prefilled
once, with decisions as short branches off them:

| State | Prefilled | Decisions read off it |
|---|---|---|
| Query | at turn start | intent, answer shape, weights/sources/web, which corpora, which injectors run, decompose or not |
| Evidence (sealed) | after retrieval | sufficient, which passage supports each sub-question, which atoms and figures apply, abstain |
| Draft | after synthesis | per-claim support, citation resolution, verdict |
| Chunk | at ingest | section type, worth extracting, which schema, tension/same-as for candidate pairs |

The decision model prefills these states itself. That is the price of one
model everywhere: on the high profiles, the primary has already prefilled the
evidence state for synthesis, and the decision model prefills it again. At
0.8B that is cheap on a GPU profile and must be measured on `cpu_only` (F1).
"Tradeoffs considered", T1, records the alternative.

Synthesis is one generation over the evidence state with passages, figures and
shape already chosen.

## Point instead of copy

Much of what is generated today is a decision in disguise. The conversion is
to point at text instead of reproducing it:

- **Cite before answering.** "Which sentence (A…N) supports the answer?"
  instead of the model copying the sentence.
- **Value presence.** Select the span that holds the key value instead of
  generating it. The substring test then cannot miss, because the value came
  from the evidence.
- **Query decomposition.** Choose among templated decompositions instead of
  writing sub-queries.
- **Ingest triage.** Decide "worth extracting" per chunk before Phase 1. Chunks
  triaged out never pay for generated JSON.

These stay generation: open-vocabulary output (entity names not present in the
text, rewritten queries) and the final prose. Enum fields inside Phase 1 JSON
stay where the prereg's "Also considered" left them: they need per-field logit
capture during constrained decoding.

## Which decisions go where

**Evidence-bound decisions** (claim support, which passage, sufficiency, span,
relevance, ingest triage) go to **the decision model**, the same weights on
every client.

**Knowledge-bound decisions** have their answer in what a model knows. The one
that matters is Track A's self-assessment: can the generator answer from its
weights? A separate model cannot see the generator's knowledge. A model's own
internal state is where its own correctness is best predicted (Kadavath et
al.'s P(IK) probes were trained on the model being assessed). And the
generator differs by profile. So self-assessment is the one per-profile piece:
**one small probe per primary** (the 4B, the 9B, the 35B-A3B), each a softmax
head over that primary's last-position hidden state, keyed by the slot's
`base_name`, a few MB each. It only fires when the system is about to answer
from weights, at which point the primary is loaded anyway.

**Certification never reads the generator.** A probe on the primary judging the
primary's own draft is maximally correlated with the error it is meant to
catch, which is the failure `bench/VERIFIER_ECONOMY_PREREG_20260819.md` exists to
prevent. The gate verifier is a separately trained small model, the same on
every client, like the decision model.

## Experiment F1 — readouts with no training, measured on the smallest clients

Distributions come only from the primary today. The forced-choice backstop
(`sovereign-inference/src/embedded/grammar.rs:410-435`) routes the
`x_forced_choice` sentinel to primary because "calibrated logprobs need the
primary model", and the fast slot returns a sampled token instead of a
distribution (`evidence_loop/mod.rs:510`). That is a policy plus an
unimplemented path, not a measured property of small models. Whether a small
model's readout can be calibrated has not been measured.

Four arms on the same items:

- **(a) letter-logit readout on each primary** (4B, 9B, 35B-A3B): today's
  `forced_choice`. The reference arm.
- **(b) frozen probe on each primary**, for self-assessment only: a softmax
  head over the last-position pre-norm hidden state, fitted with log loss on
  the fit split.
- **(c) the small models, untrained**: letter-logit readout on Qwen3.5-0.8B and
  the 2B.
- **(d) a frozen probe on the embedder**: a head over Qwen3-Embedding-0.6B's
  output for the evidence-bound decisions. It is the only model already on
  every client, so if it carries a decision, that decision costs no extra
  memory anywhere. It is trained for similarity, not for reading a claim
  against evidence, so the expectation is weak. It is cheap to rule out.

The items, for every arm:

1. the prereg's instrument validation (determined items, position bias,
   watched to fail);
2. the constructible decisions ("A trained decision model", item 2);
3. Track A's fit split (arms (a) and (b)).

Report reliability curves, Brier with its resolution term, and log loss, on
held-out splits. Arms (a) and (c) are reported before and after Platt scaling.
A decision that one arm already carries gets no training spend.

**Latency is measured on `cpu_only` and `low_mem` hardware first**: prefill of
a representative state (2k and 6k tokens) through the 0.8B and the embedder.
If a 0.8B prefill on `cpu_only` costs more than the steps its decisions would
save, the plan changes shape on that profile (T6), and it is better to know
before any training.

Unverified, and read from the engine before F1 is scheduled:

- whether each primary's last-position pre-norm hidden state can be read. The
  engine reads it today for MTP (`model_slot.rs:3713-3745`,
  `set_embeddings_pre_norm`), and the comment at `:3716` says a pinned prefix
  carries no pre-norm h. Whether the suffix's last position does is the thing
  to check, and whether non-MTP builds expose it at all;
- whether the engine can return logits or hidden states from the fast slot;
- whether it can apply a LoRA adapter to a loaded slot at runtime (item 3
  below);
- whether a restored prefix can be shared across concurrent sequences.

## A trained decision model

`VERIFIER_V0.md` already trains a Jev-shaped model for one question: document
and claim in, a verdict read off one token, with the shared-document prefix
cached across a turn's claims (§6, verdict-first mode). This plan widens it
from one question to the decision family above and sets its size by the goal.
Five changes.

**1. Objective: a classification head, trained with a proper scoring rule.**
The spec trains ORPO preferences between reasoning traces and calibrates
afterwards by temperature scaling. For decisions, a softmax head over the final
hidden state is trained with cross-entropy (or Brier) against the constructed
or outcome label. Calibration is the training target, not a correction, and the
label set is not limited to labels that encode as single tokens. It is also
much cheaper. The measured 13.3 days per epoch on the Halo and about 62 h
rented (`VERIFIER_V0.md` §4) are costs of ~3k-token chosen-plus-rejected
reasoning sequences. A decision example is a state, a question, and one label.
The justify-then-verdict ORPO path stays for the gate's glassbox explanations;
the decision head does not need it.

**2. Stream B's labels-by-construction generalise.** The same substrate labels
several decisions mechanically:

| Decision | Label by construction |
|---|---|
| Which passage supports this claim (A…N, or none) | the chunk the claim was extracted from; remove it for "none" |
| Is this evidence sufficient | the same window with and without the source chunk |
| Where is the value (span pointing) | Stream B's span offsets (`VERIFIER_V0.md` §10, lever 2) |
| Does this chunk carry extractable claims (ingest triage) | Phase 1 output on the bank corpora |

Other decisions can only be labelled by outcome: self-assessment (per primary),
whether a retrieval step contributes, corpus choice. The data doc's S3 and S4
manufacture those labels; the journal adds real traffic later.

The red lines are unchanged: nothing trains on calibration banks, on the
holdouts the gate is certified by, or on receipts before Stream C's timing.

**3. Base and delivery: Qwen3.5-0.8B, identical weights on every profile.**
On `cpu_only` and `low_mem`, the fast slot is Qwen3.5-0.8B, so the decision
model can ship as a runtime adapter on it at no extra memory, if the engine can
apply one. On `default` and above, where the fast slot is the 2B and memory is
plentiful, the same weights load as their own model, about 0.9 GB at Q8_0.
Either way the weights are identical. If the engine cannot apply a runtime
adapter, the model loads on its own everywhere, which on `cpu_only` and
`low_mem` costs about 0.5–0.9 GB beside the existing slots. That budget is
checked against `capacity.rs` before it is assumed.

**4. The gate verifier follows the same rule.** One small verifier on every
client, not a 4B on hosts with room for one. That settles `VERIFIER_V0.md`
§6.1's phasing problem for good. The 4B remains the accuracy reference on the
eval card and an optional second opinion on the high profiles (T4).

**5. Selection and certification stay in separate weights.**
`bench/VERIFIER_ECONOMY_PREREG_20260819.md` holds that a verifier is worth roughly as
much as its errors are decorrelated from the generator's. One model that
decides the evidence is sufficient and then certifies the answer built on it
recreates that correlation inside the pipeline. So pipeline-control decisions
share one head set on the decision model, and the gate verifier is trained
separately. Whether the two share a base checkpoint is open (below).

### How we know it is good enough

Not a leaderboard rank. The bar is in-stack:

- **This stack's frozen banks**, per genre, including the genre held out of
  training entirely (data doc, "Evaluation banks").
- **Proper scoring rules**: log loss and Brier with its calibration and
  resolution terms, ECE beside them, and operating-point sensitivity at the τ
  actually shipped.
- **Head-to-head on identical items**: against today's label-only calls, arm
  (a) on each primary, and the 4B verifier as the accuracy reference.
- **Pipeline outcomes**: oracle headroom closed, and turn wall-clock against
  gate outcomes, measured per profile.
- **Latency on the smallest profile**, not on the development Halo.

Public benchmarks stay as the off-distribution check: the model must not
collapse on them (the FaithBench rule), but it is not tuned to top them.

## Experiment F2 — does distilling the base help decisions?

**The question, posed narrowly.** Can the 0.8B inherit enough of a stronger
Qwen's probability geometry that its states read this stack's decisions
better? This is not an attempt to make a better general 0.8B. That would be a
fast-slot question (below), and it is judged on different evals.

**The pairing is clean.** Read from the Hugging Face configs on 2026-09-23:

| | `Qwen/Qwen3.8-27B` | `Qwen/Qwen3.5-0.8B-Base` |
|---|---|---|
| `model_type` | `qwen3_5` | `qwen3_5` |
| vocabulary | 248,320 | 248,320 |
| layers × width | 64 × 5120 | 24 × 1024 |
| layer pattern | 3 linear attention : 1 full attention | the same |
| licence | Apache-2.0 | Apache-2.0 |

The vocabularies are identical, so logit distillation needs no mapping layer.
The student starts from the base checkpoint, not the chat one, and vision is
ignored.

**The teacher is chosen by measurement.** Candidates: Qwen3.8-27B, the shipped
Qwen3.5-35B-A3B, and Qwen3.8-Flash-Next (`models.toml`, `flash_next`
profile). Each is scored with its calibrated forced-choice readout on the
decision banks' fit split. The best teacher is whichever reads this stack's
evidence best, not the newest.

**What the teacher does.** Mostly it labels; it rarely writes. The data doc's
"Teacher roles" ranks the three uses. The most valuable is soft labels on
decision items already constructed. Next is top-K logits over natural text it
did not write. Text it writes itself is the weakest.

**Arms.** Every arm then gets the same decision-head training (item 1 of "A
trained decision model") and the same frozen probe:

| Arm | Backbone before head training |
|---|---|
| 0 | stock Qwen3.5-0.8B-Base |
| SFT | + 25–50k open reasoning traces (data doc, S6) |
| KD | + 5M tokens of teacher top-K logits over S6's mix |
| SFT+KD | both |
| T | + teacher soft labels on decision items only |
| SFT-Max | + filtered Qwen3.8-Max-50k traces, **only if** the operator clears them (data doc, S6) |

**Loss for the KD arms.** Over the teacher's top-K set S, with temperature T:
q is the teacher's softmax over S, p the student's, and
L_KD = T² · KL(q ‖ p). It is combined with ordinary token cross-entropy,
starting at 0.6 KD / 0.4 CE and T = 2. One extra detail carries the rest:
each teacher position also stores the probability mass outside the top-K, and
the student gets a matching "other" bucket. Without it, top-K normalisation
tells the student the teacher put all its mass on K tokens. That distorts
calibration, which is the property this plan exists for.

**Readouts.**

- **Primary:** the frozen probe and the trained heads on this stack's banks, per
  genre, including the held-out genre.
- **Secondary:** a frozen probe's AUC on UltraFeedback chosen/rejected pairs and
  PRM800K step correctness (does good-versus-bad become more readable?);
  teacher–student KL on held-out sequences; a small general benchmark set, for
  the fast-slot question.

**Scaling rule.** Only arms that move the primary readout scale to 50M tokens.
On-policy KD (the student generates, the teacher scores its states) is added
only if offline KD moves it, since it is the most expensive stage.

**Reading the result.** If T matches the KD arms on the primary readout,
general distillation is not needed for decisions and stands or falls as a
fast-slot upgrade. If the KD arms beat T, general probability geometry
transfers to decisions, and scaling it pays twice.

**Not in the pilot:** hidden-state matching (the 5120 → 1024 and 64 → 24
correspondences are arbitrary choices; an ablation once logit KD works),
sequences beyond 2–4k tokens, and vision.

**Cost.** Rough, and priced from a pilot before anything larger:

- the teacher pass over 5M tokens is about 3 × 10¹⁷ FLOPs, minutes on one
  rented H100; over 50M, a few GPU-hours;
- top-64 files are about 2 GB at 5M tokens and about 20 GB at 50M;
- student training is trivial on a rented GPU. The Halo's training path
  currently faults at weight load (`VERIFIER_V0.md` §4), so it is not an option;
- the pilot as a whole costs tens of dollars.

**Coupling to the fast slot.** Qwen3.5-0.8B is the fast slot on `cpu_only` and
`low_mem`, where it writes user-facing output, and item 3 of "A trained
decision model" ships the decision model as an adapter on it. If a distilled
base is adopted, the fast slot changes for every small client. It is then
certified on its own evals: chat quality on those profiles, and
speculative-decoding acceptance wherever a 0.8B drafts for a primary.
Distilling toward a teacher the clients do not run could lower that
acceptance.

## Tradeoffs considered

Each entry: what it is, what it would buy, why it was set aside, and what would
reopen it.

**T1. Evidence-bound decisions on a probe over the primary, where its state is
already prefilled.** An earlier draft of this document proposed it. It would
buy near-zero marginal cost on the high profiles (the evidence state is
prefilled for synthesis anyway) and the capacity of a 35B-A3B. Set aside: the
primary is one of three models, so decisions would behave differently by
profile, need three calibrations, and give probabilities that are not
comparable across a mesh. Reopen if the decision model misses its bar on a
decision that only high-profile clients need, or if the double prefill proves
expensive where it runs.

**T2. Per-profile decision models, e.g. an adapter on each profile's fast slot
(0.8B and 2B).** Would buy a stronger model on the larger profiles at no extra
memory. Set aside for the same reasons as T1: two models, two eval cards, two
probability scales. Reopen if the 0.8B misses the bar and the 2B clears it, in
which case the question becomes whether `cpu_only` and `low_mem` can load a
2B.

**T3. Post-training the primary (LoRA or full) for decisions.** Its compute
follows the active parameters, about 3B for the 35B-A3B, so a LoRA run is
affordable on rented GPUs (roughly 8–24 hours for 400M tokens at realistic MoE
utilisation). Its memory follows the total parameters: about 70 GB of bf16
weights for LoRA, about 560 GB for a full fine-tune with AdamW. The Halo cannot
train it (it faults at weight load, and its measured sustained throughput is
2.7 TFLOPS). Set aside for reasons beyond cost:

- it changes the attention weights, so decisions through the adapter need their
  own prefill and lose the shared state;
- it changes the generator, putting synthesis at risk and invalidating every
  label stamped with the generator's digest;
- it trains against a bf16 base and serves on a quantized GGUF;
- it only exists on the high profiles.

The cheaper rungs, in order, were a frozen probe, a small MLP or one trainable
block over frozen states, then a LoRA on the last few layers (which would need
engine work for a partially adapted cache). Reopen only if a primary-specific
decision earns its own prefill.

**T4. The 4B verifier of `VERIFIER_V0.md` as the gate model.** It would buy
roughly two to three points of grounding accuracy on generic benchmarks (the
table under "The goal"). Set aside as the default because it does not fit the
smaller profiles beside their primary (§6.1). Kept as the accuracy reference on
the eval card, and as an optional second-opinion slot on high-profile clients,
where the disagreement-triggered design of §6.1 applies. Reopen as the default
if the small verifier's gap on this stack's banks exceeds what the gate's
operating point can absorb.

**T5. An encoder classifier (Flan-T5, DeBERTa class) rather than a Qwen
decoder.** MiniCheck-Flan-T5-Large is the measured sub-1B result, and
encoder classifiers are the historical standard for this task. Set aside for
now: the embedded engine is built around llama.cpp and the Qwen family (one
tokenizer across generate, route and verify, per `VERIFIER_V0.md` §2), and
encoder-decoder support there is partial. Reopen if Qwen3.5-0.8B with a head
underperforms a same-size encoder on this stack's banks by more than the
engine cost of carrying a second architecture.

**T6. The embedder as the decision model.** It is the only model on every
client, so it would cost no memory anywhere. It is F1's arm (d) rather than the
plan because it is trained for similarity, not for reading a claim against
evidence. Reopen if arm (d) carries any decision; also the fallback shape for
`cpu_only` if F1 shows a 0.8B prefill there is too slow to pay for itself.

**T7. Letter-token logits instead of a trained head.** This is what the
forced-choice funnel does today, and it needs no training. It is kept as F1's
reference arm. Set aside for the trained model because it is limited to labels
that encode as single tokens (`model_slot.rs:716` drops the rest silently),
calibration is only post hoc, and position bias has to be caught by rotation
rather than trained out.

**T8. Jev as a service, or matching Jev's general capability.** The service was
rejected in the prereg's "Also considered": third-party dependency, state
leaving the machine, self-reported numbers. Matching its capability (84.6% on
MMLU-Pro) needs a knowledge-scale model that does not load on every client,
and it is not the goal. The one Jev comparison worth making is on FINAL-Bench,
where Jev's figure is published, and only if it measures something this stack
needs.

**T9. A full general distillation of the base.** The maximal recipe: about
100M tokens of teacher traces for SFT warm-up, about 1B tokens of offline
top-64 logit KD, and 250–500M tokens of on-policy KD, on 8× H100, benchmarked
on MMLU-Pro, GPQA, maths and code. It would buy a generally stronger 0.8B,
which the small clients' fast slot would also gain. Its cost is modest (roughly
$1–3k rented). Set aside as the default for three reasons:

- it spends a small model's capacity on maths, code and chat, which these
  decisions barely use;
- the decision data trains the backbone afterwards anyway and may overwrite
  much of what general KD added;
- it couples the decision model to a fast-slot change for every small client.

It is kept at pilot scale as F2's KD arms. Reopen at full scale if they beat
arm T on the primary readout, or if the fast slot is to be upgraded on its own
merits.

## Order

1. **R1, R2** (prereg addendum). Per-step time and per-step contribution.
   Instrumentation, no bars. Everything priced below reads from them.
2. **F1**, latency on the smallest profiles first. It prices the whole plan,
   and its engine reads (hidden-state reads, fast-slot logits, runtime
   adapters, shared-prefix concurrency) settle R5 as a side effect.
3. **R3.** One sealed evidence prefix across sufficiency, synthesis and the
   gate. The largest speed win that needs no model change.
4. **Data, D0–D1** (`CALIBRATED_DECISIONS_DATA_20260923.md`). Evaluation
   banks sized and frozen, licence ledger, yield checks. Nothing trains first.
5. **F2 teacher selection and the 5M pilot.** The teacher is scored on the
   decision banks' fit split. The pilot follows once D0's banks are frozen.
6. **Self-assessment probes, one per primary.** Trained on cached activations;
   each stamped with its primary's digest and refitted when the primary
   changes.
7. **The decision model and the small gate verifier**, Qwen3.5-0.8B with a
   classification head, trained with log loss, on the backbone F2 selects. The gate first: it has a
   measured prize (about 57 s per turn). The spec's M2 mix study extends to
   reasoning-only against reasoning plus decisions, gated the same way:
   non-harmful external, helpful internal.
8. **Retrieval control (early exit, step gating) only after an oracle-headroom
   run shows a prize.** `research/verifier-v0/findings/VERIFICATION_SCALING_AXES.md`
   §16 measured the covered case: a zero-shot 4B sufficiency judge at AUC
   0.633, beaten by counting distinct documents (0.661), and at most +5/158
   facts for per-query k-escalation even with a perfect evaluator. Retrieval
   is about 2–3% of a turn (§14). §16.1 scopes that to k-escalation on one
   corpus; the uncovered regime (§18–21) is where a prize could live, and it
   has never been measured at a size that survives noise. The data doc's S3
   builds that bank. Proposed bars once it exists: turn wall-clock p50/p95
   falls with disjoint intervals, gate outcomes on the frozen holdout
   unchanged, `cells_v1` unchanged.
9. **Outcome-labelled decisions from real traffic**, once the journal has
   volume.

## Open for the operator

1. Whether M3 of `VERIFIER_V0.md` (the 4B run) has run, and whether it
   continues now that the 4B is the reference rather than the shipped gate
   model (T4).
2. Whether the decision model and the gate verifier share a base checkpoint
   (cheaper, one download) or only a tokenizer (more decorrelated).
3. The latency bar per profile: what a decision may cost on `cpu_only` before
   T6's fallback applies. Set before F1's data exists.
4. Whether Qwen3.8-Max-50k's traces may be used for training. The data doc's
   S6 lays out the terms and three uses short of training on them. The
   SFT-Max arm runs only on a yes.
5. Which state owns ingest triage's threshold: a per-corpus τ refit locally,
   in the spirit of `VERIFIER_V0.md` §6.1's local self-calibration, or one
   shipped default.
