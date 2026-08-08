# H1 kill gate — the verdict

**H1 lives.** The reranker margin separates answerable from absent better
than `top_cosine` by **+0.0995 AUROC** on 4,207 calibration pairs. The kill
criterion (`NATIVE_GROUNDING.md §7.3`: die if margin AUROC < top_cosine +
0.05) is cleared in **1,000 of 1,000** paired bootstrap resamples. The
fallback — training the 4B head via the verifier-v0 pipeline — is not
needed and is not the recommendation.

The gate was allowed to fail. `H1Outcome::Killed` is a real branch with its
own exit code (3) and its own test (`the_h1_gate_is_allowed_to_kill`, which
feeds it a coin-flip margin against a perfect cosine and asserts the kill).
It did not fire.

## The numbers

Run 2026-08-08, BeefyMac (M2 Max, 64 GB). Reranker
Qwen3-Reranker-0.6B-Q8_0; embedder `qwen-embedding-0.6b`. 4,207 pairs
(2,255 answerable / 1,952 absent), 1,640 s at 2.6 pairs/s.

| | AUROC | honesty-recall @ FA 5% | @ 10% | @ 20% | best BAcc |
|---|---|---|---|---|---|
| **rerank_margin** | **0.8990** | **0.665** | **0.742** | **0.828** | 0.824 @ tau=6.68 |
| `top_cosine` | 0.7994 | 0.235 | 0.391 | 0.614 | 0.731 @ tau=0.685 |

*honesty-recall* = of the genuinely absent pairs, the fraction a threshold
correctly abstains on. *FA* (false alarm) = of the genuinely answerable
pairs, the fraction it wrongly abstains on.

**The AUROC gap understates the practical one.** At the 5% false-alarm
budget — the operating point a production router would actually want —
the reranker catches **66.5%** of unanswerable questions where cosine
catches **23.5%**. That is 2.8x the honesty at the same cost in wrongly
refused answers, and it matters more than the headline AUROC.

### The delta against its bar, honestly

| | value |
|---|---|
| delta (margin - cosine) | **+0.0995** |
| 95% CI (1,000 paired resamples, seed 1592590337) | **[+0.0889, +0.1092]** |
| P(delta >= kill bar 0.05) | **1.000** |
| P(delta >= beat bar 0.10) | **0.469** |

The point estimate lands **0.0005 below** §7.3's "beat" bar of +0.10, so
the artifact records `survives` rather than `beat`. **Do not read that
distinction as a result.** The bootstrap puts the beat bar at a coin flip
(0.469), which means this measurement does not resolve beat-vs-survive at
all — a different sample of 4,207 pairs would land on either side. What it
resolves, and resolves completely, is the question the gate was built to
ask: H1 is not dead.

The interval is *sampling* uncertainty over which pairs are in the set.
The measurement itself is exactly deterministic: two full end-to-end runs
of the 24-pair smoke produced byte-identical scores and verdict, and
`--from-scores` rebuilds every curve and the verdict from frozen scores
with no model loaded (verified byte-identical).

### Split by corpus family

| family | pairs | margin AUROC | cosine AUROC | delta |
|---|---|---|---|---|
| **sep** | 4,188 | 0.8994 | 0.7999 | +0.0995 |
| **literary** | 19 | 0.9556 | 0.7944 | +0.1612 |

**The literary row is not a measurement and must not be quoted as one.**
19 pairs is brothers-karamazov-book-1's entire ceiling (13 `Claim` atoms,
3 of which quote text found in none of its 41 real chunks). Its curve is
emitted for completeness; a per-family AUROC on 19 pairs has a confidence
interval wide enough to contain almost anything, and its looking *better*
than SEP is not evidence that the reranker prefers fiction. The overall
number is the SEP number, because SEP is 99.5% of the set.

## What this does and does not license

**Does:** Phase 1's kill gate is settled in H1's favour. §8's next phase —
wiring the answerability scorer at the early-decline seam
(`handlers/knowledge_query.rs:541,640`) behind a dark flag — is funded by
this result.

**Does not:** nothing here is a runtime integration, and this order
deliberately landed none. Two things a follow-on order must decide that
this measurement cannot:

1. **Where the three-way cut goes.** §5 H1 wants `answer` / `hedge` /
   `abstain`, and this artifact gives one curve, not two thresholds. The
   committed `h1_rerank_margin.overall.curve.json` carries all 4,200
   operating points, so the cut can be chosen from data — but choosing it
   is a decision, and it belongs to the order that ships it with a
   `DEFAULTS_LEDGER.md` row.
2. **Whether the calibration distribution matches the runtime one.**
   These pools are same-article distractors, chosen deliberately so the
   negatives are topically hard (§5 H1's "~0.75 in-topic thin" failure is
   exactly the case `top_cosine` loses). Real retrieval draws from the
   whole corpus and will produce an easier mix, so the honesty-recall
   numbers here are, if anything, a floor. That is an argument for
   confidence, not a substitute for measuring it on live retrieval.

## Known limits of this set

- **30.5% of SEP claims were dropped**, not scored: their quoted evidence
  fragment appears in no real passage (paraphrase rather than quotation),
  and the miner refuses to attach the nearest-looking chunk. The scored
  population is therefore claims whose evidence is *quotable*, which may
  be marginally easier than the full population.
- **`answerable_witness_absent` is 823 of 2,255.** Not a label defect: the
  label rests on verbatim anchor containment, and the witness terms come
  from the claim's paraphrased content. It is reported because a consumer
  that mistook `gold_match` for the label would be misled.
- **Contamination: CLEAN**, 0 collisions against 6,920 13-gram shingles
  from all three banks. The dev and test banks were never read except
  through the shingle index.

## Artifacts

| file | what |
|---|---|
| `h1_verdict.json` | the gate's decision + the bootstrap interval |
| `h1_{rerank_margin,top_cosine}.overall.curve.json` | full operating curves, 4,200 / 4,012 points |
| `h1_*.sep.curve.json`, `h1_*.literary.curve.json` | the §7.3 family split |
| `h1_scores.jsonl` | every pair's two scores — replays the whole report with no model |

Reproduce the report from frozen scores (no model, seconds):

```
svrn bench flywheel h1-gate \
  --from-scores sovereign/bench/calibration/h1/h1_scores.jsonl \
  --out-dir sovereign/bench/calibration/h1
```

Reproduce the measurement (needs the reranker GGUF, ~27 min):

```
svrn bench flywheel h1-gate --rerank-model <qwen3-reranker-0.6b-q8_0.gguf>
```
