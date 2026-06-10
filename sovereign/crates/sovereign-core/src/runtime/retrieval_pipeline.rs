// SPDX-License-Identifier: AGPL-3.0-or-later
//! The retrieval-injection pipeline: ONE declarative, traced runner for
//! the chunk-gathering orchestration that was previously duplicated
//! inline across two ~600-line functions
//! (`handlers/knowledge_query.rs::prepare_knowledge_query_plan` and
//! `retrieval.rs::prepare_knowledge_context`).
//!
//! The injection *steps* (atlas grounding, RAPTOR grounding, entity
//! boost, query decomposition, title expansion, atom enumeration,
//! source expansion, …) were always shared `impl Runtime` methods —
//! the duplication was the **orchestration**: which steps run, in what
//! order, under which gates, with which limits. This module makes that
//! orchestration **data**: a [`RetrievalPipeline`] is an ordered list
//! of named [`RetrievalStep`]s run by one tracing runner, and the two
//! per-intent step lists ([`kq_pipeline`] / [`deep_pipeline`]) pin each
//! handler's exact historical sequence. Phase 1 (this landing) changes
//! structure, not behavior: every step body is a verbatim transplant of
//! the corresponding inline block, calling the same unchanged helpers.
//!
//! # Step tables (the orchestration, as data)
//!
//! These tables ARE the pipelines — `kq_pipeline()` / `deep_pipeline()`
//! reproduce them, and golden tests pin the sequences. Since the
//! Phase 2 convergence (2026-06-09) both pipelines are
//! **per-intent head + the SHARED 12-step core + per-intent tail**.
//!
//! ## Heads (per intent)
//!
//! | pipeline | step | gate | helper |
//! |---|------|------|--------|
//! | KQ | `main_retrieval` | — | `search_corpus_indexes_with_overrides` (K=`KQ_PER_CORPUS_LIMIT`, hot-corpus overrides) |
//! | KQ | `scope_personal_filter` | `scope == "personal"` | prefix retain on the merged pool |
//! | deep | `main_retrieval_mesh` | skipped on attached doc | local search ∥ mesh fan-out (`tokio::join!`), scope filter on local hits, mesh fold + peer attribution |
//! | deep | `store_search` | skipped on attached doc | `StateStore::search_documents`, corpus-type docs only, seal honored |
//!
//! ## Shared core (`shared_core_steps`, both pipelines)
//!
//! | # | step | gate | helper |
//! |---|------|------|--------|
//! | 1 | `entity_boost` | entities found | comparison-aware extractor + higher per-entity K when `is_comparison` (KQ-only intent); plain question-entity extraction otherwise |
//! | 2 | `meta_atlas_boost` | registry present | `meta_atlas_boost` |
//! | 3 | `query_decomp` | `SOVEREIGN_QUERY_DECOMP=1` | `decompose_question` → `fan_out_decomposed_queries` |
//! | 4 | `title_expand` | `SOVEREIGN_TITLE_EXPAND=1` | `expand_question_to_titles` → fan-out; titles kept for reserve |
//! | 5 | `noise_floor` | — | `drop_no_overlap_chunks` |
//! | 6 | `atom_enum` | `SOVEREIGN_ATOM_ENUM=1` | `enumerate_typed_atom_chunks` (post-floor by design) |
//! | 7 | `raptor_grounding_early` | `SOVEREIGN_RAPTOR_GROUNDING` on AND `SOVEREIGN_RAPTOR_LATE=0` | `apply_raptor_grounding` |
//! | 8 | `atlas_grounding` | `SOVEREIGN_ATLAS_GROUNDING` (default on) | `apply_atlas_grounding` + per-corpus trace |
//! | 9 | `reweight_and_sort` | — | `reweight_by_query_relevance` + `cross_corpus_sort_cmp` |
//! | 10 | `graph_neighbor_expand` | `SOVEREIGN_GRAPH_NEIGHBOR_EXPAND=1` | `expand_via_wikipedia_graph` (+ re-reweight/sort) |
//! | 11 | `dedupe_merged` | — | retain on first `(corpus_id, content)` |
//! | 12 | `cap_and_reserve` | — | `cap_chunks_per_article` + comparison (KQ-only) / title / atom-enum / raptor reserves |
//!
//! On attached-document turns the deep pipeline drops the two grounding
//! steps from the core (no query embedding exists) along with its head.
//!
//! ## Tails (per intent)
//!
//! | pipeline | step | helper |
//! |---|------|--------|
//! | KQ | `truncate_merged` | `truncate(KQ_MERGED_LIMIT + raptor_n)` + `after_truncate` + `post_merge` audits |
//! | deep | `truncate_merged` | truncate only (deep's audit is `deep_turn_summary` below) |
//! | deep | `top_sources_expand` | `expand_from_top_sources` (unconditional) + `deep_turn_summary` audit |
//!
//! KQ-only, **outside** the pipeline (consumes the chunk set): empty-
//! result parametric path, evidence shape + `resolve_synthesis_route`,
//! `decide_expansion_strategy` → dominant-source / top-sources
//! expansion, ctx-aware budget clamp, PPR rerank, late RAPTOR, prompt +
//! request assembly. Those stay in `prepare_knowledge_query_plan`.
//! Deep-only, outside the pipeline: provenance/search-method labeling,
//! PPR rerank + late RAPTOR + prompt/history assembly, seal audit,
//! system message, speed pick. Those stay in `prepare_knowledge_context`.
//!
//! # Phase 2 convergence log (2026-06-09; A/B'd via the CI bench gate)
//!
//! - **Deep grounding position**: atlas/RAPTOR moved from pre-floor
//!   (right after main retrieval — where the noise floor could silently
//!   drop zero-overlap virtual grounding chunks) to the KQ post-floor /
//!   post-atom-enum position. The KQ position is the deliberate design.
//! - **KQ dedupe**: `dedupe_merged` now runs on both paths (was
//!   deep-only); KQ's fan-out steps can re-fetch main-retrieval chunks.
//! - **Code-level**: `entity_boost`, `cap_and_reserve`, `dedupe_merged`
//!   unified into shared step fns parameterized by `PipelineState`
//!   (`is_comparison` is only ever true on the KQ pipeline).
//!
//! # Remaining divergences (deliberate or deferred — do NOT converge blind)
//!
//! - **KQ expansion is route-aware** (`decide_expansion_strategy`,
//!   post-pipeline); deep always runs `expand_from_top_sources`.
//!   Genuinely tuned per intent — stays.
//! - **Deep's scope filter is local-hits-only** (pre-mesh-fold); KQ's
//!   filters the whole pool. Filtering deep's folded mesh hits would
//!   change mesh semantics — deferred until the recipe-level
//!   `[corpus] scope` annotation lands.
//! - **Deep's store-search embeds with `embed()`** (no query-side
//!   instruction prefix) while every other query leg uses
//!   `embed_query()`. Likely a latent inconsistency, but benches don't
//!   exercise the store leg, so changing it would be an unmeasured
//!   behavior change — deferred.
//! - **KQ has no mesh leg** — KnowledgeQuery turns never fan out to
//!   mesh peers. A feature gap, not an accident of structure; adding it
//!   is product work, not convergence.
//!
//! # Tracing
//!
//! The runner emits one `tracing::info!(target: "retrieval.pipeline")`
//! event per step with `chunks_before` / `chunks_after` / `delta` —
//! the deterministic structural witness for "which step changed the
//! pool". (Log-string note: the per-step messages that used to differ
//! only by intent prefix — e.g. "KnowledgeQuery: query-decomp
//! retrieval" — are preserved via the state's `label`; the two
//! graph-expansion messages were unified to
//! `retrieval: graph neighbor expansion` with a `label` field, and the
//! Deep path now also emits the meta-atlas / per-corpus post-atlas
//! traces that were previously KQ-only. Pure observability additions;
//! the `retrieval_audit`-target events are byte-compatible.)

use std::collections::HashMap;
use std::mem::take;

use super::*;

// ─── Flag registry ───────────────────────────────────────────────

/// A `SOVEREIGN_*` env knob that gates or tunes a pipeline step.
/// The registry ([`retrieval_pipeline_flags`]) is the single place
/// that enumerates them — the SSOT for "what retrieval knobs exist".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvFlag {
    pub name: &'static str,
    /// Human-readable default ("on", "off", a number…), not a parse.
    pub default: &'static str,
    pub purpose: &'static str,
}

const FLAG_ATLAS_GROUNDING: EnvFlag = EnvFlag {
    name: "SOVEREIGN_ATLAS_GROUNDING",
    default: "on",
    purpose: "Atlas graph-walk grounding (cosine seeds → BFS over typed edges → FTS-fetch evidence chunks). =0/false/off/no disables.",
};
const FLAG_QUERY_DECOMP: EnvFlag = EnvFlag {
    name: "SOVEREIGN_QUERY_DECOMP",
    default: "off",
    purpose: "Pure-Rust question decomposition; each sub-query gets its own focused retrieval pass.",
};
const FLAG_TITLE_EXPAND: EnvFlag = EnvFlag {
    name: "SOVEREIGN_TITLE_EXPAND",
    default: "off",
    purpose: "Fast-slot LLM names explicit article titles for abstract questions; titles are fan-out-searched and reserved through the merge.",
};
const FLAG_ATOM_ENUM: EnvFlag = EnvFlag {
    name: "SOVEREIGN_ATOM_ENUM",
    default: "off",
    purpose: "Enumeration-class questions get the corpus's top-degree typed atoms injected as virtual chunks (post-floor).",
};
const FLAG_RAPTOR_GROUNDING: EnvFlag = EnvFlag {
    name: "SOVEREIGN_RAPTOR_GROUNDING",
    default: "on",
    purpose: "RAPTOR collapsed-tree summary nodes injected as virtual chunks. SOVEREIGN_RAPTOR_LATE picks early (pre-merge) vs late (post-rerank) injection.",
};
const FLAG_GRAPH_NEIGHBOR_EXPAND: EnvFlag = EnvFlag {
    name: "SOVEREIGN_GRAPH_NEIGHBOR_EXPAND",
    default: "off",
    purpose: "Axis-aware structural-graph one-hop expansion (per-entity axis neighbors + co-citation bridges).",
};

/// Every env knob the retrieval pipeline (and its immediate post-steps)
/// reads, with the step it belongs to. Renderable as a doc table
/// (Phase 3). Step name `"-"` marks knobs read *inside* a helper
/// rather than gating a whole step.
// Consumed by the registry-coverage test today; Phase 3 of the
// pipeline-collapse plan renders the docs flag table from it.
pub fn retrieval_pipeline_flags() -> Vec<(&'static str, EnvFlag)> {
    vec![
        ("atlas_grounding", FLAG_ATLAS_GROUNDING),
        ("query_decomp", FLAG_QUERY_DECOMP),
        ("query_decomp", EnvFlag { name: "SOVEREIGN_DECOMP_DECAY", default: "1.0", purpose: "Score decay applied to fanned-out sub-query hits (<1 = augment, never displace)." }),
        ("title_expand", FLAG_TITLE_EXPAND),
        ("atom_enum", FLAG_ATOM_ENUM),
        ("atom_enum", EnvFlag { name: "SOVEREIGN_ATOM_ENUM_TOPK", default: "see helper", purpose: "How many enumerated atoms become virtual chunks." }),
        ("atom_enum", EnvFlag { name: "SOVEREIGN_ATOM_ENUM_POOL", default: "see helper", purpose: "Candidate-pool cap before ranking." }),
        ("atom_enum", EnvFlag { name: "SOVEREIGN_ATOM_ENUM_RANK", default: "rrf", purpose: "Atom ranking mode." }),
        ("atom_enum", EnvFlag { name: "SOVEREIGN_ATOM_ENUM_SCORE", default: "see helper", purpose: "Score stamped on enumerated virtual chunks." }),
        ("atom_enum", EnvFlag { name: "SOVEREIGN_ATOM_ENUM_NOFILTER", default: "off", purpose: "Disable the enumeration-question classifier filter." }),
        ("atom_enum", EnvFlag { name: "SOVEREIGN_ATOM_ENUM_RELATIONS", default: "off", purpose: "Include relation atoms in the enumeration." }),
        ("raptor_grounding_early", FLAG_RAPTOR_GROUNDING),
        ("raptor_grounding_early", EnvFlag { name: "SOVEREIGN_RAPTOR_LATE", default: "on", purpose: "Inject RAPTOR summaries AFTER the leaf pipeline (QA-neutral) instead of pre-merge." }),
        ("raptor_grounding_early", EnvFlag { name: "SOVEREIGN_RAPTOR_TOP_M", default: "see helper", purpose: "Top-M summary nodes injected." }),
        ("raptor_grounding_early", EnvFlag { name: "SOVEREIGN_RAPTOR_MIN_LEVEL", default: "see helper", purpose: "Minimum tree level for injected summaries." }),
        ("raptor_grounding_early", EnvFlag { name: "SOVEREIGN_RAPTOR_DEDUPE", default: "see helper", purpose: "Collapse one entry's multi-level nodes to its best." }),
        ("graph_neighbor_expand", FLAG_GRAPH_NEIGHBOR_EXPAND),
        ("-", EnvFlag { name: "SOVEREIGN_CONV_PPR_WEIGHT", default: "see helper", purpose: "Post-pipeline: PPR rerank weight for conversation-corpus chunks." }),
        ("-", EnvFlag { name: "SOVEREIGN_HISTORY_RETRIEVAL", default: "on", purpose: "History layer: retrieval over prior conversation turns (=0 disables)." }),
        ("-", EnvFlag { name: "SOVEREIGN_COMPACTION_DISABLE", default: "off", purpose: "History layer: =1 disables dropped-history compaction." }),
        ("-", EnvFlag { name: "SOVEREIGN_FORENSIC", default: "off", purpose: "=1 enables audit_pipeline_stage composition snapshots between steps." }),
    ]
}

// ─── Pipeline plumbing ───────────────────────────────────────────

/// What a step reports back to the runner. The runner computes the
/// chunk-count delta itself from the state.
#[derive(Debug, Default)]
pub struct StepOutcome {
    /// Optional human note surfaced on the per-step trace line
    /// (e.g. "late-inject mode — skipped").
    pub note: Option<String>,
}

pub type StepFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = StepOutcome> + Send + 'a>>;

/// A step is a plain `fn` so step lists are cheap, `'static` data.
pub type StepFn =
    for<'a, 'ctx> fn(&'a Runtime, &'a mut PipelineState<'ctx>) -> StepFuture<'a>;

pub struct RetrievalStep {
    pub name: &'static str,
    /// The step's primary `SOVEREIGN_*` gate, if any — registry entry +
    /// surfaced on the trace line. Phase 1: the gate is still *checked
    /// inside the helper* (behavior-preserving); this field documents it.
    pub flag: Option<EnvFlag>,
    pub run: StepFn,
}

pub fn step(name: &'static str, flag: Option<EnvFlag>, run: StepFn) -> RetrievalStep {
    RetrievalStep { name, flag, run }
}

/// Everything the steps read and write. Inputs are borrows from the
/// handler's scope; working/threaded fields are owned so the handler
/// can move them out after the run.
pub struct PipelineState<'ctx> {
    // ── inputs ──
    pub message: &'ctx str,
    pub context: &'ctx ConversationContext,
    pub intent: &'ctx Intent,
    pub scope: Option<&'ctx str>,
    pub enabled_corpora: Option<&'ctx [String]>,
    /// Query-side embedding of the (follow-up-expanded) retrieval query.
    pub embedding: Vec<f32>,
    /// Grounding label: `"KnowledgeQuery"` or `"DeepQuery"` — the label
    /// the atlas/RAPTOR helpers and shared log lines carry.
    pub label: &'static str,
    /// Main-retrieval label: KQ uses `"KnowledgeQuery"`; the deep path
    /// uses `format!("{intent:?}")` (e.g. `SimpleQuery`).
    pub search_label: String,
    // ── working set ──
    pub chunks: Vec<corpus_engine::ScoredChunk>,
    // ── threaded step products ──
    pub hot_corpora: HashMap<String, usize>,
    pub entities: Vec<String>,
    pub is_comparison: bool,
    pub title_expand_titles: Option<Vec<String>>,
    pub meta_atlas_hits: Vec<MetaAtlasHitRecord>,
    // ── deep-only products ──
    /// corpus_id → peer name for mesh-served corpora we don't host.
    pub peer_attribution: HashMap<String, String>,
    pub local_hits: usize,
    pub sources_expanded: usize,
}

impl<'ctx> PipelineState<'ctx> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        message: &'ctx str,
        context: &'ctx ConversationContext,
        intent: &'ctx Intent,
        scope: Option<&'ctx str>,
        embedding: Vec<f32>,
        label: &'static str,
        search_label: String,
    ) -> Self {
        Self {
            message,
            context,
            intent,
            scope,
            enabled_corpora: context.conversation.enabled_corpora.as_deref(),
            embedding,
            label,
            search_label,
            chunks: Vec::new(),
            hot_corpora: HashMap::new(),
            entities: Vec::new(),
            is_comparison: matches!(intent, Intent::ComparisonQuery),
            title_expand_titles: None,
            meta_atlas_hits: Vec::new(),
            peer_attribution: HashMap::new(),
            local_hits: 0,
            sources_expanded: 0,
        }
    }
}

pub struct RetrievalPipeline {
    pub name: &'static str,
    pub steps: Vec<RetrievalStep>,
}

impl RetrievalPipeline {
    pub fn step_names(&self) -> Vec<&'static str> {
        self.steps.iter().map(|s| s.name).collect()
    }

    /// Run every step in order, emitting one structural trace line per
    /// step (the glassbox contract: "which step changed the pool" is
    /// answerable from logs alone, ARCH §0.1/§9).
    pub async fn run(&self, rt: &Runtime, state: &mut PipelineState<'_>) {
        for s in &self.steps {
            let before = state.chunks.len();
            let outcome = (s.run)(rt, state).await;
            let after = state.chunks.len();
            tracing::info!(
                target: "retrieval.pipeline",
                pipeline = self.name,
                step = s.name,
                chunks_before = before,
                chunks_after = after,
                delta = after as i64 - before as i64,
                flag = s.flag.map(|f| f.name).unwrap_or(""),
                note = outcome.note.as_deref().unwrap_or(""),
                "retrieval.pipeline: step"
            );
        }
    }
}

// ─── The two pipelines, as data ──────────────────────────────────

/// The 12-step core both pipelines share (Phase 2 convergence,
/// 2026-06-09): entity/meta-atlas boosts → decomp/title expansion →
/// noise floor → atom-enum → RAPTOR/atlas grounding → reweight/sort →
/// graph expand → dedupe → cap+reserve. Per-intent differences ride
/// `PipelineState` (`is_comparison`, `label`), not separate code.
///
/// Phase 2 behavior changes folded into this core (validated by the
/// CI-bench A/B vs the committed synth baselines):
/// - The deep path's atlas/RAPTOR grounding moved from pre-floor
///   (right after main retrieval) to the KQ position — post-floor,
///   post-atom-enum. The KQ position is the deliberate design: the
///   noise floor drops zero-overlap chunks, and grounding-injected
///   virtual chunks (bag-of-atoms entries, RAPTOR summaries) carry no
///   query-token overlap by construction, so the old deep order could
///   silently drop them at the floor.
/// - `dedupe_merged` now also runs on the KQ path (was deep-only):
///   entity-boost / title-expand fan-outs can return chunks the main
///   retrieval already found; duplicates waste merge slots and inflate
///   `top_source_repeat_count` in the evidence shape.
fn shared_core_steps() -> Vec<RetrievalStep> {
    vec![
        step("entity_boost", None, step_entity_boost),
        step("meta_atlas_boost", None, step_meta_atlas_boost),
        step("query_decomp", Some(FLAG_QUERY_DECOMP), step_query_decomp),
        step("title_expand", Some(FLAG_TITLE_EXPAND), step_title_expand),
        step("noise_floor", None, step_noise_floor),
        step("atom_enum", Some(FLAG_ATOM_ENUM), step_atom_enum),
        step("raptor_grounding_early", Some(FLAG_RAPTOR_GROUNDING), step_raptor_grounding_early),
        step("atlas_grounding", Some(FLAG_ATLAS_GROUNDING), step_atlas_grounding),
        step("reweight_and_sort", None, step_reweight_and_sort),
        step("graph_neighbor_expand", Some(FLAG_GRAPH_NEIGHBOR_EXPAND), step_graph_neighbor_expand),
        step("dedupe_merged", None, step_dedupe_merged),
        step("cap_and_reserve", None, step_cap_and_reserve),
    ]
}

/// KnowledgeQuery / ComparisonQuery: per-intent head (single-corpus
/// main retrieval + personal-scope filter) + the shared core + the
/// KQ truncate (which carries the `post_merge` audit). See the
/// module-doc table.
pub fn kq_pipeline() -> RetrievalPipeline {
    let mut steps = vec![
        step("main_retrieval", None, kq_main_retrieval),
        step("scope_personal_filter", None, kq_scope_personal_filter),
    ];
    steps.extend(shared_core_steps());
    steps.push(step("truncate_merged", None, kq_truncate_merged));
    RetrievalPipeline {
        name: "knowledge_query",
        steps,
    }
}

/// DeepQuery / SimpleQuery: per-intent head (local ∥ mesh retrieval +
/// StateStore search) + the shared core + the deep tail (plain
/// truncate + unconditional top-sources expansion).
///
/// `include_corpus_search = false` is the attached-document
/// short-circuit: the corpus/mesh/store head is skipped AND the two
/// grounding steps are dropped from the core (no query embedding is
/// computed on attached-doc turns — historical behavior preserved),
/// but the entity/decomp/title/floor/merge tail still runs on the
/// empty pool, matching the pre-pipeline control flow.
pub fn deep_pipeline(include_corpus_search: bool) -> RetrievalPipeline {
    let mut steps: Vec<RetrievalStep> = Vec::new();
    let mut core = shared_core_steps();
    if include_corpus_search {
        steps.push(step("main_retrieval_mesh", None, deep_main_retrieval_mesh));
        steps.push(step("store_search", None, deep_store_search));
    } else {
        core.retain(|s| s.name != "raptor_grounding_early" && s.name != "atlas_grounding");
    }
    steps.extend(core);
    steps.push(step("truncate_merged", None, deep_truncate_merged));
    steps.push(step("top_sources_expand", None, deep_top_sources_expand));
    RetrievalPipeline { name: "deep_query", steps }
}

// ─── Shared steps (identical on both paths, modulo the label) ────

fn step_meta_atlas_boost<'a, 'ctx>(
    rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Canonical-entity boost (Move 4). For every question entity
        // that resolves through the cross-corpus canonical registry,
        // fetch focused chunks from the entity's primary + alternative
        // corpora and inject them with a score lift that survives
        // merge truncation. `None` registry / empty matches = no-op.
        st.meta_atlas_hits = rt
            .meta_atlas_boost(&mut st.chunks, &st.entities, st.enabled_corpora)
            .await;
        if !st.meta_atlas_hits.is_empty() {
            let total_added: usize = st.meta_atlas_hits.iter().map(|r| r.chunks_added).sum();
            tracing::info!(
                hits = st.meta_atlas_hits.len(),
                chunks_added = total_added,
                "{}: meta-atlas boost",
                st.label
            );
        }
        StepOutcome::default()
    })
}

fn step_query_decomp<'a, 'ctx>(
    rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Optional question decomposition (gated by env flag). Catches
        // concept axes that proper-noun extraction misses and gives
        // each side of a comparison its own focused pass.
        if let Some(sub_queries) = rt.decompose_question(st.message, st.intent) {
            let added = rt
                .fan_out_decomposed_queries(
                    &sub_queries,
                    &mut st.chunks,
                    "QueryDecomp",
                    st.enabled_corpora,
                )
                .await;
            tracing::info!(
                sub_queries = sub_queries.len(),
                chunks_added = added,
                "{}: query-decomp retrieval",
                st.label
            );
        }
        StepOutcome::default()
    })
}

fn step_title_expand<'a, 'ctx>(
    rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Optional title expansion (gated by SOVEREIGN_TITLE_EXPAND=1).
        // Targets the abstract-question failure mode entity boost +
        // comparison decomp don't reach: questions with zero
        // extractable entities whose answer lives in an article keyed
        // by a concrete noun the question never says. Titles are kept
        // on the state so `cap_and_reserve` can pin their chunks
        // through the merge truncate.
        let titles = rt.expand_question_to_titles(st.message, st.context).await;
        if let Some(t) = &titles {
            let added = rt
                .fan_out_decomposed_queries(t, &mut st.chunks, "TitleExpand", st.enabled_corpora)
                .await;
            tracing::info!(
                titles = ?t,
                chunks_added = added,
                "{}: title-expand retrieval",
                st.label
            );
        }
        st.title_expand_titles = titles;
        audit_pipeline_stage(&st.chunks, "after_title_expand", st.message);
        StepOutcome::default()
    })
}

fn step_noise_floor<'a, 'ctx>(
    _rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Noise floor — drop chunks with zero query-token overlap in
        // title or content. Pure-RRF noise that fills prompt budget
        // without signal. See `drop_no_overlap_chunks` for the
        // v33/v36 design history.
        let pre_floor = st.chunks.len();
        st.chunks = drop_no_overlap_chunks(take(&mut st.chunks), st.message);
        if st.chunks.len() < pre_floor {
            tracing::info!(
                pre_floor,
                post_floor = st.chunks.len(),
                "{}: noise floor dropped no-overlap chunks",
                st.label
            );
        }
        audit_pipeline_stage(&st.chunks, "after_noise_floor", st.message);
        StepOutcome::default()
    })
}

fn step_atom_enum<'a, 'ctx>(
    rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Entity-typed atom enumeration (opt-in SOVEREIGN_ATOM_ENUM=1).
        // Injected POST noise-floor on purpose: the chunks are metadata
        // (no query-token overlap) and the floor would drop them.
        if let Some(atom_chunks) = rt
            .enumerate_typed_atom_chunks(st.message, st.enabled_corpora)
            .await
        {
            tracing::info!(
                count = atom_chunks.len(),
                "{}: atom-enum virtual chunks injected",
                st.label
            );
            st.chunks.extend(atom_chunks);
        }
        StepOutcome::default()
    })
}

fn step_raptor_grounding_early<'a, 'ctx>(
    rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // RAPTOR collapsed-tree grounding, EARLY position
        // (SOVEREIGN_RAPTOR_LATE=0): summaries enter before the merge
        // so they participate in expansion + rerank. The DEFAULT (late
        // on) injects post-rerank instead — that call stays with the
        // prompt-assembly code in the handlers, outside this pipeline.
        if !raptor_late_inject_enabled() {
            rt.apply_raptor_grounding(&st.embedding, &mut st.chunks, st.label, st.enabled_corpora)
                .await;
            StepOutcome::default()
        } else {
            StepOutcome {
                note: Some("late-inject mode — early injection skipped".into()),
            }
        }
    })
}

fn step_atlas_grounding<'a, 'ctx>(
    rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Atlas grounding — graph-walk navigation when the provider
        // exposes the graph layer; bag-of-atoms top-K fallback
        // otherwise. See `apply_atlas_grounding` for the full design.
        rt.apply_atlas_grounding(
            st.message,
            &st.embedding,
            &mut st.chunks,
            st.label,
            st.scope,
            st.enabled_corpora,
        )
        .await;
        // Per-corpus snapshot RIGHT AFTER apply_atlas_grounding
        // returns. Paired with the graph-walk trace inside apply and
        // the post-truncate trace downstream — if counts here match
        // the push trace but diverge post-truncate, the drop is in
        // sort+cap+truncate. (ARCH §0.1)
        let per_corpus: std::collections::BTreeMap<String, usize> = {
            let mut m: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
            for c in &st.chunks {
                *m.entry(c.corpus_id.clone()).or_insert(0) += 1;
            }
            m
        };
        tracing::info!(
            n_chunks = st.chunks.len(),
            per_corpus = ?per_corpus,
            label = st.label,
            "retrieval: post-apply_atlas_grounding (per-corpus)"
        );
        StepOutcome::default()
    })
}

fn step_reweight_and_sort<'a, 'ctx>(
    _rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Reweight by query relevance so in-domain chunks rise above
        // same-RRF-score off-domain ties, then cross-corpus sort.
        reweight_by_query_relevance(&mut st.chunks, st.message);
        st.chunks.sort_by(cross_corpus_sort_cmp);
        StepOutcome::default()
    })
}

fn step_graph_neighbor_expand<'a, 'ctx>(
    rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Optional structural-graph expansion (env-gated inside the
        // helper). Axis-aware: co-citation between two named entities
        // is exactly the bridge-concept signal a comparative answer
        // needs.
        if let Some(neighbors) = rt
            .expand_via_wikipedia_graph(&st.chunks, st.message, st.enabled_corpora)
            .await
        {
            if !neighbors.is_empty() {
                let added = neighbors.len();
                st.chunks.extend(neighbors);
                reweight_by_query_relevance(&mut st.chunks, st.message);
                st.chunks.sort_by(cross_corpus_sort_cmp);
                tracing::info!(
                    added,
                    total = st.chunks.len(),
                    label = st.label,
                    "retrieval: graph neighbor expansion"
                );
            }
        }
        StepOutcome::default()
    })
}

// ─── KnowledgeQuery-specific steps ───────────────────────────────

fn kq_main_retrieval<'a, 'ctx>(
    rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Per-corpus limit `KQ_PER_CORPUS_LIMIT = 20` gives the merge
        // real headroom (see the constant's comment). Hot-corpora
        // pre-merge K boost: corpora the user has been learning from
        // get a wider pool so the cross-corpus merge filter doesn't
        // drop their top results. See `build_per_corpus_k_overrides`.
        st.hot_corpora = collect_hot_corpora(&st.context.conversation.messages);
        let per_corpus_overrides =
            build_per_corpus_k_overrides(&st.hot_corpora, KQ_PER_CORPUS_LIMIT);
        st.chunks = rt
            .search_corpus_indexes_with_overrides(
                &st.embedding,
                st.message,
                KQ_PER_CORPUS_LIMIT,
                &st.search_label,
                per_corpus_overrides.as_ref(),
                st.enabled_corpora,
            )
            .await;
        StepOutcome::default()
    })
}

fn kq_scope_personal_filter<'a, 'ctx>(
    _rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Scope-driven retrieval filter. When the router classifies the
        // query as `scope = "personal"`, restrict the pool to
        // user-owned corpora — without this, conversations-personal
        // hits get crowded out by wikipedia/SEP chunks that match the
        // QUERY SHAPE better than the actual conversation chunks do.
        // TODO: replace prefix match with a recipe-level
        // `[corpus] scope = "personal"` annotation once schema lands.
        if st.scope == Some("personal") {
            const PERSONAL_CORPUS_PREFIXES: &[&str] =
                &["conversations-", "personal-", "journal-", "inner-work-"];
            let before = st.chunks.len();
            st.chunks.retain(|c| {
                PERSONAL_CORPUS_PREFIXES
                    .iter()
                    .any(|p| c.corpus_id.starts_with(p))
            });
            if before != st.chunks.len() {
                tracing::info!(
                    kept = st.chunks.len(),
                    dropped = before - st.chunks.len(),
                    scope = "personal",
                    "KnowledgeQuery: scope-filtered retrieval to personal-corpus prefixes"
                );
            }
        }
        StepOutcome::default()
    })
}

fn step_entity_boost<'a, 'ctx>(
    rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Entity boost — fetch articles named in the question via
        // focused per-entity searches (the embedded query lands on
        // topic-central articles, not entity-biographical ones).
        // For ComparisonQuery (KQ pipeline only — the deep path never
        // carries that intent, so `is_comparison` is always false
        // there): (a) comparison-aware extractor catches lowercase
        // contrast entities the proper-noun heuristic skips;
        // (b) higher per-entity limit so each side of the contrast has
        // candidates before per-entity merge reservation.
        st.entities = if st.is_comparison {
            extract_comparison_entities(st.message)
        } else {
            extract_question_entities(st.message)
        };
        let entity_query_limit = if st.is_comparison {
            COMPARISON_ENTITY_QUERY_LIMIT
        } else {
            ENTITY_QUERY_LIMIT
        };
        if !st.entities.is_empty() {
            let initial_count = st.chunks.len();
            let mut entity_added = 0usize;
            for entity in st.entities.iter().take(MAX_ENTITY_QUERIES) {
                let entity_emb = rt.inference.embed_query(entity).await.unwrap_or_default();
                let entity_chunks = rt
                    .search_corpus_indexes_with_overrides(
                        &entity_emb,
                        entity,
                        entity_query_limit,
                        "EntityBoost",
                        None,
                        st.enabled_corpora,
                    )
                    .await;
                entity_added += entity_chunks.len();
                st.chunks.extend(entity_chunks);
            }
            tracing::info!(
                entities = ?st.entities.iter().take(MAX_ENTITY_QUERIES).collect::<Vec<_>>(),
                initial_count,
                entity_added,
                is_comparison = st.is_comparison,
                "{}: entity-boost retrieval",
                st.label
            );
        }
        StepOutcome::default()
    })
}

fn step_cap_and_reserve<'a, 'ctx>(
    _rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        st.chunks = cap_chunks_per_article(take(&mut st.chunks), MAX_CHUNKS_PER_ARTICLE_AT_MERGE);
        // ComparisonQuery only (KQ pipeline): reserve per-entity slots
        // before truncate so neither side of the contrast can be
        // out-ranked out of the merge (the v20
        // `compare_einstein_newton_gravity` regression). No-op on the
        // deep path (`is_comparison` is always false there).
        if st.is_comparison {
            st.chunks =
                reserve_chunks_per_entity(take(&mut st.chunks), &st.entities, COMPARISON_PER_ENTITY_RESERVE);
        }
        // Title-expand reservation: the upstream step made an
        // intentional source selection the cross-corpus sort must not
        // silently demote (v21b audit: T0/T3/T8).
        if let Some(titles) = &st.title_expand_titles {
            if !titles.is_empty() {
                st.chunks =
                    reserve_chunks_per_entity(take(&mut st.chunks), titles, COMPARISON_PER_ENTITY_RESERVE);
            }
        }
        // Atlas-directed reservation: atom-enum chunks carry no query
        // embedding, sort below every cosine-scored base chunk, and a
        // plain truncate drops them wholesale (the synth-boundary bug:
        // injected N, survived 0). Pin them; same for RAPTOR summaries.
        st.chunks = reserve_atom_enum_chunks(take(&mut st.chunks));
        st.chunks = reserve_raptor_chunks(take(&mut st.chunks));
        audit_pipeline_stage(&st.chunks, "after_cap_and_reserve", st.message);
        StepOutcome::default()
    })
}

fn kq_truncate_merged<'a, 'ctx>(
    _rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Additive RAPTOR: reserved collapsed-tree summaries get slots
        // ON TOP of the full leaf budget so they supplement rather than
        // crowd out (measured −14pts SEP source coverage otherwise).
        let raptor_n = st
            .chunks
            .iter()
            .filter(|c| c.metadata.get("source").map(|s| s == "raptor").unwrap_or(false))
            .count();
        st.chunks.truncate(KQ_MERGED_LIMIT + raptor_n);
        audit_pipeline_stage(&st.chunks, "after_truncate", st.message);

        // Naturalistic audit — post-merge composition. Answers "after
        // cap + truncate, which corpus and which article actually has
        // shelf space in the prompt?" Separates merge-layer starvation
        // from cap-layer starvation.
        {
            let mut by_corpus: HashMap<String, usize> = HashMap::new();
            let mut by_article: HashMap<(String, String), usize> = HashMap::new();
            for c in &st.chunks {
                *by_corpus.entry(c.corpus_id.clone()).or_insert(0) += 1;
                *by_article
                    .entry((c.corpus_id.clone(), c.title.clone().unwrap_or_default()))
                    .or_insert(0) += 1;
            }
            let mut corpus_pairs: Vec<(String, usize)> = by_corpus.into_iter().collect();
            corpus_pairs.sort_by(|a, b| b.1.cmp(&a.1));
            let mut article_pairs: Vec<((String, String), usize)> =
                by_article.into_iter().collect();
            article_pairs.sort_by(|a, b| b.1.cmp(&a.1));
            let article_top: Vec<(String, String, usize)> = article_pairs
                .into_iter()
                .take(5)
                .map(|((cid, t), n)| (cid, t, n))
                .collect();
            // Atom-enum survival — the load-bearing glassbox number for
            // the atlas-directs-retrieval contract. 0 here with a
            // non-zero "injected count=N" upstream is the
            // synth-boundary bug; N means the reservation pinned them.
            let atom_enum_survived = st
                .chunks
                .iter()
                .filter(|c| {
                    c.metadata
                        .get("source")
                        .map(|s| s == "atom-enum")
                        .unwrap_or(false)
                })
                .count();
            tracing::info!(
                target: "retrieval_audit",
                event = "post_merge",
                total = st.chunks.len(),
                atom_enum_survived,
                by_corpus = ?corpus_pairs,
                top5_articles = ?article_top,
                "retrieval_audit: post_merge"
            );
        }
        StepOutcome::default()
    })
}

// ─── DeepQuery-specific steps ────────────────────────────────────

fn deep_main_retrieval_mesh<'a, 'ctx>(
    rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Local corpus search and mesh fan-out run concurrently — the
        // mesh call does HTTP (up to ~3s budget per peer), the local
        // call is LanceDB disk I/O. K calibration mirrors
        // KnowledgeQuery (`KQ_PER_CORPUS_LIMIT`).
        st.hot_corpora = collect_hot_corpora(&st.context.conversation.messages);
        let per_corpus_overrides =
            build_per_corpus_k_overrides(&st.hot_corpora, KQ_PER_CORPUS_LIMIT);
        let local_corpora_fut = rt.search_corpus_indexes_with_overrides(
            &st.embedding,
            st.message,
            KQ_PER_CORPUS_LIMIT,
            &st.search_label,
            per_corpus_overrides.as_ref(),
            st.enabled_corpora,
        );
        let mesh_fut = async {
            match &rt.mesh_knowledge {
                Some(m) => {
                    m.search(st.message, &st.embedding, KQ_PER_CORPUS_LIMIT)
                        .await
                }
                None => Vec::new(),
            }
        };
        let (mut local_scored, mesh_scored) = tokio::join!(local_corpora_fut, mesh_fut);

        // Scope filter applies to LOCAL hits only (mesh hits are folded
        // after it — historical behavior, preserved). Prefix match is
        // the same TODO placeholder as the KQ step.
        if matches!(st.scope, Some("personal")) {
            const PERSONAL_CORPUS_PREFIXES: &[&str] =
                &["conversations-", "personal-", "journal-", "inner-work-"];
            let before = local_scored.len();
            local_scored.retain(|c| {
                PERSONAL_CORPUS_PREFIXES
                    .iter()
                    .any(|p| c.corpus_id.starts_with(p))
            });
            tracing::info!(
                kept = local_scored.len(),
                dropped = before.saturating_sub(local_scored.len()),
                scope = ?st.scope,
                label = %st.search_label,
                "prepare_knowledge_context: scope-filtered retrieval to personal-corpus prefixes"
            );
        }

        st.local_hits = local_scored.len();
        // Glass-box log: how many hits from local vs. mesh, and which
        // corpora did mesh claim to serve? If mesh_hits > 0 but
        // `peer_tagged` is 0, the mesh is only round-tripping local
        // corpora. If both are 0 with a live mesh, the handler on
        // :9741 is either not running or returning empty.
        let peer_tagged = mesh_scored.iter().filter(|h| h.peer_name.is_some()).count();
        let mesh_corpora: std::collections::BTreeSet<&str> =
            mesh_scored.iter().map(|h| h.corpus_id.as_str()).collect();
        tracing::info!(
            local_hits = local_scored.len(),
            mesh_hits = mesh_scored.len(),
            mesh_peer_tagged = peer_tagged,
            mesh_corpora = ?mesh_corpora,
            "runtime: knowledge fan-out summary"
        );
        st.chunks.extend(local_scored);

        // Fold mesh hits in, tagging peer attribution per corpus. A
        // corpus that already appears locally doesn't get tagged — we
        // own it, mesh is just parroting.
        let local_corpora_ids: std::collections::HashSet<String> =
            st.chunks.iter().map(|c| c.corpus_id.clone()).collect();
        for hit in mesh_scored {
            if !local_corpora_ids.contains(&hit.corpus_id) {
                if let Some(name) = &hit.peer_name {
                    st.peer_attribution
                        .entry(hit.corpus_id.clone())
                        .or_insert_with(|| name.clone());
                }
            }
            // Stamp peer attribution on the chunk itself so eval
            // --inspect / desktop hit panels can show "peer:<name>".
            let mut metadata = HashMap::new();
            if let Some(name) = &hit.peer_name {
                metadata.insert("peer".to_string(), name.clone());
                metadata.insert("source".to_string(), "mesh".to_string());
            }
            st.chunks.push(corpus_engine::ScoredChunk {
                content: hit.content,
                title: hit.title,
                url: hit.url,
                corpus_id: hit.corpus_id,
                score: hit.score,
                metadata,
                chunk_id: hit.chunk_id,
                source_doc_id: hit.source_doc_id,
                // Mesh-served hits don't carry vector_distance over the
                // wire today; the cross-corpus merge falls back to
                // score-sort for them.
                vector_distance: None,
            });
        }
        StepOutcome::default()
    })
}

fn deep_store_search<'a, 'ctx>(
    rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Also search StateStore for corpus-type documents (used by the
        // test harness and for corpora ingested directly into the
        // store). User-uploaded documents are NOT included.
        let embedding = rt.inference.embed(st.message).await.unwrap_or_default();
        let store_chunks = rt
            .store
            .search_documents(&embedding, st.message, 5)
            .await
            .unwrap_or_default();
        for doc in &store_chunks {
            let SourceType::Corpus { corpus_id } = &doc.source_type else {
                continue;
            };
            // Honor the per-conversation isolate seal — this StateStore
            // path was the one gap outside `apply_corpus_allow_list`.
            if let Some(allow) = st.enabled_corpora {
                if !allow.iter().any(|c| c == corpus_id) {
                    continue;
                }
            }
            st.chunks.push(corpus_engine::ScoredChunk {
                content: doc.content.clone(),
                title: Some(doc.source.clone()),
                url: None,
                corpus_id: corpus_id.clone(),
                score: 0.5,
                metadata: HashMap::new(),
                chunk_id: None,
                source_doc_id: None,
                vector_distance: None,
            });
        }
        StepOutcome::default()
    })
}

fn step_dedupe_merged<'a, 'ctx>(
    _rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Dedupe by (corpus_id, content) before cap + truncate. Deep:
        // a corpus that appears both locally and via mesh must not
        // waste context budget on duplicate chunks. KQ (Phase 2): the
        // entity-boost / title-expand fan-outs can return chunks the
        // main retrieval already found.
        let mut seen: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        st.chunks
            .retain(|c| seen.insert((c.corpus_id.clone(), c.content.clone())));
        StepOutcome::default()
    })
}

fn deep_truncate_merged<'a, 'ctx>(
    _rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Additive RAPTOR — see the KQ merge site for the rationale:
        // reserved summaries get slots on top of the leaf budget.
        let raptor_n = st
            .chunks
            .iter()
            .filter(|c| c.metadata.get("source").map(|s| s == "raptor").unwrap_or(false))
            .count();
        st.chunks.truncate(KQ_MERGED_LIMIT + raptor_n);
        StepOutcome::default()
    })
}

fn deep_top_sources_expand<'a, 'ctx>(
    rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Multi-source cohesion expansion. DeepQuery is the path
        // multi-article synthesis questions take, so pulling depth from
        // the top-N source documents pays off here. The expander
        // returns the initial set unchanged when fewer than 2 distinct
        // titled sources appear, so it's safe to call unconditionally.
        let (expanded, sources_expanded, _total_fetched) =
            rt.expand_from_top_sources(take(&mut st.chunks)).await;
        st.chunks = expanded;
        st.sources_expanded = sources_expanded;

        // DeepQuery/Simple glassbox (opt-in via the `retrieval_audit`
        // target). Emit the FINAL composition so cross-corpus dilution
        // is diagnosable: `final_by_corpus` answers "did the target
        // corpus survive the merge?"
        if tracing::enabled!(target: "retrieval_audit", tracing::Level::INFO) {
            use std::collections::HashSet;
            let mut by_corpus: HashMap<String, usize> = HashMap::new();
            let mut seen: HashSet<String> = HashSet::new();
            for c in &st.chunks {
                *by_corpus.entry(c.corpus_id.clone()).or_insert(0) += 1;
                seen.insert(
                    c.source_doc_id
                        .clone()
                        .or_else(|| c.title.clone())
                        .unwrap_or_default(),
                );
            }
            let mut corpus_pairs: Vec<(String, usize)> = by_corpus.into_iter().collect();
            corpus_pairs.sort_by(|a, b| b.1.cmp(&a.1));
            // Atom-enum survival — see the KQ post_merge event for the
            // rationale.
            let atom_enum_survived = st
                .chunks
                .iter()
                .filter(|c| {
                    c.metadata
                        .get("source")
                        .map(|s| s == "atom-enum")
                        .unwrap_or(false)
                })
                .count();
            let query_preview: String = st.message.chars().take(80).collect();
            tracing::info!(
                target: "retrieval_audit",
                event = "deep_turn_summary",
                intent = ?st.intent,
                query = %query_preview,
                final_chunks = st.chunks.len(),
                distinct_sources = seen.len(),
                atom_enum_survived,
                final_by_corpus = ?corpus_pairs,
                sources_expanded,
                meta_atlas_hits = st.meta_atlas_hits.len(),
                "retrieval_audit: deep_turn_summary"
            );
        }
        StepOutcome::default()
    })
}

// ─── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    /// The step order is bench-tuned DATA — a change to either golden
    /// list is a behavior change and needs a bench A/B, not a drive-by
    /// edit. These lists pin the POST-Phase-2 sequences (2026-06-09):
    /// the shared 12-step core, deep grounding at the KQ (post-floor)
    /// position, and dedupe on both paths.
    #[test]
    fn kq_step_sequence_is_pinned() {
        assert_eq!(
            kq_pipeline().step_names(),
            vec![
                "main_retrieval",
                "scope_personal_filter",
                "entity_boost",
                "meta_atlas_boost",
                "query_decomp",
                "title_expand",
                "noise_floor",
                "atom_enum",
                "raptor_grounding_early",
                "atlas_grounding",
                "reweight_and_sort",
                "graph_neighbor_expand",
                "dedupe_merged",
                "cap_and_reserve",
                "truncate_merged",
            ]
        );
    }

    #[test]
    fn deep_step_sequence_is_pinned() {
        assert_eq!(
            deep_pipeline(true).step_names(),
            vec![
                "main_retrieval_mesh",
                "store_search",
                "entity_boost",
                "meta_atlas_boost",
                "query_decomp",
                "title_expand",
                "noise_floor",
                "atom_enum",
                "raptor_grounding_early",
                "atlas_grounding",
                "reweight_and_sort",
                "graph_neighbor_expand",
                "dedupe_merged",
                "cap_and_reserve",
                "truncate_merged",
                "top_sources_expand",
            ]
        );
    }

    /// Phase 2 contract: between the per-intent heads and tails, the
    /// two pipelines run the IDENTICAL core slice — per-intent
    /// differences ride `PipelineState`, not divergent step lists.
    #[test]
    fn kq_and_deep_share_the_core_slice() {
        let kq = kq_pipeline().step_names();
        let deep = deep_pipeline(true).step_names();
        // KQ: 2-step head, 1-step tail. Deep: 2-step head, 2-step tail.
        let kq_core = &kq[2..kq.len() - 1];
        let deep_core = &deep[2..deep.len() - 2];
        assert_eq!(kq_core, deep_core);
        assert_eq!(kq_core.len(), 12);
    }

    /// Attached-document turns skip corpus/mesh/atlas/raptor/store but
    /// historically still ran the entity/merge tail on the empty pool —
    /// preserved as data.
    #[test]
    fn deep_attached_doc_variant_skips_corpus_search() {
        let names = deep_pipeline(false).step_names();
        assert_eq!(names.first(), Some(&"entity_boost"));
        assert!(!names.contains(&"main_retrieval_mesh"));
        assert!(!names.contains(&"store_search"));
        assert!(!names.contains(&"atlas_grounding"));
        assert!(!names.contains(&"raptor_grounding_early"));
        assert_eq!(names.len(), deep_pipeline(true).step_names().len() - 4);
    }

    /// Every step-level gate flag must appear in the registry — the
    /// registry is the SSOT a doc table renders from.
    #[test]
    fn registry_covers_every_step_flag() {
        let registry = retrieval_pipeline_flags();
        for pipeline in [kq_pipeline(), deep_pipeline(true)] {
            for s in &pipeline.steps {
                if let Some(flag) = s.flag {
                    assert!(
                        registry.iter().any(|(_, f)| f.name == flag.name),
                        "step {} flag {} missing from retrieval_pipeline_flags()",
                        s.name,
                        flag.name
                    );
                }
            }
        }
    }
}
