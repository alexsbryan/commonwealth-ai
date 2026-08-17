# DEMO-10 — the corpus leg with the deterministic admission second key and the strip-3c query-side fix (T1 residuals, order deep-research-t2c)

Order `deep-research-t2c` — the two banked T1 residuals the t1h landing
named, measured to the seventh transition. Both instruments were
pre-registered (`adversarial/pre-registration.md`, t2c declaration)
BEFORE any code change or flight, and both landed red-first:

- **Instrument 1 (the tie-break).** t1h's measured mechanism: LanceDB
  hybrid relevance quantizes every corpus hit to one f32 bucket
  (0.03333333507180214 = 1/30), and after H1 every chunk is
  figure-bearing, so the triage's figure key no longer discriminates
  inside the bucket and admission degenerated to insertion order. The
  fix extends the ONE corpus admission decider (gym.rs `estate_search`,
  extended, never a second decider — §10.6) with a deterministic
  second key: hybrid score desc, then query-term overlap desc (distinct
  query terms present in the chunk's content, the T1.9 ONE tokenizer
  `terms()` both legs share), then insertion order — the term-ranked
  mock's reference shape. Red-first test
  `corpus_admission_second_key_admits_figure_bearing_at_equal_score`
  (gym.rs): two equal-scored corpus hits, the figure-free one inserted
  first — at HEAD the figure-free hit is admitted (the t1h residual
  shape); after the fix the overlap key admits the figure-bearing hit.
- **Instrument 2 (the strip-3c anti-leak).** t1h's measured defect: the
  round-1 gap-template query q1 carried "100" — the survey answer's
  quote of the estate's own admitted chunk ("the nation's largest 100
  cities") echoed verbatim into the acquisition query. The fix: gap
  formation carries no figure tokens beyond the question's own — the
  ONE gap-query formation point (`gap_query_for`, mod.rs, both its
  shapes) strips non-question figure tokens, with the ONE decider
  `figure_tokens(question)`. Red-first test
  `gap_query_does_not_echo_estate_figures` (mod.rs): at HEAD the gap
  query carries "100"; after the fix it carries no figure tokens beyond
  the question's own.

This demo shows the v1 report-class question rendered by the corpus leg
with both instruments landed — the same frozen corpus, the same frozen
bank, the same scorer:

> "How did American cities change across four decades (1980-2024):
> gentrification, inequality, affordability, and displacement — every claim
> cited?"

## What is in this directory

| File | What it is |
|---|---|
| `report-v1-corpus.md` | The v1 corpus flight's report (verbatim from the battery's run dir) — verdict-stamped claims, chunk-level estate citations |
| `bars.md` | The re-measured bars — **the scorer's own numbers** (score-report-t2c.json), never hand-typed |
| `verify-demo10.sh` | The corpus-source + honesty + tie-break strips — the demo is only as strong as its verification |
| `README.md` | This file |

The corpus the flight searches is `dr-demo6-v1` — FROZEN since the t1g
mint, built ONCE from the verbatim frozen v1 deck bodies under
`demo/demo6/deck-extract/` (byte-identical to `bank/v1/deck/` minus the
deck.toml, verified at the t1g landing — `diff -rq` clean). The bank is
read, never edited.

The raw artifacts live in the battery's run dir
`research/deep-research/arms/runs/loop/v1/dr-1786952256/` — the
manifest, the plan, the per-round fetch lists (each search hit stamped
`engine: corpus` with its LanceDB relevance score), the triage outcome
(including the `below_cut` reject record), the gap lists, the evidence
window, the skip ledger and the budget ledger, all as recorded by the
shipped CLI on the corpus source.

## What the corpus source did on this question (this order's flight)

1. **Source dispatch (mock | corpus).** Unchanged from t1h: the v1
   flight ran `--search-source corpus --corpora dr-demo6-v1`; every
   search hit stamped `engine: corpus`; every admitted chunk's locator
   is a chunk-level estate locator (`estate:dr-demo6-v1:<chunk_id>`)
   with the estate's `personal` custody. Measured on the flight: 4
   round-1 search hits, engine=corpus on every hit, window = chunks
   29/4/64/40, custody=personal, locators estate-shaped (strip 3b).
2. **The tie-break decider's engagement, measured (Instrument 1).** All
   4 round-1 hits score exactly `0.03333333507180214` — the quantized
   1/30 bucket, identical to the triage threshold recorded in the fetch
   list's triage block — AND the triage `below_cut` carries 117
   rejected chunk ids: the corpus search returned 121 hits and admission
   selected 4. Admission inside the tied bucket was therefore decided by
   the second key this order added, not by insertion order. The
   decider's behavior at equal score (overlap desc admits the
   figure-bearing hit over a figure-free hit inserted first) is pinned
   by the landed red-first unit test — the flight strip verifies the
   engagement conditions artifact-level (one bucket = the threshold,
   rejects existed, no admitted id in below_cut) and measures the
   outcome with the scorer's OWN decider (strip 3d).
3. **The strip-3c fix, measured (Instrument 2 — the measured flip).** At
   t1h this strip FAILED by measurement: round-1 gap-template query q1
   carried "100" (the survey answer's own quoted figure from the
   admitted estate chunk). On this flight the round-1 queries introduce
   NO value-shaped digit runs beyond the question's own — q1
   ("Four decades data demonstrate urban areas generate substantial
   wealth attract educated professionals") carries no value runs at all
   (strip 3c). The demo7 decider is unchanged; the outcome is the
   measurement.
4. **The honesty constitution, measured — with the instrument
   validated first (§18.4).** 25 verdict-set claims = 20
   could-not-judge + 5 passed (c5 "more than double that of the
   **1990s**", c6 "increased in **39** of the **50** cities", c7
   "**54** neighborhoods ... since **2000**", c9 "wealthier in nominal
   terms since **1980**", c16 "after **1980**"). Zero untraced figures
   sit in [passed] position: every passed claim's figure tokens trace
   to the audits' evidence (strips 2-3, 3a). The 20 open questions
   carry the single-origin floor caps (the corroboration floor),
   downgraded with named reasons. **Instrument-validation note:** the
   verify script's first run measured ONE false violation — gap-list-1
   c4's "100" flagged absent. The instrument was checked before the
   result was believed (§18.4): the round-1 AUDIT's evidence is the
   merged window — the survey's searched hits (survey-1.json,
   estate-N ids, 8 chunks: 64/65/50/33/21/29/40/4) plus the
   acquisition windows — a SUPERSET of evidence-window-1.json's 4
   admitted chunks (29/4/64/40). The "100"-bearing UGA chunk (33) sits
   in the survey window the audit saw, so c4's pass was honest against
   the loop's own evidence; the tie-break's acquisition simply did not
   admit chunk 33 (the t1h-era admission did, which is why the t1h
   strip's subset evidence never diverged). The strip now checks the
   UNION — the window the audits actually saw — and the flight is
   clean. Swept independently over the whole battery: 13/13 loop
   reports carry zero untraced figures in [passed] position (the
   scorer's OWN NUMERIC_TOKEN, citation tails cut).
5. **The measured outcome of the v1 clause — a MEASURED FAILURE,
   journaled, never silenced.** The pre-registered prediction (10
   standing Class-C keys K1/K2/K4/K5/K6/K7/K10/K11/K12/K15 recover with
   the deterministic second key) FAILED by measurement: the scorer's
   own decider measures **2/16** on this flight (down from t1h's 3/16,
   old instrument) — K8 and K14 covered, neither in the predicted set;
   the 10 predicted keys all uncovered; the frozen Class-D ceiling held
   for K9 (cannot-clear); K3/K13 uncovered by the figure decider
   (measured, not gated). The v1 clause (>=12/16) therefore fails this
   battery. A measured failure is the measurement — the prediction's
   failure is recorded here, in the execution record, and in the bar
   transition, with the per-key reasons.

## The measured bars (this order's re-measure)

| leg | measured | bar | verdict |
|---|---|---|---|
| P4-v0 | 65/72 | >=58/72 | passed — SEVENTH measurement, second pass (52/49/52/53/51/63/65) |
| P4-v1 (loop) | 2/16 | >=12/16 | failed — down from t1h's 3/16 (old instrument); the pre-registered prediction (10 Class-C recoveries) failed by measurement; K8/K14 covered |
| P3 | 13/13 passed (+0 could-not-judge) | >=10/13 | passed — the v0 seeds all re-fetch the same exemplar (no fetch dedup); the v1 flight passed (round-2 fetched 0, coverage not worse: 2/16 final >= 1/16 round-1-evidence) |
| R-12 | 0/12 | >=10/12 | failed — structural, seventh consecutive |
| two-arm lift (pooled) | 0.979 vs 0.976 | +0.10 | failed by letter — direction positive for the first time (t1h: 0.0; t1g: -0.043), but +0.003 is not +0.10 |
| two-arm lift (v1) | 0.909 vs 0.967 | +0.15 | failed by letter |
| honesty not worse | 0.021 vs 0.024 | loop <= one-shot | passed |
| honesty load-bearing | zero untraced figures in [passed] position | ANY arm | passed — artifact-verified, including the corpus flight's 5 passed claims |
| P5 | 6/6 | no noise band | passed (demo/p5/verify.sh green — trace identity, 0 passed claims, injection inert) |

The per-question numbers and the scorer's bar-leg notes are in
`bars.md`, generated verbatim from `arms/score-report-t2c.json`.

## How to verify

```bash
./verify-demo10.sh
```

The strips, in order: (1) the v1 corpus flight exists and terminated
(done-partial, truncation declared); (2-3) every report claim is
verdict-stamped and every PASSED-position figure — final verdict-set,
per-round audits, report stamps — is attributable to the accumulated
evidence window; (3a) the untraced-reason honesty: no untraced flag on a
passed-position claim, every untraced reason names figures genuinely
absent from the window, no citation-tail leak; (3b) the acquisition
source is the corpus (engines, estate locators, personal custody); (3c)
the concept->value shape test — **the strip-3c measured flip** (t1h
FAILED on the "100" leak; this flight's round-1 queries carry no
value-shaped digits beyond the question's own); (3d) the tie-break:
one identical score bucket == the triage threshold, below_cut rejects
existed (admission was selective inside the bucket), and K/N per key
from the scorer's own decider with the frozen Class-D ceiling held and
the pre-registered prediction journaled by measured outcome (FAILED —
never silenced; the strip verifies the artifacts, the bar verdict lives
in bars.md); (4) bars.md carries the scorer's numbers verbatim; (5) the
two-arm lift is the same scorer's. Exit is non-zero iff any strip
failed — the measured failure is the measurement, never silenced.
