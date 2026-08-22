# The judge teardown — mined contracts for judging as a first-class subsystem

Status: seat-authored 2026-08-21 under directive 31799f54 (operator verbatim:
"design our systems with the critical link of the judge at the fore — and send
all the firepower of research and academia at the problem"; method correction
ef12a34c: this teardown ran on harness web tools, NOT our deep-research loop —
the loop has not earned the critical-path instrument role; see §8 for the
sequel that converts this doc into the loop's answer key).

Purpose: the same treatment `aiq-teardown.md` got, aimed at the evaluation
literature. Not a survey — every section ends in what it OBLIGATES of our
stack. Our four production judges are the named targets: the deep-research
audit witness, the DRB-I RACE scorer pin (currently Qwen3.8-27B), the
agent-bench judge (case law: must be the 122B), and the grounding gates.

---

## §1 The law: optimization against a proxy judge peaks, then reverses

The quantitative core ([Gao, Schulman, Hilton — Scaling Laws for Reward Model
Overoptimization](https://arxiv.org/abs/2210.10760), ICML 2023): when a policy
optimizes against an imperfect proxy for true quality, gold-reward gain
follows **Δgold ≈ α·d − β·d²** where d is the proxy-reward improvement (d ∝
√KL from the reference). Gold rises, peaks, then *declines* while the proxy
keeps climbing. Three measured properties:

- The peak is real and reachable: enough optimization pressure against a
  fixed judge makes the system *worse in truth while scoring better*.
- Bigger/better judges move the peak out and flatten the decline —
  overoptimization shrinks roughly as an inverse power law in judge size.
- The practical knob is optimization pressure (their KL penalty; for us:
  tuning caps, iteration budgets, how hard any rung is allowed to chase a
  bench number).

**Obligation:** "capped at the judge" is the benign reading. The malign
reading is the reversal. Any tuning loop that chases a judge-score must also
watch a judge-independent anchor (for us: the honesty floor, evidence
grounding, the frozen banks' deterministic legs) for *decline*, not just the
proxy for rise. A rung that raises the judge score while dropping the anchor
is past its peak — that is the campaign kill-line's mathematical form.

## §2 The judge's error model: a measured bias profile, not a vibe

The taxonomy ([CALM — Justice or Prejudice?](https://arxiv.org/abs/2410.02736);
12 bias types) and the canonical field study ([Zheng et al., MT-Bench /
Chatbot Arena](https://arxiv.org/abs/2306.05685), 12k+ citations): position
bias, verbosity bias, self-enhancement/self-preference, authority, sycophancy,
format. Measurement primitives that transfer directly:

- **Swap consistency**: judge the same pair both orders; the disagreement
  rate IS the position-bias measurement.
- **Robustness rate** over perturbations that should not matter (author
  names, formatting, authority markers).
- **Judge drift**: score shifts from rubric-wording edits or judge-model
  version bumps ([prompt-sensitivity benchmark](https://arxiv.org/abs/2604.23478));
  anchor examples and human-calibration sets are the standard stabilizers.
- Base nondeterminism: same input, different scores across runs — every
  single-judge number carries a run-to-run band that must be measured once
  and then assumed.

**Obligation:** every judge we deploy gets a one-page measured profile —
swap-consistency, order-of-magnitude verbosity sensitivity, self-family
preference check, drift stamps per model/rubric version — before it gates a
landing. A judge without a profile is an uncalibrated instrument, and we
already have the case law (the 27B falsified on the DRB-II validator; the
persona-QA scorer that fired on aligned-but-wrong grounds).

## §3 What a judge can perceive: decompose, and accept a ceiling

- Strong judges agree with humans at roughly **human-human levels**: GPT-4 >
  80% overall, 85% on MT-Bench non-implementation tasks ([Zheng et al.,
  2306.05685](https://arxiv.org/abs/2306.05685)). The ceiling is not zero —
  but it is a ceiling: ~1 in 7 verdicts disagrees with a competent human even
  for the best-studied judge class.
- [G-Eval](https://aclanthology.org/2023.emnlp-main.153.pdf) (Liu et al.,
  EMNLP 2023, ~3.5k citations): judges perceive more when the rubric is
  decomposed — CoT-generated evaluation steps, form-filling scoring, token-
  probability weighting for graded scores. Rubric SHAPE is a judge-capability
  lever, not just paperwork.

**Obligation:** when a judge under-reads, the first suspects are (a) its
calibration (§2 profile), (b) the rubric's decomposability (§3), (c) the
presentation legibility of the deliverable (§7 C7) — in that order, before
blaming the model. Our RACE rubric is already dimension-decomposed (G-Eval
shaped); our audit witness is form-filling shaped. Both are correct by
design; neither has a measured profile.

## §4 The cap is conditional: elicitation and the verification ceiling

Two results complicate "capped at the judge" into something more useful:

- [Weak-to-strong generalization](https://arxiv.org/abs/2312.09390) (Burns
  et al. 2023): strong models supervised by weak judges recover most of their
  capability *if elicited well* (naive finetuning works surprisingly well;
  auxiliary confidence losses better; imitating the weak supervisor is the
  failure mode). The judge is a floor on what is ELICITED, not a hard cap on
  what EXISTS.
- The [verification ceiling](https://arxiv.org/abs/2509.20837) (Gureja et
  al. 2025): closed loops that select or train on judge-verdicts retain only
  solutions the judge can *recognize* — capability silently caps at judge
  legibility. The break: calibrated verification over diverse, deliberately
  hard problem–solution pairs the judge cannot easily pattern-match.

**Obligation:** the verification ceiling is our specific risk — a deep
research loop optimized against RACE verdicts converges to "what RACE-shaped
judges recognize as good research." The countermeasure is judge-independent
hardness in the loop: evidence grounding, corroboration, the frozen banks'
deterministic legs. These are not compliance decoration; per the literature
they are what keeps capability from capping at judge legibility.

## §5 Panels: diversity buys calibration, correlation destroys it

[PoLL — Replacing Judges with Juries](https://arxiv.org/abs/2404.18796)
(Vergani et al. 2024): a panel of diverse *smaller* judges beats a single
large judge on human alignment at ~7-8× lower cost, and eliminates
single-family self-preference. Two caveats that matter more than the headline:

- **Correlated errors undermine panels** (Apple, follow-up work): same-family
  judges replicate each other's blind spots; panel diversity must be FAMILY
  diversity, not replica count.
- **Disagreement is signal, not noise**: judge disagreement localizes the
  ambiguous cases worth escalation ([jury practice](https://orq.ai/blog/llm-juries-in-practice));
  majority-voting away the disagreement discards exactly the information that
  says "this one needs a stronger instrument or a human."

**Obligation:** disagreement between our judges (27B vs 122B vs deterministic
gates) routes escalation and gets RECORDED as a localization, never averaged
away. Our fleet is a natural PoLL panel (local models across sizes + families
+ deterministic C-class scorers) — the mesh already has the pieces; nobody
assembles them.

## §6 Self-correction without external feedback fails — the witness is load-bearing

[LLMs cannot self-correct reasoning yet](https://arxiv.org/abs/2310.01798)
(Huang et al., ~1.3k citations): without external feedback, self-correction
*degrades* performance; the early positive results leaked oracle answers.
[Kamoi et al., TACL 2024](https://direct.mit.edu/tacl/article/doi/10.1162/tacl_a_00713/125177):
models can often FIX an error when told where it is, but cannot reliably
FIND their own errors — the generator-verifier asymmetry inside one model.

**Obligation:** this is the deepest external validation of our architecture:
the containment witness and the corroboration floor are the "external
feedback" the literature says is REQUIRED for any self-assessment to count.
Any proposal to let the drafting model judge its own substance (cheaper, one
fewer stage) is not a simplification — it is re-adding the documented failure
mode. Our judge-first direction strengthens, not replaces, evidence-grounded
floors.

## §7 The eight contracts (the mining output)

**C1 — A judge ships with a measured transfer function.** Before any judge
gates a landing: run it once against a fixed gold set (vendored articles,
human-rated subsets, synthetic fixtures with known quality), record the
compression/bias/noise band. The 27B-vs-45.15 article probe running today is
C1's first instance. A judge without a transfer function is an instrument we
have not validated (§18.4, our own law).

**C2 — Optimization pressure is capped and anchored.** Every tuning loop
against a judge score carries a declared iteration cap AND a judge-
independent anchor watched for decline (the α·d − β·d² law, §1). The anchor
tripping is a kill, not a tune.

**C3 — Pairwise judging is order-randomized, swap-consistency measured.**
Any A-vs-B verdict runs both orders; the consistency rate is recorded in the
run's metadata (§2). Below-threshold consistency → could-not-judge, escalate
the instrument.

**C4 — No system is judged by its own family.** Self-preference is measured
and structural: the drafting model's family never solely judges its output
(agent-bench case law already enforces this — the 122B judges, the 4B
drafts). Panels count only if family-diverse (§5).

**C5 — Disagreement routes, never averages.** When two judges or a judge and
a deterministic gate disagree beyond noise, the case escalates (stronger
judge, or human) and the disagreement is recorded as localization signal.

**C6 — Self-assessment requires external feedback.** No verdict on own-
generated substance without the witness/evidence path (§6). This is the
audit witness's constitutional protection.

**C7 — Presentation legibility is part of the measured system.** A true
improvement the judge cannot perceive is operationally absent (the 93%-walled
finding); conversely verbosity/length bias means presentation optimization
must stop at legibility, never continue into inflation (§2's evil twin).
Deliverables render substance with support tiers visible — R3b is C7's first
instance.

**C8 — Closed loops inject judge-independent hardness.** Any pipeline that
selects/trains on judge verdicts (synthetic data, bench-driven iteration)
must contain verification the judge cannot pattern-match (evidence anchors,
diverse hard pairs — §4), or capability caps at judge legibility silently.

## §8 The gold-set sequel (why this doc is also an answer key)

The loop was barred from producing this teardown (method correction,
ef12a34c). Sequel: once the campaign lands and the loop's DRB-I number is
known, re-run THESE SAME questions through the loop as a measured exercise —
the trusted synthesis above is the answer key, the loop's synthesis is the
candidate, and the diff (coverage of the veins, contract extraction fidelity,
citation accuracy against the primaries) is a transfer function for the loop
as a researcher. The instrument this doc designs for becomes measurable by
it.

## §9 Immediate adoptions (no new orders needed)

1. The flight card's judge section carries the probe number + the 27B's
   transfer-function caveat verbatim (C1) — done via the running probe.
2. The campaign's kill-lines ARE C2 anchors — name them as such in the
   close-out record (no text change, framing only).
3. Post-flight, the four production judges each get the §2 one-page profile
   (one session-chunk, zero API for three of them — the witness and gates
   are deterministic; the agent-bench judge and RACE pin need small judge-
   call budgets).
4. Panel assembly (C4/C5) is a design note for the bench program, not this
   campaign.

Primary sources: [2210.10760](https://arxiv.org/abs/2210.10760) ·
[2306.05685](https://arxiv.org/abs/2306.05685) ·
[2303.16634](https://aclanthology.org/2023.emnlp-main.153.pdf) ·
[2312.09390](https://arxiv.org/abs/2312.09390) ·
[2404.18796](https://arxiv.org/abs/2404.18796) ·
[2410.02736](https://arxiv.org/abs/2410.02736) ·
[2310.01798](https://arxiv.org/abs/2310.01798) ·
[2509.20837](https://arxiv.org/abs/2509.20837) ·
[2604.23478](https://arxiv.org/abs/2604.23478) ·
[Kamoi TACL 2024](https://direct.mit.edu/tacl/article/doi/10.1162/tacl_a_00713/125177)
