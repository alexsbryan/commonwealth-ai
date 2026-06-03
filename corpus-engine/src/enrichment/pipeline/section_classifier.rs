//! Phase 0 — section-type classifier.
//!
//! Reads one section's title + opening body + frontmatter signals and
//! emits a `SectionClassification` carrying the genre tag the routed
//! Phase 1 dispatches on. Meta-shape, not content extraction —
//! answers "what *kind* of writing is this?" so a downstream Phase 1
//! prompt + schema can fit the genre.
//!
//! Empirical anchor (from the obsidian-vault bench loop): forcing all
//! sections through the literary `entities_introduced / claims /
//! questions_raised` schema produced total extraction failure on
//! mechanism-design essays (`Pharmacy Benefit`, `FIFA Financialized`
//! both emitted 0 entities, 0 claims), Claim-cap saturation at 10 on
//! a dozen argumentative essays, and `Work` atom semantic drift
//! across genres (project-note model artifacts vs essay-cited papers
//! vs story-cited-zero). Phase 0 is the structural fix.
//!
//! Cost: one fast-slot or primary-slot chat call per section.
//! Idempotent on `(section_id, content_hash)` so re-classifying an
//! unchanged section reads the cached entry.

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::types::{
    AudienceRelation, ChapterInput, ChatCompletionFn, ChatPrompt, DiscourseMode,
    DiscourseModeDistribution, EpistemicPosture, SectionClassification,
    SectionClassificationVector, SectionType, TemporalFrame,
};
use crate::error::{Error, Result};

/// Asset path is `include_str!`'d so prompt revisions land in version
/// control as data, not Rust string literals — same convention as the
/// per-pipeline `*_atlas_prompts/phase1_system.md`.
const PHASE0_CLASSIFIER_SYSTEM: &str = include_str!("section_classifier_prompt.md");

/// v2 — MECE axis-vector classifier prompt. Replaces the flat
/// `primary_type` enum with a (discourse_mode, epistemic_posture,
/// temporal_frame, audience_relation) tuple. The legacy prompt above
/// stays available via `compose_classification_prompt` for any caller
/// not yet migrated.
const PHASE0_AXES_SYSTEM: &str = include_str!("section_classifier_axes_prompt.md");

/// JSON Schema the model output must satisfy. Embedded into the
/// `ChatPrompt` via `with_response_schema` so the daemon's
/// grammar-constrained decode path locks the shape.
pub fn phase0_classification_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["primary_type", "confidence", "reasoning"],
        "properties": {
            "primary_type": {
                "type": "string",
                "enum": [
                    "fiction",
                    "argumentative_essay",
                    "criticism",
                    "journal",
                    "meeting_record",
                    "reference",
                    "project_note",
                    "poetry",
                    "mixed"
                ]
            },
            "confidence": {
                "type": "number",
                "minimum": 0.0,
                "maximum": 1.0
            },
            "secondary_type": {
                "type": ["string", "null"],
                "enum": [
                    "fiction",
                    "argumentative_essay",
                    "criticism",
                    "journal",
                    "meeting_record",
                    "reference",
                    "project_note",
                    "poetry",
                    "mixed",
                    null
                ]
            },
            "reasoning": {
                "type": "string",
                "minLength": 1,
                "maxLength": 400
            }
        }
    })
}

/// Builds the Phase 0 chat prompt for one section.
///
/// The user message carries:
///   - title (always)
///   - frontmatter tags (when present in `chapter.metadata`)
///   - the section's opening — capped at ~3000 chars so the prompt
///     fits a fast-slot budget. Classification is a meta-shape call;
///     more text than that hurts more than it helps (the model starts
///     paying attention to content rather than genre signals).
pub fn compose_classification_prompt(chapter: &ChapterInput) -> ChatPrompt {
    let mut user = String::new();
    user.push_str("# Section to classify\n\n");
    user.push_str(&format!("**Title:** {}\n", chapter.title));
    let tag_field = chapter.metadata.get("tags");
    if let Some(tags) = tag_field {
        if !tags.is_empty() {
            user.push_str(&format!("**Frontmatter tags:** {tags}\n"));
        }
    }
    if let Some(ord) = chapter.metadata.get("ordinal") {
        user.push_str(&format!("**Position:** chapter {ord}\n"));
    }
    user.push_str("\n**Opening:**\n\n");
    user.push_str(&truncate_to_chars(&chapter.text, OPENING_BUDGET_CHARS));
    user.push_str("\n\n---\n\n");
    user.push_str(
        "Return a single JSON object per the schema in the system message. \
         No prose, no <think> block, no code-fence markers.",
    );
    ChatPrompt::new(PHASE0_CLASSIFIER_SYSTEM, user)
        .with_response_schema("section_classification", phase0_classification_schema())
        .with_phase_id("phase0_classify")
        .with_max_output_tokens(256)
}

/// Cap on the section excerpt the classifier sees. Classification is
/// genre detection, not extraction — feeding the full chapter dilutes
/// the opening's genre signal with body content the model would
/// otherwise extract from. 3000 chars ≈ first 500 words, which is
/// where the genre is structurally established (essay thesis,
/// fiction opening, journal date stamp, meeting attendees, etc).
const OPENING_BUDGET_CHARS: usize = 3000;

fn truncate_to_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out = String::with_capacity(max_chars + 16);
    for (i, ch) in s.chars().enumerate() {
        if i >= max_chars {
            break;
        }
        out.push(ch);
    }
    out.push_str("\n\n[…truncated for classification…]");
    out
}

/// Compute the short content hash used for cache invalidation. Mirror
/// of `local_corpus::watched::walker::hash_file`'s 16-char prefix
/// shape so the same hash discipline applies across the codebase.
pub fn content_hash(section_text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(section_text.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[derive(Debug, Deserialize)]
struct RawClassification {
    primary_type: String,
    confidence: f32,
    #[serde(default)]
    secondary_type: Option<String>,
    #[serde(default)]
    reasoning: String,
}

fn parse_section_type(s: &str) -> Result<SectionType> {
    match s.trim().to_ascii_lowercase().as_str() {
        "fiction" => Ok(SectionType::Fiction),
        "argumentative_essay" | "argumentative essay" | "essay" => {
            Ok(SectionType::ArgumentativeEssay)
        }
        "criticism" => Ok(SectionType::Criticism),
        "journal" => Ok(SectionType::Journal),
        "meeting_record" | "meeting record" | "meeting" => Ok(SectionType::MeetingRecord),
        "reference" => Ok(SectionType::Reference),
        "project_note" | "project note" | "project" => Ok(SectionType::ProjectNote),
        "poetry" => Ok(SectionType::Poetry),
        "mixed" => Ok(SectionType::Mixed),
        other => Err(Error::Serialization(format!(
            "phase 0 classifier returned unknown section_type {other:?}; \
             expected one of fiction|argumentative_essay|criticism|journal|\
             meeting_record|reference|project_note|poetry|mixed"
        ))),
    }
}

/// Parse the model's JSON response. Public so unit tests can pin the
/// contract without spinning up a chat closure.
pub fn parse_classification_response(
    response: &str,
    section_id: &str,
    content_hash: &str,
    classified_at_unix: u64,
) -> Result<SectionClassification> {
    let stripped = super::types::strip_reasoning_tags(response);
    let cleaned: String = super::types::extract_json_block(&stripped)
        .map(|s| s.to_string())
        .unwrap_or_else(|| stripped.clone());
    let raw: RawClassification = serde_json::from_str(&cleaned).map_err(|e| {
        Error::Serialization(format!(
            "phase 0 classifier response is not valid JSON: {e}; \
             body head: {}",
            cleaned.chars().take(200).collect::<String>()
        ))
    })?;
    let primary = parse_section_type(&raw.primary_type)?;
    let secondary = match raw.secondary_type {
        Some(ref s) if !s.is_empty() && s != "null" => Some(parse_section_type(s)?),
        _ => None,
    };
    let confidence = raw.confidence.clamp(0.0, 1.0);
    let reasoning = raw.reasoning.trim().to_string();
    if reasoning.is_empty() {
        return Err(Error::Serialization(
            "phase 0 classifier returned empty reasoning — the schema \
             requires a non-empty rationale so operators can audit \
             classification choices"
                .into(),
        ));
    }
    Ok(SectionClassification {
        section_id: section_id.to_string(),
        primary_type: primary,
        confidence,
        secondary_type: secondary,
        reasoning,
        content_hash: content_hash.to_string(),
        classified_at_unix,
    })
}

/// Classify one section. Caller decides whether to consult a cache
/// before invoking this — the function itself is pure ingest →
/// dispatch → parse → return.
pub async fn classify_section(
    chapter: &ChapterInput,
    chat: ChatCompletionFn,
) -> Result<SectionClassification> {
    let prompt = compose_classification_prompt(chapter);
    let response = chat(&prompt).await?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let hash = content_hash(&chapter.text);
    parse_classification_response(&response, &chapter.chapter_id, &hash, now)
}

/// Cache helper — true when the cached entry is still valid for the
/// supplied chapter (same content_hash). The runner uses this before
/// dispatching a chat call so an unchanged section doesn't burn an
/// LLM call on every build.
pub fn cache_hit(cached: &SectionClassification, chapter: &ChapterInput) -> bool {
    cached.content_hash == content_hash(&chapter.text)
}

// ─── v2: MECE axis-vector classifier ──────────────────────────────
//
// The functions below are the v2 surface: same per-section chat
// dispatch, but the prompt asks for a four-axis vector instead of a
// flat `primary_type`. Output parses into `SectionClassificationVector`.
// The v1 functions above remain for any caller that hasn't migrated.

/// JSON Schema for the v2 axis vector. Locks the four axes + weighted
/// distribution shape via grammar-constrained decode. The
/// `secondaries` array is allowed to be empty (single-mode sections)
/// and capped at 2 entries.
pub fn phase0_axes_schema() -> serde_json::Value {
    let mode_enum = serde_json::json!([
        "narrative",
        "argumentative",
        "descriptive",
        "reflective",
        "procedural",
        "lyric"
    ]);
    serde_json::json!({
        "type": "object",
        "required": [
            "discourse_mode",
            "epistemic_posture",
            "temporal_frame",
            "reasoning"
        ],
        "properties": {
            "discourse_mode": {
                "type": "object",
                "required": ["primary", "primary_weight"],
                "properties": {
                    "primary": { "type": "string", "enum": mode_enum.clone() },
                    "primary_weight": {
                        "type": "number",
                        "minimum": 0.0,
                        "maximum": 1.0
                    },
                    "secondaries": {
                        "type": "array",
                        "maxItems": 2,
                        "items": {
                            "type": "array",
                            "minItems": 2,
                            "maxItems": 2,
                            "items": [
                                { "type": "string", "enum": mode_enum },
                                {
                                    "type": "number",
                                    "minimum": 0.0,
                                    "maximum": 1.0
                                }
                            ]
                        }
                    }
                }
            },
            "epistemic_posture": {
                "type": "string",
                "enum": [
                    "factual",
                    "normative",
                    "fictional",
                    "hypothetical"
                ]
            },
            "temporal_frame": {
                "type": "string",
                "enum": ["episodic", "atemporal", "prospective"]
            },
            "audience_relation": {
                "type": ["string", "null"],
                "enum": [
                    "private_first_person",
                    "specific_recipient",
                    "public_impersonal",
                    null
                ]
            },
            "reasoning": {
                "type": "string",
                "minLength": 1,
                "maxLength": 400
            }
        }
    })
}

/// Build the v2 chat prompt. Same body shaping as the v1 composer
/// (title + frontmatter tags + truncated opening); only the system
/// preamble + response schema differ.
pub fn compose_axes_classification_prompt(chapter: &ChapterInput) -> ChatPrompt {
    let mut user = String::new();
    user.push_str("# Section to classify\n\n");
    user.push_str(&format!("**Title:** {}\n", chapter.title));
    if let Some(tags) = chapter.metadata.get("tags") {
        if !tags.is_empty() {
            user.push_str(&format!("**Frontmatter tags:** {tags}\n"));
        }
    }
    if let Some(ord) = chapter.metadata.get("ordinal") {
        user.push_str(&format!("**Position:** chapter {ord}\n"));
    }
    user.push_str("\n**Opening:**\n\n");
    user.push_str(&truncate_to_chars(&chapter.text, OPENING_BUDGET_CHARS));
    user.push_str("\n\n---\n\n");
    user.push_str(
        "Return one JSON object per the schema in the system message — a \
         four-axis classification vector. No prose, no <think> block, no \
         code-fence markers.",
    );
    ChatPrompt::new(PHASE0_AXES_SYSTEM, user)
        .with_response_schema("section_classification_vector", phase0_axes_schema())
        .with_phase_id("phase0_classify_axes")
        .with_max_output_tokens(384)
}

#[derive(Debug, Deserialize)]
struct RawAxesClassification {
    discourse_mode: RawDiscourseModeDistribution,
    epistemic_posture: String,
    temporal_frame: String,
    #[serde(default)]
    audience_relation: Option<String>,
    #[serde(default)]
    reasoning: String,
}

#[derive(Debug, Deserialize)]
struct RawDiscourseModeDistribution {
    primary: String,
    primary_weight: f32,
    #[serde(default)]
    secondaries: Vec<(String, f32)>,
}

fn parse_discourse_mode(s: &str) -> Result<DiscourseMode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "narrative" => Ok(DiscourseMode::Narrative),
        "argumentative" => Ok(DiscourseMode::Argumentative),
        "descriptive" => Ok(DiscourseMode::Descriptive),
        "reflective" => Ok(DiscourseMode::Reflective),
        "procedural" => Ok(DiscourseMode::Procedural),
        "lyric" => Ok(DiscourseMode::Lyric),
        other => Err(Error::Serialization(format!(
            "phase 0 axes classifier returned unknown discourse_mode {other:?}; \
             expected one of narrative|argumentative|descriptive|reflective|\
             procedural|lyric"
        ))),
    }
}

fn parse_epistemic_posture(s: &str) -> Result<EpistemicPosture> {
    match s.trim().to_ascii_lowercase().as_str() {
        "factual" => Ok(EpistemicPosture::Factual),
        "normative" => Ok(EpistemicPosture::Normative),
        "fictional" => Ok(EpistemicPosture::Fictional),
        "hypothetical" => Ok(EpistemicPosture::Hypothetical),
        other => Err(Error::Serialization(format!(
            "phase 0 axes classifier returned unknown epistemic_posture {other:?}; \
             expected one of factual|normative|fictional|hypothetical"
        ))),
    }
}

fn parse_temporal_frame(s: &str) -> Result<TemporalFrame> {
    match s.trim().to_ascii_lowercase().as_str() {
        "episodic" => Ok(TemporalFrame::Episodic),
        "atemporal" => Ok(TemporalFrame::Atemporal),
        "prospective" => Ok(TemporalFrame::Prospective),
        other => Err(Error::Serialization(format!(
            "phase 0 axes classifier returned unknown temporal_frame {other:?}; \
             expected one of episodic|atemporal|prospective"
        ))),
    }
}

fn parse_audience_relation(s: &str) -> Result<AudienceRelation> {
    match s.trim().to_ascii_lowercase().as_str() {
        "private_first_person" | "private first person" => Ok(AudienceRelation::PrivateFirstPerson),
        "specific_recipient" | "specific recipient" => Ok(AudienceRelation::SpecificRecipient),
        "public_impersonal" | "public impersonal" => Ok(AudienceRelation::PublicImpersonal),
        other => Err(Error::Serialization(format!(
            "phase 0 axes classifier returned unknown audience_relation {other:?}; \
             expected one of private_first_person|specific_recipient|public_impersonal"
        ))),
    }
}

/// Parse the v2 axes response into a `SectionClassificationVector`.
/// Validates the discourse-mode weight distribution sums to 1.0 (±0.01)
/// — model output outside that range is rejected as malformed.
///
/// Public so unit tests can pin the contract without spinning up a
/// chat closure.
pub fn parse_axes_classification_response(
    response: &str,
    section_id: &str,
    content_hash: &str,
    classified_at_unix: u64,
) -> Result<SectionClassificationVector> {
    let stripped = super::types::strip_reasoning_tags(response);
    let cleaned: String = super::types::extract_json_block(&stripped)
        .map(|s| s.to_string())
        .unwrap_or_else(|| stripped.clone());
    let raw: RawAxesClassification = serde_json::from_str(&cleaned).map_err(|e| {
        Error::Serialization(format!(
            "phase 0 axes classifier response is not valid JSON: {e}; \
             body head: {}",
            cleaned.chars().take(200).collect::<String>()
        ))
    })?;

    let primary = parse_discourse_mode(&raw.discourse_mode.primary)?;
    let primary_weight = raw.discourse_mode.primary_weight.clamp(0.0, 1.0);

    let mut secondaries: Vec<(DiscourseMode, f32)> = Vec::new();
    for (mode_str, weight) in raw.discourse_mode.secondaries.iter() {
        let mode = parse_discourse_mode(mode_str)?;
        // The dispatcher fans out by mode — duplicate entries would
        // double-route. Reject before downstream sees ambiguity.
        if mode == primary || secondaries.iter().any(|(m, _)| *m == mode) {
            return Err(Error::Serialization(format!(
                "phase 0 axes classifier returned duplicate discourse mode \
                 {mode:?} — primary and secondaries must be distinct"
            )));
        }
        let w = weight.clamp(0.0, 1.0);
        if w >= primary_weight {
            return Err(Error::Serialization(format!(
                "phase 0 axes classifier returned secondary mode {mode:?} \
                 with weight {w:.3} ≥ primary_weight {primary_weight:.3}; \
                 secondaries must be strictly smaller than the primary"
            )));
        }
        secondaries.push((mode, w));
    }
    secondaries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let dist = DiscourseModeDistribution {
        primary,
        primary_weight,
        secondaries,
    };
    if !dist.weights_sum_to_one() {
        return Err(Error::Serialization(format!(
            "phase 0 axes classifier returned discourse_mode weights summing to \
             {:.3}; expected 1.0 ± 0.01",
            dist.weight_sum()
        )));
    }

    let posture = parse_epistemic_posture(&raw.epistemic_posture)?;
    let frame = parse_temporal_frame(&raw.temporal_frame)?;
    let audience = match raw.audience_relation {
        Some(ref s) if !s.is_empty() && s != "null" => Some(parse_audience_relation(s)?),
        _ => None,
    };
    let reasoning = raw.reasoning.trim().to_string();
    if reasoning.is_empty() {
        return Err(Error::Serialization(
            "phase 0 axes classifier returned empty reasoning — the schema \
             requires a non-empty rationale so operators can audit \
             classification choices"
                .into(),
        ));
    }

    Ok(SectionClassificationVector {
        section_id: section_id.to_string(),
        discourse_mode: dist,
        epistemic_posture: posture,
        temporal_frame: frame,
        audience_relation: audience,
        content_hash: content_hash.to_string(),
        classified_at_unix,
        reasoning,
    })
}

/// v2 classifier — emits the MECE axis vector directly. Same dispatch
/// shape as `classify_section`: pure ingest → chat → parse.
pub async fn classify_section_axes(
    chapter: &ChapterInput,
    chat: ChatCompletionFn,
) -> Result<SectionClassificationVector> {
    let prompt = compose_axes_classification_prompt(chapter);
    let response = chat(&prompt).await?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let hash = content_hash(&chapter.text);
    parse_axes_classification_response(&response, &chapter.chapter_id, &hash, now)
}

/// v2 cache helper — same role as `cache_hit` but for the vector.
pub fn cache_hit_axes(cached: &SectionClassificationVector, chapter: &ChapterInput) -> bool {
    cached.content_hash == content_hash(&chapter.text)
}

// ─── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fake_chapter() -> ChapterInput {
        ChapterInput {
            chapter_id: "sec_0001".into(),
            title: "Ostrom Summary".into(),
            text: "Your textbook tells you a story...".into(),
            metadata: HashMap::new(),
            approx_tokens: 100,
        }
    }

    #[test]
    fn schema_lists_all_classifier_outputs() {
        let schema = phase0_classification_schema();
        let enums = schema["properties"]["primary_type"]["enum"]
            .as_array()
            .expect("primary_type enum");
        let names: Vec<&str> = enums.iter().filter_map(|v| v.as_str()).collect();
        for t in SectionType::CLASSIFIER_OUTPUTS {
            assert!(
                names.contains(&t.tag()),
                "schema missing {} in primary_type enum",
                t.tag()
            );
        }
    }

    #[test]
    fn prompt_carries_title_and_opening() {
        let ch = fake_chapter();
        let prompt = compose_classification_prompt(&ch);
        assert!(prompt.user.contains("Ostrom Summary"));
        assert!(prompt.user.contains("Your textbook tells you a story"));
    }

    #[test]
    fn opening_is_truncated_past_budget() {
        let mut ch = fake_chapter();
        ch.text = "A".repeat(OPENING_BUDGET_CHARS * 2);
        let prompt = compose_classification_prompt(&ch);
        assert!(prompt.user.contains("[…truncated for classification…]"));
        // The full body should NOT appear in the prompt.
        assert!(!prompt
            .user
            .contains(&"A".repeat(OPENING_BUDGET_CHARS + 100)));
    }

    #[test]
    fn parse_essay_high_confidence() {
        let resp = r#"{
            "primary_type": "argumentative_essay",
            "confidence": 0.92,
            "secondary_type": null,
            "reasoning": "Long-form policy argument with named mechanisms and dollar figures."
        }"#;
        let c = parse_classification_response(resp, "sec_0001", "deadbeef", 1700000000)
            .expect("parses");
        assert_eq!(c.primary_type, SectionType::ArgumentativeEssay);
        assert!((c.confidence - 0.92).abs() < 1e-3);
        assert!(c.secondary_type.is_none());
        assert!(c.reasoning.contains("policy"));
        assert_eq!(c.section_id, "sec_0001");
        assert_eq!(c.content_hash, "deadbeef");
    }

    #[test]
    fn parse_mixed_with_secondary() {
        let resp = r#"{
            "primary_type": "journal",
            "confidence": 0.55,
            "secondary_type": "argumentative_essay",
            "reasoning": "Opens as a daily entry but spends most of its length on a sustained argument about housing policy."
        }"#;
        let c = parse_classification_response(resp, "sec_0002", "cafef00d", 1700000000)
            .expect("parses");
        assert_eq!(c.primary_type, SectionType::Journal);
        assert_eq!(c.secondary_type, Some(SectionType::ArgumentativeEssay));
        assert!((c.confidence - 0.55).abs() < 1e-3);
    }

    #[test]
    fn parse_clamps_out_of_range_confidence() {
        let resp = r#"{
            "primary_type": "fiction",
            "confidence": 1.4,
            "reasoning": "Short story with named characters."
        }"#;
        let c = parse_classification_response(resp, "sec_0003", "cafe", 0).expect("parses");
        assert_eq!(c.confidence, 1.0, "out-of-range confidence must clamp");
    }

    #[test]
    fn parse_rejects_unknown_type() {
        let resp = r#"{
            "primary_type": "manifesto",
            "confidence": 0.7,
            "reasoning": "..."
        }"#;
        let err = parse_classification_response(resp, "sec_0004", "h", 0)
            .expect_err("unknown type must error");
        let msg = format!("{err}");
        assert!(msg.contains("unknown section_type"), "msg: {msg}");
    }

    #[test]
    fn parse_rejects_empty_reasoning() {
        let resp = r#"{
            "primary_type": "fiction",
            "confidence": 0.9,
            "reasoning": "   "
        }"#;
        let err = parse_classification_response(resp, "sec_0005", "h", 0)
            .expect_err("empty reasoning must error");
        let msg = format!("{err}");
        assert!(msg.contains("empty reasoning"), "msg: {msg}");
    }

    #[test]
    fn parse_strips_reasoning_tags_before_json() {
        let resp = "<think>weighing genre cues</think>\n{\"primary_type\":\"poetry\",\"confidence\":0.8,\"reasoning\":\"Compressed line-broken imagery without narrative.\"}";
        let c = parse_classification_response(resp, "sec_0006", "h", 0).expect("parses");
        assert_eq!(c.primary_type, SectionType::Poetry);
    }

    #[test]
    fn content_hash_is_deterministic_and_short() {
        let h1 = content_hash("hello world");
        let h2 = content_hash("hello world");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
        let h3 = content_hash("hello world!");
        assert_ne!(h1, h3);
    }

    #[test]
    fn cache_hit_invalidates_on_content_change() {
        let mut ch = fake_chapter();
        let cached = SectionClassification {
            section_id: "sec_0001".into(),
            primary_type: SectionType::ArgumentativeEssay,
            confidence: 0.9,
            secondary_type: None,
            reasoning: "x".into(),
            content_hash: content_hash(&ch.text),
            classified_at_unix: 0,
        };
        assert!(cache_hit(&cached, &ch));
        ch.text.push_str(" — appended later");
        assert!(!cache_hit(&cached, &ch));
    }

    #[test]
    fn section_type_tags_round_trip_through_serde() {
        for t in SectionType::CLASSIFIER_OUTPUTS {
            let s = serde_json::to_string(t).expect("ser");
            let back: SectionType = serde_json::from_str(&s).expect("de");
            assert_eq!(*t, back, "round-trip failed for {}", t.tag());
        }
    }

    #[test]
    fn unknown_serde_variant_falls_back_to_unknown() {
        // Forward-compat: a cache produced by a future variant we
        // don't know about should deserialize as `Unknown`, not
        // error.
        let json = r#""this_is_a_future_variant""#;
        let back: SectionType = serde_json::from_str(json).expect("falls back");
        assert_eq!(back, SectionType::Unknown);
    }

    // ─── v2 axis-vector classifier tests ─────────────────────────

    #[test]
    fn axes_schema_lists_every_discourse_mode() {
        let schema = phase0_axes_schema();
        let enums = schema["properties"]["discourse_mode"]["properties"]["primary"]["enum"]
            .as_array()
            .expect("primary enum");
        let names: Vec<&str> = enums.iter().filter_map(|v| v.as_str()).collect();
        for m in DiscourseMode::ALL {
            assert!(
                names.contains(&m.tag()),
                "axes schema missing discourse mode {}",
                m.tag()
            );
        }
    }

    #[test]
    fn parse_axes_single_mode_at_1_0() {
        let resp = r#"{
            "discourse_mode": {
                "primary": "descriptive",
                "primary_weight": 1.0,
                "secondaries": []
            },
            "epistemic_posture": "factual",
            "temporal_frame": "atemporal",
            "audience_relation": "public_impersonal",
            "reasoning": "Glossary card defining one concept with no narrative or argument."
        }"#;
        let v = parse_axes_classification_response(resp, "sec_1", "feedface", 1700000000)
            .expect("parses");
        assert_eq!(v.discourse_mode.primary, DiscourseMode::Descriptive);
        assert!((v.discourse_mode.primary_weight - 1.0).abs() < 1e-3);
        assert!(v.discourse_mode.secondaries.is_empty());
        assert_eq!(v.epistemic_posture, EpistemicPosture::Factual);
        assert_eq!(v.temporal_frame, TemporalFrame::Atemporal);
        assert_eq!(
            v.audience_relation,
            Some(AudienceRelation::PublicImpersonal)
        );
        assert_eq!(v.section_id, "sec_1");
    }

    #[test]
    fn parse_axes_hybrid_two_secondaries() {
        let resp = r#"{
            "discourse_mode": {
                "primary": "argumentative",
                "primary_weight": 0.55,
                "secondaries": [
                    ["narrative", 0.30],
                    ["descriptive", 0.15]
                ]
            },
            "epistemic_posture": "normative",
            "temporal_frame": "episodic",
            "reasoning": "Wheeler-family vignette opens; sustained argument about industrial seasonality follows."
        }"#;
        let v = parse_axes_classification_response(resp, "sec_2", "cafef00d", 0).expect("parses");
        assert_eq!(v.discourse_mode.primary, DiscourseMode::Argumentative);
        assert_eq!(v.discourse_mode.secondaries.len(), 2);
        // Sorted descending by weight.
        assert_eq!(v.discourse_mode.secondaries[0].0, DiscourseMode::Narrative);
        assert!((v.discourse_mode.secondaries[0].1 - 0.30).abs() < 1e-3);
        assert_eq!(
            v.discourse_mode.secondaries[1].0,
            DiscourseMode::Descriptive
        );
        assert!(v.discourse_mode.weights_sum_to_one());
        assert_eq!(v.audience_relation, None);
    }

    #[test]
    fn parse_axes_rejects_bad_weight_sum() {
        let resp = r#"{
            "discourse_mode": {
                "primary": "narrative",
                "primary_weight": 0.6,
                "secondaries": [["argumentative", 0.5]]
            },
            "epistemic_posture": "factual",
            "temporal_frame": "episodic",
            "reasoning": "Bad weights — sums to 1.1."
        }"#;
        let err =
            parse_axes_classification_response(resp, "sec_3", "x", 0).expect_err("must reject");
        let msg = format!("{err}");
        // Two valid failure modes: either the weights-sum check fires
        // first, or the secondary-not-strictly-smaller-than-primary
        // check does. Both indicate the parser is doing its job.
        assert!(
            msg.contains("weights summing") || msg.contains("must be strictly smaller"),
            "expected weight-sum or secondary-ordering rejection; got: {msg}"
        );
    }

    #[test]
    fn parse_axes_rejects_secondary_equal_or_above_primary() {
        let resp = r#"{
            "discourse_mode": {
                "primary": "narrative",
                "primary_weight": 0.5,
                "secondaries": [["argumentative", 0.5]]
            },
            "epistemic_posture": "factual",
            "temporal_frame": "episodic",
            "reasoning": "Tied — secondary cannot match primary by construction."
        }"#;
        let err =
            parse_axes_classification_response(resp, "sec_4", "x", 0).expect_err("must reject");
        assert!(format!("{err}").contains("must be strictly smaller"));
    }

    #[test]
    fn parse_axes_rejects_duplicate_modes() {
        let resp = r#"{
            "discourse_mode": {
                "primary": "narrative",
                "primary_weight": 0.7,
                "secondaries": [["narrative", 0.3]]
            },
            "epistemic_posture": "factual",
            "temporal_frame": "episodic",
            "reasoning": "Duplicate narrative — should reject."
        }"#;
        let err =
            parse_axes_classification_response(resp, "sec_5", "x", 0).expect_err("must reject");
        assert!(format!("{err}").contains("duplicate"));
    }

    #[test]
    fn parse_axes_rejects_empty_reasoning() {
        let resp = r#"{
            "discourse_mode": {
                "primary": "lyric",
                "primary_weight": 1.0,
                "secondaries": []
            },
            "epistemic_posture": "fictional",
            "temporal_frame": "atemporal",
            "reasoning": "   "
        }"#;
        let err =
            parse_axes_classification_response(resp, "sec_6", "x", 0).expect_err("must reject");
        assert!(format!("{err}").contains("empty reasoning"));
    }

    #[test]
    fn parse_axes_strips_reasoning_tags_before_json() {
        let resp = "<think>weighing axes</think>\n{\"discourse_mode\":{\"primary\":\"reflective\",\"primary_weight\":1.0,\"secondaries\":[]},\"epistemic_posture\":\"factual\",\"temporal_frame\":\"episodic\",\"reasoning\":\"First-person day-end processing.\"}";
        let v = parse_axes_classification_response(resp, "sec_7", "x", 0).expect("parses");
        assert_eq!(v.discourse_mode.primary, DiscourseMode::Reflective);
    }

    #[test]
    fn legacy_section_type_projects_from_vector() {
        let v = parse_axes_classification_response(
            r#"{
                "discourse_mode": {"primary":"argumentative","primary_weight":1.0,"secondaries":[]},
                "epistemic_posture": "normative",
                "temporal_frame": "atemporal",
                "reasoning": "x"
            }"#,
            "sec_8",
            "x",
            0,
        )
        .expect("parses");
        assert_eq!(v.legacy_section_type(), SectionType::ArgumentativeEssay);
    }

    #[test]
    fn vector_from_legacy_round_trips_through_known_pairs() {
        for st in SectionType::CLASSIFIER_OUTPUTS {
            let legacy = SectionClassification {
                section_id: "s".into(),
                primary_type: *st,
                confidence: 0.9,
                secondary_type: None,
                reasoning: "x".into(),
                content_hash: "h".into(),
                classified_at_unix: 0,
            };
            let v = SectionClassificationVector::from_legacy(&legacy);
            assert!(
                v.discourse_mode.weights_sum_to_one(),
                "from_legacy({}) produced weights summing to {}",
                st.tag(),
                v.discourse_mode.weight_sum()
            );
        }
    }

    #[test]
    fn active_modes_respects_threshold() {
        let dist = DiscourseModeDistribution {
            primary: DiscourseMode::Argumentative,
            primary_weight: 0.55,
            secondaries: vec![
                (DiscourseMode::Narrative, 0.30),
                (DiscourseMode::Descriptive, 0.15),
            ],
        };
        let active = dist.active_modes(0.25);
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].0, DiscourseMode::Argumentative);
        assert_eq!(active[1].0, DiscourseMode::Narrative);
        // Descriptive @ 0.15 falls below threshold.
    }
}
