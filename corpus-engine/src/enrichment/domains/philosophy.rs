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
    "",                   // unnamed opening section — the most common case in SEP
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
}
