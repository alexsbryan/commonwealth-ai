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
//! 2026-06-10 rationalization both pipelines are
//! **the SHARED 3-step head + the SHARED 12-step core + a per-intent
//! tail** — the intent decides HOW to answer (model tier, expansion,
//! synthesis shape), never WHERE knowledge lives.
//!
//! ## Shared head (`shared_head_steps`, both pipelines; the deep
//! attached-doc variant skips it — see below)
//!
//! | # | step | gate | helper |
//! |---|------|------|--------|
//! | 1 | `main_retrieval_mesh` | — | local search ∥ mesh fan-out (`tokio::join!`), local-hits scope filter (feeds `local_hits`), mesh fold + peer attribution |
//! | 2 | `scope_personal_filter` | `scope == "personal"` | whole-pool prefix retain — also drops off-scope mesh strays (local already filtered in-head) |
//! | 3 | `store_search` | — | `StateStore::search_documents` with the shared query embedding, corpus-type docs only, seal honored |
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
//! | deep | `top_sources_expand` | `decide_expansion_strategy(intent, PrimarySynthesis, shape)` → `expand_from_top_sources` + `expansion_decision` / `deep_turn_summary` audits |
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
//! # Divergence resolutions (2026-06-10 archaeology pass)
//!
//! Each former "remaining divergence" was traced to its introducing
//! commit and resolved or explicitly kept:
//!
//! - **Expansion policy — RESOLVED (same policy, now one expression).**
//!   Deep's "unconditional" `expand_from_top_sources` (2026-04-29)
//!   carried an internal ≥2-titled-groups guard that is exactly
//!   `decide_expansion_strategy`'s (2026-05-25) TopSources/NoExpansion
//!   split under `PrimarySynthesis`. The deep tail now calls the SSOT
//!   strategy fn — provably chunk-set-identical (the helper's guard is
//!   strictly tighter than `shape.distinct_sources`, so every strategy
//!   skip is a turn the helper would have no-op'd) — and emits the same
//!   `expansion_decision` audit the KQ planner does. The KQ-only
//!   `DominantSource` arm + budget switching remain KQ-only (route-
//!   dependent, genuinely tuned).
//! - **Scope-filter shape — CONVERGED.** The local-hits-only shape
//!   (2026-05-17) was an accident of where the variable lived, not a
//!   mesh-semantics decision. Both pipelines now run the shared
//!   whole-pool `scope_personal_filter` step; on deep it additionally
//!   drops mesh strays on personal-scope turns (mesh peers structurally
//!   never serve personal corpora, so those hits are off-scope noise by
//!   construction). The in-head local filter stays to keep `local_hits`
//!   provenance counts unchanged.
//! - **Store-search embedding — CONVERGED.** Plain `embed(message)` was
//!   a missed retrofit from 2026-05-18 (when the corpus leg moved to
//!   `embed_query(retrieval_query)`); the store leg now reuses the
//!   pipeline's query embedding — query-consistent with every other leg
//!   and one less embed call. (Store-corpus docs are real prod data:
//!   the gutenberg/parquet store-ingest paths write them.)
//! - **KQ mesh + store legs — RESOLVED (first-principles decision,
//!   user-directed 2026-06-10).** KnowledgeQuery turns never fanned out
//!   to mesh peers or searched StateStore corpus docs; Deep/Simple
//!   turns have since 2026-04-21. There was no principled reason — the
//!   mesh leg landed on the then-only retrieval path and the KQ planner,
//!   carved out later, never inherited it. Both pipelines now run the
//!   identical `shared_head_steps()`: which knowledge sources exist is
//!   a property of the INSTALL (corpora + mesh + store), not of the
//!   intent label. Environments without a mesh (`mesh_knowledge: None`,
//!   every bench) and without store-ingested corpora see byte-identical
//!   behavior. Follow-up: the KQ plan does not yet surface mesh peer
//!   attribution in its provenance (`search_method` labels live on the
//!   deep handler) — wire when KQ provenance grows a mesh story.
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
    purpose:
        "Pure-Rust question decomposition; each sub-query gets its own focused retrieval pass.",
};
const FLAG_DEMAND_PLAN: EnvFlag = EnvFlag {
    name: "SOVEREIGN_DEMAND_PLAN",
    default: "off",
    purpose: "One Housekeep fast-slot structured-output call plans the turn's demands (sub_queries, entities, optional stance contrast + section terms). Sub-queries fan out; entities merge into entity_boost; the plan feeds the epistemic demand set (EPISTEMIC_STATE.md P1b / RETRIEVAL_REDESIGN S2). One model, two producers with the deterministic facets.",
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
const FLAG_META_BRIDGE: EnvFlag = EnvFlag {
    name: "SOVEREIGN_META_BRIDGE",
    default: "off",
    purpose: "Cross-corpus bridge boost: question entities matching a bridge topic pull the LINKED corpus's framing via typed edges (the 'stereo' view). Built by `sovereign meta-atlas align`.",
};
const FLAG_PPR_EXPAND: EnvFlag = EnvFlag {
    name: "SOVEREIGN_PPR_EXPAND",
    default: "on (dark without a reranker)",
    purpose: "PPR walk + typed causal/contested edges over the wikipedia link graph propose answer-side articles; a cross-encoder admission gate (requires rerank_fn — SOVEREIGN_RERANK_MODEL_PATH) injects only CE-yes candidates, placed mid-pool. Spawned early, joined late: overlaps the core steps. =0/false/off/no disables (RETRIEVAL_REDESIGN.md S4 attempt log).",
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
        ("demand_plan", FLAG_DEMAND_PLAN),
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
        ("atom_enum", EnvFlag { name: "SOVEREIGN_ATOM_ENUM_OVERVIEW", default: "on", purpose: "Overview/summary questions (\"most important thing in X\", \"summarize X\") inject the scoped corpus's atlas Claim atoms as virtual chunks (the corpus's key points) so the answer grounds on them instead of abstaining over an anchorless pool. Default ON (set =0 to disable). Independent of SOVEREIGN_ATOM_ENUM; detected by question shape (no LLM call)." }),
        ("raptor_grounding_early", FLAG_RAPTOR_GROUNDING),
        ("raptor_grounding_early", EnvFlag { name: "SOVEREIGN_RAPTOR_LATE", default: "on", purpose: "Inject RAPTOR summaries AFTER the leaf pipeline (QA-neutral) instead of pre-merge." }),
        ("raptor_grounding_early", EnvFlag { name: "SOVEREIGN_RAPTOR_TOP_M", default: "see helper", purpose: "Top-M summary nodes injected." }),
        ("raptor_grounding_early", EnvFlag { name: "SOVEREIGN_RAPTOR_MIN_LEVEL", default: "see helper", purpose: "Minimum tree level for injected summaries." }),
        ("raptor_grounding_early", EnvFlag { name: "SOVEREIGN_RAPTOR_DEDUPE", default: "see helper", purpose: "Collapse one entry's multi-level nodes to its best." }),
        ("graph_neighbor_expand", FLAG_GRAPH_NEIGHBOR_EXPAND),
        ("ppr_struct_spawn", FLAG_PPR_EXPAND),
        ("ppr_struct_expand", FLAG_PPR_EXPAND),
        ("cap_and_reserve", EnvFlag { name: "SOVEREIGN_MERGE_SELECT", default: "on", purpose: "Demand-aware merge composition: entity fetch-obligations + ONE facility-style selector (pins + per-named-entity demand slots + greedy diminishing-returns-per-article with within-article strength floor) replacing the cap/reserve/truncate heuristic pile. =0/false/off/no restores the legacy stack." }),
        ("bridge_boost", FLAG_META_BRIDGE),
        ("-", EnvFlag { name: "SOVEREIGN_CONV_PPR_WEIGHT", default: "see helper", purpose: "Post-pipeline: PPR rerank weight for conversation-corpus chunks." }),
        ("-", EnvFlag { name: "SOVEREIGN_HISTORY_RETRIEVAL", default: "on", purpose: "History layer: retrieval over prior conversation turns (=0 disables)." }),
        ("-", EnvFlag { name: "SOVEREIGN_COMPACTION_DISABLE", default: "off", purpose: "History layer: =1 disables dropped-history compaction." }),
        ("-", EnvFlag { name: "SOVEREIGN_FORENSIC", default: "off", purpose: "=1 enables audit_pipeline_stage composition snapshots between steps." }),
        ("-", EnvFlag { name: "SOVEREIGN_EPISTEMIC_STATE", default: "on", purpose: "Post-pipeline: assemble the per-turn epistemic ledger (EPISTEMIC_STATE.md) into message metadata. Pure collation, no model calls; =0 disables." }),
        ("-", EnvFlag { name: "SOVEREIGN_COVERAGE_PROBE", default: "on", purpose: "Post-pipeline, gap/abstain turns only: cross-corpus nearest-chunk-cosine probe classifying a gap as TopicUncovered vs ClaimUncovered. =0 disables." }),
        ("-", EnvFlag { name: "SOVEREIGN_COVERAGE_NEAR_SIM", default: "0.55", purpose: "Similarity floor for the coverage probe's TopicUncovered/ClaimUncovered split (calibrate against the chaos absent banks)." }),
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
pub type StepFn = for<'a, 'ctx> fn(&'a Runtime, &'a mut PipelineState<'ctx>) -> StepFuture<'a>;

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

/// A stance contrast the demand planner detected — the axis two
/// positions disagree on plus the two poles. Retrieval groundwork for
/// contested/synthesis questions (RETRIEVAL_REDESIGN S2); reuses the
/// `comparison_axis` vocabulary in the planner prompt hint.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StanceContrast {
    pub axis: String,
    /// Exactly two poles when present.
    pub poles: Vec<String>,
}

/// The demand planner's structured output (EPISTEMIC_STATE.md P1b /
/// RETRIEVAL_REDESIGN S2). One Housekeep fast-slot call fills this; the
/// pipeline fans out `sub_queries`, merges `entities` into
/// `entity_boost`, and the epistemic assembler turns the whole plan into
/// the turn's demand set — one demand model, two producers with the
/// deterministic facets.
#[derive(Debug, Clone, Default)]
pub struct DemandPlan {
    pub sub_queries: Vec<String>,
    pub entities: Vec<String>,
    pub stance_contrast: Option<StanceContrast>,
    pub section_terms: Vec<String>,
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
    /// User's per-conversation corpus selection (display + Filter 4).
    /// CLIENT-CONTROLLED and forgeable — NOT a security boundary. Drives
    /// routing (`is_governance_turn`/`is_proxy_turn`) and the user-facing
    /// allow-list; `None` ⇒ "all installed corpora".
    pub enabled_corpora: Option<&'ctx [String]>,
    /// Per-principal retrieval ceiling (`{Org} ∪ {Private owned by the
    /// principal}`) — the airtight upper bound applied as `corpus_ceiling`
    /// Filter 5 at every chunk search, INDEPENDENT of the forgeable
    /// `enabled_corpora` selection above. `None` on the single-user path
    /// (no principal) ⇒ no ceiling, retrieval bit-identical to pre-feature.
    /// See `ConversationContext::corpus_ceiling`.
    pub corpus_ceiling: Option<&'ctx [String]>,
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
    /// The demand planner's output (I4-A, `SOVEREIGN_DEMAND_PLAN`). `None`
    /// when the planner is off or the turn skipped it (simple/factual).
    /// The `title_expand_titles` precedent: a step product retained on the
    /// state for downstream consumers (entity_boost merge + the epistemic
    /// demand set).
    pub demand_plan: Option<DemandPlan>,
    pub title_expand_titles: Option<Vec<String>>,
    pub meta_atlas_hits: Vec<MetaAtlasHitRecord>,
    /// In-flight PPR structural-expansion lane (spawned right after
    /// `entity_boost`, joined at `ppr_struct_expand`) — the lane is
    /// pool-independent, so it overlaps the core grounding steps
    /// instead of serializing after them.
    pub ppr_pending: Option<tokio::task::JoinHandle<Vec<corpus_engine::ScoredChunk>>>,
    /// In-flight entity-obligations fetch (merge-select architecture;
    /// spawned with the PPR lane, joined with it). Question-named
    /// entities title-resolved and title-fetched directly — supply for
    /// the merge selector's demand slots.
    pub obligations_pending: Option<tokio::task::JoinHandle<Vec<corpus_engine::ScoredChunk>>>,
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
            corpus_ceiling: context.corpus_ceiling.as_deref(),
            embedding,
            label,
            search_label,
            chunks: Vec::new(),
            hot_corpora: HashMap::new(),
            entities: Vec::new(),
            is_comparison: matches!(intent, Intent::ComparisonQuery),
            demand_plan: None,
            title_expand_titles: None,
            meta_atlas_hits: Vec::new(),
            ppr_pending: None,
            obligations_pending: None,
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
impl Runtime {
    /// Atlas dirs of the sealed corpora that are governance-managed —
    /// those carrying a `governance_oplog.jsonl`. Empty for an ordinary
    /// (non-governance) turn, which is exactly what keeps the active-set
    /// filter and the governance gate inert outside governance corpora.
    pub(crate) fn governance_atlas_dirs(
        &self,
        enabled_corpora: Option<&[String]>,
    ) -> Vec<std::path::PathBuf> {
        let Some(engine) = self.corpus_engine.as_ref() else {
            return Vec::new();
        };
        let Some(corpora) = enabled_corpora else {
            return Vec::new();
        };
        corpora
            .iter()
            .filter_map(|cid| {
                let atlas = engine
                    .index_dir()
                    .join(cid)
                    .join(corpus_engine::enrichment::atlas::ATLAS_DIRNAME);
                atlas
                    .join("governance_oplog.jsonl")
                    .exists()
                    .then_some(atlas)
            })
            .collect()
    }

    /// True if any sealed corpus is governance-managed. Such turns take
    /// the `GateSurface::Governance` calibration so the cite-or-abstain
    /// gate is judged against the governance bank, not the general one.
    pub(crate) fn is_governance_turn(&self, enabled_corpora: Option<&[String]>) -> bool {
        !self.governance_atlas_dirs(enabled_corpora).is_empty()
    }

    /// True if any sealed corpus belongs to the proxy-voting family
    /// (`proxy-cik…`). Such turns take the `GateSurface::ProxyArgument`
    /// calibration (its own bank/override) so the cite-or-abstain gate is
    /// judged on the proxy red lines (RL-1: no confabulated opposition for
    /// a management item; RL-2: both sides cited for a shareholder
    /// proposal), not the general KnowledgeQuery bank. Keyed on the
    /// machine-stable `proxy-cik` corpus-id family (FR-2) — the same
    /// load-bearing convention the recipe + setup script install under.
    pub(crate) fn is_proxy_turn(&self, enabled_corpora: Option<&[String]>) -> bool {
        enabled_corpora
            .map(|cs| cs.iter().any(|c| c.starts_with("proxy-cik")))
            .unwrap_or(false)
    }

    /// The cite-or-abstain gate surface for a KnowledgeQuery turn: a
    /// domain surface (Governance / ProxyArgument) when the sealed corpus
    /// is domain-managed, else the general KnowledgeQuery surface. Defined
    /// once so the streaming and non-streaming KQ paths can't diverge on
    /// which bank calibrates the gate.
    pub(crate) fn kq_gate_surface(
        &self,
        enabled_corpora: Option<&[String]>,
    ) -> crate::runtime::grounding::GateSurface {
        use crate::runtime::grounding::GateSurface;
        if self.is_governance_turn(enabled_corpora) {
            GateSurface::Governance
        } else if self.is_proxy_turn(enabled_corpora) {
            GateSurface::ProxyArgument
        } else {
            GateSurface::KnowledgeQuery
        }
    }
}

/// Active-set governance filter (FR-9 RL-3, the no-dead-law red line):
/// for a sealed governance corpus, drop the retrieved chunks of any
/// *amended section* — a section carrying a superseded/retracted rule —
/// so synthesis can only ground its answer in *current law* (the
/// superseding decision lives in its own, kept, section). A strict no-op
/// for every non-governance corpus (no `governance_oplog.jsonl` ⇒ empty
/// dead-law set), so it rides inertly in the shared core. Section-level
/// is aggressive by design: a chunk holds a whole section's rules and we
/// can't excise one rule's sentence, so an amended section's co-located
/// un-amended provisions are dropped too — the precise fix is sub-chunk
/// (atom-span) filtering. Glass-box: logs the drop.
///
/// Positioned after `cap_and_reserve` (post-grounding, pre-truncate): the
/// grounding steps run on the full pool first; dead law is trimmed before
/// the final truncate. NOTE: on the deep path `top_sources_expand` runs
/// after this and could in principle re-introduce a dead-law section;
/// governance questions route KQ/simple in practice, and the FR-9 Lane-B
/// bench is the instrument that would catch any deep-path leak.
fn step_governance_active_set<'a, 'ctx>(
    rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        use corpus_engine::enrichment::governance_view::{chunk_to_section_map, GovernanceView};
        let atlas_dirs = rt.governance_atlas_dirs(st.enabled_corpora);
        if atlas_dirs.is_empty() {
            return StepOutcome::default();
        }
        // A rule's evidence is a *section* id ("sec_00001"), so bridge
        // section → chunk row ids via chapters.json and drop the chunks of
        // every amended (dead-law) section.
        let mut dead_chunks: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let mut dead_section_total = 0usize;
        for atlas in &atlas_dirs {
            let index_root = atlas.parent().unwrap_or(atlas);
            let view = match GovernanceView::from_atlas_dir(atlas) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        atlas = %atlas.display(),
                        error = %e,
                        "governance active-set: could not load view; leaving chunks unfiltered"
                    );
                    continue;
                }
            };
            let dead_sections = view.dead_law_sections();
            if dead_sections.is_empty() {
                continue;
            }
            dead_section_total += dead_sections.len();
            for (chunk_id, section) in chunk_to_section_map(index_root) {
                if dead_sections.contains(&section) {
                    dead_chunks.insert(chunk_id);
                }
            }
        }
        if dead_chunks.is_empty() {
            // Glass-box: a governance turn with dead-law sections but no
            // matching retrieved chunks is worth a trace (missing chapters.json
            // bridge, or the dead sections simply weren't retrieved this turn).
            if dead_section_total > 0 {
                tracing::info!(
                    dead_sections = dead_section_total,
                    "{}: governance active-set found dead-law sections but mapped no chunk ids",
                    st.label
                );
            }
            return StepOutcome::default();
        }
        let before = st.chunks.len();
        st.chunks.retain(|c| {
            c.chunk_id
                .map(|id| !dead_chunks.contains(&id))
                .unwrap_or(true)
        });
        let dropped = before - st.chunks.len();
        if dropped > 0 {
            tracing::info!(
                dropped,
                dead_law_chunks = dead_chunks.len(),
                dead_sections = dead_section_total,
                "{}: governance active-set dropped dead-law chunks (amended sections)",
                st.label
            );
        }
        StepOutcome {
            note: (dropped > 0).then(|| format!("dropped {dropped} dead-law chunk(s)")),
        }
    })
}

/// Why a scoped corpus couldn't serve retrieval — drives the readiness
/// disclosure message below. Mirrors the skip reasons in the eligibility
/// filter (`prepare_knowledge_context`) so what gets skipped is what gets
/// disclosed.
enum ReadinessIssue {
    /// The index build never finished (ingest stalled / sync paused).
    NotBuilt,
    /// The build finished but the vector index was never written.
    NoVectorIndex,
    /// Built with a different embedding model than the one now loaded.
    DimMismatch { built: usize },
}

/// Corpus-readiness glassbox: when retrieval comes up EMPTY, a SCOPED corpus
/// may have been silently SKIPPED because it isn't ready to serve — its index
/// never finished building (ingest stalled / sync paused), its vector index is
/// missing, or it was built with a different embedding model (dims can't
/// compare to the loaded model). Any of these excludes it from `eligible` and
/// leaves `corpora_searched=0`. Without this the model fabricates over a corpus
/// it never searched (KnowledgeQuery) or goes agentic and leaks `<tool_code>`
/// (DeepQuery). Inject a synthetic disclosure chunk so EVERY synthesis path
/// sharing this core relays the actionable cause — and the now-non-empty result
/// also stops the deep path from going agentic. The user learns their corpus is
/// stale/unbuilt instead of getting a confident wrong answer. Reuses
/// `installed_indexes` (the same per-corpus readiness the desktop startup guard
/// probes) and mirrors the eligibility filter; inert whenever retrieval found
/// anything.
fn step_readiness_disclosure<'a, 'ctx>(
    rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        if !st.chunks.is_empty() {
            return StepOutcome::default();
        }
        let loaded_dims = st.embedding.len();
        if loaded_dims == 0 {
            return StepOutcome::default(); // embed unavailable — nothing to compare
        }
        let engine = match rt.corpus_engine.as_ref() {
            Some(e) => e,
            None => return StepOutcome::default(),
        };
        let scoped = st.enabled_corpora;
        // Find the SCOPED corpus (if any) that couldn't serve retrieval, and why.
        // Only disclose a corpus the user actually SCOPED to — an unscoped
        // (search-everything) empty result shouldn't single out one stale corpus
        // the question may have nothing to do with.
        let unready = engine.installed_indexes().await.ok().and_then(|idx| {
            idx.into_iter()
                .filter(|info| scoped.is_some_and(|s| s.iter().any(|c| c == &info.corpus_id)))
                .find_map(|info| {
                    let issue = if !info.indexes_built {
                        ReadinessIssue::NotBuilt
                    } else if !info.vector_index_built {
                        ReadinessIssue::NoVectorIndex
                    } else if info.embedding_dimensions != 0
                        && info.embedding_dimensions != loaded_dims
                    {
                        ReadinessIssue::DimMismatch {
                            built: info.embedding_dimensions,
                        }
                    } else {
                        return None;
                    };
                    Some((info.corpus_id, issue))
                })
        });
        let (corpus, issue) = match unready {
            Some(u) => u,
            None => return StepOutcome::default(),
        };
        // `reason` is the internal log tag (full detail stays in the trace
        // below — glassbox); `cause` is the PLAIN-language phrase the user
        // actually reads. The user-facing phrase deliberately omits embedding
        // dimensions / "vector index" / "SYSTEM NOTE" jargon — those leaked into
        // answers verbatim and read as a cold, broken refusal.
        let (reason, built_dims, cause) = match issue {
            ReadinessIssue::NotBuilt => (
                "index_not_built",
                0,
                "hasn't finished building yet (a sync or import may have paused)",
            ),
            ReadinessIssue::NoVectorIndex => (
                "vector_index_missing",
                0,
                "isn't fully indexed for search yet",
            ),
            ReadinessIssue::DimMismatch { built } => {
                ("dim_mismatch", built, "needs a quick rebuild first")
            }
        };
        tracing::info!(
            target: "retrieval.pipeline",
            corpus = %corpus,
            reason,
            built_dims,
            loaded_dims,
            "{}: readiness glassbox — scoped corpus skipped; injecting rebuild disclosure",
            st.label
        );
        // Assistant GUIDANCE, not a knowledge passage: the model must relay it in
        // its own warm words and never quote/cite it. The old text was prefixed
        // "SYSTEM NOTE" and carried the dim mismatch verbatim, which the model
        // parroted ("...skipped entirely [Source: X]") — a cold refusal the UX
        // judge scored as broken. Keep it brief, warm, and actionable.
        let content = format!(
            "(Assistant guidance — relay this in your own words; do NOT quote it \
             or attach a [Source: ...] citation to it.) The \"{corpus}\" knowledge \
             base the user is asking about can't be searched right now because it \
             {cause}. In one or two warm, plain sentences, let them know you can't \
             answer from it yet and that rebuilding it in Settings → Knowledge → \
             Rebuild will fix it. Do not mention indexes, embedding models, or \
             dimensions, and do not answer from general knowledge or invent an answer."
        );
        st.chunks.push(corpus_engine::ScoredChunk {
            content,
            title: Some("Knowledge base status".to_string()),
            url: None,
            corpus_id: corpus,
            score: 1.0,
            metadata: std::collections::HashMap::new(),
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
        });
        StepOutcome {
            note: Some("injected corpus-readiness rebuild disclosure".to_string()),
        }
    })
}

fn shared_core_steps() -> Vec<RetrievalStep> {
    vec![
        // I4-A: the demand planner runs FIRST in the core so its
        // sub-queries fan out into the pool and its entities are on the
        // state before `entity_boost` merges them. Dark until
        // `SOVEREIGN_DEMAND_PLAN=1`; skips simple/factual turns.
        step("demand_plan", Some(FLAG_DEMAND_PLAN), step_demand_plan),
        // Spawned FIRST in the core (the lane extracts its own
        // entities from the message and seeds from head-pool titles —
        // it does not need `entity_boost`'s products) and joined at
        // `ppr_struct_expand`. Everything between the two positions —
        // entity-boost's per-entity embed+searches, grounding,
        // reweight — is the overlap window that hides the lane's
        // ~1.2s walk/prerank/fetch/gate instead of adding wall.
        step("ppr_struct_spawn", Some(FLAG_PPR_EXPAND), step_ppr_spawn),
        step("entity_boost", None, step_entity_boost),
        step("meta_atlas_boost", None, step_meta_atlas_boost),
        step("bridge_boost", Some(FLAG_META_BRIDGE), step_bridge_boost),
        step("query_decomp", Some(FLAG_QUERY_DECOMP), step_query_decomp),
        step("title_expand", Some(FLAG_TITLE_EXPAND), step_title_expand),
        step("noise_floor", None, step_noise_floor),
        step("atom_enum", Some(FLAG_ATOM_ENUM), step_atom_enum),
        step(
            "raptor_grounding_early",
            Some(FLAG_RAPTOR_GROUNDING),
            step_raptor_grounding_early,
        ),
        step(
            "atlas_grounding",
            Some(FLAG_ATLAS_GROUNDING),
            step_atlas_grounding,
        ),
        step("reweight_and_sort", None, step_reweight_and_sort),
        step(
            "graph_neighbor_expand",
            Some(FLAG_GRAPH_NEIGHBOR_EXPAND),
            step_graph_neighbor_expand,
        ),
        step(
            "ppr_struct_expand",
            Some(FLAG_PPR_EXPAND),
            step_ppr_struct_expand,
        ),
        step("dedupe_merged", None, step_dedupe_merged),
        step("cap_and_reserve", None, step_cap_and_reserve),
        // FR-9: drop dead-law chunks for governance corpora; inert
        // elsewhere. After the cap, before truncate (see fn doc).
        step("governance_active_set", None, step_governance_active_set),
        // Last core step: sees the FINAL post-retrieval state, so an EMPTY
        // result here means a scoped corpus may have been skipped because it
        // isn't ready (index not built, vector index missing, or embed-model/
        // dims mismatch) — inject a rebuild disclosure. Shared by
        // KnowledgeQuery + DeepQuery + ComparisonQuery (all run this core).
        step("readiness_disclosure", None, step_readiness_disclosure),
    ]
}

/// KnowledgeQuery / ComparisonQuery: per-intent head (single-corpus
/// main retrieval + personal-scope filter) + the shared core + the
/// KQ truncate (which carries the `post_merge` audit). See the
/// module-doc table.
pub fn kq_pipeline() -> RetrievalPipeline {
    let mut steps = shared_head_steps();
    steps.extend(shared_core_steps());
    steps.push(step("truncate_merged", None, kq_truncate_merged));
    RetrievalPipeline {
        name: "knowledge_query",
        steps,
    }
}

/// The shared evidence-gathering head (2026-06-10 rationalization,
/// user-directed): local corpora ∥ mesh fan-out → personal-scope
/// filter → StateStore corpus docs. First principles: the intent
/// classification decides HOW to answer (model tier, expansion,
/// synthesis shape) — never WHERE knowledge lives. Before this, KQ
/// turns silently skipped the mesh and the doc store, an accretion
/// artifact (mesh landed 2026-04-21 on the then-only path; the KQ
/// planner was carved out later and never inherited it).
fn shared_head_steps() -> Vec<RetrievalStep> {
    vec![
        step("main_retrieval_mesh", None, step_main_retrieval_mesh),
        step("scope_personal_filter", None, step_scope_personal_filter),
        step("store_search", None, step_store_search),
    ]
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
        steps.extend(shared_head_steps());
    } else {
        core.retain(|s| {
            s.name != "raptor_grounding_early"
                && s.name != "atlas_grounding"
                // No corpus retrieval ⇒ no pool to seed from and no
                // pool to inject into — the PPR lane pair is inert
                // weight on attached-doc turns.
                && s.name != "ppr_struct_spawn"
                && s.name != "ppr_struct_expand"
                // I4-A: the demand planner fans sub-queries into corpus
                // indexes — inert on attached-doc turns (no corpus pool).
                && s.name != "demand_plan"
        });
    }
    steps.extend(core);
    steps.push(step("truncate_merged", None, deep_truncate_merged));
    steps.push(step("top_sources_expand", None, deep_top_sources_expand));
    RetrievalPipeline {
        name: "deep_query",
        steps,
    }
}

// ─── Shared steps (identical on both paths, modulo the label) ────

fn step_bridge_boost<'a, 'ctx>(rt: &'a Runtime, st: &'a mut PipelineState<'ctx>) -> StepFuture<'a> {
    Box::pin(async move {
        // Cross-corpus stereo view (Phase 6, gated SOVEREIGN_META_BRIDGE,
        // default off). For each question entity matching a bridge topic,
        // pull the linked corpus's framing through the typed edge. No-op
        // when the gate is off or the bridge index is empty.
        let added = rt
            .bridge_boost(
                &mut st.chunks,
                &st.entities,
                st.message,
                &st.embedding,
                st.enabled_corpora,
                st.corpus_ceiling,
            )
            .await;
        StepOutcome {
            note: (added > 0).then(|| format!("bridge: +{added} cross-corpus chunks")),
        }
    })
}

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
            .meta_atlas_boost(
                &mut st.chunks,
                &st.entities,
                st.enabled_corpora,
                st.corpus_ceiling,
            )
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

/// `SOVEREIGN_DEMAND_PLAN=1` opts into the LLM demand planner (I4-A).
/// Default OFF — dark until the A/B swing promotes it (RETRIEVAL_REDESIGN
/// §7 measurement discipline).
pub(crate) fn demand_plan_enabled() -> bool {
    std::env::var("SOVEREIGN_DEMAND_PLAN").ok().as_deref() == Some("1")
}

fn step_demand_plan<'a, 'ctx>(rt: &'a Runtime, st: &'a mut PipelineState<'ctx>) -> StepFuture<'a> {
    Box::pin(async move {
        // Dark-first: no work unless explicitly enabled. Simple/factual
        // turns skip the ~0.3–0.8s planner call — the router already sends
        // them elsewhere, but the deep pipeline also carries SimpleQuery,
        // so gate it here too (synthesis-shaped turns only).
        if !demand_plan_enabled() || matches!(st.intent, Intent::SimpleQuery) {
            return StepOutcome::default();
        }
        let Some(plan) = rt
            .formulate_demand_plan(st.message, &st.chunks, st.context)
            .await
        else {
            return StepOutcome::default();
        };
        // Sub-queries fan out through the shared decomposition helper (the
        // same search-and-merge shape query_decomp / title_expand use).
        let added = if plan.sub_queries.is_empty() {
            0
        } else {
            rt.fan_out_decomposed_queries(
                &plan.sub_queries,
                &mut st.chunks,
                "DemandPlan",
                st.enabled_corpora,
                st.corpus_ceiling,
            )
            .await
        };
        tracing::info!(
            sub_queries = plan.sub_queries.len(),
            entities = plan.entities.len(),
            has_stance = plan.stance_contrast.is_some(),
            section_terms = plan.section_terms.len(),
            chunks_added = added,
            "{}: demand-plan retrieval",
            st.label
        );
        // Retained for entity_boost's merge + the epistemic demand set.
        st.demand_plan = Some(plan);
        StepOutcome {
            note: Some(format!("demand plan → +{added} chunks")),
        }
    })
}

fn step_query_decomp<'a, 'ctx>(rt: &'a Runtime, st: &'a mut PipelineState<'ctx>) -> StepFuture<'a> {
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
                    st.corpus_ceiling,
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

fn step_title_expand<'a, 'ctx>(rt: &'a Runtime, st: &'a mut PipelineState<'ctx>) -> StepFuture<'a> {
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
                .fan_out_decomposed_queries(
                    t,
                    &mut st.chunks,
                    "TitleExpand",
                    st.enabled_corpora,
                    st.corpus_ceiling,
                )
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

fn step_noise_floor<'a, 'ctx>(_rt: &'a Runtime, st: &'a mut PipelineState<'ctx>) -> StepFuture<'a> {
    Box::pin(async move {
        // Noise floor — drop chunks with zero query-token overlap in
        // title or content. Pure-RRF noise that fills prompt budget
        // without signal. See `drop_no_overlap_chunks` for the
        // v33/v36 design history.
        let pre_floor = st.chunks.len();
        // GLASSBOX INSTRUMENT (empty-retrieval rescue diagnosis, 2026-06-25):
        // capture the most vector-similar candidate BEFORE the lexical floor so
        // an EMPTIED result can be classified. vector_distance is raw cosine
        // distance (lower = closer). A vector-close chunk (distance < ~0.5)
        // dropped purely for zero query-token overlap means the floor killed an
        // answerable chunk — a lexical-floor-bypass vector top-K would rescue it.
        // All-far (distance high / None) means genuinely off-domain — a bypass
        // would only inject noise. This log decides whether the rescue is built.
        let best_dropped = st
            .chunks
            .iter()
            .map(|c| {
                (
                    c.vector_distance.unwrap_or(f32::INFINITY),
                    c.score,
                    c.title.clone().unwrap_or_default(),
                )
            })
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        st.chunks = drop_no_overlap_chunks(take(&mut st.chunks), st.message);
        if st.chunks.is_empty() && pre_floor > 0 {
            let (vd, sc, title) = best_dropped.unwrap_or((f32::INFINITY, 0.0, String::new()));
            tracing::info!(
                target: "retrieval.pipeline",
                pre_floor,
                query = %st.message,
                best_dropped_vector_distance = vd,
                best_dropped_score = sc,
                best_dropped_title = %title,
                "{}: NOISE FLOOR EMPTIED candidate set — all {} chunks lexically dropped (vector-rescue diagnosis)",
                st.label,
                pre_floor
            );
        }
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

fn step_atom_enum<'a, 'ctx>(rt: &'a Runtime, st: &'a mut PipelineState<'ctx>) -> StepFuture<'a> {
    Box::pin(async move {
        // Entity-typed atom enumeration (opt-in SOVEREIGN_ATOM_ENUM=1).
        // Injected POST noise-floor on purpose: the chunks are metadata
        // (no query-token overlap) and the floor would drop them.
        if let Some(atom_chunks) = rt
            .enumerate_typed_atom_chunks(st.message, st.enabled_corpora, st.corpus_ceiling)
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
            st.corpus_ceiling,
        )
        .await;
        // Per-corpus snapshot RIGHT AFTER apply_atlas_grounding
        // returns. Paired with the graph-walk trace inside apply and
        // the post-truncate trace downstream — if counts here match
        // the push trace but diverge post-truncate, the drop is in
        // sort+cap+truncate. (ARCH §0.1)
        let per_corpus: std::collections::BTreeMap<String, usize> = {
            let mut m: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
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
        // Cross-corpus discipline (env-gated, default no-op): on the now-ranked
        // pool, cap each corpus's contribution and drop chunks below a relative
        // cosine-similarity floor, so a many-corpus fan-out can't bury the one
        // relevant chunk under 32 corpora of near-miss noise.
        apply_cross_corpus_discipline(&mut st.chunks, st.label);
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
            .expand_via_wikipedia_graph(
                &st.chunks,
                st.message,
                st.enabled_corpora,
                st.corpus_ceiling,
            )
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

fn step_ppr_spawn<'a, 'ctx>(rt: &'a Runtime, st: &'a mut PipelineState<'ctx>) -> StepFuture<'a> {
    Box::pin(async move {
        // Spawn the pool-independent lanes (each env-gated inside its
        // helper): the PPR structural-expansion lane (wikipedia_graph
        // + rerank_fn) and the entity-obligations fetch
        // (merge-select). The tasks own Arc clones and a seed
        // snapshot — no pipeline borrow — and join at
        // `ppr_struct_expand`, overlapping every step in between.
        st.ppr_pending =
            rt.spawn_ppr_lane(&st.chunks, st.message, st.enabled_corpora, st.corpus_ceiling);
        st.obligations_pending =
            rt.spawn_entity_obligations(st.message, st.enabled_corpora, st.corpus_ceiling);
        let spawned = match (st.ppr_pending.is_some(), st.obligations_pending.is_some()) {
            (true, true) => Some("ppr + obligations spawned".to_string()),
            (true, false) => Some("ppr lane spawned".to_string()),
            (false, true) => Some("obligations spawned".to_string()),
            (false, false) => None,
        };
        StepOutcome { note: spawned }
    })
}

fn step_ppr_struct_expand<'a, 'ctx>(
    _rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Join the spawned PPR lane and place its gate-admitted
        // chunks mid-pool (synthetic vector_distance), then a plain
        // re-sort integrates them — no reweight pass (which would
        // compound score multipliers on the whole pool). A lane that
        // overruns the deadline is abandoned, not awaited — the
        // pipeline's latency contract wins over a slow expansion.
        const PPR_JOIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(4);
        let join = |handle: Option<tokio::task::JoinHandle<Vec<corpus_engine::ScoredChunk>>>,
                        what: &'static str| {
            let handle = handle?;
            Some(async move {
                match tokio::time::timeout(PPR_JOIN_DEADLINE, handle).await {
                    Ok(Ok(a)) => a,
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, what, "lane task failed — skipping");
                        Vec::new()
                    }
                    Err(_) => {
                        tracing::warn!(
                            deadline_secs = PPR_JOIN_DEADLINE.as_secs(),
                            what,
                            "lane overran the join deadline — abandoned"
                        );
                        Vec::new()
                    }
                }
            })
        };
        let ppr = join(st.ppr_pending.take(), "ppr_expand");
        let obligations = join(st.obligations_pending.take(), "entity_obligations");
        let mut total_added = 0usize;
        if let Some(fut) = ppr {
            let admitted = fut.await;
            if !admitted.is_empty() {
                let placed = crate::runtime::retrieval::query_expansion::place_ppr_admitted(
                    admitted, &st.chunks,
                );
                total_added += placed.len();
                st.chunks.extend(placed);
            }
        }
        if let Some(fut) = obligations {
            // Obligation chunks get the same mid-pool placement as PPR
            // admissions (bare chunks carry no vector_distance and
            // would sort last — the exact tail position the budget
            // trim and dominant-expander eat first).
            let fetched = fut.await;
            if !fetched.is_empty() {
                let placed = crate::runtime::retrieval::query_expansion::place_ppr_admitted(
                    fetched, &st.chunks,
                );
                total_added += placed.len();
                st.chunks.extend(placed);
            }
        }
        if total_added > 0 {
            st.chunks.sort_by(cross_corpus_sort_cmp);
            tracing::info!(
                added = total_added,
                total = st.chunks.len(),
                label = st.label,
                "retrieval: structural + obligation injection"
            );
        }
        StepOutcome::default()
    })
}

// ─── Personal-scope retain decision (SSOT) ───────────────────────

/// Legacy fallback for corpora whose `_corpus_meta.json` predates the
/// `personal_scope` stamp. The stamped flag (`IndexInfo::personal_scope`,
/// from recipe `[retrieval] personal_scope`) is the authoritative
/// signal; this list only keeps pre-stamp conversations/journal corpora
/// retained until their metadata is backfilled. Do NOT grow this list —
/// stamp the corpus instead (the watched-folder manager does this at
/// registration + resume).
const PERSONAL_CORPUS_PREFIXES: &[&str] =
    &["conversations-", "personal-", "journal-", "inner-work-"];

/// The single retain predicate both personal-scope filter sites use.
/// Pure so it unit-tests without an engine.
fn is_personal_corpus(corpus_id: &str, stamped: &std::collections::HashSet<String>) -> bool {
    stamped.contains(corpus_id)
        || PERSONAL_CORPUS_PREFIXES
            .iter()
            .any(|p| corpus_id.starts_with(p))
}

/// Corpus ids whose index metadata declares `personal_scope = true`
/// (watched folders / Obsidian vaults stamp this at registration;
/// recipes via `[retrieval] personal_scope`). Engine-less runtimes and
/// `installed_indexes()` failures degrade to the legacy prefix list —
/// the pre-metadata behavior, never worse.
async fn personal_corpus_ids(rt: &Runtime) -> std::collections::HashSet<String> {
    let Some(engine) = rt.corpus_engine.as_ref() else {
        return Default::default();
    };
    match engine.installed_indexes().await {
        Ok(ix) => ix
            .into_iter()
            .filter(|i| i.personal_scope)
            .map(|i| i.corpus_id)
            .collect(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "personal-scope filter: installed_indexes() failed — \
                 falling back to legacy prefix list only"
            );
            Default::default()
        }
    }
}

// ─── KnowledgeQuery-specific steps ───────────────────────────────

fn step_scope_personal_filter<'a, 'ctx>(
    rt: &'a Runtime,
    st: &'a mut PipelineState<'ctx>,
) -> StepFuture<'a> {
    Box::pin(async move {
        // Scope-driven retrieval filter. When the router classifies the
        // query as `scope = "personal"`, restrict the pool to
        // user-owned corpora — without this, conversations-personal
        // hits get crowded out by wikipedia/SEP chunks that match the
        // QUERY SHAPE better than the actual conversation chunks do.
        // On the deep pipeline this runs on the WHOLE pool after the
        // mesh fold (divergence convergence, 2026-06-10): mesh peers
        // structurally never serve personal corpora (`mesh_sharing =
        // false`), so any mesh hit on a personal-scope turn is
        // off-scope noise — exactly what this filter exists to drop.
        // The in-head local filter (which feeds `local_hits`) already
        // ran; the retain predicate is idempotent, so this step only
        // removes mesh strays there.
        // Retain decision = `is_personal_corpus` (metadata stamp with
        // legacy prefix fallback) — the 2026-06-10 obsidian audit fix:
        // the old prefix-only match silently dropped watched-folder
        // corpora (`watched-<hash>` ids) from personal-scope turns.
        if st.scope == Some("personal") {
            let stamped = personal_corpus_ids(rt).await;
            let before = st.chunks.len();
            st.chunks
                .retain(|c| is_personal_corpus(&c.corpus_id, &stamped));
            if before != st.chunks.len() {
                tracing::info!(
                    kept = st.chunks.len(),
                    dropped = before - st.chunks.len(),
                    stamped_personal = stamped.len(),
                    scope = "personal",
                    "{}: scope-filtered retrieval to personal corpora",
                    st.label
                );
            }
        }
        StepOutcome::default()
    })
}

fn step_entity_boost<'a, 'ctx>(rt: &'a Runtime, st: &'a mut PipelineState<'ctx>) -> StepFuture<'a> {
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
        // I4-A: merge the demand planner's entities (LLM producer) with the
        // deterministic extractor's (one demand model, two producers). The
        // combined set feeds both these per-entity searches and the later
        // merge selector's demand slots.
        if let Some(plan) = &st.demand_plan {
            for e in &plan.entities {
                let e = e.trim();
                if !e.is_empty() && !st.entities.iter().any(|x| x.eq_ignore_ascii_case(e)) {
                    st.entities.push(e.to_string());
                }
            }
        }
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
                        st.corpus_ceiling,
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
        // Merge-select architecture (SOVEREIGN_MERGE_SELECT): ONE
        // demand-aware set-composition objective replaces the whole
        // heuristic pile below (per-article cap → comparison/title/
        // atom/RAPTOR reserve passes → downstream truncate). Pins and
        // RAPTOR additivity are honored inside the selector; the
        // legacy path is byte-identical when the flag is off. See
        // merge_select.rs for the objective and the bucket-1 receipts.
        if merge_select_enabled() {
            st.chunks =
                merge_demand_select(take(&mut st.chunks), &st.entities, KQ_MERGED_LIMIT);
            audit_pipeline_stage(&st.chunks, "after_cap_and_reserve", st.message);
            return StepOutcome {
                note: Some("merge_demand_select".to_string()),
            };
        }
        st.chunks = cap_chunks_per_article(take(&mut st.chunks), MAX_CHUNKS_PER_ARTICLE_AT_MERGE);
        // ComparisonQuery only (KQ pipeline): reserve per-entity slots
        // before truncate so neither side of the contrast can be
        // out-ranked out of the merge (the v20
        // `compare_einstein_newton_gravity` regression). No-op on the
        // deep path (`is_comparison` is always false there).
        if st.is_comparison {
            st.chunks = reserve_chunks_per_entity(
                take(&mut st.chunks),
                &st.entities,
                COMPARISON_PER_ENTITY_RESERVE,
            );
        }
        // Title-expand reservation: the upstream step made an
        // intentional source selection the cross-corpus sort must not
        // silently demote (v21b audit: T0/T3/T8).
        if let Some(titles) = &st.title_expand_titles {
            if !titles.is_empty() {
                st.chunks = reserve_chunks_per_entity(
                    take(&mut st.chunks),
                    titles,
                    COMPARISON_PER_ENTITY_RESERVE,
                );
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
            .filter(|c| {
                c.metadata
                    .get("source")
                    .map(|s| s == "raptor")
                    .unwrap_or(false)
            })
            .count();
        // Gate-admitted structural chunks are additive like RAPTOR's
        // slots (bounded ≤ PPR_MAX_ADMITTED): an admission that
        // displaces a scoring marginal chunk converts a win into a
        // 1-for-1 trade (measured three times, 2026-07-17); the
        // formatter's char budget remains the true ceiling.
        let admitted_n = st
            .chunks
            .iter()
            .filter(|c| {
                c.metadata
                    .get("injected_by")
                    .map(|s| s == "ppr_expand")
                    .unwrap_or(false)
            })
            .count();
        st.chunks.truncate(KQ_MERGED_LIMIT + raptor_n + admitted_n);
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

fn step_main_retrieval_mesh<'a, 'ctx>(
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
            st.corpus_ceiling,
        );
        // Sealed conversations: subtract locally-installed corpora from
        // the mesh seal before fanning out. The local leg above already
        // searches every locally-installed corpus, so a mesh round-trip
        // for those ids only parrots the same index back (the fold
        // comment below: "we own it, mesh is just parroting") — at the
        // cost of a FULL duplicate hybrid search through the daemon
        // (measured as the +16-dup dedupe delta and a large share of
        // the 3.6-9s/question parity-lane latency, RETRIEVAL_REDESIGN.md
        // §8 P1). Locality check is a stat on the corpus dir — a miss
        // (corpus under a non-basename dir) safely falls back to
        // including the id in the fan-out. Unsealed (`None`) keeps the
        // full broad-research fan-out unchanged.
        let mesh_seal_remote: Option<Vec<String>> = match (st.enabled_corpora, &rt.corpus_engine) {
            (Some(allow), Some(engine)) => {
                let index_dir = engine.index_dir();
                Some(
                    allow
                        .iter()
                        .filter(|id| !index_dir.join(id.as_str()).join("chunks.lance").exists())
                        .cloned()
                        .collect(),
                )
            }
            (Some(allow), None) => Some(allow.to_vec()),
            (None, _) => None,
        };
        let mesh_fut = async {
            match (&rt.mesh_knowledge, &mesh_seal_remote) {
                // Every sealed corpus is local — the mesh has nothing
                // non-redundant to add; skip the round-trip entirely.
                (Some(_), Some(remote)) if remote.is_empty() => {
                    tracing::info!(
                        label = %st.search_label,
                        "retrieval: mesh fan-out skipped — all sealed corpora are local"
                    );
                    Vec::new()
                }
                (Some(m), Some(remote)) => {
                    m.search(st.message, &st.embedding, KQ_PER_CORPUS_LIMIT, Some(remote))
                        .await
                }
                // Pass the conversation seal so the mesh fan-out's
                // local-view (and peer) search is scoped at the
                // source. The mesh-fold below (`st.enabled_corpora`
                // guard) only filters the *results*; without sealing
                // here, the fan-out still opens every hosted index
                // first — a 1.9M-row `wikipedia` search that
                // OOM-kills the daemon before the filter runs.
                (Some(m), None) => {
                    m.search(st.message, &st.embedding, KQ_PER_CORPUS_LIMIT, None)
                        .await
                }
                (None, _) => Vec::new(),
            }
        };
        let (mut local_scored, mesh_scored) = tokio::join!(local_corpora_fut, mesh_fut);

        // Scope filter applies to LOCAL hits only (mesh hits are folded
        // after it — historical behavior, preserved). Retain decision =
        // `is_personal_corpus`, the same SSOT predicate as the
        // `scope_personal_filter` step.
        if matches!(st.scope, Some("personal")) {
            let stamped = personal_corpus_ids(rt).await;
            let before = local_scored.len();
            local_scored.retain(|c| is_personal_corpus(&c.corpus_id, &stamped));
            tracing::info!(
                kept = local_scored.len(),
                dropped = before.saturating_sub(local_scored.len()),
                stamped_personal = stamped.len(),
                scope = ?st.scope,
                label = %st.search_label,
                "retrieval: scope-filtered local hits to personal corpora"
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
        let mut mesh_sealed_out = 0usize;
        for hit in mesh_scored {
            // Conversation corpus seal applies to mesh hits too. The
            // local leg filters through `apply_corpus_allow_list`, but
            // mesh hits historically folded in unfiltered — which let a
            // peer's (or this node's own daemon's) wikipedia serve
            // chunks into a conversation sealed to one corpus. Measured
            // 2026-06-11: "1950 Liechtenstein weapons law referendum"
            // chunks reached a sealed-to-one-novel conversation off the
            // word "weapon". Exact-match only: mesh hits don't carry
            // `parent_corpus_id`, so layer corpora of an allowed parent
            // are not retained here — sealed errs restrictive.
            if let Some(allow) = st.enabled_corpora {
                if !allow.iter().any(|c| c == &hit.corpus_id) {
                    mesh_sealed_out += 1;
                    continue;
                }
            }
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
        if mesh_sealed_out > 0 {
            tracing::info!(
                mesh_sealed_out,
                label = %st.search_label,
                "retrieval: conversation seal dropped mesh hits from non-enabled corpora"
            );
        }
        StepOutcome::default()
    })
}

fn step_store_search<'a, 'ctx>(rt: &'a Runtime, st: &'a mut PipelineState<'ctx>) -> StepFuture<'a> {
    Box::pin(async move {
        // Also search StateStore for corpus-type documents (used by the
        // test harness and for corpora ingested directly into the
        // store, e.g. the gutenberg/parquet store-ingest paths). User-
        // uploaded documents are NOT included.
        //
        // Reuses the pipeline's query embedding (divergence
        // convergence, 2026-06-10). This leg historically called plain
        // `embed(message)` — a missed retrofit from 2026-05-18 when the
        // corpus leg switched to `embed_query(retrieval_query)` (the
        // query-side instruction prefix for asymmetric models). Sharing
        // `st.embedding` makes the store leg query-consistent with the
        // corpus leg and drops a redundant embed round-trip.
        let store_chunks = rt
            .store
            .search_documents(&st.embedding, st.message, 5)
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
            .filter(|c| {
                c.metadata
                    .get("source")
                    .map(|s| s == "raptor")
                    .unwrap_or(false)
            })
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
        // the top-N source documents pays off here.
        //
        // The WHICH-expansion decision now goes through the same SSOT
        // policy fn the KQ planner uses (divergence convergence,
        // 2026-06-10) with `route = PrimarySynthesis` — deep IS
        // structurally the primary-synthesis path (SimpleQuery rides it
        // only when corpora hit). This is chunk-set-IDENTICAL to the
        // historical unconditional call: the helper's internal guard
        // (≥ 2 distinct *titled* groups, conversation-history excluded)
        // is strictly tighter than `shape.distinct_sources`, so every
        // turn the strategy skips is a turn the helper would have
        // no-op'd anyway. DominantSource is unreachable under
        // PrimarySynthesis (see `decide_expansion_strategy`'s truth
        // table). Deep's prompt budget stays EXPANDED_KNOWLEDGE_CHARS
        // unconditionally — only KQ varies budget by expansion outcome.
        let shape = compute_evidence_shape(&st.chunks, st.message);
        let (strategy, reason) =
            decide_expansion_strategy(st.intent, SynthesisRoute::PrimarySynthesis, &shape);
        tracing::info!(
            target: "retrieval_audit",
            event = "expansion_decision",
            intent = ?st.intent,
            route = ?SynthesisRoute::PrimarySynthesis,
            strategy = ?strategy,
            reason = reason,
            top_source_repeat = shape.top_source_repeat_count,
            distinct_sources = shape.distinct_sources,
            "retrieval_audit: expansion_decision"
        );
        let (expanded, sources_expanded, _total_fetched) = match strategy {
            ExpansionStrategy::TopSources => rt.expand_from_top_sources(take(&mut st.chunks), st.message).await,
            // NoExpansion (< 2 source keys): the helper would return
            // the set unchanged — skip the call. DominantSource:
            // unreachable here, treated identically for totality.
            ExpansionStrategy::DominantSource | ExpansionStrategy::NoExpansion => {
                (take(&mut st.chunks), 0, 0)
            }
        };
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
    /// the shared 14-step core (incl. the FR-9 governance active-set
    /// filter), deep grounding at the KQ (post-floor) position, and
    /// dedupe on both paths.
    /// I4-A dark-first: the demand planner must be OFF unless explicitly
    /// enabled — every existing surface + bench changes behaviour only by
    /// opt-in (RETRIEVAL_REDESIGN §7).
    #[test]
    fn demand_plan_default_off() {
        std::env::remove_var("SOVEREIGN_DEMAND_PLAN");
        assert!(!super::demand_plan_enabled());
    }

    #[test]
    fn kq_step_sequence_is_pinned() {
        assert_eq!(
            kq_pipeline().step_names(),
            vec![
                "main_retrieval_mesh",
                "scope_personal_filter",
                "store_search",
                "demand_plan",
                "ppr_struct_spawn",
                "entity_boost",
                "meta_atlas_boost",
                "bridge_boost",
                "query_decomp",
                "title_expand",
                "noise_floor",
                "atom_enum",
                "raptor_grounding_early",
                "atlas_grounding",
                "reweight_and_sort",
                "graph_neighbor_expand",
                "ppr_struct_expand",
                "dedupe_merged",
                "cap_and_reserve",
                "governance_active_set",
                "readiness_disclosure",
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
                "scope_personal_filter",
                "store_search",
                "demand_plan",
                "ppr_struct_spawn",
                "entity_boost",
                "meta_atlas_boost",
                "bridge_boost",
                "query_decomp",
                "title_expand",
                "noise_floor",
                "atom_enum",
                "raptor_grounding_early",
                "atlas_grounding",
                "reweight_and_sort",
                "graph_neighbor_expand",
                "ppr_struct_expand",
                "dedupe_merged",
                "cap_and_reserve",
                "governance_active_set",
                "readiness_disclosure",
                "truncate_merged",
                "top_sources_expand",
            ]
        );
    }

    /// Phase 2 contract: between the per-intent heads and tails, the
    /// two pipelines run the IDENTICAL core slice — per-intent
    /// differences ride `PipelineState`, not divergent step lists.
    #[test]
    fn kq_and_deep_share_head_and_core() {
        let kq = kq_pipeline().step_names();
        let deep = deep_pipeline(true).step_names();
        // Shared 3-step head + shared 18-step core (the last core step is
        // `readiness_disclosure` — inert when retrieval found anything;
        // `demand_plan` is the new first core step, I4-A); the pipelines
        // differ ONLY in their tails (KQ: audited truncate; deep: plain
        // truncate + strategy-driven top-sources expansion).
        assert_eq!(&kq[..21], &deep[..21]);
        assert_eq!(kq.len(), 22);
        assert_eq!(deep.len(), 23);
        assert_eq!(kq[21], "truncate_merged");
        assert_eq!(&deep[21..], &["truncate_merged", "top_sources_expand"]);
    }

    /// Attached-document turns skip corpus/mesh/atlas/raptor/store but
    /// historically still ran the entity/merge tail on the empty pool —
    /// preserved as data.
    #[test]
    fn deep_attached_doc_variant_skips_corpus_search() {
        let names = deep_pipeline(false).step_names();
        assert_eq!(names.first(), Some(&"entity_boost"));
        assert!(!names.contains(&"main_retrieval_mesh"));
        assert!(!names.contains(&"scope_personal_filter"));
        assert!(!names.contains(&"store_search"));
        assert!(!names.contains(&"atlas_grounding"));
        assert!(!names.contains(&"raptor_grounding_early"));
        assert!(!names.contains(&"ppr_struct_spawn"));
        assert!(!names.contains(&"ppr_struct_expand"));
        // I4-A: the demand planner is inert without a corpus pool.
        assert!(!names.contains(&"demand_plan"));
        // Head (3) + demand_plan + the 4 corpus-only core steps = 8 fewer.
        assert_eq!(names.len(), deep_pipeline(true).step_names().len() - 8);
    }

    /// The personal-scope retain predicate: metadata stamp first,
    /// legacy prefix fallback second, everything else dropped. Pins
    /// the 2026-06-10 fix — watched-folder corpora (`watched-<hash>`
    /// ids, no recognizable prefix) are retained iff stamped.
    #[test]
    fn personal_scope_predicate_stamp_plus_prefix_fallback() {
        let stamped: std::collections::HashSet<String> =
            ["watched-959ee8a8f330".to_string()].into_iter().collect();

        // Stamped watched-folder corpus: retained (THE fix).
        assert!(is_personal_corpus("watched-959ee8a8f330", &stamped));
        // Unstamped watched-folder corpus (metadata not backfilled
        // yet, e.g. installed_indexes() failed): dropped — degraded
        // mode equals pre-fix behavior, never worse.
        assert!(!is_personal_corpus("watched-deadbeef0000", &stamped));
        // Legacy prefix corpora retained without a stamp.
        assert!(is_personal_corpus("conversations-anthropic", &stamped));
        assert!(is_personal_corpus("journal-2026", &stamped));
        // Reference corpora dropped regardless.
        assert!(!is_personal_corpus("wikipedia", &stamped));
        assert!(!is_personal_corpus("sep", &stamped));
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
