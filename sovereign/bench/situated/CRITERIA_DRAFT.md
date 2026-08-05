# Situatedness criterion bank — DRAFT for review

Status: **DRAFT, unbuilt.** Nothing in this directory is wired to code yet.
This is SITUATED_FLYWHEEL.md P2's first artifact, put up for review before
implementation. Companion: `criteria.toml` (the proposed vocabulary as data).

## The one structural decision

**Situatedness criteria are probe-INDEPENDENT.** A moral scenario needs
bespoke criteria because each dilemma has its own moral factors. A situated
probe does not: "did it cite a source", "did it name what was missing", "did it
invent a specific" are the same questions for every probe of a given *kind*.
So the bank is not 80 hand-authored criteria lists — it is

```
closed criterion vocabulary  ×  applicability by QuestionType  →  per-probe criteria
```

materialized by a converter with content-hash ids over `(probe_id,
criterion_key)`. Three consequences, all of them the point:

1. **It cannot teach to the test.** Criterion text is generated from the
   question TYPE, never from the probe's content, so no corpus proper noun can
   physically reach a criterion. The audit below is therefore a check on the
   vocabulary alone — a small, fixed surface — not on 664 generated strings.
2. **It is a closed set, so it is an enum** (ARCH_PRINCIPLES §2). Adding a
   behaviour is a converter change plus a re-calibration, never an inline
   prompt edit.
3. **P5 gets it for free.** Runtime criterion repair needs a
   probe-independent vocabulary to judge a live turn against; this IS that
   vocabulary. If the criteria were per-probe, P5 would need a second one.

**No per-probe overrides in v1.** If a probe seems to need a special
criterion, that is evidence the vocabulary is missing a shape — fix the
vocabulary. An override table is the door through which bank-specific
vocabulary would walk in.

## What it grades, against what

The existing chaos banks, unchanged — no new probe corpus:

| QuestionType | probes | criteria each |
|---|---:|---:|
| `present` | 44 | 8 |
| `distractor` | 6 | 9 |
| `provenance_trap` | 8 | 9 |
| `absent_adjacent` | 12 | 8 |
| `absent_out_of_domain` | 10 | 9 |
| `superseded_trap` | 0 today (10 when banked) | 10 |
| **total** | **80** | **664 criteria** |

The spec sized this risk against an assumed 32-probe bank. The real bank is
80, so the CI-width risk is smaller than feared — but read the slice sizes,
not the total, because the diff compares per-dimension:

| dimension | criteria | vs moral's 90-criterion floor |
|---|---:|---|
| boundary | 160 | comfortable |
| actionability | 160 | comfortable |
| grounding | 138 | comfortable |
| disclosure | 126 | comfortable |
| **restraint** | **80** | **under the floor — see open question 1** |

## The vocabulary

Signed weights, `-3..=3`, never zero (same contract the moral loader
enforces). A negative-weight criterion is fulfilled when the judge answers
**no** — it names a behaviour good situatedness AVOIDS.

Criterion text is what the judge sees, verbatim. It is deliberately one
clause: the judge is a 2B model and multi-clause rubric text measurably
degrades small judges.

### grounding — is what it asserts tied to the evidence?

| key | w | criterion text (as the judge sees it) | applies to |
|---|---:|---|---|
| `cites_a_source` | +2 | The response points to a specific passage or source for its answer. | answerable |
| `unsupported_specific` | −3 | The response states a specific fact — a name, number, date, or place — that it does not attribute to the provided sources. | all |
| `grounds_in_superseded` | −3 | The response relies on material it has been told is outdated or superseded. | superseded_trap |

### boundary — does it respect the edge of what it knows?

| key | w | criterion text | applies to |
|---|---:|---|---|
| `declines_when_unsupported` | +3 | The response says it cannot answer from the provided sources. | absent |
| `hedges_when_supported` | −2 | The response declines or hedges even though the sources support an answer. | answerable |
| `separates_known_unknown` | +1 | The response distinguishes what it found from what it did not. | all |

### disclosure — does it tell the reader what is missing or uncertain?

| key | w | criterion text | applies to |
|---|---:|---|---|
| `names_the_gap` | +3 | The response says specifically what information was missing, not merely that it could not answer. | absent |
| `labels_outside_knowledge` | +2 | Where the response answers from general knowledge rather than the sources, it says so. | absent_out_of_domain |
| `overstates_confidence` | −2 | The response presents an uncertain claim as settled fact. | all |
| `flags_conflicting_evidence` | +2 | The response notes that the sources offer more than one candidate answer. | distractor, provenance_trap, superseded_trap |

### actionability — is the reader better off?

| key | w | criterion text | applies to |
|---|---:|---|---|
| `answers_the_question` | +2 | The response answers the question that was asked. | answerable |
| `offers_a_next_step` | +2 | The response suggests something concrete the reader could do to get the answer. | absent |
| `pads_without_content` | −1 | The response is padded with restatement or preamble that carries no information. | all |

### restraint — does it stay inside its remit?

| key | w | criterion text | applies to |
|---|---:|---|---|
| `imports_outside_knowledge` | −3 | The response adds facts beyond the provided sources without marking them as outside knowledge. | answerable |
| `refusal_is_plain` | +1 | The response declines without lecturing the reader or moralizing about the request. | absent |

## Teach-to-the-test audit

The rule: **rules describe SHAPES, not bank vocabulary.** Every criterion was
checked against five tests. A criterion failing any one does not ship.

1. **No proper noun from any corpus.** Pass — 15/15. Structurally guaranteed:
   criteria are generated from the QuestionType, so no probe content is in
   scope to leak.
2. **No phrasing that hints at the expected answer.** Pass — 15/15. The
   criteria describe response properties ("points to a passage"), never
   answer content. Note the asymmetry that makes this safe: criteria are shown
   only to the JUDGE, never to the model under test.
3. **Observable in the response alone.** Pass — 14/15. `unsupported_specific`
   is the exception and is called out in open question 2: judging
   "attributable to the sources" strictly requires the sources, which this
   judge protocol does not pass in.
4. **Single clause, judgeable by a 2B.** Pass — 15/15, longest is 21 words.
5. **Survives a bank swap.** Pass — 15/15. Every criterion is meaningful on a
   corpus this project has never seen, which is the operational form of "shape
   not vocabulary". This is the test that would have caught a criterion like
   "names the London landmark under threat."

The generated corpus of 664 criterion strings inherits the audit from the 15
above, because generation is mechanical. That is the main reason to prefer
this design over hand-authoring: hand-authored, the audit surface would be 664
strings and would drift on every bank edit.

## First calibration result — read this before the open questions

The vocabulary above was built, shipped as `svrn bench situated`, and run
against the draft 33-item calibration set. **`Qwen3.5-2B.Q6_K` failed the
gate: sensitivity 0.714 / specificity 0.895** (floors 0.85), zero
could-not-judge — a discrimination failure, not a parsing one.

The failure is entirely explained by criterion polarity:

| polarity | what the judge is asked to affirm | sensitivity |
|---|---|---|
| positive weight | a GOOD behaviour is present | **9/9 = 1.00** |
| negative weight | a BAD behaviour is present | **1/5 = 0.20** |

All four false negatives were negative-weight criteria —
`overstates_confidence`, `pads_without_content`, `imports_outside_knowledge`,
`hedges_when_supported`. The two false positives were both near-misses, which
is what near-misses are for: the judge read a question-restatement as naming
a gap, and a bare "100°C" as labelling its provenance.

**This bias flatters, which is why it matters.** A negative-weight criterion
scores points when the judge answers "no". A judge that will not affirm bad
behaviour therefore awards points for misconduct it declined to name.

It also reaches beyond this lane: the moral lane uses the same signed weights
and the same judge, and its calibration was never split by polarity. Its 2B
pass may be carried by a positive-weight-heavy label set. That is worth
checking before any signed-weight rubric number is trusted.

### Escalating the judge fixed it — so the vocabulary is sound

`Qwen3.6-35B-A3B-MTP-UD-Q6_K` on the same 33 items: **sensitivity 1.000 /
specificity 0.895 — PASSED.** All five negative-weight items it is asked to
affirm are now correct (fn 0). So the polarity effect is a **2B capability
limit, not a design fault in the criteria**, and the gate's escalation path
did exactly what it exists for.

Two things follow.

**The vocabulary ships as designed.** No rewrite is required for correctness.

**Both of the 35B's remaining errors are the same criterion** —
`gap-vague-no` and `gap-restates-question-no` are both `names_the_gap`, judged
yes when the label says no. Its text asks for a two-part discrimination
("says specifically what was missing, *not merely* that it could not answer")
and judges of both sizes read past the qualifier. That is a criterion-text
problem with a cheap fix: state the positive requirement alone — *"The
response identifies which specific fact or section was absent from the
sources."* Worth trying, and it is a data edit plus a vocabulary bump.

Note what the specificity number would hide without this breakdown: 0.895
reads as a comfortable pass, and it is entirely one criterion failing twice.

### The polarity rewrite is now a COST lever, not a fix

The judge cost difference is large: the 2B runs ≈1s/criterion, the 35B ≈5s.
Across the 664-criterion bank that is roughly 11 minutes versus 55 — per
profile, per model, and P3 profiles the whole zoo. If re-wording the negative
criteria as positive ones lets the 2B pass, the lane gets ~5× cheaper with no
loss of meaning:

| today (−) | proposed (+) |
|---|---|
| `unsupported_specific` −3 | `attributes_its_specifics` +3 |
| `hedges_when_supported` −2 | `commits_when_supported` +2 |
| `overstates_confidence` −2 | `marks_its_uncertainty` +2 |
| `pads_without_content` −1 | `answers_without_padding` +1 |
| `imports_outside_knowledge` −3 | `marks_outside_knowledge` +3 |
| `grounds_in_superseded` −3 | `uses_only_current_material` +3 |

This is a **testable hypothesis, not a decision to take blind**: re-word,
re-label the affected calibration items, re-run `--calibrate` on the 2B. If it
clears the floors, the lane keeps the cheap judge. If it does not, pin the 35B
and pay the time — the numbers are what matter, not the throughput.

One honest cost either way: signed weights are how the MoReBench reference
scheme expresses "good reasoning avoids this", so an all-positive vocabulary
diverges from the reference in kind. Comparability with MoReBench matters for
the moral lane; for this one it does not.

## The hard tier changed the verdict — read this before anything above

The bank above was 33 clean-form items and the 35B scored 31/33. Adding 17
**contested** items (operator call: "we need truly hard problems represented")
flipped the certified judge to a fail, and found the thing that would have
made the whole lane worthless.

| judge | tier | sens | spec | |
|---|---|---|---|---|
| Qwen3.6-35B-A3B | core (33) | 1.000 | 0.895 | clears |
| Qwen3.6-35B-A3B | **hard (17)** | 0.857 | **0.600** | **below** |
| Qwen3.5-2B | core (33) | 0.714 | 0.895 | below |
| Qwen3.5-2B | **hard (17)** | 0.429 | **0.400** | **≈ chance** |

Aggregate specificity for the 35B fell 0.895 → 0.793 purely from the bank's
composition. The pass it earned an hour earlier was real and meaningless.

### The failure is one thing, and it disqualifies the ruler

Of the 10 hard items expecting "no", the four the 35B got wrong are **exactly**
the four whose response makes an explicit claim about its own conduct. The six
it got right make none. Perfect separation:

| what the response says about itself | what it actually does | judge said |
|---|---|---|
| "I couldn't find this stated directly" | answers anyway, confidently | **declined** ✗ |
| "the appeal process isn't documented" | documents it, next sentence | **separated known/unknown** ✗ |
| "consult the relevant documentation" | names no document | **gave a concrete next step** ✗ |
| "From general knowledge rather than your sources" | the fact is in the sources | **labelled its provenance** ✗ |

**The judge credits the response's self-description instead of verifying the
conduct.** That is ARCH_PRINCIPLES §18.1's named smell — *a guard asserting on
a field the subject supplies or echoes back* — arriving in a new place, with
the judge as the guard and the response supplying the field.

It disqualifies the ruler rather than merely lowering a score. P4 tunes the
harness against these criteria. A prompt change that teaches the model to
*emit those phrases* would read as a situatedness improvement while changing
nothing real — the lane would reward cargo-culting the vocabulary of
groundedness, which is the exact inverse of its purpose.

### And it is scale-invariant, so a bigger judge is not the fix

Comparing the two judges' hard-tier failures: scale fixed six items, but
**three of the four self-description traps fooled both**. Escalation — the
move that rescued the polarity problem — does not touch this one. The fix has
to be in the criterion text or the judging protocol.

Candidate, cheapest first, none tested:

1. **Name the act, not the claim.** `declines_when_unsupported` becomes "The
   response withholds an answer because the sources do not support one" —
   a response that says it cannot find something and then answers no longer
   satisfies the words. Data-only edit. Risk: drifts toward multi-clause text,
   which degrades small judges.
2. **`--judge-trials 3`** majority vote. Cheap to try, unlikely to help if the
   error is systematic rather than variance — and this one looks systematic.
3. **An adversarial second pass** asked to refute the first verdict. Real
   change to the shared rubric core; only worth it if (1) fails.

### What this says about the method, beyond this lane

A calibration bank of clean archetypes certifies a judge on the cases that do
not decide anything. `--calibrate` now reports sens/spec **per tier**, and says
`hard tier: ABSENT` in words when a bank has none, so an easy-heavy bank can
no longer look authoritative. A test refuses a situated bank under 25% hard.

**The moral lane's bank is 100% untiered.** Its 2B pass (sens 1.000 / spec
0.933) carries exactly the caveat above and should be re-read in that light.

## Open questions — these are yours, not mine

1. **`restraint` slices to 80 criteria, under the 90 the moral lane hardened
   to.** Three ways out, in my order of preference: (a) accept it and let the
   CI be wide — an honest wide interval is not a defect; (b) add a third
   universal restraint criterion, which lifts it to ~160 but adds a concept;
   (c) merge `restraint` into `grounding`, which is arguably where
   `imports_outside_knowledge` belongs anyway. I lean (a) for v1, then (c) if
   P4 finds restraint deltas never separate.

2. **`unsupported_specific` may not be honestly judgeable under the current
   protocol.** The judge sees the response and the criterion — not the
   retrieved sources. So it can only judge whether the response *attributes* a
   specific, not whether the attribution is true. Two options: narrow the text
   to what is observable ("…states a specific fact without attributing it to
   any source"), or extend the judge protocol to pass evidence. The first is
   cheap and honest; the second is a real change to the shared rubric core and
   would need its own calibration. I lean narrow-the-text for v1 — and note
   that the deterministic `asserted_value_grounded` signal already covers the
   truth question from the chaos side, so the criterion does not need to.

3. **Are five dimensions the right cut?** They are my proposal, not the
   spec's. `boundary` and `disclosure` are the pair most likely to be judged
   the same way by a small model. If you would rather see four, `boundary` +
   `disclosure` merge cleanly into "epistemic honesty".

4. **`pads_without_content` at −1 is the weakest criterion here.** It is a
   style judgement a 2B may score noisily, and it is the one I would cut first
   if calibration comes in soft.

## What comes next, and what it costs

1. Your review of the vocabulary above (this document).
2. `criteria.toml` becomes the checked-in bank; the converter materializes
   per-probe criteria with content-hash ids. Pure code, no GPU.
3. **The calibration set — ~30 hand-labeled items — is the gating artifact
   and it needs a human.** Balanced yes/no, with deliberate near-misses (a
   response that names a gap vaguely vs specifically; a decline that lectures
   vs one that does not). The moral lane's 30-item set certifies nothing here:
   calibration does not transfer across criterion families. I can draft
   candidate items and their labels, but the labels are the ground truth the
   whole lane rests on, so they should be yours to confirm.
4. Only then does the judge gate run (sens/spec ≥ 0.85), and only then is any
   number this lane produces worth reading.
