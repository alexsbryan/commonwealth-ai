//! Field skeleton types — the primary artifact of enrichment.
//!
//! `FieldSkeleton` is the complete field model, serialized to
//! `field_skeleton.json` in the corpus index directory.
//! `PartialSkeleton` is used during the pipeline as a working structure.

use serde::{Deserialize, Serialize};

use super::clustering::FieldModelStats;

/// A partial skeleton built during Phase 1 (skeleton extraction).
/// Grows as positions are extracted from overview chunks.
#[derive(Debug, Clone, Default)]
pub struct PartialSkeleton {
    pub domain_id: String,
    pub questions: Vec<SkeletonQuestion>,
}

impl PartialSkeleton {
    pub fn new(domain_id: &str) -> Self {
        Self {
            domain_id: domain_id.to_string(),
            questions: Vec::new(),
        }
    }
}

/// A question identified during skeleton extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkeletonQuestion {
    pub id: String,
    pub question: String,
    pub question_type: String,
    pub status: String,
    pub primary_article_ids: Vec<String>,
    pub positions: Vec<SkeletonPosition>,
}

/// A named position within a skeleton question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkeletonPosition {
    pub id: String,
    pub name: String,
    pub claim: String,
    pub status: String,
    pub proponents: Vec<String>,
    pub source: String, // "skeleton" | "discovered"
    pub cluster_ids: Vec<i32>,
    pub centroid_chunk_ids: Vec<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery_confidence: Option<f32>,
}

/// A fault line in the skeleton (from overview extraction or detection).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkeletonFaultLine {
    pub id: String,
    pub between_positions: Vec<String>,
    pub crux: String,
    pub key_chunk_ids: Vec<u64>,
    pub confidence: f32,
    pub source: String, // "skeleton" | "detected"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_condition: Option<String>,
}

/// An open question identified in the field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkeletonOpenQuestion {
    pub id: String,
    pub question: String,
    pub status: String,
    pub related_question_id: Option<String>,
    pub representative_chunk_ids: Vec<u64>,
}

/// The complete field skeleton — the primary enrichment artifact.
/// Serialized to `field_skeleton.json` for small bounded domains.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldSkeleton {
    pub schema_version: u32,
    pub corpus_id: String,
    pub generated_at: String,
    pub extraction_method: String,
    pub prompt_version: String,
    pub domain_id: String,
    pub canonical_questions: Vec<CanonicalQuestion>,
    pub open_questions: Vec<SkeletonOpenQuestion>,
    pub field_stats: FieldModelStats,
}

/// A canonical question with positions and fault lines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalQuestion {
    pub id: String,
    pub question: String,
    pub status: String,
    pub question_type: String,
    pub primary_entries: Vec<String>,
    pub positions: Vec<SkeletonPosition>,
    pub fault_lines: Vec<SkeletonFaultLine>,
}

impl FieldSkeleton {
    /// Find the canonical question most relevant to a query embedding.
    /// Returns the question whose embedded text is most similar.
    pub fn find_question_by_text(&self, topic: &str) -> Option<&CanonicalQuestion> {
        // Simple text matching — for embedding-based search, use LanceDB.
        let topic_lower = topic.to_lowercase();
        self.canonical_questions
            .iter()
            .find(|q| q.question.to_lowercase().contains(&topic_lower))
    }

    /// Look up a position's display name by ID.
    pub fn position_name(&self, position_id: &str) -> Option<&str> {
        for q in &self.canonical_questions {
            for p in &q.positions {
                if p.id == position_id {
                    return Some(&p.name);
                }
            }
        }
        None
    }

    /// Get open questions related to a canonical question.
    pub fn open_questions_for_question(&self, question_id: &str) -> Vec<&SkeletonOpenQuestion> {
        self.open_questions
            .iter()
            .filter(|oq| oq.related_question_id.as_deref() == Some(question_id))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.canonical_questions.is_empty()
    }
}
