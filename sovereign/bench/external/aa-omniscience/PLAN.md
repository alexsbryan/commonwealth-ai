<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Plan — get Qwen3.8-27B to OI_taxed >= 10

Target set 2026-08-21. Companion to `PREREG.md` (which registers the honest-
measurement contract); this file is the route and its exit gates.

**The target is ambitious and should be stated as such.** OI_taxed 10 is more
than six times Claude 4.1 Opus's published 4.8 re-scored under our tax (1.52),
on a bench where all but three models score below zero. The reason it is
nonetheless plausible is arithmetic, not optimism — see the headroom argument.

## The metric, solved for what we need

With `n = 600`, `tax = 0.1`, `a = 600 - c - i` (partials ignored; they occupy
the denominator with no credit and no tax, so they are a small favourable term):

    OI_taxed * 6  =  1.1c - 0.9i - 60          =>   OI_taxed >= 10  <=>  1.1c - 0.9i >= 120

Three consequences, all decision-relevant:

**1. The oracle ceiling is a straight line in raw accuracy.** A perfect gate
answers everything it would get right and abstains on the rest (`i = 0`):

    OI_oracle = 110 * A - 10                   (A = naked accuracy)

| naked accuracy A | oracle ceiling | headroom we must capture to hit 10 |
|---|---|---|
| 18.2% | **10.0** | 100% — the reachability floor |
| 20% | 12.0 | 83% |
| 25% | 17.5 | 57% |
| 30% | 23.0 | 43% |
| 36% (Opus's) | 29.6 | 34% |

**If A0's accuracy is below 18.2%, no abstention policy of any kind reaches 10**
and the route must add knowledge, not calibration. That single number is what
Phase 0 buys, and it is why Phase 0 comes before any harness work.

**2. The operating point.** Answering `N` questions at mean precision `q`
contributes `N*(2q - 0.9)`; we need 120.

| precision q | questions answered | resulting accuracy | hallucination rate |
|---|---|---|---|
| 0.90 | 133 | 20.0% | 2.7% |
| 0.80 | 171 | 22.8% | 7.3% |
| 0.70 | 240 | 28.0% | 16.7% |

Opus 4.1 for contrast: ~67% of the bank answered at 54% precision, 46%
hallucination rate. **We are not trying to out-know it. We are trying to answer
a third as often and be right far more of the time.**

**3. The thesis, quantified.** Opus 4.1 scores 4.8 official against its own
oracle ceiling of 36.0 at 36% accuracy — it captures **13%** of the calibration
value available to it. The leaderboard is not knowledge-limited, it is
calibration-limited, and calibration is a harness problem. That is the whole
bet, and it is the same bet `SITUATED_HARNESS_STUDY.md` already won on grounded
competence (naked 4B 0.21 / naked 35B 0.42 -> both 0.67 harnessed).

## Route

### Phase 0 — measure, and validate the instrument (~4.4 h machine, ~1 h human)
Full 600-item A0 as a launchd one-shot; hand-grade a stratified 60 per
`PREREG.md`. Outputs: `A`, `oi_official`, `oi_taxed`, hallucination rate, and a
judge fit verdict.
**Gate G0** — judge agreement >= 90% overall, no class below 80% recall.
Below that, nothing downstream is readable. Fix the judge first.

### Phase 1 — the ceiling, free (0 model calls)
Re-score A0's existing rows under the oracle gate: `110A - 10`. Pure arithmetic
on data we already have.
**Gate G1** — oracle < 10 means calibration alone cannot reach the target.
Do not build the gate; branch to Phase 4. This gate exists so we cannot spend a
week on a ceiling we could have computed in a second.

### Phase 2 — find a confidence signal (cost depends on which)
Ranked cheapest-first. The signal is the whole game: everything after this is
threshold arithmetic.

- **(a) Token logprobs — preferred.** llama.cpp already implements `logprobs` /
  `top_logprobs` in full (`research/verifier-v0/data/llama.cpp/tools/server/`);
  the daemon simply does not forward the field — verified 2026-08-21, a request
  with `logprobs: true` returns `logprobs: null`. So this is **plumbing an
  existing capability through the Rust inference path**, not building an
  estimator. Marginal cost per question afterwards: zero. Reusable by every
  other gate in the system, which is the §19 argument for doing it this way.
- **(b) k-sample self-consistency — no code change.** k=5 at temp 0.7 over 600
  = 3,000 calls, ~10 h. Run a 200-item cut first (~3.3 h) to get the curve
  before paying for the rest.

Measure the **signal -> correctness curve** and report its AUC.
**Gate G2** — AUC < 0.65 kills the signal, not the threshold. Note `7572a65e`
paid for this lesson already: *"THIS IS A CURVE PROBLEM, NOT AN OPERATING-POINT
PROBLEM ... moving the threshold is already exhausted and bought +3.63 BAcc."*
Do not tune an operating point on a curve that cannot discriminate.

### Phase 3 — pick the operating point honestly (0 extra model calls)
Threshold selection must be held out or the number is in-sample fiction. Use
**leave-one-domain-out**, 6 folds — the bank is already stratified 100/domain,
and it mirrors the leave-one-subset-out discipline verifier-v0 used. Report
held-out `oi_taxed`, and report it next to accuracy, always.
**Gate G3** — held-out delta vs A0 >= +5.0 with accuracy no worse than
A0 - 3pp (the `PREREG.md` bar). |delta| < 2.0 is the registered null.

### Phase 4 — add knowledge, only if Phase 1 demands it
Honest pricing up front: the gold answers are ASC citations, Carnegie stages,
case-law counties, NumPy patch versions. **Wikipedia covers the Humanities and
parts of SEM and very little of Finance, Law, or Software Engineering.** The
realistic option is search at answer time, which leaves AA's protocol entirely
and is reported as its own clearly-labelled row.
**Gate G4** — cost per index point. A 40 GB install and 20 h of ingest for +3
points is a bad trade and gets said out loud rather than absorbed.

### Phase 5 — the mechanism check (~30 min, worth doing regardless)
Run 60 items through the *existing* grounding pipeline and count NOT_ATTEMPTED
against A0's. Tests the prediction registered in `PREREG.md` from invariant
`0ee9fc42`: the abstain path re-synthesizes from parametric memory with a
caveat prefix, which AA grades INCORRECT (-1) while the chaos honesty
classifier grades it honest. Either it confirms a defect no local instrument
can currently see, or it retires the worry. Both are worth 30 minutes.

## Order of spend

Phases 0, 1 and 5 together are one overnight run plus an hour of hand-grading,
and they decide whether Phase 2 is worth starting. Nothing in Phase 2 should
begin before G1 has a number.
