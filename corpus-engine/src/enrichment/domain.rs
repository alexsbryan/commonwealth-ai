//! Domain trait and supporting types for field model enrichment.
//!
//! A `Domain` encodes the epistemic conventions of a field of knowledge.
//! It is the single extension point for generalizing across corpora.
//!
//! Object-safe. All methods take `&self`. No associated types.
//! The engine holds `Arc<dyn Domain>` and calls these methods directly.

use crate::index::StoredChunk;
use serde::{Deserialize, Serialize};

/// Type alias so domain prompt code can use `Chunk` without importing index types.
pub type Chunk = StoredChunk;

/// Encodes the epistemic conventions of a field of knowledge.
/// The single extension point for generalizing across corpora.
///
/// Object-safe. All methods take &self. No associated types.
/// The engine holds Arc<dyn Domain> and calls these methods directly.
///
/// Implementations in this task:
///   PhilosophyDomain — fully implemented
///   MultiDomain      — constructor only, methods todo!()
///   ScienceDomain, PolicyDomain, LegalDomain,
///   CommunityKnowledgeDomain — empty structs, all methods todo!()
pub trait Domain: Send + Sync + 'static {
    // ── Identity ──────────────────────────────────────────────────────────
    fn id(&self) -> &str;
    fn name(&self) -> &str;

    // ── Epistemic vocabulary ──────────────────────────────────────────────
    fn position_statuses(&self) -> &PositionStatusVocab;
    fn question_types(&self) -> &[QuestionType];

    // ── Overview document identification ──────────────────────────────────
    fn overview_filter(&self) -> ChunkFilter;

    // ── Prompts ───────────────────────────────────────────────────────────
    // Each takes concrete chunk references; returns a complete prompt string.
    // The engine calls these; domain implementations define them.

    fn skeleton_extraction_prompt(&self, chunks: &[&Chunk]) -> String;
    fn cluster_labeling_prompt(&self, representative_chunks: &[&Chunk]) -> String;
    fn fault_line_detection_prompt(
        &self,
        chunks_a: &[&Chunk],
        chunks_b: &[&Chunk],
        position_a: &str,
        position_b: &str,
    ) -> String;
    fn open_question_prompt(&self, chunks: &[&Chunk]) -> String;

    // ── Clustering and alignment parameters ───────────────────────────────
    fn clustering_config(&self) -> ClusteringConfig;
    fn alignment_config(&self) -> AlignmentConfig;
    fn fault_line_config(&self) -> FaultLineConfig;

    // ── Storage strategy ──────────────────────────────────────────────────
    fn skeleton_storage(&self) -> SkeletonStorage;

    // ── Chunk role classification ─────────────────────────────────────────
    // Default covers the common case. Override for domain-specific
    // role vocabularies (e.g. LegalDomain adds ChunkRole::Holding).
    fn classify_chunk_role(&self, label: &ClusterLabel) -> ChunkRole {
        if !label.is_argumentative {
            return ChunkRole::NonArgumentative;
        }
        if label.is_open_question {
            return ChunkRole::OpenQuestion;
        }
        if label.is_objection {
            return ChunkRole::Objection;
        }
        ChunkRole::Argument
    }
}

// ── Vocabulary types ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PositionStatusVocab {
    pub dominant: &'static str,
    pub minority: &'static str,
    pub contested: &'static str,
    pub settled: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum QuestionType {
    Factual,
    Normative,
    Conceptual,
    Legal,
    Practical,
}

#[derive(Debug, Clone)]
pub enum SkeletonStorage {
    /// LanceDB tables AND field_skeleton.json export.
    /// Use for small bounded domains: SEP, CRS, CBO.
    JsonAndLance,
    /// LanceDB tables only. field_index.json carries stats only.
    /// Use for large unbounded domains: Wikipedia, Stack Exchange.
    LanceOnly,
}

#[derive(Debug, Clone)]
pub struct ChunkFilter {
    pub is_first_in_entry: Option<bool>,
    pub section_name_in: Option<Vec<String>>,
    pub min_token_count: Option<usize>,
    pub metadata_key_values: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ClusteringConfig {
    pub min_cluster_size: usize,
    pub epsilon: f32,
    pub label_sample_size: usize,
}

#[derive(Debug, Clone)]
pub struct AlignmentConfig {
    pub alignment_threshold: f32,
    pub min_chunks_for_discovery: usize,
}

#[derive(Debug, Clone)]
pub struct FaultLineConfig {
    pub proximity_threshold: f32,
    pub min_confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ChunkRole {
    Argument,
    Objection,
    Evidence,
    Historical,
    Illustrative,
    Definition,
    OpenQuestion,
    NonArgumentative,
}

impl ChunkRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Argument => "argument",
            Self::Objection => "objection",
            Self::Evidence => "evidence",
            Self::Historical => "historical",
            Self::Illustrative => "illustrative",
            Self::Definition => "definition",
            Self::OpenQuestion => "open_question",
            Self::NonArgumentative => "non_argumentative",
        }
    }
}

/// Returned by the cluster labeling call. Every field is populated by the
/// model. The engine uses these to classify chunks and detect open questions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterLabel {
    pub topic: String,
    pub position_name: Option<String>,
    pub is_argumentative: bool,
    pub is_objection: bool,
    pub is_open_question: bool,
    pub is_coherent: bool,
    /// Set by MultiDomain only. PhilosophyDomain always sets this to "philosophy".
    #[serde(default)]
    pub domain_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_role_as_str_round_trip() {
        let roles = [
            (ChunkRole::Argument, "argument"),
            (ChunkRole::Objection, "objection"),
            (ChunkRole::Evidence, "evidence"),
            (ChunkRole::Historical, "historical"),
            (ChunkRole::Illustrative, "illustrative"),
            (ChunkRole::Definition, "definition"),
            (ChunkRole::OpenQuestion, "open_question"),
            (ChunkRole::NonArgumentative, "non_argumentative"),
        ];
        for (role, expected) in &roles {
            assert_eq!(role.as_str(), *expected);
        }
    }

    #[test]
    fn classify_chunk_role_argumentative() {
        // Use PhilosophyDomain to test the default classify_chunk_role.
        let domain = crate::enrichment::domains::philosophy::PhilosophyDomain;
        let label = ClusterLabel {
            topic: "compatibilism".into(),
            position_name: Some("Compatibilism".into()),
            is_argumentative: true,
            is_objection: false,
            is_open_question: false,
            is_coherent: true,
            domain_id: None,
        };
        assert_eq!(domain.classify_chunk_role(&label), ChunkRole::Argument);
    }

    #[test]
    fn classify_chunk_role_objection() {
        let domain = crate::enrichment::domains::philosophy::PhilosophyDomain;
        let label = ClusterLabel {
            topic: "critique".into(),
            position_name: None,
            is_argumentative: true,
            is_objection: true,
            is_open_question: false,
            is_coherent: true,
            domain_id: None,
        };
        assert_eq!(domain.classify_chunk_role(&label), ChunkRole::Objection);
    }

    #[test]
    fn classify_chunk_role_open_question() {
        let domain = crate::enrichment::domains::philosophy::PhilosophyDomain;
        let label = ClusterLabel {
            topic: "unresolved".into(),
            position_name: None,
            is_argumentative: true,
            is_objection: false,
            is_open_question: true,
            is_coherent: true,
            domain_id: None,
        };
        assert_eq!(domain.classify_chunk_role(&label), ChunkRole::OpenQuestion);
    }

    #[test]
    fn classify_chunk_role_non_argumentative() {
        let domain = crate::enrichment::domains::philosophy::PhilosophyDomain;
        let label = ClusterLabel {
            topic: "definitions".into(),
            position_name: None,
            is_argumentative: false,
            is_objection: false,
            is_open_question: false,
            is_coherent: true,
            domain_id: None,
        };
        assert_eq!(
            domain.classify_chunk_role(&label),
            ChunkRole::NonArgumentative
        );
    }

    #[test]
    fn cluster_label_json_round_trip() {
        let label = ClusterLabel {
            topic: "free will".into(),
            position_name: Some("Compatibilism".into()),
            is_argumentative: true,
            is_objection: false,
            is_open_question: false,
            is_coherent: true,
            domain_id: Some("philosophy".into()),
        };
        let json = serde_json::to_string(&label).unwrap();
        let parsed: ClusterLabel = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.topic, "free will");
        assert_eq!(parsed.position_name.as_deref(), Some("Compatibilism"));
        assert!(parsed.is_argumentative);
        assert_eq!(parsed.domain_id.as_deref(), Some("philosophy"));
    }

    #[test]
    fn cluster_label_missing_domain_defaults_to_none() {
        let json = r#"{"topic":"test","position_name":null,"is_argumentative":true,"is_objection":false,"is_open_question":false,"is_coherent":true}"#;
        let label: ClusterLabel = serde_json::from_str(json).unwrap();
        assert!(label.domain_id.is_none());
    }

    #[test]
    fn chunk_role_serde_round_trip() {
        let role = ChunkRole::Argument;
        let json = serde_json::to_string(&role).unwrap();
        let parsed: ChunkRole = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, role);
    }
}
