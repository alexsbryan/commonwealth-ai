# DEMO-5 — term-ranked retrieval rendering the v1 report-class question

Order `deep-research-t1f` (T1.9 realistic mock retrieval). The v1 question is
the report-class question the deep-research loop exists to serve:

> "How did American cities change across four decades (1980-2024):
> gentrification, inequality, affordability, and displacement — every claim
> cited?"

The t1e-era cap: the mock's EXACT-VALUE instrument returned a hit only when
the query already contained one of the deck's curated match tokens — which
carry the bank's exact figures (Gini 0.5469, Case-Shiller 325.78) — so an
honest loop (bank vocabulary never enters a prompt) could not retrieve the
documents the figures live in, and the t1e battery measured the cap: P4-v1
3/16 loop vs 7/16 one-shot, the residual journaled as "the deck's SPECIFIC
values ... unreachable under the frozen scorer". Real search retrieves
documents by TERM relevance: "NYC Gini coefficient" hits the document
containing 0.5469 without the loop ever knowing the value. This demo shows
the **term-ranked retrieval instrument** — pre-registered BEFORE the
re-measure (`adversarial/pre-registration.md`, T1.9 declaration) — rendering
that question: the deck's term index scores hits by relevance count, round-1
carries DISTINCT scores (16/15/14/13), the concept query retrieves and
admits the value-bearing document without ever naming its figures — and the
report is every-number-attributable with every absence named.

## What is in this directory

| File | What it is |
|---|---|
| `report-v1-fixed.md` | The v1 flight's report (verdict-stamped claims, chunk-level citations), with the re-measured bars beside it |
| `bars.md` | The re-measured bars — **the scorer's own numbers** (score-report-t1f.json), never hand-typed |
| `verify-demo5.sh` | The instrument strips — the demo is only as strong as its verification |
| `README.md` | This file |

The raw artifacts live in the battery's run dir
`research/deep-research/arms/runs/loop/v1/dr-*/` — the manifest, the plan
(with `figure_specifiers` on the acquisition record), the per-round fetch
lists (each search hit carrying its term-relevance score, each triage
outcome its admission rule), gap lists, evidence windows, skip ledgers and
the budget ledger are all there, as recorded by the shipped CLI on the
mock-deck surface.

## What the term-ranked instrument did differently on this question

1. **Term-ranked scores (T1.9).** The deck's term index is built over each
   hit's full declared surface (match tokens + title + snippet + body file)
   at deck load — the bodies were already part of the deck (the harvest), so
   the index is total and the frozen banks are read, never edited. A hit's
   `score` IS its relevance: the number of distinct query terms in its term
   set. Round 1 of this flight: 16/15/14/13 — distinct relevance counts,
   where the old exact-value instrument returned flat 0.9-score ties.
2. **Concept -> value retrieval.** The round-1 queries (7 of them) name no
   value-shaped figure — era years (1970-2023) and generic descriptors
   ("15-year-old homes", "per 1,000 renters") only — yet the value-bearing
   document (h11, the source report whose body carries Gini 0.5469,
   Case-Shiller 325.78, the 18:1 ratio) retrieves at the TOP relevance score
   and is admitted by the round-1 triage (code_set_k + eps_admits). Verify
   strip 3b proves both halves from the artifacts, deriving the distinctive
   figures from the frozen deck at verify time (shape-generic, never bank
   keys).
3. **The honest report.** 4 passed claims, every figure in-window;
   attribution density 1.0 on the loop arm (35/35 numeric claims trace). The
   one-shot arm's single untraced claim (Bridgeport "$560,000+" — the figure
   is in the window and the deck verbatim; the scorer's canonical form
   strips commas, so `\b560000\b` cannot match the window's "$560,000") is
   journaled in the t1f execution section — a canonical-matching artifact,
   not an absence. Zero untraced figures sit in [passed] position in any
   arm.

## How to verify

```bash
./verify-demo5.sh
```

The strips, in order:

1. the v1 flight exists and terminated (report.md + terminal manifest);
2. every claim in the report is verdict-stamped — a claim with no verdict
   is a silent number;
3. every figure token in the report's passed claims appears in the run's
   accumulated evidence window (the scorer's own tokenizer, loaded from
   score-arms.py — one decider); flagged claims' absences are named by
   their stamps;
3b. the retrieval mechanics on THIS flight are term-ranked — the round-1
   fetch list carries DISTINCT relevance scores (no flat 0.9 ties), and an
   admitted hit carries value-shaped figure runs (3+ digits, not era years,
   not all-zero) that appear in NO round-1 query: the query never names the
   bank's figures, yet the value-bearing document is retrieved and
   admitted;
4. `bars.md` carries the scorer's per-question covered fractions and bar
   legs verbatim — the bars are the scorer's numbers, never hand-typed;
5. the two-arm lift is computed by the same scorer over the same pairs.

Two verify amendments were journaled 2026-08-15, after watching the gate
fail on the real flight (§18.1): strip 3's membership-check bug (`token in
bodies` checked chunk EQUALITY, so no token ever matched — demo4's flight
never fired it because its passed claims carry no figures) and strip 3b's
raw-digit overlap test (era years and generic descriptors legitimately
appear in the round-1 queries AND the value-bearing bodies). Both are fixed
and journaled in the strip header, this README, and the t1f execution
section — never silently.

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
    --out runs/../score-report-t1f.json
cd ../demo/demo5
./verify-demo5.sh
```
