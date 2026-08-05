# Threshold calibration — arm A, LLM-AggreFact 11-subset card

**2026-08-04 · no GPU · `scripts/calibrate_threshold.py` · evidence `THRESHOLD_CALIBRATION.json`**

## Bottom line

Calibrating the verdict threshold is worth **+3.63 macro BAcc, free** — 65.02 → 68.65,
held out. Not the ~6 points two prior frames carried; that estimate was inflated
roughly 2× and the reasons are mechanical, below.

**The product answer is worse than the leaderboard answer, and it is the one that
matters.** At a 10% false-alarm budget — the loosest a user-facing gate can be
before "it flags everything" becomes the operator's experience — arm A catches
**41.3%** of hallucinations. At 5%, **29.6%**. No threshold fixes this: it is the
shape of the curve (AUC 0.785), not the choice of point on it. Arm A is a
leaderboard checkpoint, not a shippable gate.

## The decided value

```
predict GROUNDED when p_grounded >= 0.00362465
```

Chosen as the **median of the 11 leave-one-subset-out thetas**. Every fold's theta
is a legitimate estimate fitted without one domain; the median is the one least
moved by any single domain, and unlike the in-sample argmax it was never fitted on
all 11 subsets at once.

Wired in as `eval_grounding.py --decision-threshold P` (requires `--logprobs`).
It is a **third column** (`pred_threshold`, `macro_avg_bacc_threshold`) beside the
emitted-token lanes, never a replacement: every committed baseline stays comparable
and the substitution is never silent (§18.3). Default off.

## Where the "~6 points" went

The carried estimate came from `operating_curve.py`, which reports a **pooled**
best BAcc of 71.83 against a shipped 64.95. Two corrections stand between that and
a reportable number:

| | macro BAcc | what it is |
|---|---|---|
| pooled best (the carried figure) | 71.83 | wrong metric — the card averages 11 subsets, it does not pool 2,186 items |
| macro best, in-sample | 70.53 | right metric, but chosen on the items it is scored on — **not reportable** |
| **macro, leave-one-subset-out** | **68.65** | the honest number |
| shipped (emitted token) | 65.02 | the argmax ORPO happened to leave |

Optimism of the in-sample maximum: **+1.88**. Metric mismatch: **+1.30**. Together
they account for the whole gap between 6.9 claimed and 3.63 real.

A stratified half-split (200 repeats, same 11 domains on both sides) held out at
**69.71 ± 0.89**. That it lands *above* the LOSO 68.65 is the finding, not noise:
the ~1 point between them is the cost of meeting an **unseen domain**, which is
what deployment actually does. Item-level overfitting is small; domain transfer is
where the loss is.

## Per-fold transfer — one global threshold is a compromise, and it shows

| held-out subset | theta fitted elsewhere | BAcc | tpr | tnr | vs shipped |
|---|---|---|---|---|---|
| AggreFact-CNN | 0.000385 | 56.32 | 95.1 | 17.5 | **−6.75** |
| AggreFact-XSum | 0.003625 | 63.00 | 36.0 | 90.0 | +5.00 |
| ClaimVerify | 0.000411 | 72.92 | 90.6 | 55.2 | +1.77 |
| ExpertQA | 0.003625 | 55.50 | 34.0 | 77.0 | +0.00 |
| FactCheck-GPT | 0.008808 | 70.00 | 43.0 | 97.0 | **+11.50** |
| Lfqa | 0.008808 | 77.50 | 68.0 | 87.0 | **+11.50** |
| RAGTruth | 0.000411 | 71.00 | 79.0 | 63.0 | −0.47 |
| Reveal | 0.008808 | 83.00 | 76.0 | 90.0 | +4.50 |
| TofuEval-MediaS | 0.003625 | 70.00 | 71.0 | 69.0 | +0.50 |
| TofuEval-MeetB | 0.000411 | 74.00 | 92.0 | 56.0 | +7.00 |
| Wice | 0.007163 | 61.93 | 33.3 | 90.5 | +5.43 |

Fitted theta ranges **0.000385 → 0.008808**, a 23× spread. AggreFact-CNN is the
warning: a threshold fitted on the other ten domains costs it 6.75 points. The
+3.63 headline is a mean over exactly this variance and already pays for it — do
not quote the per-subset wins without it.

## Product operating points — the metric the operator named

Hallucination recall (`tnr_hallucinated`) at a bounded false-alarm rate, with the
same LOSO discipline applied to the threshold selection:

| false-alarm budget | theta | macro tnr | held-out tnr | macro tpr | BAcc |
|---|---|---|---|---|---|
| 5% | 4.65e-06 | 29.07 | **29.61** | 95.08 | 62.08 |
| 10% | 2.05e-05 | 41.33 | **41.31** | 90.07 | 65.70 |
| 20% | 1.18e-04 | 59.13 | **59.95** | 80.08 | 69.61 |
| 30% | 1.14e-03 | 70.47 | **70.00** | 70.03 | 70.25 |

In-sample and held-out agree to within a point at every budget, so unlike the BAcc
number this table needs no discount. To catch even 60% of hallucinations, arm A must
flag one supported claim in five.

The shipped operating point is the mirror image: macro tnr **90.9** at macro tpr
**39.1** — it catches nearly everything by calling nearly everything hallucinated,
and flags 61% of good content. Neither end of this curve is a product.

## Transfer to FaithBench — nothing here was used to fit theta

| | macro BAcc |
|---|---|
| shipped | 51.69 |
| at the decided theta | **54.83** (tpr 65.9, tnr 43.7) |
| FaithBench's own in-sample ceiling | 56.06 |

+3.14 for free on a dataset no fold ever saw — consistent with the +3.63 on the card,
which is the strongest available evidence that the decided value is a real
calibration and not a fit to LLM-AggreFact's quirks. It does not rescue FaithBench:
**56.06 is the most any threshold could reach**, so the near-chance FaithBench
result is a discrimination failure, not a calibration one. That closes the open
question from the FaithBench note — recalibration was the cheap hypothesis and it
is now falsified.

## What this changes

- **M3 target.** +3.63 is real and costs nothing, but it does not move the
  ≥75.7 bar. It is a floor adjustment, not a route to the objective.
- **The gate question is now answered with a number.** "Can arm A gate answers?"
  → not at any threshold; 41% recall at a 10% false-alarm budget. The next
  checkpoint needs to move **AUC**, not its operating point.
- **Report hallucination recall at a fixed budget alongside BAcc from here on.**
  BAcc hid this completely: the 30%-budget row and the best-BAcc point are the
  same place, so a BAcc-chasing process would have selected an operating point
  that flags 30% of supported claims and called it the optimum.

## Verification

- `scripts/test_calibrate_threshold.py` — 9 test groups, exit 0. The load-bearing
  one builds a subset whose inclusion visibly moves the fitted theta and asserts
  LOSO does not leak it; a leaking implementation reports ~100 where the correct
  answer is 50.
- The shipped-baseline recomputation is anchored against the run's own
  `summary.json` (65.02 vs reported 64.95 — averaging-then-rounding vs
  rounding-then-averaging).
- The lane wired into `eval_grounding.py` was cross-checked against
  `calibrate_threshold.py` on all 2,186 rows at the decided theta: **70.48 vs
  70.53**, the same rounding path. Two implementations of one threshold had to
  agree (§10.6) and do.
- Threshold grid is every distinct observed score (1,850 candidates), so the
  optimum is exhaustive rather than sampled. Half-split is seeded and asserted
  deterministic.
