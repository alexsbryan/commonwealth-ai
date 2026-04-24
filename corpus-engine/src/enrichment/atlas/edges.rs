//! Atlas edge types — the typed relationships between atoms.
//!
//! Spec §3 enumerates seven intra-corpus edge types. Step 3a emits
//! only `Involves` (event ↔ entity); the remaining edge types are
//! scaffolded here so Phase 3b, Phase 4, and Phase 8 can plug in
//! without widening the on-disk schema.
//!
//! Cross-corpus edges (§3.1) live in the same struct family but are
//! written to a separate `atlas/cross_corpus_edges.json` file and
//! have their own provenance tags.

use serde::{Deserialize, Serialize};

use super::atoms::{AtomId, ChunkRef};

// ── Typed identifier ─────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EdgeId(String);

impl EdgeId {
    pub fn new(index: usize) -> Self {
        Self(format!("edge-{index:05}"))
    }

    pub fn from_raw(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ── Edge type enum ───────────────────────────────────────────

/// Discriminator for the seven intra-corpus edge types (§3) plus the
/// three cross-corpus edge types (§3.1). Carried on every edge as a
/// string tag for forward compatibility — an older consumer that
/// doesn't recognise a new edge type should fail loudly rather than
/// silently dropping it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum EdgeType {
    // Intra-corpus edges (§3)
    Transition,
    Causes,
    Grounds,
    Tension,
    Involves,
    Composes,
    Configures,
    // Cross-corpus edges (§3.1)
    Grounding,
    Framing,
    Provenance,
}

// ── Provenance ───────────────────────────────────────────────

/// How the edge was produced. Callers of the traversal engine use
/// this alongside `confidence` to decide whether to qualify the
/// output — `derived` edges are cheap and deterministic; LLM-produced
/// edges warrant language calibration in the brief assembler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeProvenance {
    /// LLM produced this edge during Phase 1 extraction or Phase 5
    /// resolution — the model saw the passages and named the link.
    LlmExtraction,
    /// LLM produced this edge during a pairwise analysis pass (Phase
    /// 6 tensions, Phase 5 cross-claim).
    LlmPairwise,
    /// LLM produced this edge during Phase 8 configuration detection.
    LlmConfiguration,
    /// Deterministic post-hoc computation. Step 3a's Involves edges
    /// land here — the event sketch already lists its participants,
    /// so resolving them to entity ids is mechanical, not inferential.
    Derived,
    /// Structural parse of a corpus that carries explicit link
    /// structure (e.g. Wikipedia wikilinks). Reserved for the future
    /// structure-first ingestion strategy.
    WikilinkStructural,
}

// ── Edge record ──────────────────────────────────────────────

/// Single directed edge between two atoms. The interpretation of
/// `source` and `target` depends on `edge_type` per spec §3 — the
/// compiler can't distinguish them, so readers should always check
/// the type before interpreting the endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub id: EdgeId,
    pub edge_type: EdgeType,
    pub source: AtomId,
    pub target: AtomId,
    /// Evidence passages for the edge, when the type carries its own
    /// grounding (`Grounds`, `Tension`). Empty for structural edges
    /// like `Involves` that derive from atom fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ChunkRef>,
    /// Event that triggered a `Transition`, if the pipeline
    /// identified one. Populated by Phase 3b.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_event: Option<AtomId>,
    /// Sub-question a `Tension` turns on (spec §3 example). Populated
    /// by Phase 6.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_question: Option<String>,
    /// Extraction confidence in `[0.0, 1.0]`. Surfaced in the brief
    /// assembler only when below the threshold (default 0.7) so
    /// high-confidence findings present cleanly.
    pub confidence: f32,
    pub provenance: EdgeProvenance,
}

// ── Top-level edges file ─────────────────────────────────────

/// On-disk shape of `atlas/edges.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgesFile {
    pub schema_version: String,
    pub edges: Vec<Edge>,
}

impl EdgesFile {
    pub const SCHEMA_VERSION: &'static str = "2.0";

    pub fn new(edges: Vec<Edge>) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION.to_string(),
            edges,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn involves_edge(index: usize, event_ix: usize, entity_ix: usize) -> Edge {
        Edge {
            id: EdgeId::new(index),
            edge_type: EdgeType::Involves,
            source: AtomId::event(event_ix),
            target: AtomId::entity(entity_ix),
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        }
    }

    #[test]
    fn involves_edge_roundtrips_through_serde() {
        let edge = involves_edge(1, 1, 2);
        let json = serde_json::to_string(&edge).unwrap();
        assert!(json.contains("\"edge_type\":\"Involves\""));
        assert!(json.contains("\"provenance\":\"derived\""));
        // Optional fields are skipped when empty.
        assert!(!json.contains("trigger_event"));
        assert!(!json.contains("sub_question"));
        let back: Edge = serde_json::from_str(&json).unwrap();
        assert_eq!(back.edge_type, EdgeType::Involves);
        assert_eq!(back.provenance, EdgeProvenance::Derived);
    }

    #[test]
    fn cross_corpus_edge_types_deserialize() {
        let json = r#"{
          "id": "edge-00001",
          "edge_type": "Grounding",
          "source": "entity-0001",
          "target": "entity-0002",
          "confidence": 0.9,
          "provenance": "llm_extraction"
        }"#;
        let back: Edge = serde_json::from_str(json).unwrap();
        assert_eq!(back.edge_type, EdgeType::Grounding);
        assert_eq!(back.provenance, EdgeProvenance::LlmExtraction);
    }

    #[test]
    fn edges_file_carries_schema_version() {
        let f = EdgesFile::new(vec![involves_edge(1, 1, 2)]);
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"schema_version\":\"2.0\""));
        assert!(json.contains("\"edges\":["));
    }

    #[test]
    fn edge_id_format_is_zero_padded() {
        assert_eq!(EdgeId::new(3).as_str(), "edge-00003");
    }
}
