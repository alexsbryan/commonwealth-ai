# T5A overall-derivation — the worked derivation (phases 1-2)

Order deep-research-t5a. Companion to `../T5A_MEASUREMENT_PATH.md` (the
upstream inventory). This directory holds the instrument validation: the
overall_score derivation of the benchmark's own evaluator, reproduced on
official artifacts with every number shown. Reproduce at any time with
`python3 verify_derivation.py` (exit 0 = all assertions held; run it before
the phase-3 flights, §18.4 — the instrument must be validated before the
result).

## Verdicts (all four verdicts, §5)

| ID | What was validated | Verdict |
|---|---|---|
| D-A | Aggregation layer on repo-shipped race data (claude-3-7-sonnet-latest) | **PASSED** — recomputed means equal `race_result.txt` to 4 decimals |
| D-B | Leaderboard row 39 (perplexity-Research, the order's reference) end-to-end | **PASSED** — all 7 numbers reproduced from official per-task artifacts |
| D-C | Like-for-like references: perplexity on OUR 10 frozen subset tasks | **PASSED** — 42.1779 (gemini-2.5-pro era) / 44.9683 (GPT-5.5 era), computed |
| D-D | Structural: leaderboard overall is not a function of its dim columns | **PASSED** — 45/45 rows differ from mean-of-4-dims (structural, expected) |
| D-E | FACT stats machinery on the vendored fixture | **PASSED** — vendored stat.py output byte-identical to official `fact_result.txt` |
| D-F | Named non-reproductions (claude row RACE 36.63 vs shipped 42.18) | **PASSED** (as named discrepancy) — provenance chain documented; judge-offset measurement = phase 3, seat-gated |
| D-AB | A/B-arm readiness (official subset articles fetched + pinned) | **PASSED** — 10 rows, sha256 `b1ce5783…`, prompts match the frozen `query.subset.jsonl`, articles substantive |

No verdict is "could-not-judge" or "never-ran": every number below was
computed in this session (2026-08-17) from the cited artifact.

## D-A — aggregation layer: shipped race data → official summary

Input: upstream repo `results/race/claude-3-7-sonnet-latest/raw_results.jsonl`
(100 per-task records, 0 errors; each record = the 4 normalized dims +
overall from `deepresearch_bench_race.py:187-195`).

Per the driver's summary block (`deepresearch_bench_race.py:490-514`),
each model number is the simple mean over tasks. Recomputed:

| Dimension | mean(raw_results.jsonl) | official race_result.txt | match |
|---|---|---|---|
| comprehensiveness | 0.4110 | 0.4110 | exact |
| insight | 0.4051 | 0.4051 | exact |
| instruction_following | 0.4621 | 0.4621 | exact |
| readability | 0.4172 | 0.4172 | exact |
| overall_score | 0.4218 | 0.4218 | exact |

The aggregation layer (mean of per-task values) is proven on official
artifacts. The per-task values themselves are judge output (not shipped
below that level — see D-F).

## D-B — the reference row, end-to-end (40.46 and friends)

Input: leaderboard space `data/raw_results/perplexity-Research/raw_results.jsonl`
(100 per-task records; sha256 = its LFS oid `1141aa12…`, verified) +
`data/fact_results/perplexity-Research/fact_result.txt`; output compared
against the vendored `leaderboard.csv` row 39 (sha256 `dd184970…`,
byte-identical to the space's `data/leaderboard.csv` — the Gemini-2.5 Eval
tab, judge gemini-2.5-pro / gemini-2.5-flash per space `app.py`).

RACE (mean of per-task ratios ×100, per `utils/rank_leaderboard.py`
`parse_race_result`):

| Number | worked | leaderboard row 39 |
|---|---|---|
| comprehensiveness | mean ×100 = 39.0988 | 39.10 |
| insight | mean ×100 = 35.6508 | 35.65 |
| instruction_following | mean ×100 = 46.1125 | 46.11 |
| readability | mean ×100 = 43.0778 | 43.08 |
| overall_score | mean ×100 = 40.4581 | 40.46 |

FACT (`valid_rate` ×100 → citation_accuracy; `total_valid_citations` →
effective_citations, per `parse_fact_result`):

| Number | worked | leaderboard row 39 |
|---|---|---|
| citation_accuracy | 0.826271186440678 ×100 = 82.6271 | 82.63 |
| effective_citations | 31.2 | 31.20 |

**The complete reference row is reproduced from official per-task
artifacts.** The formula chain that produced it, cited:
`deepresearch_bench_race.py:155-160` (overall = target_total /
(target_total + reference_total)) → `:490-514` (task means) →
`utils/rank_leaderboard.py` (`parse_race_result`: ×100; `parse_fact_result`:
valid_rate ×100, total_valid_citations raw).

## D-C — like-for-like references for the phase-3 flights

The leaderboard's 40.46 is a mean over 100 tasks. Our flights cover the 10
frozen subset tasks (ids 56, 58, 59, 62, 65, 69, 78, 83, 90, 95 — all
English). The same official per-task data, subset to those ids:

| Metric | gemini-2.5-pro era (space `data/`) | GPT-5.5 era (space `data_gpt55/`) |
|---|---|---|
| comprehensiveness | 41.4520 | 44.5813 |
| insight | 38.4296 | 43.6290 |
| instruction_following | 47.1045 | 46.3320 |
| readability | 43.6024 | 47.0024 |
| **overall_score** | **42.1779** | **44.9683** |
| (full-100 overall, cross-check) | 40.4581 ✓ = 40.46 | 43.0516 ✓ = 43.05 (space `data_gpt55/leaderboard.csv`) |

Per-task subset rows (gemini era) — the per-task reference for the phase-3
comparison:

```
id | overall | comp | insight | inst | read
56 | 0.4417 | 0.4045 | 0.4284 | 0.5000 | 0.4597
58 | 0.4394 | 0.4367 | 0.4138 | 0.4701 | 0.4748
59 | 0.3880 | 0.3975 | 0.3212 | 0.4695 | 0.3991
62 | 0.4237 | 0.4254 | 0.3955 | 0.4695 | 0.4366
65 | 0.4002 | 0.3733 | 0.3448 | 0.4592 | 0.4339
69 | 0.4211 | 0.4163 | 0.3800 | 0.4764 | 0.4395
78 | 0.4303 | 0.4155 | 0.3920 | 0.4764 | 0.4402
83 | 0.4018 | 0.4196 | 0.3640 | 0.4262 | 0.3710
90 | 0.4414 | 0.4341 | 0.4124 | 0.4904 | 0.4531
95 | 0.4302 | 0.4223 | 0.3909 | 0.4726 | 0.4524
```

Same judge-era spread appears in both task sets (gpt55 ≈ gemini + ~2.6 on
full-100; +2.8 on subset) — the judge-era caveat is quantitative, and the
subset reference resolves the task-set confound.

Criteria readiness (shipped `data/criteria_data/criteria.jsonl`, the 10
subset rows): `dimension_weight` sums to 1.0 in every row AND every
per-dim criterion-weight list sums to 1.0 in every row — none fail. The
verifier asserts both structurally (D-C readiness block) on every run.
Per-row dimension weights for the record: id 56
{readability 0.13, insight 0.41, comprehensiveness 0.28, instruction_following 0.18},
58 {0.11, 0.4, 0.3, 0.19}, 59 {0.17, 0.35, 0.31, 0.17}, 62 {0.15, 0.39, 0.3, 0.16},
65 {0.13, 0.38, 0.21, 0.28}, 69 {0.15, 0.35, 0.31, 0.19}, 78 {0.2, 0.28, 0.29, 0.23},
83 {0.15, 0.23, 0.34, 0.28}, 90 {0.13, 0.38, 0.3, 0.19}, 95 {0.15, 0.34, 0.28, 0.23}.

## D-D — structural: overall is not a function of the CSV's dim columns

The leaderboard overall is a **mean of per-task ratios of weighted totals**
(score_calculator.py totals → deepresearch_bench_race.py:155-160), while
the dim columns are means of per-dim ratios (:163-175). These are different
functions; the aggregate columns cannot re-derive overall. Verified:
`|mean(4 dims) − overall| > 0.01` on **45/45** rows (perplexity:
40.985 vs 40.46). Any attempt to "recompute overall from the CSV" is
therefore not the benchmark's formula — the derivation must run per-task
from judge output (which is exactly what the phase-3 flights do).

## D-E — FACT stats machinery on the vendored fixture

Vendored `vendor/utils/stat.py` run on the vendored
`vendor/fixture-validated.jsonl` (the official claude-3-7-sonnet-latest
validated output, byte-identical to upstream
`results/fact/claude-3-7-sonnet-latest/validated.jsonl`):

```
total_citations: 28.07
total_valid_citations: 24.51
valid_rate: 0.8731742073387959
```

Byte-identical to the official `fact_result.txt`. These same two numbers
are the leaderboard row claude-3-7-sonnet-with-search's citation dims
(87.32 = 0.873174×100, 24.51) — the only leaderboard row whose citation
dims reproduce from pinned-commit artifacts. (Note the FACT validation ran
through upstream `validate.py`; our vendored copy carries the T2b 'decline'
amendment — see T5A_MEASUREMENT_PATH.md §4.1. It is not re-run in this
order; FACT numbers stay old-instrument.)

## D-F — named non-reproductions (absence reported, never substituted)

1. **claude row RACE dims**: leaderboard row claude-3-7-sonnet-with-search
   overall 36.63 ≠ the pinned-commit shipped run's 42.18 (0.4218×100).
   The citation dims match exactly (87.32/24.51) — same underlying
   articles, a different RACE run. The row's provenance is the old space
   (pre-migration, gemini-2.5-pro era; README.md:46), which is
   auth-gated — the discrepancy is named and kept, never reconciled by
   assumption.
2. **Per-criterion judge scores are not shipped** — `raw_results.jsonl`
   persists only the 5 normalized numbers per task
   (deepresearch_bench_race.py:187-195). No shipped artifact can re-derive
   a per-task overall without re-running a judge.
3. **Judge-offset measurement** (order work item 2b): measuring our 122B
   judge against the official judge on the same articles requires a judge
   run — phase 3, seat-gated (the 122B load routes through the seat; the
   official articles to re-judge are fetchable, see the draft
   pre-registration below).
4. **Cleaning identity**: the RACE driver cleans target articles with an
   LLM (`ArticleCleaner`, chunk-based, clean_article.py:97). Our reports
   will either go through our local cleaner or skip cleaning — the choice
   is a named caveat at flight time, not a silent substitution.

## D-AB — A/B-arm readiness (decided flight, operator resolve 2026-08-17)

The A/B arm re-judges perplexity's 10 official subset articles with our
122B — the same-judge same-task judge-offset measurement (work item 2b).
The inputs are fetched and pinned so the arm can fly the moment the gate
opens:

- `inputs/perplexity-subset-articles.jsonl` — the 10 rows {id, prompt,
  article} extracted from the official 100-row `data/raw_data/
  perplexity-Research.jsonl` (space). sha256 `b1ce57831916bd0e…`; ids
  [56, 58, 59, 62, 65, 69, 78, 83, 90, 95]; every prompt matches the
  frozen `query.subset.jsonl` (linkage verified, NONE mismatched);
  articles 11.6k–24.8k chars (substantive, uncleaned — cleaning choice
  recorded at flight).
- `inputs/perplexity-raw_data.jsonl` — the full 100-row source file
  (provenance), sha256 `0a3b8558…`. The GPT-5.5-era copy of the same file
  is byte-identical (same sha256, verified 2026-08-17) and therefore not
  stored — the subset articles are era-independent, so the A/B needs no
  era split.

The verifier asserts all of this (D-AB block) on every run.

## Draft §18.6 pre-registration entries (for pre-registration.md at phase-3 gating)

DRAFT — **APPROVED AS DRAFTED** by operator resolve 2026-08-17 (seat relay,
M0), with the comparison-targets item amended per the resolve (item 5:
the recommended additional flight is now a DECIDED flight). **APPENDED to
`adversarial/pre-registration.md` as the T5a declaration section on
2026-08-17, BEFORE any phase-3 scoring run (§18.6), as a separate section
from t4a's concurrent entries.** The execution record lands there at flight
time; this README stays the worked derivation.

1. **Judge pin**: local daemon on :9741 (the seat's proven 122B load
   sequence), model pin recorded in the execution record at flight time.
   The judge-identity caveat rides every number: ours is a different model
   from the official judges (gemini-2.5-pro / GPT-5.5 for RACE;
   gemini-2.5-flash / GPT-5.4-mini for FACT).
2. **Criteria source**: the shipped frozen `data/criteria_data/criteria.jsonl`
   rows for ids [56, 58, 59, 62, 65, 69, 78, 83, 90, 95] (upstream clone @
   469cce54). Never regenerated. (Shipped — verified, weights sum to 1.0.)
3. **Reference articles**: the shipped cleaned reference articles for the
   same 10 prompts (upstream `data/test_data/cleaned_data/reference.jsonl`).
4. **Derivation formula**: `calculate_weighted_scores` aggregation
   (vendored `vendor/utils/score_calculator.py`) + per-task
   `overall = target_total/(target_total+reference_total)` and per-dim
   ratios (deepresearch_bench_race.py:155-175) + task means ×100 — the
   same recipe as the reference numbers (D-A, D-B), executed by our own
   scorer script (no upstream driver run: it would call the official API
   defaults, and the formula is the vendored code, which is frozen).
5. **Comparison targets** (reported together, each labeled):
   - our 10-task mean vs **42.1779** (perplexity, gemini-2.5-pro era,
     same 10 tasks) — like-for-like task set;
   - vs **44.9683** (perplexity, GPT-5.5 era, same 10 tasks);
   - vs **40.46** (the order's literal reference, 100-task) — with the
     task-set + judge-era caveats attached;
   - **DECIDED flight** (operator resolve 2026-08-17; ~10 extra judge
     calls): re-judge perplexity's 10 official subset articles with our
     122B → same-judge same-task A/B — this IS the judge-offset
     measurement (work item 2b) and the cleanest "beat the reference"
     comparison. Inputs pinned: `inputs/perplexity-subset-articles.jsonl`
     (sha256 `b1ce5783…`, 10 rows, prompts matched to the frozen
     `query.subset.jsonl`).
6. **Caveats**: judge identity; cleaning identity (decision recorded at
   flight); FACT numbers remain old-instrument (never re-judged here);
   the vendored validate.py 'decline' amendment is not exercised by this
   order (FACT not re-run); the 10-task mean is a subset statistic — the
   subset reference resolves the task-set confound, the judge confound is
   named, never collapsed.
7. **Execution record** (appended at landing): per-task judge calls,
   model pin, date, raw scores, derived numbers, exit codes of
   `verify_derivation.py` before the flights.

## Inputs (official artifacts fetched 2026-08-17, sha256)

| File | sha256 | Source |
|---|---|---|
| perplexity-raw_results.jsonl | 1141aa12… (== its LFS oid) | space `data/raw_results/perplexity-Research/raw_results.jsonl` |
| perplexity-race_result.txt | b0c72cc8… | same dir |
| perplexity-fact_result.txt | f9b9475a… | space `data/fact_results/perplexity-Research/fact_result.txt` |
| perplexity-gpt55-raw_results.jsonl | 4b3e2179… | space `data_gpt55/raw_results/perplexity-Research/raw_results.jsonl` |
| perplexity-gpt55-race_result.txt | 8d42846b… | same dir |
| perplexity-subset-articles.jsonl | b1ce5783… | space `data/raw_data/perplexity-Research.jsonl`, 10 subset rows (A/B arm input) |
| perplexity-raw_data.jsonl | 0a3b8558… | space `data/raw_data/perplexity-Research.jsonl` full 100 rows (provenance; `data_gpt55` copy byte-identical, not stored) |

Everything else is read live from the upstream clone (read-only) and the
frozen vendored files.
