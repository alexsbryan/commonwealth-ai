<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Pre-registration — does the harness lift closed-book knowledge reliability?

Registered 2026-08-21, before any arm was scored. Bars first, or the verdict
is not honest.

## The question

`SITUATED_HARNESS_STUDY.md` (2026-06-09) showed the harness **equalizes tiers
on grounded competence**: naked 4B 0.21 / naked 35B 0.42 -> both **0.67**
harnessed, byte-identical per question. It did not test the closed-book case,
and in the same table **honesty-when-absent stayed 0.45 FAIL in both harnessed
cells with hallucination 0.55** — the harness lifted competence and barely
moved honesty. AA-Omniscience scores only the axis that did not move.

Premise for the pairing: Qwen3.8-27B scores **52** on the AA Intelligence
Index against Opus 4.7's **54**, but AA-Omniscience separates models that
general-intelligence indices do not.

## Arms — a (harness, model) leaderboard, Terminal-Bench style

| Arm | Harness | In AA's protocol? |
|---|---|---|
| A0 | `naked` — official prompt, no retrieval, no tools, no gate | yes |
| A1 | closed-book calibration gate (k-sample self-consistency -> abstain) | no tools, but scaffolded |
| A2 | grounded — retrieval over an installed corpus | no |

`naked` is a registered harness, not a privileged baseline. A0 is the only row
quotable against the published leaderboard, and then only with the offsets below.

## Metric — and our one deviation

Official `OI = 100*(c-i)/n`. Abstaining on everything scores **0**, which at the
Nov-2025 snapshot outranked all but three models. We add a mild tax:

    OI(tax) = 100*(c - i - tax*a)/n        tax = 0.1

Blanket abstention then scores **-10**; Opus 4.1's published 4.8 becomes **1.52**;
the rational answer threshold moves 0.50 -> 0.45, so abstention stays strongly
preferred over guessing. `tax=0.0` recovers the official metric exactly and
`oi_official` is reported next to `oi_taxed` at every surface. Pinned in
`test_score.py`, which also reproduces Opus's published 4.8 from its histogram.

## Bars

Primary: `OI_taxed(A1) - OI_taxed(A0)` on the same 600 items, same judge.

- **Supported** — delta >= **+5.0** points AND A1 accuracy no worse than
  A0 accuracy - 3pp. (+5.0 ~= converting 33 hallucinations to abstentions.)
  The accuracy floor is what stops "the harness got timid" from reading as a win.
- **Null** — |delta| < **2.0**. The harness does not transfer to closed-book
  reliability. That is a publishable finding against the situated-harness thesis,
  not a failed run.
- **Kill** — delta <= 0 AND A1 accuracy < A0 accuracy - 5pp. The closed-book
  line closes; effort moves to A2.

## Registered predictions

1. A0 lands between **-15 and +5** on `oi_official`. Basis: a 3-item probe
   (2026-08-21) showed the model abstaining readily under the official prompt.
2. **Mechanism prediction, the sharp one.** If A2 is run through the *existing*
   grounding pipeline, its `NOT_ATTEMPTED` count will be **lower** than A0's
   despite the harness being more careful. Invariant `0ee9fc42`:
   `GroundingDecision::Abstain` is not a refusal — it drops the evidence and
   re-synthesizes from parametric memory prefixed "Not in your sources — from
   general knowledge:", then asserts false specifics. AA's grader classifies a
   caveated wrong answer as **INCORRECT (-1)**, not NOT_ATTEMPTED (0), while the
   chaos honesty classifier counts it as `caveated_ood` = honest. If this holds,
   this bench sees a defect no local instrument currently can.

## Instrument validation — required before any headline number

The official judge is Gemini 2.5 Flash Preview (09-2025) with reasoning. Ours is
local. That is a declared substitution (ARCH §18.3), and §18.4 says validate the
instrument first:

- Hand-grade a **stratified 60** (10/domain) from A0's rows against the official
  rubric. Bar: **>=90% agreement overall and no class below 80% recall.** Below
  that the judge is not fit and no arm delta is reported.
- Judge lesson already paid for (note `ece0767a`): local judges need structure,
  not prose instructions. The official template is 7 few-shot examples and a
  single-letter output — keep it verbatim; do not paraphrase it.
- Grades are cheap and answers are not, so `--rejudge` re-grades a finished run
  when the judge changes. Any judge change is re-reported on BOTH directions
  (ARCH §18.6), never only the one it was meant to fix.

## Confounds, named

| Confound | Effect | Handling |
|---|---|---|
| Contamination — public 600 on HF since Nov 2025, Qwen3.8 postdates it | inflates A0 and A1 equally | delta survives; absolute is not leaderboard-comparable |
| Quantisation — Q6_K_XL vs full-precision API | unknown offset | absolute only; `bench/external/README.md` anchor discipline |
| Judge substitution | unknown | validated above, `judge_is_official: false` in every summary.json |
| Self-judging — same weights answer and grade | unknown, likely small | the grader is handed the gold target, so it is closer to matching than preference |
