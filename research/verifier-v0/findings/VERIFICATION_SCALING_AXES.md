# Verification scaling axes — what LLM-as-a-Verifier offers a 4B, and what it does not

**Status:** further-research pre-registration. Written 2026-08-20.
**Parent:** `sovereign/docs/specs/VERIFIER_V0.md` (§0 build-vs-adopt, §1 success
criteria). Successor instrument to `HEADROOM_STUDY.md` and
`THRESHOLD_CALIBRATION.md`.
**External source:** `github.com/llm-as-a-verifier/llm-as-a-verifier`,
paper arXiv:2607.05391, MIT licensed.

**Provenance rule for this doc.** External numbers are *transcribed* from the
repo README and the arXiv abstract, fetched once each on 2026-08-20. The
paper's ablation tables were **not read**. Every number attributed to us is
one of ours, measured by our harness, with its file cited. The two are never
mixed in a table — same discipline as `BASELINES.md`.

---

## 0. The question

The operator's goal is a 4B that verifies at a high level. LLM-as-a-Verifier
claims state-of-the-art verification "without requiring additional training."
Does it provide for that goal?

**No — and the reason is structural, not incidental.** Their framework
provides three scaling axes on top of a verifier signal. It does not provide
the signal. We already built the signal, and their task is the one regime
where the weakness of a small model does not bite.

That sentence is the whole finding. §2 is why.

## 1. Where our gate sits inside their formula

Their trajectory reward:

```
R(x, tau) = (1/CK) SUM_c SUM_k SUM_g  p(v_g | x, c, tau) * phi(v_g)
```

over **C** criteria, **K** repeated evaluations, and **G** ordered score
tokens. The paper's own framing of the three: score granularity, repeated
evaluation, criteria decomposition.

Our production gate is the **C=1, K=1, G=2** corner of that formula:

| axis | their range | ours | site |
|---|---|---|---|
| G — score granularity | 20 tokens, `A`..`T` | 2 tokens, `A`/`B` | `judge.rs:113` |
| K — repeated evaluation | K with A/B slot alternation | 1, `temperature: 0.0` | `judge.rs:87` |
| C — criteria decomposition | C domain criteria | 1 question | `judge.rs:1266` |

We independently arrived at their central mechanism. `forced_choice_ab`
(`judge.rs:87`) returns `(p_A, p_B)` and `claim_chunk_support`
(`judge.rs:444`) computes `a/(a+b)` — a continuous score read from the
distribution, never a sampled discrete verdict. We independently arrived at
their tokenization trick too: single letters, because digits `1..20` are
multi-token and there is no single position to read.

What we have not done is turn any of the three knobs up.

This matters because `THRESHOLD_CALIBRATION.md` closes with the sentence
their paper is an answer to: *"The next checkpoint needs to move AUC, not its
operating point."* All three axes are training-free AUC moves. They deserve a
measurement before another training round is funded.

## 2. Ranking versus thresholding — why their size-agnosticism does not transfer

**Their task is best-of-N ranking. Ours is absolute thresholding. Rank order
survives monotone squashing; a fixed tau does not.**

PPT, their ranking algorithm, consumes only `sigma(R_a - R_b)` — a difference
of scores. A verifier whose outputs all pile up between 0.42 and 0.76 can
still order candidates perfectly, and every one of their headline numbers is
a selection result: Terminal-Bench V2 86.5, SWE-Bench Verified 78.2,
MedAgentBench 73.3, RoboRewardBench 87.4.

Our gate consumes `vp = 1.0 - max_support` against a fixed tau
(`judge.rs:377`). Squashing destroys that directly: it moves every score
toward the middle without changing any ordering, and the threshold is the
only thing that reads absolute position.

So their framework can afford to be indifferent to verifier size in a way
ours cannot. It provides for a 4B in the regime where calibration is
irrelevant. Ours is the regime where calibration is the entire job.

**Consequence for the campaign:** PPT and ProgressTracker are not grounding-gate
work. They belong to the deep-research best-of-N arc (`drb1-race`), where
ranking is genuinely the task and where a squashed-but-correctly-ordered small
verifier is fit for purpose. Filing them there is not a demotion; it is the
regime where their evidence actually applies.

## 3. The size evidence — theirs is absent, ours is negative for a stock 4B

**Theirs.** Their only self-hosted example is `vllm serve Qwen/Qwen3.5-9B`.
Every published verifier is Gemini 2.5 Flash or deepseek-v4-flash. There is
no ablation across verifier size, nothing below 10B evaluated, no
minimum-capability statement, and no failure-mode analysis for a weak
verifier. The abstract's selling point is "without requiring additional
training", which makes the paper structurally silent on how to make a small
model good at this.

**Ours.** We have measured the exact primitive their mechanism depends on, at
4B, twice:

| measurement | number | site |
|---|---|---|
| stock 4B forced-choice distribution on known fabrications | **0.42-0.76** (vs primary critic 0.96-0.98) | `judge.rs:98-101` |
| vanilla fast-slot 4B, joined protocol, control bank AUC | **0.763** (incumbent 0.824, rung-1000 0.848) | `HEADROOM_STUDY.md` add. 5 |
| same, catch at matched FA 32.6% | **67.9%** (incumbent 74.4%, rung-1000 85.9%) | same |

Read naively their paper predicts: plug in a small model, scale G/K/C,
approach frontier verification. Our data says the small model's *distribution*
is the weak link, and G is the axis that reads that distribution at higher
resolution. **Reading a squashed distribution more finely does not unsquash
it.** That is the falsifiable core of the caution in §5.

**And the thing their framework explicitly does not do is the thing that
already worked here.** rung-1000, at 4B, beats the 35B incumbent on our
distribution: AUC 0.9622 vs 0.8752 on the constructed bank, 0.848 vs 0.824 on
real prose, catch at matched FA 85.9% vs 74.4% joined. Training is what makes
a 4B verify at a high level. We have that; the paper does not offer it.

## 4. Inventory before build (ARCH §19)

What the stack already holds, checked before proposing anything new:

- **N-ary forced choice is already implemented and is better than a logprobs
  API.** `forced_choice_probs` (`model_slot.rs:667`) takes a candidate list of
  any length, requires each candidate to encode to a single token (trying a
  leading-space variant, erroring loudly otherwise — *"forced_choice: no
  candidate encodes to a single token"*), does one O(vocab) pass at the final
  position, and softmaxes over exactly the candidate set. One pass,
  `completion_tokens: 0`, no sampler, no parser. `enum: ["A", ..., "T"]` works
  today with **no code change**. An OpenAI-compatible logprobs API returns a
  truncated top-k that must be renormalized; ours is exact over the candidate
  set. Their letters-not-digits trick is a workaround for API tokenization
  that our engine already enforces as a contract.
- **The prompt is already in the cache-optimal layout.** `chunk_judge_prompt`
  (`judge.rs:1266`) is PASSAGE first, CLAIM at the tail — the same shape their
  README credits for a 78.8% cache hit rate and ~3.4x fewer uncached input
  tokens.
- **Banks, harness and referee exist.** 2,494-case constructed bank, 222-row
  control bank, 97 strong-label journal rows, `headroom_study.py`,
  `control_report.txt`, the gate-call census, `GateCallMechanism::ChunkJudge`.
- **Threshold discipline exists.** LOSO fitting, half-split validation, and
  the rule that a calibrated lane is a third column and never a silent
  substitution (`THRESHOLD_CALIBRATION.md`).

What is genuinely missing: C > 1, K > 1, a G > 2 register the checkpoint has
been trained on, and an A/B swap control.

**One correction to an earlier reading.** `claim_chunk_support` passes
`stable_prefix_len: None` (`judge.rs:451`) — it is the only `forced_choice_ab`
caller that does. That looked like free money. It is not, for two
reasons the code states plainly.

The pinned-prefix cache holds `MAX_ENTRIES = 6` families per slot, and its
own accounting names **six** live families — synthesis primary and fast
variants, gate verifier, gap check, router coarse, title
(`prefix_state.rs:122-126`). The live set already fills the cache. There is no
seventh slot for a per-chunk family, and a loop over up to 12 distinct
passages would thrash a 6-entry LRU and evict production families with it.

And the family the gate already owns is the **joint** register, not the
per-chunk one: `judge.rs:594` and `judge.rs:1553` both thread
`stable_prefix_len` through, while `claim_chunk_support` alone passes `None`.
So the pin is not missing from the gate — it is present on exactly the
register Addendum 4 independently chose on accuracy grounds. Joining a claim's
evidence into one passage keeps the family count at one per turn, because the
joined passage is identical across the turn's claims.

**The accuracy decision and the cache decision are the same decision.** Moving
the slot to joined evidence inherits the existing pinned family rather than
requesting a new one. That is worth stating because it removes a piece of work
rather than adding one: there is no prefix item to build, only one to verify.

## 5. Pre-registered experiments

Cheapest first. Every row states its prediction and its kill bar **before**
the data exists. A kill bar is a stopping rule, not an invitation to tune.

### E1 — A/B swap control (instrument validation, ARCH §18.4)

The letter-to-label mapping is fixed: `A` always means supported. Part of
`support` is therefore the model's letter prior, not its judgement, and no
run has ever checked. This is an unvalidated instrument under the number the
whole card rests on.

- **Protocol:** re-score the 222-row control bank with `A`/`B` semantics
  swapped in `chunk_judge_prompt`, support recomputed as `b/(a+b)`. Both
  judges, joined evidence. Prompt constant only; no code change.
- **Prediction:** AUC shifts by < 0.01 on both judges.
- **Kill bar:** shift > 0.03 on either judge means the calibration is partly a
  letter prior. `THRESHOLD_CALIBRATION.md` and `HEADROOM_STUDY.md` get an
  erratum, and every subsequent axis is measured swap-averaged.
- **Cost:** one re-run.

### E2 — C=4 criteria decomposition (the axis this doc recommends first)

`HEADROOM_STUDY.md` gives per-kind incumbent miss rates that are wildly
uneven: entity_swap 14.4%, negation_flip 12.2%, unsupported_addition 7.8%,
number_perturb 6.2%. One question asking one thing has to carry all of them in
one logit.

- **Protocol:** four binary probes per (claim, passage) — relation established
  / entities identical / polarity identical / quantities identical — each a
  `forced_choice_ab` call in the register rung-1000 was ORPO'd on. Score is
  the mean. Both judges, joined evidence, 222-row control bank plus the 97
  strong-label journal rows as the false-alarm instrument.
- **Prediction:** +0.02 AUC on control for rung-1000; entity_swap and
  negation_flip miss rates converge toward the other kinds.
- **Kill bar:** no AUC gain at C=4 kills the axis. Do not tune the criteria
  wording to rescue it — that is fitting the instrument to the bank.
- **Second bar:** journal-strong false alarms must not rise above the joined
  baseline of 18.6%. An accuracy gain bought with timidity is not a gain
  (§1 red line: sensitivity never buys specificity, and the reverse).
- **Cost:** 4x the calls per claim. Measure latency alongside; the per-claim
  budget in spec §1 is a shipping constraint, not a footnote.

### E3 — K as perturbation ensembling (not resampling)

At `temperature: 0.0` reading a distribution, K identical repeats return
identical numbers. Their K cancels positional bias through slot alternation;
ours has no sampling variance to average. The useful variance source here is
perturbation.

- **Protocol:** K=4 as {A/B swap} x {chunk order permuted}, mean of the four
  scores. Runs on top of whatever E1 concludes.
- **Prediction:** +0.01 AUC, smaller than E2's.
- **Kill bar:** if E1 shows a swap shift < 0.01, the swap half of K is
  measuring nothing and K collapses to chunk-order permutation alone; if that
  alone gains nothing, drop the axis.

### E4 — G=20 graded head (a training item, not a config flip)

`HEADROOM_STUDY.md` Addendum 5 records the symptom: *"the score distribution
is coarse (0.85/0.9 identical)"* — the tau sweep could not distinguish two
operating points because a two-token margin saturates.

The wire path already supports it (§4). The checkpoint does not: rung-1000 was
ORPO'd on binary verdicts and has never seen an `A`..`T` register, so its
logits over 20 letters are noise. The paper's G scaling is measured on
frontier models that already carry a calibrated prior over what 14-of-20
means; a 4B has no such prior and they never tested one.

- **Protocol:** M4 data-recipe change. The Stream B corruption taxonomy is
  already an ordinal severity scale that we currently flatten to a bit —
  verbatim / reframe / number_perturb / entity_swap / negation_flip /
  unsupported_addition / chimera / distractor. Relabel to a graded target,
  retrain, then read the 20-way distribution.
- **Prediction:** the 0.85/0.9 tau tie breaks; AUC moves where calibration
  could not (`THRESHOLD_CALIBRATION.md` proved recalibration alone is
  exhausted — FaithBench's in-sample ceiling is 56.06, so that failure is
  discrimination, not calibration).
- **Kill bars:** FaithBench non-regression, per the standing mix-study gate.
  And a G=20 head that does not beat the C=4 ensemble from E2 does not ship —
  the simpler axis wins ties.
- **Order:** after E2. If C=4 already buys the separation, G costs a training
  round for a second copy of the same gain.

### E5 — confirm the joined slot inherits the pin (verification, not construction)

Per §4 this is a check, not a build. The joined register already declares
`stable_prefix_len`; the question is whether a turn's claims 2..N actually
restore against it in production rather than silently full-prefilling — which
is the documented failure mode: *"a mismatch does not error and does not change
a verdict; it silently full-prefills"* (`judge.rs:1300-1306`).

- **Protocol:** run a multi-claim turn through the joined slot and read the
  gate-call census under `GateCallMechanism`. No code change unless it fails.
- **Prediction:** claims 2..N restore. The measured spread for this cache is
  26 ms restore vs ~7.7 s re-prefill (census 2026-08-13); the controlled A/B
  that shipped it was 868.4s to 669.0s, 1.30x, against an off-arm spread of
  66.5s (`prefix_state.rs:168-176`).
- **Kill bar:** if it does not restore, the boundary is wrong and that is a
  bug to fix, not an axis to abandon. Verdicts must be bit-identical either
  way; verdict drift is a bug in the pin, never a tradeoff to accept.
- **Watch:** family pressure is already at the cap. Adding any new pinned
  family displaces a production one, so the per-chunk register stays unpinned
  unless `MAX_ENTRIES` is raised deliberately and the byte budget re-checked.

## 6. What this does not answer

- **The multi_hop hole is untouched by every axis here.** Both judges fail
  cross-chunk synthesis at ~99.5% false-alarm (`HEADROOM_STUDY.md`), and no
  amount of C, K or G fixes a procedure that judges chunks separately. Joined
  evidence is the fix, and it is already decided.
- **Nothing here is measured on a 4B other than rung-1000 and the vanilla
  fast slot.** The claim "a trained 4B verifies at a high level" rests on one
  checkpoint, one corpus family, and banks whose generator that checkpoint
  trained against. The generator-familiarity control (Addendum 3) is the only
  counterweight and it is n=78 fabrications.
- **Their ablation tables were not read.** If the paper reports G scaling
  ablated by verifier size, that is the single most decision-relevant table in
  it and it should be read before E4 is funded.
- **Best-of-N in our stack is unscoped.** PPT is filed to `drb1-race`; whether
  peer answer selection has enough candidates for a tournament to beat pairwise
  is an open question in that arc, not this one.

## 7. What changed in this doc's own reading

An earlier pass ranked G=20 as the strongest axis. With the size evidence in
hand the order flips: **C before G for a small verifier.** C decomposes into
binary probes in the register the checkpoint was trained on and that we have
validated at 4B; G asks a 4B for a calibrated 20-way prior that the paper
never tested below 10B and that our checkpoint has never seen. G stays worth
doing, as a training target rather than a knob.

---

## 8. Addendum — M, the independent-verifier axis: measured, and dead on this task (2026-09-08)

**Added by a session asking a different question and arriving here.** The mesh
hypothesis under examination was: *groundedness scales with the number of
independent checks, so a mesh gets more grounded as it grows.* C, K and G are
all knobs one node turns alone. **M — how many independent judges are
combined — is the only axis a second node can buy**, and it is the axis this
doc did not name.

Pre-registration, prediction and kill bars are in the header of
`scripts/ensemble_axis.py`, written before any ensemble number existed.
**The prediction was +0.02 AUC. The measurement is −0.035, CI entirely below
zero.** Every combiner tested loses to the best single judge, on both metrics,
without exception.

### Instrument first (§18.4)

`runs/headroom/` already holds per-item scores for three judges on the same
joined control bank, so M costs **zero inference** — it is a join. All three
single-judge numbers reproduce Addendum 4/5 exactly before anything is
combined: incumbent 0.6404 (published 0.640), rung-1000 0.8479 (0.848),
vanilla-4b 0.7634 (0.763); journal-strong FA 34.0 / 18.6 / 20.6%, all three on
the nose. `our_margin`, not `our_max_p`, is the doc's score variable.
(Noted, not averaged: the incumbent column differs on 7/222 rows between the
two run files.)

### Result — n=222 (144 grounded / 78 real fabrications), joined evidence

Baseline: rung-1000 alone @ tau 0.5 — **catch 80.8% @ FA 27.8%**, AUC 0.8479.

| members | rule | AUC | ΔAUC | catch@matched-FA | Δcatch |
|---|---|---|---|---|---|
| rung+incumbent | mean | 0.7985 | −0.0495 | 78.2% | −2.6pt |
| rung+vanilla | mean | 0.8133 | −0.0346 | 73.1% | −7.7pt |
| all three | mean | 0.8024 | −0.0456 | 73.1% | −7.7pt |
| all three | min | 0.7323 | −0.1156 | 60.3% | −20.5pt |
| all three | max | 0.7849 | −0.0630 | 70.5% | −10.3pt |

Twelve cells (4 combos × 3 parameter-free rules); all twelve negative. Best on
catch is rung+incumbent[mean] at −2.6pt, bootstrap 95% CI [−15.4, +6.4]pt —
**killed by its own bar.** The AUC delta for rung+vanilla is tighter and
unambiguous: −0.0346, CI [−0.0505, −0.0201].

### Why — the errors are NESTED, not complementary

The oracle bound (an item counts caught if *any* judge flags it at its own
matched-FA tau — a ceiling, not a method):

| set | catch | vs rung alone |
|---|---|---|
| rung-1000 alone | 61/78 = 78.2% | — |
| vanilla-4b alone | 52/78 = 66.7% | — |
| **oracle union(rung, vanilla)** | 62/78 = 79.5% | **−1.3pt** |
| oracle union(all three) | 67/78 = 85.9% | +5.1pt |

**Vanilla-4b adds exactly one item rung-1000 misses.** That is not a combiner
failure — there is nothing to combine. Both are Qwen3.5-4B; rung-1000 is a
LoRA over the other's base. **Same lineage is not independence.** Different
weights over one base model produce nested error sets, and averaging a strong
judge with a nested weaker one can only pull correct scores toward wrong ones.

The incumbent — the only genuinely different family in the panel — is the
*only* source of complementary signal (+5.1pt oracle, 6 items rung misses).
And it is too weak overall (46.2% catch) for any parameter-free rule to import
those 6 without its other 42 misses. Extracting them needs per-item trust
weighting, which needs a labelled set to fit; on n=222 that is fitting the
instrument, and it is not done here.

### What this changes

- **"More nodes ⇒ more grounded" is refuted for the absolute-thresholding
  gate.** Not weakened — every cell negative, the strongest CI excluding zero.
- **The membership claim fails in its strong form.** "Any independent node
  contributes" is false: adding the collapsed incumbent to rung-1000 costs
  0.05 AUC. A mesh verification fan needs a **competence floor and a lineage
  requirement**, not a node count.
- **Independence is a property of model lineage, not of process.** This is the
  transferable finding. A diverse panel is untested and remains the real test
  of the thesis; a panel of one family is now measured and it is inert.
- The E2 (C=4) recommendation is untouched and stands. Decomposing one judge's
  question is not the same move as adding a second judge, and this result is
  evidence *for* preferring it: the within-judge axis has headroom the
  across-judge axis does not.

### Scope — what this does NOT license

- **n=222, 78 fabrications.** The catch-delta CI is ±11pt; this bank sees large
  effects, not small ones. The AUC CIs are tight (±0.015) because AUC uses all
  pairs.
- **Two of three judges share a base model,** which is the finding but also the
  limit: a genuinely diverse panel (different families) was never run.
- **The incumbent sits in a known-collapsed regime** on joined evidence
  (Addendum 4: control FA 88.9%). Its weakness is partly an artifact, and this
  is the most likely way the result is too pessimistic.
- **This is thresholding.** §2 already says ranking is the regime where small
  verifiers ensemble well, and best-of-N peer answer selection is filed to
  `drb1-race`. **M is dead for the gate; it is untested and still open for
  answer selection**, which is where a mesh would actually rank candidates.

Reproduce: `scripts/ensemble_axis.py` (AUC + bootstrap + miss overlap) and
`scripts/ensemble_axis_op.py` (combiners + operating point + oracle bound).
Both refuse to report if the single-judge numbers stop reproducing.

### 8.1 Follow-on — the complementarity is structured, and the threshold dwarfs it

`scripts/perkind_complementarity.py`, same day, on the 2,510-item constructed
bank (`runs/headroom/scored.jsonl`), both judges matched to the same FA on
grounded items at three operating points (5/10/20%). Free — a join, no inference.

Per-kind, at FA=10%. `inc-only` = fabrications the 35B incumbent catches that
rung-1000 misses: the jury's ceiling, which no combiner can invent or exceed.

| kind | n | incumbent | rung-1000 | union | inc-only |
|---|---|---|---|---|---|
| distractor_absorption | 283 | 98.2% | 99.6% | 99.6% | **0.0%** |
| unsupported_addition | 285 | 18.2% | 97.2% | 97.2% | **0.0%** |
| cross_chunk_chimera | 127 | 85.0% | 96.1% | 96.1% | 0.0% |
| number_perturb | 80 | 46.2% | 76.2% | 78.8% | 2.5% |
| entity_swap | 286 | 35.0% | 71.0% | 73.1% | 2.1% |
| negation_flip | 75 | 25.3% | **53.3%** | 58.7% | **5.3%** |
| **ALL** | 1136 | 52.3% | 86.7% | 87.8% | **1.1%** |

**Two findings, and the second is the important one.**

**(1) The complementarity is structured, not random — and it is small.** The
second judge earns its seat only on the *relational* corruptions (polarity,
entity identity, quantity) and contributes exactly ZERO on the
presence/absence ones. Mechanically right: containment is a question any
competent judge answers, while polarity requires reading the relation. Stable
across all three FA points. But in aggregate the ceiling is +1.1% — a whole
second 35B recovers about one fabrication in ninety.

**(2) The operating point is 12x more powerful than the jury.** rung-1000
alone: 78.9% catch @ FA 5%, 86.7% @ 10%, **92.5% @ 20%**. Moving the threshold
5%->20% buys **+13.6pt**; adding a second model buys **+1.1pt**.

This is why §8's ensembles could only come out negative: they were measured
inside a zero-sum frame. A gate whose output is a BIT must trade every extra
catch against a suppressed correct answer. **A verdict that annotates rather
than refuses does not** — a false alarm costs an unnecessary hedge on a true
sentence, a true catch prevents a confident lie, and those costs are
asymmetric enough to buy the FA budget outright.

**What this licenses, and what it does not.**

- **Funds E2 (C=4).** The best judge's per-kind profile is wildly uneven
  (99.6% vs 53.3%). Narrow probes PARTITION the question instead of
  duplicating it, which is exactly why §8's panel was redundant and a
  specialist panel need not be. Fans out across nodes: N probes, one claim.
- **Names two untested mesh shapes.** *Independent evidence* — every judge in
  §8 read the same window; two copies of one model on different corpus shards
  may be more independent than two families on one page. And *coverage* —
  if FA is cheap, the marginal node is better spent on ANOTHER claim than on
  re-checking one, which scales linearly and needs no independence property.
- **Does not rehabilitate M.** The ceiling is measured and it is +1.1%.

## 9. Sequential passes — the pre-registered rule broke its own kill bar, and the correction is sharper

**Written 2026-09-08, same session as §8/§8.1.** Those measured PARALLEL passes
and killed them. This is the other regime — sequential refinement, where
"convergence" is literally the right word.

**The pre-registration** (`scripts/refinement_loop_sim.py` header, written
before the run) reasoned: a pass removes defects at `catch x b` and INTRODUCES
them at `FA x (1-b)`, since a fix applied to an already-correct claim is a new
defect. So the loop should settle at `b* = FA/(catch+FA)` — exactly where
precision hits 50% — predicting 5.96 / 10.27 / 17.78% at FA 5/10/20%, and
predicting that from a 9% production base rate FA=10% and FA=20% **degrade**.

**Every equilibrium missed, all in the same direction, and the loop improved
where it was predicted to degrade.** Kill bar: fired.

### What was actually wrong

The one-step arithmetic was exact — six of six cells within 0.2pt:

| FA | base | predicted b₁ | simulated b₁ |
|---|---|---|---|
| 20% | 9% | 18.8% | **18.8%** |
| 10% | 9% | 10.2% | 10.3% |
| 5% | 9% | 6.4% | 6.4% |

So the error was in the DYNAMICS, not the formula. The mechanism:
**damage is self-correcting.** A false alarm damages a correct claim — but the
damaged claim is now genuinely defective, so it re-enters the next pass's
flagging pool where the verifier's catch rate is far above its false-alarm
rate. Measured: **78.4 / 86.5 / 92.3%** of claims damaged in iteration 1 are
re-flagged in iteration 2, tracking the verifier's own catch (79.2 / 87.1 /
93.0%). The loop repairs its own collateral faster than it creates it. The
steady-state formula assumed a well-mixed population re-exposed each round;
the real process freezes what passes and recycles only what it touched.

### The corrected law — detectability of damage, not precision

The whole result rests on one assumption: that a claim damaged BY A REVISION is
as detectable as an INJECTED corruption. Almost certainly optimistic — injected
corruptions are entity swaps and negation flips, while revision damage is
fluent, model-generated, and is the "shared-bias residual" the audit gate
already names. So it is parameterised and swept. δ = P(revision damage scores
like a real corruption); 1−δ = P(it is invisible).

| δ | FA 5% | FA 10% | FA 20% |
|---|---|---|---|
| 1.0 | 3.0% | 2.5% | 2.3% |
| 0.6 | 4.3% | 5.9% | 8.8% |
| 0.4 | 5.1% | 7.0% | **11.6%** |
| 0.2 | 5.8% | 9.0% | **14.7%** |
| 0.0 | 6.6% | **10.2%** | **17.0%** |

*(equilibrium defect rate from a 9% start; bold = worse than where it began)*

**At δ=0 the simulation reproduces the pre-registered b\* almost exactly**
(6.6 / 10.2 / 17.0 against 5.96 / 10.27 / 17.72). The formula was never wrong —
it is the **boundary case where damage is invisible**, and it was misapplied as
the general law. The general law is:

> **A refinement loop converges iff the damage it causes is detectable by the
> same verifier that causes it. Precision governs the FIRST pass; δ governs the
> limit.** FA=5% is safe at any δ. FA=10% turns destructive below δ≈0.1.
> FA=20% turns destructive below δ≈0.5.

So an aggressive operating point is a **bet on δ**, and a conservative one is
robust to not knowing it. Two consequences that invert the usual intuition:

- **One pass is the dangerous configuration, not many.** A single pass at
  FA=20% on a 9% base rate takes the defect rate from 9.2% to **18.8%** — it
  nearly doubles it, at every δ, because δ only acts from iteration 2 onward.
  A latency-constrained system that runs exactly one gate pass is sitting on
  the worst point of this curve.
- **Iteration depth is what makes an aggressive gate safe** — and depth is
  precisely what cheap passes buy. This is the one mesh shape every result in
  §8/§8.1/§9 leaves standing, and it is about sequential depth, not parallel
  width.

### P3 confirmed: the drop variant converges to silence

The same loop with action=DROP instead of REPAIR, from a 9% base, settles in
ONE iteration and freezes (survivors are never re-judged):

| FA | defect rate | coverage |
|---|---|---|
| 5% | 8.8% → 2.1% | 100% → 88.4% |
| 20% | 9.1% → **0.8%** | 100% → **73.3%** |

Dropping a quarter of the answer buys an 11× cut in the defect rate. **Any
objective without a coverage term scores that as a triumph.** The second
condition stands: a refinement loop needs a two-sided objective or it converges
toward saying nothing, looking better by its own metric the whole way down.

### What is now the decision-relevant unknown

**δ.** It is measurable and it is not measured: take the claims the verifier
false-alarms on, have a model revise them under the gate's own instruction,
re-judge the revisions, and count what fraction the verifier catches. At FA=20%
on the constructed bank's 1,374 grounded items that is ~275 revisions — a few
hundred inference calls, and it converts the load-bearing assumption of this
section into a number.

Reproduce: `scripts/refinement_loop_sim.py` (pre-registration + P1/P2/P3) and
`scripts/refinement_loop_diag.py` (one-step check, mechanism, δ sweep).

## 10. δ measured — damage barely happens, so detectability was the wrong question

**2026-09-08, same session as §8/§8.1/§9.** §9 closed by naming δ (how
detectable revision damage is) as the decision-relevant unknown, and swept it
because it had never been measured. It is now measured, and the sweep was
asking about the wrong term.

Data: `findings/DELTA_MEASUREMENT.json`. Scripts: `measure_delta.py`
(revisions + witness), `delta_witness.py`, `judge_revisions.py`,
`delta_report.py`. Raw runs under `runs/delta/` (gitignored, per the
heavy-asset convention).

### Two instrument defects fixed first, either of which alone would have produced a wrong δ

**D1 — the false-alarm population was 93% `multi_hop_conjunction`.** Setting
tau over ALL grounded claims let one known architectural defect (the gate
judges chunks separately, ~99.5% FA on cross-chunk synthesis, §6) select the
population a refinement loop would act on. On this study's own denominator —
grounded *excluding* multi_hop, **n=965, reproduced exactly** — the population
is 78 `verbatim` + 115 `reframe`, and rung-1000's catch on non-multi_hop
fabrications is **99.9–100%**, not the 79–93% §8.1 reported over the whole
bank. *(§8.1's numbers are correct as stated — they name their denominator —
but they are not comparable to Addendum 4/5, which exclude multi_hop.)*

**D2 — the mechanical witness was mostly firing on artifacts.**
`control_mine.asserted_values` was written for chaos-transcript prose. On SEP
text three classes fire spuriously, each with a worked example in
`delta_witness.py`: values carried over from the original claim (`John Dewey`,
on a revision whose only edit was `richard`→`Richard`); sentence-initial
capitalised runs (`Despite Avicenna`, `Under Mayo`); and the quote regex
matching between two possessive apostrophes. Correcting the three took the
damage rate from **10.7% to 1.6%**. Both are reported (§18.6).

Instrument validation that passed: a **laziness control** — the reviser echoed
**0/6** true defects unchanged, and repaired them correctly (restoring "Best
Interests standard", "OI", "states of composite systems"), so its declining to
change a false alarm is judgement, not inertia. And re-scored originals
reproduce recorded margins to within 0.04–0.52 on a ±18 scale.

### The result

Operating point derived **in-run** (tau from 250 fresh grounded, catch from 100
fresh ungrounded; §9's stored tau is a max over a chunk prefix — early exit
fired on 901/2510 rows — so it does not transfer): **tau +15.629, FA 20.0%,
catch 100.0%.**

| quantity | value |
|---|---|
| originals still flagged in-run | 188/193 = 97.4% |
| **p_damage** (revision asserts a value absent from evidence AND from the original) | **2/122 = 1.64%**, CI [0.45%, 5.78%] |
| reviser declined (returned unchanged) | 9/193 = 4.7% |
| **δ** | **COULD NOT JUDGE** — 2 damaged items, 0 of them flagged |
| revisions still flagged (loop churns) | 80/193 = 41.5%, CI [34.7%, 48.5%] |
| paired margin shift | median **+3.11**, mean +6.77 |
| revisions that escaped the flag | 108/188 |

**δ is unmeasurable, and the reason is the finding.** §9 assumed
`p_damage = 1.0` — that acting on a false alarm always damages the claim. It is
**~60× smaller than that**. Recomputing §9's attractor with the measured rate:

| damage rate `d` | b* at FA=20% |
|---|---|
| 1.0 (§9's assumption) | 16.67% |
| **1.64% (measured)** | **0.33%** |
| 5.78% (CI upper bound) | 1.14% |

Against a 9% starting defect rate the loop is safe at every operating point by
more than an order of magnitude, and the conclusion survives the CI's
pessimistic end.

### The corrected law, and the constraint that actually binds

> **The reviser is a second gate.** Handed a claim that is in fact grounded and
> told it is unsupported, the model either declines to change it (4.7%) or
> rewrites it while keeping it grounded. Damage is rare because the revision
> step is itself a competent judge of the evidence — an unmodelled safety
> mechanism that sits UPSTREAM of the self-correction §9 proposed.

§9 predicted convergence and got it, but for the wrong reason: not damage being
re-detected, but damage not occurring. Both give convergence; only one is true.

**What binds instead is termination, not degradation.** 41.5% of revisions are
still flagged after one pass, while moving a median +3.11 toward grounded. So
the loop spins on the false-alarm population without hurting it — its cost is
unbounded where its quality is safe. **For a gate whose output is an
annotation, that is the right trade; for one that blocks a turn, the budget is
now the binding constraint, and it is a latency question rather than a quality
one.**

### Scope

- One operating point (FA=20%), one bank, one reviser (the daemon's resident
  4B, not the 35B primary a production turn would synthesise with).
- **The witness is sound, not complete.** It sees value-anchored damage —
  names, numbers, quoted titles. Relational damage (a negation flip, a subtle
  scope shift) is invisible to it, so **1.64% is a LOWER BOUND**, and this is
  the most likely way the result is too optimistic. Measuring relational damage
  needs a labeller this session did not have.
- 122 of 193 revisions were witness-checkable; the other 71 assert no
  extractable value.

## 11. Depth terminates at two passes — the floor is a mutual fixed point, and the loop announces it

**2026-09-08, the judge phase §10 left NEVER-RAN.** §9 and §10 established that
sequential refinement is *safe* (p_damage 1.64%, and the reviser is itself a
second gate). §10 closed by naming the cost side as the open question: 41.5% of
revisions were still flagged after one pass, so "buy passes, get quality" — the
one mesh shape §8/§8.1 left standing — rested on an unmeasured extrapolation
from a single pass. The two hypotheses and their bars were written into
`termination_sweep.py`'s header before pass 2 was run.

Generation walked all 193 claims through 5 passes on the daemon's 4B (965 rows,
zero errors). Judging is one run against one rung-1000 server, tau derived
in-run from 150 fresh grounded non-multi_hop claims: **tau = +14.317**.

### The result — the stuck-floor hypothesis fired

| pass | in | cleared | rate | still flagged | cumulative damage |
|---|---|---|---|---|---|
| 1 | 193 | 142 | 73.6% | 51 | 2/123 = 1.6% |
| 2 | 51 | 13 | **25.5%** | 38 | 0/31 |
| 3 | 38 | 1 | 2.6% | 37 | 0/19 |
| 4 | 37 | 2 | 5.4% | 35 | 0/18 |
| 5 | 35 | 0 | 0.0% | 35 | 0/18 |

**Primary bar: MISSED, decisively.** Pass-2 clearance 13/51 = 25.5%, Wilson 95%
CI [15.5%, 38.9%] — the optimistic end of the interval is still below the 40%
floor bar. Clearance does not decay geometrically, it collapses: 73.6 → 25.5 →
2.6 → 5.4 → 0.0. Passes 3-5 together clear **three items**.

**Secondary bar: PASSED.** Cumulative damage against the ORIGINAL claim is
0/18 at pass 5 and never exceeds §10's per-pass 1.64%. Depth is safe. It is
simply bounded.

### The floor is a hard core, not a threshold artifact

Re-running the whole loop at other thresholds on the same judged margins:

| tau | p1 | p2 | p3 | p4 | p5 | floor |
|---|---|---|---|---|---|---|
| +12.00 | 81.9% | 22.9% | 0.0% | 0.0% | 0.0% | 27/193 = 14.0% |
| +14.32 | 73.6% | 25.5% | 2.6% | 5.4% | 0.0% | 35/193 = 18.1% *(this run)* |
| +15.63 | 59.1% | 13.9% | 5.9% | 1.6% | 1.6% | 62/193 = 32.1% *(§10's tau)* |
| +16.50 | 43.5% | 11.9% | 4.2% | 2.2% | 0.0% | 90/193 = 46.6% |

Every operating point shows the same shape, and **at §10's own tau the pass-2
rate is 13.9% — worse.** This run drew the charitable threshold (§18.6: the
change is reported in the direction that hurts the finding as well as the one
that helps it). The floors are strictly NESTED — all 35 items stuck at +14.32
are among the 62 stuck at +15.63, 100% overlap — so the residual is a hard core
that tau resizes rather than a population tau invents.

### Why it is stuck: both sides stopped working

The 35 sit a median **+4.70** against tau **+14.32** — about nine points below
on a ±18 scale, not at the boundary (one item is 0.13 away; the rest are not).
And they do not move: median margin shift from pass 1 to pass 5 is **+0.04**,
mean −0.62, with 18/35 drifting toward tau and 17 away — a coin flip.

The reason is visible in the text: **34 of 35 have a pass-5 revision byte-identical
to pass 4.** The reviser is returning them unchanged. §10 found that the reviser
is a second gate that declines to damage claims it judges grounded; this is that
same mechanism at its limit. The verifier says unsupported, the reviser says
there is nothing to fix, and both are confident. The loop is not *spinning* on
this population — it is **idling**, and §10's phrase "its cost is unbounded" is
wrong in a way that is good news.

> **A refinement loop's residual is a disagreement between two confident
> components, and neither compute nor iteration is the currency that settles
> it.** Composition, over the whole population: `verbatim` claims are stuck at
> 24.4% (19/78) against `reframe` at 13.9% (16/115).

### Termination is free, and it is not the gate clearing

The loop signals its own fixed point one pass before the gate would: a revision
identical to its predecessor. Over all 193 claims that no-op rate runs 68.4% →
91.2% → 94.3% → 96.4% across passes 2-5. **Stop on the no-op, not on the
verdict** — it converts §10's open cost question into a fixed budget of about
two working passes plus one detection pass, with no quality cost, because a
pass that changes nothing cannot clear anything.

### What this does to the mesh argument

§8 killed parallel breadth (same-lineage judges nest their errors; the whole
jury is worth +1.1pt against a threshold move worth +13.6pt). Sequential depth
was the survivor. It now has a measured ceiling: **80.3% of the false-alarm
population clears in two passes, and everything after is 1.6%.** So the
compute→groundedness conversion is bounded on BOTH axes — breadth dead, depth
worth roughly 2x rather than Nx.

The operating point dominates again, exactly as in §8.1: the floor moves from
14.0% to 46.6% across the tau range while five passes of compute move it by
1.6pt. **Of the two mechanisms the frame carried forward — depth and coverage —
depth is now measured and capped, which leaves coverage as the only untested
route from mesh capacity to groundedness.** That is a retrieval question, not a
verification one, and nothing in §8-§11 speaks to it.

### Scope

- One bank, one reviser (the daemon's 4B, not a 35B primary), one judge lineage.
  The floor being a *disagreement* makes the shared-lineage caveat sharper here
  than elsewhere: a reviser of a different lineage than the verifier is the one
  intervention this experiment cannot rule out, and §8's lineage finding is the
  reason to expect it might matter.
- The damage witness is the §10 witness and inherits its bound — value-anchored
  damage only, so cumulative damage is a LOWER bound.
- Clearance rates are one run at one tau; the sensitivity table above is
  re-thresholding the same margins, not a second measurement (§18.5).

Reproduce: `scripts/termination_pipeline.sh` (serve + judge),
`scripts/termination_sweep.py --phase judge --passes 5`.
Data: `runs/delta/termination.jsonl`, `runs/delta/termination_summary.json`.

## 12. Spike — a stack change is invisible against its own resampling noise, and has no sign

**2026-09-09, ten seconds of compute, no inference.** §11 left coverage as the
only untested route from mesh capacity to groundedness. The proposed product
shape was re-evaluation: on a STATIC corpus the mesh itself is the only thing
that changes, so a past answer can be upgraded without any source moving —
*"SEP didn't change, we got better at reading it."* That is only a product if a
stack change flips CLAIM-LEVEL verdicts at a rate distinguishable from noise.

Rung 6 had already run the experiment and nobody had read it this way: arm A
(pre-rung-3) vs arm B (rung-3+4), same 21 SEP questions, same static corpus,
same judge, **two runs per arm** — so the same files carry the signal and its
own noise floor. The bench headline was a per-question MEAN, which is preserved
exactly when N facts flip up and N flip down. This reads
`synth.judge_evidence[].present`, the per-fact boolean, which is the granularity
of a thing a user was told.

| comparison | stack delta | facts | flips | rate | up | down |
|---|---|---|---|---|---|---|
| within-stack (A↔A2, B↔B2) | none | 318 | 23 | **7.23%** | 11 | 12 |
| across-stack (A↔B, A2↔B2) | 2 days | 318 | 21 | **6.60%** | 8 | 13 |
| baseline (July 6 ↔ Sept 8) | ~2 months | 636 | 79 | **12.42%** | 37 | 42 |

**Primary bar MISSED: 0.91x against a bar of 2.0x.** Re-running the identical
binary flips 7.2% of user-visible facts; changing the retrieval stack flips
6.6%. The rung-6 stack change is invisible against resampling.

**The extension is the finding.** Over ~2 months of stack changes the rate
roughly doubles to 12.4% — 1.72x the noise floor — so churn *does* scale with
stack delta and the mechanism is real. But the net movement is **+1, −1, −3, −2**
across the four comparisons, and −0.79% overall. Two months of retrieval work
that moved bench means produced as many fact-level downgrades as upgrades.

> **The effect is real and directionless.** We cannot currently tell an
> improvement from a reshuffle at the level of a thing a user was told — which
> is exactly the level a notification would speak at. A re-evaluation feature is
> therefore blocked not on the trigger's rate but on ATTRIBUTION: establishing
> that a stack change improved THIS claim, rather than the mean.

Two consequences beyond the feature. First, the two-stack paired re-run
(old retriever+judge vs new, same process) stops being an optimisation and
becomes the whole mechanism. Second, and independent of any of this: **a 7.2%
per-fact resampling floor on a 21-question bank means most single-run
comparisons on it are measuring noise** — rung 6 said as much at the mean, and
this quantifies it at the fact level.

Confound, named: the July baseline is un-isolated while the arms are isolated to
`sep` (rung 6 README); the isolated and un-isolated May baselines read the same
judge mean, so the comparison holds with that attached. And n is small — 7.23%
vs 6.60% is itself within noise, so the honest primary reading is
"across ≈ within", not "across < within".

Reproduce: `scripts/verdict_flip_spike.py` (bars in the header, written before
the run).

## 13. The retrieval ceiling is a QUERY artifact, not a corpus gap — and that is the first live mesh mechanism

**2026-09-09, minutes, no synthesis.** §12 killed re-evaluation as a product.
This is the follow-on it pointed at: of the two routes from mesh capacity to
groundedness, is the residue we cannot retrieve **absent** from what we hold, or
**unreached**? Acquisition and fan-out are different builds and the fork had
never been measured.

### Configuration diversity is exhausted

Rung 6's four arms are 2 stack versions x 2 samples over the same 21 SEP
questions. Their retrieval coverage of expected facts:

| | facts retrieved | of 158 |
|---|---|---|
| armA / armA2 / armB / armB2 | 141 / 140 / 139 / 140 | 89.2 / 88.6 / 88.0 / 88.6% |
| best single arm, per question | 148 | 93.7% |
| **union of all four** | **150** | **94.9%** |

**Diversity of configuration buys +1.3 points, on 2 of 21 questions.** Four
retrieval configurations converge and miss the SAME 8 facts. This is §8's
lineage result in the retrieval layer: same-lineage judges nest their errors,
and **same-query retrievals nest their misses.** Twenty rerankers over one
library are twenty views of one blind spot.

### But the hard core is one query away

The 8 facts no arm reached are `self-reference`, `BonJour`, `mentalism`,
`MacCallum`, `regularity theory`, `Worrall`, `individualism`, `descriptivism` —
canonical vocabulary of the very entries their own questions name. Issuing each
as an ORACLE query against the same `sep` corpus, scored by the bench's own rule
(`score.rs::partition_facts`, every token >= 3 chars present):

- **hard core: 8/8 recovered.** Bar was >= 5/8.
- **instrument control: 7/7** on facts the arms did find, so the probe detects
  presence rather than always answering yes.

The haystack is the CLI's truncated titles + snippets (~190 chars a hit), so the
probe is conservative and 8/8 is a LOWER bound on presence.

> **The window is not the corpus.** §11 established that you cannot extract more
> groundedness than the window contains. This shows the window's limit is a
> QUERY-FORMULATION artifact, not the library's contents — and query diversity is
> cheap, is the fast half of the pipeline, and is genuinely independent across
> nodes in a way configuration diversity is not.

### What this licenses, and what it does not

Live mechanism: **fan out over formulations, not over models.** One question
decomposed into sub-queries, N cheap retrievals in parallel over the same or
different libraries, the union of evidence returned to ONE synthesis — which
leaves the expensive stage (58.8 s median here) singular while multiplying the
stage that determines the ceiling.

The limit, stated plainly because it is the whole caveat: **the oracle query is
the fact itself, and you cannot query for a fact you do not know you are
missing.** This proves REACHABILITY and bounds the problem; it does NOT show
that any realistic formulation strategy recovers the gap. The next experiment is
exactly that — generate sub-queries from the question alone, fan out, union, and
measure what share of the 8 comes back without the oracle.

### Scope

- One bank (21 SEP questions), one corpus, n=8 hard-core facts.
- The bench matcher is keyword-substring by construction (`score.rs`), so both
  the miss and the recovery are keyword events, not semantic ones.
- Retrieval cost is UNMEASURED on this path: `embed_ms` and `search_ms` are 0
  for all 21 questions while synthesis runs a 58.8 s median. "Retrieval is the
  cheap part" is believed here, not measured, and instrumenting it is owed
  before anything is designed around the asymmetry.

Reproduce: `scripts/oracle_query_probe.py` (bars in the header, written before
the first query).

## 14. Fan-out over formulations is dead; k is the lever, and the binding cost is context not time

**2026-09-09.** §13 proved the hard core is REACHABLE by an oracle query and
said plainly that reachability is not a strategy. This removes the oracle:
sub-queries generated from the QUESTION ALONE by the resident 4B, three arms
through one instrument (plain `corpus search`, bench matcher, truncated
snippets — identically biased across arms, so deltas are conservative).

### The pre-registered bar was mis-specified, and saying so is the result

The bar read "fanout >= 4/8 of the hard core -> live". Fan-out returned **4/8**
and the script duly printed LIVE. **The baseline in the same harness returned
5/8.** An absolute bar with no paired control cannot distinguish "the strategy
works" from "anything at this budget works", and the paired control beats the
strategy. The verdict is therefore NEGATIVE, not positive, and the bar is
recorded as the defect it was.

| arm | hard core | all 158 facts | retrievals | context chars |
|---|---|---|---|---|
| baseline — 1 query @ 40 | **5/8** | 138 = 87.3% | 21 | 182,339 |
| fan-out — 4 sub-queries @ 10 | 4/8 | 138 = 87.3% | 84 | 164,444 (0.90x) |

At an equal hit budget, splitting it across four narrower queries returns **more
redundancy, not more coverage** — same total facts, fewer unique characters,
four times the retrievals. Decomposition is not free diversity; the sub-queries
land on the same documents.

### The lever is k, and it is nearly free in time

One query, same harness, sweeping the hit count:

| k | hard core | all facts | context chars | retrieval wall / question |
|---|---|---|---|---|
| 10 | 2/8 | 114/158 = 72.2% | 48,823 | 1.20 s |
| 20 | 2/8 | 126/158 = 79.7% | 93,322 | 1.24 s |
| 40 | 5/8 | 138/158 = 87.3% | 182,339 | 1.43 s |
| 80 | **7/8** | **147/158 = 93.0%** | 360,195 | 1.55 s |

**7 of 8 hard-core facts and 93.0% coverage from asking one query for more
hits.** Against the rung-6 arms' 88.0-89.2% with an atlas walk and a reranker,
and against fan-out's 4/8. The boring lever beats the clever one, which is the
third time this arc has landed there (§8.1: the threshold beat the jury;
§11: the bound beat the ladder).

**And retrieval wall time is almost flat in k** — +29% for 8x the hits, because
the query embedding dominates and the search itself is cheap. Measured against a
58.8 s synthesis median, retrieval is **~2-3% of a turn**. This closes the debt
§13 recorded: the premise "retrieval is the fast, load-bearing part" is now a
number rather than a belief.

### So the constraint is context, and that relocates the mesh mechanism

Time is not what k costs. **Characters are: 7.4x from k=10 to k=80**, and that is
on TRUNCATED snippets — real chunks are several times larger, so the context bill
is materially worse than this table shows while the coverage figures stay
conservative. You cannot feed a 93%-coverage candidate set to one synthesis.

> **Retrieval is nearly free and selection is the scarce resource.** The mesh's
> job is therefore not to retrieve more in parallel — one node can already
> over-retrieve almost for nothing — but to REDUCE a large candidate set to a
> small high-value window. That is judging N candidate chunks, which is
> embarrassingly parallel, and unlike §8's jury the nodes judge DIFFERENT
> chunks rather than voting on one claim, so lineage-nested error does not
> apply the same way.

That is the one shape this arc has not tested and the first the measured cost
structure actually favours. It is a hypothesis, not a result.

### Scope

- One bank, one corpus, one decomposition prompt at K=4. A different prompt or a
  larger K could differ; what is measured is that THIS realistic strategy loses
  to a larger k at equal budget.
- Snippet-truncated haystack: coverage is a lower bound, context cost an
  understatement.
- Coverage is retrieval-layer only. Whether a bigger window improves the ANSWER
  is unmeasured and prior work says it is not monotone (prefill dominates;
  K-cuts have hurt synthesis). ~21 x 59 s per arm to find out.

Reproduce: `scripts/formulation_fanout.py` (bars in the header, mis-specification
and all).

## 15. Selection is a context-efficiency multiplier, not a ceiling raise — and production may already have it

**2026-09-09.** §14 relocated the mesh mechanism to selection: retrieval is
nearly free in time, context is scarce, so reduce a large candidate set to a
small high-value window. This bounds what ANY selector could win, before pricing
one.

**Inventory first (§19), and it changes the question.** Two things already
exist. `corpus-engine/src/index/search.rs:659` composes the final set by greedy
**facility-location** over the pool instead of top-k truncation, behind a
`coverage_factor` flag with a no-lose fallback. And the cross-encoder reranker
was REJECTED 2026-08-04 on cost, not quality — `quality/env-flags.toml:1152`
records "+2.8s TTFT + 4th resident slot + 60x memory fragility", with the
re-open condition stated as **"a NEW integration shape (off-TTFT-path margins,
no in-turn slot)"**. That sentence describes a mesh peer, which is the first
time this arc has found the mesh answering a question the repo had already
written down.

### The curve

One retrieval per question at k=80 (1.55 s/question), then two policies over the
same candidates: `rank` = first N hits (top-k truncation, the paired control),
`oracle` = greedily pick the N hits maximising fact coverage (reads the answer
key; an unreachable ceiling, like §13's oracle query).

| N | rank | oracle | headroom | chars @ rank |
|---|---|---|---|---|
| 5 | 50.6% | **86.1%** | **+35.4%** | 21,751 |
| 10 | 63.3% | 86.1% | +22.8% | 43,498 |
| 20 | 75.3% | 88.0% | +12.7% | 86,961 |
| 28 | 79.7% | 88.6% | +8.9% | 121,540 |
| 40 | 83.5% | 89.2% | +5.7% | 173,350 |
| 80 | 90.5% | 90.5% | +0.0% | 346,084 |

**Selection does not raise the ceiling — it collapses the window.** Both
policies land on 90.5% once N=80 (they are the same set). What changes is the
price: the oracle reaches **86.1% coverage in FIVE chunks / 21.7k chars**, where
rank order needs 40 chunks / 173k chars to reach a *lower* 83.5%. That is ~8x
less context for more coverage, and it is the "small high-value window" argument
stated as a ratio rather than a hope.

The secondary bar as written ("smallest N where the oracle reaches the ceiling")
was **uninformative by construction** — defining the ceiling as the oracle's own
maximum puts it trivially at N=80. Second bar-specification defect in two
sections; the corrected comparison is the one above, computed from the same data.

### The caveat that cuts hardest, and it is not in the bars

`rank` here is NAIVE top-k truncation. **Production is not naive.** The rung-6
arms score 88.0-89.2% at 28 chunks with their atlas walk and `dedup_by_source`,
against this oracle's 88.6% at N=28. On the face of it the shipped pipeline is
already at oracle parity at that window — so **+8.9% is headroom over naive
truncation, NOT over what we ship**, and the honest statement of the remaining
headroom is that it is unmeasured and may be near zero.

The comparison is confounded and cannot be resolved from here: the arms use full
chunks, this harness uses ~190-char truncated snippets, which biases these
coverage numbers DOWN and the context figures down as well. Settling it needs
one instrument — full chunks through the eval pipeline with `coverage_factor`
on and off.

### What this licenses

- **The small-window claim is real and quantified** (86.1% in 5 chunks), and it
  is the right shape for a context-scarce turn.
- **A new selector is NOT licensed** on this evidence, because the incumbent may
  already capture the gap. The next measurement is not a better selector, it is
  `coverage_factor` on/off, full chunks, one instrument.
- The reranker's re-open condition is a live thread and belongs to whoever
  measures the above first — its quality already passed; only its integration
  cost failed, and that is exactly the cost a peer absorbs.

### Scope

One bank, one corpus, k=80 pool, truncated snippets, greedy set-cover on the
answer key. Retrieval-layer coverage only: whether a 5-chunk window synthesises
a BETTER answer than a 28-chunk one is unmeasured, and prior work says window
size is not monotone for synthesis.

Reproduce: `scripts/selection_headroom.py`.

## 16. CRAG-style adaptive routing: the evaluator buys nothing, and the prize is small even with a perfect one

**2026-09-09.** §15's architecture argument was the CRAG shape — grade the
retrieved window with something cheap, escalate only when it is judged
insufficient, synthesise once. That rests on one unmeasured assumption: that
sufficiency is PREDICTABLE without the answer key. This measures that and
nothing else.

**Cheapest possible instrument, by construction.** ONE retrieval pass per
question at k=80 — every window is a prefix, so the sweep costs zero extra
retrievals. Ground truth is free (the bench's own matcher over the bank's
expected facts). FREE predictors are tested BEFORE paying for a model. The
evaluator is one constrained token on the resident 4B, binary and uncalibrated
because the daemon returns no logprobs (recorded dead end). Ground truth:
6/21 windows fully sufficient at k=28.

### B1/B2 — sufficiency is barely predictable, and the model loses to counting

| predictor | AUC |
|---|---|
| `f_max` (top similarity) | 0.511 |
| `f_mean5` | 0.561 |
| `f_gap` (top1 − top10) | 0.378 |
| **`f_titles` (distinct documents)** | **0.661** |
| `f_model` (4B evaluator) | 0.633 |

The 4B sufficiency judge is **worse than counting how many distinct documents
are in the window**, and the best signal available is itself weak. Model lift
over the best free predictor: **−0.028**, against a bar of ≥+0.10 to earn its
call and ≤+0.03 to skip it. Skip it.

Similarity scores being near-useless (0.51) is the predicted result and worth
keeping: a score says how well the top chunk matches the QUESTION, and §13/§14
established the misses are terms the question never contains.

### B3 — and the routing itself buys exactly nothing

| policy | coverage | context |
|---|---|---|
| fixed k=28 | 126/158 = 79.7% | 121,540 |
| **adaptive (escalate 11/21 on the evaluator)** | **139/158 = 88.0%** | **238,901** |
| **fixed k=55 — iso-cost control** | **139/158 = 88.0%** | **238,444** |
| fixed k=80 | 143/158 = 90.5% | 346,084 |

**+0 facts.** A single dial, set once, matches the adaptive policy exactly at the
same context budget — and needs no evaluator, no extra call, and no branch.

### The ceiling: even a PERFECT evaluator wins at most +3.2 points

Escalating the N questions with the largest TRUE gain (an oracle evaluator; the
ceiling on any CRAG-style router here), against the best fixed k at each budget:

| escalated | adaptive | iso-cost fixed k | delta |
|---|---|---|---|
| 3 | 84.2% | k=34 → 82.3% | **+3** |
| 5 | 86.7% | k=39 → 83.5% | **+5** |
| 8 | 88.6% | k=46 → 86.1% | +4 |
| 11 | 90.5% | k=53 → 88.0% | +4 |
| 15 | 90.5% | k=62 → 89.9% | +1 |

> **The prize is +5 facts of 158 at its best point, and the cheapest evaluator
> captures 0 of it.** So this is not "our evaluator was bad" — it is that
> adaptive escalation has almost nothing to win on this bank no matter how good
> the evaluator gets.

### The law, and when it would NOT hold

> **Adaptive escalation is worth the discriminative power of its evaluator times
> the VARIANCE of per-query retrieval difficulty.** At AUC≈0.5 it degenerates
> into a fixed-k policy with extra steps — escalating a near-random half is, in
> expectation, raising k for everyone by half as much. And when per-query
> difficulty is uniform, it degenerates into fixed-k *even with a perfect
> evaluator*, which is what the oracle table shows.

That names the condition under which the published pattern does work, and why
this bank is the wrong place to see it: 21 questions on ONE encyclopedia, all
answerable from it, is about as homogeneous as retrieval difficulty gets. CRAG's
own setting is heterogeneous — some queries answerable from the corpus, some
requiring the web — which is exactly where the variance term is large. **The
mechanism to test is therefore acquisition routing on a MIXED corpus, not
sufficiency routing on a single one.**

### Scope

- The cheapest evaluator (zero-shot, one token, general 4B), NOT CRAG's
  fine-tuned retrieval evaluator. That limit does not touch the oracle bound,
  which holds for any evaluator.
- Binary and uncalibrated: no logprobs from the daemon, so no threshold sweep.
- One bank, 21 questions, 6 positives — AUCs on that base are wide.
- Retrieval-layer coverage only; no synthesis arm.
- Fourth time in this arc the boring lever matched or beat the clever one
  (§8.1 threshold > jury, §11 bound > ladder, §14 k > decomposition, this fixed-k
  > adaptive routing). That is now a pattern worth treating as a prior.

Reproduce: `scripts/crag_sufficiency_probe.py` (bars in the header).
