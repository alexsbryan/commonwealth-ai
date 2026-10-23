// SPDX-License-Identifier: AGPL-3.0-or-later
//! Knowledge search API (v0.3 §6) and the landscape-digest surface
//! (§6.5).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use crate::manifest::EmbedModelInfo;

// -----------------------------------------------------------------
// Section 6 — Knowledge Search API
// -----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeSearchRequest {
    /// Pre-computed query embedding. OPTIONAL as of v0.4: when empty,
    /// the HOST embeds `query_text` with its advertised
    /// [`EmbedModelInfo::query_instruction_prefix`] — the OICP contract
    /// is thin-client (the host owns the embed model), so a client need
    /// only send text. Mesh peers still pre-embed and send this to
    /// avoid re-embedding on every hop; when present it is used as-is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query_embedding: Vec<f32>,
    /// The query text. `query` is accepted as an alias — it is the
    /// natural OICP thin-client field name; `query_text` is retained
    /// for the mesh-internal shape.
    #[serde(default, alias = "query")]
    pub query_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpora: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// v0.6 §2.8 (`knowledge:scoped`): the text names (`text_sha256`, 64
    /// lowercase hex) every hit must be cut from. Empty means any text. Under
    /// a filter a hit that names no text is never returned.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<String>,
    /// v0.6 §2.11 (`knowledge:sentences`): what the hits are. `passage`, the
    /// default, is the search as it was and is never written on the wire, so
    /// a request that does not ask for sentences is byte-for-byte unchanged.
    #[serde(default, skip_serializing_if = "Granularity::is_passage")]
    pub granularity: Granularity,
    /// v0.6 §2.17 (`knowledge:priority`): who waits on the answer. Absent,
    /// the request is served as before §2.17 and is never written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<RequestPriority>,
}

/// v0.6 §2.17: who waits on a request's answer, a closed set. While an
/// `interactive` request's model work is in flight a `background` one starts
/// none; the answer is the same either way. A value outside the set is a
/// decode error, never read as absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestPriority {
    /// A person waits on it as they type.
    Interactive,
    /// The client runs it on its own schedule, such as a draft's checks.
    Background,
}

impl RequestPriority {
    /// The wire name, as the host's events name it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Background => "background",
        }
    }
}

/// v0.6 §2.11: what a knowledge search's hits are cut as.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    /// The host's passages (its chunks), as every version before v0.6 §2.11.
    #[default]
    Passage,
    /// Sentences of the stored texts, ranked by the host's sentence scorer.
    Sentence,
}

impl Granularity {
    /// True for the default, which the wire leaves unwritten.
    pub fn is_passage(&self) -> bool {
        *self == Self::Passage
    }
}

/// v0.6 §2.11: the closed set of sentence scorers. One is named in the
/// host's data file and advertised as `knowledge.sentence_scorer`; a name
/// outside the set is a decode error, never a default, and adding one is a
/// spec revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SentenceScorer {
    /// Cosine between the query's embedding (query space) and each
    /// sentence's (document space), over every sentence in scope.
    Cosine,
    /// Cosine's top candidates (their number in the host's data file)
    /// reordered by the host's rerank slot; the scores are the reranker's.
    Rerank,
}

impl SentenceScorer {
    /// The wire spelling, as serde writes it.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cosine => "cosine",
            Self::Rerank => "rerank",
        }
    }
}

/// v0.6 §2.11: what scored a sentence reply. Scores are comparable only
/// under one `name` and `model`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scorer {
    /// The scorer, always the one the manifest advertises.
    pub name: SentenceScorer,
    /// The model that produced the scores: the embedder for `cosine`, the
    /// reranker for `rerank`.
    pub model: String,
}

/// v0.6 §2.11: how a sentence reply was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SentenceRoute {
    /// Every sentence in scope is embedded, and every one was scored.
    Index,
    /// Some scoped text is still being indexed: the sentences of the top
    /// passage hits were scored instead, by the same scorer.
    Passages,
}

/// v0.6 §2.11 (`knowledge:sentence_queue`): where a corpus with sentences
/// waiting stands in the host's embedding queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexPlace {
    /// The host is embedding this corpus's sentences now.
    Embedding,
    /// Other corpora are embedded first; `behind` names them.
    Queued,
}

/// v0.6 §2.11: a corpus ahead of another in the embedding queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedCorpus {
    pub corpus: String,
    /// Its sentences not yet embedded; absent when the host has not yet
    /// counted them (a corpus it has queued and not opened).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<u64>,
}

/// v0.6 §2.11: one scoped corpus with sentences waiting, and its place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentencePlace {
    pub corpus: String,
    /// The corpus's own sentences not yet embedded (not only the scoped ones).
    pub pending: u64,
    pub place: IndexPlace,
    /// The corpora embedded before this one, the one embedding now first,
    /// in the order the host embeds them; empty exactly when `place` is
    /// `embedding`.
    #[serde(default)]
    pub behind: Vec<QueuedCorpus>,
}

/// v0.6 §2.11: a sentence scan stopped at the host's ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Truncated {
    /// Sentences scored: the host's ceiling.
    pub scanned: u64,
    /// Sentences in the scope, more than `scanned`.
    pub in_scope: u64,
}

/// v0.6 §2.11's named refusal.
pub mod reasons {
    /// The scorer the host's data file names cannot run here (`rerank` with
    /// no rerank slot, `cosine` with no embedder): `503`, with the scorer
    /// named beside it, never answered by another scorer.
    pub const SCORER_UNAVAILABLE: &str = "scorer unavailable";
}

impl KnowledgeSearchRequest {
    /// The default result limit per §6.1 when `limit` is omitted.
    pub const DEFAULT_LIMIT: u32 = 20;

    /// Effective result limit, applying the §6.1 default of 20.
    pub fn effective_limit(&self) -> u32 {
        self.limit.unwrap_or(Self::DEFAULT_LIMIT)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnowledgeSearchResponse {
    pub results: Vec<KnowledgeResult>,
    pub corpora_searched: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corpora_unavailable: Vec<String>,
    /// The subset of `corpora_unavailable` that NO live member advertises —
    /// named apart so "nobody hosts it" reads differently from "its host is
    /// offline". Every entry here is also in `corpora_unavailable`, so a
    /// reader that predates this field still sees the corpus as missing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corpora_unhosted: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_chunks_searched: Option<u64>,
    /// v0.6 §2.11: what scored the sentences. Present exactly when the
    /// request asked for `granularity: sentence` and the host honoured it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scorer: Option<Scorer>,
    /// v0.6 §2.11: how the sentences were found; beside `scorer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<SentenceRoute>,
    /// v0.6 §2.11: scoped sentences not yet embedded. Present exactly when
    /// `route` is `passages`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sentences_pending: Option<u64>,
    /// v0.6 §2.11 (`knowledge:sentence_queue`): the place of each scoped
    /// corpus whose sentences are not all embedded. Present exactly when
    /// `route` is `passages` on a host advertising the feature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sentence_queue: Option<Vec<SentencePlace>>,
    /// v0.6 §2.11: the scan stopped at the host's ceiling. Absent means
    /// every sentence in scope was scored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<Truncated>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeResult {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub corpus_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub score: f32,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
    /// Stable LanceDB row id for the chunk on the producing peer.
    /// Lets the desktop's reading surface deref a citation back to
    /// the source chunk (see ENRICHMENT_V2 / glass-box reading
    /// surface plan). `None` for synthetic chunks (atlas-virtual,
    /// local-doc) and for older peers that haven't been upgraded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<u64>,
    /// Document grouping key for "elsewhere in this document"
    /// lookups and for chunk-neighbor ordering. `None` when the
    /// extractor didn't tag chunks with a document id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_doc_id: Option<String>,
    /// The custody class the SERVING index recorded, in the wire spelling
    /// `kernel_types::Custody::as_str` defines (`public-web` | `personal` |
    /// `peer`). `None` means the serving side recorded none — which is
    /// different from `unknown` and must stay different: the requester joins
    /// this with its own "arrived from another node" fact and refuses on
    /// absence.
    ///
    /// Deliberately a `String` and not `kernel_types::Custody`. `oicp-types`
    /// is pinned to ZERO internal dependencies (the `[[package_leaf]]` budget
    /// in `quality/ARCH_LAYERS.toml`) so the protocol crate stays liftable by
    /// a third party; the wire spelling is the contract and
    /// `Custody::parse_wire` is its one parser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custody: Option<String>,
    /// `leaf` | `summary` — whether the serving index vouched for this as
    /// source text or as prose ABOUT source text
    /// (`kernel_types::Grain::as_str`).
    ///
    /// Added 2026-08-26. Before it the requester could not tell a peer-served
    /// RAPTOR rollup from a peer-served passage, so it had to treat every mesh
    /// hit as unciteable content built in-process. `None` from a peer that
    /// predates this field, and the requester must read absence as `summary`,
    /// the refusing value — a rollup wrongly marked `leaf` becomes quotable,
    /// which is the direction that fabricates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grain: Option<String>,
    /// The member name of the peer that served this passage, stamped by the
    /// requester's fan-out. `None` for a locally-served hit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_name: Option<String>,
    /// That peer's node id, stamped beside `peer_name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_node_id: Option<String>,
    /// v0.5 §3: the stored text this hit was cut from, with its record and
    /// metadata. When the host advertises `knowledge:document`, every hit
    /// carries it or names why not in [`Self::document_absent`], and
    /// `document.metadata` supersedes the v0.2 `metadata` map, which the
    /// reference host leaves empty. `None` from a host that predates the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<crate::evidence::Document>,
    /// v0.5 §3: why this hit carries no `document`, one of
    /// [`crate::evidence::reasons::HIT_ABSENCES`]: a corpus built before the
    /// text store (`texts not stored`), one that opted out (`text not
    /// stored`), or a damaged record (`document unreadable`). `None` beside a
    /// `document`, and from a host that predates the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_absent: Option<String>,
    /// v0.6 §2.8 (`knowledge:range`): the hit's text as a range of its
    /// stored text, given only when the chunk, its title header removed, is
    /// an exact slice of it. `None` beside `span_absent`, and from a host
    /// that predates the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<crate::evidence::Span>,
    /// v0.6 §2.8: why this hit carries no `span`, one of
    /// [`crate::evidence::reasons::SPAN_ABSENCES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_absent: Option<String>,
}

// -----------------------------------------------------------------
// Corpus shard advertisement (mesh gossip capability report)
// -----------------------------------------------------------------
//
// Named by BOTH sides — `commonwealth-core` builds the capability report
// that carries these, `sovereign-mesh` fills them in from the local index,
// and `sovereign-tools`' atlas peer-advice rule reads them — so the schema
// lives in the protocol leaf rather than in either side's crate
// (quality/ARCH_LAYERS.toml package closures). Moved from
// `commonwealth_core::knowledge` 2026-09-21; no field changed.

/// A contiguous range of chunk IDs within a corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRange {
    /// First chunk ID (inclusive).
    pub start_id: u64,
    /// Last chunk ID (exclusive).
    pub end_id: u64,
}

impl ChunkRange {
    pub fn new(start_id: u64, end_id: u64) -> Self {
        debug_assert!(start_id < end_id, "empty chunk range: {start_id}..{end_id}");
        Self { start_id, end_id }
    }

    pub fn count(&self) -> u64 {
        self.end_id - self.start_id
    }
}

/// Information about a corpus shard hosted on a node.
/// Used in capability reports.
///
/// ## Phase: canonical-sync surface (Phase 6 of the resilience track)
///
/// Three new fields drive the mesh's self-healing canonical sync:
///
/// - `chunk_count`: how many chunks this peer's canonical contains.
///   Compared by the auto-recover path to pick the healthier peer
///   when several have a canonical for the same id.
/// - `canonical_fingerprint`: blake3 of the sorted content_hash list
///   for the canonical at this peer. Two peers with byte-identical
///   chunks arrive at the same string. The puller validates this
///   value against the file it actually downloaded so a poisoned
///   tarball fails closed.
/// - `total_shards` + `processed_shards`: lets a peer compute its
///   coverage ratio (`processed / total`) for sharded corpora.
///   Auto-recover compares ratios — not raw chunk counts — to pick
///   the most-complete peer, which is robust to legitimate corpus
///   updates that shrink the chunk set.
///
/// All three are `Option`/`Vec`-defaulted so older peers (whose
/// gossip blobs predate this struct) deserialize cleanly. A peer
/// missing the fields just opts out of the new sync paths until
/// it upgrades.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusShardInfo {
    pub corpus_id: String,
    pub chunk_range: Option<ChunkRange>,
    pub is_replica: bool,
    pub last_updated: u64,
    /// Total chunks in this peer's canonical (or partition).
    /// Defaults to 0 for older peers; auto-recover treats `0` as
    /// "unknown" rather than "empty" — peers with a fingerprint
    /// but no chunk_count are eligible to pull from but not
    /// rankable by count.
    #[serde(default)]
    pub chunk_count: u64,
    /// Stable content fingerprint for the canonical. See
    /// `corpus_engine::IndexInfo::canonical_fingerprint` for the
    /// algorithm. `None` for partitions and for canonicals that
    /// haven't been stamped yet (the daemon's lazy-stamp pass on
    /// next start fills these in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_fingerprint: Option<String>,
    /// Total source shards this corpus expects (e.g. 38 for the
    /// canonical Wikipedia ingest). `None` for non-sharded corpora.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_shards: Option<usize>,
    /// Source shards this peer's canonical (or partition) has
    /// processed. The auto-recover path takes the union of these
    /// across peers to compute coverage ratios.
    #[serde(default)]
    pub processed_shards: Vec<usize>,
    // ── Atlas advertisement (Phase C1) ──────────────────────────
    //
    // These three fields let a peer joining the mesh decide
    // whether to pull this corpus's atlas instead of running
    // local Tier-2 enrichment. All three default to 0 / `None` so
    // older peers (and peers whose corpus has no atlas yet)
    // serialise + deserialise cleanly with no protocol break.
    /// Total atoms (entities + events + …) in this peer's
    /// `<corpus>/atlas/atoms.json`. `0` means "no atlas yet" or
    /// "older peer that doesn't advertise atlas state."
    #[serde(default)]
    pub atlas_atom_count: u64,
    /// Entities at `enrichment_depth = "extracted"` (Tier-2
    /// enriched). The mesh ranks atlases by this — a peer with a
    /// higher count has done more deep-extraction work and is the
    /// preferred atlas source for fresh nodes.
    #[serde(default)]
    pub atlas_tier2_count: u64,
    /// SHA-256 of `atoms.json` (hex). Receipt the puller validates
    /// against after fetching a peer's atlas — a corrupted /
    /// poisoned transfer fails closed. `None` = no atlas or
    /// fingerprint not yet stamped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atlas_fingerprint: Option<String>,
}

impl CorpusShardInfo {
    /// Coverage ratio for sharded corpora — `processed_shards.len()
    /// / total_shards`. Returns `None` for non-sharded corpora
    /// (`total_shards.is_none()`) and for the degenerate case
    /// `total_shards = 0`. Used by `auto_recover` to pick the
    /// most-complete peer.
    pub fn coverage_ratio(&self) -> Option<f64> {
        let total = self.total_shards?;
        if total == 0 {
            return None;
        }
        Some(self.processed_shards.len() as f64 / total as f64)
    }
}

// -----------------------------------------------------------------
// Section 6.5 — Knowledge Landscape Digest API
// -----------------------------------------------------------------
//
// The daemon-side `KnowledgeViewManager` exposes its assembled
// digests via `POST /v1/knowledge/landscape_digest`, so an attached
// desktop (which does NOT construct its own manager — see
// `AppState::is_attach_mode`) can splice the same prompt blocks the
// daemon would. Wire shape mirrors the existing
// `LandscapeDigest` type in `sovereign-core::types`; we redefine it
// here to keep `oicp-types` a leaf crate with no upstream Sovereign
// deps. The receiving side maps between the two.

/// One assembled landscape-digest block (e.g. personal-knowledge,
/// conversation-history, cross-view, relational, strategic). The
/// `body` is markdown ready to splice; the `view_id` lets clients
/// dedupe / re-order if needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LandscapeDigestEntry {
    pub view_id: String,
    pub body: String,
}

/// Request body for `POST /v1/knowledge/landscape_digest`. All
/// fields are optional — the simplest valid request is `{}`,
/// equivalent to "give me the unconstrained digest set with no
/// active-skill privacy filter and no in-conversation context."
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LandscapeDigestRequest {
    /// Active skill id. Today this is informational only; reserved
    /// for v2 skill-tiered digest work. The daemon does NOT
    /// introspect it for privacy gating — see
    /// `active_is_local_only` for that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_skill: Option<String>,
    /// Caller-resolved "the active skill has privacy = local_only"
    /// flag. The desktop has the canonical skill registry and
    /// computes this against `SkillRegistry::local_only_skill_ids`;
    /// the daemon trusts the flag and applies it directly. This
    /// design keeps the daemon out of the skill-registry business
    /// while preserving the splice-time privacy filter (a
    /// `local_only` session must NOT receive
    /// conversational/institutional/cross-view blocks).
    #[serde(default)]
    pub active_is_local_only: bool,
    /// In-conversation message contents. Drives the "this entity is
    /// already on screen, don't re-introduce it" predicate in the
    /// relational/strategic blocks. Empty = no in-conv suppression.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conversation_messages: Vec<String>,
}

/// Response shape — a flat list of digests in the order the daemon
/// would have spliced them. The desktop calls
/// `ConversationContext::set_landscape_digests` with the converted
/// list and the runtime treats it identically to a locally-spliced
/// payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LandscapeDigestResponse {
    pub digests: Vec<LandscapeDigestEntry>,
}

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

/// The ring namespace the newsworthy watcher's tracked articles replicate on.
/// Federation wire, re-exported at corpus-index's `ingest_port::newsworthy`.
pub const APP_ID_TRACKED: &str = "wikipedia-newsworthy-tracked";

#[cfg(test)]
#[path = "knowledge_tests.rs"]
mod tests;
