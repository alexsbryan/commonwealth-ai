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

---

# Addendum — the elicitation probe (P1)

Registered 2026-08-25, after A0 was scored and **before any forced-guess item
was asked**. A0's numbers are in note `1fc325b3`; nothing below was fitted to
the data it produced.

## Why the original gate does not settle the objective

G1 asked whether a gate over A0's answer set could reach OI_taxed 10. It cannot:
oracle `110A - 10` = **2.65** at A = 11.5%. That verdict stands and Phase 2
stays closed.

**But `110A - 10` assumes the answer set is fixed.** It prices a harness that
may only *withhold*. A harness that changes what the model *attempts* moves A
itself, and the pool it would move is large: 383 abstentions at +1.1 each is
**+70.2 OI points** of theoretical headroom — 26x the entire oracle ceiling G1
measured. The objective was never "can a gate reach 10", it is "how far can a
harness lift this model", and that question is open.

## The one number that decides the route

`q_forced` — precision on the 383 items A0 declined, re-asked under
`FORCED_ANSWER_PROMPT` (abstention removed), greedy, same judge, same seed.
Diagnostic arm, outside AA's protocol, never a leaderboard row.

With a perfect selector over both pools, `OI_oracle = 2.65 + 383*q*1.1/6`:

| q_forced | OI_oracle | verdict |
|---|---|---|
| 0.05 | 6.2 | target unreachable even perfectly |
| 0.08 | 8.3 | still short |
| **0.105** | **10.0** | target becomes reachable in principle |
| 0.15 | 13.2 | reachable with margin |
| 0.30 | 23.7 | large |

## Bars

- **Elicitation live** — `q_forced >= 0.15`. Build the selector (k-sample
  self-consistency over the pool). The knowledge is in the weights and the
  harness problem is extraction.
- **Elicitation dead** — `q_forced < 0.08`. Perfect selection tops out at 8.3,
  and no real selector captures full oracle value. The knowledge is absent;
  the only route is retrieval, which leaves AA's protocol and is its own row.
- **Ambiguous** — `0.08 <= q_forced < 0.15`. Decide on the stratum and domain
  breakdown below, not on the headline.

## Registered predictions

1. **Blanket conversion is EV-negative and this bench will say so.** Converting
   abstentions without a selector pays `383*(2q - 0.9)`, positive only above
   `q = 0.45`. Predicted `q_forced` is far below that, so "just make it answer
   more" is refused by arithmetic, not by taste. At q = 0.20 blanket conversion
   costs **-31.9 OI**.
2. **The abstention text is a free selector feature.** `q_forced` on the 260
   *specific* abstentions ("I do not know the exact percentage from that
   February 1982 Gallup poll") will exceed `q_forced` on the 123 *generic* ones
   ("I do not know the answer.", median 7 completion tokens vs 31). Basis: a
   specific decline shows the model parsed the question and localised its gap.
   If this holds, a first selector needs no logprobs plumbing at all — which is
   the §19 argument against starting Phase 2(a).
3. `q_forced` lands **below A0's 31.8% precision on attempted**. The model's own
   decision to decline carries real signal; if it does not, that is itself the
   finding.

## Reported regardless

The forced arm's own NOT_ATTEMPTED rate — the model may decline even when told
not to. That is not a failure of the arm, it measures how deeply the
conservatism is baked in, and it is reported next to `q_forced` always.

---

# Addendum — retrieval coverage (R1)

Registered 2026-08-26, **before any coverage judgment was run** and before any
grounded arm exists. The two closed-book verdicts are in notes `1fc325b3` (G1)
and `5e52b17b` (P1); nothing below is fitted to them.

## Why measure `r` before running the arms

The grounded arm costs ~3.8 h per 60 items and the gate-on/gate-off pair costs
7.5 h. Its ceiling is set by one quantity we have not measured — **`r`, the
fraction of the bank whose answer is actually present in what retrieval
returns.** A full-system arm that retrieves at coverage `r`, answers at
precision `p` on what it retrieves, and honestly abstains on the rest scores

    OI_taxed = 100*( r*(2p - 0.9) - 0.1 )

so the target is reachable only where `p >= (0.2/r + 0.9)/2`:

| r | p needed for OI 10 | OI at p=0.8 | OI at p=0.9 | OI at p=1.0 |
|---|---|---|---|---|
| 0.10 | unreachable | -3.0 | -1.0 | +1.0 |
| 0.15 | unreachable | +0.5 | +3.5 | +6.5 |
| **0.182** | **1.00** | +2.7 | +6.4 | **+10.0** |
| 0.20 | 0.95 | +4.0 | +8.0 | +12.0 |
| 0.30 | 0.78 | +11.0 | +17.0 | +23.0 |
| 0.40 | 0.70 | +18.0 | +26.0 | +34.0 |

**`r = 0.182` is a hard floor, not a threshold to tune.** Below it a literally
perfect answerer — right on every retrieved item, honestly silent on every
other — still misses 10. That is the same shape as G1: a ceiling argument that
costs one 30-minute measurement instead of a 7.5-hour run.

## The instrument, and why the obvious one is disqualified

Retrieval only, no chat slot: `chat inspect --format json` (documented to hit
`/v1/embeddings` and nothing else, so it survives a contended host). Then each
item's retrieved context is graded by the existing local judge on a single
question — does this context state the gold answer? — as a two-letter template
in the shape of `OMNISCIENCE_GRADER_TEMPLATE`, few-shot and single-letter, per
the judge lesson in note `ece0767a`.

**A string match on the gold answer is NOT the instrument and must not be used.**
Verified on the Mentuhotep/Intef item 2026-08-26: "Herakleopolis" appears 4x in
the top-10 as the Tenth Dynasty's capital while the fact asked for — the city
assigned to Intef, son of Tjefi — is absent from every retrieved chunk. The grep
scores that item covered; it is not. String presence over-counts on topical
adjacency and under-counts on paraphrase.

## Instrument validation — required before any `r` is reported (§18.4)

1. **Shuffled-context negative control.** 30 (question, gold) pairs judged
   against a DIFFERENT item's retrieved context. Bar: **>=90% NOT COVERED.**
   This is the one that catches the judge answering from its own weights
   instead of from the passage, which is the failure that would silently
   manufacture coverage.
2. **Hand-check a stratified 20** of the real judgments. Bar: **>=90% agreement
   AND false-positive rate on the COVERED class <=10%.** Asymmetric on purpose:
   a false positive inflates `r`, and that is the direction that wrongly
   green-lights a 7.5-hour run.
3. Below either bar, no `r` is reported and no arm is scheduled.

## Bars

Measured on the stratified 60 (`load_bank(60)`), unscoped retrieval — the
faithful mirror of what the answer path does.

- **GO** — `r >= 0.30`. Target 10 needs only `p >= 0.78`. Run the gate-on /
  gate-off pair at full length.
- **AMBIGUOUS** — `0.182 <= r < 0.30`. Target reachable only at `p >= 0.78`
  rising to 1.0. Run the pair anyway, but the deliverable is the abstention
  delta, not a leaderboard number. Decide on the per-domain table, not the
  headline.
- **KILL the retrieval-to-10 route** — `r < 0.182`. No precision reaches the
  target. The answer is then a different or additional corpus — tavily,
  stackexchange, openalex, crs_reports — not a longer run against this one.
  **Even on a KILL, still run one gate-on/gate-off pair**: the abstention
  benefit is the product claim and it is cheap relative to what it proves.

## Registered predictions

1. `r` at k=10 lands between **0.15 and 0.40**.
2. **The sharp one — retrieval REORDERS the domains.** Closed-book, Humanities
   was the worst stratum in the P1 probe (q_forced 0.000, 0 of 69). It will NOT
   be the worst here, because that failure was parametric recall and retrieval
   is precisely the lever that addresses it. Finance and Law will be worst:
   their answers are ASC and regulatory section identifiers that live in
   paywalled standards, not in wikipedia. If the closed-book ordering survives
   retrieval unchanged, the corpus is not the binding constraint and this
   whole line needs rethinking.
3. **gate ON - gate OFF >= +15 OI points** on the same 60. The sign is not in
   doubt. Worst-case arithmetic is far larger: if the gate-off arm converts
   every unretrieved item into a caveated parametric guess at the measured
   q=0.0757 (invariant `0ee9fc42`), each such item pays `2q-1 = -0.849`, and at
   r=0.30 the two arms differ by **~52 points**. The floor is set at 15 because
   gate-off may simply decline on some unretrieved items rather than confabulate.

## Confounds, named

| Confound | Effect | Handling |
|---|---|---|
| `--limit` is DISPLAY depth, not context-assembly depth | `r` overstates what the answer path was actually handed | report at k=5 and k=10; **k=5 is operative**, k=10 is the ceiling. The mapping to assembled context is UNVERIFIED and is stated as such |
| Entailment != usable | context may state the fact in a form the answerer cannot extract | `r` is an upper bound on achievable `r*p`, never a prediction of it |
| The 60 is harder than the bank | naked acc 8.3% vs 11.5% full-600 | grounded numbers off it read pessimistic; compare to the 60-item anchor (oi_taxed -23.33), never to -18.55 |
| Self-judging | same weights answer and grade | `judge_is_official: false`, as every other arm |

## Reported regardless

Per-domain `r`, and **the count of items where the gold string appears in the
retrieved context but the judge rules NOT COVERED** — that number is the size of
the trap the grep instrument falls into, and it is worth publishing whichever
way the headline goes.

## Amendments to R1 — registered 2026-08-26, after an n=6 instrument smoke, before any real `r`

Both amendments came out of a 6-item smoke run whose only purpose was to
exercise the instrument. No coverage number from that smoke is reported and
none is fitted to below. Both changes make the bar HARDER, which is the only
direction an amendment may move after a pre-registration is written.

**A1 — the validation was one-sided, and a positive control is now required.**
The shuffled-context control catches a judge answering from its own weights
(a false COVERED). It cannot catch the opposite failure: a judge that always
answers B passes it at 100% and reports `r = 0.0`, which is
indistinguishable from a corpus that genuinely holds nothing. The smoke
returned exactly that shape — control 3/3 NOT COVERED, r = 0.0 on all six —
and it was not possible to tell the two apart from the summary alone. Added:
a **positive control** that plants the fact mid-passage in the item's own
retrieved context and requires the judge to find it, **bar >=0.90 COVERED**,
placed mid-passage rather than at the front so recency cannot carry it.
`summarize` now refuses to emit `r` unless BOTH controls pass.

**A2 — the operative cut is one corpus, because the unscoped cut is a host
artifact.** R1 registered unscoped retrieval as primary, on the reasoning that
it mirrors the answer path. The smoke shows what that actually retrieves on
this machine: for the Law item on OSHA's cadmium rulemaking, the top-10 chunks
are **Rust unit tests from this repository** (`score_wrong_specialization_returns_none`,
`requirements_default_is_local_only`). This host carries ~40 installed corpora,
most of them dev fixtures and code indexes that no deployment would have. An
`r` measured across them prices this laptop, not the product.

The operative number is therefore **`--scope wikipedia`**, and the registered
unscoped number is still computed and published beside it as
`r_registered_unscoped_k10`. This is a declared substitution under §18.3, not
a quiet swap: both appear in every summary, and a reader can see the gap.

The same smoke also re-condemns the grep instrument from a second direction.
Both of its two `gold_string_present` hits were noise — gold `6` matching a
digit inside Rust source, and gold `;` matching semicolons. A single-character
gold answer matches essentially any passage. The judged verdict on both was
NOT COVERED, correctly.

