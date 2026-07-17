<!-- GENERATED FILE — do not edit by hand.
Source: sovereign-core/src/runtime/retrieval_pipeline.rs
Regenerate: UPDATE_RETRIEVAL_PIPELINE_DOC=1 cargo test -p sovereign-core --test retrieval_pipeline_doc -->

# Retrieval pipeline — steps and knobs

The retrieval-injection orchestration is data: each pipeline is an
ordered list of named steps run by one tracing runner (one
`tracing::info!(target: "retrieval.pipeline")` line per step with
`chunks_before/after/delta`). The governing principle: **the intent
decides HOW to answer (model tier, expansion, synthesis shape) — never
WHERE knowledge lives.** Both pipelines share the same 3-step
evidence-gathering head and 13-step core (incl. the FR-9 governance
active-set filter); they differ only in their
tails. Step ORDER is bench-tuned data, pinned by golden tests — see
the module doc in `retrieval_pipeline.rs` for design rationale and the
dated convergence/divergence log.

## Step sequences

### KnowledgeQuery / ComparisonQuery (`kq_pipeline`)

| # | step | gate flag |
|---|---|---|
| 1 | `main_retrieval_mesh` | — |
| 2 | `scope_personal_filter` | — |
| 3 | `store_search` | — |
| 4 | `ppr_struct_spawn` | `SOVEREIGN_PPR_EXPAND` |
| 5 | `entity_boost` | — |
| 6 | `meta_atlas_boost` | — |
| 7 | `bridge_boost` | `SOVEREIGN_META_BRIDGE` |
| 8 | `query_decomp` | `SOVEREIGN_QUERY_DECOMP` |
| 9 | `title_expand` | `SOVEREIGN_TITLE_EXPAND` |
| 10 | `noise_floor` | — |
| 11 | `atom_enum` | `SOVEREIGN_ATOM_ENUM` |
| 12 | `raptor_grounding_early` | `SOVEREIGN_RAPTOR_GROUNDING` |
| 13 | `atlas_grounding` | `SOVEREIGN_ATLAS_GROUNDING` |
| 14 | `reweight_and_sort` | — |
| 15 | `graph_neighbor_expand` | `SOVEREIGN_GRAPH_NEIGHBOR_EXPAND` |
| 16 | `ppr_struct_expand` | `SOVEREIGN_PPR_EXPAND` |
| 17 | `dedupe_merged` | — |
| 18 | `cap_and_reserve` | — |
| 19 | `governance_active_set` | — |
| 20 | `readiness_disclosure` | — |
| 21 | `truncate_merged` | — |

### DeepQuery / SimpleQuery (`deep_pipeline(true)`)

| # | step | gate flag |
|---|---|---|
| 1 | `main_retrieval_mesh` | — |
| 2 | `scope_personal_filter` | — |
| 3 | `store_search` | — |
| 4 | `ppr_struct_spawn` | `SOVEREIGN_PPR_EXPAND` |
| 5 | `entity_boost` | — |
| 6 | `meta_atlas_boost` | — |
| 7 | `bridge_boost` | `SOVEREIGN_META_BRIDGE` |
| 8 | `query_decomp` | `SOVEREIGN_QUERY_DECOMP` |
| 9 | `title_expand` | `SOVEREIGN_TITLE_EXPAND` |
| 10 | `noise_floor` | — |
| 11 | `atom_enum` | `SOVEREIGN_ATOM_ENUM` |
| 12 | `raptor_grounding_early` | `SOVEREIGN_RAPTOR_GROUNDING` |
| 13 | `atlas_grounding` | `SOVEREIGN_ATLAS_GROUNDING` |
| 14 | `reweight_and_sort` | — |
| 15 | `graph_neighbor_expand` | `SOVEREIGN_GRAPH_NEIGHBOR_EXPAND` |
| 16 | `ppr_struct_expand` | `SOVEREIGN_PPR_EXPAND` |
| 17 | `dedupe_merged` | — |
| 18 | `cap_and_reserve` | — |
| 19 | `governance_active_set` | — |
| 20 | `readiness_disclosure` | — |
| 21 | `truncate_merged` | — |
| 22 | `top_sources_expand` | — |

### DeepQuery attached-document variant (`deep_pipeline(false)`)

| # | step | gate flag |
|---|---|---|
| 1 | `entity_boost` | — |
| 2 | `meta_atlas_boost` | — |
| 3 | `bridge_boost` | `SOVEREIGN_META_BRIDGE` |
| 4 | `query_decomp` | `SOVEREIGN_QUERY_DECOMP` |
| 5 | `title_expand` | `SOVEREIGN_TITLE_EXPAND` |
| 6 | `noise_floor` | — |
| 7 | `atom_enum` | `SOVEREIGN_ATOM_ENUM` |
| 8 | `reweight_and_sort` | — |
| 9 | `graph_neighbor_expand` | `SOVEREIGN_GRAPH_NEIGHBOR_EXPAND` |
| 10 | `dedupe_merged` | — |
| 11 | `cap_and_reserve` | — |
| 12 | `governance_active_set` | — |
| 13 | `readiness_disclosure` | — |
| 14 | `truncate_merged` | — |
| 15 | `top_sources_expand` | — |

## Env-knob registry

Every `SOVEREIGN_*` knob the pipeline (and its immediate
post-steps) reads. Step `-` marks knobs read inside a helper
rather than gating a whole step. A registry-coverage test
asserts every step-level gate appears here.

| step | flag | default | purpose |
|---|---|---|---|
| atlas_grounding | `SOVEREIGN_ATLAS_GROUNDING` | on | Atlas graph-walk grounding (cosine seeds → BFS over typed edges → FTS-fetch evidence chunks). =0/false/off/no disables. |
| query_decomp | `SOVEREIGN_QUERY_DECOMP` | off | Pure-Rust question decomposition; each sub-query gets its own focused retrieval pass. |
| query_decomp | `SOVEREIGN_DECOMP_DECAY` | 1.0 | Score decay applied to fanned-out sub-query hits (<1 = augment, never displace). |
| title_expand | `SOVEREIGN_TITLE_EXPAND` | off | Fast-slot LLM names explicit article titles for abstract questions; titles are fan-out-searched and reserved through the merge. |
| atom_enum | `SOVEREIGN_ATOM_ENUM` | off | Enumeration-class questions get the corpus's top-degree typed atoms injected as virtual chunks (post-floor). |
| atom_enum | `SOVEREIGN_ATOM_ENUM_TOPK` | see helper | How many enumerated atoms become virtual chunks. |
| atom_enum | `SOVEREIGN_ATOM_ENUM_POOL` | see helper | Candidate-pool cap before ranking. |
| atom_enum | `SOVEREIGN_ATOM_ENUM_RANK` | rrf | Atom ranking mode. |
| atom_enum | `SOVEREIGN_ATOM_ENUM_SCORE` | see helper | Score stamped on enumerated virtual chunks. |
| atom_enum | `SOVEREIGN_ATOM_ENUM_NOFILTER` | off | Disable the enumeration-question classifier filter. |
| atom_enum | `SOVEREIGN_ATOM_ENUM_RELATIONS` | off | Include relation atoms in the enumeration. |
| atom_enum | `SOVEREIGN_ATOM_ENUM_OVERVIEW` | on | Overview/summary questions ("most important thing in X", "summarize X") inject the scoped corpus's atlas Claim atoms as virtual chunks (the corpus's key points) so the answer grounds on them instead of abstaining over an anchorless pool. Default ON (set =0 to disable). Independent of SOVEREIGN_ATOM_ENUM; detected by question shape (no LLM call). |
| raptor_grounding_early | `SOVEREIGN_RAPTOR_GROUNDING` | on | RAPTOR collapsed-tree summary nodes injected as virtual chunks. SOVEREIGN_RAPTOR_LATE picks early (pre-merge) vs late (post-rerank) injection. |
| raptor_grounding_early | `SOVEREIGN_RAPTOR_LATE` | on | Inject RAPTOR summaries AFTER the leaf pipeline (QA-neutral) instead of pre-merge. |
| raptor_grounding_early | `SOVEREIGN_RAPTOR_TOP_M` | see helper | Top-M summary nodes injected. |
| raptor_grounding_early | `SOVEREIGN_RAPTOR_MIN_LEVEL` | see helper | Minimum tree level for injected summaries. |
| raptor_grounding_early | `SOVEREIGN_RAPTOR_DEDUPE` | see helper | Collapse one entry's multi-level nodes to its best. |
| graph_neighbor_expand | `SOVEREIGN_GRAPH_NEIGHBOR_EXPAND` | off | Axis-aware structural-graph one-hop expansion (per-entity axis neighbors + co-citation bridges). |
| ppr_struct_spawn | `SOVEREIGN_PPR_EXPAND` | on (dark without a reranker) | PPR walk + typed causal/contested edges over the wikipedia link graph propose answer-side articles; a cross-encoder admission gate (requires rerank_fn — SOVEREIGN_RERANK_MODEL_PATH) injects only CE-yes candidates, placed mid-pool. Spawned early, joined late: overlaps the core steps. =0/false/off/no disables (RETRIEVAL_REDESIGN.md S4 attempt log). |
| ppr_struct_expand | `SOVEREIGN_PPR_EXPAND` | on (dark without a reranker) | PPR walk + typed causal/contested edges over the wikipedia link graph propose answer-side articles; a cross-encoder admission gate (requires rerank_fn — SOVEREIGN_RERANK_MODEL_PATH) injects only CE-yes candidates, placed mid-pool. Spawned early, joined late: overlaps the core steps. =0/false/off/no disables (RETRIEVAL_REDESIGN.md S4 attempt log). |
| cap_and_reserve | `SOVEREIGN_MERGE_SELECT` | off | Demand-aware merge composition: entity fetch-obligations + ONE facility-style selector (pins + per-named-entity demand slots + greedy diminishing-returns-per-article) replacing the cap/reserve/truncate heuristic pile. A/B flag for the composition architecture. |
| bridge_boost | `SOVEREIGN_META_BRIDGE` | off | Cross-corpus bridge boost: question entities matching a bridge topic pull the LINKED corpus's framing via typed edges (the 'stereo' view). Built by `sovereign meta-atlas align`. |
| - | `SOVEREIGN_CONV_PPR_WEIGHT` | see helper | Post-pipeline: PPR rerank weight for conversation-corpus chunks. |
| - | `SOVEREIGN_HISTORY_RETRIEVAL` | on | History layer: retrieval over prior conversation turns (=0 disables). |
| - | `SOVEREIGN_COMPACTION_DISABLE` | off | History layer: =1 disables dropped-history compaction. |
| - | `SOVEREIGN_FORENSIC` | off | =1 enables audit_pipeline_stage composition snapshots between steps. |

## Verdict buckets (2026-06-10 flag audit)

- **Validated, default ON** — `SOVEREIGN_ATLAS_GROUNDING`,
`SOVEREIGN_RAPTOR_GROUNDING` (+`_LATE` position),
`SOVEREIGN_HISTORY_RETRIEVAL`; router-side:
`SOVEREIGN_KQ_EFFORT_TIER`, `SOVEREIGN_ROUTER_ROBUST_COARSE`
(both A/B-validated 2026-06-09). Disable only for A/B runs.
- **Experimental, opt-in (default OFF)** — `SOVEREIGN_ATOM_ENUM`
(net-negative on focused enumeration per the 2026-06-04
bench; keep gated), `SOVEREIGN_TITLE_EXPAND` (see
wikipedia_learn/V36_FINDINGS.md), `SOVEREIGN_QUERY_DECOMP`,
`SOVEREIGN_GRAPH_NEIGHBOR_EXPAND`, `SOVEREIGN_COMPACTION_DISABLE`.
Flipping one ON in prod requires its own bench A/B.
- **Tunable parameters** — the `_TOPK/_POOL/_RANK/_SCORE`,
`_TOP_M/_MIN_LEVEL/_DEDUPE`, `DECOMP_DECAY`,
`CONV_PPR_WEIGHT` family. Sub-knobs of their parent feature.
- **Debug / escape hatches** — `SOVEREIGN_FORENSIC` (audit
snapshots), `SOVEREIGN_ATOM_ENUM_NOFILTER` (ablation).
Never set in normal operation.
