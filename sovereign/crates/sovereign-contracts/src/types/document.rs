// SPDX-License-Identifier: AGPL-3.0-or-later
//! Document / asset / RAPTOR-atlas types — split from the former monolithic
//! `types.rs` (ARCH_PRINCIPLES §3.2). Re-exported by `types/mod.rs`, so every
//! `sovereign_core::types::*` import path is unchanged (behaviour-preserving).
#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use crate::oicp;
#[allow(unused_imports)]
use serde::{Deserialize, Serialize};

// A persistent document that has been ingested once and can be
// queried many times. Lives in the document library alongside
// corpora. The ingest cost is paid once; subsequent queries are
// fast because the embedding index and structural skeleton are
// already built.

/// A document that has been uploaded, parsed, embedded, and
/// structurally analysed. Created by `DocumentAssetManager::ingest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentAsset {
    pub id: String,
    pub title: String,
    pub filename: String,
    pub file_size_mb: f32,
    pub word_count: usize,
    pub chunk_count: usize,
    pub document_type: DocumentTypeTag,
    pub ingested_at: chrono::DateTime<chrono::Utc>,
    /// LanceDB index ID for this document's embedded chunks.
    pub index_id: String,
    /// Structural skeleton — built during ingest, stored permanently.
    /// None until the skeleton phase completes.
    pub skeleton: Option<DocumentSkeleton>,
    pub state: AssetState,
    /// The principal that uploaded this document on a multi-user hub. `None`
    /// for single-user / pre-multi-tenant documents — visible to everyone
    /// (the back-compat default). When set, the document is visible only to
    /// that principal (same deny-set rule the corpus surfaces use).
    #[serde(default)]
    pub owner: Option<String>,
}

impl DocumentAsset {
    /// The source key used to look up this document's chunks in the
    /// `DocumentStore`. For assets ingested via `DocumentAssetManager`,
    /// this is `"asset:{id}"`. For legacy documents promoted from the
    /// old chunks table, this is the original file path stored in
    /// `index_id` (prefixed with `"legacy:"`).
    pub fn source_key(&self) -> String {
        if let Some(original) = self.index_id.strip_prefix("legacy:") {
            original.to_string()
        } else {
            format!("asset:{}", self.id)
        }
    }
}

/// Processing state of a document asset. Drives the UI's progress
/// display and determines which operations are available.
///
/// Tiered retrieval surface (proper-curried-peach plan, 2026-05-22):
/// the state machine exposes three discrete capability tiers between
/// `Pending` and `Ready`. Each tier unlocks a specific retrieval mode
/// without waiting for the next.
///
/// - **PartiallyReady** (T1): chunks + embeddings persisted → cosine
///   top-K retrieval works.
/// - **MultiHopReady** (T2): entity index + action atoms built →
///   personalised-PageRank multi-hop retrieval works.
/// - **Ready** (T3): RAPTOR atlas + motifs + structural metadata
///   built → full briefing-driven synthesis with scene-scale
///   signposts.
///
/// `BuildingSkeleton` is reused as the "in-flight enrichment" state
/// for both the T2 and T3 phases. The progress counter rises through
/// each phase; a `MultiHopReady` milestone fires between them. The
/// progress bar may briefly reset at the milestone — by design (it
/// signals a real capability checkpoint, not just continuous work).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AssetState {
    /// File accepted. Processing not yet started.
    Pending,
    /// Embedding chunks into LanceDB. RAG not yet available.
    Indexing {
        chunks_done: usize,
        chunks_total: usize,
    },
    /// T1 done. Embeddings persisted; cosine retrieval works. T2
    /// enrichment running (entity extraction + action atoms).
    PartiallyReady,
    /// T2 or T3 enrichment in progress. Reused variant — the phase
    /// is implicit in the surrounding state-machine flow (T2 fires
    /// before MultiHopReady; T3 fires after).
    BuildingSkeleton {
        chunks_done: usize,
        chunks_total: usize,
    },
    /// T2 done. Entity index + action atoms available; PPR multi-hop
    /// retrieval works. T3 enrichment (RAPTOR + motifs) running.
    MultiHopReady,
    /// T3 done. All operations available.
    Ready,
    /// Ingest failed.
    Failed { reason: String },
}

impl AssetState {
    /// True when the document has enough indexed data to answer
    /// retrieval queries. All three tiers (T1, T2, T3) qualify —
    /// only the *quality* differs. Pending / Indexing return false
    /// because chunks aren't in the store yet.
    pub fn is_queryable(&self) -> bool {
        matches!(
            self,
            AssetState::PartiallyReady
                | AssetState::BuildingSkeleton { .. }
                | AssetState::MultiHopReady
                | AssetState::Ready
        )
    }

    /// Short human-readable label for the UI.
    pub fn label(&self) -> &'static str {
        match self {
            AssetState::Pending => "Waiting",
            AssetState::Indexing { .. } => "Indexing",
            AssetState::PartiallyReady => "Partially ready",
            AssetState::BuildingSkeleton { .. } => "Building structure",
            AssetState::MultiHopReady => "Multi-hop ready",
            AssetState::Ready => "Ready",
            AssetState::Failed { .. } => "Failed",
        }
    }

    /// Progress as a 0.0–1.0 fraction.
    ///
    /// `MultiHopReady` returns 0.7 — between PartiallyReady's 0.5
    /// and Ready's 1.0, signalling that the second enrichment tier
    /// has landed. `BuildingSkeleton`'s fraction continues to span
    /// 0.5 → 1.0 in both T2 and T3 phases; the bar visually resets
    /// at the MultiHopReady checkpoint, which is intentional — that
    /// reset *is* the visual milestone.
    pub fn progress_fraction(&self) -> Option<f32> {
        match self {
            AssetState::Indexing {
                chunks_done,
                chunks_total,
            } if *chunks_total > 0 => Some(*chunks_done as f32 / *chunks_total as f32 * 0.5),
            AssetState::PartiallyReady => Some(0.5),
            AssetState::BuildingSkeleton {
                chunks_done,
                chunks_total,
            } if *chunks_total > 0 => Some(0.5 + *chunks_done as f32 / *chunks_total as f32 * 0.5),
            AssetState::MultiHopReady => Some(0.7),
            AssetState::Ready => Some(1.0),
            _ => None,
        }
    }
}

/// Coarse classification of a document's genre/type. Influences
/// which skeleton extraction prompts are used and which starter
/// chips are shown in the conversation view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum DocumentTypeTag {
    /// Novels, memoirs, literary non-fiction.
    Narrative,
    /// Dissertations, essays, philosophy.
    Argument,
    /// Legal briefs, scientific papers.
    Evidence,
    /// History, biography, journalism.
    Chronicle,
    /// Manuals, specifications, documentation.
    Technical,
    /// A person's private memory/journal entries (the memory-pool
    /// RAPTOR port). Summaries frame recurring feelings, situations,
    /// and periods in the person's life rather than document
    /// structure.
    Journal,
    /// Not yet classified or doesn't fit a category.
    #[default]
    Unknown,
}

impl DocumentTypeTag {
    pub fn label(&self) -> &'static str {
        match self {
            DocumentTypeTag::Narrative => "Narrative",
            DocumentTypeTag::Argument => "Argument",
            DocumentTypeTag::Evidence => "Evidence",
            DocumentTypeTag::Chronicle => "Chronicle",
            DocumentTypeTag::Technical => "Technical",
            DocumentTypeTag::Journal => "Journal",
            DocumentTypeTag::Unknown => "Document",
        }
    }
}

// ─── Document Skeleton ────────────────────────────────────────
//
// The structural skeleton is built by the ingest pipeline via
// batched LLM inference over the document's chunks. It enables
// synthesis (whole-document analysis) and entity-aware routing
// that plain RAG cannot do.

/// Structural skeleton of a document — entities, sections, and
/// key moments. Built once during ingest, stored permanently.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSkeleton {
    /// Annotated sections with structural function labels.
    pub sections: Vec<SectionAnnotation>,
    /// Top entities ranked by presence across the document.
    pub main_entities: Vec<RankedEntity>,
    /// Entity name → chunk indices + representative quotes.
    pub entity_index: std::collections::HashMap<String, EntityAppearances>,
    /// Key turning points, revelations, or structural shifts.
    pub structural_moments: Vec<StructuralMoment>,
    /// One-paragraph overview used by the router to decide
    /// operation type without reading the full document.
    pub overview: String,
    /// Atlas-light: per-entity action atoms with chunk-level evidence.
    /// Each atom captures *what an entity does*, anchored to a chunk
    /// so retrieval can be entity-action lookup, not just embedding
    /// similarity. Built optionally during ingest. Empty for pre-atlas
    /// ingests (`#[serde(default)]` keeps old `skeleton_json` rows
    /// deserialising cleanly).
    ///
    /// The book-report bench (2026-05-21) surfaced the failure this
    /// addresses: even with K=16 embedding RAG + entity-name queries
    /// from the briefing, the chunk containing "Winnie stitched the
    /// address label into the lapel" never surfaced. Conrad's
    /// chapter-5 family-drama passages don't embed close to
    /// "Greenwich Park bomber identification" queries. Action atoms
    /// bridge that semantic gap: query "what did Winnie do?" →
    /// atom lookup → chunk_index 11 → return Conrad's actual prose.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionAtom>,
    /// Document-type-agnostic multi-chunk units the LLM grouped at
    /// ingest. Each Segment is a contiguous chunk_range with a
    /// title + summary + function label, capturing whatever
    /// "coherent unit larger than a chunk, smaller than the whole
    /// document" means for this doc_type (scene in fiction, section
    /// in a paper, procedure in a manual, episode in a chronicle).
    ///
    /// Retrieval-time use: when a chunk K is hit by cosine K-NN,
    /// look up the Segment containing K and return the whole
    /// segment together. Replaces the runtime ±1 mechanical
    /// neighbour expansion with LLM-judged structural boundaries.
    ///
    /// Empty for pre-segment ingests; `#[serde(default)]` keeps
    /// old `skeleton_json` rows deserialising cleanly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<DocumentSegment>,
    pub built_at: chrono::DateTime<chrono::Utc>,
}

/// A coherent multi-chunk unit the LLM grouped at ingest time.
/// Generic across document types — the `function` enum reuses the
/// same `SectionFunction` codes the per-chunk SectionAnnotation
/// uses, so the same vocabulary serves both per-chunk and per-
/// segment annotations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSegment {
    /// Stable id within this document — `seg-<chunk_start>`.
    pub id: String,
    /// Inclusive range of chunk_indices this segment spans.
    /// `[start, end]` (both endpoints inclusive) — segments are
    /// guaranteed to be at least 1 chunk.
    pub chunk_start: usize,
    pub chunk_end: usize,
    /// Short, doc-type-aware title in the document's own
    /// register. Free-form so a narrative gets "Heat searches
    /// the wreckage" while a paper gets "Method — fMRI protocol".
    pub title: String,
    /// 1-3 sentence neutral summary of what the segment covers.
    pub summary: String,
    /// Main entities active in this segment (subset of skeleton's
    /// main_entities, scoped to this range).
    pub key_entities: Vec<String>,
    /// Structural function — reuses the existing chunk-scope
    /// SectionFunction enum so retrieval code doesn't branch on
    /// segment-vs-chunk distinction.
    pub function: SectionFunction,
}

/// What an entity does in the document, anchored to a chunk so the
/// passage is recoverable as evidence. Atlas-light — one notch above
/// the entity_index quote_samples (which are just first-200-chars
/// of chunks where the entity appears) and one notch below the full
/// atlas Atom schema (with typed Entity/Event/Relation IDs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionAtom {
    /// Canonical entity name from `main_entities`.
    pub entity: String,
    /// Action verb the LLM extracted ("stitched", "discovers",
    /// "killed"). Lowercase, no surrounding whitespace.
    pub verb: String,
    /// What the verb acts on or modifies — short noun phrase.
    pub object: String,
    /// The chunk this action lives in. Used by retrieval to
    /// surface the original passage when the model queries
    /// the entity name.
    pub chunk_index: usize,
    /// Verbatim ~140-char snippet from the chunk that grounds
    /// the atom. Lets the model see the document's actual
    /// phrasing without re-querying the chunk.
    pub evidence: String,
}

/// A chunk annotated with its structural role in the document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionAnnotation {
    pub chunk_index: usize,
    pub function: SectionFunction,
    pub key_entities: Vec<String>,
    /// What this section establishes, advances, or resolves.
    pub establishes: String,
}

/// The narrative/argumentative role a section plays.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SectionFunction {
    Introduces,
    Develops,
    Complicates,
    Resolves,
    Transitions,
    Evidences,
}

impl SectionFunction {
    pub fn label(&self) -> &'static str {
        match self {
            SectionFunction::Introduces => "Introduces",
            SectionFunction::Develops => "Develops",
            SectionFunction::Complicates => "Complicates",
            SectionFunction::Resolves => "Resolves",
            SectionFunction::Transitions => "Transitions",
            SectionFunction::Evidences => "Evidences",
        }
    }
}

/// An entity ranked by how prominently it appears in the document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedEntity {
    pub name: String,
    pub kind: EntityKind,
    /// Fraction of sections where this entity appears (0.0–1.0).
    pub presence_rate: f32,
    /// First chunk index where this entity appears.
    pub first_appearance: usize,
    /// Last chunk index where this entity appears.
    pub last_appearance: usize,
}

/// Classification of an entity found in a document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EntityKind {
    Character,
    Argument,
    Concept,
    Claim,
    Evidence,
    Theme,
    Person,
    Event,
}

impl EntityKind {
    pub fn label(&self) -> &'static str {
        match self {
            EntityKind::Character => "Character",
            EntityKind::Argument => "Argument",
            EntityKind::Concept => "Concept",
            EntityKind::Claim => "Claim",
            EntityKind::Evidence => "Evidence",
            EntityKind::Theme => "Theme",
            EntityKind::Person => "Person",
            EntityKind::Event => "Event",
        }
    }
}

/// Where an entity appears in the document, with sample quotes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityAppearances {
    pub chunk_indices: Vec<usize>,
    /// Up to 3 representative quotes from the entity's appearances.
    pub quote_samples: Vec<String>,
}

/// A structurally significant moment in the document — a turning
/// point, key revelation, or major transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuralMoment {
    pub chunk_index: usize,
    /// Short description: "Shevek departs Anarres", "Author
    /// concedes the counterargument".
    pub description: String,
    /// 0.0–1.0 importance score. Used to cap the skeleton at
    /// 15–40 moments for a full-length document.
    pub salience: f32,
}

// ─── RAPTOR Atlas ─────────────────────────────────────────────
//
// RAPTOR (Recursive Abstractive Processing for Tree-Organized
// Retrieval) replaces per-chunk LLM skeleton extraction with a
// cluster-summarize-recurse tree. Each node carries a summary
// (signpost), evidence chunk IDs (for tool retrieval), and verbatim
// quote spans (for hallucination-safe quotation).
//
// Load-bearing contract: a node's `summary` must NOT contain `"`.
// Enforced at generation by lark_grammar so downstream tools can
// rely on "anything inside double quotes in a model answer came
// from a quote_span or a fetched chunk — never from a summary."

/// A node in the RAPTOR tree. Level 0 nodes cluster raw document
/// chunks; level N+1 nodes cluster level N node summaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaptorNode {
    /// Stable UUID — primary key + child reference from the level
    /// above.
    pub node_id: String,
    /// 0 = clusters of raw chunks. Higher = clusters of summaries.
    pub level: u8,
    /// LLM-generated paraphrase. By contract free of `"` characters.
    pub summary: String,
    /// Embedding of `summary` — used to match user queries against
    /// nodes at this level.
    pub summary_embedding: Vec<f32>,
    /// GMM centroid in the *input* embedding space (chunk embeddings
    /// for level 0; child summary embeddings for level > 0).
    /// Persisted so incremental updates can re-score new members
    /// without re-clustering the whole document.
    pub centroid_embedding: Vec<f32>,
    /// Child node IDs. Empty at level 0.
    pub children_node_ids: Vec<String>,
    /// Chunks directly in this cluster. Populated only at level 0.
    pub direct_member_chunk_ids: Vec<u32>,
    /// Transitive union of all chunks under this subtree. Used for
    /// scoped chunk retrieval ("search within this node's evidence").
    pub evidence_chunk_ids: Vec<u32>,
    /// 3-5 verbatim spans pulled from member chunks at build time,
    /// chosen for highest cosine similarity to the cluster centroid.
    /// This is the model's hallucination-safe quotable surface for
    /// the node.
    pub quote_spans: Vec<QuoteSpan>,
    /// Primary entities active in this cluster. Union of GLiNER
    /// tags on member chunks and entities the summarization prompt
    /// explicitly identified.
    pub primary_entities: Vec<String>,
    /// Cluster tightness in [0,1]. Higher = members more similar to
    /// centroid. Drives the briefing's coherence-weighted budget so
    /// tight clusters earn their slot in the prompt.
    pub cluster_coherence: f32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// A verbatim span from a source chunk — safe to quote without
/// triggering the bench's hallucination detector. `text` is stored
/// redundantly so the briefing can ship it inline without a
/// round-trip to the chunk store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteSpan {
    pub chunk_id: u32,
    pub char_start: u32,
    pub char_end: u32,
    pub text: String,
}

/// A node of the memory-pool RAPTOR tree (tiered-retrieval memory
/// port, spec `TIERED_RETRIEVAL_MEMORIES.md`). Mirrors [`RaptorNode`]
/// but members are `memories.id` strings, not u32 chunk indices — the
/// builder translates through an id table before persisting — and the
/// grouping key is a memory scope (`mem:<skill>` / `mem:general`),
/// never a corpus. The scope IS the sequestration boundary: a node
/// only ever summarizes memories from within one scope.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemRaptorNodeRow {
    /// Stable UUID — primary key + child reference from the level above.
    pub node_id: String,
    /// Atlas scope key — `MemoryScope::atlas_key()`.
    pub scope: String,
    /// 0 = clusters of raw memories. Higher = clusters of summaries.
    pub level: u8,
    /// LLM paraphrase. Contract-free of `"` (same grammar as RaptorNode).
    pub summary: String,
    /// Embedding of `summary` — matched against recall queries.
    pub summary_embedding: Vec<f32>,
    /// Centroid in the input space (memory embeddings at level 0).
    pub centroid_embedding: Vec<f32>,
    pub children_node_ids: Vec<String>,
    /// Memory ids directly in this cluster. Populated only at level 0.
    pub direct_member_memory_ids: Vec<String>,
    /// Transitive union of member memory ids under this subtree.
    pub evidence_memory_ids: Vec<String>,
    /// Primary entities named by the summarization prompt.
    pub primary_entities: Vec<String>,
    /// Cluster tightness in [0,1].
    pub cluster_coherence: f32,
    /// `InferenceProvider::embed_model_id()` that produced the node
    /// embeddings — the same staleness guard as `memories.embedding_model`;
    /// recall ignores tier nodes whose model doesn't match the live one.
    pub embedding_model: String,
    pub created_at: i64,

    // ── Incremental-tree state (Phase 3, `mem_tree`) ─────────────
    //
    // Batch-built rows leave all of this at default; the incremental
    // path initialises CF lazily from member embeddings on first
    // touch. `#[serde(default)]` keeps pre-Phase-3 JSON deserialising.
    /// Upward pointer for the top-down insert + path re-summarize
    /// walk. Batch rows persist None (parents derived from
    /// `children_node_ids` at load).
    #[serde(default)]
    pub parent_node_id: Option<String>,
    /// BIRCH cluster feature N — member count folded into cf_ls/cf_ss.
    #[serde(default)]
    pub cf_n: i64,
    /// BIRCH CF linear sum of member embeddings.
    #[serde(default)]
    pub cf_ls: Vec<f32>,
    /// BIRCH CF sum of squared norms — radius in O(1).
    #[serde(default)]
    pub cf_ss: f64,
    /// Page-Hinkley running mean of insert residuals.
    #[serde(default)]
    pub ph_mean: f64,
    /// Page-Hinkley cumulative statistic.
    #[serde(default)]
    pub ph_cum: f64,
    /// Page-Hinkley running minimum of the cumulative statistic.
    #[serde(default)]
    pub ph_min: f64,
    /// Members absorbed since this node's summary was last refreshed.
    #[serde(default)]
    pub n_since_summary: i64,
    /// CF radius observed when the summary was last refreshed —
    /// anchors the split limit (radius > headroom × this).
    #[serde(default)]
    pub radius_at_summary: f32,
}

/// A recurring word or phrase that distinguishes this document from
/// a general-English corpus baseline. The motif index gives the
/// model a direct retrieval handle for lexical recurrences that
/// RAPTOR's abstraction loses ("incurious" repeating five times
/// across chapters is invisible to an embedding cluster but
/// load-bearing for thematic-tier questions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetMotif {
    pub term: String,
    /// TF-IDF score against an English-baseline IDF.
    pub tf_idf_score: f32,
    /// Chunk indices where `term` appears.
    pub occurrence_chunk_ids: Vec<u32>,
    /// True when the LLM motif-classifier judged this a recurring
    /// motif vs incidental rare-word noise.
    pub is_distinctive: bool,
}

// ─── Document Operations ──────────────────────────────────────
//
// The operation the router selected for a user's request. Stored
// alongside the response so the user can see how it was handled
// and so the UI can show the correct badge and explanation.

/// The operation type chosen by the document router for a query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DocumentAssetOperation {
    /// Retrieved specific passages matching the query.
    Rag { query: String },
    /// Synthesised across the full document, tracing entities or
    /// themes through multiple sections.
    Synthesis {
        focus: String,
        entities: Vec<String>,
    },
    /// Searched every section for all instances of a pattern.
    Aggregation { query: String },
    /// Applied a transformation (edit, rewrite, extract).
    Transformation,
    /// The question had no clear connection to the attached document, so the
    /// system answered from general knowledge rather than retrieving passages.
    /// `reason` is a short phrase for the UI explanation ("unrelated domain",
    /// "retrieval found nothing", etc.).
    OffTopic { reason: String },
}

impl DocumentAssetOperation {
    /// Short label for the operation badge in the UI.
    pub fn label(&self) -> &'static str {
        match self {
            DocumentAssetOperation::Rag { .. } => "Retrieved passages",
            DocumentAssetOperation::Synthesis { .. } => "Synthesised across full document",
            DocumentAssetOperation::Aggregation { .. } => "Found all instances",
            DocumentAssetOperation::Transformation => "Applied transformation",
            DocumentAssetOperation::OffTopic { .. } => "Answered from general knowledge",
        }
    }
}
