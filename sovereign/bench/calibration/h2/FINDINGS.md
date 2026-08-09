# H2 gate — COULD-NOT-JUDGE, twice over, and the second reason is the interesting one

**H2 was not proven and was not killed.** The §7.3 H2 measurement cannot be made
from the artifacts that exist, and there are **two independent blockers, either
one sufficient**:

1. **The hallucination label is zero-positive.** Across every gated-eligible
   turn in every committed chaos artifact, `is_hallucination` (`score.rs:281`)
   fires **0 times out of 71** on the held-out split. AUROC over a single-class
   set is undefined — for `semantic_entropy`, for `agreement`, **and for the
   Critic's `violation_prob`**. The §7.3 comparison has no side that works.

2. **The statistic is constant.** k=5 draws over five frozen turns — three
   `present`, two `absent_adjacent` — produced **1 distinct value out of 5 on
   every single turn**. `semantic_entropy = 0` and `agreement = 1.0` everywhere,
   in both label classes. An AUROC over a constant is 0.5 by construction.

Blocker 1 is a bank problem and a bank order can fix it. **Blocker 2 survives
any bank fix**, and it is a finding about the hypothesis rather than about the
data. It is the reason this document is worth reading.

The gate was allowed to fail. `H2Outcome` has four values, not two, and the
branch that reaches a real verdict is exercised by
`the_gate_can_reach_a_real_verdict_when_both_hold` — a gate nobody has watched
*pass* is as suspect as one nobody has watched fail.

---

## 1. The label supply: zero positives, and the instrument that nearly hid it

### The census

Gated-eligible means the turn carries **a question AND non-empty sealed
evidence** — H2 re-poses `(question, evidence)`, so a turn with no evidence has
nothing to seal.

| source | role | rows | eligible | **hallucinating** | clean | with numeric `vp` | absent-class probes |
|---|---|---|---|---|---|---|---|
| `saltgrass_compound_gv_shadow_20260808b` | calibration | 20 | 20 | **0** | 20 | 20 | 0 |
| `saltgrass_gv_shadow_20260808b` | held-out (dev) | 37 | 34 | **0** | 34 | 34 | 11 |
| `secret_agent_gv_shadow_20260807` | held-out (2nd source) | 43 | 37 | **0** | 37 | 37 | 11 |
| | **held-out total** | | **71** | **0** | **71** | **71** | 22 |

Across **all 12** committed chaos artifacts (383 rows) there are **6**
hallucination-labelled turns in total, every one `absent_out_of_domain` — a
*missing-provenance-caveat* label, not a fabrication label. Exactly **one** of
the six is gated-eligible (`ood-water-boiling`, in `saltgrass_ctl_r1`), and that
artifact carries no `violation_prob`, so it cannot enter the Critic comparison
either.

**The `absent_adjacent` class — the fabrication shape H2 exists to detect — has
zero positives in every artifact ever committed.** The banks contain 22 such
probes across the two held-out sources. The incumbent abstains or grounds on
every one. That is a good product result and a dead measurement surface.

### Naive ceilings (standing discernment directive, 2026-08-08)

| set | n | labels | naive ALWAYS-hallucinated | naive NEVER-hallucinated | mechanism |
|---|---|---|---|---|---|
| held-out (both sources) | 71 | 0 halluc / 71 clean | 0.0000 | **1.0000** | undefined |

A scorer that answers "never hallucinated" and looks at nothing scores
**1.0000**. AUROC is not merely bad on this set, it is **undefined**, and the
verdict artifact reports `auroc_defined: false` rather than printing `0.5` —
which a reader would mistake for a measured coin flip.

### The instrument was validated before the result, and it needed to be

The first census this order ran returned zero hallucinations everywhere,
including on rows that had them. **That was a bug in my instrument, not a
property of the data**: I keyed the out-of-domain arm on `absent_ood`, which is
what the Rust variant `AbsentOutOfDomain` reads like, where serde actually
writes **`absent_out_of_domain`** (`question.rs:21-32`, `rename_all =
"snake_case"`). A label keyed on the wrong string returns `false` for every row
and looks exactly like a clean bank.

It was caught by cross-checking against ground truth this repo had already
committed: `saltgrass_gv_shadow_20260808b.run.log:151` reports
`hallucination-rate 0.09`. The corrected port reproduces it exactly — 1
hallucination over 11 absent-class probes = 0.0909.

That cross-check is now a **test**, not a one-off (`rows.rs`,
`the_port_reproduces_a_committed_run_logs_rate`). It was itself broken once more
before it was trustworthy: the first version resolved the artifact path relative
to the process cwd, did not find it, and **silently returned** — it passed while
`is_hallucination` was deliberately sabotaged. It now resolves from
`CARGO_MANIFEST_DIR` and **asserts the artifact exists**, because a check that
can skip itself is a §18.1 never-ran wearing a pass. Re-sabotaged, it fails:
`left: 0, right: 1 — one hallucination, per the run log`.

### The Critic is not aligned with this label either

Even setting the zero aside, the comparison target does not track the label. The
highest-`vp` turns on the dev bank are `distract-forger` (0.8902) and
`present-doctor-verdict` (0.5837) — both scored non-hallucinating, neither an
absent probe.

### Competence, as CONTEXT — explicitly NOT the gate's label

Reported because a reader must be able to see the artifacts are not uniformly
clean: on the eligible dev set, `answer_correct` is 18 true / 6 false / 10 null.
The incumbent gets answers **wrong** here; it just does not **fabricate**.

Substituting `answer_correct` for the hallucination label would make the gate
scoreable and would be a lie about what was measured — §18.3's silent
substitution. It is refused. The field in the verdict artifact is named
`competence_context_not_the_label` so the refusal is structural rather than
remembered.

---

## 2. The statistic is constant — the blocker that survives a bank fix

This is deliverable 3's finding and it is the substantive one.

### What was drawn

BeefyMac (M2 Max, 64 GB), **Qwen3.5-4B-Q6_K**, k=5, seeds pinned, evidence read
from the frozen `saltgrass_gv_shadow_20260808b` transcript. Nothing generated
beyond the k values against already-frozen evidence.

| turn | qtype | distinct / 5 | reproducible | sample |
|---|---|---|---|---|
| `present-victim` | present | **1/5** | yes | "Corwin Pellow" x5 |
| `present-inn` | present | **1/5** | yes | "The Cold Lantern" x5 |
| `present-weapon` | present | **1/5** | yes | "an iron edge" x5 |
| `absent-cargo-manifest` | absent_adjacent | **1/5** | yes | "NONE" x5 |
| `absent-hetch-firstname` | absent_adjacent | **1/5** | yes | "NONE" x5 |

`semantic_entropy = 0`, `agreement = 1.0`, on **every turn in both classes**.

### It is not the sampler — that was tested, not assumed

A degenerate draw has two explanations and they are indistinguishable from the
draw alone. So the discriminator was run rather than a cause guessed
(principle 2):

**At T=5.0, `top_p=1.0`, `top_k=0`: 5/5 distinct**, and obvious multilingual
token noise. The pinned seeds reach `dist`, the k sequences sample
independently, the shared-prefix fanout is correct. **The sampler works.**

### A disproven hypothesis, recorded as disproven

I first believed the Qwen3 Instruct profile's `top_p = 0.80` was the cause:
`build_sampler` composes llama.cpp's conventional order (`top_k -> min_p ->
top_p -> temp -> dist`), so the nucleus truncates the **un-tempered** posterior,
and a peaked posterior would collapse it to one candidate ahead of `temp`.

Tested. **Wrong.** With `top_p = 1.0` and `top_k = 0` the draws were still
byte-identical at T=0.7 and T=1.0. `DRAW_TOP_P`'s doc comment now says so in
place of the claim it used to make. The draw still sets its own nucleus, on the
principled ground that a measurement of a distribution should not open by
truncating it — not on the disproven causal one.

### The temperature sweep — where the finding actually lives

`top_p = 1.0`, `top_k = 0` throughout. Artifacts in `sampler_sweep/`.

| T | distinct / 5 | what the samples are |
|---|---|---|
| 0.7 | **1/5** | the correct value |
| 1.0 | **1/5** | the correct value |
| 1.5 | 4-5/5 | **not alternative answers — token garbage**, with 2 of 5 still exactly "Corwin Pellow" |
| 2.0 | 5/5 | pure noise |
| 3.0 | 5/5 | pure noise |

**There is no temperature at which this model produces multiple *coherent*
candidate values.** It goes from a delta function straight to incoherence, with
no intervening band of plausible-but-different answers.

That band is exactly what semantic entropy needs. Farquhar et al.'s premise is
that when the asserted value is unsupported, samples diverge **in meaning**.
Divergence into multilingual token noise is not divergence in meaning: those
samples do not cluster into competing answers, they cluster into garbage, and an
entropy computed over them measures the temperature, not the model's
uncertainty.

### What this does and does not say

**Does not say H2 is dead.** Three real limits, each of which could change it:

1. **Model.** Measured on a **4B**, not on the harvest model
   (`FINAL-Bench_Darwin-36B-Opus-Q6_K`). A larger model's posterior over the
   same turn may be less peaked. **This substitution is named, not silent** —
   free RAM was ~14.5 GB with the shared daemon resident, and this order forbids
   restarting it.
2. **Unit.** The draw asks for a **short extractive value** (§5 H2's own
   specification, <=24 tokens, `extract_answer_value`'s budget). A value copied
   verbatim out of sealed evidence is close to a lookup. Sampling *full prose*
   answers, or values on turns whose evidence does **not** contain them, is a
   different distribution and is untested here.
3. **Turns.** Five turns. The two absent probes are the H2-relevant case and
   both drew unanimous, correct `NONE` — which is the incumbent behaving well,
   and also the reason entropy cannot separate present from absent on *this*
   evidence set.

**Does say:** on this model and this unit, the sampling distribution carries no
usable signal, and no amount of label supply changes that. A follow-on order
that fixes only the banks will re-run this gate and get could-not-judge again,
for reason 2.

---

## 3. The clustering floor — calibrated, committed, and honest about what it is

The floor exists and its curve is committed beside the code, per principle 2.

| | |
|---|---|
| signal | `min(margin(a->b), margin(b->a))` — the bidirectional rule as one number |
| calibration set | 341 value pairs from the **dev** banks only |
| labels | **51 same / 290 different** |
| **non-trivial positives** | **4** |
| AUROC | 0.9971 |
| AUROC, non-trivial positives only | 0.9759 |
| best balanced accuracy | 0.9787 |
| **floor** | **4.0757** (the curve's best-BAcc threshold, by rule) |

**It is equivalence-calibrated, not outcome-calibrated.** §7.3 calibrates H2
against the hallucination label; that label is zero-positive, so the floor is
instead fitted to the question rung (b) is actually asked — *are these two
values the same answer?* Labels come from frozen `asserted_value` rows
restricted to `answer_correct = true` AND `asserted_value_grounded = true`; same
probe across runs = same value, different probes = different values. Both
assumptions are written into the artifact's `provenance` field, and a negative
pair the deterministic kernel finds equivalent is **dropped rather than
mislabelled**.

**Do not quote 0.9759 as paraphrase performance.** It is computed on **four**
pairs. 47 of the 51 positives are settled for free by rung (a), because the
pipeline is effectively deterministic at temperature 0 (36/37 and 20/20
byte-identical answers across two harvests — `h4/FINDINGS.md`). The headline
0.9971 is inflated by those trivial positives, which is exactly why the
restricted number is computed and reported at all.

**Re-calibration against outcomes is expected the moment a bank supplies
positives.**

---

## 4. Cost

| | value |
|---|---|
| H2 k-sample draw, p50 | **5,264 ms** (measured, 5 turns, Qwen3.5-4B) |
| Critic call, p50 | **5,644 ms** (cited: `h4/FINDINGS.md`, from `saltgrass_gv_shadow_20260808.run.log`) |
| ratio | **0.93** — against §7.3's bar of **< 0.20** |

**Read with both caveats or not at all.** (a) It is not apples-to-apples: the
draw ran on a 4B, the Critic figure is the 36B's. (b) The draw's cost is
**prefill-dominated** — ~5,000 evidence tokens paid once per turn against <=120
decode steps. §5 H2's design shares the turn's existing evidence KV, so this is
an **upper bound** on the marginal cost, not the marginal cost. The one thing
the number does establish is that the *decode* half is negligible: the
shared-prefix fanout works and k=5 costs ~120 lockstep steps.

---

## 5. What would make this measurable

Two orders' worth, and they are independent.

### A. The bank (fixes blocker 1)

The dev banks structurally cannot supply the label. The shortfall is arithmetic:
22 `absent_adjacent` probes across the two held-out sources, **zero** of which
the incumbent fabricates on.

A follow-on bank order needs:

1. **At least 8 `absent_adjacent` probes the incumbent actually FABRICATES
   on** — answers with an ungrounded specific value, so `asserted_value_grounded
   = false` and `agent_action = answered`. Not more runs of the existing probes:
   the pipeline is reproducible, so re-running measures the same thing again.
2. **Evidence retained** on every one. H2 re-poses `(question, sealed evidence)`;
   an evidence-free row is unreplayable regardless of its label. Note this is
   *already* true of `ood-*` probes: 5 of the 6 hallucinations in the entire
   frozen corpus are unusable for exactly this reason.
3. **Spread over >= 6 distinct turns**, split so **>= 2 fabricating turns land
   on each side** of the calibration/holdout split. Two per side is the floor
   for a leakage-free split.
4. **Target >= 20 positive turns** so the naive ceiling on the held-out side
   sits meaningfully below 1.0. With 34 held-out turns, even 6 positives moves
   "never-hallucinated" to 0.82 — which is the point.
5. **More than one fabrication shape.** The existing qtype vocabulary
   (`distractor`, `provenance_trap`, `partially_present`) asked such that the
   incumbent takes the bait. One class is not a measurement.

This is the same shortfall H4 hit, on a different axis: H4 needs longform
negatives, H2 needs `absent_adjacent` fabrications. **The bank order owns both.**

### B. The hypothesis (fixes blocker 2, and must come first)

A bank fix alone will not produce a verdict. Before spending a bank order on H2,
settle whether the sampling distribution has anything to say — cheaply, with the
instrument this order built:

1. **Re-run the smoke on the harvest model** (`FINAL-Bench_Darwin-36B-Opus-Q6_K`)
   when the host can seat it. One command, five turns. If the 36B's draws are
   also 1/5 distinct at T<=1.0, H2 is dead on this unit regardless of any bank,
   and the initiative should say so and move on to H4 and H1.
   > **ANSWERED 2026-08-09, in the stated direction.** The 36B drew 1/5
   > distinct on every value-asserting probe at T=0.7, repro proven; a T=5.0
   > discriminator drew 5/5 distinct on all probes, clearing the sampler and
   > the k-fanout of blame. H2 is dead on this unit at production scale.
   > Artifacts and the full closure: `../h2b/FINDINGS.md` §4,
   > `../h2b/h2_sampler_smoke.json`, `../h2b/h2_sampler_smoke.t5_p1_k0.json`.
2. **Sample the unit that can actually diverge.** Draw on turns whose evidence
   does *not* contain the value — the fabrication case. A model forced to invent
   may well invent differently each time even where it copies identically. The
   two absent probes drawn here declined unanimously, which is the incumbent
   being *good*; the interesting draw is on a turn where it does not decline.
3. **Consider prose over values.** The <=24-token extractive value is §5 H2's
   own specification and it is close to a lookup. Farquhar et al. cluster
   free-form answers. If the value unit has no variance, the spec's unit is the
   thing to question.

**Recommendation:** B1 and B2 are hours of work against the instrument already
committed. Do them before funding A. An H2 bank order is not yet justified.

---

## Artifacts

| file | what |
|---|---|
| `h2_verdict.json` | the gate's refusal, the full census, the naive ceilings, the cost |
| `h2_cluster_floor.curve.json` | the clustering floor's operating curve + provenance + caveat |
| `h2_pair_scores.jsonl` | 341 scored value pairs — replays the curve with no model |
| `h2_sampler_smoke.json` | deliverable 3's draw: 5 turns, k=5, seeds and raw samples |
| `sampler_sweep/` | the temperature sweep, T in {1.0, 1.5, 2.0, 3.0, 5.0} |

Reproduce the verdict (no model, seconds):

```
svrn bench flywheel h2-gate \
  --calibrate sovereign/bench/chaos_monkey/results/saltgrass_compound_gv_shadow_20260808b.jsonl \
  --holdout sovereign/bench/chaos_monkey/results/saltgrass_gv_shadow_20260808b.jsonl \
  --holdout sovereign/bench/chaos_monkey/results/secret_agent_gv_shadow_20260807.jsonl \
  --smoke sovereign/bench/calibration/h2/h2_sampler_smoke.json \
  --out-dir sovereign/bench/calibration/h2
```

Reproduce the floor from frozen scores (no model, seconds):

```
svrn bench flywheel h2-calibrate \
  --source sovereign/bench/chaos_monkey/results/saltgrass_gv_shadow_20260808b.jsonl \
  ... (six --source args, see the deliverable-2 commit) \
  --from-scores sovereign/bench/calibration/h2/h2_pair_scores.jsonl
```

Reproduce the draw (needs a generator GGUF, ~30 s for 5 turns):

```
svrn bench flywheel h2-smoke \
  --transcript sovereign/bench/chaos_monkey/results/saltgrass_gv_shadow_20260808b.transcripts.jsonl \
  --model <generator.gguf> --k 5 \
  --probe present-victim --probe present-inn --probe present-weapon \
  --probe absent-cargo-manifest --probe absent-hetch-firstname
```

Run provenance: 2026-08-08, BeefyMac (macOS, 64 GB, Apple M2 Max). Reranker
Qwen3-Reranker-0.6B-Q8_0; generator **Qwen3.5-4B-Q6_K (a named substitution for
the 36B harvest model — see §2)**. All chaos transcripts read-only; no probe
generated, no bank run, no judge or Critic re-invoked.
