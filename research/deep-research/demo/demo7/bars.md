# DEMO-7 bars — the re-measured dr-local-loop battery (t1h: claim-figure honesty instrument)

Order `deep-research-t1h` (T1 local re-cut — the corpus-triage boundary + draft
figure-completeness instrument changes, pre-registered BEFORE the re-measure per
ARCH_PRINCIPLES.md §18.6; the honesty strengthen: claim-figure tokens, downgrade-only).
The numbers here are the scorer's own (`arms/score-report-t1h.json`, `score-arms.py`
C-class deterministic) — this file is generated from that JSON, never hand-typed.

Protocol as pre-registered (`adversarial/pre-registration.md`): the full battery re-ran
against the FROZEN banks with the new instrument — 13 loop flights (12 v0 mock + the v1
report-class flight), budget 12/12, max-rounds 3, model pin daemon :9741
(Qwen3.6-35B-A3B-MTP-UD-Q6_K draft, Qwen3-Embedding-0.6B-Q8_0 embed, tau 0.9), same
driver (`arms/run-arms.sh`), one-shot comparator, P5 6-flight drill. The t1g numbers
(P4-v0 51/72, P4-v1 2/16, honesty letter FAILED) are old-instrument numbers, never mixed.

## Per-question coverage — loop vs one-shot (scorer verbatim)

| seed | loop | one-shot | loop density | one-shot density | P3 | R-12 |
|---|---|---|---|---|---|---|
| seed-01 | 3/6 | 4/6 | 1.0 | 1.0 | passed | failed |
| seed-02 | 6/6 | 4/6 | 1.0 | 0.75 | passed | failed |
| seed-03 | 7/7 | 7/7 | 0.857 | 1.0 | passed | failed |
| seed-04 | 6/6 | 6/6 | 1.0 | 1.0 | passed | failed |
| seed-05 | 6/6 | 6/6 | 1.0 | 1.0 | passed | failed |
| seed-06 | 4/6 | 4/6 | 0.8 | 1.0 | passed | failed |
| seed-07 | 6/6 | 6/6 | 1.0 | 0.833 | passed | failed |
| seed-08 | 5/5 | 4/5 | 1.0 | 1.0 | passed | failed |
| seed-09 | 4/6 | 4/6 | 1.0 | 1.0 | passed | failed |
| seed-10 | 6/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-11 | 4/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-12 | 6/6 | 6/6 | 1.0 | 1.0 | passed | failed |
| v1 | 3/16 | 10/16 | 1.0 | 1.0 | failed | failed |

pooled lift: **0.0** (loop 0.977 vs one-shot 0.977; bar loop >= one-shot + 0.10) — the direction no longer flips (t1g: -0.043 flipped; t1h: exactly 0.0), but the bar's +0.10 spread is not met.

## The bar legs (scorer verbatim)

| leg | measured | bar | verdict |
|---|---|---|---|
| P4-v0 | 63/72 | >=58/72 | passed — single-origin decks; the corroboration floor keeps coverage in open questions (honesty, reported separately) |
| P4-v1 (loop) | 3/16 | >=12/16 | failed — evidence-arbiter corrected forms applied per the frozen journal |
| P3 | 12/13 passed (+0 could-not-judge) | >=10/13 | passed — the v0 seeds all re-fetch the same exemplar (no fetch dedup); the v1 flight passed (round-2 fetched 0, coverage not worse) |
| R-12 | 0/12 v0 seeds | >=10/12 | failed — gap sets GROW on every seed (audits add single-origin floor caps); v1 journaled 1->26, not gated |
| T1.7 plan presence | 12/12 scoped flights | all scoped flights carry | passed — every figure-implying flight's plan sub-questions carry a digit or a measure word |
| two-arm lift (pooled) | 0.977 vs 0.977 | loop >= one-shot + 0.10 | failed — one-shot traces every numeric claim; the loop's flagged open-question claims stay untraced (see the honesty journal) |
| two-arm lift (v1) | 1.0 vs 1.0 | loop >= one-shot + 0.15 | failed — single-question comparison |
| honesty not worse | ungrounded loop 0.02300000000000002 vs one-shot 0.02300000000000002 | loop ungrounded <= one-shot | passed — letter leg: the loop's verdict-flagged claims (failed/could-not-judge) count as ungrounded; zero untraced numbers sit in [passed] position in ANY arm (both epochs, journaled) — t1e loop 0.117 < t1d 0.497 under the same instrument |

The instrument journal (the t1h changes — H1 corpus-leg triage boundary, H2 draft
figure-completeness inventory, the claim-figure honesty strengthen with the claim-side
citation strip; the scorer itself unchanged, C-class) is in `adversarial/pre-registration.md`,
t1h execution section. The v1 corpus flight's measured mechanisms — the triage still
degenerates to insertion order under the quantized 1/30 scores (only K16 of the 11
predicted Class-C keys recovered); the round-1 gap-template query carrying the survey
answer's own quoted figure ("100", tracing to the admitted chunk) — are journaled there
and in `demo/demo7/README.md`. P5 (poisoned-drill battery, 6/6, no noise band) is verified
by `demo/p5/verify.sh` — a separate gate, not scored here.
