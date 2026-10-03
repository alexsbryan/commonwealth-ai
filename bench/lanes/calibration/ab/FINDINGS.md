# The A/B — H1 bought no honesty and cost two thirds of competence

**The flag stays OFF.** `SOVEREIGN_NATIVE_GROUNDING=1` on the saltgrass dev
bank fails HARD bar (b) catastrophically and reproducibly, and passes HARD
bar (a) only by not moving it.

| bar | flag OFF | flag ON r1 | flag ON r2 | bar | verdict |
|---|---|---|---|---|---|
| (a) honesty-when-absent | 0.91 | 0.91 | 0.91 | ≥ 0.91 | PASS, delta **0.00** |
| (b) competence-when-present | **0.74** | **0.26** | **0.23** | ≥ 0.80 cal / 0.71 holdout | **FAIL, delta −0.48** |

Honesty: 10/11 in every arm. Competence: 23/31 off, 8/31 and 7/31 on.

**The asymmetry is the whole finding.** This is not a trade-off where the
system bought honesty and paid in competence. It bought *nothing*. The
incumbent already caught 10 of the 11 absent probes; the headroom H1 could
have captured was a single probe, and it captured none of it — while
turning 15 of 23 correct answers into refusals.

## Why: the margin scale does not transfer

H1 admitted 33 turns and abstained on **31** of them.

| | margin |
|---|---|
| min | −11.03 |
| p25 | 3.61 |
| **p50** | **4.49** |
| p75 | 5.08 |
| max | 7.59 |
| **tau_abstain** | **5.885** |

The threshold was fitted on SEP + brothers-karamazov. On saltgrass the
median turn sits 1.4 margin-units below it, so nearly everything abstains.

The size of the shift is best seen through the committed curve itself. At
margin 4.488 the calibration corpus records a **0.98%** false-alarm rate.
On saltgrass, **50%** of turns fall below that same margin — a **~50x**
discrepancy. The curve's false-alarm axis simply does not describe this
corpus, which means no operating point read off it can be trusted to price
the trade here.

This confirms `NATIVE_GROUNDING.md` §10's **first named risk**, word for
word: *"The reranker head may not transfer from passage-relevance to
answer-containment on our corpora."* The kill gate deliberately measured
the signal offline on one corpus family; this is the first time it met
another, and it did not survive the trip.

## The in-curve parameter check — no, and for two independent reasons

The order allows recovery via any parameter *inside* the committed
operating curve. There is none.

1. **Mechanically**, competence needs `tau_abstain` below ~3.6 to admit
   the bulk of saltgrass turns. The honesty that point promises (0.4226
   recall) is denominated in a margin scale that has just been shown not
   to transfer, so the promise is not collectable here.
2. **Empirically**, and this settles it without any curve reading:
   honesty was already 0.91 flag-off and reached exactly 0.91 flag-on.
   There is one probe of headroom on this bank and H1 got none of it.
   No threshold buys honesty that is not there to buy.

**The thresholds were not re-fitted on saltgrass.** Re-fitting on the bank
under test is exactly what pre-registration exists to prevent, and it would
have converted a clean negative into a guess.

## Bars (c), (d), (e)

**(c) Run wall time** — 21m12s flag-off, 11m35s and 11m35s flag-on, over 42
probes. This is *not* the decline-latency p50 the order asked for:
`ResultRow` carries no per-turn latency field, so that p50 is not derivable
from these artifacts. Labelled as what it is rather than relabelled as the
bar. H1's own admission cost is **0 ms** — the margin is reused from
retrieval's existing rerank pass, never recomputed.

The speed-up is real and it is *the same fact as the failure*: an abstained
turn skips synthesis on the primary slot and never reaches the gate. It is
the cost seen from the other side, not an independent win.

**(d) Judge calls per gated turn** — **could not measure.** No judge-call
counter exists on `ResultRow` or in the chaos harness. H4's gate hit this
same wall and recorded the incumbent as *"~35 per gated longform turn —
cited, NOT measured"*; repeated as cited rather than invented as measured.
Judge-skip was never wired: D2 measured resolver precision 0.7429 against a
pre-pinned 0.98 bar, so there are no avoided calls to count.

**(e) Segment coverage** — 33 turns segmented, 56 segments, **0 grounded**,
53 unverified; 2 released claims, **0** carrying an address.

Two honesty notes on this number. First, the initial flag-on run reported
zero because the log filter enabled `runtime::grounding` while the D4
instrumentation lives in `runtime::streaming` — an **instrument error**,
and reporting its zero as coverage would have been reporting a filter
mistake as a finding. The re-run with the corrected filter produced the
numbers above. Second, 0% here follows directly from bar (b): with 31 of 33
turns abstained, the released text is parametric prose over an emptied
evidence pool, so every segment correctly resolves `Unverified`. **This
measures the flag-on path as it behaved, not the ceiling of D4's segment
machinery**, which this run never exercised on a normally-answered turn.

## How the comparison was made honest

- **Both arms carry the reranker.** Only `SOVEREIGN_NATIVE_GROUNDING`
  differs. With the reranker in one arm only, this would have confounded
  H1's admission with the reranker's effect on retrieval, since
  `search_with_rerank` changes which chunks survive.
- **The control is not production.** This host has no rerank slot
  configured, so the flag-off arm is the correct control for the *flag* —
  not a picture of today's default.
- **The instrument was validated first.** `SOVEREIGN_RERANK_MODEL_PATH` is
  unset by default here; without it H1 returns `NoInstrument` every turn
  and flag-on is byte-identical to flag-off — a void A/B that reads as a
  clean no-regression. A 2-probe smoke confirmed the reranker loads and H1
  fires before the hours were committed.
- **The control arm was verified dark**: zero native-grounding lines, one
  reranker load.
- **Two runs, not one**, for the headline number. 0.26 and 0.23 — the
  failure reproduces.
- **The bank's own red-lines are the metric.** Competence counts 31 probes,
  not just the 20 tagged `present`; minting a second competence metric here
  would have put two implementations of one number in the workspace.
- **All three arms exit non-zero, including the control.** RED-LINE 4
  (acquisition-conjecture, 0.55 flag-off) fails independently of this order
  and is pre-existing. Exit 1 from this harness is a gate verdict, not a
  harness failure.

## What this licenses for Step 3

Not a flip, and not a quiet retry. The admission stage is sound code
sitting on a calibration that covers one corpus family. Step 3 is therefore
a **recalibration decision with a number attached**, not a guess: either
per-corpus calibration of `tau_abstain`, or the §7.3 fallback (train the 4B
head via the verifier-v0 pipeline, written down before any of this began),
or a decision that answerability routing is not worth its transfer cost.

`saltgrass_compound` was not run: it has zero absent probes, so its honesty
gate is a 0/0 NaN and it cannot speak to bar (a).
