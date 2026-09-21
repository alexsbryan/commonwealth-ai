<!-- GENERATED FILE — do not edit by hand.
Source: sovereign-core/src/runtime/retrieval_pipeline.rs
Regenerate: UPDATE_RETRIEVAL_PIPELINE_DOC=1 cargo test -p sovereign-core --test main retrieval_pipeline_doc -->

# Retrieval pipeline — steps and knobs

The retrieval-injection orchestration is data: each pipeline is an
ordered list of named steps run by one tracing runner (one
`tracing::info!(target: "retrieval.pipeline")` line per step with
`chunks_before/after/delta`). The governing principle: **the intent
decides HOW to answer (model tier, expansion, synthesis shape) — never
WHERE knowledge lives.** Both pipelines share the same 3-step
evidence-gathering head and 12-step core (incl. the FR-9 governance
active-set filter); they differ only in their
tails. Step ORDER is bench-tuned data, pinned by golden tests — see
the module doc in `retrieval_pipeline.rs` for design rationale and the
dated convergence/divergence log.

## Step sequences

### KnowledgeQuery / ComparisonQuery (`kq_pipeline`)

| # | step | kind | gate flag |
|---|---|---|---|
| 1 | `main_retrieval_mesh` | `Injector` | — |
| 2 | `scope_personal_filter` | `Filter(OutOfScope)` | — |
| 3 | `store_search` | `Injector` | — |
| 4 | `ppr_struct_spawn` | `Inert` | `SOVEREIGN_PPR_EXPAND` |
| 5 | `entity_boost` | `Injector` | — |
| 6 | `meta_atlas_boost` | `Injector` | — |
| 7 | `noise_floor` | `Filter(NoQueryOverlap)` | — |
| 8 | `searched_corpora_snapshot` | `Inert` | — |
| 9 | `atlas_grounding` | `Injector` | `SOVEREIGN_ATLAS_GROUNDING` |
| 10 | `reweight_and_sort` | `Filter(CapExceeded)` | — |
| 11 | `atom_enum` | `Injector` | `SOVEREIGN_ATOM_ENUM` |
| 12 | `ppr_struct_expand` | `Injector` | `SOVEREIGN_PPR_EXPAND` |
| 13 | `dedupe_merged` | `Filter(Duplicate)` | — |
| 14 | `cap_and_reserve` | `Filter(NotSelectedByObjective)` | — |
| 15 | `governance_active_set` | `Filter(DeadLaw)` | — |
| 16 | `truncate_merged` | `Filter(BudgetExhausted)` | — |
| 17 | `scope_audit` | `Inert` | — |

### DeepQuery / SimpleQuery (`deep_pipeline(true)`)

| # | step | kind | gate flag |
|---|---|---|---|
| 1 | `main_retrieval_mesh` | `Injector` | — |
| 2 | `scope_personal_filter` | `Filter(OutOfScope)` | — |
| 3 | `store_search` | `Injector` | — |
| 4 | `ppr_struct_spawn` | `Inert` | `SOVEREIGN_PPR_EXPAND` |
| 5 | `entity_boost` | `Injector` | — |
| 6 | `meta_atlas_boost` | `Injector` | — |
| 7 | `noise_floor` | `Filter(NoQueryOverlap)` | — |
| 8 | `searched_corpora_snapshot` | `Inert` | — |
| 9 | `atlas_grounding` | `Injector` | `SOVEREIGN_ATLAS_GROUNDING` |
| 10 | `reweight_and_sort` | `Filter(CapExceeded)` | — |
| 11 | `atom_enum` | `Injector` | `SOVEREIGN_ATOM_ENUM` |
| 12 | `ppr_struct_expand` | `Injector` | `SOVEREIGN_PPR_EXPAND` |
| 13 | `dedupe_merged` | `Filter(Duplicate)` | — |
| 14 | `cap_and_reserve` | `Filter(NotSelectedByObjective)` | — |
| 15 | `governance_active_set` | `Filter(DeadLaw)` | — |
| 16 | `truncate_merged` | `Filter(BudgetExhausted)` | — |
| 17 | `top_sources_expand` | `Injector` | — |
| 18 | `scope_audit` | `Inert` | — |

### DeepQuery attached-document variant (`deep_pipeline(false)`)

| # | step | kind | gate flag |
|---|---|---|---|
| 1 | `entity_boost` | `Injector` | — |
| 2 | `meta_atlas_boost` | `Injector` | — |
| 3 | `noise_floor` | `Filter(NoQueryOverlap)` | — |
| 4 | `searched_corpora_snapshot` | `Inert` | — |
| 5 | `reweight_and_sort` | `Filter(CapExceeded)` | — |
| 6 | `atom_enum` | `Injector` | `SOVEREIGN_ATOM_ENUM` |
| 7 | `dedupe_merged` | `Filter(Duplicate)` | — |
| 8 | `cap_and_reserve` | `Filter(NotSelectedByObjective)` | — |
| 9 | `governance_active_set` | `Filter(DeadLaw)` | — |
| 10 | `truncate_merged` | `Filter(BudgetExhausted)` | — |
| 11 | `top_sources_expand` | `Injector` | — |
| 12 | `scope_audit` | `Inert` | — |

## Env-knob registry

Every `SOVEREIGN_*` knob the pipeline (and its immediate
post-steps) reads. Step `-` marks knobs read inside a helper
rather than gating a whole step. A registry-coverage test
asserts every step-level gate appears here.

| step | flag | default | purpose |
|---|---|---|---|
| atlas_grounding | `SOVEREIGN_ATLAS_GROUNDING` | on | Atlas graph-walk grounding (cosine seeds → BFS over typed edges → FTS-fetch evidence chunks). =0/false/off/no disables. |
| atom_enum | `SOVEREIGN_ATOM_ENUM` | off | Enumeration-class questions get the corpus's top-degree typed atoms injected as virtual chunks (post-floor). |
| atom_enum | `SOVEREIGN_ATOM_ENUM_TOPK` | see helper | How many enumerated atoms become virtual chunks. |
| atom_enum | `SOVEREIGN_ATOM_ENUM_POOL` | see helper | Candidate-pool cap before ranking. |
| atom_enum | `SOVEREIGN_ATOM_ENUM_RANK` | rrf | Atom ranking mode. |
| atom_enum | `SOVEREIGN_ATOM_ENUM_SCORE` | see helper | Score stamped on enumerated virtual chunks. |
| atom_enum | `SOVEREIGN_ATOM_ENUM_NOFILTER` | off | Disable the enumeration-question classifier filter. |
| atom_enum | `SOVEREIGN_ATOM_ENUM_RELATIONS` | off | Include relation atoms in the enumeration. |
| atom_enum | `SOVEREIGN_ATOM_ENUM_OVERVIEW` | on | Overview/summary questions ("most important thing in X", "summarize X") inject the scoped corpus's atlas Claim atoms as virtual chunks (the corpus's key points) so the answer grounds on them instead of abstaining over an anchorless pool. Default ON (set =0 to disable). Independent of SOVEREIGN_ATOM_ENUM; detected by question shape (no LLM call). |
| ppr_struct_spawn | `SOVEREIGN_PPR_EXPAND` | on (dark without a reranker) | PPR walk + typed causal/contested edges over the wikipedia link graph propose answer-side articles; a cross-encoder admission gate (requires rerank_fn — SOVEREIGN_RERANK_MODEL_PATH) injects only CE-yes candidates, placed mid-pool. Spawned early, joined late: overlaps the core steps. =0/false/off/no disables (RETRIEVAL_REDESIGN.md S4 attempt log). |
| ppr_struct_expand | `SOVEREIGN_PPR_EXPAND` | on (dark without a reranker) | PPR walk + typed causal/contested edges over the wikipedia link graph propose answer-side articles; a cross-encoder admission gate (requires rerank_fn — SOVEREIGN_RERANK_MODEL_PATH) injects only CE-yes candidates, placed mid-pool. Spawned early, joined late: overlaps the core steps. =0/false/off/no disables (RETRIEVAL_REDESIGN.md S4 attempt log). |
| cap_and_reserve | `SOVEREIGN_MERGE_SELECT` | on | Demand-aware merge composition: entity fetch-obligations + ONE facility-style selector (pins + per-named-entity demand slots + greedy diminishing-returns-per-article with within-article strength floor) replacing the cap/reserve/truncate heuristic pile. =0/false/off/no restores the legacy stack. |
| - | `SOVEREIGN_EXPANSION_SCOPE` | on | Scope every expansion fan-out (entity boost, decomp, title, demand-plan, graph-neighbor, and the spawned PPR + entity-obligations lanes) to the corpora the MAIN fan-out ranked highest, via PipelineState::expansion_corpora(). Not a step gate — it narrows what the expansion steps search. Also collapses the corpus prefilter from one pass per fan-out to one per turn, since a scoped fan-out skips it. Default ON since 2026-08-13 (verdict 94f01eb2) on measured numbers: retrieval slope 2.183 -> 0.849 s per 100 corpora (2.57x), SEP anchor byte-identical, banks within noise (§8.4). =0/false/off/no disables. |
| - | `SOVEREIGN_EXPANSION_SCOPE_CORPORA` | 8 | How many CORPORA an expansion fan-out may search when SOVEREIGN_EXPANSION_SCOPE is on — the scale-vs-recall dial. The unit is corpora, not chunks: a chunk budget let one corpus monopolise the scope (14 of 20 wikipedia questions scoped to `sf-assessor-roll` alone) and cost that bank 3 sources / 4 facts. |
| - | `SOVEREIGN_HISTORY_RETRIEVAL` | on | History layer: retrieval over prior conversation turns (=0 disables). |
| - | `SOVEREIGN_COMPACTION_DISABLE` | off | History layer: =1 disables dropped-history compaction. |
| - | `SOVEREIGN_EPISTEMIC_STATE` | on | Post-pipeline: assemble the per-turn epistemic ledger (EPISTEMIC_STATE.md) into message metadata. Pure collation, no model calls; =0 disables. |
| - | `SOVEREIGN_COVERAGE_PROBE` | on | Post-pipeline, gap/abstain turns only: cross-corpus nearest-chunk-cosine probe classifying a gap as TopicUncovered vs ClaimUncovered. =0 disables. |
| - | `SOVEREIGN_COVERAGE_NEAR_SIM` | 0.49 | Similarity floor for the coverage probe's TopicUncovered/ClaimUncovered split (calibrated 2026-09-09 on the secret_agent bank; the boundary is the top of the observed off-topic band across both calibration runs). |

## Verdict buckets (2026-06-10 flag audit)

- **Validated, default ON** — `SOVEREIGN_ATLAS_GROUNDING`,
`SOVEREIGN_HISTORY_RETRIEVAL`; router-side:
`SOVEREIGN_KQ_EFFORT_TIER`, `SOVEREIGN_ROUTER_ROBUST_COARSE`
(both A/B-validated 2026-06-09). Disable only for A/B runs.
- **Experimental, opt-in (default OFF)** — `SOVEREIGN_ATOM_ENUM`
(net-negative on focused enumeration per the 2026-06-04
bench; keep gated), `SOVEREIGN_COMPACTION_DISABLE`.
Flipping one ON in prod requires its own bench A/B.
- **Tunable parameters** — the `_TOPK/_POOL/_RANK/_SCORE`
family. Sub-knobs of their parent feature.
- **Retired** — the `SOVEREIGN_RAPTOR_*` family (2026-09-07,
order ei-5c). The retrieval-time summary injector they gated
was a second grounding implementation outside corpus-engine;
whole-work summaries reach the pool through the atlas walk
now, and whether a corpus HAS them is a data state
(`svrn enrich summary-atoms <corpus>`), not a knob. Setting
any of the five has no effect. See
`sovereign/DEFAULTS_LEDGER.md`.
- **Debug / escape hatches** — `SOVEREIGN_ATOM_ENUM_NOFILTER`
(ablation). Never set in normal operation.
