// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wire shapes of the mesh-app explorer routes —
//! `GET /internal/meshapp/{corpus}/…` (`sovereign_mesh::meshapp_http`).
//!
//! These are the "bundle contract": the DTOs a sandboxed explorer webview
//! reads through `window.meshApp.*`. They were defined in
//! `sovereign-meshapp`, beside the projections that fill them, which meant
//! the desktop had to link the projection crate (and through it the whole
//! knowledge engine) to NAME the answer it parses. Moved here 2026-09-11
//! (sv-surface svt-3, thin-desktop order) so a client links the contract
//! layer only; `sovereign_meshapp` re-exports every item at its historical
//! path, so the projections, the CLI `meshapp` verb and the routes are
//! unchanged. Pure serde over primitives — no atom, no index, no engine.
//!
//! `wrapped::WrappedArtifact` (the persisted story-card deck) deliberately
//! did NOT come down: it is a persisted schema whose card types are
//! defined beside the folds and the verifier that audit them, and the
//! desktop only passes it to the webview — it reads no field, so it
//! carries it as `serde_json::Value`.

use serde::{Deserialize, Serialize};

/// A degree-ranked node. `degree` = incident relationships; `alias_count` =
/// surface forms the coalesce phase folded in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNodeDto {
    pub id: String,
    pub canonical_name: String,
    pub entity_type: String,
    pub degree: usize,
    pub alias_count: usize,
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

/// One relationship incident to a node, resolved to its other endpoint and
/// carrying its cited evidence — the glassbox edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeDto {
    pub relationship_type: String,
    /// `"out"` — this node is the source; `"in"` — this node is the target.
    pub direction: String,
    pub other_id: String,
    pub other_name: String,
    pub other_type: String,
    pub excerpt: String,
    pub source_chunk: String,
    pub confidence: f32,
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

/// A node's full detail: attributes, folded aliases, every incident cited edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDetailDto {
    pub id: String,
    pub canonical_name: String,
    pub entity_type: String,
    pub attributes: serde_json::Map<String, serde_json::Value>,
    pub aliases: Vec<String>,
    pub edges: Vec<EdgeDto>,
}

/// A deterministic pattern finding (e.g. a sighting hotspot).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingDto {
    pub pattern_name: String,
    pub pattern_kind: String,
    pub entities: Vec<FindingEntityDto>,
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingEntityDto {
    pub id: String,
    pub canonical_name: String,
    pub entity_type: String,
}

/// One cross-origin identity merge: a canonical entity + the surface forms
/// folded into it + the signals that fired (the glassbox reason).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationMergeDto {
    pub canonical_id: String,
    pub canonical_name: String,
    pub surface_forms: Vec<String>,
    pub signals_fired: Vec<String>,
    pub source_count: usize,
}

/// One undirected edge of a [`SubgraphDto`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubEdgeDto {
    pub source: String,
    pub target: String,
    pub relationship_type: String,
}

/// Top-degree nodes + the edges induced among them, for a node-link map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubgraphDto {
    pub nodes: Vec<GraphNodeDto>,
    pub edges: Vec<SubEdgeDto>,
}

/// Headline scale/provenance counts for a banner.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CorpusStatsDto {
    pub atoms: usize,
    pub entities: usize,
    pub events: usize,
    pub states: usize,
    pub relations: usize,
    pub claims: usize,
    pub questions: usize,
    pub edges: usize,
    pub reconciled_merges: usize,
    pub documents: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineBucketDto {
    /// `YYYY-MM`.
    pub ym: String,
    pub count: usize,
    /// A capped sample of chunk ids in this month, for click-to-drill.
    pub chunk_ids: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineDto {
    pub buckets: Vec<TimelineBucketDto>,
    pub dated: usize,
    pub total: usize,
}

/// Full source-chunk text behind a cited edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkDto {
    pub chunk_id: String,
    pub content: String,
    pub title: Option<String>,
}

/// One chunk inside a [`FeedDocDto`] — carries the raw-metadata-derived
/// `outbound_links` (wikilink target titles for newsworthy; empty for
/// corpora whose extractor doesn't stamp links).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedChunkDto {
    pub chunk_id: String,
    pub content: String,
    pub title: Option<String>,
    pub outbound_links: Vec<String>,
}

/// One source document in a [`document_feed`] response — for the
/// newsworthy corpus, one portal day (`source_doc_id = "YYYY-MM-DD"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedDocDto {
    pub source_doc_id: String,
    pub chunks: Vec<FeedChunkDto>,
}

/// [`document_feed`] response: documents newest-first by
/// `source_doc_id` (dates sort correctly lexicographically).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentFeedDto {
    pub corpus_id: String,
    pub docs: Vec<FeedDocDto>,
}

/// A claim atom projected for the explorer's "arguments" view — the entity
/// graph ops don't surface claims, so this carries the proposition, its
/// discourse + epistemic framing, who it's attributed to (entity name,
/// resolved), and its first cited evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimDto {
    pub id: String,
    pub content: String,
    pub discourse_act: String,
    pub epistemic_status: String,
    pub quotable_excerpt: Option<String>,
    pub attributed_to: Option<String>,
    pub source_chunk: String,
    pub excerpt: String,
}

/// A question atom projected for the explorer — the inquiry, its type +
/// resolution status, how many claims address it, and where it's raised.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionDto {
    pub id: String,
    pub content: String,
    pub question_type: String,
    pub resolution_status: String,
    pub addressed_by: usize,
    pub source_chunk: String,
}
