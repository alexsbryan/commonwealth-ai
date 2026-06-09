// SPDX-License-Identifier: AGPL-3.0-or-later
//! PhilosophyDomain — full implementation for the Stanford Encyclopedia of Philosophy.

use super::super::domain::{
    AlignmentConfig, Chunk, ChunkFilter, ClusteringConfig, Domain, FaultLineConfig,
    PositionStatusVocab, QuestionType, SkeletonStorage,
};

const CLUSTERING_MIN_CLUSTER_SIZE: usize = 50;
const CLUSTERING_EPSILON: f32 = 0.10;
const CLUSTERING_LABEL_SAMPLE_SIZE: usize = 5;
const ALIGNMENT_THRESHOLD: f32 = 0.65;
const ALIGNMENT_MIN_CHUNKS_DISCOVERY: usize = 80;
const FAULT_LINE_PROXIMITY_THRESHOLD: f32 = 0.60;
const FAULT_LINE_MIN_CONFIDENCE: f32 = 0.70;
const OVERVIEW_MIN_TOKEN_COUNT: usize = 80;

const OVERVIEW_SECTION_NAMES: &[&str] = &[
    "", // unnamed opening section — the most common case in SEP
    "Introduction",
    "Overview",
    "Preliminary Remarks",
    "Background",
];

pub struct PhilosophyDomain;

impl Domain for PhilosophyDomain {
    fn id(&self) -> &str {
        "philosophy"
    }

    fn name(&self) -> &str {
        "Philosophy"
    }

    fn position_statuses(&self) -> &PositionStatusVocab {
        &PositionStatusVocab {
            dominant: "Majority view",
            minority: "Minority position",
            contested: "Contested",
            settled: "Established",
        }
    }

    fn question_types(&self) -> &[QuestionType] {
        &[
            QuestionType::Factual,
            QuestionType::Normative,
            QuestionType::Conceptual,
        ]
    }

    fn overview_filter(&self) -> ChunkFilter {
        ChunkFilter {
            is_first_in_entry: Some(true),
            section_name_in: Some(
                OVERVIEW_SECTION_NAMES
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
            min_token_count: Some(OVERVIEW_MIN_TOKEN_COUNT),
            metadata_key_values: vec![],
            metadata_in: vec![],
            metadata_compare: vec![],
        }
    }

    fn skeleton_extraction_prompt(&self, chunks: &[&Chunk]) -> String {
        let passages = chunks
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "[Passage {} — {}]\n{}",
                    i + 1,
                    c.title.as_deref().unwrap_or_default(),
                    c.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        format!(
            r#"You are reading introductory passages from the Stanford Encyclopedia of Philosophy.
Identify the structure of philosophical debate in each passage.

For EACH passage, extract (if present):
- The canonical question being addressed (a specific, debatable question)
- Named philosophical positions (use the names the text itself uses)
- Key proponents (named philosophers or named traditions only)
- Epistemic status: "majority", "minority", or "contested"

IMPORTANT EXCLUSIONS:
- Do not extract positions from thought experiments or fictional scenarios
  ("suppose Tim and Harry...", "imagine a surgeon who...") — these are
  illustrations, not positions
- Named positions must appear in the text. Do not invent names.
- Proponents must be named individuals or named schools. Not "some philosophers".
- If a passage is purely historical, biographical, or definitional, return
  an empty positions array.

Passages:
{passages}

Return ONLY a JSON array, one object per passage:
[
  {{
    "passage_index": 0,
    "canonical_question": "...",
    "question_type": "factual|normative|conceptual",
    "positions": [
      {{
        "name": "...",
        "claim": "...",
        "status": "majority|minority|contested",
        "proponents": ["..."]
      }}
    ]
  }}
]"#,
            passages = passages
        )
    }

    fn cluster_labeling_prompt(&self, representative_chunks: &[&Chunk]) -> String {
        let passages = representative_chunks
            .iter()
            .map(|c| c.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        format!(
            r#"These passages from a philosophy encyclopedia are semantically similar.
Characterize what they represent.

Passages:
{passages}

Return JSON:
{{
  "topic": "...",
  "position_name": "...",
  "is_argumentative": true,
  "is_objection": false,
  "is_open_question": false,
  "is_coherent": true
}}"#,
            passages = passages
        )
    }

    fn fault_line_detection_prompt(
        &self,
        chunks_a: &[&Chunk],
        chunks_b: &[&Chunk],
        position_a: &str,
        position_b: &str,
    ) -> String {
        let format_passages = |chunks: &[&Chunk]| {
            chunks
                .iter()
                .map(|c| c.content.as_str())
                .collect::<Vec<_>>()
                .join("\n---\n")
        };

        format!(
            r#"Two philosophical positions are in dialogue. Identify the specific crux of
their disagreement.

Position A: {position_a}
Passages representing this position:
{passages_a}

Position B: {position_b}
Passages representing this position:
{passages_b}

Identify:
1. The specific claim or argument where these positions directly conflict
2. What each position says in response to the other's strongest challenge
3. What fact or argument, if established, would resolve the dispute

Return JSON:
{{
  "crux": "...",
  "confidence": 0.0,
  "resolution_condition": "..."
}}"#,
            position_a = position_a,
            position_b = position_b,
            passages_a = format_passages(chunks_a),
            passages_b = format_passages(chunks_b),
        )
    }

    fn open_question_prompt(&self, chunks: &[&Chunk]) -> String {
        let passages = chunks
            .iter()
            .map(|c| c.content.as_str())
            .collect::<Vec<_>>()
            .join("\n---\n");

        format!(
            r#"These passages from a philosophy encyclopedia reflect unresolved inquiry.
Characterize the open question they represent.

Passages:
{passages}

Return JSON:
{{
  "question": "...",
  "why_unresolved": "..."
}}"#,
            passages = passages
        )
    }

    fn clustering_config(&self) -> ClusteringConfig {
        ClusteringConfig {
            min_cluster_size: CLUSTERING_MIN_CLUSTER_SIZE,
            epsilon: CLUSTERING_EPSILON,
            label_sample_size: CLUSTERING_LABEL_SAMPLE_SIZE,
            max_cluster_points: 30_000,
            reduced_dims: 128,
        }
    }

    fn alignment_config(&self) -> AlignmentConfig {
        AlignmentConfig {
            alignment_threshold: ALIGNMENT_THRESHOLD,
            min_chunks_for_discovery: ALIGNMENT_MIN_CHUNKS_DISCOVERY,
        }
    }

    fn fault_line_config(&self) -> FaultLineConfig {
        FaultLineConfig {
            proximity_threshold: FAULT_LINE_PROXIMITY_THRESHOLD,
            min_confidence: FAULT_LINE_MIN_CONFIDENCE,
        }
    }

    fn skeleton_storage(&self) -> SkeletonStorage {
        SkeletonStorage::JsonAndLance
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn philosophy_domain_is_object_safe() {
        let domain: std::sync::Arc<dyn Domain> = std::sync::Arc::new(PhilosophyDomain);
        assert_eq!(domain.id(), "philosophy");
        assert_eq!(domain.name(), "Philosophy");
    }

    #[test]
    fn clustering_config_values() {
        let config = PhilosophyDomain.clustering_config();
        assert_eq!(config.min_cluster_size, 50);
        assert!((config.epsilon - 0.10).abs() < f32::EPSILON);
        assert_eq!(config.label_sample_size, 5);
    }

    #[test]
    fn overview_filter_includes_empty_section() {
        let filter = PhilosophyDomain.overview_filter();
        let sections = filter.section_name_in.unwrap();
        assert!(sections.contains(&String::new()));
        assert!(sections.contains(&"Introduction".to_string()));
    }

    #[test]
    fn skeleton_storage_is_json_and_lance() {
        assert!(matches!(
            PhilosophyDomain.skeleton_storage(),
            SkeletonStorage::JsonAndLance
        ));
    }

    #[test]
    fn skeleton_extraction_prompt_not_empty() {
        let chunk = Chunk {
            id: 1,
            content: "Free will is the ability to act otherwise.".into(),
            title: Some("Free Will".into()),
        };
        let prompt = PhilosophyDomain.skeleton_extraction_prompt(&[&chunk]);
        assert!(!prompt.is_empty());
        assert!(prompt.contains("Free Will"));
        assert!(prompt.contains("canonical question"));
    }

    #[test]
    fn skeleton_extraction_prompt_contains_instructions() {
        let chunk = Chunk {
            id: 1,
            content: "Compatibilists hold that free will is consistent with determinism.".into(),
            title: Some("Compatibilism".into()),
        };
        let prompt = PhilosophyDomain.skeleton_extraction_prompt(&[&chunk]);
        // Must include key instructions
        assert!(prompt.contains("IMPORTANT EXCLUSIONS"));
        assert!(prompt.contains("thought experiments"));
        assert!(prompt.contains("JSON array"));
        assert!(prompt.contains("passage_index"));
        assert!(prompt.contains("proponents"));
    }

    #[test]
    fn skeleton_extraction_prompt_handles_multiple_chunks() {
        let chunks = [
            Chunk {
                id: 1,
                content: "First passage about free will.".into(),
                title: Some("Free Will".into()),
            },
            Chunk {
                id: 2,
                content: "Second passage about determinism.".into(),
                title: Some("Determinism".into()),
            },
            Chunk {
                id: 3,
                content: "Third passage about moral responsibility.".into(),
                title: None,
            },
        ];
        let refs: Vec<&Chunk> = chunks.iter().collect();
        let prompt = PhilosophyDomain.skeleton_extraction_prompt(&refs);
        assert!(prompt.contains("[Passage 1 — Free Will]"));
        assert!(prompt.contains("[Passage 2 — Determinism]"));
        assert!(prompt.contains("[Passage 3 — ]")); // no title
    }

    #[test]
    fn cluster_labeling_prompt_structure() {
        let chunk = Chunk {
            id: 1,
            content: "Frankfurt cases show that moral responsibility does not require alternative possibilities.".into(),
            title: None,
        };
        let prompt = PhilosophyDomain.cluster_labeling_prompt(&[&chunk]);
        assert!(prompt.contains("semantically similar"));
        assert!(prompt.contains("position_name"));
        assert!(prompt.contains("is_argumentative"));
        assert!(prompt.contains("is_objection"));
        assert!(prompt.contains("is_open_question"));
        assert!(prompt.contains("is_coherent"));
        assert!(prompt.contains("Frankfurt cases"));
    }

    #[test]
    fn fault_line_detection_prompt_includes_both_positions() {
        let chunk_a = Chunk {
            id: 1,
            content: "Compatibilism argues that free will is consistent with determinism.".into(),
            title: None,
        };
        let chunk_b = Chunk {
            id: 2,
            content:
                "Hard incompatibilism denies free will under both determinism and indeterminism."
                    .into(),
            title: None,
        };
        let prompt = PhilosophyDomain.fault_line_detection_prompt(
            &[&chunk_a],
            &[&chunk_b],
            "Compatibilism",
            "Hard Incompatibilism",
        );
        assert!(prompt.contains("Position A: Compatibilism"));
        assert!(prompt.contains("Position B: Hard Incompatibilism"));
        assert!(prompt.contains("crux"));
        assert!(prompt.contains("confidence"));
        assert!(prompt.contains("resolution_condition"));
    }

    #[test]
    fn open_question_prompt_structure() {
        let chunk = Chunk {
            id: 1,
            content: "It remains unclear whether manipulation arguments undermine compatibilism."
                .into(),
            title: None,
        };
        let prompt = PhilosophyDomain.open_question_prompt(&[&chunk]);
        assert!(prompt.contains("unresolved inquiry"));
        assert!(prompt.contains("question"));
        assert!(prompt.contains("why_unresolved"));
        assert!(prompt.contains("manipulation arguments"));
    }

    #[test]
    fn alignment_config_values() {
        let config = PhilosophyDomain.alignment_config();
        assert!((config.alignment_threshold - 0.65).abs() < f32::EPSILON);
        assert_eq!(config.min_chunks_for_discovery, 80);
    }

    #[test]
    fn fault_line_config_values() {
        let config = PhilosophyDomain.fault_line_config();
        assert!((config.proximity_threshold - 0.60).abs() < f32::EPSILON);
        assert!((config.min_confidence - 0.70).abs() < f32::EPSILON);
    }

    #[test]
    fn position_statuses_vocabulary() {
        let vocab = PhilosophyDomain.position_statuses();
        assert_eq!(vocab.dominant, "Majority view");
        assert_eq!(vocab.minority, "Minority position");
        assert_eq!(vocab.contested, "Contested");
        assert_eq!(vocab.settled, "Established");
    }

    #[test]
    fn question_types_include_core_three() {
        let types = PhilosophyDomain.question_types();
        assert_eq!(types.len(), 3);
        assert!(types.contains(&super::super::super::domain::QuestionType::Factual));
        assert!(types.contains(&super::super::super::domain::QuestionType::Normative));
        assert!(types.contains(&super::super::super::domain::QuestionType::Conceptual));
    }
}
