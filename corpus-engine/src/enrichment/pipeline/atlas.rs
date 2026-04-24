//! Atlas schema — v2.1 "unified atlas" atom and edge types.
//!
//! This module is the load-bearing vocabulary for the v2 enrichment
//! pipeline's successor schema. See `ENRICHMENT_V2.md` requirements
//! §2 (atom types) and §5.1 (Phase 1 `SectionExtraction`).
//!
//! # Scope of this landing
//!
//! Only the Phase 1 **sketch** types live here so far. Sketches are
//! intentionally minimal — they list atoms at the right granularity
//! plus a short `anchor` keyphrase so a reviewer can grep them out of
//! the source text. Classification work (what *kind* of state, event,
//! relation) is deferred to Phase 5 resolution, where the LLM reads
//! aligned passages with cluster context and can classify more
//! accurately than a single-section extraction can.
//!
//! The enum vocabulary (`EntityType`, `StateType`, `RelationType`,
//! `EventType`, `DiscourseAct`, `EpistemicStatus`, `QuestionType`,
//! `ClaimScope`) is published here but referenced only by atoms —
//! either by the Phase 1 sketches for the two fields that are genuinely
//! hard to recover later (`entity_type`, and claim's `discourse_act` +
//! `epistemic_status`) or by the resolved atom types that land in a
//! follow-on landing.
//!
//! Phase 1 sketches carry:
//!   - `SectionExtraction` — the record the LLM emits for one section.
//!   - Seven `*Sketch` structs — lightweight extraction records for
//!     entities, entity-states, relations, relation-states, events,
//!     claims, questions.
//!
//! The sketches carry names (not yet-assigned atom IDs) and short
//! anchors (not yet-resolved chunk refs). Phase 5 resolves sketches
//! from across sections into canonical `Entity`/`State`/… atoms with
//! chunk-level evidence.

use serde::{Deserialize, Serialize};

// ── Open/Closed surface: enrichment depth ─────────────────────
//
// Every atom carries an `enrichment_depth` tag that records which
// ingestion strategy produced it. The brief assembler reads this tag
// and calibrates language accordingly: "Robinson argued…" for
// `Extracted`, "according to her Wikipedia entry…" for `Structural`,
// "Robinson's article characterises…" for `StructuralClassified`.
//
// Today only `Extracted` is produced (literary_atlas pipeline). The
// other variants are wired in now so a future structure-first
// ingestion strategy can land without a schema revision.

/// Provenance depth of an atlas atom. The three values correspond to
/// the three ingestion modes described in the v2.1 spec §5:
///
/// - `Structural` — produced by deterministic parsing of structural
///   signals (infoboxes, section headers, wikilinks). Shallow but
///   broad; no LLM inference, no evidence grounding beyond section
///   boundaries.
/// - `Extracted` — produced by LLM extraction over the section body.
///   Deep and evidence-grounded; the default for authored works.
/// - `StructuralClassified` — `Structural` atoms that a later
///   classification pass has typed and characterised without
///   discovering new atoms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentDepth {
    Structural,
    Extracted,
    StructuralClassified,
}

impl EnrichmentDepth {
    /// Default for per-section extraction records — the
    /// literary/philosophy/journal pipelines all produce `Extracted`
    /// atoms. The structure-first ingestion strategy overrides this
    /// at construction time; it doesn't go through the serde default.
    pub const fn extracted_default() -> Self {
        EnrichmentDepth::Extracted
    }
}

impl Default for EnrichmentDepth {
    fn default() -> Self {
        EnrichmentDepth::extracted_default()
    }
}

/// Serde-level helper used by `#[serde(default = …)]` on
/// `SectionExtraction::enrichment_depth`. Using a function instead of
/// `Default::default` keeps the intent self-documenting at the call
/// site ("v1 caches deserialise as Extracted").
fn extracted_default() -> EnrichmentDepth {
    EnrichmentDepth::extracted_default()
}

// ── Enum vocabulary ───────────────────────────────────────────
//
// Each enum has a fixed set of "named" variants plus an `Other(String)`
// fallback. Serialization uses the snake_case name for the known
// variants and the inner string verbatim for `Other`, so a JSON
// response like `{"entity_type": "psychoanalyst"}` round-trips to
// `EntityType::Other("psychoanalyst".into())` without loss.
//
// The `from_str_repr` / `as_str_repr` helpers are the single source of
// truth for the string<->enum mapping; both Serialize and Deserialize
// route through them.

macro_rules! string_enum_with_other {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$variant_meta:meta])* $variant:ident = $lit:literal ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        $vis enum $name {
            $( $(#[$variant_meta])* $variant, )*
            /// Fallback for values the schema has not yet named. The
            /// inner string preserves whatever the model emitted so a
            /// reviewer can see which tags are recurring and decide
            /// whether to promote one to a named variant.
            Other(String),
        }

        impl $name {
            pub fn as_str_repr(&self) -> &str {
                match self {
                    $( Self::$variant => $lit, )*
                    Self::Other(s) => s.as_str(),
                }
            }

            pub fn from_str_repr(s: &str) -> Self {
                let t = s.trim();
                // Case-insensitive match on the named variants; any
                // whitespace / hyphen / underscore variation is
                // normalised to the canonical snake_case literal before
                // comparison. This keeps `"Psychological"`,
                // `"psychological"`, and `"Psycho-logical"` all mapping
                // to the same variant.
                let norm = normalise_enum_tag(t);
                $(
                    if norm == $lit {
                        return Self::$variant;
                    }
                )*
                Self::Other(t.to_string())
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str_repr())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Ok(Self::from_str_repr(&s))
            }
        }
    };
}

/// Normalise an enum tag coming off the wire into the canonical
/// snake_case literal. Lowercases, then maps hyphens / whitespace to
/// underscores, then collapses runs.
fn normalise_enum_tag(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut last_was_underscore = false;
    for c in lower.chars() {
        let mapped = match c {
            ' ' | '\t' | '-' | '/' => '_',
            c => c,
        };
        if mapped == '_' {
            if !last_was_underscore && !out.is_empty() {
                out.push('_');
                last_was_underscore = true;
            }
        } else {
            out.push(mapped);
            last_was_underscore = false;
        }
    }
    // Trim trailing underscore.
    if out.ends_with('_') {
        out.pop();
    }
    out
}

string_enum_with_other! {
    /// Kind of named thing (§2.1).
    pub enum EntityType {
        Person = "person",
        Concept = "concept",
        Institution = "institution",
        Work = "work",
        Place = "place",
    }
}

string_enum_with_other! {
    /// Kind of condition an entity or relation occupies (§2.2).
    ///
    /// `Relational` is intentionally absent — relation states are
    /// carried by the `Relation` atom's own trajectory (§2.6), not by
    /// a state-type variant on an entity.
    pub enum StateType {
        Psychological = "psychological",
        Epistemic = "epistemic",
        Social = "social",
        Professional = "professional",
    }
}

string_enum_with_other! {
    /// Kind of persistent interaction between entities (§2.6).
    pub enum RelationType {
        Interpersonal = "interpersonal",
        Intellectual = "intellectual",
        Institutional = "institutional",
        Adversarial = "adversarial",
        Collaborative = "collaborative",
        Compositional = "compositional",
    }
}

string_enum_with_other! {
    /// Kind of happening that marks a transition or grounds a claim (§2.3).
    pub enum EventType {
        Action = "action",
        Publication = "publication",
        Encounter = "encounter",
        Realization = "realization",
        ExternalForce = "external_force",
        Decision = "decision",
    }
}

string_enum_with_other! {
    /// What the text *does* with a claim — its illocutionary force (§2.5).
    pub enum DiscourseAct {
        Argue = "argue",
        Assert = "assert",
        Enact = "enact",
        Hypothesize = "hypothesize",
        Warn = "warn",
        Commit = "commit",
        Object = "object",
        Interpret = "interpret",
        Imply = "imply",
    }
}

string_enum_with_other! {
    /// How certain a claim is within the corpus context (§2.5).
    pub enum EpistemicStatus {
        Confident = "confident",
        Tentative = "tentative",
        Contested = "contested",
        Retracted = "retracted",
        Attributed = "attributed",
    }
}

string_enum_with_other! {
    /// How wide a claim's applicability is (§2.5).
    pub enum ClaimScope {
        Universal = "universal",
        Contextual = "contextual",
        Personal = "personal",
        Fictional = "fictional",
    }
}

string_enum_with_other! {
    /// Kind of inquiry a question represents (§2.4).
    pub enum QuestionType {
        Thematic = "thematic",
        Factual = "factual",
        Interpretive = "interpretive",
        Open = "open",
        Rhetorical = "rhetorical",
    }
}

// ── Sketch types (Phase 1 output — per-section records) ───────
//
// Sketches are pre-resolution: they carry *names* rather than atom IDs
// and a short `anchor` keyphrase rather than a ChunkRef. Phase 5 will
// merge sketches across sections into canonical atoms with chunk-level
// evidence and classify each sketch into its type (psychological vs
// social state, decision vs encounter, etc.) using cluster context that
// the single-section extractor doesn't have.
//
// The two exceptions to "classify later" live on claims and entities.
// `entity_type` (person/concept/institution/work/place) is a surface-
// feature check that's cheap to make during extraction and helpful for
// early routing. `discourse_act` + `epistemic_status` are load-bearing
// downstream and lose fidelity once the claim is lifted from its
// passage into a cluster.

fn is_empty_str(s: &String) -> bool {
    s.is_empty()
}

/// A named thing that appears in the section.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntitySketch {
    pub canonical_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub entity_type: EntityType,
    /// One-sentence characterisation derived from the section itself —
    /// not from external knowledge. A routing aid for clustering, not
    /// a representation of the entity's full meaning (requirements
    /// §1.2, Wittgenstein note).
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub description: String,
    /// 3–8 word keyphrase from the source text that introduces or
    /// establishes the atom. Used by a reviewer to grep back to the
    /// passage; replaced by a `ChunkRef` during Phase 5 resolution.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

/// A state an entity occupies during this section.
///
/// Classification of the state (psychological / epistemic / social /
/// professional) is deferred to Phase 5 resolution, where cluster
/// context makes the call more accurate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntityStateSketch {
    /// Name of the entity this state belongs to. Resolved against the
    /// entity sketches (this section + prior sections) in Phase 5.
    pub entity_name: String,
    /// Concise description of the state.
    pub label: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

/// A persistent interaction between entities, introduced in this
/// section. Relation-type classification is deferred to Phase 5.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelationSketch {
    /// Names of the participating entities. Ordered when the relation
    /// is asymmetric (mentor → mentee, employer → employee); order is
    /// informational, not mechanical.
    pub participants: Vec<String>,
    /// Concise label for the relationship.
    pub label: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

/// A state a relation occupies during this section.
///
/// Distinct from `EntityStateSketch`: this is a state of the
/// *interaction between* entities (e.g. "adversarial testing" between
/// Jane and Rochester), not a state of any single participant. State-
/// type classification is deferred to Phase 5.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelationStateSketch {
    pub participants: Vec<String>,
    pub label: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

/// Something that happens in this section and marks a transition,
/// creates a relation, or grounds a claim. Event-type classification
/// is deferred to Phase 5.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventSketch {
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<String>,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

/// A knowledge-carrying act the text performs in this section.
///
/// `discourse_act` and `epistemic_status` are kept on the sketch
/// (exception to the defer-classification rule — see module doc) but
/// `scope` is deferred to Phase 5 since it's often resolvable from
/// cluster context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimSketch {
    /// The claim in propositional form.
    pub content: String,
    pub discourse_act: DiscourseAct,
    pub epistemic_status: EpistemicStatus,
    /// Name of the entity who holds or articulates this claim, if
    /// attributable. `None` means the claim is made by the text
    /// itself (narrator, article author, or enacted through
    /// structure).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributed_to: Option<String>,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

/// A question this section raises. Question-type classification
/// (thematic / factual / interpretive / open / rhetorical) is deferred
/// to Phase 5. Cross-section question advancement tracking moves to
/// Phase 2's embedding clustering, which recovers those links more
/// reliably than a single-section extractor can.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuestionSketch {
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

// ── Stage 1a — Seed entities ─────────────────────────────────
//
// The pipeline's first LLM call (or structural-parse pass) extracts
// a seed list of canonical entity names from the opening section.
// Every subsequent Stage 1b map call receives this seed list so
// chapter-level extractions resolve pronouns and alias variants
// against a stable set of canonical names instead of inventing
// them per chapter.
//
// The fragmentation we saw in the Step 3a smoke run (Fyodor
// Karamазов / Fyodor Pavlovich Karamazov / Fyo Karamzоv as three
// separate entity atoms) is the direct cost of NOT doing this —
// each chapter call had no knowledge of what canonical form the
// rest of the corpus would use. Stage 1a fixes the problem at
// the source.

/// One entry in the seed entity list. Intentionally identical in
/// shape to `EntitySketch` so downstream Stage 2 entity resolution
/// can treat seeds and sketches uniformly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SeedEntity {
    pub canonical_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub entity_type: EntityType,
    /// One-sentence characterisation drawn from the seed section
    /// itself. Routing aid for downstream map calls — they use this
    /// to confirm the referent before emitting an entity mention.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub description: String,
}

/// Top-level seed list for a corpus. Written to `cache/seed.json`
/// after Stage 1a and read by Stage 1b map workers.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SeedEntities {
    pub schema_version: u32,
    pub corpus_id: String,
    /// Source of the seed list — identifies whether an LLM call,
    /// a structural parse (wikilinks / infobox), or a no-op
    /// produced this record. Persisted so the operator can tell
    /// at a glance which kind of seed is in play.
    pub origin: SeedOrigin,
    pub entries: Vec<SeedEntity>,
    pub written_at: String,
}

impl SeedEntities {
    pub const SCHEMA_VERSION: u32 = 1;
}

/// Where a seed list came from. Mirrors `SeedStrategy` on the
/// Pipeline trait — the strategy names the capability, this enum
/// names the concrete source for an already-produced record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedOrigin {
    /// Produced by a one-shot LLM call over the corpus's first
    /// section (e.g. `LiteraryAtlasPipeline::compose_seed_prompt`).
    Llm,
    /// Produced by a deterministic parse of structural signals a
    /// reference corpus already carries (article title + lead-
    /// section wikilinks for Wikipedia; infobox for Wikidata-backed
    /// corpora).
    Structural,
    /// The pipeline does not need a seed list. This variant is
    /// included so a `SeedEntities::empty_for(corpus_id)` can be
    /// written to disk even for no-seed pipelines, keeping the
    /// cache-file contract uniform.
    None,
}

impl Default for SeedOrigin {
    fn default() -> Self {
        SeedOrigin::None
    }
}

/// How a pipeline produces its seed entity list. The runner
/// dispatches on this enum to decide whether to call
/// `compose_seed_prompt` (Llm), `extract_seed_structural`
/// (Structural), or skip Stage 1a entirely (None).
///
/// Lives here rather than on `trait_def.rs` because the type is
/// part of the public atlas vocabulary that other crates (the
/// CLI, future domain pipelines) consume directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedStrategy {
    /// One-shot LLM call on the corpus's first section produces
    /// the seed list. See `Pipeline::compose_seed_prompt` +
    /// `parse_seed_response`.
    Llm,
    /// Deterministic structural parse produces the seed list. See
    /// `Pipeline::extract_seed_structural`.
    Structural,
    /// No seed list needed. Stage 1b map calls run without seed
    /// context. Use this for ad-hoc / fragment corpora where
    /// chapter-level extraction without context is acceptable.
    None,
}

/// Phase 1 extraction record for one section.
///
/// The domain prompt controls which fields are populated. Fields the
/// domain doesn't populate remain empty — no phantom atoms are
/// generated. A literary prompt emphasises `entities_*`,
/// `relations_*` and `events`; a philosophy prompt emphasises
/// `claims` and `questions_raised`; a journal prompt emphasises
/// entity-state and relation-state trajectories of the author.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SectionExtraction {
    pub section_id: String,
    /// Which ingestion strategy produced this section's atoms. All
    /// sketches inside a single `SectionExtraction` share the same
    /// depth — an LLM extraction over a full section produces
    /// `Extracted` atoms; a structural parse produces `Structural`.
    /// Default = `Extracted` so v1 cache files written before this
    /// field existed continue to parse without migration.
    #[serde(default = "extracted_default")]
    pub enrichment_depth: EnrichmentDepth,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities_introduced: Vec<EntitySketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities_developed: Vec<EntityStateSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations_introduced: Vec<RelationSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations_developed: Vec<RelationStateSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<ClaimSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions_raised: Vec<QuestionSketch>,
}

impl SectionExtraction {
    /// True when the extraction contains no atoms of any kind. Used
    /// by validation to distinguish "the LLM extracted nothing"
    /// (parse-quality bug) from sparse-but-real extractions.
    pub fn has_no_atoms(&self) -> bool {
        self.entities_introduced.is_empty()
            && self.entities_developed.is_empty()
            && self.relations_introduced.is_empty()
            && self.relations_developed.is_empty()
            && self.events.is_empty()
            && self.claims.is_empty()
            && self.questions_raised.is_empty()
    }

    /// Total atom count across all typed fields.
    pub fn atom_count(&self) -> usize {
        self.entities_introduced.len()
            + self.entities_developed.len()
            + self.relations_introduced.len()
            + self.relations_developed.len()
            + self.events.len()
            + self.claims.len()
            + self.questions_raised.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_type_roundtrips_named_variants() {
        for variant in [
            EntityType::Person,
            EntityType::Concept,
            EntityType::Institution,
            EntityType::Work,
            EntityType::Place,
        ] {
            let s = serde_json::to_string(&variant).unwrap();
            let parsed: EntityType = serde_json::from_str(&s).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn entity_type_unknown_lands_in_other() {
        let parsed: EntityType = serde_json::from_str("\"deity\"").unwrap();
        assert_eq!(parsed, EntityType::Other("deity".into()));
        // Round-trips back as-is.
        let back = serde_json::to_string(&parsed).unwrap();
        assert_eq!(back, "\"deity\"");
    }

    #[test]
    fn enum_tag_normalisation_is_case_insensitive_and_punctuation_tolerant() {
        // Model emits "Psychological" — we accept it.
        assert_eq!(
            StateType::from_str_repr("Psychological"),
            StateType::Psychological
        );
        // Model emits "External Force" — maps to ExternalForce.
        assert_eq!(
            EventType::from_str_repr("External Force"),
            EventType::ExternalForce
        );
        // Hyphen variant — maps.
        assert_eq!(
            EventType::from_str_repr("external-force"),
            EventType::ExternalForce
        );
        // Leading/trailing whitespace — trimmed.
        assert_eq!(
            DiscourseAct::from_str_repr("  argue  "),
            DiscourseAct::Argue
        );
    }

    #[test]
    fn section_extraction_roundtrips_full_payload() {
        let extraction = SectionExtraction {
            section_id: "ch_0013".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            entities_introduced: vec![EntitySketch {
                canonical_name: "Alyosha".into(),
                aliases: vec!["Alyosha Karamazov".into(), "Alexei Fyodorovich".into()],
                entity_type: EntityType::Person,
                description: "Youngest Karamazov brother; novice at the monastery.".into(),
                anchor: "Alyosha knelt at the elder's feet".into(),
            }],
            entities_developed: vec![EntityStateSketch {
                entity_name: "Alyosha".into(),
                label: "Unshaken faith meeting the elder's imminent death".into(),
                anchor: "could not imagine the world without Zosima".into(),
            }],
            relations_introduced: vec![RelationSketch {
                participants: vec!["Alyosha".into(), "Zosima".into()],
                label: "Novice-elder bond — spiritual formation".into(),
                anchor: "the elder laid his hand".into(),
            }],
            relations_developed: vec![RelationStateSketch {
                participants: vec!["Dmitri".into(), "Fyodor".into()],
                label: "Adversarial rivalry over Grushenka".into(),
                anchor: "glared past one another".into(),
            }],
            events: vec![EventSketch {
                description: "Zosima instructs Alyosha to leave the monastery.".into(),
                participants: vec!["Zosima".into(), "Alyosha".into()],
                anchor: "go out into the world".into(),
            }],
            claims: vec![ClaimSketch {
                content: "Active love in reality is harder than the love one dreams of.".into(),
                discourse_act: DiscourseAct::Argue,
                epistemic_status: EpistemicStatus::Confident,
                attributed_to: Some("Zosima".into()),
                anchor: "love in dreams is greedy".into(),
            }],
            questions_raised: vec![QuestionSketch {
                content: "Can a faith shaped in the cell survive the world outside?".into(),
                anchor: "faith in the cell".into(),
            }],
        };

        let json = serde_json::to_string_pretty(&extraction).unwrap();
        let parsed: SectionExtraction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, extraction);
        assert_eq!(parsed.atom_count(), 7);
        assert!(!parsed.has_no_atoms());
    }

    #[test]
    fn section_extraction_carries_extracted_depth_by_default() {
        // A v1-shaped cache file predates the enrichment_depth field.
        // It must still deserialise, and the loaded record must land
        // at `Extracted` so existing corpora don't quietly downgrade
        // to `Structural`.
        let v1_json = r#"{
          "section_id": "sec_0001",
          "questions_raised": [{"content": "Why?"}]
        }"#;
        let parsed: SectionExtraction = serde_json::from_str(v1_json).unwrap();
        assert_eq!(parsed.enrichment_depth, EnrichmentDepth::Extracted);
    }

    #[test]
    fn section_extraction_roundtrip_preserves_explicit_structural_depth() {
        // When a future structure-first ingestion strategy writes
        // sections at `Structural` depth, the tag must round-trip
        // exactly — not silently fall back to the default.
        let original = SectionExtraction {
            section_id: "wiki-joan-robinson".into(),
            enrichment_depth: EnrichmentDepth::Structural,
            questions_raised: vec![QuestionSketch {
                content: "What did she argue about capital?".into(),
                anchor: String::new(),
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&original).unwrap();
        assert!(
            json.contains("\"enrichment_depth\":\"structural\""),
            "expected snake_case serialisation, got: {json}"
        );
        let parsed: SectionExtraction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.enrichment_depth, EnrichmentDepth::Structural);
    }

    #[test]
    fn enrichment_depth_variants_serialize_as_snake_case() {
        // Pin the wire format — other tooling (manifest summariser,
        // schema_validation.json writer, future structure-first
        // adapter) reads these strings directly.
        assert_eq!(
            serde_json::to_string(&EnrichmentDepth::Structural).unwrap(),
            "\"structural\""
        );
        assert_eq!(
            serde_json::to_string(&EnrichmentDepth::Extracted).unwrap(),
            "\"extracted\""
        );
        assert_eq!(
            serde_json::to_string(&EnrichmentDepth::StructuralClassified).unwrap(),
            "\"structural_classified\""
        );
    }

    #[test]
    fn section_extraction_accepts_minimal_payload() {
        // A philosophy-only section: claims + questions, no entities.
        let json = r#"{
          "section_id": "sec_0001",
          "claims": [{
            "content": "Free will is compatible with causal determinism.",
            "discourse_act": "argue",
            "epistemic_status": "contested"
          }],
          "questions_raised": [{
            "content": "Does moral responsibility require indeterminism?"
          }]
        }"#;
        let parsed: SectionExtraction = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.claims.len(), 1);
        assert_eq!(parsed.questions_raised.len(), 1);
        assert!(parsed.entities_introduced.is_empty());
        assert_eq!(parsed.atom_count(), 2);
    }

    #[test]
    fn entity_sketch_tolerates_unknown_enum_tags() {
        // A model improvises "deity" for entity_type — the
        // Other(String) variant preserves it for downstream review.
        let json = r#"{
          "canonical_name": "Grace",
          "entity_type": "deity",
          "description": "Personified force in the text"
        }"#;
        let parsed: EntitySketch = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.entity_type, EntityType::Other("deity".into()));
    }

    #[test]
    fn empty_extraction_reports_no_atoms() {
        let e = SectionExtraction {
            section_id: "sec_0099".into(),
            ..Default::default()
        };
        assert!(e.has_no_atoms());
        assert_eq!(e.atom_count(), 0);
    }

    #[test]
    fn claim_sketch_requires_discourse_act_and_epistemic_status() {
        // Both fields are load-bearing — they're exceptions to the
        // "defer classification to Phase 5" rule because they lose
        // fidelity once the claim is lifted from its passage.
        let no_act = r#"{"content":"X","epistemic_status":"confident"}"#;
        let err = serde_json::from_str::<ClaimSketch>(no_act).unwrap_err();
        assert!(err.to_string().contains("discourse_act"));

        let no_status = r#"{"content":"X","discourse_act":"argue"}"#;
        let err = serde_json::from_str::<ClaimSketch>(no_status).unwrap_err();
        assert!(err.to_string().contains("epistemic_status"));
    }

    #[test]
    fn enum_vocabulary_covers_spec_taxonomies() {
        // Pin the spec §2 taxonomy. These enums are published here
        // but not all are referenced by Phase 1 sketches — StateType,
        // EventType, RelationType, ClaimScope, QuestionType are used
        // during Phase 5 resolution. This test keeps the vocabulary
        // live against dead-code elimination.
        let _ = StateType::Psychological;
        let _ = EventType::Decision;
        let _ = RelationType::Interpersonal;
        let _ = ClaimScope::Universal;
        let _ = QuestionType::Thematic;
    }

    #[test]
    fn normalise_enum_tag_collapses_punctuation() {
        assert_eq!(normalise_enum_tag("External Force"), "external_force");
        assert_eq!(normalise_enum_tag("external-force"), "external_force");
        assert_eq!(normalise_enum_tag(" external  force "), "external_force");
        assert_eq!(normalise_enum_tag("psychological"), "psychological");
    }
}
