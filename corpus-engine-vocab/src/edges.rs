// SPDX-License-Identifier: AGPL-3.0-or-later
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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
    // Gap-B typed-extension edges (§3.2 — surfaced from Phase 1
    // typed extensions during routed Phase-1 resolution).
    /// Evidence atom supports a target claim or position. source =
    /// Claim with `claim_kind: "evidence"` OR an evidence atom;
    /// target = Claim or Position. Derived deterministically from
    /// the typed extension's `evidence_invocations[].supports` field
    /// when that string fuzzy-resolves to an atom id.
    EvidenceFor,
    /// Concession claim addresses a target position. source = Claim
    /// with `claim_kind: "concession"`; target = Position. Derived
    /// from `concessions[].addresses` when resolution succeeds. The
    /// edge's existence (combined with the source claim's
    /// `concession_outcome`) lets a downstream reader follow "what
    /// did the author grant in service of what view?".
    Concedes,
    /// Opposition atom frames a target Concept/Entity as one side
    /// of its binary. Two edges per Opposition (left + right side)
    /// so the graph stays traversable from a concept back to every
    /// opposition that includes it without scanning Opposition
    /// .left_label / .right_label strings.
    OpposesIn,
    /// AD-2 (architecture-over-Enron Phase 1): a Document / Entity /
    /// Claim atom carries an asset payload — typically an email body
    /// atom linking to an attachment's Asset atom, but generally any
    /// "atom A bundles atom-described-binary B" relation. source = the
    /// carrier atom; target = the [`crate::atoms::Asset`]
    /// atom. Emitted at extraction time alongside the Asset atom; the
    /// edge stays prose-shaped (the graph never grows a Table /
    /// Spreadsheet variant — the parsed-form path on the Asset is
    /// enough for the future structured-query path).
    Attaches,
}

impl EdgeType {
    /// Compact lowercase tag for this edge kind. The single decider for the
    /// LABEL spelling, distinct from the PascalCase serde tag carried on the
    /// wire.
    ///
    /// This existed as two copies — `sovereign-mesh::reading_formatters::
    /// edge_type_label` and `sovereign-desktop`'s `edge_type_label_dto` — AND
    /// THEY DISAGREED. The desktop copy answered
    /// `unreachable!("typed edges wired in Gap B Stage 4")` for `EvidenceFor`,
    /// `Concedes` and `OpposesIn`, three kinds the Gap-B resolver has emitted
    /// since it landed (`atlas::resolution` builds all three) and which
    /// round-trip through the CSR type byte. So the desktop's reading surface
    /// PANICKED on any atlas containing one, while the mesh rendered it
    /// correctly. Two copies of one closed set, one of them frozen against a
    /// stale assumption — the failure ARCH §10.6 exists to prevent, and the
    /// reason the labels live here now.
    pub fn label(&self) -> &'static str {
        match self {
            EdgeType::Transition => "transition",
            EdgeType::Causes => "causes",
            EdgeType::Grounds => "grounds",
            EdgeType::Tension => "tension",
            EdgeType::Involves => "involves",
            EdgeType::Composes => "composes",
            EdgeType::Configures => "configures",
            EdgeType::Grounding => "grounding",
            EdgeType::Framing => "framing",
            EdgeType::Provenance => "provenance",
            EdgeType::EvidenceFor => "evidence_for",
            EdgeType::Concedes => "concedes",
            EdgeType::OpposesIn => "opposes_in",
            EdgeType::Attaches => "attaches",
        }
    }
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
    /// Code-corpus containment edge: Crate → Module, Module → Item.
    /// Derived deterministically from file paths and the tree-sitter
    /// chunk index — the item is _located inside_ the parent in the
    /// source tree. No LLM involvement; same trust class as
    /// `WikilinkStructural`.
    ContainmentStructural,
    /// Code-corpus cross-reference edge sourced from the SCIP index:
    /// one item uses, calls, implements, or references another.
    /// Confidence is 1.0 because SCIP is a compiler-resolved fact.
    /// Trait impls emit two edges (Self→Trait and Trait→Self) to
    /// keep the graph traversable in either direction.
    ScipStructural,
    /// Code-corpus dependency edge derived from `Cargo.toml`: the
    /// owning crate declares a dependency on the target. Used for
    /// `Crate → ExternalCrate` placeholder edges.
    CargoStructural,
    /// Code-corpus tree-sitter fallback for items that SCIP didn't
    /// resolve (uncovered language, partial index). The walker
    /// recovers the edge from a `use` statement or import textually.
    TreeSitterStructural,
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

#[cfg(test)]
mod label_tests {
    use super::*;

    /// The three kinds `sovereign-desktop`'s deleted `edge_type_label_dto`
    /// answered with `unreachable!("typed edges wired in Gap B Stage 4")`.
    /// They have been emitted by `atlas::resolution` since Gap B landed, so
    /// that copy panicked on any atlas containing one. Named individually
    /// here so the regression cannot come back quietly.
    #[test]
    fn gap_b_edge_kinds_have_labels_and_do_not_panic() {
        assert_eq!(EdgeType::EvidenceFor.label(), "evidence_for");
        assert_eq!(EdgeType::Concedes.label(), "concedes");
        assert_eq!(EdgeType::OpposesIn.label(), "opposes_in");
    }

    #[test]
    fn every_edge_kind_has_a_distinct_label() {
        let all = [
            EdgeType::Transition,
            EdgeType::Causes,
            EdgeType::Grounds,
            EdgeType::Tension,
            EdgeType::Involves,
            EdgeType::Composes,
            EdgeType::Configures,
            EdgeType::Grounding,
            EdgeType::Framing,
            EdgeType::Provenance,
            EdgeType::EvidenceFor,
            EdgeType::Concedes,
            EdgeType::OpposesIn,
            EdgeType::Attaches,
        ];
        let mut labels: Vec<&str> = all.iter().map(|e| e.label()).collect();
        let n = labels.len();
        labels.sort();
        labels.dedup();
        assert_eq!(labels.len(), n, "two edge kinds share a label");
        assert!(labels.iter().all(|l| !l.is_empty()));
    }
}
