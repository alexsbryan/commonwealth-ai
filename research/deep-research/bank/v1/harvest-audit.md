# Bank v1 — the bounded harvest audit (mint record)

Order `deep-research-t1c` phase 1 authorized ONE bounded, audited, one-time
harvest: fetch only the sources the vendored report (`bank/v1/source-report.pdf`,
"Urban Gentrification Metrics: Four Decades of American City Transformation")
attributes claims to, so each coverage key pins its figure to a real named
source. Every fetch below is logged: what, where, outcome. Raw materials
(extracted text) live at `/tmp/dr-harvest/` (not committed); the exact fetch
times are in the session transcript (`3e6f9516-1434-4357-88c6-c98784f09b8a`).
All fetches 2026-08-14, curl with browser UA (or WebFetch/WebSearch for
locating), no egress beyond the pages below.

## Fetch log

| # | outlet / URL | outcome |
|---|---|---|
| 1 | governing.com — "Gentrification in America" (Mike Maciag, Feb 2015) | OK → `governing.txt` (K1/K8/K13 pin) |
| 2 | Wikipedia API search (income inequality NYC Gini) | OK — located the 0.5469 figure in the states-list article |
| 3 | en.wikipedia.org/wiki/Income_inequality_in_the_United_States | OK → `wiki-income.txt` (Census Gini series; NO metro Gini) |
| 4 | en.wikipedia.org/wiki/List_of_U.S._states_and_territories_by_income_inequality | OK → `wiki-states.txt` (NYC Gini 0.5469) |
| 5 | en.wikipedia.org/wiki/Economic_inequality | OK → `wiki-econ.txt` (no metro Gini; not used) |
| 6 | en.wikipedia.org/wiki/Miami | OK → `wiki-Miami.txt` (no metro Gini; not used) |
| 7 | en.wikipedia.org/wiki/Atlanta | OK → `wiki-Atlanta.txt` (no metro Gini; not used) |
| 8 | en.wikipedia.org/wiki/New_Orleans | OK → `wiki-New_Orleans.txt` (no metro Gini; not used) |
| 9 | smartasset.com canonical URLs (two slug guesses) | 404 ×2 — blocked; study text harvested via #10 |
| 10 | ca.finance.yahoo.com — syndication of the SmartAsset 2024 study (Jaclyn DeJohn, Feb 29 2024) | OK → `smartasset-syn.txt` (K3 pin; the deck cites the canonical smartasset.com study URL, body text from the syndication copy — same study, verbatim) |
| 11 | brookings.edu — "City and metropolitan inequality on the rise, driven by declining incomes" | OK → `brookings.txt` (K4-partial/K15 pin) |
| 12 | constructioncoverage.com — "Cities With the Highest Home Price-to-Income Ratios [2025 Edition]" | OK → `cc-pti.txt` + `cc-index.txt` (K6/K7 pin) |
| 13 | pewresearch.org wrong slug (2018/11/19) | 404 — corrected to #14 |
| 14 | pewresearch.org — "Demographic and economic trends in urban, suburban and rural communities" (2018-05-22) | OK → `pew-dem2.txt` (K11/K16 pin) |
| 15 | pewresearch.org — "Similarities and differences between urban, suburban and rural communities" (What Unites/Divides UDR) | OK → `pew-udr.txt` (checked, no BA+/metro figures; not used) |
| 16 | statista.com (Case-Shiller page) | 302 paywall loop even with `-L` + browser UA — BLOCKED; K5 falls to the exemplar body + #17 verification |
| 17 | tradingeconomics.com — Case-Shiller index page | OK (verification only): 325.78 July 2024, Jan 2000 = 100 base → +225%; not a deck body |
| 18 | coopercenter.org (statchatva.org) — "Since the pandemic, young adults have fueled the revival of small towns and rural areas" (Sep 17 2024) | OK → `coop-ya.txt` (K10/K12 pin; slug now 301s to the Cooper Center home — body preserved at harvest time) |
| 19 | news.stanford.edu — "Gentrification disproportionately affects minorities" (Dec 1 2020) | OK → `stanford.txt` (K14 pin; site now 403s bots — title/date verified from the harvested text) |
| 20 | terry.uga.edu — "Gentrification by the numbers" (Merritt Melancon, Oct 25 2023; Richard Martin) | OK → `uga.txt` (K8 corroboration; site now 403s bots) |
| 21 | fred.stlouisfed.org (Gini series) | curl 000 ×3 (unreachable from this host); WebFetch 403 — BLOCKED; K2's national-Gini corroboration falls to #3; the FRED pin is unresolved (journaled) |
| 22-36 | WebSearches (~15) — locating only: governing report URL, smartasset study, brookings article, construction coverage study, pew reports, cooper center article, stanford news, uga article, statista page, FRED series, Wikipedia API queries | OK (locating; no content retained beyond the fetches above) |

## Post-harvest URL verification (same pages only, HEAD requests, no bodies)

`curl -sIL` re-verification of the deck's canonical URLs: governing 200,
brookings 200, pew 200, constructioncoverage 200 (canonical slug drops the
article's "-the-"), smartasset data-studies/income-inequality-2024 200,
statchatva 301 → Cooper Center home (moved), news.stanford.edu 403 (bot
block — slug form `report/2020/12/01/…` per the article's verified date),
terry.uga.edu 403 (bot block).

## Body provenance classes

- **named-source verbatim** — governing, wikipedia-states, wikipedia-income,
  brookings, construction-coverage, pew-demographic-trends, cooper-center,
  stanford, terry-uga: contiguous verbatim excerpts of the harvested pages
  (excerpt boundaries recorded in this audit's authoring, extracted
  mechanically from the /tmp files).
- **syndicated copy, canonical citation** — smartasset: body text verbatim
  from the Yahoo Canada syndication (#10); the deck cites the canonical
  SmartAsset study URL.
- **exemplar-quote** — source-report.md: the vendored report's own prose with
  figures restored from the order's key list (the PDF's inline figures
  extract as blanks — see `seeds.md` NWCI record); marked so the deck carries
  the K2 conflict material and the single-origin fallback for clauses no
  named source carries.

## Blocked-and-journaled

- FRED (unreachable/403) — the 3,140+ series claim stays exemplar-only.
- Statista (paywall) — K5 (Case-Shiller 325.78, +225%) is exemplar-only in
  the deck; verified against tradingeconomics (325.78, Jan 2000=100).
- smartasset.com canonical (404 at harvest) — body via the syndication copy.
- The metro-level Gini figures (Atlanta/Miami 0.57, New Orleans 0.56) and the
  national 0.40 (2013) exist on NO harvested named source; they are
  exemplar-only (K2's conflict shape is the witness material, see seeds.md).
- K4's SF "+$120k (2014-2016)" and Atlanta/DC "≥18:1" are exemplar-only; the
  Brookings prose names the cities without those ratios.
- K9's "48 of 50" is exemplar-only; no named source carries it (the
  exemplar's own prose leaves the figure blank in the PDF extraction).

## Deck integrity

Deck directory `bank/v1/deck/` (deck.toml + 11 body files), frozen at mint:
sha256 (of the sorted per-file hashes) =
`e63a14499d849301f3f0bbd00024c178609c5899b97d5b6ec0a6ee5b1e88c5ee`.
The recipe: `(cd deck && sha256sum deck.toml $(ls *.md | sort) | sha256sum)`.
