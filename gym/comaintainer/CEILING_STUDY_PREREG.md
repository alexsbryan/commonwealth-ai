# Pre-registration — the reliability ceiling for the comaintainer verdict task

Written 2026-08-18, BEFORE any rating is collected. Fixed at commit 3d7c8daa.
Nothing below may be edited after the first rating lands; changes append.

## Why this study exists

Every charter version (v1 -> v8) has been iterated against **raw percent
agreement** on a 6-class nominal rating task. For that problem class the
established standard is chance-corrected agreement (Cohen's kappa /
Krippendorff's alpha) interpreted against a **measured** reliability ceiling.
Neither has ever been computed here. Recomputed from existing runs 2026-08-18:

| rater | raw agreement | Cohen's kappa | 95% bootstrap CI |
|---|---|---|---|
| frontier vs gold | 60.9% | 0.524 | [0.349, 0.686] |
| local vs gold | 47.8% | 0.380 | [0.202, 0.551] |

Conventional bands: >=0.80 reliable, >=0.667 tentative conclusions only.
Both engines fall short. **But a rater cannot exceed the reliability of the
labels it is scored against, and that ceiling is unmeasured** — so we do not
currently know whether 0.524 is a poor model or a well-performing model on an
irreducibly ambiguous task. This study measures the ceiling.

## Design

- **Items:** the 46 tier-A, non-situated episodes on which the frontier and
  local runs are PAIRED (both engines answered the same items under charter
  sha 8e4d0e52). Fixed set; no additions.
- **Rater:** the operator — the author of most of the recorded case law these
  labels came from.
- **Therefore this is a TEST-RETEST (intra-rater) reliability measurement,
  not inter-rater.** Named honestly: it measures whether the house agrees with
  its own recorded verdicts when re-judging blind, months later. That is a
  legitimate and in fact *generous* upper bound — an independent second rater
  would be expected to score lower, so the ceiling this yields is optimistic.
- **Blinding:** the rater sees `request.situation`, `request.proposal`,
  `request.evidence` and nothing else — the same block a candidate model sees
  (gym README taxonomy). The rating page does not contain the gold labels,
  the model answers, `expect.rationale`, or `provenance` in any form. Verified
  structurally: the answer key is a separate file the page never loads.
- **Leak check:** all 46 items passed `markers.lint_leaks`, the bank's own
  five-class leak linter, run over exactly the text the rater will see. 0 hits.
- **Order:** randomized once, seed 20260818, recorded with the data.
- **Response:** one of the six verdicts (closed set, `markers.VERDICTS`),
  plus a 3-level confidence, plus an optional `unjudgeable` flag meaning
  "under-specified as written." Per-item elapsed time recorded automatically.
- **could-not-judge is a legal answer** and is not treated as a non-response.

## Primary outcome

Cohen's kappa, operator vs gold, over all 46 items, with a 4,000-sample
bootstrap CI (same estimator already applied to both engines, seed recorded).

## Secondary outcomes

1. operator vs frontier, and operator vs local kappa — the three-way picture.
2. Per-class recall for the operator: which verdict classes are irreducibly
   ambiguous rather than merely hard for a model.
3. **Label-precision estimate.** Items where the operator disagrees with the
   recorded verdict are candidate label errors. The bank's label precision was
   measured at 86% on the PRE-FIX bank and has never been measured since.
4. Median seconds per item — the cost of the task, and the basis for any claim
   that a model saves the operator time.
5. Confidence vs correctness — whether the rater is calibrated.

## Interpretation rules — fixed in advance

- Bands are Krippendorff's: **>=0.80 reliable · 0.667-0.80 tentative
  conclusions only · <0.667 not reliable.** These are not negotiable after
  seeing the number.
- **The ceiling caps every model target.** No charter version may declare a
  kappa target above the measured operator-vs-gold kappa. A model asked to
  exceed the ceiling is being asked to agree with wrong labels.
- n=46 yields a CI roughly +/-0.17 wide (observed on the engine runs). That is
  enough to separate 0.38 from 0.80. It is NOT enough to separate 0.52 from
  0.60, and no such claim will be made.

## Decision rules — the stakes, fixed in advance

- **If operator-vs-gold kappa >= 0.80:** the task is reliably specifiable. The
  gap to the models is real and charter iteration is the right lever. Restate
  OW1/OW2 in kappa against this ceiling.
- **If 0.667 <= kappa < 0.80:** the task is tentatively specifiable. Model
  targets are set at the ceiling, not at 1.0, and the honest claim about any
  model is bounded accordingly.
- **If kappa < 0.667:** the 6-way nominal verdict task is NOT reliably
  specifiable, and no amount of charter prose fixes that. The program moves to
  `COMAINTAINER.md 6.6` forced-choice probe decomposition — the field's
  standard remedy for a low-agreement nominal task — and charter iteration
  against exact-6 STOPS. Charter v1->v8's measured gains would then be
  reinterpreted as movement inside the noise of an unreliable instrument.
- **If >=25% of items are flagged `unjudgeable`:** the episodes, not the
  raters, are the problem; the bank needs a specification pass before any
  further scoring.

## Threats to validity, named now

- **Memory contamination.** The operator may recall specific landings, which
  inflates agreement. Not correctable; it makes the ceiling optimistic and is
  reported as such.
- **Single rater.** No true inter-rater kappa is obtainable from this study.
- **n=46.** Fixed by the paired set. Wide intervals; stated above.
- **Order effects / fatigue.** Randomized order mitigates systematic bias;
  per-item time is recorded so a fatigue trend is visible.
- **The gold labels are not ground truth.** Tier A means "settled by a later
  instrument," which is the best available grounding, not certainty.

---

## Amendment 1 — summary-first presentation (2026-08-18, before any rating)

Appended, not edited. Operator direction: the items are unreadable at volume
(15,286 words across 46 items; p25 263 chars, p75 2,872, max 5,843), and
truncation-style collapse is not a reduction because it assumes the first
line carries the item.

**Change.** Each item is presented as ONE generated sentence. The full text
is one keypress away (`V`) and the rater may open it at will. The summary is
generated on the LOCAL daemon (dogfooding directive 63b8fa6e), at temperature
0, from situation+proposal+evidence, with an explicitly non-evaluative prompt.

**The confound this introduces, and how it is measured rather than avoided.**
Both engines rated the FULL text. A rater who reads only a summary is rating a
different stimulus. Rather than accept that silently:

- `S` logs "summary is inadequate" per item and auto-opens the full text.
- Every rating records `expanded`, `ms_to_expand`, `summary_inadequate`, and
  `had_summary`.
- New secondary outcome: kappa computed separately over items rated
  summary-only vs items opened to full text. If those two kappas differ
  materially, the summary changed the judgment and the primary number is
  reported summary-only-excluded.
- An item whose summary generation failed shows NO summary and opens full
  text by default — refused, never a blank string rendered as if adequate
  (ARCH 18.3).

**New decision rule.** If >=20% of items are marked `summary_inadequate`, the
summarization layer is not fit for the task; the primary kappa is computed on
full-text-expanded items only, and the summary experiment is reported as a
failed instrument rather than quietly retained.

**Priming guard.** Summaries are scanned for verdict vocabulary
(approve/revise/measure-first/escalate/could-not-judge) and evaluative words
(should/must/correct/wrong/risky/good/bad). Any hit is reported before rating
begins; a summary that names a verdict would prime the rater.

---

## Amendment 2 — Amendment 1 is WITHDRAWN (2026-08-18, before any rating)

Operator correction: the rater's role is to score whether a conclusion follows
from its premises. A generated sentence reading "evidenced by improved coverage"
has already MADE that inference — it grants both that evidence exists and that
it supports the claim, which pre-decides between approve and measure-first.
Amendment 1's priming guard screened evaluative ADJECTIVES and missed
evaluative RELATIONS, which for this task are the ones that matter.

**Root cause of the detour, recorded (ARCH 19): the bank's schema already
separates claim from support and no model was ever needed.** Measured:

| field | median | p75 | max | note |
|---|---|---|---|---|
| proposal | 179 | 180 | 978 | the CLAIM; 41 of 46 under 600 chars |
| evidence | 61 | 1,076 | 4,000 | the SUPPORT; **22 of 46 are `[none provided]`** |
| situation | 155 | 1,171 | 4,000 | background only — the sole long field |

The volume problem was never the claim or the evidence. It was the background.

**Presentation as built.** `proposal` verbatim, `evidence` verbatim (an empty
one rendered as a visible "nothing offered", since absence is the signal that
decides measure-first and could-not-judge), `situation` folded behind `V`.
**Zero model calls in the presentation path**, so the summarization confound
Amendment 1 introduced is removed rather than measured. `expanded` now records
whether the rater needed the background, which stays a secondary outcome.

**Cost of the detour, for the record.** Two model batches: the 4B produced 46/46
degenerate summaries (the prompt echoed back — caught only because every one was
exactly 17 words); the 27B produced 46 valid but inference-making summaries; the
third pass hit a wedged primary slot (0/6 served over 3 min while the fast slot
served 6/6) and was abandoned. All three are superseded by reading two fields
that were already separate.
