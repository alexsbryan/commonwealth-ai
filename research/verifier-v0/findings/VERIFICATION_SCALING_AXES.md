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
