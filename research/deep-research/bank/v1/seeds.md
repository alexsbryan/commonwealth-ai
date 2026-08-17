# Bank v1 — the report-class seed question and coverage keys

**Bank v1 mint, 2026-08-14, order `deep-research-t1c`.**
The report-class probe: one question cut from the operator's exemplar report
("Urban Gentrification Metrics: Four Decades of American City
Transformation", vendored at `bank/v1/source-report.pdf`), with 16 coverage
keys. Where bank v0's 12 seeds measure the compass against deal-rumor-scale
questions, this seed measures the loop against the class of question the
product actually serves: four decades, ~30 metros, six measure families,
dense numeric claims, cross-entity synthesis.

## NWCI record (the not-written-by-consulting-output test)

The question and all sixteen keys below were authored from the operator's
exemplar (the PDF prose + the order's key list) and author knowledge ALONE —
before any arm run, before any answer, before any retrieval. The bounded
harvest (harvest-audit.md) ran AFTER the keys were drafted, to pin each
figure to its named source; the pinning is recorded per key below, and the
pinning never changed a key's wording — it changed only which clauses the
deck can support (the arbiter journal below names every deck-unsupported
clause).

**The NWCI test applied:** every key is authorable from the exemplar text
alone (a report-class question whose keys need system output to write is a
kill-report, not a workaround). All sixteen pass.

**Evidence-arbiter rule (bank v0's, inherited):** the round's evidence is the
arbiter. A key whose exact figure is corrected by the evidence is satisfied
when the CORRECTED fact is named and supported (recorded as an evidence
correction). A key is a gap only when the fact itself — under either the
hypothesis or the corrected form — is not named/supported.

## The question

"How did American cities change across four decades (1980-2024):
gentrification, inequality, affordability, and displacement — every claim
cited?"

## The sixteen coverage keys (verbatim from the order, operator-ratified at approval)

1. Portland 58.1% / DC 51.9% / Minneapolis 50.6% / Seattle 50% of eligible
   tracts gentrified (the four most intensive cities).
2. NYC Gini 0.5469 (2013) vs national 0.40; Atlanta/Miami 0.57; New Orleans
   0.56 — AND the conflict shape: "NYC leads at 0.5469" cannot pass while
   0.57s sit in the same report; conflicting figures across sources must
   render could-not-judge or a named discrepancy, never a synthesized pass
   (the exemplar's own failure mode).
3. 80/20 ratio: New Orleans 7.87:1; Boston 7.81:1 ($172,476 vs $22,095).
4. 95/20 ratio: Atlanta and DC ≥18:1; SF top incomes +$120k (2014-2016).
5. Case-Shiller 325.78 (July 2024), +225% since January 2000.
6. Home prices +177% vs median household income +92% since 2000 (compound
   claim — both figures must trace to distinct sources).
7. California price-to-income 9.6-12.2 vs national average 4.7.
8. Gentrification rate doubled after 2000 vs the 1990s; ~20% of
   lower-income neighborhoods in major cities affected.
9. 48 of 50 largest metros: worsening economic mobility for low-income
   families; Houston the sole improvement (+1.1%).
10. Manufacturing 19.5M jobs (1979) → <11.5M (pandemic); finance and
    professional services 12M → 32M.
11. White share of urban cores −7pp since 2000; 53% of urban counties
    majority nonwhite.
12. ~80% of under-45 population growth since 1980 in metros >1M.
13. Gentrifying neighborhoods: poverty −0.7pp; non-gentrifying low-income
    neighborhoods: +6.7pp.
14. Residents of historically Black gentrifying neighborhoods move to poorer
    non-gentrifying areas (displacement pattern).
15. 57 of 100 largest metros: inequality significantly higher in 2014 than
    2007 (post-2000 acceleration).
16. Educational gentrification: 35% of urban residents BA+ vs 31% suburban
    vs 19% rural.

## Per-key pinning + arbiter journal (deck support at mint)

| key | named-source pin (deck hit) | second origin | clauses the deck does NOT support (arbiter note) |
|---|---|---|---|
| K1 | governing (the four cities + rates, verbatim) | exemplar body | — |
| K2 | wikipedia-states (NYC 0.5469) | exemplar body | national 0.40 (2013), Atlanta/Miami 0.57, New Orleans 0.56: exemplar-only — no named source carries them. The CONFLICT shape is the witness material: the exemplar body carries both 0.5469-leads and the 0.57s, so a report asserting "NYC leads" with the 0.57s in the window trips the conflict clause (see pre-registration) |
| K3 | smartasset (7.87 / 7.81 / $172,476 / $22,095, verbatim) | exemplar body | — |
| K4 | brookings (95/20 definition, 9.3/9.7/11.8, the city list naming Atlanta and DC) | exemplar body | Atlanta/DC "≥18:1" and SF "+$120k (2014-2016)": exemplar-only. Deck-supported form: Atlanta and DC rank among the high-95/20 cities (no ratio given) — a claim asserting 18:1 for either is evidence-unsupported |
| K5 | none (Statista paywalled) | — | exemplar-only in the deck; tradingeconomics verification (325.78, Jan 2000=100) journaled in the audit |
| K6 | construction-coverage (92% $41,990→$80,610; 177% $122,775→$339,937, verbatim — distinct figures, one study) | exemplar body | — |
| K7 | construction-coverage (9.6-12.2 verbatim; national 4.6 — the study's own figure) | exemplar body | the national "4.7" clause: exemplar-only (the named source says 4.6) — evidence correction surfaces the deck-supported 4.6 |
| K8 | governing (20% vs 9%, "more than double") + terry-uga ("very large across-the-board increase in the 2000s") | exemplar body | — |
| K9 | none | — | exemplar-only; no named source carries "48 of 50" (exemplar's own prose blanks the count in the PDF extraction) — expected not to clear |
| K10 | cooper-center (19.5M 1979 → <11.5M; 12M → 32M, verbatim) | exemplar body | — |
| K11 | pew (urban core −7pp / suburbs −8pp; 53% majority-nonwhite, verbatim) | exemplar body | — |
| K12 | cooper-center ("nearly 80 percent of the country's growth in its under 45 population... metros with over a million residents", verbatim) | exemplar body | — |
| K13 | governing (−0.7 / +6.7 percentage-point change table, verbatim) | exemplar body | — |
| K14 | stanford (Hwang, verbatim: "poor residents moving from historically Black gentrifying neighborhoods tend to move to poorer non-gentrifying neighborhoods") | exemplar body | — |
| K15 | brookings ("57 have a significantly higher 95/20 ratio in 2014 than in 2007", verbatim) + brookings 8.5→9.3 | exemplar body | the exemplar's "SSTI" attribution is corrected to Brookings (arbiter note) |
| K16 | pew (35% / 31% / 19%, verbatim) | exemplar body | — |

**Two-origin rule:** every key's deck support is 2-origin wherever a named
source carries the figure (named source + exemplar). K5, K9, and the
exemplar-only clauses of K2/K4/K7 are single-origin — the loop's
corroboration floor caps those claims at could-not-judge by construction,
which is a MEASURED honesty result of this bank, not a scoring bypass: P4
coverage is scored per the v0 README semantics (named + evidence-supported
over the answer+evidence artifacts), and the floor's caps are reported
separately (honesty never blended into coverage — DEEP_RESEARCH.md P2).

## Frozen

Seeds + deck frozen at mint: deck sha256
`e63a14499d849301f3f0bbd00024c178609c5899b97d5b6ec0a6ee5b1e88c5ee`.
Run, never edit.
