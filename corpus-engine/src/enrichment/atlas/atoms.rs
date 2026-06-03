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
///
/// Two id shapes coexist (Move 6):
///   - **Sequential** (`entity-0001`, `event-0042`, …) — the original
///     v2.0 shape. Produced by `AtomId::entity(idx)` and friends.
///     Migrated in place by `sovereign atlas migrate-ids`.
///   - **Content-hash** (`entity-<16 hex>` derived from canonical
///     fields) — produced by `AtomId::*_content_hash(...)`. Stable
///     across re-extractions of the same conceptual atom. Required
///     for incremental atlas updates: re-extracting an article
///     gives back the same Einstein atom id every time, so
///     cross-corpus edges + meta-atlas anchors survive deltas.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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
    pub fn argument_reconstruction(index: usize) -> Self {
        Self(format!("argument-{index:04}"))
    }
    pub fn position(index: usize) -> Self {
        Self(format!("position-{index:04}"))
    }
    pub fn opposition(index: usize) -> Self {
        Self(format!("opposition-{index:04}"))
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

    // ── Move 6: content-hash constructors ──────────────────
    //
    // Each *_content_hash constructor takes the atom's identifying
    // fields + the owning corpus_id and produces a stable id that
    // re-extraction will reproduce exactly. Hash inputs are
    // normalised where appropriate (canonical_name via lookup_key,
    // enum types via their snake_case as_str_repr) so casing/punct
    // variations resolve to the same id.
    //
    // Truncated to 16 hex chars (64 bits). Collision space is
    // sufficient for <10M atoms per corpus by the birthday bound;
    // a pre-deployment scan over the live atlas confirms zero
    // collisions before migration is run.

    /// Entity id: hash(lookup_key(canonical_name) | entity_type | corpus_id).
    /// Doesn't depend on first_appearance so the id is stable when
    /// the originating document is deleted or shifts.
    pub fn entity_content_hash(
        canonical_name: &str,
        entity_type: &crate::enrichment::pipeline::atlas::EntityType,
        corpus_id: &str,
    ) -> Self {
        let key = crate::atlas_canonical::lookup_key(canonical_name);
        let input = format!("entity|{key}|{}|{corpus_id}", entity_type.as_str_repr());
        Self(format!("entity-{}", short_hash(&input)))
    }

    /// Event id: hash(trimmed_description | event_type | first_section_id | corpus_id).
    /// Less stable than Entity across re-extractions when LLM
    /// wording shifts — acceptable for v1 since Event ids primarily
    /// scope within one corpus.
    pub fn event_content_hash(
        description: &str,
        event_type: &crate::enrichment::pipeline::atlas::EventType,
        first_section_id: &str,
        corpus_id: &str,
    ) -> Self {
        let input = format!(
            "event|{}|{}|{first_section_id}|{corpus_id}",
            description.trim(),
            event_type.as_str_repr()
        );
        Self(format!("event-{}", short_hash(&input)))
    }

    /// State id: hash(entity_id | state_type | label | corpus_id).
    pub fn state_content_hash(
        entity_id: &AtomId,
        state_type: &crate::enrichment::pipeline::atlas::StateType,
        label: &str,
        corpus_id: &str,
    ) -> Self {
        let input = format!(
            "state|{}|{}|{}|{corpus_id}",
            entity_id.as_str(),
            state_type.as_str_repr(),
            label.trim()
        );
        Self(format!("state-{}", short_hash(&input)))
    }

    /// Relation id: hash(sorted_participants | relation_type | label | corpus_id).
    /// Participant order doesn't matter for relation identity (A↔B
    /// is the same as B↔A), so sort before hashing.
    pub fn relation_content_hash(
        participants: &[AtomId],
        relation_type: &crate::enrichment::pipeline::atlas::RelationType,
        label: &str,
        corpus_id: &str,
    ) -> Self {
        let mut sorted: Vec<&str> = participants.iter().map(|a| a.as_str()).collect();
        sorted.sort();
        let input = format!(
            "relation|{}|{}|{}|{corpus_id}",
            sorted.join(","),
            relation_type.as_str_repr(),
            label.trim()
        );
        Self(format!("relation-{}", short_hash(&input)))
    }

    /// Claim id: hash(content | discourse_act | epistemic_status | corpus_id).
    pub fn claim_content_hash(
        content: &str,
        discourse_act: &crate::enrichment::pipeline::atlas::DiscourseAct,
        epistemic_status: &crate::enrichment::pipeline::atlas::EpistemicStatus,
        corpus_id: &str,
    ) -> Self {
        let input = format!(
            "claim|{}|{}|{}|{corpus_id}",
            content.trim(),
            discourse_act.as_str_repr(),
            epistemic_status.as_str_repr()
        );
        Self(format!("claim-{}", short_hash(&input)))
    }

    /// Question id: hash(content | question_type | corpus_id).
    pub fn question_content_hash(
        content: &str,
        question_type: &crate::enrichment::pipeline::atlas::QuestionType,
        corpus_id: &str,
    ) -> Self {
        let input = format!(
            "question|{}|{}|{corpus_id}",
            content.trim(),
            question_type.as_str_repr()
        );
        Self(format!("question-{}", short_hash(&input)))
    }

    /// Configuration id: hash(label | corpus_id). Configurations are
    /// section-level interpretive structure; their label is the
    /// stable surrogate.
    pub fn configuration_content_hash(label: &str, corpus_id: &str) -> Self {
        let input = format!("config|{}|{corpus_id}", label.trim());
        Self(format!("config-{}", short_hash(&input)))
    }

    /// ArgumentReconstruction id: hash(name | corpus_id).
    pub fn argument_reconstruction_content_hash(name: &str, corpus_id: &str) -> Self {
        let input = format!("argument|{}|{corpus_id}", name.trim());
        Self(format!("argument-{}", short_hash(&input)))
    }

    /// Position id: hash(canonical_name | stance | corpus_id).
    pub fn position_content_hash(canonical_name: &str, stance: &str, corpus_id: &str) -> Self {
        let key = crate::atlas_canonical::lookup_key(canonical_name);
        let input = format!("position|{key}|{}|{corpus_id}", stance.trim());
        Self(format!("position-{}", short_hash(&input)))
    }

    /// Opposition id: hash(canonical_label | corpus_id).
    pub fn opposition_content_hash(canonical_label: &str, corpus_id: &str) -> Self {
        let key = crate::atlas_canonical::lookup_key(canonical_label);
        let input = format!("opposition|{key}|{corpus_id}");
        Self(format!("opposition-{}", short_hash(&input)))
    }

    /// True iff this id looks like a Move-6 content-hash id
    /// (length matches `<type>-<16 hex>`). Used by the migration
    /// module to skip already-migrated atoms.
    pub fn is_content_hash(&self) -> bool {
        let parts: Vec<&str> = self.0.splitn(2, '-').collect();
        if parts.len() != 2 {
            return false;
        }
        parts[1].len() == 16 && parts[1].chars().all(|c| c.is_ascii_hexdigit())
    }
}

/// 16-char prefix of blake3 hex digest. 64-bit truncation; safe for
/// <10M atoms per corpus by birthday bound.
fn short_hash(input: &str) -> String {
    let full = blake3::hash(input.as_bytes()).to_hex().to_string();
    full[..16].to_string()
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
    /// `source_doc_id` of the document this evidence chunk belongs to,
    /// when known. Lets the Atlas join an atom to its document's index
    /// recency (`crate::freshness`) and bubble freshly-(re)indexed
    /// content to the top. `#[serde(default)]` — older `atoms.json`
    /// omit it (read as `None`); the post-reindex atlas rebuild
    /// backfills it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_doc_id: Option<String>,
}

impl ChunkRef {
    pub fn new(chunk_id: impl Into<String>, preview: Option<String>) -> Self {
        Self {
            chunk_id: chunk_id.into(),
            passage_preview: preview,
            source_doc_id: None,
        }
    }

    /// Builder: attach the owning document's `source_doc_id`.
    pub fn with_source_doc(mut self, source_doc_id: Option<String>) -> Self {
        self.source_doc_id = source_doc_id;
        self
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

// ── Provenance (AD-4) ────────────────────────────────────────

/// What signal produced this atom. The architecture-over-Enron push
/// (Phase 4 reconciliation) co-locates this on the atom rather than a
/// separate join table so the reconciliation audit log can write
/// fast and the cross-origin merge can read fast.
///
/// `Other(String)` keeps the enum open for future signal kinds without
/// a schema migration — same extensibility convention as `EntityType`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum SignalKind {
    /// Span produced by the GLiNER chunk-level NER pass.
    GlinerSpan,
    /// Atom produced by an LLM-batch entity-extraction call (the
    /// pre-2026 path; `corpus-engine::enrichment::entity_extraction`).
    #[default]
    LlmBatch,
    /// Atom produced by the column-aware extractor reading a tabular
    /// parsed-form parquet cache. Column header value is the surface
    /// form; the column header name supplies the entity-type hint.
    ColumnHeader,
    /// Atom produced from a parsed RFC-5322 / MIME email header
    /// (`From:`, `To:`, `Cc:`).
    EmailHeader,
    /// Atom produced from a described-asset attachment (an attachment
    /// resolved to a Person/Organization via signature block, calendar
    /// ATTENDEE, etc.).
    AttachmentDescription,
    /// Atom produced by the manual reconciliation audit (`sovereign
    /// atlas reconciliation split / merge`).
    OperatorAction,
    /// Reserved escape hatch for downstream callers.
    Other(String),
}


/// Atom origin record (AD-4).
///
/// Captures *which signal* produced a surface form. The atlas merge
/// reads this to write a per-merge audit entry naming exactly which
/// signals fired (e.g. "merged on (EmailHeader + GlinerSpan + Judge)").
///
/// `extractor_id` is the human-readable name of the producing
/// extractor (`"email_rfc5322"`, `"gliner_chunk_ner"`,
/// `"column_aware"`, `"llm_batch"`). `source_chunk_id` is the chunk
/// the span came from (or `None` for document-level atoms like
/// `EmailHeader`-typed Entity).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Provenance {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub extractor_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_doc_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_chunk_id: Option<String>,
    #[serde(default)]
    pub signal_kind: SignalKind,
}

impl Provenance {
    pub fn new(
        extractor_id: impl Into<String>,
        source_doc_id: impl Into<String>,
        signal_kind: SignalKind,
    ) -> Self {
        Self {
            extractor_id: extractor_id.into(),
            source_doc_id: source_doc_id.into(),
            source_chunk_id: None,
            signal_kind,
        }
    }

    pub fn with_chunk(mut self, chunk_id: impl Into<String>) -> Self {
        self.source_chunk_id = Some(chunk_id.into());
        self
    }
}

// ── Entity ───────────────────────────────────────────────────

/// A named thing that persists across sections (spec §2.1).
///
/// The `affiliation` / `role` / `participants` fields are populated by
/// the personal-conversational entity-extraction phase and otherwise
/// left empty. They are not part of the literary/philosophy atlas
/// pipeline's output; they round-trip through serde as absent fields
/// so existing atoms.json files on disk deserialise unchanged.
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
    /// Verbatim defining sentence from the source for `concept`
    /// entities — the article's canonical "X is defined as..."
    /// passage, ≤200 chars. Differs from `description` (which is a
    /// gloss): `defining_quote` is exact text that retrieval can
    /// surface to a downstream judge or reader. `None` for non-
    /// concept entity types or when the article does not lift a
    /// distinct definitional sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defining_quote: Option<String>,
    /// Corpus-relative importance (0.0–1.0), derived from frequency,
    /// narrative weight, and cross-reference density.
    pub salience: f32,
    pub enrichment_depth: EnrichmentDepth,
    /// Organisational affiliation for `Person` entities — e.g. "Acme
    /// Corp" for "Sarah Chen at Acme". Populated only by the
    /// personal/conversational entity-extraction phase; absent on
    /// literary/philosophy pipeline output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affiliation: Option<String>,
    /// Role or title for `Person` entities — e.g. "VP Engineering".
    /// Same scoping as `affiliation`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Participating entity IDs for `Initiative` entities — the
    /// people and organisations involved in the work. Empty on
    /// non-initiative entities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<AtomId>,
    /// Atom origin record (AD-4 — architecture-over-Enron Phase 4).
    /// Captures which signal produced this atom. Defaults to an empty
    /// [`Provenance`] for back-compat with pre-2.2 atoms.json files.
    #[serde(default)]
    pub provenance: Provenance,
    /// Gap-B qualifier for `Concept`-typed entities sourced from the
    /// routed-Phase-1 typed-extension dispatcher. Populated when the
    /// resolver projects a Mechanism / Definition / Image / Motif /
    /// FormalDevice sketch onto a Concept atom — the value tells the
    /// brief assembler and atlas-tier retrieval which argumentative
    /// or descriptive slot the concept fills:
    ///
    /// - `mechanism` — argumentative named lever ("spread pricing",
    ///   "EUV monopoly", "salary cap").
    /// - `definition` — descriptive zettel-style "X is the practice
    ///   of …".
    /// - `image` — lyric concrete sense-image ("the bruised plum").
    /// - `motif` — lyric recurring image-as-structure.
    /// - `formal_device` — lyric compositional move (anaphora,
    ///   caesura, refrain).
    ///
    /// `None` on base-schema Concept atoms (literary / philosophy
    /// extractions) and on every non-Concept entity_type. The field
    /// is `Option<String>` rather than an enum so future modes can
    /// add qualifier values without bumping AtomsFile::SCHEMA_VERSION.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concept_kind: Option<String>,
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
    /// Verbatim ≤200-char excerpt from the source supporting this
    /// claim — populated when the claim states a position or argument
    /// for which a single quotable sentence exists in the article.
    /// Distinct from `content` (which is a propositional-form
    /// paraphrase): retrieval can surface this directly so a judge
    /// or downstream reader sees the article's own words. `None`
    /// when no clean quotable sentence exists or when the claim is
    /// derived rather than extracted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quotable_excerpt: Option<String>,
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
    /// Code symbol / file path the claim asserts something about,
    /// after [`AnchorSnapProcessor`] has snapped it to the closest
    /// verbatim span in the source. For engineering-atlas pipelines
    /// this comes from the LLM's `code_anchors[0]` (the first
    /// declared anchor for the claim). For literary-atlas pipelines
    /// the field is absent.
    ///
    /// `None` rather than empty string so the drift report renderer
    /// can distinguish "claim never carried an anchor" from "claim
    /// carried an empty anchor." Forward-compat: existing atoms.json
    /// files written before this field was added deserialise cleanly
    /// thanks to `#[serde(default)]`; absent serialisations write no
    /// key thanks to `skip_serializing_if`.
    ///
    /// Wired through to `NarrativeAtomView.canonical_name` in
    /// `atlas_drift_report.rs` so the cross-corpus fuzzy matcher
    /// has a real code-symbol-shaped string to look up (the prose
    /// content is too long to ever fuzzy-match a function name).
    /// Without this field, every normative claim falls through to
    /// the critical bucket as "(no anchor)" — the canonical
    /// reproducer for the post-2026-05-12 drift report's empty
    /// Act-on bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// Gap-B qualifier marking a Claim atom sourced from a typed-
    /// extension non-base sketch. Populated when the resolver
    /// projects an Evidence / Concession / PropertyClaim /
    /// Observation / Realisation / Blocker sketch onto the Claim
    /// envelope. Brief renderer reads the qualifier to phrase
    /// appropriately ("X is invoked as evidence for Y", "the
    /// section grants X but Y still holds", etc.). `None` on base
    /// argumentative-prose claims.
    ///
    /// Values: `property` | `evidence` | `concession` | `observation`
    /// | `realisation` | `blocker` | `status` | `example`. String
    /// rather than enum so future modes add values without
    /// migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_kind: Option<String>,
    /// Concession-specific qualifier. Set only when
    /// `claim_kind == "concession"`. Values: `intact` | `narrowed`
    /// | `retracted` — same vocabulary as the typed-extension
    /// `ConcessionSketch.outcome` field. The brief renderer uses
    /// this to phrase the concession's load-bearing direction:
    /// `intact` reads as "X is granted, but Y still holds";
    /// `narrowed` as "X is granted, bounding Y to ..."; `retracted`
    /// as "X compels yielding Y".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concession_outcome: Option<String>,
    /// Evidence-specific qualifier. Set only when
    /// `claim_kind == "evidence"`. Values: `study` | `figure` |
    /// `historical_example` | `case_study` | `personal_anecdote`
    /// | `quotation` | `other`. Routes the brief renderer to
    /// citation-style phrasing for studies/figures and
    /// narrative-style phrasing for historical examples /
    /// anecdotes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_kind: Option<String>,
    pub enrichment_depth: EnrichmentDepth,
}

// ── ArgumentReconstruction ───────────────────────────────────

/// A named philosophical argument as the article reconstructs it —
/// premise list, conclusion, objections. Targets the essay-judge
/// axis "argument_depth", which under-credits passages that contain
/// the argument's pieces scattered across paragraphs without an
/// explicit reconstruction.
///
/// Optional and sparse — most sections do not contain a named
/// argument. Phase 1 emits this only when the section both names
/// the argument and presents its premise structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgumentReconstruction {
    pub id: AtomId,
    /// Article-level name ("Knowledge Argument", "Consequence
    /// Argument", "Function Argument").
    pub name: String,
    /// Originating philosopher resolved to an Entity atom. `None`
    /// when the section presents the argument without naming a
    /// specific proponent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proponent: Option<AtomId>,
    /// Premises in order — one propositional-form statement each.
    pub premises: Vec<String>,
    /// Conclusion the premises support.
    pub conclusion: String,
    /// Named objections as the section presents them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objections: Vec<Objection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ChunkRef>,
    pub section_position: SectionPosition,
    pub enrichment_depth: EnrichmentDepth,
}

/// One objection an article presents against an argument.
///
/// Deserialises permissively: accepts either a bare string (legacy
/// shape — `name` only) or a `{ name, content }` object so older
/// atoms.json keeps loading after the schema migration. New
/// extractions populate `content` with one-sentence prose so the
/// dialectical_breadth axis sees the substance, not just the name.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Objection {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content: String,
}

impl<'de> Deserialize<'de> for Objection {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Str(String),
            Obj {
                name: String,
                #[serde(default)]
                content: String,
            },
        }
        match Repr::deserialize(d)? {
            Repr::Str(name) => Ok(Objection {
                name,
                content: String::new(),
            }),
            Repr::Obj { name, content } => Ok(Objection { name, content }),
        }
    }
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

// ── Position (Gap B) ─────────────────────────────────────────

/// A NAMED stance the corpus identifies — a whole view the section
/// either endorses, rebuts, or surveys. Distinct from Claim atoms:
/// a Position is "the view that X" — a thing the section *names*
/// and may reference repeatedly; a Claim is "X" — a single assertion
/// the section makes. Sourced from typed-extension
/// `argumentative.positions[]` (workstream B / Gap B), promoted from
/// Phase 1 cache into the resolved atlas so atlas-tier retrieval
/// surfaces it the same way it surfaces Claim atoms.
///
/// Examples across domains:
/// - `the rent-concentration thesis` (argumentative essay, AI)
/// - `Hardin's tragedy thesis` (argumentative essay, commons)
/// - `Ostrom's third-pattern view` (argumentative essay, commons)
/// - `the parks-as-product thesis` (urbanism essay, Jacobs)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub id: AtomId,
    /// Reader-facing name for the stance, in 3-7 words.
    pub canonical_name: String,
    /// One-sentence statement of what the position SAYS.
    pub content: String,
    /// `endorse` | `rebut` | `survey` | `mixed`. Carried as a string
    /// rather than an enum so future stance refinements land without
    /// migration; readers snap on the four known values.
    pub stance: String,
    /// Resolved Entity id for the proponent — Person or Institution
    /// the position is attributed to. `None` when the section voices
    /// the position itself or when proponent didn't resolve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proponent_id: Option<AtomId>,
    /// Atoms (Mechanism Concepts, evidence Claims) the resolver
    /// detected as supporting this position via the Phase 1
    /// `evidence_invocations[].supports` linkage. Populated by the
    /// resolver's edge-emission pass after all atoms have ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_ids: Vec<AtomId>,
    pub first_appearance: ChunkRef,
    /// 3-8 word keyphrases the prompt anchored the position against.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<String>,
    /// Corpus-relative importance (0.0–1.0). Derived from frequency +
    /// section salience, same ordinal-rank discipline as entity
    /// salience.
    pub salience: f32,
    pub enrichment_depth: EnrichmentDepth,
}

// ── Opposition (Gap B) ───────────────────────────────────────

/// A NAMED X-vs-Y framing the section sets up. Distinct from a Claim
/// that uses an opposition argumentatively: the Opposition atom names
/// the structural binary itself ("markets vs governments", "planting
/// vs maintenance", "supply expansion vs substitution"). Sourced from
/// typed-extension `argumentative.oppositions[]`.
///
/// `left_atom_id` / `right_atom_id` resolve the two sides to existing
/// Concept Entity atoms when fuzzy-match succeeds. Falls back to
/// `left_label` / `right_label` raw strings when resolution fails —
/// the atom still surfaces in retrieval; just doesn't graph-traverse.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opposition {
    pub id: AtomId,
    /// Reader-facing label combining both sides: "markets vs
    /// governments". Used by retrieval scoring + brief renderer.
    pub canonical_label: String,
    /// Resolved Concept Entity id for the left side. `None` when
    /// fuzzy-match didn't snap to an existing concept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_atom_id: Option<AtomId>,
    /// Raw left-side label from the typed-extension sketch. Always
    /// populated even when `left_atom_id` is present, so a reader
    /// sees the exact phrasing the section used.
    pub left_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_atom_id: Option<AtomId>,
    pub right_label: String,
    /// Axis along which the two sides differ. Empty when the section
    /// uses the opposition without naming the axis.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub axis: String,
    /// One-sentence statement of how the section uses this
    /// opposition argumentatively.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub framing: String,
    pub first_appearance: ChunkRef,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<String>,
    pub salience: f32,
    pub enrichment_depth: EnrichmentDepth,
}

// ── Asset (AD-2: described-asset substrate) ──────────────────

/// An opaque-bytes object — an email attachment, a folder-walked
/// binary, a calendar export, a transactions CSV — referenced from
/// the atom graph by content-hash. AD-2 of the architecture-over-Enron
/// push: the atom graph **stays prose-shaped** (no Table/Record/Series
/// variants); the Asset variant is a thin pointer at the
/// [`crate::asset_store::AssetStore`] entry, plus an optional link to
/// the [`Entity`] / [`Claim`] / future Document atom that holds the
/// **described** prose for the asset.
///
/// `described_by` is `None` when the dispatcher emitted only the
/// opaque-fallback description ("binary, 2.1MB, magic=outlook-pst")
/// without enrichment getting that far. `Some(atom_id)` once a
/// downstream atlas pass attaches a Document/Entity atom whose text is
/// the description.
///
/// Asset atoms are emitted by [`crate::extractors::described_asset`]
/// **at extraction time**, before atlas enrichment runs, and live in a
/// pre-merged sidecar (`atlas/asset_atoms.jsonl`). They are unioned
/// into `atoms.json` during the next atlas write. This crosses the
/// extractor→atom boundary deliberately — an Asset is a structural
/// fact about the corpus, not an inferential extraction from prose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: AtomId,
    /// SHA-256 hex of the raw bytes; the same value
    /// [`crate::asset_store::AssetStore::raw_path`] takes as input.
    pub sha256: String,
    /// Best-effort MIME type. `application/octet-stream` for the
    /// opaque-fallback case.
    pub mime: String,
    /// Original filename the asset was first observed with, when
    /// available. Empty when the source did not preserve a filename
    /// (raw binary stream).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub original_filename: String,
    /// Byte length of the raw payload.
    pub size: u64,
    /// `asset_kind` from the dispatcher: `"pdf"`, `"docx"`, `"xlsx"`,
    /// `"ical"`, `"opaque"`, …. The dispatcher's tag, not the MIME —
    /// MIME can be missing or wrong; `asset_kind` is what the
    /// sub-extractor self-identified as.
    pub asset_kind: String,
    /// Description atom for this asset, when emitted. See struct
    /// docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub described_by: Option<AtomId>,
    /// Path to the typed parsed cache (parquet for XLSX, ics for
    /// calendar, …). `None` for prose-shaped sub-extractors and the
    /// opaque fallback. Absolute path so the column-aware extractor
    /// (Phase 4) reads it directly without re-resolving the asset
    /// store root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parsed_form: Option<std::path::PathBuf>,
    /// First message / document that referenced this asset, by
    /// `source_doc_id`. Lets `sovereign atlas reconciliation-oplog`
    /// trace an attachment back to its first carrier.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub first_seen_source_doc_id: String,
    pub enrichment_depth: EnrichmentDepth,
}

impl Asset {
    /// Build a content-addressed atom id from the sha256 prefix.
    /// 16 hex chars (64 bits) — same collision budget as the other
    /// content-hash constructors.
    pub fn make_id(sha256: &str) -> AtomId {
        let short = sha256.get(0..16).unwrap_or(sha256);
        AtomId::from_raw(format!("asset-{short}"))
    }
}

// ── Atom envelope (on-disk shape per spec §6.2) ──────────────

/// Discriminated atom-type tag. Matches the `"atom_type"` string in
/// the on-disk JSON. Spec §2 enumerates the seven atom types; Step
/// 3a emits only `Entity` and `Event`.
///
/// `Hash` + `Ord` derives let downstream consumers (e.g.
/// `sovereign-tools::atlas_view`) key maps by atom type for counts
/// and grouping — trivially correct on a payload-free enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AtomType {
    Entity,
    Event,
    State,
    Relation,
    Claim,
    Question,
    Configuration,
    ArgumentReconstruction,
    Position,
    Opposition,
    /// AD-2: described-asset substrate. See [`Asset`].
    Asset,
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
    ArgumentReconstruction(ArgumentReconstruction),
    /// Gap-B typed-extension atom — see [`Position`].
    Position(Position),
    /// Gap-B typed-extension atom — see [`Opposition`].
    Opposition(Opposition),
    /// AD-2 described-asset atom — see [`Asset`].
    Asset(Asset),
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
            AtomEnvelope::ArgumentReconstruction(a) => &a.id,
            AtomEnvelope::Position(a) => &a.id,
            AtomEnvelope::Opposition(a) => &a.id,
            AtomEnvelope::Asset(a) => &a.id,
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
            AtomEnvelope::ArgumentReconstruction(a) => a.enrichment_depth,
            AtomEnvelope::Position(a) => a.enrichment_depth,
            AtomEnvelope::Opposition(a) => a.enrichment_depth,
            AtomEnvelope::Asset(a) => a.enrichment_depth,
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
    ///
    /// History:
    /// - `2.0` — initial typed-atom shape (Entity / Event / State /
    ///   Relation / Claim / Question / Configuration /
    ///   ArgumentReconstruction / Position / Opposition).
    /// - `2.1` — added `Asset` variant + `Attaches` edge kind
    ///   (architecture-over-Enron Phase 1; AD-2). Reader-side: every
    ///   non-Asset reader sees a non-`Asset` atom unchanged; an old
    ///   reader hitting an `Asset` envelope on disk fails loudly per
    ///   the deliberate "no `#[serde(other)]`" choice on AtomEnvelope.
    /// - `2.2` — added `Entity::provenance` field (AD-4;
    ///   architecture-over-Enron Phase 4). Old atoms.json deserialise
    ///   with a default empty `Provenance`.
    pub const SCHEMA_VERSION: &'static str = "2.2";

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

    // ── Move 6: content-hash atom id stability tests ──────

    #[test]
    fn entity_content_hash_is_stable_across_calls() {
        let a = AtomId::entity_content_hash("Albert Einstein", &EntityType::Person, "wikipedia");
        let b = AtomId::entity_content_hash("Albert Einstein", &EntityType::Person, "wikipedia");
        assert_eq!(a, b);
        assert_eq!(a.as_str().len(), "entity-".len() + 16);
        assert!(a.is_content_hash());
    }

    #[test]
    fn entity_content_hash_normalises_canonical_name() {
        // Lookup_key normalises case + punctuation; same key → same id.
        let a = AtomId::entity_content_hash("Albert Einstein", &EntityType::Person, "wikipedia");
        let b = AtomId::entity_content_hash("ALBERT-EINSTEIN", &EntityType::Person, "wikipedia");
        assert_eq!(a, b);
    }

    #[test]
    fn entity_content_hash_differs_across_corpora() {
        let a = AtomId::entity_content_hash("Albert Einstein", &EntityType::Person, "wikipedia");
        let b = AtomId::entity_content_hash("Albert Einstein", &EntityType::Person, "sep");
        assert_ne!(a, b);
    }

    #[test]
    fn entity_content_hash_differs_across_types() {
        let a = AtomId::entity_content_hash("Mercury", &EntityType::Place, "wikipedia");
        let b = AtomId::entity_content_hash("Mercury", &EntityType::Concept, "wikipedia");
        assert_ne!(a, b);
    }

    #[test]
    fn relation_content_hash_ignores_participant_order() {
        let p1 = AtomId::entity_content_hash("Alice", &EntityType::Person, "c");
        let p2 = AtomId::entity_content_hash("Bob", &EntityType::Person, "c");
        let a = AtomId::relation_content_hash(
            &[p1.clone(), p2.clone()],
            &crate::enrichment::pipeline::atlas::RelationType::Interpersonal,
            "married_to",
            "c",
        );
        let b = AtomId::relation_content_hash(
            &[p2, p1],
            &crate::enrichment::pipeline::atlas::RelationType::Interpersonal,
            "married_to",
            "c",
        );
        assert_eq!(a, b);
    }

    #[test]
    fn sequential_ids_are_not_content_hash() {
        assert!(!AtomId::entity(1).is_content_hash());
        assert!(!AtomId::event(42).is_content_hash());
    }

    #[test]
    fn content_hash_ids_pass_is_content_hash_check() {
        let id = AtomId::entity_content_hash("Test", &EntityType::Person, "c");
        assert!(id.is_content_hash());
    }

    #[test]
    fn all_atom_variants_have_content_hash_constructors() {
        // Smoke that every variant compiles + emits a content-hash-shaped id.
        use crate::enrichment::pipeline::atlas::*;
        let parent = AtomId::entity_content_hash("e", &EntityType::Person, "c");
        let ids = vec![
            AtomId::entity_content_hash("e", &EntityType::Person, "c"),
            AtomId::event_content_hash("d", &EventType::Action, "s0", "c"),
            AtomId::state_content_hash(&parent, &StateType::Epistemic, "l", "c"),
            AtomId::relation_content_hash(&[parent.clone()], &RelationType::Interpersonal, "l", "c"),
            AtomId::claim_content_hash("c", &DiscourseAct::Assert, &EpistemicStatus::Confident, "c"),
            AtomId::question_content_hash("q", &QuestionType::Thematic, "c"),
            AtomId::configuration_content_hash("cfg", "c"),
            AtomId::argument_reconstruction_content_hash("arg", "c"),
            AtomId::position_content_hash("pos", "endorse", "c"),
            AtomId::opposition_content_hash("X vs Y", "c"),
        ];
        for id in &ids {
            assert!(id.is_content_hash(), "expected content-hash shape: {}", id.as_str());
        }
        // All distinct (different prefixes + different inputs).
        let mut sorted: Vec<&str> = ids.iter().map(|a| a.as_str()).collect();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "ids must be distinct");
    }

    #[test]
    fn entity_content_hash_handles_unicode() {
        let a = AtomId::entity_content_hash("Søren Kierkegaard", &EntityType::Person, "sep");
        assert!(a.is_content_hash());
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
            defining_quote: None,
            salience: 0.92,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
                    provenance: Default::default(),
                    concept_kind: None,
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
            quotable_excerpt: None,
            attributed_to: Some(AtomId::entity(7)),
            confidence: Some(0.91),
            anchor: None,
            enrichment_depth: EnrichmentDepth::Extracted,
                    claim_kind: None,
            concession_outcome: None,
            evidence_kind: None,
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
    fn claim_anchor_round_trips_through_json() {
        // Pin the contract that `Claim.anchor` survives serialise →
        // deserialise. The drift-report renderer feeds `anchor` to
        // the cross-corpus fuzzy matcher; before this field existed
        // every normative claim landed in the critical "(no anchor)"
        // bucket because the matcher consulted the prose content
        // instead of the code symbol. This test exists so a future
        // refactor (e.g. switching the serde representation, dropping
        // the field "because it's optional") fails loudly here
        // rather than silently re-introducing the bug.
        use crate::enrichment::pipeline::atlas::{ClaimScope, DiscourseAct, EpistemicStatus};
        let claim = Claim {
            id: AtomId::claim(7),
            content: "`open_index_for_corpus` always opens `<index_dir>/<corpus_id>`.".into(),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: vec![],
            quotable_excerpt: None,
            attributed_to: None,
            confidence: None,
            anchor: Some("open_index_for_corpus".into()),
            enrichment_depth: EnrichmentDepth::Extracted,
                    claim_kind: None,
            concession_outcome: None,
            evidence_kind: None,
};
        let json = serde_json::to_string(&AtomEnvelope::Claim(claim.clone())).unwrap();
        assert!(
            json.contains("\"anchor\":\"open_index_for_corpus\""),
            "anchor field must serialise into the JSON envelope, got: {json}"
        );

        let back: AtomEnvelope = serde_json::from_str(&json).unwrap();
        match back {
            AtomEnvelope::Claim(c) => {
                assert_eq!(c.anchor.as_deref(), Some("open_index_for_corpus"));
            }
            _ => panic!("expected Claim"),
        }

        // Forward-compat: an atoms.json file written before this
        // field existed deserialises cleanly with `anchor = None`,
        // thanks to `#[serde(default)]`. Synthesise that legacy
        // shape and round-trip it.
        let legacy = r#"{"atom_type":"Claim","data":{
            "id":"claim-0001",
            "content":"legacy claim with no anchor field",
            "discourse_act":"assert",
            "epistemic_status":"confident",
            "scope":"fictional",
            "enrichment_depth":"extracted"
        }}"#;
        let parsed: AtomEnvelope = serde_json::from_str(legacy).unwrap();
        match parsed {
            AtomEnvelope::Claim(c) => assert!(c.anchor.is_none()),
            _ => panic!("expected Claim from legacy atoms.json shape"),
        }

        // And: when anchor is None we DO NOT emit the key (so the
        // file size doesn't grow for pre-engineering-atlas pipelines
        // that never set an anchor).
        let no_anchor = Claim {
            id: AtomId::claim(8),
            content: "no-anchor claim".into(),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Fictional,
            evidence: vec![],
            quotable_excerpt: None,
            attributed_to: None,
            confidence: None,
            anchor: None,
            enrichment_depth: EnrichmentDepth::Extracted,
                    claim_kind: None,
            concession_outcome: None,
            evidence_kind: None,
};
        let json = serde_json::to_string(&AtomEnvelope::Claim(no_anchor)).unwrap();
        assert!(
            !json.contains("\"anchor\""),
            "anchor=None must skip serialisation, got: {json}"
        );
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
        // SCHEMA_VERSION is the source of truth — the test pins
        // whatever the current value is so a future bump updates this
        // assertion automatically. The shape of `atoms` is what we
        // actually want to assert.
        let expected_ver = format!("\"schema_version\":\"{}\"", AtomsFile::SCHEMA_VERSION);
        assert!(
            json.contains(&expected_ver),
            "{json} should contain {expected_ver}"
        );
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
            defining_quote: None,
            salience: 0.1,
            enrichment_depth: EnrichmentDepth::Structural,
            affiliation: None,
            role: None,
            participants: Vec::new(),
                    provenance: Default::default(),
                    concept_kind: None,
};
        let env = AtomEnvelope::Entity(entity);
        assert_eq!(env.id().as_str(), "entity-0005");
        assert_eq!(env.enrichment_depth(), EnrichmentDepth::Structural);
    }
}
