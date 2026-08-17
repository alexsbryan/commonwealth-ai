# DEMO-10 bars — the re-measured dr-local-loop battery (t2c: corpus-leg tie-break + strip-3c)

Order `deep-research-t2c` (the two banked T1 residuals: the v1 corpus-leg equal-score
tie-break — Instrument 1, the deterministic second key in the ONE admission decider —
and the strip-3c query-side figure leak — Instrument 2, gap formation carries no estate
figure tokens; both pre-registered BEFORE the re-measure per ARCH_PRINCIPLES.md §18.6).
The numbers here are the scorer's own (`arms/score-report-t2c.json`, `score-arms.py`
C-class deterministic) — this file is generated from that JSON, never hand-typed.

Protocol as pre-registered (`adversarial/pre-registration.md`): the full battery re-ran
against the FROZEN banks with the two instruments landed — 13 loop flights (12 v0 mock +
the v1 report-class flight on the corpus source, `--search-source corpus --corpora
dr-demo6-v1`), budget 12/12, max-rounds 3, model pin daemon :9741
(Qwen3.6-35B-A3B-MTP-UD-Q6_K draft, Qwen3-Embedding-0.6B-Q8_0 embed, tau 0.9), same
driver (`arms/run-arms.sh`), one-shot comparator (exit 0, 126.27s), P5 6-flight drill +
verify. The t1h numbers (P4-v0 63/72, P4-v1 3/16) are old-instrument numbers, never
mixed with the new.

## Per-question coverage — loop vs one-shot (scorer verbatim)

| seed | loop | one-shot | loop density | one-shot density | P3 | R-12 |
|---|---|---|---|---|---|---|
| seed-01 | 5/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-02 | 5/6 | 4/6 | 1.0 | 0.667 | passed | failed |
| seed-03 | 7/7 | 7/7 | 1.0 | 1.0 | passed | failed |
| seed-04 | 6/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-05 | 6/6 | 6/6 | 1.0 | 1.0 | passed | failed |
| seed-06 | 4/6 | 4/6 | 0.833 | 1.0 | passed | failed |
| seed-07 | 6/6 | 4/6 | 1.0 | 1.0 | passed | failed |
| seed-08 | 4/5 | 5/5 | 1.0 | 1.0 | passed | failed |
| seed-09 | 5/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-10 | 6/6 | 6/6 | 1.0 | 1.0 | passed | failed |
| seed-11 | 5/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-12 | 6/6 | 6/6 | 1.0 | 1.0 | passed | failed |
| v1 | 2/16 | 11/16 | 0.909 | 0.967 | passed | failed |

pooled lift: **0.0030000000000000027** (loop 0.979 vs one-shot 0.976; bar loop >=
one-shot + 0.10) — the direction is positive (t1h: exactly 0.0; t2c: +0.003, the loop's
density now exceeds the one-shot's), but the bar's +0.10 spread is not met.

## The bar legs (scorer verbatim)

| leg | measured | bar | verdict |
|---|---|---|---|
| P4-v0 | 65/72 | >=58/72 | passed — single-origin decks; the corroboration floor keeps coverage in open questions (honesty, reported separately) |
| P4-v1 (loop) | 2/16 | >=12/16 | failed — evidence-arbiter corrected forms applied per the frozen journal |
| P3 | 13/13 passed (+0 could-not-judge) | >=10/13 | passed — the v0 seeds all re-fetch the same exemplar (no fetch dedup); the v1 flight passed (round-2 fetched 0, coverage not worse) |
| R-12 | 0/12 v0 seeds | >=10/12 | failed — gap sets GROW on every seed (audits add single-origin floor caps); v1 journaled 1->26, not gated |
| T1.7 plan presence | 12/12 scoped flights | all scoped flights carry | passed — every figure-implying flight's plan sub-questions carry a digit or a measure word |
| two-arm lift (pooled) | 0.979 vs 0.976 | loop >= one-shot + 0.10 | failed — one-shot traces every numeric claim; the loop's flagged open-question claims stay untraced (see the honesty journal) |
| two-arm lift (v1) | 0.9090909090909091 vs 0.9666666666666667 | loop >= one-shot + 0.15 | failed — single-question comparison |
| honesty not worse | ungrounded loop 0.02100000000000002 vs one-shot 0.02400000000000002 | loop ungrounded <= one-shot | passed — letter leg: the loop's verdict-flagged claims (failed/could-not-judge) count as ungrounded; zero untraced numbers sit in [passed] position in ANY arm (both epochs, journaled) — t1e loop 0.117 < t1d 0.497 under the same instrument |

The instrument journal (the t2c changes — Instrument 1: `rank_corpus_hits`, the corpus
admission decider's deterministic second key (hybrid score desc -> query-term overlap
desc -> insertion order, the term-ranked mock's reference shape, §10.6); Instrument 2:
the strip-3c figure-strip family at the ONE gap-formation point (`gap_query_for`, both
its shapes) — gap queries carry no figure tokens beyond the question's own; the scorer
itself unchanged, C-class) is in `adversarial/pre-registration.md`, t2c declaration +
execution sections. The measured outcome of the v1 clause is a MEASURED FAILURE —
2/16, DOWN from t1h's 3/16 (old instrument): the pre-registered prediction (10 standing
Class-C keys recover with the second key) FAILED by measurement; the two covered keys
(K8, K14) were outside the predicted set, the frozen Class-D ceiling held for K9
(cannot-clear), and the strip-3c fix measured (round-1 gap queries figure-free beyond
the question's own — the t1h "100" leak is gone). The failure is journaled, never
silenced, in `adversarial/pre-registration.md` (execution record) and
`demo/demo10/README.md`. P5 (poisoned-drill battery, 6/6, no noise band) is verified by
`demo/p5/verify.sh` — a separate gate, not scored here.
