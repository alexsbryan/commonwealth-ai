//! Phase 6 LLM Tension classifier — Landing 4 of the v2 atlas
//! pipeline.
//!
//! Consumes [`tensions::TensionCandidate`] entries (produced by the
//! deterministic enumerator in `tensions.rs`) and decides per-pair
//! whether the candidate is a genuine structural Tension or merely
//! co-occurs around a shared participant. Yes-decisions promote to
//! [`Edge`] records with `EdgeType::Tension` and
//! `EdgeProvenance::LlmPairwise`, ready to merge into
//! `atlas/edges.json`.
//!
//! Module scope:
//!
//! - **Pure logic only.** No filesystem, no inference, no async.
//!   The CLI driver in
//!   `sovereign-cli/enrich_cmd/atlas_tensions_classify.rs`
//!   threads the candidate list, the prompt composer, the inference
//!   client, and the response parser; this module supplies the data
//!   model + a small library of resolution helpers.
//! - **Pipeline-agnostic.** Both `LiteraryAtlasPipeline` and
//!   `PhilosophyAtlasPipeline` consume this module. The two
//!   pipelines author their own system-prompt file (literary /
//!   philosophical voice differs) and use their own
//!   `compose_phase6_atlas_classifier` /
//!   `parse_phase6_atlas_classifier` to thread it through. The
//!   classifier *response* shape is shared (the corpus's flavour
//!   doesn't change the schema).
//! - **Schema-stable.** [`Phase6Classification`] mirrors the JSON
//!   the model emits exactly; serde-derived for round-trip with the
//!   model and with cache files. The schema is grammar-constrained
//!   via [`phase6_classifier_response_schema`] but the daemon's
//!   sampler is a known no-op (see
//!   `sovereign-inference/embedded.rs:3140`), so the parser does
//!   the actual enforcement.

use serde::{Deserialize, Serialize};

use super::tensions::TensionCandidate;
use crate::enrichment::atlas::atoms::{
    AtomEnvelope, AtomId, AtomsFile, ChunkRef, Claim, Entity, State,
};
use crate::enrichment::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};

// ── Resolved candidate content ──────────────────────────────────────

/// Discriminator carried alongside an atom's text so the classifier
/// prompt knows whether each side is a Claim or a State.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtomKind {
    Claim,
    State,
}

/// Everything the prompt needs to classify one candidate. Built
/// from a [`TensionCandidate`] plus the atlas's atoms by
/// [`resolve_candidate_content`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateContent {
    pub candidate_id: String,
    pub source_atom: AtomId,
    pub source_kind: AtomKind,
    pub source_text: String,
    pub target_atom: AtomId,
    pub target_kind: AtomKind,
    pub target_text: String,
    /// Display name of the entity both atoms reference, when the
    /// candidate's `discovery == EntityOverlap`. `None` for
    /// intra-cluster pairs (no canonical shared entity).
    pub shared_entity_name: Option<String>,
    /// Optional shared-entity id for the eventual edge's
    /// `evidence` field.
    pub shared_entity_id: Option<AtomId>,
    /// Aggregated evidence chunks across both atoms, deduplicated.
    /// Used to populate the resulting Tension edge's `evidence` field
    /// so traversal callers can ground the tension to passages
    /// without re-reading the source atoms.
    pub evidence: Vec<ChunkRef>,
}

// ── Classifier response ─────────────────────────────────────────────

/// What the LLM returns for one candidate. Field semantics match the
/// system prompt at
/// `philosophy_atlas_prompts/phase6_classifier_system.md` /
/// `literary_atlas_prompts/phase6_classifier_system.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase6Classification {
    /// Hard verdict — `true` means promote to a Tension edge.
    pub is_tension: bool,
    /// One-sentence question the tension turns on. Required when
    /// `is_tension` is true; ignored otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_question: Option<String>,
    /// Confidence in `[0.0, 1.0]`. Default 0.7 if the model omits.
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    /// One short sentence explaining the call. Required for both
    /// verdicts so a reviewer can audit a `is_tension: false`
    /// without rerunning the model.
    pub rationale: String,
}

fn default_confidence() -> f32 {
    0.7
}

// ── Atom content resolution ─────────────────────────────────────────

/// Index over an `AtomsFile` so callers can fan out candidate
/// resolution without a linear scan per lookup. Cheap to build (one
/// pass over `atoms`) and cheap to drop.
pub struct AtomIndex<'a> {
    pub claims: std::collections::HashMap<AtomId, &'a Claim>,
    pub states: std::collections::HashMap<AtomId, &'a State>,
    pub entities: std::collections::HashMap<AtomId, &'a Entity>,
}

impl<'a> AtomIndex<'a> {
    pub fn build(atoms: &'a AtomsFile) -> Self {
        let mut claims = std::collections::HashMap::new();
        let mut states = std::collections::HashMap::new();
        let mut entities = std::collections::HashMap::new();
        for atom in &atoms.atoms {
            match atom {
                AtomEnvelope::Claim(c) => {
                    claims.insert(c.id.clone(), c);
                }
                AtomEnvelope::State(s) => {
                    states.insert(s.id.clone(), s);
                }
                AtomEnvelope::Entity(e) => {
                    entities.insert(e.id.clone(), e);
                }
                _ => {}
            }
        }
        Self {
            claims,
            states,
            entities,
        }
    }
}

/// Resolve a candidate to the full content the prompt needs. Returns
/// `None` when either endpoint can't be found in the index — e.g. a
/// stale candidate file pointing at atoms that have been re-resolved
/// since enumeration. Callers should log + skip rather than failing
/// the whole run.
pub fn resolve_candidate_content(
    candidate: &TensionCandidate,
    index: &AtomIndex<'_>,
) -> Option<CandidateContent> {
    let (source_kind, source_text, source_evidence) =
        atom_text_and_evidence(&candidate.source_atom, index)?;
    let (target_kind, target_text, target_evidence) =
        atom_text_and_evidence(&candidate.target_atom, index)?;

    let shared_entity_name = candidate
        .shared_entity
        .as_ref()
        .and_then(|id| index.entities.get(id))
        .map(|e| e.canonical_name.clone());

    // Dedup evidence by chunk_id — both endpoints often cite the
    // same passage. Preserve insertion order (source-first) so the
    // prompt sees the source's grounding before the target's.
    let mut evidence: Vec<ChunkRef> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for chunk in source_evidence.iter().chain(target_evidence.iter()) {
        if seen.insert(chunk.chunk_id.clone()) {
            evidence.push(chunk.clone());
        }
    }

    Some(CandidateContent {
        candidate_id: candidate.id.clone(),
        source_atom: candidate.source_atom.clone(),
        source_kind,
        source_text,
        target_atom: candidate.target_atom.clone(),
        target_kind,
        target_text,
        shared_entity_name,
        shared_entity_id: candidate.shared_entity.clone(),
        evidence,
    })
}

fn atom_text_and_evidence<'a>(
    id: &AtomId,
    index: &'a AtomIndex<'_>,
) -> Option<(AtomKind, String, &'a [ChunkRef])> {
    if let Some(c) = index.claims.get(id) {
        return Some((AtomKind::Claim, c.content.clone(), c.evidence.as_slice()));
    }
    if let Some(s) = index.states.get(id) {
        return Some((AtomKind::State, s.label.clone(), s.evidence.as_slice()));
    }
    None
}

// ── Promote classification → edge ───────────────────────────────────

/// Promote an LLM-yes classification into a [`Edge`] record. Returns
/// `None` when the model verdict was `is_tension: false`. The edge's
/// `id` is supplied by the caller (typically `EdgeId::new(N)` where
/// N continues the existing edges file's numbering).
///
/// Defensive wiring:
/// - confidence is clamped to `[0.0, 1.0]` (models occasionally emit
///   `1.05` / `-0.1` even with a constrained schema).
/// - sub_question is trimmed and emptied if blank — `None` is more
///   honest than an empty string for downstream consumers.
/// - evidence comes from the resolved [`CandidateContent`] not the
///   model response, so the grounding is verifiable.
pub fn classification_to_edge(
    candidate: &TensionCandidate,
    classification: &Phase6Classification,
    content: &CandidateContent,
    edge_id: EdgeId,
) -> Option<Edge> {
    if !classification.is_tension {
        return None;
    }
    let confidence = classification.confidence.clamp(0.0, 1.0);
    let sub_question = classification
        .sub_question
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    Some(Edge {
        id: edge_id,
        edge_type: EdgeType::Tension,
        source: candidate.source_atom.clone(),
        target: candidate.target_atom.clone(),
        evidence: content.evidence.clone(),
        trigger_event: None,
        sub_question,
        confidence,
        provenance: EdgeProvenance::LlmPairwise,
    })
}

// ── JSON schema for grammar-constrained generation ──────────────────

const PHASE6_CLASSIFIER_RESPONSE_SCHEMA: &str = r##"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "Phase6ClassifierResponse",
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "is_tension": { "type": "boolean" },
    "sub_question": { "type": ["string", "null"] },
    "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
    "rationale": { "type": "string", "minLength": 1 }
  },
  "required": ["is_tension", "rationale"]
}"##;

/// Return the Phase 6 classifier response schema as a parsed
/// `serde_json::Value` for `ChatPrompt::with_response_schema()`.
/// Called by both pipelines' compose methods.
pub fn phase6_classifier_response_schema() -> serde_json::Value {
    serde_json::from_str(PHASE6_CLASSIFIER_RESPONSE_SCHEMA)
        .expect("PHASE6_CLASSIFIER_RESPONSE_SCHEMA must be valid JSON")
}

// ── Cleanup helpers shared with the parser ──────────────────────────

/// Strip a leading `<think>...</think>` block from the model
/// response. Reuses the same defensive pattern as the Phase 1 parser
/// — Qwen / DeepSeek-style models like to emit reasoning even when
/// the prompt forbids it.
pub fn strip_think_block(s: &str) -> &str {
    let trimmed = s.trim_start();
    if let Some(rest) = trimmed.strip_prefix("<think>") {
        if let Some(end) = rest.find("</think>") {
            return rest[end + "</think>".len()..].trim_start();
        }
    }
    s
}

/// Parse the LLM response into a [`Phase6Classification`]. Strict on
/// shape — the prompt says "JSON only" — but tolerates a leading
/// `<think>` block, leading/trailing whitespace, and wrapped
/// markdown code fences (which models occasionally emit despite the
/// hard constraint). Returns the underlying `serde_json::Error` on
/// shape mismatch so the runner can record a `PhaseFailureKind`.
pub fn parse_phase6_classifier_response(
    raw: &str,
) -> Result<Phase6Classification, serde_json::Error> {
    let body = strip_think_block(raw);
    // Strip ```json … ``` fences if present.
    let body = body.trim();
    let stripped = body
        .strip_prefix("```json")
        .or_else(|| body.strip_prefix("```"))
        .unwrap_or(body);
    let stripped = stripped.strip_suffix("```").unwrap_or(stripped);
    serde_json::from_str(stripped.trim())
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{AtomsFile, SectionRange};
    use crate::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EntityType, EpistemicStatus, StateType,
    };

    fn mk_claim(id: usize, content: &str, evidence_chunk: &str) -> Claim {
        Claim {
            id: AtomId::claim(id),
            content: content.to_string(),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: vec![ChunkRef::new(evidence_chunk, None)],
            attributed_to: None,
            confidence: Some(0.9),
            anchor: None,
            enrichment_depth: EnrichmentDepth::Extracted,
            quotable_excerpt: None,
            claim_kind: None,
            concession_outcome: None,
            evidence_kind: None,
        }
    }

    fn mk_state(id: usize, entity: AtomId, label: &str, evidence_chunk: &str) -> State {
        State {
            id: AtomId::state(id),
            entity_id: entity,
            label: label.to_string(),
            state_type: StateType::Psychological,
            evidence: vec![ChunkRef::new(evidence_chunk, None)],
            section_range: SectionRange::point(evidence_chunk),
            confidence: Some(0.9),
            enrichment_depth: EnrichmentDepth::Extracted,
        }
    }

    fn mk_entity(id: usize, name: &str) -> Entity {
        Entity {
            id: AtomId::entity(id),
            canonical_name: name.to_string(),
            aliases: Vec::new(),
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: format!("Test entity {name}"),
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            defining_quote: None,
            provenance: Default::default(),
            concept_kind: None,
        }
    }

    #[test]
    fn resolve_candidate_content_pulls_claim_state_text_and_entity_name() {
        let claim = mk_claim(1, "Macbeth resolves to murder Duncan.", "sec_0001");
        let entity = mk_entity(1, "Macbeth");
        let state = mk_state(
            1,
            entity.id.clone(),
            "Macbeth, vacillating, frozen by conscience",
            "sec_0002",
        );
        let atoms = AtomsFile::new(vec![
            AtomEnvelope::Claim(claim.clone()),
            AtomEnvelope::State(state.clone()),
            AtomEnvelope::Entity(entity.clone()),
        ]);
        let index = AtomIndex::build(&atoms);
        let cand = TensionCandidate {
            id: "cand-0001".into(),
            source_atom: claim.id.clone(),
            target_atom: state.id.clone(),
            discovery: super::super::tensions::CandidateSource::EntityOverlap,
            cluster_id: None,
            shared_entity: Some(entity.id.clone()),
        };
        let content = resolve_candidate_content(&cand, &index).expect("resolves");
        assert_eq!(content.source_kind, AtomKind::Claim);
        assert_eq!(content.target_kind, AtomKind::State);
        assert_eq!(content.shared_entity_name.as_deref(), Some("Macbeth"));
        assert_eq!(content.evidence.len(), 2, "evidence dedupes across atoms");
    }

    #[test]
    fn resolve_candidate_returns_none_for_unknown_atom() {
        let atoms = AtomsFile::new(Vec::new());
        let index = AtomIndex::build(&atoms);
        let cand = TensionCandidate {
            id: "cand-0001".into(),
            source_atom: AtomId::claim(99),
            target_atom: AtomId::state(99),
            discovery: super::super::tensions::CandidateSource::EntityOverlap,
            cluster_id: None,
            shared_entity: None,
        };
        assert!(resolve_candidate_content(&cand, &index).is_none());
    }

    #[test]
    fn parse_phase6_classifier_response_accepts_yes_verdict() {
        let raw = r#"{
            "is_tension": true,
            "sub_question": "Is Macbeth's resolve a settled commitment?",
            "confidence": 0.85,
            "rationale": "Resolution and paralysis cannot both fully obtain."
        }"#;
        let r = parse_phase6_classifier_response(raw).unwrap();
        assert!(r.is_tension);
        assert_eq!(r.confidence, 0.85);
        assert!(r.sub_question.is_some());
    }

    #[test]
    fn parse_phase6_classifier_response_accepts_no_verdict_without_sub_question() {
        let raw = r#"{"is_tension": false, "rationale": "Atoms align rather than conflict."}"#;
        let r = parse_phase6_classifier_response(raw).unwrap();
        assert!(!r.is_tension);
        assert!(r.sub_question.is_none());
        assert_eq!(r.confidence, 0.7, "default confidence kicks in");
    }

    #[test]
    fn parse_phase6_classifier_response_strips_think_block() {
        let raw = "<think>thinking</think>\n\
            {\"is_tension\": false, \"rationale\": \"Aligned.\"}";
        let r = parse_phase6_classifier_response(raw).unwrap();
        assert!(!r.is_tension);
    }

    #[test]
    fn parse_phase6_classifier_response_strips_markdown_fence() {
        let raw = "```json\n\
            {\"is_tension\": false, \"rationale\": \"Aligned.\"}\n\
            ```";
        let r = parse_phase6_classifier_response(raw).unwrap();
        assert!(!r.is_tension);
    }

    #[test]
    fn classification_to_edge_yes_produces_tension_edge() {
        let claim = mk_claim(1, "X", "sec_1");
        let entity = mk_entity(1, "Y");
        let state = mk_state(1, entity.id.clone(), "Z", "sec_2");
        let atoms = AtomsFile::new(vec![
            AtomEnvelope::Claim(claim.clone()),
            AtomEnvelope::State(state.clone()),
            AtomEnvelope::Entity(entity.clone()),
        ]);
        let index = AtomIndex::build(&atoms);
        let cand = TensionCandidate {
            id: "cand-0001".into(),
            source_atom: claim.id.clone(),
            target_atom: state.id.clone(),
            discovery: super::super::tensions::CandidateSource::EntityOverlap,
            cluster_id: None,
            shared_entity: Some(entity.id.clone()),
        };
        let content = resolve_candidate_content(&cand, &index).unwrap();
        let cls = Phase6Classification {
            is_tension: true,
            sub_question: Some("Q?".into()),
            confidence: 0.9,
            rationale: "structural conflict".into(),
        };
        let edge = classification_to_edge(&cand, &cls, &content, EdgeId::new(1)).unwrap();
        assert_eq!(edge.edge_type, EdgeType::Tension);
        assert_eq!(edge.confidence, 0.9);
        assert_eq!(edge.provenance, EdgeProvenance::LlmPairwise);
        assert!(edge.sub_question.is_some());
        assert_eq!(edge.evidence.len(), 2);
    }

    #[test]
    fn classification_to_edge_no_returns_none() {
        let cand = TensionCandidate {
            id: "cand-0001".into(),
            source_atom: AtomId::claim(1),
            target_atom: AtomId::state(1),
            discovery: super::super::tensions::CandidateSource::EntityOverlap,
            cluster_id: None,
            shared_entity: None,
        };
        let content = CandidateContent {
            candidate_id: cand.id.clone(),
            source_atom: cand.source_atom.clone(),
            source_kind: AtomKind::Claim,
            source_text: String::new(),
            target_atom: cand.target_atom.clone(),
            target_kind: AtomKind::State,
            target_text: String::new(),
            shared_entity_name: None,
            shared_entity_id: None,
            evidence: Vec::new(),
        };
        let cls = Phase6Classification {
            is_tension: false,
            sub_question: None,
            confidence: 0.8,
            rationale: "co-occur".into(),
        };
        assert!(classification_to_edge(&cand, &cls, &content, EdgeId::new(1)).is_none());
    }

    #[test]
    fn phase6_classifier_response_schema_parses_as_valid_json() {
        let v = phase6_classifier_response_schema();
        assert!(v.is_object());
    }
}
