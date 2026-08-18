# T5A_MEASUREMENT_PATH.md — the upstream DRB evaluator inventory

Order deep-research-t5a, phases 1-2 (inventory + instrument validation).
Companion artifact: `overall-derivation/` (the worked derivation, verdicts,
reproducible script). Draft §18.6 pre-registration content is at the end of
`overall-derivation/README.md`, to be appended to pre-registration.md at
phase-3 gating.

BLUF:

- The benchmark's own evaluator is **two pipelines**: RACE (report quality:
  4 dims + overall, judged against a human reference article) and FACT
  (citation accuracy / effective citations). The overall_score that the
  order targets is the **RACE** ratio, averaged per task, ×100.
- Everything needed to wire it to our re-flight outputs is **shipped and
  available**: the driver, the prompts, the frozen per-task criteria
  (100/100 rows), the reference articles, the official judged outputs for
  one system (claude-3-7-sonnet-latest), and — on the leaderboard space —
  per-task judged results for the reference system itself
  (perplexity-Research), which let the 40.46 reference be reproduced
  end-to-end.
- One hard attribution fact: our vendored `leaderboard.csv` is the
  **Gemini-2.5-Pro-era** leaderboard (the space's `data/` tab), NOT the
  GPT-5.5-era one (`data_gpt55/`). Every comparison against 40.46 is a
  cross-judge comparison; the judge-identity caveat rides every number.

---

## 1. Provenance

| Piece | Where | Identity |
|---|---|---|
| Upstream repo (read-only clone) | `/home/alexbryan/dev/deep_research_bench` | github.com/Ayanami0730/deep_research_bench @ `469cce54ea7f6a63c163d3d9fec879cf289ec484` (2026-05-11) |
| Vendored FACT utils (mirror) | `research/deep-research/drb/vendor/utils/` | byte-identical to upstream `utils/` for 10/11 files; `validate.py` differs (see §5.2) |
| Vendored fixture | `research/deep-research/drb/vendor/fixture-validated.jsonl` | byte-identical to upstream `results/fact/claude-3-7-sonnet-latest/validated.jsonl` (sha256 `ddb741cc…`) |
| Leaderboard CSV (vendored) | `research/deep-research/drb/leaderboard.csv` | = space `data/leaderboard.csv` (sha256 `dd184970…`, verified 2026-08-17) — the **Gemini-2.5 Eval tab** |
| Leaderboard space | huggingface.co/spaces/muset-ai/DeepResearch-Bench-Leaderboard | hosts `data/` (Gemini-Eval) + `data_gpt55/` (GPT-5.5) leaderboards, per-model per-task results, aggregator scripts |
| Paper | arXiv 2506.11763 v2 (ar5iv), Appendix E | FACT definition; see `paper-fact-definition.md` |

The clone is outside the repo, read-only, never pushed. The benchmark's
data never leaves this machine. Frozen files (`vendor/`, `query.subset.jsonl`,
`SHA256SUMS`, `leaderboard.csv`) are untouched.

## 2. The evaluator landscape: two pipelines, three judge eras

| Era | RACE judge | FACT judge | Where documented |
|---|---|---|---|
| Paper era (through 2025-07-15) | gemini-2.5-pro | gemini-2.5-flash | README.md:46; website: "we selected Gemini 2.5 Pro Preview as the Judge LLM in our final framework" |
| Pinned commit (2026-05-11) | gpt-5.5 (env `RACE_MODEL`) | gpt-5.4-mini (env `FACT_MODEL`) | README.md:16-29; `utils/api.py:13-20,47-54,74-75`; README.md:192-197 |
| Leaderboard space today | two tabs: `data_gpt55/` tab (GPT-5.5) and `data/` tab (Gemini-2.5 Eval) | same | space `create_leaderboard.py:89-90` (the app entry) caption: "Leaderboard tab — Race judge: GPT-5.5 | Fact-check: GPT-5.4-mini"; "Gemini-2.5 Eval tab — Race judge: gemini-2.5-pro | Fact-check: gemini-2.5-flash"; `tabs/leaderboard_tab_gpt55.py:7` reads `data_gpt55/leaderboard.csv`, `tabs/leaderboard_tab.py:9` reads `data/leaderboard.csv` |

Consequence: **the 40.46 reference (vendored CSV, `data/` tab) was judged
by gemini-2.5-pro (RACE) and gemini-2.5-flash (FACT)** — the paper-era
judges, not the current GPT-5.5. The same system re-judged under GPT-5.5
scores 43.05 (space `data_gpt55/leaderboard.csv`). Judge-era spread is
real (~2.6 pts on overall) and is a named caveat, never collapsed.

## 3. RACE — the overall_score machinery

### 3.1 Driver: `deepresearch_bench_race.py` (527 lines)

- Fixed config: `CRITERIA_FILE = data/criteria_data/criteria.jsonl`,
  `REFERENCE_FILE = data/test_data/cleaned_data/reference.jsonl`
  (:29-30).
- Per task (function `process_single_item`, :58-200):
  1. look up task's criteria, target article (cleaned), reference article
     (cleaned) by prompt (:64-86);
  2. strip weights from criteria (`format_criteria_list`, :33-56) and
     build one prompt containing **both** articles and the criteria
     (:95-104);
  3. judge output must contain the four dims
     `["comprehensiveness","insight","instruction_following","readability"]`
     (:126), else retry (max 10, :111-141);
  4. `calculate_weighted_scores(llm_output_json, criteria_data, language)`
     (:153);
  5. **the derivation**: `overall_score = target_total /
     (target_total + reference_total)` (:155-160); per-dim
     `normalized_dims[dim] = target_weighted_avg / (target_weighted_avg +
     reference_weighted_avg)` (:162-175). Final record: id, prompt, the 4
     normalized dims, overall (:187-195) — **per-criterion judge scores are
     not persisted**.
- Aggregation to a model score: `process_language_data` collects per-task
  records; `main` writes `results/race/<model>/raw_results.jsonl` sorted by
  id (:479-488) and computes the **simple mean over tasks** of each dim and
  of overall (:490-514), written as `race_result.txt` with 4 decimals
  (:509-514). The leaderboard is those means ×100 (see §6).
- Cleaning step: before scoring, the target articles are cleaned
  (citation stripping) by `ArticleCleaner` using the same LLM client
  (:206-224; `utils/clean_article.py` — chunk-based, 50k-token chunks,
  `_CHUNK_TOKEN_STEP = 50_000`, clean_article.py:97; prompt:
  `prompt/clean_prompt.py:28-29` removes citation marks/lists).

### 3.2 Score prompt: `prompt/score_prompt_en.py` (+ `_zh.py` parallel)

- `generate_merged_score_prompt` (:2-79) is what the driver uses: compares
  `<article_1>` (target) and `<article_2>` (reference) per criterion from
  `<criteria_list>`, 0-10 continuous (:36-41), output JSON with per-dim
  arrays of `{criterion, analysis, article_1_score, article_2_score}`
  (:48-74).
- Also present (not used by the driver at this commit):
  `generate_static_score_prompt` (:82-248, fixed 4-dim criteria),
  `point_wise_score_prompt` (:251-321, single-article),
  `vanilla_prompt` (:324-358).

### 3.3 Weighted aggregation: `utils/score_calculator.py`

- `calculate_weighted_scores` (159 lines): dimension weights from
  `criteria_data["dimension_weight"]` (:23); per-dim criterion→weight map
  (:32-35); per criterion, weight lookup with fallbacks — exact,
  case-insensitive, substring, then **mean weight of the dim's criteria**
  (:92-117); per-dim weighted average `dim_target_avg =
  weighted_sum / total_weight` (:136-147); totals `target_total +=
  dim_target_avg * dim_weight` (:149-151).
- Note: the dimension weights cancel in the RACE ratios (both numerator
  and denominator carry the same dim weight), but they scale the totals;
  the per-dim weights within a dimension do not cancel and shape the dim
  averages.

### 3.4 Criteria: `data/criteria_data/criteria.jsonl` — SHIPPED, use theirs

- 100 rows (one per task; ids 1-100 matching `data/prompt_data/query.jsonl`).
  Row shape: `{id, prompt, dimension_weight, criterions}`;
  `criterions[dim]` = list of `{criterion, explanation, weight}` with
  weights summing to 1.0 per dim; `dimension_weight` sums to 1.0 (verified
  for all 10 subset rows; see derivation D-C).
- Generated (when needed) by `utils/generate_criteria.py` from
  `prompt/criteria_prompt_en.py` / `_zh.py`: per task, the dimension-weight
  prompt (criteria_prompt_en.py:8-87; formula line :27 "Total Score =
  Comprehensiveness * Weight + …", weights must sum to 1.0), sampled 5×
  and averaged/normalized/rounded (generate_criteria.py:129-178), then 4
  per-dimension criteria prompts (comp :97+, insight :197+, instruction
  following, readability) (:182-224). **Not needed here — the criteria are
  shipped; regeneration would poison comparability and is prohibited by
  the order.**

### 3.5 Data: tasks, articles, references

- `data/prompt_data/query.jsonl` — 100 tasks: `{id, topic, language, prompt}`
  (50 zh + 50 en).
- `data/test_data/raw_data/<model>.jsonl` — official model outputs
  `{id, prompt, article}`; shipped sample: `claude-3-7-sonnet-latest.jsonl`
  (100 rows).
- `data/test_data/raw_data/reference.jsonl` + `cleaned_data/reference.jsonl`
  — the human reference articles (100 rows; cleaned copy is what the
  driver scores against).
- `data/test_data/cleaned_data/claude-3-7-sonnet-latest.jsonl` — shipped
  cleaned target (the output of the cleaning step).

## 4. FACT — the citation dims machinery

Chain (`run_benchmark.sh` phase 2, :49-85): `extract` →
`deduplicate` → `scrape` (Jina) → `validate` → `stat` →
`results/fact/<model>/fact_result.txt`.

- `utils/extract.py` — statement-URL extraction per article
  (prompt :39-58; citation forms 1-3, ref_idx=0 for inline URLs).
- `utils/deduplicate.py` — dedupe statement-URL pairs.
- `utils/scrape.py` — fetch cited pages (Jina).
- `utils/validate.py` — LLM verdict per claim vs scraped page
  (supported / unsupported / unknown).
- `utils/stat.py` (29 lines) — **the citation dims**: per task, count
  citations with determinate verdicts and supported verdicts (:13-25);
  output `total_citations` (mean per task), `total_valid_citations` (mean
  per task), `valid_rate` (:27-29).
  - Leaderboard mapping (space `utils/rank_leaderboard.py`,
    `parse_fact_result`): `citation_accuracy = valid_rate × 100`,
    `effective_citations = total_valid_citations` (raw count, not ×100).

### 4.1 Vendored amendment (named, frozen)

`vendor/utils/validate.py` differs from upstream `utils/validate.py:39`
(the English validation prompt): the vendored copy adds a fourth verdict
class **'decline'** for statements that refuse/assert absence
(vendored :39-42), a T2b-era fabrication-bracket amendment already
recorded in pre-registration.md. Frozen in `SHA256SUMS`
(`dcdddb1d…`). The FACT numbers we produce carry this amendment; the
official numbers do not. Judge-identity caveat, kept visible.

## 5. Shipped official judged outputs — the instrument-validation fixtures

In the upstream repo `results/`:

| Artifact | Contents | Validated against (derivation) |
|---|---|---|
| `results/race/claude-3-7-sonnet-latest/raw_results.jsonl` | 100 per-task records (4 dims + overall, no errors) | D-A: means reproduce `race_result.txt` exactly |
| `results/race/claude-3-7-sonnet-latest/race_result.txt` | 0.4110 / 0.4051 / 0.4621 / 0.4172 / 0.4218 | D-A |
| `results/fact/claude-3-7-sonnet-latest/{extracted,deduplicated,scraped,validated}.jsonl` | FACT chain outputs; `validated.jsonl` = vendored fixture (byte-identical) | D-E: vendored stat.py reproduces `fact_result.txt` byte-for-byte |
| `results/fact/claude-3-7-sonnet-latest/fact_result.txt` | 28.07 / 24.51 / 0.8731742073387959 | D-B: matches leaderboard row claude-3-7-sonnet-with-search citation dims (87.32 / 24.51) exactly |

On the leaderboard space (fetched 2026-08-17 into `overall-derivation/inputs/`,
sha256 pinned there): per-task + aggregate results for the **reference
system itself**:

| Artifact | Judge era | Used for |
|---|---|---|
| `data/raw_results/perplexity-Research/{raw_results.jsonl,race_result.txt}` | Gemini-Eval tab (gemini-2.5-pro) | D-B: row-39 reproduction; D-C: subset reference 42.1779 |
| `data/fact_results/perplexity-Research/fact_result.txt` | Gemini-Eval tab (gemini-2.5-flash) | D-B: 82.63 / 31.20 reproduction |
| `data_gpt55/raw_results/perplexity-Research/{raw_results.jsonl,race_result.txt}` | GPT-5.5 tab | D-C: GPT-5.5-era subset reference 44.9683 |

Per-task result dirs exist for 43 models in `data/raw_results/` and 11 in
`data_gpt55/raw_results/` (tree listed 2026-08-17).

## 6. The leaderboard production chain

- Space `utils/rank_leaderboard.py` builds `data/leaderboard.csv` from
  `data/raw_results/<model>/race_result.txt` + `data/fact_results/`:
  `parse_race_result` (:16) multiplies the 4-decimal means **×100**
  (:29-37); `parse_fact_result` (:42) maps `valid_rate ×100` →
  `citation_accuracy` and `total_valid_citations` → `effective_citations`
  (:57-59); missing → "-"; sorts by overall then dims (:115); two baidu
  slugs excluded.
  `utils/rank_leaderboard_gpt55.py` mirrors it for `data_gpt55/`.
- Tabs: `tabs/leaderboard_tab_gpt55.py:7` (main tab, `data_gpt55/…`,
  GPT-5.5-judged, 11 models) and `tabs/leaderboard_tab.py:9`
  (Gemini-2.5 Eval tab, `data/…`, 45 models — **our vendored CSV**;
  sha256-verified identical).
- So the CSV's own columns cannot re-derive overall: overall is a mean of
  per-task RACE ratios, not a function of the published dim means (D-D:
  mean-of-4-dims ≠ overall on 45/45 rows; perplexity 40.985 vs 40.46).

## 7. What is NOT shipped (named absences — §18.3)

1. **Per-criterion judge scores**: `raw_results.jsonl` persists only the 5
   normalized numbers per task; the judge's per-criterion 0-10 pairs are
   not shipped, so a per-task overall cannot be re-derived from shipped
   data — only a judge re-run reproduces it (phase 3, seat-gated).
2. **Per-task FACT data**: `fact_result.txt` is aggregate-only for most
   models (the claude fixture ships the full `validated.jsonl`).
3. **Old-space rows**: leaderboard rows grok/sonar×3/claude-3-7-sonnet-
   with-search/gpt-4o are not in the new space's `data/raw_results/`; the
   old space (Ayanami0730/DeepResearch-Leaderboard) is auth-gated. The
   claude row's RACE numbers (36.63) do NOT match the shipped
   `race_result.txt` (42.18) — same underlying FACT articles, different
   RACE run; the discrepancy is named (D-F), never reconciled by
   assumption.
4. **Cleaned data for the subset tasks**: cleaned target articles exist
   only for claude-3-7-sonnet-latest; our reports will need the cleaning
   step (or a named decision to skip it) at phase 3 — cleaner identity is
   a caveat.

## 8. Phase-3 ready subset mapping (our frozen 10)

`query.subset.jsonl` ids [56, 58, 59, 62, 65, 69, 78, 83, 90, 95], all
English. Verified: criteria.jsonl rows exist for all 10 prompts
(dimension_weight sums to 1.0 per row); reference.jsonl articles exist for
all 10 prompts. The RACE recipe for phase 3: for each of the 10 tasks —
prompt + our re-flight report (article_1) + shipped reference (article_2)
+ shipped per-task criteria → merged score prompt (en) → judge (122B at
:9741, seat-executed load) → `calculate_weighted_scores` + ratio (the
formula of deepresearch_bench_race.py:155-175) → per-task overall →
mean ×100. Same recipe, same criteria, same references, same tasks as the
reference system. The recipe is executed by `overall-derivation/score_race.py`
(imports the recipe from the pinned clone, vendored client/calculator/
extractor; `--dry-run` validates all linkages with zero judge calls;
judge guard refuses unless the pinned 122B is served — §18.3).

**A/B arm inputs (decided flight, operator resolve 2026-08-17)** — pinned in
`overall-derivation/inputs/`: `perplexity-subset-articles.jsonl`
(sha256 `b1ce5783…`, the 10 official subset rows {id, prompt, article}
from the space's `data/raw_data/perplexity-Research.jsonl`; prompts
matched to `query.subset.jsonl`, NONE mismatched) + `perplexity-raw_data.jsonl`
(sha256 `0a3b8558…`, the full 100-row provenance; the `data_gpt55` copy is
byte-identical, so the articles are era-independent). Re-judging these 10
articles with our 122B yields the same-judge same-task judge-offset
measurement (work item 2b) against 42.1779.

## 9. Judge-identity table — every reference number, labeled

| Number | What | Judge (RACE) | Task set | Source |
|---|---|---|---|---|
| 40.46 | perplexity overall (the order's reference) | gemini-2.5-pro | 100 tasks | vendored leaderboard.csv row 39 = space `data/leaderboard.csv` |
| 39.10/35.65/46.11/43.08 | dims of the same row | gemini-2.5-pro | 100 | same |
| 82.63 / 31.20 | citation_accuracy / effective_citations | gemini-2.5-flash | 100 | same row; reproduced from `data/fact_results/perplexity-Research/fact_result.txt` |
| 40.4581 | recomputed from official per-task data | gemini-2.5-pro | 100 | `data/raw_results/perplexity-Research/raw_results.jsonl` (exact match) |
| **42.1779** | **perplexity on OUR 10 subset tasks (like-for-like reference)** | gemini-2.5-pro | 10 | same per-task data, subset ids |
| 43.05 | perplexity overall, GPT-5.5 era | GPT-5.5 | 100 | space `data_gpt55/leaderboard.csv` |
| **44.9683** | perplexity on OUR 10 subset tasks, GPT-5.5 era | GPT-5.5 | 10 | `data_gpt55/raw_results/perplexity-Research/raw_results.jsonl` |
| 36.63 | claude-3-7-sonnet-with-search overall | gemini-2.5-pro (old space, pre-migration) | 100 | vendored leaderboard.csv; NOT reproducible from pinned-commit artifacts (shipped run = 42.18) |
| 87.32 / 24.51 | claude row citation dims | gemini-2.5-flash | 100 | reproduced exactly from shipped `fact_result.txt` + vendored fixture |

All numbers in this table are verified in `overall-derivation/` with the
reproducible script `verify_derivation.py` (exit 0 = all assertions held).
