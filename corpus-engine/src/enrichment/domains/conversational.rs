//! ConversationalDomain — enrichment for conversations indexed as a
//! `KnowledgeView` over the `conversations` + `messages` tables.
//!
//! Each document is a single conversation assembled by the acquirer
//! (user/assistant turns concatenated). Clusters surface recurring
//! topics across many conversations; fault lines surface genuinely
//! unresolved threads; open questions surface questions the person
//! keeps asking without landing on a stable answer.
//!
//! Scope is bounded by a time window (typically 180 days), enforced
//! at the acquirer layer. Conversations tagged with a `privacy =
//! "local_only"` skill (notably `inner-work`) are filtered out of
//! this view at ingest by the acquirer — the separation is
//! structural, not enforced at enrichment time.

use super::super::domain::{
    AlignmentConfig, Chunk, ChunkFilter, ClusteringConfig, Domain, FaultLineConfig,
    PositionStatusVocab, QuestionType, SkeletonStorage,
};

/// Conversational corpora are small-to-medium — one user's 180-day
/// window is typically tens to low hundreds of documents. `min_cluster_size`
/// is small so rare recurring topics still cluster.
const CLUSTERING_MIN_CLUSTER_SIZE: usize = 2;
const CLUSTERING_EPSILON: f32 = 0.18;
const CLUSTERING_LABEL_SAMPLE_SIZE: usize = 3;
const ALIGNMENT_THRESHOLD: f32 = 0.55;
const ALIGNMENT_MIN_CHUNKS_DISCOVERY: usize = 8;
const FAULT_LINE_PROXIMITY_THRESHOLD: f32 = 0.55;
const FAULT_LINE_MIN_CONFIDENCE: f32 = 0.65;
/// Conversations that aren't at least this many tokens are excluded
/// from overview-document selection. Keeps one-line quick questions
/// out of the skeleton.
const OVERVIEW_MIN_TOKEN_COUNT: usize = 200;

pub struct ConversationalDomain;

impl Domain for ConversationalDomain {
    fn id(&self) -> &str {
        "conversational"
    }

    fn name(&self) -> &str {
        "Conversational knowledge"
    }

    fn position_statuses(&self) -> &PositionStatusVocab {
        &PositionStatusVocab {
            dominant: "Recurring",
            minority: "One-off",
            contested: "Unresolved",
            settled: "Concluded",
        }
    }

    fn question_types(&self) -> &[QuestionType] {
        &[
            QuestionType::Factual,
            QuestionType::Conceptual,
            QuestionType::Practical,
            QuestionType::Normative,
        ]
    }

    fn overview_filter(&self) -> ChunkFilter {
        // Tier 3 item 4: `metadata_in` now lets us express the
        // spec's intended predicate — a conversation is an overview
        // document when the active skill was one of the research /
        // framing skills. Length threshold kept as an AND-guard
        // against pathologically short one-message conversations.
        ChunkFilter {
            is_first_in_entry: None,
            section_name_in: None,
            min_token_count: Some(OVERVIEW_MIN_TOKEN_COUNT),
            metadata_key_values: vec![],
            metadata_in: vec![(
                "skill_id".to_string(),
                vec![
                    "research-analyst".to_string(),
                    "epistemic-research".to_string(),
                ],
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
                    "[Conversation {} — {}]\n{}",
                    i + 1,
                    c.title.as_deref().unwrap_or("(untitled)"),
                    c.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
            ;

        format!(
            r#"You are reading conversations between one person and an AI
assistant. Identify the central question or goal that organized
each conversation.

For EACH conversation, extract (if present):
- The question or goal the person brought to the conversation,
  phrased as they would phrase it
- Whether the conversation reached a stable answer, ended with
  an open thread, or was deferred

IMPORTANT:
- Do not describe what the assistant said — describe what the
  person was trying to work out
- If the conversation was purely logistical or one-off Q&A,
  return an empty positions array

Conversations:
{passages}

Return ONLY a JSON array, one object per conversation:
[
  {{
    "passage_index": 0,
    "canonical_question": "...",
    "question_type": "factual|conceptual|practical|normative",
    "positions": [
      {{
        "name": "...",
        "claim": "...",
        "status": "Concluded|Unresolved|Recurring|One-off",
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
            .map(|c| {
                format!(
                    "— {} —\n{}",
                    c.title.as_deref().unwrap_or("(untitled)"),
                    c.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        format!(
            r#"These conversations cluster together — they share an underlying
domain of inquiry. Identify what that domain is.

Conversations:
{passages}

Name it as the person would name their own work — not as a category,
as a practice. Two to four words. Lowercase.

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
inquiry rather than a domain the person has settled views on.
`is_coherent` = false if the conversations don't actually share a
theme on re-reading."#,
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
            r#"Two threads from this person's conversation history are in
tension — they addressed the same underlying question but landed
in different places, or one raised a concern the other didn't
answer. Name the tension.

Thread A: {position_a}
Conversations in this thread:
{passages_a}

Thread B: {position_b}
Conversations in this thread:
{passages_b}

Identify:
1. The specific unresolved point between the two threads
2. What Thread B raised that Thread A didn't address (or vice versa)
3. What would resolve it — a decision, more data, a different framing

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
            r#"Across these conversations the person kept raising a question
without arriving at a stable answer.

Conversations:
{passages}

Name the question as the person would phrase it if asked directly,
"what are you still trying to figure out?"

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

    fn entity_extraction_prompt(&self, chunks: &[&Chunk]) -> Option<String> {
        // Empty-slice probe: the engine asks "do you opt in?" before
        // dispatching any inference. Conversational opts in.
        if chunks.is_empty() {
            return Some(String::new());
        }

        let passages = chunks
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "[Conversation {} — {}]\n{}",
                    i + 1,
                    c.title.as_deref().unwrap_or("(untitled)"),
                    c.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        Some(format!(
            r#"You are reading conversations between one person (the user) and an
AI assistant. Your job is named-entity extraction: identify the
*people*, *organizations*, and *initiatives* the user discusses. The
user is the speaker — do not extract them.

Definitions:
- **Person**: a named individual the user mentions ("Sarah Chen",
  "Mike from engineering"). Capture organizational affiliation and
  role if the conversation states them.
- **Organization**: a named company, institution, or team. Capture
  the relationship to the user (client, employer, vendor, partner,
  internal team) if the conversation implies one.
- **Initiative**: a concrete project, strategic priority, or piece
  of ongoing work the user is organizing effort around ("API
  migration", "Q3 enterprise push", "reduce churn to 5%"). An
  initiative implies *active effort toward a future state* —
  distinguish from topics the user is merely thinking about. The
  rule of thumb: if the user could say "we're working on X" or "I
  committed to X", it's an initiative; if they could only say "I
  think about X", it's not.

  Tactics, milestones, sub-strategies, implementation paths, or
  work artifacts *within* an initiative are NOT separate initiatives
  — they belong to the parent. Example: "API migration" is an
  initiative; the "parallel migration path" the user agreed to ship
  is a tactic inside it, not a separate initiative.

  Single-conversation work products and deliverables are NOT
  initiatives — they're artifacts inside an initiative or client
  engagement. Things that are NOT initiatives, even when the user
  is actively working on them: "the migration plan revision",
  "the SOC2 crosswalk", "the SOW reformat", "the discovery scope",
  "usage-based pricing alternative", "a SOW", "a draft", "a
  scoping doc". An initiative has a stable name the user uses
  *across multiple conversations*; if the phrase only appears in
  one conversation and reads like a task title, it's an artifact,
  not an initiative.

  Use the canonical name without possessive prefixes or scope
  suffixes — write "Q3 enterprise push", not "Meridian's Q3
  enterprise push" or "Acme's API migration"; write "Architecture
  refresh", not "Architecture refresh discovery" or "Architecture
  refresh kickoff". Capture organizational ownership through
  `participants` (the org's atom appears there), not in the name;
  capture phase or status through the `status` field.

When the same person, organization, or initiative is referenced both
by short form ("Mike", "Acme") and long form ("Mike Torres",
"Acme Corp") across conversations, emit the long form once — the
post-extraction merger will resolve short-form references to it.

Use the [Conversation N] labels to record where each entity appeared
in `mentions`. If you list a person as a participant on an initiative,
make sure that person also appears in the `persons` array.

Conversations:
{passages}

Return ONLY a JSON object:
{{
  "persons": [
    {{
      "name": "Sarah Chen",
      "affiliation": "Acme Corp",
      "role": "VP Engineering",
      "mentions": ["Conversation 1"]
    }}
  ],
  "organizations": [
    {{
      "name": "Acme Corp",
      "relationship": "client",
      "mentions": ["Conversation 1"]
    }}
  ],
  "initiatives": [
    {{
      "name": "API migration",
      "status": "phase 2 of 4, on track for Q2",
      "participants": ["Mike Torres"],
      "mentions": ["Conversation 2"]
    }}
  ]
}}

Empty arrays for any kind that didn't appear. Omit affiliation, role,
status, or relationship fields when the conversation doesn't support
them — do not invent. Skip first-person pronouns and the AI assistant."#,
            passages = passages
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversational_domain_identity() {
        let d = ConversationalDomain;
        assert_eq!(d.id(), "conversational");
        assert_eq!(d.name(), "Conversational knowledge");
    }

    #[test]
    fn conversational_domain_is_object_safe() {
        let domain: std::sync::Arc<dyn Domain> = std::sync::Arc::new(ConversationalDomain);
        assert_eq!(domain.id(), "conversational");
    }

    #[test]
    fn clustering_config_min_is_two() {
        assert_eq!(
            ConversationalDomain.clustering_config().min_cluster_size,
            CLUSTERING_MIN_CLUSTER_SIZE
        );
    }

    #[test]
    fn overview_filter_has_length_threshold() {
        let f = ConversationalDomain.overview_filter();
        assert_eq!(f.min_token_count, Some(OVERVIEW_MIN_TOKEN_COUNT));
    }

    #[test]
    fn skeleton_extraction_prompt_mentions_conversations() {
        let chunk = Chunk {
            id: 1,
            content: "User asked about oil price modeling.".into(),
            title: Some("Oil prices Q1".into()),
        };
        let prompt = ConversationalDomain.skeleton_extraction_prompt(&[&chunk]);
        assert!(prompt.contains("conversations between one person and an AI"));
        assert!(prompt.contains("[Conversation 1 — Oil prices Q1]"));
    }

    #[test]
    fn skeleton_prompt_handles_untitled() {
        let chunk = Chunk {
            id: 1,
            content: "A brief exchange.".into(),
            title: None,
        };
        let prompt = ConversationalDomain.skeleton_extraction_prompt(&[&chunk]);
        assert!(prompt.contains("[Conversation 1 — (untitled)]"));
    }

    #[test]
    fn cluster_labeling_prompt_requests_practice_framing() {
        let chunk = Chunk {
            id: 1,
            content: "Discussion of architecture".into(),
            title: Some("Arch chat".into()),
        };
        let prompt = ConversationalDomain.cluster_labeling_prompt(&[&chunk]);
        assert!(prompt.contains("as a practice"));
        assert!(prompt.contains("Two to four words"));
    }

    #[test]
    fn fault_line_prompt_names_threads() {
        let a = Chunk {
            id: 1,
            content: "first thread content".into(),
            title: None,
        };
        let b = Chunk {
            id: 2,
            content: "second thread content".into(),
            title: None,
        };
        let prompt = ConversationalDomain.fault_line_detection_prompt(
            &[&a],
            &[&b],
            "scope-of-framework",
            "scope-of-implementation",
        );
        assert!(prompt.contains("Thread A: scope-of-framework"));
        assert!(prompt.contains("Thread B: scope-of-implementation"));
        assert!(prompt.contains("resolution_condition"));
    }

    #[test]
    fn skeleton_storage_is_json_and_lance() {
        assert!(matches!(
            ConversationalDomain.skeleton_storage(),
            SkeletonStorage::JsonAndLance
        ));
    }
}
