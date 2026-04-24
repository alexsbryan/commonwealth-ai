//! Resolved atom types — the on-disk form of §2 of the v2.1 spec.
//!
//! Phase 1 produces `SectionExtraction` sketches (names, no IDs,
//! short anchors). Phase 3a/3b resolves those sketches into these
//! canonical atoms with assigned `AtomId`s and chunk-level evidence.
//! The traversal engine reads these records; it never looks at the
//! sketches directly.
//!
//! Only `Entity` and `Event` land in this step — State, Relation,
//! Claim, Question, and Configuration follow in Phase 3b and Phase 5.

use serde::{Deserialize, Serialize};

use crate::enrichment::pipeline::atlas::{
    ClaimScope, DiscourseAct, EnrichmentDepth, EntityType, EpistemicStatus, EventType,
    QuestionType, RelationType, StateType,
};

// ── Shared shapes ────────────────────────────────────────────

/// Typed identifier for an atom. String-backed so the wire format is
/// self-describing (`"entity-001"`, `"event-042"`) and cheap to emit.
/// Use the builder constructors rather than `from_raw` unless you're
/// deserialising or writing adapters for another store.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AtomId(String);

impl AtomId {
    pub fn entity(index: usize) -> Self {
        Self(format!("entity-{index:04}"))
    }
    pub fn event(index: usize) -> Self {
        Self(format!("event-{index:04}"))
    }
    pub fn state(index: usize) -> Self {
        Self(format!("state-{index:04}"))
    }
    pub fn relation(index: usize) -> Self {
        Self(format!("relation-{index:04}"))
    }
    pub fn claim(index: usize) -> Self {
        Self(format!("claim-{index:04}"))
    }
    pub fn question(index: usize) -> Self {
        Self(format!("question-{index:04}"))
    }
    pub fn configuration(index: usize) -> Self {
        Self(format!("config-{index:04}"))
    }

    /// Build from a raw string. Callers are responsible for honouring
    /// the `<type>-<index>` convention; use the typed constructors
    /// above when generating new IDs.
    pub fn from_raw(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Reference to a specific passage in the corpus. Step 3a fills
/// `chunk_id` with the section id (the grain we have from sketches);
/// Phase 5 refines it to the paragraph chunk id once the full chunk
/// index is traversed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkRef {
    pub chunk_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passage_preview: Option<String>,
}

impl ChunkRef {
    pub fn new(chunk_id: impl Into<String>, preview: Option<String>) -> Self {
        Self {
            chunk_id: chunk_id.into(),
            passage_preview: preview,
        }
    }
}

/// Inclusive range over section ids (e.g. `ch011..=ch013`). The range
/// is ordinal-based — start and end are section ids, not byte offsets.
/// Use `SectionRange::point(id)` for atoms that live in a single
/// section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SectionRange {
    pub start: String,
    pub end: String,
}

impl SectionRange {
    pub fn point(section_id: impl Into<String>) -> Self {
        let s: String = section_id.into();
        Self { start: s.clone(), end: s }
    }
}

/// A single ordinal location in the corpus — a section id + optional
/// within-section position. Events use this instead of
/// `SectionRange` because an event happens *at* a point, not *across*
/// a span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SectionPosition {
    pub section_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paragraph_index: Option<usize>,
}

impl SectionPosition {
    pub fn section(section_id: impl Into<String>) -> Self {
        Self {
            section_id: section_id.into(),
            paragraph_index: None,
        }
    }
}

// ── Entity ───────────────────────────────────────────────────

/// A named thing that persists across sections (spec §2.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: AtomId,
    pub canonical_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub entity_type: EntityType,
    pub first_appearance: ChunkRef,
    /// One-sentence characterisation derived from the corpus, not from
    /// external knowledge (spec §2.1, Wittgenstein note).
    pub description: String,
    /// Corpus-relative importance (0.0–1.0), derived from frequency,
    /// narrative weight, and cross-reference density.
    pub salience: f32,
    pub enrichment_depth: EnrichmentDepth,
}

// ── Event ────────────────────────────────────────────────────

/// Something that happens at a specific point in the corpus
/// (spec §2.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: AtomId,
    pub description: String,
    pub event_type: EventType,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<AtomId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ChunkRef>,
    pub section_position: SectionPosition,
    /// Causal antecedents. Empty at Step 3a — causal chains are
    /// assembled in Phase 4 once events are clustered and their
    /// transition links inferred.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causal_antecedents: Vec<AtomId>,
    pub enrichment_depth: EnrichmentDepth,
}

// ── State ────────────────────────────────────────────────────

/// A condition an entity or relation occupies at a point or interval
/// in the corpus (spec §2.2). `entity_id` points at either an Entity
/// atom or a Relation atom — relation states reuse the same machinery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub id: AtomId,
    pub entity_id: AtomId,
    pub label: String,
    pub state_type: StateType,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ChunkRef>,
    pub section_range: SectionRange,
    /// Extraction confidence, LLM-reported.
    ///
    /// `None` when the state was emitted by the deterministic
    /// resolver (Phase 3b) with no LLM scoring. A previous version
    /// of this field stamped `Some(1.0)` on every deterministic
    /// atom, which collapsed the confidence histogram into a
    /// meaningless bimodal distribution. Schema-validation §3 skips
    /// `None`-valued atoms so the histogram reflects only
    /// LLM-reported values. Phase 5 (deferred atom interpretation)
    /// will replace `None` with a real score when it ships.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    pub enrichment_depth: EnrichmentDepth,
}

// ── Relation ─────────────────────────────────────────────────

/// A persistent interaction between two or more entities (spec §2.6).
/// First-class because its properties are emergent — not reducible to
/// either participant's individual trajectory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    pub id: AtomId,
    pub label: String,
    pub participants: Vec<AtomId>,
    pub relation_type: RelationType,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ChunkRef>,
    pub section_range: SectionRange,
    pub enrichment_depth: EnrichmentDepth,
}

// ── Claim ────────────────────────────────────────────────────

/// A knowledge-carrying act performed by the text (spec §2.5). Carries
/// both `discourse_act` (illocutionary force) and `epistemic_status`
/// (certainty) as independent axes per §2.5.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub id: AtomId,
    pub content: String,
    pub discourse_act: DiscourseAct,
    pub epistemic_status: EpistemicStatus,
    pub scope: ClaimScope,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ChunkRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributed_to: Option<AtomId>,
    /// Extraction confidence — how clearly the system identified this
    /// claim. Distinct from `epistemic_status` which is the claim's
    /// certainty within the text.
    ///
    /// `None` when the claim was emitted by the deterministic
    /// resolver with no LLM scoring. See `State.confidence` for the
    /// rationale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    pub enrichment_depth: EnrichmentDepth,
}

// ── Question ─────────────────────────────────────────────────

/// An inquiry the corpus raises, addresses, or leaves open (spec §2.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    pub id: AtomId,
    pub content: String,
    pub question_type: QuestionType,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addressed_by: Vec<AtomId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raised_at: Vec<ChunkRef>,
    pub resolution_status: ResolutionStatus,
    pub enrichment_depth: EnrichmentDepth,
}

/// Status of a Question atom's inquiry. Matches spec §2.4.
/// Serialises as a tagged union so downstream code can branch on
/// `kind` without walking into unexpected variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResolutionStatus {
    /// A single Claim atom answers the question.
    Resolved { claim_id: AtomId },
    /// Multiple Claim atoms disagree on the question.
    Contested { claim_ids: Vec<AtomId> },
    /// The corpus raises the question without answering.
    Open,
    /// The corpus reframes the question as ill-posed.
    Dissolved,
}

// ── Configuration ────────────────────────────────────────────

/// The interpretive structure the work as a whole enacts through
/// the arrangement of its parts (spec §2.7). Optional per corpus —
/// most salient for authored literary / philosophical works.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Configuration {
    pub id: AtomId,
    pub label: String,
    pub description: String,
    pub constituent_atoms: Vec<AtomId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ChunkRef>,
    pub confidence: f32,
    /// Explicit acknowledgment that this is an interpretive product
    /// (Ricoeur constraint per spec §1.2). States what alternative
    /// readings the extraction considered.
    pub interpretive_note: String,
    pub enrichment_depth: EnrichmentDepth,
}

// ── Atom envelope (on-disk shape per spec §6.2) ──────────────

/// Discriminated atom-type tag. Matches the `"atom_type"` string in
/// the on-disk JSON. Spec §2 enumerates the seven atom types; Step
/// 3a emits only `Entity` and `Event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AtomType {
    Entity,
    Event,
    State,
    Relation,
    Claim,
    Question,
    Configuration,
}

/// On-disk representation of a single atom. Untagged body per
/// `atom_type` keeps the JSON spec-compliant while still typing the
/// payload in Rust.
///
/// Serialised shape:
/// ```json
/// {
///   "id": "entity-0001",
///   "atom_type": "Entity",
///   "enrichment_depth": "extracted",
///   "data": { ... entity fields ... }
/// }
/// ```
///
/// All seven spec §2 atom types are represented. Step 3a emits
/// `Entity` + `Event`; Step 3b adds `State` + `Relation` + `Claim` +
/// `Question`; Step 5 (Phase 8) adds `Configuration`. Deliberately
/// no `#[serde(other)]` — an unknown atom type on disk is a bug,
/// not a silent fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "atom_type", content = "data", rename_all = "PascalCase")]
pub enum AtomEnvelope {
    Entity(Entity),
    Event(Event),
    State(State),
    Relation(Relation),
    Claim(Claim),
    Question(Question),
    Configuration(Configuration),
}

impl AtomEnvelope {
    pub fn id(&self) -> &AtomId {
        match self {
            AtomEnvelope::Entity(a) => &a.id,
            AtomEnvelope::Event(a) => &a.id,
            AtomEnvelope::State(a) => &a.id,
            AtomEnvelope::Relation(a) => &a.id,
            AtomEnvelope::Claim(a) => &a.id,
            AtomEnvelope::Question(a) => &a.id,
            AtomEnvelope::Configuration(a) => &a.id,
        }
    }

    pub fn enrichment_depth(&self) -> EnrichmentDepth {
        match self {
            AtomEnvelope::Entity(a) => a.enrichment_depth,
            AtomEnvelope::Event(a) => a.enrichment_depth,
            AtomEnvelope::State(a) => a.enrichment_depth,
            AtomEnvelope::Relation(a) => a.enrichment_depth,
            AtomEnvelope::Claim(a) => a.enrichment_depth,
            AtomEnvelope::Question(a) => a.enrichment_depth,
            AtomEnvelope::Configuration(a) => a.enrichment_depth,
        }
    }
}

/// Top-level atom file written to `atlas/atoms.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomsFile {
    pub schema_version: String,
    pub atoms: Vec<AtomEnvelope>,
}

impl AtomsFile {
    /// Current on-disk schema version for the atoms file. Bumped when
    /// the envelope or any variant's data shape changes in a
    /// backwards-incompatible way.
    pub const SCHEMA_VERSION: &'static str = "2.0";

    pub fn new(atoms: Vec<AtomEnvelope>) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION.to_string(),
            atoms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atom_id_constructors_produce_zero_padded_ids() {
        assert_eq!(AtomId::entity(1).as_str(), "entity-0001");
        assert_eq!(AtomId::event(42).as_str(), "event-0042");
        assert_eq!(AtomId::state(7).as_str(), "state-0007");
    }

    #[test]
    fn entity_atom_roundtrips_through_envelope() {
        let entity = Entity {
            id: AtomId::entity(1),
            canonical_name: "Alyosha".into(),
            aliases: vec!["Alexei Fyodorovich".into()],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0004", Some("the third son".into())),
            description: "Youngest Karamazov brother; novice at the monastery.".into(),
            salience: 0.92,
            enrichment_depth: EnrichmentDepth::Extracted,
        };
        let env = AtomEnvelope::Entity(entity.clone());
        let json = serde_json::to_string(&env).unwrap();
        // Pin the on-disk shape — atom_type as PascalCase, data nested.
        assert!(json.contains("\"atom_type\":\"Entity\""));
        assert!(json.contains("\"canonical_name\":\"Alyosha\""));
        let back: AtomEnvelope = serde_json::from_str(&json).unwrap();
        match back {
            AtomEnvelope::Entity(e) => {
                assert_eq!(e.canonical_name, entity.canonical_name);
                assert_eq!(e.salience, entity.salience);
                assert_eq!(e.enrichment_depth, EnrichmentDepth::Extracted);
            }
            _ => panic!("expected Entity variant"),
        }
    }

    #[test]
    fn event_atom_roundtrips_with_participants() {
        let event = Event {
            id: AtomId::event(1),
            description: "Zosima instructs Alyosha to leave the monastery.".into(),
            event_type: EventType::Decision,
            participants: vec![AtomId::entity(1), AtomId::entity(2)],
            evidence: vec![ChunkRef::new(
                "sec_0013",
                Some("go out into the world".into()),
            )],
            section_position: SectionPosition::section("sec_0013"),
            causal_antecedents: Vec::new(),
            enrichment_depth: EnrichmentDepth::Extracted,
        };
        let env = AtomEnvelope::Event(event);
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"atom_type\":\"Event\""));
        let back: AtomEnvelope = serde_json::from_str(&json).unwrap();
        match back {
            AtomEnvelope::Event(e) => {
                assert_eq!(e.participants.len(), 2);
                assert_eq!(e.section_position.section_id, "sec_0013");
            }
            _ => panic!("expected Event variant"),
        }
    }

    #[test]
    fn state_atom_roundtrips_with_entity_id_and_section_range() {
        use crate::enrichment::pipeline::atlas::StateType;
        let state = State {
            id: AtomId::state(17),
            entity_id: AtomId::entity(1),
            label: "Reluctant attraction — Jane watches Rochester with increasing intensity".into(),
            state_type: StateType::Psychological,
            evidence: vec![ChunkRef::new("ch015", None), ChunkRef::new("ch017", None)],
            section_range: SectionRange { start: "ch014".into(), end: "ch018".into() },
            confidence: Some(0.82),
            enrichment_depth: EnrichmentDepth::Extracted,
        };
        let env = AtomEnvelope::State(state.clone());
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"atom_type\":\"State\""));
        assert!(json.contains("\"state_type\":\"psychological\""));
        let back: AtomEnvelope = serde_json::from_str(&json).unwrap();
        match back {
            AtomEnvelope::State(s) => {
                assert_eq!(s.entity_id, state.entity_id);
                assert_eq!(s.confidence, Some(0.82));
            }
            _ => panic!("expected State variant"),
        }
    }

    #[test]
    fn relation_atom_carries_participants_in_order() {
        use crate::enrichment::pipeline::atlas::RelationType;
        let relation = Relation {
            id: AtomId::relation(3),
            label: "Jane–Rochester: employer/dependent bond becoming mutual transformation".into(),
            participants: vec![AtomId::entity(1), AtomId::entity(2)],
            relation_type: RelationType::Interpersonal,
            evidence: Vec::new(),
            section_range: SectionRange {
                start: "ch012".into(),
                end: "ch038".into(),
            },
            enrichment_depth: EnrichmentDepth::Extracted,
        };
        let json = serde_json::to_string(&AtomEnvelope::Relation(relation.clone())).unwrap();
        assert!(json.contains("\"atom_type\":\"Relation\""));
        let back: AtomEnvelope = serde_json::from_str(&json).unwrap();
        match back {
            AtomEnvelope::Relation(r) => {
                assert_eq!(r.participants, relation.participants);
                assert_eq!(r.relation_type, relation.relation_type);
            }
            _ => panic!("expected Relation"),
        }
    }

    #[test]
    fn claim_atom_carries_discourse_act_and_epistemic_status() {
        use crate::enrichment::pipeline::atlas::{ClaimScope, DiscourseAct, EpistemicStatus};
        let claim = Claim {
            id: AtomId::claim(42),
            content: "Active love costs more than dreamt love.".into(),
            discourse_act: DiscourseAct::Argue,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: vec![ChunkRef::new("ch_5_p3", Some("love in dreams is greedy".into()))],
            attributed_to: Some(AtomId::entity(7)),
            confidence: Some(0.91),
            enrichment_depth: EnrichmentDepth::Extracted,
        };
        let json = serde_json::to_string(&AtomEnvelope::Claim(claim.clone())).unwrap();
        assert!(json.contains("\"discourse_act\":\"argue\""));
        assert!(json.contains("\"epistemic_status\":\"confident\""));
        assert!(json.contains("\"scope\":\"universal\""));
        let back: AtomEnvelope = serde_json::from_str(&json).unwrap();
        match back {
            AtomEnvelope::Claim(c) => {
                assert_eq!(c.discourse_act, DiscourseAct::Argue);
                assert_eq!(c.attributed_to, claim.attributed_to);
            }
            _ => panic!("expected Claim"),
        }
    }

    #[test]
    fn question_atom_resolution_status_variants_roundtrip() {
        use crate::enrichment::pipeline::atlas::QuestionType;
        for status in [
            ResolutionStatus::Resolved { claim_id: AtomId::claim(1) },
            ResolutionStatus::Contested {
                claim_ids: vec![AtomId::claim(1), AtomId::claim(2)],
            },
            ResolutionStatus::Open,
            ResolutionStatus::Dissolved,
        ] {
            let q = Question {
                id: AtomId::question(1),
                content: "Can authentic feeling survive contact with social reality?".into(),
                question_type: QuestionType::Thematic,
                addressed_by: Vec::new(),
                raised_at: Vec::new(),
                resolution_status: status,
                enrichment_depth: EnrichmentDepth::Extracted,
            };
            let json = serde_json::to_string(&AtomEnvelope::Question(q.clone())).unwrap();
            let back: AtomEnvelope = serde_json::from_str(&json).unwrap();
            match back {
                AtomEnvelope::Question(r) => {
                    assert_eq!(r.content, q.content);
                }
                _ => panic!("expected Question"),
            }
        }
    }

    #[test]
    fn configuration_atom_requires_interpretive_note() {
        let cfg = Configuration {
            id: AtomId::configuration(1),
            label: "Anna's descent mirrored against Levin's ascent, arguing authentic \
                   life requires participation in something beyond individual desire."
                .into(),
            description: "Two trajectories with no shared characters after Part 1, \
                         structurally mirrored."
                .into(),
            constituent_atoms: vec![AtomId::entity(1), AtomId::entity(2)],
            evidence: Vec::new(),
            confidence: 0.71,
            interpretive_note: "Alternative reading: the parallel is ironic rather than \
                               argumentative. We extract the parallel-as-argument reading \
                               as primary but flag the ironic reading as live."
                .into(),
            enrichment_depth: EnrichmentDepth::Extracted,
        };
        let env = AtomEnvelope::Configuration(cfg.clone());
        // Every atom type exposes id() and enrichment_depth() without match.
        assert_eq!(env.id().as_str(), "config-0001");
        assert_eq!(env.enrichment_depth(), EnrichmentDepth::Extracted);
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"atom_type\":\"Configuration\""));
        assert!(json.contains("interpretive_note"));
    }

    #[test]
    fn atoms_file_serialises_with_schema_version() {
        let file = AtomsFile::new(vec![]);
        let json = serde_json::to_string(&file).unwrap();
        assert!(json.contains("\"schema_version\":\"2.0\""));
        assert!(json.contains("\"atoms\":[]"));
    }

    #[test]
    fn atom_envelope_exposes_id_and_depth_without_matching() {
        let entity = Entity {
            id: AtomId::entity(5),
            canonical_name: "X".into(),
            aliases: Vec::new(),
            entity_type: EntityType::Concept,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "x".into(),
            salience: 0.1,
            enrichment_depth: EnrichmentDepth::Structural,
        };
        let env = AtomEnvelope::Entity(entity);
        assert_eq!(env.id().as_str(), "entity-0005");
        assert_eq!(env.enrichment_depth(), EnrichmentDepth::Structural);
    }
}
