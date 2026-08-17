# DEMO-7 — the corpus leg with figure-bearing admission and the honesty strengthen (T1 re-cut, order deep-research-t1h)

Order `deep-research-t1h` — the T1 local re-cut measured to the sixth
transition. The diagnosis (`research/deep-research/diagnosis/t1h-failure-taxonomy.md`,
landed FIRST per the order) classified every missed P4 key by the stage
of METHODOLOGY.md's thirteen that let it drop, and named the fixes: H1
(the corpus-leg triage boundary — the hit surface now carries the BODY,
and the figure-bearing decider reads title+snippet+content, ONE decider
preserved), H2 (draft figure-completeness — the deterministic figure
inventory in the draft prompt), and the honesty strengthen (the
witness's numeric-specificity rule, then the claim's OWN figure tokens
checked against the evidence BEFORE extraction — the partial-trace
shape the t1g probe exposed: the extractor dropped "2024" from the
specifics while the claim carried it and the window did not).

This demo shows the v1 report-class question rendered by the corpus leg
with those fixes landed — the same frozen corpus, the same frozen bank,
the same scorer:

> "How did American cities change across four decades (1980-2024):
> gentrification, inequality, affordability, and displacement — every claim
> cited?"

The probe that validated the instrument changes before any measurement
(`/tmp/dr-probe2/dr-*`, throwaway — instrument validation, NOT part of
the battery) recorded the mechanism this demo's flight is expected to
show: the window admits body-figure-bearing chunks (chunks 33/4/64/40 —
"30 years", "25 to 44" — under H1, versus the t1g-era 3 figure-free
chunks), and the passed-position honesty property holds structurally:
the [passed] claims carry only figures the evidence window carries —
the t1g-era violation (a [passed] claim restating "1980"/"2024" with
"2024" in no chunk) is downgraded, `reason: "claim figures absent from
the evidence — untraced: 2024"` — the exact pre-registered mechanism.

## What is in this directory

| File | What it is |
|---|---|
| `report-v1-corpus.md` | The v1 corpus flight's report (verbatim from the battery's run dir) — verdict-stamped claims, chunk-level estate citations |
| `bars.md` | The re-measured bars — **the scorer's own numbers** (score-report-t1h.json), never hand-typed |
| `verify-demo7.sh` | The corpus-source + honesty strips — the demo is only as strong as its verification |
| `README.md` | This file |

The corpus the flight searches is `dr-demo6-v1` — FROZEN since the t1g
mint, built ONCE from the verbatim frozen v1 deck bodies under
`demo/demo6/deck-extract/` (byte-identical to `bank/v1/deck/` minus the
deck.toml, verified at this order's landing — `diff -rq` clean). The
bank is read, never edited.

The raw artifacts live in the battery's run dir
`research/deep-research/arms/runs/loop/v1/dr-*/` — the manifest, the
plan, the per-round fetch lists (each search hit stamped `engine:
corpus` with its LanceDB relevance score), the triage outcome, the gap
lists, the evidence window, the skip ledger and the budget ledger, all
as recorded by the shipped CLI on the corpus source.

## What the corpus source did on this question (this order's flight)

1. **Source dispatch (mock | corpus).** Unchanged from t1g: the v1
   flight ran `--search-source corpus --corpora dr-demo6-v1`; every
   search hit stamped `engine: corpus`; every admitted chunk's locator
   is a chunk-level estate locator (`estate:dr-demo6-v1:<chunk_id>`)
   with the estate's `personal` custody. Measured on the flight
   (dr-1786933992): 4 round-1 search hits, engine=corpus on every hit,
   6 window chunks, custody=personal, locators estate-shaped (strip
   3b).
2. **The body-figure boundary fixed (H1).** The hit surface now carries
   the body (`content: Option<String>` on every port), and the
   figure-bearing decider reads title+snippet+content — so a chunk
   whose BODY carries the figure-bearing digits is admitted ahead of
   figure-free hits inside the quantized top bucket, where t1g's
   title-only read died. MEASURED: the triage still degenerates to
   insertion order inside the identical 1/30 f32 bucket (every round-1
   score 0.03333333507180214 — the t1g mechanism persists), but the
   admitted chunks now carry body figures ("100" on terry-uga, the
   "30 years"/"25 to 44" era figures on the admitted bodies), and the
   window admits 6 chunks across 3 rounds. The battery measured the
   count: 1 of the 11 predicted Class-C keys recovered (K16 — 35%,
   31%, 19% — the first value-bearing key the corpus leg ever carried
   to the answer); the other 10 Class-C keys and the 3 Class-D
   unreachable keys (K3/K9/K13, the frozen-arbiter ceiling) stand.
3. **The honesty strengthen.** The witness checks the claim's OWN
   figure tokens against the evidence before extraction — a claim
   figure the window does not carry is untraced, full stop, both
   polarities. MEASURED: the passed-position strip PASSES (the t1g
   era-years violation — a [passed] claim restating "1980"/"2024" with
   neither in the window — cannot recur; this flight carries zero
   passed-position claims and zero untraced flags, all 23 claims
   downgraded with named reasons), and the untraced-reason honesty
   strip (3a) holds: no untraced flag names a figure the window
   carries, and no claim's own citation tail leaked a digit into its
   reason (the amendment-2 class — the battery's seed-01..05 flights
   caught it RED-FIRST, were invalidated and never scored).
4. **The measured query-side finding (strip 3c, FAILS by design).** The
   round-1 gap-template query q1 (formed from the survey answer's gap
   row g2) carries the value-shaped run "100": the survey answer
   (model) quoted the estate's own admitted chunk (terry-uga, "the
   nation's largest 100 cities") and the gap-template carried the
   figure verbatim into the query. The figure traces to the admitted
   window — attribution is intact — but the query-side anti-leak
   property (the DEMO-5/6 shape test: round-1 queries introduce no
   value-shaped digits beyond the question's own) is violated by
   measurement on this flight, and the strip fails naming "100". The
   mechanism is the survey gap-formation quoting the estate's bodies —
   the same H1 surface that fixed the triage now feeds the query
   formation; journaled, never silenced.

## The measured bars (this order's re-measure)

| leg | measured | bar | verdict |
|---|---|---|---|
| P4-v0 | 63/72 | >=58/72 | passed — the FIRST pass in six measurements (52/49/52/53/51/63) |
| P4-v1 (loop) | 3/16 | >=12/16 | failed — K16 recovered of the 11 Class-C predicted |
| P3 | 12/13 | >=10/13 | passed — the v1 corpus flight's round-2 fetched 3 = round-1's 3 (not < 20%): the loop churned under the corpus source rather than converging; journaled |
| R-12 | 0/12 | >=10/12 | failed — structural, sixth consecutive |
| two-arm lift (pooled) | 0.977 vs 0.977 | +0.10 | failed by letter — exactly 0.0, direction no longer flipped (t1g: -0.043) |
| two-arm lift (v1) | 1.0 vs 1.0 | +0.15 | failed by letter |
| honesty letter | 0.023 vs 0.023 | loop <= one-shot | passed (t1g: 0.062 vs 0.019 failed) |
| honesty load-bearing | zero untraced figures in [passed] position | ANY arm | passed — verified independently over 96 flight artifacts (all arms-tree epochs t1d..t1h), not from the scorer's fixed note text |
| P5 | 6/6 | no noise band | passed (demo/p5/verify.sh green — the fabrication-absent strip asserts 0 passed claims; under the strengthened witness every drill claim is downgraded, the strip's asserted count) |

The per-question numbers and the scorer's bar-leg notes are in
`bars.md`, generated verbatim from `arms/score-report-t1h.json`.

## How to verify

```bash
./verify-demo7.sh
```

The strips, in order: (1) the v1 corpus flight exists and terminated;
(2-3) every report claim is verdict-stamped and every PASSED-position
figure — final verdict-set, per-round audits, report stamps — is
attributable to the accumulated evidence window (the t1g era-years
violation is now a PASS, measured); (3a) the untraced-reason honesty:
no untraced flag on a passed-position claim, every untraced reason
names figures genuinely absent from the window, and no citation-tail
leak (the amendment-2 class); (3b) the acquisition source is the
corpus (engines, estate locators, personal custody); (3c) the
concept->value shape test — **measured failure on this flight** (the
"100" query leak above, journaled); (4) bars.md carries the scorer's
numbers verbatim; (5) the two-arm lift is the same scorer's. Exit is
non-zero iff any strip failed — the measured failure is the
measurement, never silenced.
