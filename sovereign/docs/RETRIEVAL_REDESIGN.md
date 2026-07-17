# Retrieval redesign — demand-set retrieval over a repaired floor

**Date:** 2026-07-16
**Status:** design + Phase 0 landing. Grounded in the 2026-07-16 ci-bench
baseline (`541711ec qa bench baseline`), two measured probes (ANN recall,
FTS latency) run against the live indexes, per-question forensics on every
failing bank item, and a three-lens survey of 2024–2026 retrieval research.
Prior internal experiments are treated as evidence, not folklore — every
claim below either cites a file in this repo or an external paper.

← companions: `archive/RERANK_EXPERIMENT.md` (the dedup/atlas-weight
ablations), `../bench/wikipedia_learn/V36_FINDINGS.md` (why filter-layer
protection collapses into monoculture), `retrieval-pipeline.md` (generated
step/knob registry), `TIERED_RETRIEVAL.md`.

---

## 1. What the baseline actually says

`scripts/sovereign-ci-bench.sh` retrieval lanes, 2026-07-16, limit=30
(Qwen3-Embedding-0.6B, LanceDB IVF-PQ + Tantivy inverted, RRF hybrid):

| Lane | sources | facts | weak categories |
|---|---|---|---|
| wikipedia/questions | **66%** (38/58) | 85% | multi_article_synthesis 9/15 · causal 5/9 · comparative 7/11 · contested facts **16/27** |
| sep/questions | 83% (55/66) | 93% | position_summary, concept_distinction title-coverage 75% |
| sep/summarize | 100% | **66%** | comprehensive_summary |
| sep/summarize_obscure | 100% | 77% | comprehensive_summary |
| wikipedia/summarize | — | 71% | comprehensive_summary |
| newsworthy + factual_recall | 100% | 100% | — |

Single-best-match retrieval is a solved problem on this stack. Every
failure is a **set-composition failure** — questions whose answer needs
several articles, several stances, or several sections of one article:

- **Missing-entity sub-query** (6/8 failing wiki questions): the question
  names entities (Einstein, Newton, Watt, Szilard) whose articles never
  get their own retrieval probe; the single query embedding lands on one
  topic and K=30 is spent around it. `compare_einstein_newton_gravity`
  retrieves *neither* person's article.
- **Section skew** (dominant SEP-summarize failure): the target article is
  retrieved (sources 1/1, often 11–15 of 30 chunks) but the pool clusters
  in a few sections; missed facts verifiably live in un-retrieved sections
  (e.g. `harm` appears 167× in mill-moral-political.md — zero retrieved
  chunks contain it).
- **Stance blindness** (contested lane): `contested_atomic_bombings_morality`
  spends 17/30 chunks on the right article and still scores 2/6 facts —
  the pool covers the event, not the debate.
- **Scorer/bank artifacts** (~10 missed facts): `versus`,
  `omnibenevolence`, `gratuitous`, `guarantor` never appear in the target
  articles; three expected titles have no title-normalize-equal article
  (`Decolonization` vs "Decolonisation of Africa"). These are ruler
  problems, tracked separately in §8 — fixing them is measurement repair,
  not product improvement, and must never be conflated with lift.

## 2. Two integrity findings (measured 2026-07-16)

**F1 — the ANN leg runs at ~60% recall.** IVF-PQ queries used
`nprobes(50)` with **no refine_factor** (`corpus-engine/src/index/search.rs`).
Measured with stored-vector queries against a near-exact reference
(nprobes=400, refine=30):

| corpus | recall@30, production | recall@30, +refine_factor=30 | latency |
|---|---|---|---|
| wikipedia (1.9M) | **0.585** (min 0.33) | 0.982 | 30ms → 47ms |
| sep (188k) | **0.657** (min 0.37) | 0.985 | 5ms → 31ms |

At 1024d/64-subvector PQ, quantization error alone silently discards
~40% of the true top-30 before fusion, dedup, rerank, atlas, or any
pipeline step ever sees them. Every retrieval experiment to date —
including the negative results on wider pools and one-hop expansion — ran
on this floor. **Fixed in Phase 0** (`ann_params()` in search.rs;
`SOVEREIGN_ANN_NPROBES` / `SOVEREIGN_ANN_REFINE_FACTOR`, defaults 50/30).

**F2 — the CI retrieval lane measures a parallel code path, not the
product.** `bench all` (no `--synth`) shells into `eval run`, which calls
`CorpusIndex::search_with_rerank` directly with rerank off and atlases
empty — i.e. a bare hybrid search. The 19-step production pipeline
(`runtime/retrieval_pipeline.rs`: entity boost, atlas grounding, RAPTOR,
noise floor, caps/reserves, expansion) is exercised only under `--synth`.
The lane also ran a wide 200-row hybrid search per question whose results
were discarded on the no-atlas path (`eval_cmd/runner.rs:1394`; guarded in
Phase 0), and feeds the threads bank to the questions parser
(`bench_cmd/discover.rs:166` classifies on `[bank]` presence alone — the
"1 stale" in every run). Consequence: CI cannot see a regression **or an
improvement** in most of the retrieval stack. §7 makes bench-vs-prod
parity a deliverable, not a wish.

Latency ledger (wikipedia, warm): embed ~30ms, ANN ~30–47ms, FTS ~100ms —
yet the lane observes 1.8–3.2s per search. True leaf work is ~10% of the
observed wall. The gap (cold posting lists, the wasted wide search,
per-call `count_rows`/`list_indices`, hybrid materialization) is
unattributed pending per-leg timers (§7). **The throughput budget for
smarter retrieval is funded by waste already being spent.**

## 3. Component model

A grounded-QA retrieval system, from first principles:

```
Q1 demand modeling      what does this question need? facets, entities,
                        stances, sections — the *coverage contract*
Q2 candidate generation high-recall lanes: dense, sparse, graph, title
Q3 precision scoring    trustworthy per-candidate relevance at depth
Q4 set selection        choose the evidence SET under a budget to
                        maximize demand coverage
Q5 context assembly     order, dedupe, format, cite
Q6 verify & repair      per-facet answerability → one bounded retry
```

Mapping today's stack onto it:

| stage | today | state |
|---|---|---|
| Q1 | one query embedding; `entity_boost` (prod path only); `query_decomp`/`title_expand` exist but OFF after failed A/Bs | **missing in measured path; vestigial in prod** |
| Q2 | hybrid vec+FTS (recall repaired by F1); atlas/link-graph lanes exist, inactive or unseeded | partial |
| Q3 | jina-reranker-v3 cross-encoder, env-gated off (~1.7s @ k=50 sequential) | experimental |
| Q4 | **does not exist.** Its job is emulated by a pile of interacting heuristics: noise floor → per-article cap(10) → per-entity reserve(3) → truncate(20) → dominant-source expansion | **the hole** |
| Q5 | `KQ_MERGED_LIMIT=20`, 8000-char budget, no principled ordering | adequate |
| Q6 | gap check (always-on, KQ), grounding gate downstream | present, not facet-aware |

The V33/V36 failure (`V36_FINDINGS.md`) is the definitive evidence for
the Q4 hole: diversity today is an *accidental by-product* of a lexical
noise-floor filter, so any attempt to protect fanned-out articles through
that filter collapsed the pool into single-article monoculture (12/14
chunks from one article). Per-chunk relevance filters cannot express a
set-level objective. All three failing categories (§1) are set-level
objectives.

## 4. Target architecture — demand-set retrieval

The redesign adds the two missing stages as first-class, data-declared
pipeline steps and demotes the heuristic pile to legacy knobs that can be
A/B'd off once selection lands. Everything is env-gated and reversible;
step order stays pinned by the golden tests.

```
            ┌────────────────────────────────────────────────┐
            │ Q1 demand_plan  (one fast-slot call, llguidance │
            │ JSON; pure-Rust fallback)                       │
            │  → sub_queries[≤5], entities[], stance_contrast,│
            │    section_coverage, original query always kept │
            └───────────────┬────────────────────────────────┘
        ┌───────────┬───────┴────┬─────────────┬──────────────┐
        ▼           ▼            ▼             ▼              ▼
   hybrid(q0)  hybrid(sq1..n)  title/alias   atom-PPR lane   stance lanes
   (refined    (parallel,      lane (Tantivy (seed atoms via (Position/
   ANN)        concurrent)     title_idx     atoms_ann →     Opposition
                               exact)        PPR over        atoms →
                                             edges.csr →     scoped fetch)
                                             chunk mass)
        └───────────┴────────────┴─────────────┴──────────────┘
                                ▼
            ┌────────────────────────────────────────────────┐
            │ Q3 precision (optional, pool ≤200): local       │
            │ reranker OR fusion scores when reranker absent  │
            └───────────────┬────────────────────────────────┘
                            ▼
            ┌────────────────────────────────────────────────┐
            │ Q4 coverage_select: greedy facility-location    │
            │ over demand points (sub-queries/entities/       │
            │ stances/sections) with per-facet quotas.        │
            │ Replaces noise_floor+cap+reserve+truncate as    │
            │ the set-composer. Sub-ms on ≤200 embeddings.    │
            └───────────────┬────────────────────────────────┘
                            ▼
            Q5 assembly (outline/sandwich order) → Q6 per-facet
            gap check → at most ONE corrective pass
```

Why this shape survives the graveyard of prior attempts:

- **Title-expand died at the noise floor** (v33/v36) because protection
  was expressed as filtering. Here fanned-out chunks enter selection as
  facet-tagged candidates; nothing filters them, the selector *composes*.
  This is exactly the C3 recommendation V36 made and nobody implemented.
- **Wider pools alone did nothing** (RERANK_EXPERIMENT part A) because
  top-k truncation re-collapsed them. Coverage selection is the missing
  consumer of a wide pool.
- **Naive multi-query+RRF is a production wash** (Dell, arXiv:2603.02153:
  reformulation redundancy + rank instability). Facet-*targeted*
  decomposition plus coverage-aware selection is the combination with
  replicated wins (arXiv:2507.00355: +36.7% MRR@10 multi-hop;
  arXiv:2606.29328: +8.4–10.7 EM vs cosine top-k, beats MMR by +7.5).
- **RAPTOR's −14pt here is replicated externally** (48.8 F1 vs 57.0 dense,
  arXiv:2502.11371) — hierarchical *abstractive* structure is the wrong
  tool; native structure (sections, entities, stances) is the working one.

## 5. The swings, ranked (lift ÷ latency, with evidence)

**S0 — repair the floor (landed with this doc).**
(a) `refine_factor=30` + tunable nprobes on every IVF-PQ query: +40pt ANN
recall for +17–26ms (§2 F1). (b) Delete the wasted 200-row search
(−1 full hybrid search/question on the no-atlas path). (c) `bench all`
threads-bank dispatch fix. Validation: A/B on retrieval lanes; gate =
no lane regresses, wall time does not grow.

**S1 — coverage_select (Q4).** Greedy facility-location selection over
the candidate pool with the original query + sub-queries as demand
points; per-facet quotas (each facet ≥1 chunk before any facet takes a
3rd); stance quotas on contested; per-section quotas on summaries.
Training-free, sub-ms, no new model. Evidence: GeoRAG (arXiv:2606.29328,
+8.4–10.7 EM over cosine top-k, −35% cost vs reranking); DF-RAG
(arXiv:2601.17212, query-adaptive diversity beats fixed-λ MMR by 2–10 F1);
ScalDPP validated on Qwen3-0.6B embeddings (arXiv:2604.03240, Recall@4
+31.9%). Targets: multi-article synthesis, section skew. Expected: wiki
sources 66→80%+, SEP summarize facts 66→80%+.

**S2 — demand_plan (Q1).** One constrained fast-slot call producing
{sub_queries, entities, stance_contrast, section_coverage}; parallel
lane fan-out; entity lane = exact Tantivy `title_idx` lookup (fixes
"question names Newton and Einstein, neither article retrieved" — the
dominant wiki failure, 6/8). Pure-Rust fallback = existing
`decompose_question` + entity extraction. Evidence: arXiv:2507.00355
(decomposition+pooled rerank +36.7% MRR@10); Collab-RAG (arXiv:2504.04915,
fine-tuned 3B decomposer beats frozen 32B — the 2B fast slot is viable,
llguidance-constrained; FanOutQA ships 7,305 human decompositions as free
tuning data); 2 iterations capture 95% of 5-iteration gains
(arXiv:2606.21553). Cost: ~0.3–0.8s planner call + parallel searches
(~150ms/lane warm, concurrent). Contested questions get contrast
sub-queries ("criticism of X", "case against X") — the framework-routing
matrix already classifies these turns.

**S3 — atom-PPR lane (HippoRAG-2 adapted).** Seed atoms by ANN against
the query (atoms_ann.lance exists), personalized PageRank over
`edges.csr` with low-weight chunk reset (`doc_to_atoms.json` provides
containment), rank chunks by mass, fuse as one more candidate lane.
Missing pieces are exactly three: chunk nodes in the walk, cross-article
synonym edges (cosine over atom embeddings, offline), the PPR itself
(sparse matvec, ms at this scale). Evidence: HippoRAG 2 (ICML 2025,
arXiv:2502.14802): avg F1 59.8 vs 57.0 dense / 49.6 GraphRAG, recall@5
+5 on MuSiQue, **no single-hop regression**; the controlled comparison
(arXiv:2502.11371) puts PPR-style graph retrieval first on multi-hop.
Our own atlas grounding (+33pt SEP sources when active) is the one-hop
special case. Targets: multi-article synthesis, causal chains.

**S4 — precision layer decision (Q3).** With S1 composing sets, the
reranker's role narrows to per-candidate precision at pool depth
50–100 (not 200 — cross-encoder gains degrade with pool growth,
"Drowning in Documents" arXiv:2411.11767). Prior art: dedup captured
the SEP lift without the model, but wiki needed the cross-encoder for
within-article chunk choice (RERANK_EXPERIMENT: dedup-only wiki −3
sources, +cross-encoder +5 over that). Options, verified-deployable
first:

- **(a) Tuned convex-combination fusion replacing RRF** — 0ms, +3–8%
  nDCG in the 2-list regime (Bruch, TOIS 2023; OpenSearch/Weaviate
  corroborate). Requires per-leg score logging (§7.2) + α tuned on the
  existing banks; guard keyword-ish queries with a BM25-heavy class.
  RRF's k=60 heritage is 7–30-list fusion; we fuse 2–6 lists.
- **(b) Qwen3-Reranker-0.6B via llama.cpp `/rerank`** — native support
  merged 2025-09-25 (llama.cpp #15824), official ggml-org Q8_0 GGUF
  (639 MB), Apache-2.0, MTEB-R 65.8 vs bge-v2-m3 57.0. Prefill-bound:
  top-50 × ~300 tokens ≈ 2.5–3.5s on Strix Halo Vulkan — interactive
  only with pool ≤20–30 or mesh `x:rerank` offload. Mandatory
  golden-set score-parity check vs the HF reference (community GGUFs
  pre-#15824 are broken; KV-cache quantization perturbs scores).
- **(c) Late-interaction MaxSim rerank** (answerai-colbert-small-v1
  33M / GTE-ModernColBERT 150M, both Apache-2.0) — ~50–100ms per query
  over stored token vectors; the best per-ms reranker. Cost is
  index-side: ~27–55 GB token vectors for wikipedia (96–128d int8 +
  50% token pooling) + one encoding pass. LanceDB has native
  multivector/MaxSim columns. Pilot on SEP (188k chunks ≈ 3–6 GB).
- **(d) Learned-sparse third leg** — OpenSearch doc-v3-distill
  (Apache-2.0, doc-side-only inference: queries are tokenizer + idf
  lookup, <1ms) indexed in Seismic (pure Rust, MIT). ~+3–5 BEIR-class
  points over BM25; one ~2–5h encode per corpus. Keep BM25 as the
  OOV/identifier fallback leg.

Sequence: (a) ships with §7.2 for free; bench (b) at pool {20,50} vs
(c) head-to-head; (d) is an offline-enrichment swing (S6 family).

**S5 — per-facet gap check → one corrective pass (Q6).** Extend the
existing always-on gap check to per-facet answerability; an unanswered
facet triggers exactly one re-plan of that facet (CRAG-style evaluator:
+19pt PopQA with a 0.77B judge, arXiv:2401.15884; bounded iteration per
arXiv:2606.21553). Keeps average latency ~1× (most turns pass).

**S6 — offline enrichment for retrieval (idle compute).**
(a) Synonym/coref edges between atoms (S3 substrate). (b) Doc2Query++
dual-index on high-value corpora (arXiv:2510.09557 — separate
question-embedding table fused as a third RRF lane; zero query-time
cost; days of fast-slot batch at 1.9M chunks, so scope to SEP first).
(c) Section metadata on chunks where absent (S1 section quotas).

Explicitly rejected, with reasons on file: HyDE at query time (hurts
strong hybrid retrievers — EACL 2024 Findings), paraphrase multi-query
(Dell null), RAPTOR revival (three independent confirmations of our
negative), GNN retrieval (training + curated-KG assumptions), open-ended
agentic search loops (worst EM-per-latency class, arXiv:2507.09477),
community-summary global search for QA (loses fine-grained evidence,
arXiv:2502.11371).

## 5b. Theory check-in (2026-07-17 literature sweep, 2024-26)

A 28-source sweep (harvest: scratchpad/research_harvest.txt of the
2026-07-17 session; claims are primary-source extractions, not yet
adversarially verified) mapped our measured phenomena onto current
theory. Three imports are actionable; the validations and negatives
are recorded so we stop re-deriving them.

**Validations of the shipped design:**
- Merge-level demand selection IS the 2024-26 submodular thread:
  S-RAG (knapsack-constrained monotone submodular, 1−1/e guarantee),
  AdaGReS (greedy relevance−redundancy, ε-approximate submodularity),
  IDCO/GeoRAG (demand-weighted facility location). GeoRAG's
  Proposition 1 proves any query-proximity-monotone selector misses
  one peak of a bimodal information demand outright — the theorem
  behind our contested/multi-article measurements.
- "What Survives Into Context" (arXiv:2607.00725): 27% of
  retrieval-perfect questions lose the answer AT PACKING; and
  set-level packing pays with SMALL readers (wins at 3B, reverses at
  14B) — our A3B-active reader sits in the paying regime.
- Power of Noise (SIGIR 2024) + The Distracting Effect: the strict
  admission gate is the right call — similar-but-not-answering
  passages cut accuracy up to 25%/single passage. Gold POSITION in
  context matters (middle is worst) — mid-pool placement is not just
  a survival trick.
- HippoRAG 2's own recognition-memory filter over-rejects with a 70B
  LLM (18% of failure cases end with ZERO admitted triples): our
  gate refusing Fermi at 0.6B is a structural property of admission
  filtering, not a local calibration bug.
- Negatives we measured, now externally replicated: plain
  MMR/diversity HURTS (only full coverage objectives win); iterative
  retrieval is question-shape-conditional; offline relevance metrics
  correlate weakly-to-NEGATIVELY with end-task F1 under multi-passage
  injection; coverage-maximization ≠ answer quality.

**Actionable imports, in leverage order:**
1. **Bridge-conditioned admission** (BridgeRAG, arXiv:2604.03384,
   training-free): score later-hop candidates by utility CONDITIONED
   on the bridge evidence — s(q, bridge, candidate) — not similarity
   to the original question. Their measured failure is ours verbatim
   ("cross-encoder reranking causes catastrophic failure on
   multi-hop" by demoting bridge evidence). Implemented same session
   as doc-side conditioning (typed-edge provenance prefixed to the
   gate doc; query side untouched to preserve shared-prefix KV).
   Their negative: LLM-generated rewrites CANNOT substitute real
   bridge passages.
2. **Utility-calibrated admission** (relevance→utility thread:
   arXiv:2507.19102 utility distillation at 1.7B; arXiv:2601.17532
   IGP): score candidates by the reduction in OUR OWN fast
   generator's uncertainty when the chunk is injected — label-free,
   logits-only, 1.5B+IGP beat 7B-without. Their key negative matches
   ours: reordering without admission thresholding does nothing.
   ECoRAG/EXIT formalize evidence-vs-answer calibration (EXIT's
   three-class scheme — direct evidence / relevant-but-lacking /
   irrelevant — is the training recipe if we ever tune the gate).
3. **LinearRAG recipe for non-wiki substrates** (arXiv:2510.10114):
   relation-free entity–sentence–passage tri-graph from NER +
   embeddings, ZERO LLM extraction, beats HippoRAG2 on 2Wiki at 77%
   less indexing time — the validated blueprint for the SEP graph
   (we already own GLiNER, embeddings, and the atom store) that the
   'Kripke' resolution gap needs.
   Also: Provence (ICLR 2025) unifies rerank+prune in one 0.4B
   cross-encoder pass (sentence sequence-labeling head) — the
   long-term shape for within-article selection; SEAL-RAG names our
   fetch-obligation pattern ("replace, don't expand"; context
   dilution).

**Import (1) executed same session — bridge-conditioned admission,
VALIDATED + kept.** Typed-edge candidates carry (seed, relationship)
provenance into the gate; each gate doc opens with its bridge context
("[causal link from 'Manhattan Project'] Title: Leo Szilard …") —
doc-side conditioning, so `score_batch`'s shared-prefix KV reuse is
untouched. Measured effect on the very chunks the bare framing
rejected: Einstein −1.18 → **+4.25**, Wigner −2.21 → **+1.00**, junk
unchanged at −6..−8. Downstream, admitted chunks are retained by the
dominant expander (priority grounding) and additive at the truncate
(raptor precedent). Three successive budget-widening attempts then
produced byte-identical −2-source deltas — resolved by a
single-question forensic run (`SOVEREIGN_FORENSIC=1` + mini-bank):
the "additive" retention shared `grounding.len()` with the generic
loop's budget check, so every retained admission silently evicted the
last pool-order grounding chunk (receipt: admitted 'Quantum
mechanics' evicting the 'Copenhagen interpretation' chunk). One-line
fix: generic budget = `EXPANSION_GROUNDING_CHUNKS +
retained_admitted`. Result: `synth_manhattan` converts end-to-end
(+Albert Einstein source AND fact — the first answer-side conversion
through the full typed-edges → prerank → gate → retention chain),
boundary questions hold, sepq and all canaries byte-identical; sole
residual −1 marginal fact on contested_globalization. **Production
bar now: wiki/questions 38/58 (66%) sources · 109/130 (84%) facts at
p50 ~3.0s** — from 34/58 · 101/130 at the start of 2026-07-16.

## 6. The dual tension, accounted

Interactive budget target: **p50 retrieval wall ≤ today's** (1.8–3.2s on
wikipedia) **while quality climbs.** Redesigned spend, wikipedia warm:

| stage | cost |
|---|---|
| demand_plan (fast slot, ~80 output tokens, constrained) | 300–800ms |
| embed q0 + sub-queries (batched) | 30–80ms |
| 4–6 hybrid lanes, concurrent (refined ANN) | ~200–400ms wall |
| atom-PPR lane | ~5–20ms |
| precision (if shipped: 100 cand batched) | ≤500ms |
| coverage_select | <1ms |
| assembly + per-facet gap check (fast slot) | 100–300ms |
| **total** | **~0.7–2.1s** |

paid for by S0's waste deletion and by not running the corrective pass
on the ~80% of turns that don't need it. Simple/factual intents skip
demand_plan entirely (router already classifies) — their path gets
*faster* (wasted search removed, refined ANN ≈ +17ms).

## 7. Measurement discipline (before any S1–S5 lands)

1. **Bench-prod parity lane.** Add a retrieval lane that drives the
   production pipeline in-process (the `--synth` glassbox path minus
   synthesis) so pipeline steps are visible to CI. Keep the raw-index
   lane as the substrate watchdog. Without this, S1–S3 are invisible to
   the gate that's supposed to protect them.
2. **Per-leg timers** (`vec_ms`, `fts_ms`, `fusion_ms`, `rerank_ms`,
   `select_ms`) in the search path + per-article histogram in the
   `post_merge` audit event (the V36 methodology note, finally honored).
3. **Per-swing A/B** on `bench all --filter wikipedia|sep` retrieval
   lanes + the 13-thread bank with `--judge-trials 3` for anything
   touching the prod pipeline (substring single-trial noise ≈ 17pt).
4. **Hard gates:** newsworthy + factual_recall stay 100%; no category
   regresses >1 item; p50 search wall ≤ baseline.
5. **Ruler fixes are separate commits, separately reported** (bank
   phrasing artifacts in §1; title-normalize variants; the `versus`
   blocker). They move the score without moving the product — the
   scoreboard must say so.

## 8. Phase plan

- **P0 (landed with this doc):** S0a refine_factor + S0b wasted-search
  guard; A/B re-run of both retrieval lanes; threads dispatch fix.
  Re-baseline all retrieval lanes after review (`--update-baseline`).

  **P0 A/B result (2026-07-16, vs the committed baselines):**

  | lane | Δ | verdict |
  |---|---|---|
  | wikipedia/summarize facts | 71% → **75%** | improved |
  | wikipedia/questions | sources 38→39, multi-article facts 21→23, title-coverage ↑0.01 | green |
  | sep/summarize_obscure facts | 77% → 78% | improved |
  | sep/summarize, single_*, newsworthy | unchanged | green |
  | sep/questions title-coverage | 0.86 → 0.84 (−1 source) | **flagged regressed** |
  | wikipedia search p50 | 1857ms → 1864ms | within budget |

  The one flagged regression is a ruler-vs-substrate case, verified
  per-question: on `argument_internalism_externalism_justification`
  the refined ANN promoted more truly-relevant chunks (epistemology
  3→4, justep-intext 2→3) which displaced the single rank-~30
  `knowledge-analysis` chunk that had satisfied the third expected
  source — while facts on the SAME question improved 0.71→0.86. A more
  faithful substrate lost a lucky marginal title hit. The structural
  fix is S1/S2 (an entity/facet lane pins named sources by design);
  do not hack the ANN to win it back. Re-baselining sep/questions at
  the repaired floor is the correct action, pending review.
- **P1 (implemented 2026-07-16, same session):**
  - *Parity surface:* `Runtime::retrieve_evidence` (public, in
    `handlers/knowledge_query.rs`) runs context build → the full
    `kq_pipeline()` → merge/truncate and returns the composed
    `EvidenceRetrieval` pool without synthesis; `eval run
    --prod-pipeline [--isolate]` drives it per question and scores the
    pool with the same rigid scorers (`runner::run_bank_prod`). Intent
    pinned to KnowledgeQuery (routing has its own lane). Wiring it as a
    committed `bench all` lane with its own baseline id is the follow-up
    once the numbers below justify the lane cost.
  - *Per-leg timers:* `CorpusIndex::search` now traces `meta_ms`
    (count_rows + list_indices), `query_ms` (LanceDB execute+collect),
    `convert_ms`, `select_ms`, and a `coverage_select` line with
    pool→out distinct-title counts.
  - *S1 coverage_select:* greedy facility-location set composition in
    `corpus-engine/src/index/search.rs` (`facility_location_select`),
    gated `SOVEREIGN_COVERAGE_SELECT=1`, pool = `limit ×
    SOVEREIGN_COVERAGE_POOL_FACTOR` (default 4, cap 200). Applies at the
    shared leaf, so the raw bench lane, the parity lane, and live chat
    all inherit the same behavior when the flag is on. A/B results
    recorded below when captured.

  **P1 A/B results (2026-07-16, target-scoped, vs the S0 run):**

  *Coverage-select (raw lane, `SOVEREIGN_COVERAGE_SELECT=1`, pool 120→30):*

  | lane | S0 | +coverage | Δ |
  |---|---|---|---|
  | sep/questions facts | 148/159 (93%) | **152/159 (96%)** | +4 |
  | wikipedia/questions facts | 111/130 (85%) | **115/130 (88%)** | +4 |
  | wikipedia/summarize facts | 60/80 (75%) | **62/80 (78%)** | +2 (+5 vs baseline) |
  | sources (both questions banks) | — | ≈flat | missing sources are absent from the pool, not truncated — needs S2 lanes |
  | single_atomic facts / single_roman sources / sep summarize+obscure | — | −1 / −1 / −1,−2 | single-article depth pays for blind breadth |

  Verdict: the set-composition objective works where predicted (multi-fact
  breadth) and costs where predicted (single-article depth). It ships
  OFF by default; S2's demand structure (per-facet/section quotas,
  summary-shape detection) is what turns the tradeoff into a win on both
  sides. Selection cost: sub-ms (`select_ms` in the new timing trace).

  *Parity lane — the first measurement of what the production pipeline
  actually delivers, pool-size-controlled (raw re-run at limit 20 = the
  pipeline's output size):*

  | bank | raw@20 | prod pipeline | pipeline net value |
  |---|---|---|---|
  | wikipedia/questions | 36/58 (62%) · 102/130 (78%) | 30/58 (52%) · 90/130 (69%) | **−6 sources · −12 facts** |
  | sep/questions | 51/66 (77%) · 146/159 (92%) | 48/66 (73%) · 146/159 (92%) | −3 sources · flat |
  | sep/summarize | 8/8 · 41/80 (51%) | 8/8 · **50/80 (62%)** | **+9 facts** |

  Latency: 3.6–9s/question through the pipeline (isolated!) vs ~1.9s raw.
  The per-step glassbox trace on `contested_atomic_bombings_morality`
  shows the mechanism: entity_boost adds 6 chunks that dedupe later
  removes (−16 dups total), cap/truncate squeeze to 20, then the
  post-pipeline **DominantSource expansion** reshapes the pool to 14
  chunks with 10 from one article — collapsing 7 distinct sources into
  near-monoculture on a question whose answer is the *debate around*
  that article. Atlas grounding contributed 0 (no contexts loaded from
  cache — silently dark in this environment).

  Interpretation: the pipeline's concentration machinery (RAPTOR
  injection, dominant-source expansion) was bench-tuned on
  summarize/thread shapes, where it measurably wins (+11pt), and runs
  UNCONDITIONALLY — exporting concentration to multi-article and
  contested questions where it destroys 9–10pts. The redesign's core
  claim is confirmed at the mechanism level: composition must be
  demand-conditional (S1 selection + S2 facets), not a fixed heuristic
  stack.

  **S2-lite: query-shape-conditional expansion (landed + validated
  2026-07-16).** `question_breadth_shape()` (`runtime/evidence.rs`) is a
  pure lexical classifier — contested / causal / comparative question
  shapes force `TopSources`; summary/overview shapes take precedence and
  keep `DominantSource` (so a stance word inside a summary prompt can't
  flip it). `decide_expansion_strategy` folds it in alongside the
  existing ComparisonQuery guard. Unit-tested (9 cases incl. the
  monoculture regression guard and the precedence case); full workspace
  suite green (7,666 passed). Parity A/B, isolated:

  | bank | prod unconditional | prod conditional | Δ |
  |---|---|---|---|
  | wikipedia/questions | 30/58 (52%) · 90/130 (69%) | **35/58 (60%) · 93/130 (72%)** | +5 sources · +3 facts |
  | sep/summarize | 8/8 · 50/80 (62%) | 8/8 · 50/80 (62%) | unchanged (precedence held) |
  | sep/questions | 48/66 (73%) · 146/159 (92%) | **49/66 (74%) · 148/159 (93%)** | +1 source · +2 facts |

  Remaining prod-vs-raw@20 gap on wiki (60 vs 62% sources, 72 vs 78%
  facts) — next suspects, in order: the noise floor, KQ_MERGED_LIMIT=20
  + the 8k-char budget, and missing-entity aboutness (the atomic-bombings
  debate facts live in an article no lane retrieves — S2/S3 territory).

  **Throughput fixes (landed same session, timer-attributed):**

  - *Mesh-self skip* (`step_main_retrieval_mesh`): sealed conversations
    subtract locally-installed corpora from the mesh seal; an empty
    remainder skips the mesh call entirely. The mesh round-trip of
    locally-owned corpora was a full duplicate hybrid search through the
    daemon per turn (the −16-dup dedupe delta in the trace) returning
    only parroted results. Validated quality-IDENTICAL (prod4 = prod2
    byte-for-byte on wiki + sep-summarize) at p50 7.4s→6.0s. Unsealed
    (broad-research) fan-out unchanged.
  - *Search-gate cache* (`CorpusIndex::gate_cache`): `count_rows(None)`
    + `list_indices()` ran on EVERY search and cost 1.1–2.3s per call on
    the 1.9M-row table — the single largest retrieval cost (timer
    receipt: `meta_ms=2354` vs `query_ms=851` on the same search). Now
    cached per open dataset version; invalidated by the instance's own
    write methods (`insert_*`/`delete_*`/`build_indexes`) and naturally
    by `open_index`'s version-mtime instance cache for external writes.

  Also landed: the parity mode as a committed lane — `bench all
  --prod-pipeline [--isolate]` (baselines at `baselines/<bench>-prod/` /
  `-prod-isolated`, mutually exclusive with `--synth`/`--routing-only`)
  plus two HARD `retrieval-prod:` lanes in `sovereign-ci-bench.sh` — the
  lanes that would have caught the pipeline's −12-fact regression the
  raw lanes were blind to.

  **Coverage-select on the PRODUCTION path (measured post gate-cache,
  2026-07-16 evening): a near-strict win.** The depth-vs-breadth
  conflict observed on the raw lane dissolves in prod — coverage
  diversifies the per-corpus base pool, then the (now shape-conditional)
  expansion supplies depth downstream:

  | bank | prod cov-off | prod cov-on | Δ |
  |---|---|---|---|
  | wikipedia/questions | 60% src · 72% facts | 59% · **78%** | −1 src · **+8pt facts** (= raw@20 parity) |
  | sep/summarize | 100% · 62% | 100% · **66%** | **+4pt facts** |
  | sep/questions | 80% · 92% | **83%** · 92% | **+2 src** · −1 fact |
  | p50 latency | 0.7–2.7s | 1.2–2.8s | within budget |

  Decision queued (own iteration, own battery): flip
  `SOVEREIGN_COVERAGE_SELECT` default ON. A default flip touches every
  surface (chaos honesty, governance lanes, raw-lane substrate
  baselines incl. the two single-question depth canaries), so it must
  clear the chaos-gate + full raw re-baseline + prod re-baseline as its
  own checkpoint, not ride this one. **Shipped 2026-07-16 evening
  (chaos gate PASS, suite 7,668 green) — see the checkpoint-4 commit.**

  **S3 attempt log (2026-07-16 evening) — NEGATIVE, reverted, with the
  dependency order it established.** Two question-side probes and two
  structural probes, all on the coverage-on substrate (wiki/questions
  parity, isolated; baseline 34/58 sources · 101/130 facts · 2.8s):

  | arm | sources | facts | p50 |
  |---|---|---|---|
  | entity title-lane (title-FTS fetch of question entities) | 34 | 101 | 3.0s |
  | old axis-heuristic `graph_neighbor_expand` | **36** | 100 | 4.4s |
  | PPR v1 (forward-push over link graph, 4 articles × 3 chunks, reserved, 0.6×top score) | 27 | 78 | 5.6s |
  | PPR v2 (3 × 2, below-median score, no reservation) | 35 | 95 | 4.6s |

  Findings, each with a receipt: (1) the remaining source-misses are
  ANSWER-side articles the question never names (verified:
  `synth_manhattan`'s expected Szilard/Fermi/Einstein appear nowhere in
  the question) — question-side lanes are exhausted. (2) Structural
  admission through a **title-cosine gate cannot distinguish
  load-bearing from plausible-adjacent**: every injected chunk displaces
  a fact-bearing direct hit past the 20-slot truncate, and even humble
  injection (below-median, unreserved) nets −6 facts for +1 source.
  (3) The old axis internals' +2 sources / −1 fact / +1.6s does not
  clear the default-on bar; the flag stays opt-in.

  **Consequence — S4 before S3.** The research already said bridge
  admission must be rerank-gated (arXiv:2509.25530); the probes now
  show it locally. The unlock for structural expansion is a real
  cross-encoder admission gate: Qwen3-Reranker-0.6B, verified
  llama.cpp-native (#15824, official ggml-org Q8_0, Apache-2.0), scoring
  only the ~6-10 structural candidates per query (~10-20 prefills ≈
  100-300ms — NOT a full-pool rerank). Next session's swing: S4
  admission gate + retry S3 through it. The PPR walk implementation is
  recoverable from this session's history (probe-ppr* runs in
  target/ci-bench-p1/).

  **S4 attempt log (2026-07-17) — gate VALIDATED, lane blocked
  downstream; five probes, all receipted.** Built and A/B'd the full
  S4+S3 stack on the wiki/questions prod parity lane (isolated,
  limit 30; baseline 34/58 src · 101/130 facts · p50 2.8s):

  - *Infrastructure (kept, tested):* jina-reranker-v3's public GGUF is
    DEAD for scoring (conversion drops the projection head — the
    JinaScoreToken protocol reads tied-embedding noise, rerank_slot.rs
    2026-07-09). The official ggml-org
    `qwen3-reranker-0.6b-q8_0.gguf` (639 MB, `sovereign/models/`)
    works via the existing YesNoLogit protocol: smoke test gives
    perfect ordering with +5..+8 vs −9..−11 logit-diff separation and
    a natural zero crossing. `SOVEREIGN_RERANK_GATE_ONLY=1`
    (bootstrap) installs `rerank_fn` for gate consumers WITHOUT
    enabling full-pool `search_with_rerank`. ~37 ms/pair prefill-bound
    on Strix Halo Vulkan; <1 s load.
  - *The lane* (`SOVEREIGN_PPR_EXPAND=1`, new `ppr_struct_expand`
    step after `graph_neighbor_expand`): forward-push PPR walk over
    the wiki link graph (channel A) + the seeds' `causal`/`contested`
    typed edges under front-loaded per-seed quotas (channel B) +
    title-CE prerank + whole-article fetch with lexical
    within-article pick + chunk-level CE admission, injected via
    synthetic `vector_distance` just inside the truncate boundary.
    GLiNER seed confirmation (model now installed) filters heuristic
    seed noise when wired.
  - *Iterated findings, one per probe:* (1) occurrence weights carry
    NO signal for answer-side people — Manhattan Project has 468 of
    508 neighbors at weight 1; Szilard/Fermi/Einstein live there, and
    ALL of them are `causal` edges from its Origins section (typed
    edges are the channel that finds them, measured). (2) HashMap
    frontier order let one edge-dense seed fill the typed cap —
    per-seed quotas fixed it: by probe 3b the candidate list for the
    Manhattan question contained Fermi, Wigner, Einstein, FDR, with
    the title prerank ranking them top-4. (3) The displacement bar
    (beat max of the calibration tail) admits ~nothing anywhere.
    (4) The absolute CE-yes floor (bar = 0) admits 13 chunks/20
    questions and the discrimination is clean (answer-side −3..+2.6,
    junk −8..−13) — but the default Qwen3-Reranker instruct asks
    "does this passage ANSWER the query", and single chunks rarely
    answer synthesis questions (Einstein's best chunk −1.18 for the
    émigré question; a General-relativity chunk −3.95 for a
    relativity question). (5) THE BLOCKER MOVED DOWNSTREAM: chunks
    admitted with positive scores were then eaten by the
    post-pipeline machinery — Adolf Hitler (+1.55, an expected
    source for depression→fascism) admitted into the merged pool yet
    absent from the 6-chunk final pool; Natural selection (+2.65,
    the exact missing expected source for the Darwin question)
    dropped while non-expected Sexual selection survived. Quality
    was byte-identical to baseline (34/101) in ALL five probes —
    fact-safe (yesterday's −6-fact mode is structurally gone) and
    gain-free. p50 8.1s (fetch ~3.5-4s: `fetch_chunks_by_title` is a
    filtered scan, ~450 ms/article, serialized).
  - *Named unlocks, in dependency order, for whoever picks this up:*
    (a) admitted-chunk SURVIVAL through the post-pipeline
    expansion/budget stages — composition is S2 territory, again;
    (b) evidence-framed CE instruct (`SOVEREIGN_RERANK_INSTRUCT`
    exists; the default is web-search answer-ranking, our task is
    evidence composition) — untested because (a) already eats
    positive admissions; (c) fetch cost — a title scalar index or
    the atoms substrate instead of filtered scans; (d) batched CE
    decode for >20-pair calls.

  **S4 promotion (2026-07-17, same day, continued iteration —
  PROMOTED to default-ON).** Unlock (a) turned out to be the whole
  ballgame, and each named unlock fell in order:

  - *v4 — survival:* the eviction site was `expand_from_dominant_
    source`, which rebuilds the pool as dominant chunks + the FIRST
    few non-dominant chunks in pool order — tail-placed admissions
    were always its `dropped_noise`. Mid-pool placement (anchor at
    boundary/2) + stemming the causal lexeme (`contribut` — "How did
    X contribute to Y" silently missed the causal shape and routed
    DominantSource) produced the first movement in seven probes:
    wiki 34/101 → **37/105**, zero regressions.
  - *v5 — fetch:* BTree scalar index on `title`
    (`build_title_scalar_index`, 4s build on 1.9M rows) turned the
    ~450ms filtered scans into index seeks (fetch 4.2s → 272ms/turn;
    also speeds the PROD dominant-source expander, flag-independent).
  - *v6 — instruct reframe is DEAD:* an out-of-distribution instruct
    paraphrase collapsed admissions 13 → 1 and lost both wins. The
    trained default instruct IS the calibration; do not touch it.
  - *v7 — prefix-reuse KV in `score_batch`* (decode scaffold+query
    once, roll back per doc via `clear_kv_cache_seq`; verified on
    llama-cpp-4 0.4.2, smoke scores identical ±0.1 logit) + the
    calibration-tail pairs cut (bar=0 made them telemetry).
  - *v8/v9 — overlap:* the lane is pool-independent except at its
    edges, so it runs as a spawned task (`ppr_struct_spawn`, FIRST
    core step — it extracts its own entities) joined at
    `ppr_struct_expand` with a 4s abandon deadline. Attached-doc
    turns drop both steps.
  - *v10 — trims:* candidates 8→6, frontier 8→6 (every observed
    admission ranked top-4 post-prerank).

  **Attribution honesty (same-harness flag-OFF anchors):** the
  causal-stem fix alone moved flag-OFF prod wiki to 35/105 — the
  committed 34/101 baselines are STALE flag-independently, and
  sep/summarize's +6 facts and sep/questions' +1/+1 were entirely
  the stem, not the lane (sep seeds don't map to the wiki graph;
  the lane early-outs there). The lane's own contribution: wiki
  **+2 sources (37 vs 35), facts held, +182ms p50** (2956 vs 2774
  same-harness — within run noise; the overlap window here is
  atlas-dark, real prod is wider). Final battery all green: wiki
  37/105 · sep-q 56/147 (= flag-OFF) · sep-sum 8/59 (= flag-OFF) ·
  canaries byte-identical.

  `SOVEREIGN_PPR_EXPAND` now defaults ON (=0/false/off/no disables),
  dark without a reranker (`SOVEREIGN_RERANK_MODEL_PATH`, gate-only
  by default sensible via `SOVEREIGN_RERANK_GATE_ONLY=1`). Open
  follow-ups: resident rerank slot in models.toml profiles (the
  RERANK_EXPERIMENT system-design decision, unchanged), multi-seq
  batched CE decode (n_seq_max + seq_cp exist in 0.4.2), and a
  graph substrate for non-wikipedia corpora (S3's atom-CSR walk) so
  the lane stops being wiki-only.

  **S1/S2-at-the-merge — the merge-select architecture (2026-07-17
  night; landed behind `SOVEREIGN_MERGE_SELECT`, default OFF).**
  The bucket-1 forensic (~10 of 21 missing wiki sources are entities
  the question NAMES) killed the merge-ordering knob class with
  receipts — front-pull entity reservation measured −1 src/−7 facts
  (bio chunks displaced specific chunks), conditional gap-fill
  measured null (the named entity's chunks never ENTERED the pool:
  entity_boost's ANN on the entity's surface form lands topic-near,
  not canonical). Composition can't fix what retrieval didn't fetch,
  so the architecture has two halves:

  - **Entity fetch OBLIGATIONS** (`fetch_entity_obligations`, second
    spawned lane beside PPR): named entities are title-resolved via
    FTS and fetched deterministically (`fetch_chunks_by_title`, top-2
    by question overlap, `obligation_entity` tag). Resolution lessons,
    each measured: title-contains alone resolves 'Newton' to 'Newton
    (unit)'/'Arik Einstein'; the CE scoring candidate TITLES against
    the QUESTION picks 'Isaac Newton' — but an EXACT title match must
    bypass the CE entirely (it preferred 'Eastern Christianity' over
    the exact 'Christianity', flooding the pool with sibling-article
    chunks).
  - **`merge_demand_select`** (merge_select.rs, 5 unit tests):
    replaces the cap → 4-reserve-passes → truncate pile inside
    `step_cap_and_reserve` when ON. Pins honored (RAPTOR additive,
    atom-enum budgeted), one demand slot per named entity, greedy
    `max(1/(rank+20), 1/(article_best_rank+20)) · 0.7^dup` fill. The
    within-article strength floor is the load-bearing refinement:
    valuing depth chunks by their own global rank double-charged
    depth (rank + decay) and made the decay constant bank-contested
    (sep best at 0.7, wiki at 0.75+); the floor dissolved that.

  Standing (same-substrate A/B): within ±1 item of the legacy stack
  on every bank, PLUS Isaac Newton (the proven bucket-1 conversion)
  and news +2 sources — not yet STRICTLY better, so the flag ships
  dark. Open threads: sepq 'rawls' −1 src under exact-match-first
  resolution; arch-arm canaries on the current substrate.

  **FLIPPED default-ON (2026-07-17 morning re-verdict).** After the
  bridge-conditioning chain + grounding-budget fix landed, the
  re-verdict battery read: wiki 39/58 (+Isaac Newton, structurally
  unreachable in the legacy arm) · 107/130 (−2 flicker-class facts —
  saturated-budget marginals that flip between configs, 'labor' vs
  'poverty'); news 9/12 (+1, facts 100%); sepq 55/149 (−1 tail-flip);
  sepsum/canaries byte-identical. Both hard gates pass, no category
  regresses >1 item; +2 structural sources outrank −2 flicker facts,
  and the arch arm is the substrate the queued supply work (SEP
  tri-graph, IGP chunk-pick — which targets exactly those fact
  tails) builds on. `=0` restores the legacy stack byte-identically.
  Gates at flip: lint pass, suite 7,717/0, prod-isolated lanes
  re-baselined (wiki 3 green · 2 improved · 0 regressed; sep 3
  improved · 0 regressed). Post-flip loss ledger (system-wide
  forensic, same day): #1 proposal gaps in the answer-side channels
  (Szilard's quota lottery, Watt/Steam-engine not typed-reachable,
  Bohr/Heisenberg) → LinearRAG-style activation spread; #2
  within-article chunk pick at the lane fetch (Fermi −1.45
  near-misses on wrong chunks) → IGP/CE pick; #3 scorer strictness +
  sibling-article facts → ruler pass. Base retrieval and composition
  now account for ≈ zero remaining loss.

  **Question-aware multi-source top-up (same night; KEPT,
  flag-independent — both regimes benefit).** The residual −1-fact
  class ('grace', 'complementarity') traced to
  `expand_from_top_sources` topping up articles with their first-N
  chunks in DOCUMENT ORDER — fact coverage was a chunk-position
  lottery. It now fetches candidates wide (`EXPANSION_WIDE_FETCH=64`,
  ~ms via the BTree title index) and ranks the top-up by substantive
  question-token overlap. The dominant expander keeps document order
  (narrative-cohesion contract; the sepsum win is defended). A/B:
  wiki/questions facts 105→109 (grace + complementarity + 2 more),
  sep/questions 147→149, single_atomic depth canary 1/6→2/6,
  sroman/news/wsum held exactly. Day total on the production path:
  wiki/questions 34/58 · 101/130 (morning) → **37/58 · 109/130**
  (64% sources · 84% facts) at p50 ~2.9s.
- **Batched CE decode — landed + validated (2026-07-17).** The
  multi-sequence `RerankSlot::score_batch` (shared prefix decoded once,
  fanned to doc sequences, one decode per wave) **was broken** in the
  tree: it aborted on llama.cpp b9982 (`GGML_ASSERT: seq_cp() is only
  supported for full KV buffers`, llama-kv-cache.cpp:502) because the
  rerank context was created non-unified, so the partial prefix fanout
  was a CROSS-stream copy. One-line fix: `.with_kv_unified(true)` on the
  rerank context (setter already in vendored `context/params/advanced.rs`)
  → `n_stream=1`, all seqs map to stream 0, and the fanout becomes the
  cheap same-stream cell-tag update the code always intended. Correctness
  proven by `examples/rerank_batch_check.rs` against a machinery-free
  `score_sequential` oracle: ranking identical (top-8 8/8), signed gate
  bias ±0.03 (symmetric quantized-GEMM FP noise), **18.4× on 48 titles**
  (127ms vs 2339ms), 6.5× on chunks. Scoreboard A/B (`eval run
  --prod-pipeline --isolate --limit 30`, batched vs `SOVEREIGN_RERANK_
  SEQUENTIAL=1`): wiki **42/58 src** (both, = committed baseline) · 104
  vs 105 facts (one saturated-budget flicker) ; sep **56/66 src · 152/159
  facts** (both, identical). Pure latency win, no source regression —
  the "slots become free" unlock for further prerank widening.
  `SOVEREIGN_RERANK_SEQUENTIAL=1` is the A/B valve + rollback hatch.
- **Lowercase-concept obligations — built, NEGATIVE result, ships dark
  (2026-07-17).** GLiNER `["Concept"]` pass (`extract_concepts`,
  separate from the tuned 5-label confirmation filter) feeds the
  entity-obligation lane with bidirectional title match ("European
  colonialism"→"Colonialism"), behind `SOVEREIGN_CONCEPT_OBLIGATIONS`.
  All correct + firing (audit: determinism Q → `concepts=["uncertainty
  principle","determinism"]`; colonialism Q → `concepts=["european
  colonialism"]`). But the limit=30 A/B measured **ZERO source lift** —
  OFF==ON (4/7 · 8/13 on the 2-question addressable set): base retrieval
  already surfaces `Determinism`/`Colonialism` at the real bench limit.
  The checkpoint's "lowercase extraction gap" was a HYPOTHESIS that did
  not reproduce under instrumentation (and 'capitalism' isn't even in
  its question — a semantic-bridge miss, not extraction). GOTCHA that
  caused a false "eviction" reading mid-session: `eval run` default
  `--limit` is 10, but `bench all --prod-pipeline` passes
  `--retrieval-limit 30`; the miss vanishes at 30. Default OFF; kept for
  banks/corpora where base retrieval genuinely misses a named concept.
- **P2:** S2 demand_plan behind `SOVEREIGN_DEMAND_PLAN` (subsumes
  `query_decomp`/`title_expand` — retire those flags after two green
  A/Bs). Stance + section quotas ride the same flag.
- **P3:** S3 atom-PPR lane behind `SOVEREIGN_ATOM_PPR`; offline synonym
  edges job.
- **P4:** S4 precision bench-off; S5 per-facet corrective pass; S6
  doc2query++ pilot on SEP.

Every phase: green A/B on its target categories, no hard-gate breach,
one note (`kind=decision`) recording the verdict, and the losing knobs
retired rather than accreted — the pipeline registry stays the SSOT.
