//! PersonalDomain — enrichment for personal memories surfaced as a
//! `KnowledgeView` over the `memories` table.
//!
//! Unlike `PhilosophyDomain`, which models a field of shared inquiry,
//! `PersonalDomain` models a single person's intellectual and emotional
//! terrain. "Positions" here are the stances a person has taken across
//! memories; "fault lines" are places where those stances are in tension;
//! "open questions" are the questions the person keeps returning to
//! without resolution.
//!
//! The prompts are framed as a thoughtful friend might frame them —
//! not as a clinical observer. The output surfaces a landscape, not a
//! diagnosis.

use super::super::domain::{
    AlignmentConfig, Chunk, ChunkFilter, ClusteringConfig, ComparisonOp, Domain, FaultLineConfig,
    MetadataComparison, PositionStatusVocab, QuestionType, SkeletonStorage,
};

/// Personal corpora are small by design — one person's memories over
/// months or years, typically hundreds to low thousands of rows.
/// `min_cluster_size` must be small enough that meaningful clusters
/// emerge on a few dozen memories.
const CLUSTERING_MIN_CLUSTER_SIZE: usize = 3;
const CLUSTERING_EPSILON: f32 = 0.15;
const CLUSTERING_LABEL_SAMPLE_SIZE: usize = 5;
const ALIGNMENT_THRESHOLD: f32 = 0.55;
const ALIGNMENT_MIN_CHUNKS_DISCOVERY: usize = 12;
const FAULT_LINE_PROXIMITY_THRESHOLD: f32 = 0.55;
const FAULT_LINE_MIN_CONFIDENCE: f32 = 0.65;
const OVERVIEW_MIN_TOKEN_COUNT: usize = 30;

pub struct PersonalDomain;

impl Domain for PersonalDomain {
    fn id(&self) -> &str {
        "personal"
    }

    fn name(&self) -> &str {
        "Personal knowledge"
    }

    fn position_statuses(&self) -> &PositionStatusVocab {
        &PositionStatusVocab {
            dominant: "Held",
            minority: "Tentative",
            contested: "In tension",
            settled: "Settled",
        }
    }

    fn question_types(&self) -> &[QuestionType] {
        &[
            QuestionType::Normative,  // values, what matters
            QuestionType::Conceptual, // self-understanding, framing
            QuestionType::Factual,    // lived events recalled
        ]
    }

    fn overview_filter(&self) -> ChunkFilter {
        // Spec-aligned predicate (Tier 3 item 4): a memory counts as
        // an "overview document" when its extracted confidence is
        // high enough that the position it expresses is settled
        // rather than passing. The length threshold is kept as an
        // AND-guard against pathologically short high-confidence
        // memories ("I'm tired") that would add noise.
        ChunkFilter {
            is_first_in_entry: None,
            section_name_in: None,
            min_token_count: Some(OVERVIEW_MIN_TOKEN_COUNT),
            metadata_key_values: vec![],
            metadata_in: vec![],
            metadata_compare: vec![MetadataComparison {
                key: "confidence".to_string(),
                op: ComparisonOp::Gt,
                value: 0.7,
            }],
        }
    }

    fn skeleton_extraction_prompt(&self, chunks: &[&Chunk]) -> String {
        let passages = chunks
            .iter()
            .enumerate()
            .map(|(i, c)| format!("[Memory {}]\n{}", i + 1, c.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        format!(
            r#"You are reading memories from one person's long-term record. These
are things the person has said about themselves, their work, their
relationships, and what they care about.

For EACH memory, identify (if present):
- The central concern, value, or question it expresses — stated as
  the person would recognize it, not as a clinical category
- The stance the memory takes on that concern (held, tentative,
  in tension with something else)

IMPORTANT:
- Do not invent framings the memory does not support
- Do not treat one-time statements as settled positions
- If a memory is purely factual or logistical, return an empty
  positions array

Memories:
{passages}

Return ONLY a JSON array, one object per memory:
[
  {{
    "passage_index": 0,
    "canonical_question": "...",
    "question_type": "normative|conceptual|factual",
    "positions": [
      {{
        "name": "...",
        "claim": "...",
        "status": "held|tentative|in tension|settled",
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
            r#"These memories from one person's record cluster together — they
share a theme the person returns to.

Memories:
{passages}

Name the domain of concern they share, as a thoughtful friend would
name it — not as a clinical category. Two to four words. Lowercase.

Return JSON:
{{
  "topic": "...",
  "position_name": "...",
  "is_argumentative": false,
  "is_objection": false,
  "is_open_question": false,
  "is_coherent": true
}}

`is_open_question` = true if the cluster is primarily unresolved
inquiry rather than a settled concern.
`is_argumentative` = true if the memories stake out a stance (as
opposed to describing events or logistics).
`is_coherent` = false if the memories don't actually share a theme
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
            r#"Two stances from the same person's memories are in tension. Name
the specific tension without trying to resolve it.

Stance A: {position_a}
Memories expressing this stance:
{passages_a}

Stance B: {position_b}
Memories expressing this stance:
{passages_b}

Identify:
1. The specific point where the stances are in genuine tension
   (not a surface contradiction — a place where the person's
   thinking is live, not settled)
2. What each stance says that the other would have to answer to

Return JSON:
{{
  "crux": "...",
  "confidence": 0.0,
  "resolution_condition": "..."
}}

`confidence` reflects how clearly the tension is in the memories
themselves (0.0 = reading it in, 1.0 = explicit in the text)."#,
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
            r#"These memories reflect something the person keeps returning to
without arriving at a stable answer.

Memories:
{passages}

Name the question they are asking — as the person might phrase it
if they were to name their own inquiry. Not as a research question,
as a live uncertainty.

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
            max_cluster_points: 0, // no cap — personal corpora are small
            reduced_dims: 0,       // no projection on small data
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
        // Personal corpora are bounded and small — `JsonAndLance` lets
        // the KnowledgeView landscape digest read `field_skeleton.json`
        // directly without a LanceDB scan.
        SkeletonStorage::JsonAndLance
    }

    fn entity_extraction_prompt(&self, chunks: &[&Chunk]) -> Option<String> {
        // Empty-slice probe: the engine asks "do you opt in?" before
        // dispatching any inference. Personal opts in.
        if chunks.is_empty() {
            return Some(String::new());
        }

        let passages = chunks
            .iter()
            .enumerate()
            .map(|(i, c)| format!("[Memory {}]\n{}", i + 1, c.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        Some(format!(
            r#"You are reading memories from one person's long-term record. Your
job is named-entity extraction: identify the *people*, *organizations*,
and *initiatives* the user mentions. The user IS the subject of these
memories — do not extract them.

Definitions:
- **Person**: a named individual (e.g. "Sarah Chen"). If the memory
  states an organization or role, capture it.
- **Organization**: a named company, institution, or group ("Acme Corp",
  "the design team"). If the memory implies the user's relationship to
  it (employer, client, vendor, partner), capture it as `relationship`.
- **Initiative**: a concrete project, goal, or piece of ongoing work the
  user is involved in ("the Q3 launch", "API migration", "reduce churn"
  as an active effort). Topics of casual reflection ("I think about
  craft") are NOT initiatives — initiatives imply effort toward a
  future state.

  Tactics, milestones, sub-strategies, implementation paths, or
  work artifacts *within* an initiative are NOT separate initiatives
  — they belong to the parent. "API migration" is an initiative;
  the "parallel migration path" the user agreed to ship is a tactic
  inside it, not a separate initiative.

  Single-memory work products are NOT initiatives — they're
  artifacts. "the migration plan revision", "the SOC2 crosswalk",
  "the SOW reformat", "a scoping doc" are work artifacts, not
  initiatives, even when the user is actively producing them. An
  initiative has a stable name across multiple memories.

  Use the canonical name without possessive prefixes or scope
  suffixes — write "Q3 enterprise push", not "Acme's Q3 enterprise
  push"; write "Architecture refresh", not "Architecture refresh
  discovery". Capture organizational ownership through
  `participants`, not in the name; capture phase or status through
  the `status` field.

When the same person, organization, or initiative is referenced both
by short form ("Mike", "Acme") and long form ("Mike Torres",
"Acme Corp") across memories, prefer the long form when any memory
provides it — the post-extraction merger will resolve short-form
references to the long-form atom.

Use the [Memory N] labels to record where each entity appeared. The
`mentions` array on each entity is required; list the memory labels
that mention it. If you list a person as a participant on an initiative,
make sure that person also appears in the `persons` array.

Memories:
{passages}

Return ONLY a JSON object:
{{
  "persons": [
    {{
      "name": "Sarah Chen",
      "affiliation": "Acme Corp",
      "role": "VP Engineering",
      "mentions": ["Memory 1"]
    }}
  ],
  "organizations": [
    {{
      "name": "Acme Corp",
      "relationship": "client",
      "mentions": ["Memory 1"]
    }}
  ],
  "initiatives": [
    {{
      "name": "Q3 enterprise push",
      "status": "team aligning on vertical focus",
      "participants": ["Sarah Chen", "Acme Corp"],
      "mentions": ["Memory 2"]
    }}
  ]
}}

Empty arrays for any kind that didn't appear. Omit the description,
affiliation, role, status, or relationship fields when the memory
doesn't support them — do not invent."#,
            passages = passages
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_domain_identity() {
        let d = PersonalDomain;
        assert_eq!(d.id(), "personal");
        assert_eq!(d.name(), "Personal knowledge");
    }

    #[test]
    fn personal_domain_is_object_safe() {
        let domain: std::sync::Arc<dyn Domain> = std::sync::Arc::new(PersonalDomain);
        assert_eq!(domain.id(), "personal");
    }

    #[test]
    fn clustering_config_values() {
        let c = PersonalDomain.clustering_config();
        assert_eq!(c.min_cluster_size, CLUSTERING_MIN_CLUSTER_SIZE);
        assert!((c.epsilon - CLUSTERING_EPSILON).abs() < f32::EPSILON);
    }

    #[test]
    fn overview_filter_uses_token_count() {
        let f = PersonalDomain.overview_filter();
        assert_eq!(f.min_token_count, Some(OVERVIEW_MIN_TOKEN_COUNT));
        assert!(f.section_name_in.is_none());
    }

    #[test]
    fn skeleton_extraction_prompt_mentions_memories() {
        let chunk = Chunk {
            id: 1,
            content: "I've been thinking about what good work looks like.".into(),
            title: None,
        };
        let prompt = PersonalDomain.skeleton_extraction_prompt(&[&chunk]);
        assert!(prompt.contains("memories from one person"));
        assert!(prompt.contains("[Memory 1]"));
        assert!(prompt.contains("canonical_question"));
    }

    #[test]
    fn cluster_labeling_prompt_mentions_theme() {
        let chunk = Chunk {
            id: 1,
            content: "I keep coming back to this question about autonomy.".into(),
            title: None,
        };
        let prompt = PersonalDomain.cluster_labeling_prompt(&[&chunk]);
        assert!(prompt.contains("theme"));
        assert!(prompt.contains("is_coherent"));
    }

    #[test]
    fn fault_line_prompt_frames_tension() {
        let chunk_a = Chunk {
            id: 1,
            content: "I value simplicity in everything I build.".into(),
            title: None,
        };
        let chunk_b = Chunk {
            id: 2,
            content: "I'm excited to architect this complex system.".into(),
            title: None,
        };
        let prompt = PersonalDomain.fault_line_detection_prompt(
            &[&chunk_a],
            &[&chunk_b],
            "simplicity",
            "complexity attraction",
        );
        assert!(prompt.contains("Stance A: simplicity"));
        assert!(prompt.contains("Stance B: complexity attraction"));
        assert!(prompt.contains("without trying to resolve it"));
    }

    #[test]
    fn open_question_prompt_frames_live_uncertainty() {
        let chunk = Chunk {
            id: 1,
            content: "What kind of life do I actually want?".into(),
            title: None,
        };
        let prompt = PersonalDomain.open_question_prompt(&[&chunk]);
        assert!(prompt.contains("stable answer"));
        assert!(prompt.contains("live uncertainty"));
        assert!(prompt.contains("why_unresolved"));
    }

    #[test]
    fn skeleton_storage_is_json_and_lance() {
        assert!(matches!(
            PersonalDomain.skeleton_storage(),
            SkeletonStorage::JsonAndLance
        ));
    }
}
