# DEMO-4 — the figure-hunting loop rendering the v1 report-class question

Order `deep-research-t1e` (T1.7 query formation). The v1 question is the
report-class question the figure-hunting fixes exist to serve:

> "How did American cities change across four decades (1980-2024):
> gentrification, inequality, affordability, and displacement — every claim
> cited?"

The t1d battery measured the cap: the daemon's thematic sub-questions never
carried the figure tokens (Gini 0.5469, Case-Shiller 325.78, the 95/20 ratio,
white share, manufacturing jobs), so the figure-specific deck hits were
unreachable by any downstream fix, and the K-cut admitted by insertion order
at all-0.9 ties. This demo shows the **figure-hunting loop** rendering that
question: the plan artifact records the question's own figure specifiers and
every sub-question carries one; the triage records its figure-bearing
admission rule; the report is every-number-attributable with every absence
named.

## What is in this directory

| File | What it is |
|---|---|
| `report-v1-fixed.md` | The v1 flight's report (verdict-stamped claims, chunk-level citations), with the re-measured bars beside it |
| `bars.md` | The re-measured bars — **the scorer's own numbers** (score-report-t1e.json), never hand-typed |
| `verify-demo4.sh` | The honesty strips — the demo is only as strong as its verification |
| `README.md` | This file |

The raw artifacts live in the battery's run dir
`research/deep-research/arms/runs/loop/v1/dr-*/` — the manifest, the plan
(with `figure_specifiers` on the acquisition record), the per-round fetch
lists (each triage outcome carrying its `admission_rule`), gap lists,
evidence windows, skip ledgers and the budget ledger are all there, as
recorded by the shipped CLI on the mock-deck surface.

## What the figure-hunting loop did differently on this question

1. **R1 figure-hunting sub-questions (prompt shape).** The plan prompt asks
   the draft to name the specific measure each sub-question implies — an
   index, a ratio, a share, a rate, a count, a median, a price, a percentage
   change — and the entities (cities, years). Generic shape, never bank
   vocabulary. The plan artifact records the question's own figure
   specifiers (`["1980", "2024"]` on this flight — the question's digit
   runs) and folds them into any sub-question the draft left bare, so the
   frontier is figure-bearing structurally, whatever the draft returned.
2. **R4 specifier fold-in.** A gap query with no figure specifier gets the
   question's specifiers appended; the floor-capped fact query already
   carries the claim's figures and never passes through here.
3. **R5 figure-bearing admission.** `triage_hits` ranks score-first, then
   figure-bearing-ness (the hit's own title/snippet carries a digit), then
   insertion order, and records the rule on the triage outcome
   (`admission_rule: "score-then-figure-bearing"`). The K-cut cannot
   silently exclude the hits the figures live in.

## How to verify

```bash
./verify-demo4.sh
```

The strips, in order:

1. the v1 flight exists and terminated (report.md + terminal manifest);
2. every claim in the report is verdict-stamped — a claim with no verdict
   is a silent number;
3. every figure token in the report's claims appears in the run's
   accumulated evidence window, or the claim is flagged
   could-not-judge/never-ran (absence named);
3b. the acquisition mechanics on THIS flight are the figure-hunting loop's —
   the launch plan records the question's own figure specifiers, EVERY
   plan sub-question carries a figure specifier (the SHAPE requirement,
   re-derived independently here), and every triage outcome records
   `score-then-figure-bearing`;
4. `bars.md` carries the scorer's per-question covered fractions and bar
   legs verbatim — the bars are the scorer's numbers, never hand-typed;
5. the two-arm lift is computed by the same scorer over the same pairs.

The floor is the unweakened `CORROBORATION_FLOOR = 2`: a claim passes only
on ≥2 distinct source origins. This demo's reports are the floor's honest
output — passed where corroborated, could-not-judge where the deck capped
it, never a silent number.

## Re-produce

```bash
cd research/deep-research/arms
./run-arms.sh                       # 13 flights: 12 v0 + v1 (12/12 budget, pre-registered)
python3 score-arms.py --pairs runs/pairs.json \
    --loop runs/loop --oneshot runs/oneshot \
    --out runs/../score-report-t1e.json
cd ../demo/demo4
./verify-demo4.sh
```
