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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_skeleton() -> FieldSkeleton {
        FieldSkeleton {
            schema_version: 1,
            corpus_id: "sep".into(),
            generated_at: "2026-04-09T00:00:00Z".into(),
            extraction_method: "dual_pass_v1".into(),
            prompt_version: "1.0.0".into(),
            domain_id: "philosophy".into(),
            canonical_questions: vec![
                CanonicalQuestion {
                    id: "q_free_will".into(),
                    question: "Is free will compatible with determinism?".into(),
                    status: "contested".into(),
                    question_type: "conceptual".into(),
                    primary_entries: vec!["Free Will".into()],
                    positions: vec![
                        SkeletonPosition {
                            id: "p_compatibilism".into(),
                            name: "Compatibilism".into(),
                            claim: "Free will is compatible with determinism".into(),
                            status: "majority".into(),
                            proponents: vec!["Frankfurt".into(), "Dennett".into()],
                            source: "skeleton".into(),
                            cluster_ids: vec![1, 2],
                            centroid_chunk_ids: vec![100, 200],
                            discovery_confidence: None,
                        },
                        SkeletonPosition {
                            id: "p_hard_incompatibilism".into(),
                            name: "Hard Incompatibilism".into(),
                            claim: "Moral responsibility is impossible".into(),
                            status: "minority".into(),
                            proponents: vec!["Pereboom".into()],
                            source: "skeleton".into(),
                            cluster_ids: vec![3],
                            centroid_chunk_ids: vec![300],
                            discovery_confidence: None,
                        },
                    ],
                    fault_lines: vec![SkeletonFaultLine {
                        id: "fl_1".into(),
                        between_positions: vec![
                            "p_compatibilism".into(),
                            "p_hard_incompatibilism".into(),
                        ],
                        crux: "Whether alternative possibilities are required".into(),
                        key_chunk_ids: vec![100, 300],
                        confidence: 0.91,
                        source: "detected".into(),
                        resolution_condition: None,
                    }],
                },
                CanonicalQuestion {
                    id: "q_personal_identity".into(),
                    question: "What constitutes personal identity over time?".into(),
                    status: "contested".into(),
                    question_type: "conceptual".into(),
                    primary_entries: vec!["Personal Identity".into()],
                    positions: vec![],
                    fault_lines: vec![],
                },
            ],
            open_questions: vec![SkeletonOpenQuestion {
                id: "oq_1".into(),
                question: "What explains manipulation arguments?".into(),
                status: "active_research".into(),
                related_question_id: Some("q_free_will".into()),
                representative_chunk_ids: vec![500],
            }],
            field_stats: FieldModelStats::default(),
        }
    }

    #[test]
    fn find_question_by_text_matches() {
        let skeleton = test_skeleton();
        let q = skeleton.find_question_by_text("free will").unwrap();
        assert_eq!(q.id, "q_free_will");
    }

    #[test]
    fn find_question_by_text_case_insensitive() {
        let skeleton = test_skeleton();
        let q = skeleton.find_question_by_text("FREE WILL").unwrap();
        assert_eq!(q.id, "q_free_will");
    }

    #[test]
    fn find_question_by_text_no_match() {
        let skeleton = test_skeleton();
        assert!(skeleton
            .find_question_by_text("quantum mechanics")
            .is_none());
    }

    #[test]
    fn position_name_lookup() {
        let skeleton = test_skeleton();
        assert_eq!(
            skeleton.position_name("p_compatibilism"),
            Some("Compatibilism")
        );
        assert_eq!(
            skeleton.position_name("p_hard_incompatibilism"),
            Some("Hard Incompatibilism")
        );
        assert!(skeleton.position_name("p_nonexistent").is_none());
    }

    #[test]
    fn open_questions_for_question_filters() {
        let skeleton = test_skeleton();
        let oqs = skeleton.open_questions_for_question("q_free_will");
        assert_eq!(oqs.len(), 1);
        assert_eq!(oqs[0].question, "What explains manipulation arguments?");

        let oqs_empty = skeleton.open_questions_for_question("q_personal_identity");
        assert!(oqs_empty.is_empty());
    }

    #[test]
    fn is_empty_checks_questions() {
        let skeleton = test_skeleton();
        assert!(!skeleton.is_empty());

        let empty = FieldSkeleton {
            canonical_questions: vec![],
            ..test_skeleton()
        };
        assert!(empty.is_empty());
    }

    #[test]
    fn skeleton_json_round_trip() {
        let skeleton = test_skeleton();
        let json = serde_json::to_string_pretty(&skeleton).unwrap();
        let parsed: FieldSkeleton = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.corpus_id, "sep");
        assert_eq!(parsed.domain_id, "philosophy");
        assert_eq!(parsed.canonical_questions.len(), 2);
        assert_eq!(parsed.canonical_questions[0].positions.len(), 2);
        assert_eq!(parsed.canonical_questions[0].fault_lines.len(), 1);
        assert_eq!(parsed.open_questions.len(), 1);

        let pos = &parsed.canonical_questions[0].positions[0];
        assert_eq!(pos.name, "Compatibilism");
        assert_eq!(pos.proponents, vec!["Frankfurt", "Dennett"]);
        assert_eq!(pos.source, "skeleton");
        assert!(pos.discovery_confidence.is_none());
    }

    #[test]
    fn partial_skeleton_new() {
        let ps = PartialSkeleton::new("philosophy");
        assert_eq!(ps.domain_id, "philosophy");
        assert!(ps.questions.is_empty());
    }
}
