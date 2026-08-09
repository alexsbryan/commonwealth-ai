# H1 calibration — what was ported, and the one thing that surprised us

**Ported bit-identically** from `skunkworks/native-grounding`
(`sovereign/bench/calibration/h1/`), verified by sha256 at copy time and
re-asserted by the fitter on every run:

| file | sha256 |
|---|---|
| `h1_scores.jsonl` | `594adad6f8e4a1098991f72d2f6637f72cc1fcfd3ef962a80cdd4e163d099963` |
| `h1_verdict.json` | `5b22c1ba92ba34b3a2c1d6f17e93364de50c2cb6484f7eea8cc1d2079c554d41` |
| `h1_rerank_margin.overall.curve.json` | `eb1b00657d8571e96d32cd323e8b608544a8a9cabddd13cdff676564ca2116ac` |
| `FINDINGS.md` | `e05da2351463e173ab1f9af6d7a96e6940d13ae1d606250af1695111fe9431cd` |

`h1_top_cosine.overall.curve.json` came along as the comparator the
verdict is stated against. The per-family curves did not: FINDINGS is
explicit that the 19-pair literary row "is not a measurement and must not
be quoted as one", and the runtime has no per-family behaviour to
calibrate.

## What the kill gate did NOT hand the runtime

It settled the SIGNAL — margin beats cosine by +0.0995 AUROC, kill bar
cleared in 1,000 of 1,000 resamples. It did not settle an operating
point, and it did not give `GroundingVerdict.answerability` (declared
0..1) a way to exist: the reranker emits a model-dependent logit in
roughly [-10, +10].

`fit_admission_calibration.py` derives both, once, and
`h1_admission_calibration.json` is its only output and the runtime's only
source (`admission.rs` reads it via `include_str!`). There is no second
Platt fit and no second threshold anywhere in the workspace.

- **Platt:** `answerability = sigmoid(0.852843642835961 * margin - 5.6450050886280065)`,
  fitted by fixed-schedule full-batch gradient descent — no RNG, no
  shuffling, no early stop. Verified deterministic: two runs produced
  byte-identical output.
- **`tau_abstain` = 0.34849** (margin 5.885392) — the 5% false-alarm
  point FINDINGS names as "the operating point a production router would
  actually want". Below it: `Abstain`. This is the conservative
  threshold because it is the one D5's competence-when-present bar rests
  on.
- **`tau_answer` = 0.51315** (margin 6.680749) — the frozen curve's own
  best-balanced-accuracy threshold. At or above: `Answer`. Between the
  two: `Hedge`.

Region occupancy on the calibration set: `Abstain` catches 1,298 of
1,952 absent pairs and costs 113 of 2,255 answerable ones; the `Hedge`
band is 328 pairs (7.8%).

## The surprise: the score file is a lossy record of the curve's inputs

The runtime test `the_committed_operating_point_reproduces_the_frozen_honesty_recall`
failed on first run, reporting false alarm 0.050111 where FINDINGS
reports 0.049667. It was allowed to fail, and it was worth diagnosing
rather than loosening.

**It is not a threshold disagreement.** `h1_scores.jsonl` records each
margin rounded to six decimals (`"rerank_margin":6.56612`), while the
curve's thresholds are full-precision f32 widened to f64
(`5.885392189025879`). Exactly one answerable pair sits inside that
~2e-7 gap, so replaying the score FILE puts it on the abstain side while
the curve — built from the unrounded f32s — put it on the answer side.
1 / 2,255 = 0.000444, which is precisely the observed difference.

Recomputing all 4,200 curve points against the score file: 965
honesty-recall and 1,100 false-alarm points differ, every one of them by
this same rounding, and none by a threshold error. Independently, the
best-balanced-accuracy point recomputed from the scores lands at
0.823581930864018 — the curve's value to 15 digits.

**Consequences, stated rather than buried:**

1. The committed thresholds are correct and are used as-is. No
   re-derivation was done, and none is warranted for a 2e-7 boundary.
2. The runtime test asserts honesty-recall exactly (no pair straddles it)
   and false alarm to within one pair, with the 5% budget as a separate
   hard bound. Loosening it further would stop it being a gate.
3. The frozen score file cannot reproduce the verdict to the last pair.
   If a future re-fit needs that, it needs the unrounded scores, which
   were not committed — that is a note for whoever recalibrates, not a
   defect in this port.
