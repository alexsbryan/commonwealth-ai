//! InstitutionalDomain — enrichment for engineering + project notes
//! (decisions, invariants, open questions, postmortems) indexed as a
//! `KnowledgeView` over the agent working-notes store.
//!
//! Unlike `PhilosophyDomain` (a field of shared external inquiry) or
//! `PersonalDomain` (one person's emotional/intellectual terrain),
//! `InstitutionalDomain` models a single project's evolving
//! architectural consensus: what was decided, what's locked as an
//! invariant, what's been tried and abandoned, what remains
//! unresolved.
//!
//! The prompts treat positions as *stances the project has taken*,
//! fault lines as *decisions in tension with each other or with
//! established invariants*, and open questions as *uncertainty notes
//! that lack a resolving decision note*.
//!
//! Overview selection uses the `metadata_in` ChunkFilter predicate
//! introduced in Tier 3 item 4: a note counts as an overview
//! document when its `kind` is one of the canonical decision /
//! invariant kinds, filtered by a minimum length threshold.

use super::super::domain::{
    AlignmentConfig, Chunk, ChunkFilter, ClusteringConfig, Domain, FaultLineConfig,
    PositionStatusVocab, QuestionType, SkeletonStorage,
};

/// Notes corpora are bounded — typically hundreds to low thousands
/// of rows per project. Clustering thresholds sized for that range.
const CLUSTERING_MIN_CLUSTER_SIZE: usize = 3;
const CLUSTERING_EPSILON: f32 = 0.15;
const CLUSTERING_LABEL_SAMPLE_SIZE: usize = 4;
const ALIGNMENT_THRESHOLD: f32 = 0.60;
const ALIGNMENT_MIN_CHUNKS_DISCOVERY: usize = 12;
const FAULT_LINE_PROXIMITY_THRESHOLD: f32 = 0.60;
const FAULT_LINE_MIN_CONFIDENCE: f32 = 0.70;
const OVERVIEW_MIN_TOKEN_COUNT: usize = 30;

/// Note kinds that count as "overview" for skeleton extraction.
/// Decisions and invariants are the project's established positions;
/// postmortem_pointer notes capture lessons that shape those positions.
/// Todos / attempts / reflections are excluded — they're process
/// artifacts, not institutional stance.
const OVERVIEW_NOTE_KINDS: &[&str] = &["decision", "invariant", "postmortem_pointer"];

pub struct InstitutionalDomain;

impl Domain for InstitutionalDomain {
    fn id(&self) -> &str {
        "institutional"
    }

    fn name(&self) -> &str {
        "Institutional knowledge"
    }

    fn position_statuses(&self) -> &PositionStatusVocab {
        &PositionStatusVocab {
            dominant: "Established",
            minority: "Proposed",
            contested: "Contested",
            settled: "Locked",
        }
    }

    fn question_types(&self) -> &[QuestionType] {
        &[
            QuestionType::Conceptual,
            QuestionType::Practical,
            QuestionType::Factual,
        ]
    }

    fn overview_filter(&self) -> ChunkFilter {
        ChunkFilter {
            is_first_in_entry: None,
            section_name_in: None,
            min_token_count: Some(OVERVIEW_MIN_TOKEN_COUNT),
            metadata_key_values: vec![],
            // Spec-aligned (Tier 3 item 4): only decisions,
            // invariants, and postmortem_pointers contribute to the
            // skeleton. Other kinds (todo, attempt, reflection)
            // still flow into clustering / open-question detection
            // but don't set the skeleton's position scaffold.
            metadata_in: vec![(
                "kind".to_string(),
                OVERVIEW_NOTE_KINDS.iter().map(|s| s.to_string()).collect(),
            )],
            metadata_compare: vec![],
        }
    }

    fn skeleton_extraction_prompt(&self, chunks: &[&Chunk]) -> String {
        let passages = chunks
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "[Note {} — {}]\n{}",
                    i + 1,
                    c.title.as_deref().unwrap_or("(untitled)"),
                    c.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        format!(
            r#"You are reading engineering decision and invariant notes from a
project's working record. Each note captures a stance the project has
taken: an architectural choice, a constraint that must not be violated,
or a postmortem insight that now shapes future work.

For EACH note, identify (if present):
- The architectural or implementation question it answers, phrased
  as the question a developer would ask encountering this code for
  the first time
- The stance the project has taken (established, proposed,
  contested, locked)
- Any named dependencies / components the stance applies to

IMPORTANT:
- Do not invent questions the note does not answer
- Do not escalate a single decision into an invariant — respect the
  note's declared kind
- If a note is purely procedural (a todo, a one-off attempt), return
  an empty positions array

Notes:
{passages}

Return ONLY a JSON array, one object per note:
[
  {{
    "passage_index": 0,
    "canonical_question": "...",
    "question_type": "conceptual|practical|factual",
    "positions": [
      {{
        "name": "...",
        "claim": "...",
        "status": "Established|Proposed|Contested|Locked",
        "proponents": []
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
            r#"These engineering notes cluster together — they share an
architectural concern. Identify what that concern is.

Notes:
{passages}

Name the architectural domain as the project's own developers would
name it — not as a generic category, as the actual system it
concerns. Two to four words. Lowercase.

Return JSON:
{{
  "topic": "...",
  "position_name": "...",
  "is_argumentative": true,
  "is_objection": false,
  "is_open_question": false,
  "is_coherent": true
}}

`is_open_question` = true if the cluster is primarily unresolved
inquiry rather than a settled architectural stance.
`is_objection` = true if the cluster pushes back on an
established decision.
`is_coherent` = false if the notes don't actually share a concern
on re-reading."#,
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
            r#"Two architectural stances in this project's working record are
in tension. Name the specific conflict between them.

Stance A: {position_a}
Notes representing this stance:
{passages_a}

Stance B: {position_b}
Notes representing this stance:
{passages_b}

Identify:
1. The specific architectural claim where these stances conflict
   (not a cosmetic disagreement — a place where implementing A
   would violate B, or vice versa)
2. What each stance would have to yield to resolve the conflict
3. What new information or benchmark would settle the question

Return JSON:
{{
  "crux": "...",
  "confidence": 0.0,
  "resolution_condition": "..."
}}

`confidence` reflects how explicit the conflict is in the notes
(0.0 = inferred, 1.0 = one note calls out the other by id)."#,
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
            r#"These uncertainty notes register an architectural question that
the project has NOT yet resolved with a decision note.

Notes:
{passages}

Name the question as it would appear in an architecture doc's
"Open questions" section — concrete, actionable, scoped.

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
            max_cluster_points: 0,
            reduced_dims: 0,
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
    use super::super::super::domain::{ComparisonOp, MetadataComparison};

    #[test]
    fn institutional_domain_identity() {
        let d = InstitutionalDomain;
        assert_eq!(d.id(), "institutional");
        assert_eq!(d.name(), "Institutional knowledge");
    }

    #[test]
    fn institutional_domain_is_object_safe() {
        let domain: std::sync::Arc<dyn Domain> = std::sync::Arc::new(InstitutionalDomain);
        assert_eq!(domain.id(), "institutional");
    }

    #[test]
    fn overview_filter_uses_metadata_in_predicate() {
        // Regression guard: the filter must declare a metadata_in
        // predicate on the `kind` key so the field_engine uses the
        // richer metadata-aware chunk path. Otherwise we'd fall back
        // to length-only filtering and admit todos / reflections
        // into the skeleton, muddying the institutional signal.
        let f = InstitutionalDomain.overview_filter();
        assert!(f.requires_metadata(), "must declare metadata predicates");
        assert_eq!(f.metadata_in.len(), 1);
        assert_eq!(f.metadata_in[0].0, "kind");
        let kinds = &f.metadata_in[0].1;
        assert!(kinds.iter().any(|k| k == "decision"));
        assert!(kinds.iter().any(|k| k == "invariant"));
        assert!(kinds.iter().any(|k| k == "postmortem_pointer"));
    }

    #[test]
    fn overview_filter_evaluates_kinds_correctly() {
        let f = InstitutionalDomain.overview_filter();
        let decision = serde_json::json!({"kind": "decision"});
        let invariant = serde_json::json!({"kind": "invariant"});
        let todo = serde_json::json!({"kind": "todo"});
        let reflection = serde_json::json!({"kind": "reflection"});
        assert!(f.evaluate_metadata(&decision));
        assert!(f.evaluate_metadata(&invariant));
        assert!(!f.evaluate_metadata(&todo));
        assert!(!f.evaluate_metadata(&reflection));
    }

    #[test]
    fn overview_filter_compound_with_compare_works() {
        // Sanity check: the filter composes with metadata_compare.
        // This isn't used in the default InstitutionalDomain, but
        // documenting that the vocabulary stays open for future
        // extensions (e.g. "decisions with confidence > 0.8 only").
        let _ = MetadataComparison {
            key: "confidence".to_string(),
            op: ComparisonOp::Gt,
            value: 0.8,
        };
    }

    #[test]
    fn skeleton_extraction_prompt_mentions_engineering_context() {
        let chunk = Chunk {
            id: 1,
            content: "We chose FTS5 over LanceDB for notes search".into(),
            title: Some("decision-fts5".into()),
        };
        let prompt = InstitutionalDomain.skeleton_extraction_prompt(&[&chunk]);
        assert!(prompt.contains("engineering decision"));
        assert!(prompt.contains("Established|Proposed|Contested|Locked"));
        assert!(prompt.contains("[Note 1 — decision-fts5]"));
    }

    #[test]
    fn fault_line_prompt_frames_architectural_conflict() {
        let a = Chunk {
            id: 1,
            content: "corpus-engine stays DB-free".into(),
            title: None,
        };
        let b = Chunk {
            id: 2,
            content: "SqliteAcquirer pulls rusqlite into corpus-engine".into(),
            title: None,
        };
        let prompt = InstitutionalDomain.fault_line_detection_prompt(
            &[&a],
            &[&b],
            "corpus-engine-db-free",
            "direct-sqlite-integration",
        );
        assert!(prompt.contains("Stance A: corpus-engine-db-free"));
        assert!(prompt.contains("Stance B: direct-sqlite-integration"));
        assert!(prompt.contains("implementing A"));
        assert!(prompt.contains("resolution_condition"));
    }

    #[test]
    fn open_question_prompt_frames_unresolved_inquiry() {
        let chunk = Chunk {
            id: 1,
            content: "What's the lock protocol for Tier-2 concurrent writes?".into(),
            title: None,
        };
        let prompt = InstitutionalDomain.open_question_prompt(&[&chunk]);
        assert!(prompt.contains("uncertainty notes"));
        assert!(prompt.contains("Open questions"));
        assert!(prompt.contains("why_unresolved"));
    }

    #[test]
    fn position_statuses_are_engineering_vocab() {
        let v = InstitutionalDomain.position_statuses();
        assert_eq!(v.dominant, "Established");
        assert_eq!(v.minority, "Proposed");
        assert_eq!(v.contested, "Contested");
        assert_eq!(v.settled, "Locked");
    }

    #[test]
    fn skeleton_storage_is_json_and_lance() {
        assert!(matches!(
            InstitutionalDomain.skeleton_storage(),
            SkeletonStorage::JsonAndLance
        ));
    }
}
