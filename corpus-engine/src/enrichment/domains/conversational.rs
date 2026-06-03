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
            .join("\n\n");

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

        // Prompt layout is prefix-cache-optimised: every line of
        // stable instruction sits BEFORE the dynamic `{passages}` tail
        // so successive batches share the longest possible KV-cache
        // prefix. Pre-2026-05-28 the `Conversations:\n{passages}\n…`
        // block lived mid-prompt with stable formatting rules AFTER
        // it; the suffix collided in cache with prior batches'
        // passages-tail, hitting 0% prefix-cache hit ratio in
        // production (`prefix_cache: prefill scope` audit on enron-
        // sample-tiny Phase 1b, 50 calls, mean_hit=0 / mean_new=3444).
        // Moving every rule + the JSON example ahead of passages
        // shifts the diverging boundary to the tail so the ~1.5KB
        // instruction header re-uses cached tokens.
        Some(format!(
            r#"You are reading conversations between one person (the user) and an
AI assistant. Your job is named-entity extraction: identify the
*people*, *organizations*, *initiatives*, *works*, and *concepts*
the user discusses across whatever they choose to bring to the
conversation. The user is the speaker — do not extract them. The
assistant is a text-generation surface — do not extract it.

People talk to AI assistants about every part of life: work, yes,
but also family, friendships, neighbors, health, faith, grief,
hobbies, creative projects, cooking, gardening, parenting, pets,
travel, learning languages, fixing things around the house,
spiritual practice, politics, finances, the news, the weather,
what to cook tonight. Read the actual conversations without
presuming the domain. Extract what the user actually mentioned,
in whatever shape it appears.

Definitions:

- **Person**: a named human individual the user mentions. Could be
  a co-founder, a sister, a kindergarten teacher, a neighbor, a
  therapist, a referenced author, a historical figure, a friend's
  cousin, a child's classmate — anyone the user names. Capture
  whatever the conversation says about who they are: relationship
  to the user (spouse, advisor, doctor, friend), role
  (pediatrician, contractor, choir director, CEO), or affiliation
  (their employer, their school, their congregation). Omit fields
  the conversation doesn't support — don't invent.

- **Organization**: any named institution, company, group, or
  place that organizes a body of people. Workplaces, schools,
  hospitals, churches, mosques, community centers, government
  agencies, sports clubs, nonprofits, bands, restaurants, brands,
  museums, libraries, the local PTA, a book club, a mutual-aid
  group, a band the user follows. Capture the relationship to the
  user when the conversation implies one (employer, client,
  vendor, school the user attends or whose parent they are,
  congregation, regular hangout, the brand they buy).

- **Work**: a named piece of created content the user mentions,
  reads, watches, listens to, cites, or is making. Books, papers,
  blog posts, essays, articles, recipes, songs, albums, films,
  plays, novels, poems, podcasts, talks, sermons, comics, games,
  internal docs, RFCs, ADRs, design memos, sermons, scriptures.
  When the user mentions an author and the work together, emit
  both — the Work as a Work, the author as a Person if the
  conversation discusses them beyond just citing them.

- **Concept**: a named idea, mechanism, tradition, technique,
  condition, framework, or load-bearing term the user is
  *thinking with*. Concepts are the spine of cross-conversation
  linkage — they enable trend retrieval ("how has my view on X
  shifted"). Whole range of human life qualifies. A non-exhaustive
  span: `circadian rhythm`, `attachment style`, `the via
  negativa`, `mutual aid`, `sourdough starter`, `executive
  function`, `chord substitution`, `metta practice`, `dollar-cost
  averaging`, `the bus driver problem`, `tech debt`, `restorative
  justice`, `grief wave`. A Concept is *what the conversation
  thinks with*, not *what the conversation is about generally*.
  Distinguish sharply from Claim: "attachment style" is a Concept
  (the named framework); "her avoidant attachment is making
  reconciliation harder" is a Claim that uses the framework. Lift
  Concepts generously — when in doubt, include.

- **Initiative**: a concrete ongoing effort the user is organizing
  attention or action around. The rule is *active effort toward a
  future state*, NOT *professional project*. Examples across the
  range of what people bring to conversations:
    - work projects ("API migration", "Q3 enterprise push")
    - learning efforts ("learning Spanish", "rebuilding piano
      practice", "training for the half-marathon")
    - home / domestic ("kitchen renovation", "redoing the
      backyard", "potty training")
    - health & wellbeing ("recovering from ACL surgery",
      "managing my anxiety", "weight loss after the baby")
    - relational ("rebuilding trust with my dad", "couples
      counseling", "the move-in plan")
    - creative ("writing the novel", "the documentary on
      grandparents", "the wedding album")
    - civic / community ("the rezoning fight", "starting the
      neighborhood book exchange")
    - financial ("paying off the credit card", "the down-payment
      plan", "starting the 529s")
    - spiritual / inner ("the daily meditation practice",
      "studying Torah", "grief work after Mom")

  An initiative implies the user could naturally say "I'm working
  on X" or "we committed to X" or "I've been doing X for a while".
  If they could only say "I think about X" or "I wonder about X",
  it's not an initiative — it might surface as a question or a
  topic for clustering, but not here.

  Tactics, milestones, single-session deliverables, or sub-steps
  *within* an initiative are NOT separate initiatives — they
  belong to the parent. "Learning Spanish" is an initiative; "the
  Duolingo streak" is a tactic inside it. "Kitchen renovation" is
  an initiative; "the granite quote from Acme Stone" is an
  artifact inside it. An initiative carries a stable name the
  user uses *across multiple conversations*; if the phrase appears
  only once and reads like a task title, it's an artifact, not an
  initiative.

  Use the canonical name without possessive prefixes or scope
  suffixes. Capture ownership / participants in the `participants`
  field; capture phase or status through `status`.

When the same person, organization, or initiative is referenced
both by short form ("Mike", "Acme", "the choir") and long form
("Mike Torres", "Acme Corp", "Westside Community Choir") across
conversations, emit the long form once — the post-extraction
merger resolves short-form references to it.

Use the [Conversation N] labels to record where each entity
appeared in `mentions`. If you list a person as a participant on
an initiative, make sure that person also appears in the `persons`
array.

Return ONLY a JSON object. The example below is illustrative ONLY
— its entities are deliberately drawn from domains (12th-century
mysticism, baroque festival programming, antique instrument
restoration) chosen so they could not plausibly appear in real
chat content. **DO NOT echo any of the example names below in
your output.** They exist solely to show the JSON shape; every
entity in your output must come from the actual conversation text:

{{
  "persons": [
    {{
      "name": "Hildegard of Bingen",
      "affiliation": "Disibodenberg Abbey",
      "role": "prioress",
      "mentions": ["Conversation 1"]
    }}
  ],
  "organizations": [
    {{
      "name": "the Salzburg Festival",
      "relationship": "the user attends every August",
      "mentions": ["Conversation 1"]
    }}
  ],
  "works": [
    {{
      "name": "Scivias",
      "kind": "book",
      "creator": "Hildegard of Bingen",
      "mentions": ["Conversation 1"]
    }}
  ],
  "concepts": [
    {{
      "name": "apophatic theology",
      "description": "12th-century mystical approach of describing the divine by negation; the user is using it as a frame for reading Hildegard.",
      "mentions": ["Conversation 1"]
    }}
  ],
  "initiatives": [
    {{
      "name": "restoring a 1920s clavichord",
      "status": "cleaning the soundboard, week three",
      "participants": [],
      "mentions": ["Conversation 2"]
    }}
  ]
}}

Empty arrays for any kind that didn't appear. Omit affiliation,
role, status, creator, kind, description, or relationship fields
when the conversation doesn't support them — do not invent. Skip
first-person pronouns and the AI assistant. If you find yourself
about to emit `Hildegard of Bingen`, `Disibodenberg Abbey`, `the
Salzburg Festival`, `Scivias`, `apophatic theology`, or `restoring
a 1920s clavichord`, stop — those are example names, not corpus
content.

Conversations:
{passages}"#,
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
