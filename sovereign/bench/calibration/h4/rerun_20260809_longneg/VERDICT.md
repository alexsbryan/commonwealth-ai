# H4 — the gate's verdict, third run, first time it could judge

**Outcome: `Survives`. Exit 0. It beats the latency bar, fails the
agreement bar, and fails to out-perform a scorer that does not look at
anything.**

`svrn bench flywheel h4-gate`, code FROZEN and unchanged since
`8509e81b`. The only new inputs are tonight's longform-negative banks,
merged into this branch at `e18653b0`. The two prior runs returned
**could-not-judge** — not because the telemetry was thin but because the
held-out label set had one value in it (`8509e81b`, then `8ca3ace9`
quantifying the degeneracy: naive always-supported agreement 1.0000).
That blocker is gone. This run has a verdict.

## The three bars

| # | bar | measured | verdict |
|---|---|---|---|
| a | high-margin agreement **>= 0.90** | **0.7674** (33 / 43) | **FAILED** |
| a | kill if disagreement **> 0.25** | 0.2326 (10 / 43) | survives — by 0.0174 |
| b | audit **p50 <= 2000 ms** | **1212 ms** over 27 turns | **BEAT** |
| c | naive always-supported ceiling **0.7955** | **0.7674** | **FAILED** |

Bar (c) is the reporting obligation this order added, and it is the one
that matters most. Two readings of the naive strategy, both above the
mechanism:

- **0.7955** — the order's cited ceiling, over all 44 two-class holdout
  labels.
- **0.7907** — the like-for-like comparison, over the **high-margin pool
  the gate actually scores** (n=43, 34 supported / 9 not).

The mechanism scores **0.7674**. A scorer that answers "supported"
unconditionally, consults no evidence and loads no model, agrees with
the incumbent ladder **more often** than the sentence margin does. On
this bank the margin is not merely short of the beat bar — it is
**negative-value** as a replacement for the incumbent's per-claim call.

Metric (c)'s **`citation_fidelity` / `grounding_fidelity` deltas remain
NOT RUN**, for the third time and the same reason: computing a delta
needs an H4-derived variant of those facets, which is the chaos-scorer
cutover, explicitly out of scope here. Reported as not-run, never as
zero.

## Why it fails — the margins overlap

The verdict is not a threshold artifact. It is visible in the raw
scores, before any floor is chosen:

```
negative-class margins (n=9):  -6.73  -6.67  0.88  3.89  4.06  4.13  4.85  5.06  6.73
positive-class margins:        median 4.84,  min -7.43
```

**Seven of the nine claims the incumbent held NOT supported carry
margins from 0.88 to 6.73 — squarely inside the positive class's own
range, and five of them above the positive median.** Only two separate.
At the calibrated floor (0.8009) the confusion is:

| | incumbent supported | incumbent NOT supported |
|---|---|---|
| **mechanism supported** | 31 | 7 |
| **mechanism NOT supported** | 3 | 2 |

Negative recall **2/9**. The three false alarms are the other side of
the same overlap — there is no floor placement that fixes one without
paying for it in the other.

The calibration side says the same thing before the holdout is touched,
which is what makes this a finding rather than a bad draw:

- **AUROC 0.5876** over 49 claims (13 absent / 36 answerable). Chance is
  0.5000.
- Best balanced accuracy **0.6229**, at threshold 0.8009 — the floor,
  selected by rule (`operating_curve::build`'s best-balanced-accuracy
  point) so it carries no free parameter.
- Honesty recall at 5% false alarm: **0.2308**. At 20%: **0.3846**.

Full curve: `h4_margin.calibration.curve.json`.

## The banks — why the numbers mean something this time

| split | source | claims | supported | not supported | negative-carrying turns |
|---|---|---|---|---|---|
| calibration | `saltgrass_compound_longneg_20260808` | 49 (20 turns) | 36 | **13** | 3 |
| holdout | `saltgrass_longneg_20260808` | 44 (28 turns) | 35 | **9** | 3 |

All six negative-carrying turns are **longform**, where four prior
harvests had produced zero. Held-out exclusions: **1** claim inside the
flip band, **0** for a missing `violation_prob`, **0** unscoreable — the
high-margin restriction costs one claim here, so no thin-set caveat
applies to bar (a).

Margin distribution on the scored pool: n=43, min -7.431, p25 2.703,
median 4.440, p75 5.750, max 6.895. Holdout `violation_prob`: n=44, min
0.000, median 0.000, p75 0.010, max 0.890.

**The incumbent judge-call comparison for bar (b) is still cited, not
measured** — `~35 calls per gated longform turn` (NATIVE_GROUNDING §2,
citing `DEFAULTS_LEDGER.md:848`). The chaos `ResultRow` carries no
per-stage timing, so this gate cannot put a measured incumbent number
beside its own 1212 ms. Bar (b) is beaten against the *stated* bar; the
speedup ratio remains unmeasured.

## What this does and does not settle

**Settles.** The sentence-margin mechanism, at 0.6B rerank, does not
reproduce the incumbent ladder's per-claim supported/not-supported call
well enough to replace it — on the first bank ever assembled that can
test the question. It is fast (1212 ms p50, comfortably inside 2 s) and
it is cheap. It is not accurate on the negative class, which is the only
class a grounding gate exists to catch.

**Does not settle.** Whether the incumbent's 9 negatives are all
*correct* — seat adjudication note `e9a60bae` found 2 incumbent errors
and 5 margin-only false negatives in an earlier sample, so agreement
with the incumbent is not the same as correctness. This gate measures
agreement, by design, and the bars are written in those terms.
Hand-adjudication is the operator's and was not triggered: `Survives` is
not `Killed`, so no 20-claim sample was prepared. Disagreement landed
**0.0174 below the kill bar**, which is close enough that a differently
drawn bank could plausibly cross it.

## Reproduce

From the frozen scores, no model, seconds. The split flags are still
required — `--from-scores` replaces the *scoring*, not the split
declaration, and the gate refuses to run a single split at all:

```
svrn bench flywheel h4-gate \
  --from-scores sovereign/bench/calibration/h4/rerun_20260809_longneg/h4_claim_scores.jsonl \
  --calibrate sovereign/bench/chaos_monkey/results/saltgrass_compound_longneg_20260808.transcripts.jsonl \
  --holdout   sovereign/bench/chaos_monkey/results/saltgrass_longneg_20260808.transcripts.jsonl \
  --out-dir sovereign/bench/calibration/h4/rerun_20260809_longneg
```

Verified 2026-08-09: reproduces every number in the table above, exit 0.

The full measurement (needs the reranker GGUF, ~4 min):

```
svrn bench flywheel h4-gate \
  --calibrate sovereign/bench/chaos_monkey/results/saltgrass_compound_longneg_20260808.transcripts.jsonl \
  --holdout   sovereign/bench/chaos_monkey/results/saltgrass_longneg_20260808.transcripts.jsonl \
  --rerank-model <qwen3-reranker-0.6b-q8_0.gguf> \
  --out-dir sovereign/bench/calibration/h4/rerun_20260809_longneg
```

Host: Apple M2 Max, 64 GB; reranker Qwen3-Reranker-0.6B-Q8_0; banks
harvested 2026-08-08 by a `routed_intent`-bearing binary (provenance in
`longform_negatives_20260808.report.json`). No judge was re-invoked, no
transcript rewritten, no primary model run.

## Artifacts

| file | what |
|---|---|
| `h4_verdict.json` | the gate's own decision record, every bar and distribution |
| `h4_claim_scores.jsonl` | 93 rows, both splits — replays the verdict with no model |
| `h4_margin.calibration.curve.json` | the operating curve the floor was read off |
| `naive_baselines.json` | naive-strategy ceilings + the confusion matrix, derived read-only from the scores |
| `gate.run.log` | the run, including the capacity probe and the model load |
