// SPDX-License-Identifier: AGPL-3.0-or-later
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
    ///
    /// `Initiative` is added for the personal/conversational domains:
    /// a recurring abstract subject the user organises work around
    /// (a project, strategic priority, product launch). The pragmatic
    /// distinction from `Concept` is "active effort toward a future
    /// state" — `Initiative` implies someone is working on it.
    pub enum EntityType {
        Person = "person",
        Concept = "concept",
        Institution = "institution",
        Work = "work",
        Place = "place",
        Initiative = "initiative",
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

fn is_empty_str(s: &str) -> bool {
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
    /// Verbatim ≤200-char defining sentence from the source for
    /// `concept` entities — only populated when the section lifts a
    /// distinct "X is..." style definition. Empty otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defining_quote: Option<String>,
    /// 3–8 word keyphrase from the source text that introduces or
    /// establishes the atom. Used by a reviewer to grep back to the
    /// passage; replaced by a `ChunkRef` during Phase 5 resolution.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
    /// Declared-type attributes (ontology v1), keyed by the declared
    /// attribute name and validated by family in the parser. Empty for
    /// undeclared corpora and absent on the wire when empty, so cached
    /// section JSON re-serialises byte-identically.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub attributes: serde_json::Map<String, serde_json::Value>,
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
    /// Declared relation type name (ontology v1); `None` when the corpus
    /// declares none and classification stays deferred to Phase 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation_type: Option<String>,
    /// Declared-type attributes; see [`EntitySketch::attributes`].
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub attributes: serde_json::Map<String, serde_json::Value>,
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
    /// Declared event type name (ontology v1); `None` when the corpus
    /// declares none and classification stays deferred to Phase 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    /// Declared-type attributes; see [`EntitySketch::attributes`].
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

/// A knowledge-carrying act the text performs in this section.
///
/// `discourse_act` and `epistemic_status` are kept on the sketch
/// (exception to the defer-classification rule — see module doc) but
/// `scope` is deferred to Phase 5 since it's often resolvable from
/// cluster context — unless a declared claim type (ontology v1) fixes
/// it, in which case the sketch carries it.
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
    /// Verbatim ≤200-char excerpt from the source supporting this
    /// claim — only populated when a single quotable sentence in
    /// the section carries the claim. Empty otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quotable_excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
    /// Declared claim type name (ontology v1); `None` for the generic
    /// claim of an undeclared corpus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_kind: Option<String>,
    /// Name of the entity the claim is ABOUT (declared claim types with a
    /// `subject`), resolved to an atom id in Phase 3 the way
    /// `attributed_to` is. `attributed_to` is the voice; `subject` is the
    /// referent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Scope fixed by the declared claim type; `None` defers to Phase 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ClaimScope>,
    /// Declared-type attributes; see [`EntitySketch::attributes`].
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub attributes: serde_json::Map<String, serde_json::Value>,
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

/// A reconstruction of a named philosophical argument as it appears
/// in the section — the explicit premise→conclusion structure the
/// article gives. Targets the essay-judge axis "argument_depth"
/// where the binding constraint is the article presenting *the
/// reconstructed argument as text* even though the constituent
/// premises are scattered across paragraphs.
///
/// Optional, sparse — most sections do not contain a named argument.
/// Phase 1 extracts only when (a) the section names an argument and
/// (b) the article reconstructs its premise structure visibly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArgumentReconstructionSketch {
    /// Named argument as the article uses it ("Knowledge Argument",
    /// "Consequence Argument", "Function Argument", etc.).
    pub name: String,
    /// Philosopher/figure who originated the argument, by canonical
    /// name. Empty when the argument is article-voice or anonymous.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub proponent: String,
    /// Premises in order. Each step is a propositional-form
    /// statement of one premise (a paraphrase is acceptable —
    /// argument structure is the load-bearing axis here, not exact
    /// wording). 1-6 entries typical.
    pub premises: Vec<String>,
    /// Conclusion the premises support.
    pub conclusion: String,
    /// Named objections to this argument as the section presents
    /// them. Each entry pairs the objection's name with one
    /// expanded sentence of substance — the judge needs the latter
    /// for dialectical_breadth credit, not a bare name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objections: Vec<crate::enrichment::atlas::atoms::Objection>,
    /// 3-8 word keyphrase from the section text — same pattern as
    /// other sketches. Replaced by `ChunkRef`s during resolution.
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
#[derive(Default)]
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
    #[default]
    None,
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
    /// Reconstructed named arguments — sparse, optional. Each entry
    /// names an argument, its premises, conclusion, and objections.
    /// See `ArgumentReconstructionSketch` for the shape and the
    /// extraction discipline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argument_reconstructions: Vec<ArgumentReconstructionSketch>,
    /// **Legacy (v1)** — single typed-extension slot. Populated by
    /// the routed Phase 1 dispatcher in workstream B v1, when a
    /// section's classification was `ArgumentativeEssay` and the
    /// dispatcher attached an `ArgumentativeExtension`.
    ///
    /// **v2 replaces this with [`type_extensions`]** (plural) so a
    /// hybrid section (e.g. argumentative + narrative) can carry
    /// multiple typed extensions side-by-side. The plural field is
    /// canonical for new writes. Both fields are honoured by
    /// `has_no_atoms` and `atom_count`; legacy caches that still
    /// carry the singular field stay readable.
    ///
    /// New code should write to `type_extensions` and leave this
    /// `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_extension: Option<TypeExtension>,
    /// **v2 routed Phase 1 fan-out.** One entry per active discourse
    /// mode the Phase 0 classifier's vector surfaced above
    /// `DISCOURSE_ROUTING_THRESHOLD`. The dispatcher fires one chat
    /// call per active mode and pushes the parsed extension into this
    /// vector. Hybrid sections (Argumentative + Narrative @ 0.55/0.45)
    /// produce two entries.
    ///
    /// Each variant is unique per `discourse_mode` — the dispatcher
    /// rejects duplicates. Empty vector means the section didn't
    /// trigger any typed extension (pure literary / philosophy run,
    /// or a classification at every mode below threshold which is
    /// structurally impossible since primary ≥ 1/6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_extensions: Vec<TypeExtension>,
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
            && self.type_extension.is_none()
            && self.type_extensions.is_empty()
    }

    /// Total atom count across all typed fields, including any
    /// type extensions on either the legacy singular slot or the
    /// v2 plural slot.
    pub fn atom_count(&self) -> usize {
        let base = self.entities_introduced.len()
            + self.entities_developed.len()
            + self.relations_introduced.len()
            + self.relations_developed.len()
            + self.events.len()
            + self.claims.len()
            + self.questions_raised.len();
        let legacy_ext = self
            .type_extension
            .as_ref()
            .map(|e| e.atom_count())
            .unwrap_or(0);
        let ext: usize = self.type_extensions.iter().map(|e| e.atom_count()).sum();
        base + legacy_ext + ext
    }

    /// Visit every typed extension attached to this section,
    /// regardless of whether it sits on the legacy singular slot or
    /// the v2 plural slot. Order: plural entries first (in vector
    /// order), then the legacy singular (if any). Used by downstream
    /// consumers that need to enumerate extensions without caring
    /// about the slot.
    pub fn iter_type_extensions(&self) -> impl Iterator<Item = &TypeExtension> {
        self.type_extensions
            .iter()
            .chain(self.type_extension.iter())
    }
}

// ─── Type-specific extension atoms (routed Phase 1, workstream B) ─

/// Optional per-section payload populated by the routed-Phase-1
/// dispatcher (`obsidian_atlas`) based on the section's
/// classification. The literary / philosophy paths leave this `None`
/// and continue to populate only the legacy fields above —
/// `SectionExtraction` stays back-compat with every existing cache
/// file.
///
/// Each variant carries the atom shapes the section's genre
/// genuinely needs but the legacy schema cannot express. Argumentative
/// essays were the empirical driver — Phase 1 on `Pharmacy Benefit`
/// and `FIFA Financialized` produced 0 atoms under the literary
/// schema; the argumentative variant gives mechanisms, positions,
/// and evidence first-class slots so the claim-cap-of-10 stops being
/// the ceiling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TypeExtension {
    /// Argumentative essay: positions + mechanisms + evidence +
    /// oppositions + concessions. Populated when the Phase 0
    /// classifier's vector surfaces `DiscourseMode::Argumentative`
    /// above the routing threshold.
    Argumentative(ArgumentativeExtension),
    /// Narrative sections — story arcs, event sequences, character
    /// development. Populated when the discourse vector surfaces
    /// `DiscourseMode::Narrative`. v1 stub — atom shapes land in
    /// task #34 (`typed_schemas/narrative.rs`).
    Narrative(NarrativeExtension),
    /// Descriptive sections — definition cards, zettel notes,
    /// glossary entries, anatomical descriptions of institutions /
    /// systems. v1 stub.
    Descriptive(DescriptiveExtension),
    /// Reflective sections — first-person processing of experience,
    /// journal entries, diary-shaped notes. v1 stub.
    Reflective(ReflectiveExtension),
    /// Procedural sections — task lists, project plans, meeting
    /// recaps with action items, technical specs naming decisions
    /// and dependencies. v1 stub.
    Procedural(ProceduralExtension),
    /// Lyric sections — verse, prose poetry, spoken-word scripts
    /// classified by the section opening as Lyric. v1 stub.
    Lyric(LyricExtension),
}

impl TypeExtension {
    /// Atoms inside this extension. Used by `atom_count`.
    pub fn atom_count(&self) -> usize {
        match self {
            TypeExtension::Argumentative(a) => a.atom_count(),
            TypeExtension::Narrative(a) => a.atom_count(),
            TypeExtension::Descriptive(a) => a.atom_count(),
            TypeExtension::Reflective(a) => a.atom_count(),
            TypeExtension::Procedural(a) => a.atom_count(),
            TypeExtension::Lyric(a) => a.atom_count(),
        }
    }

    /// Which discourse mode this extension was routed from. Used by
    /// the dispatcher's duplicate-detection guard and by downstream
    /// modulator passes that filter extensions by mode.
    pub fn discourse_mode_tag(&self) -> &'static str {
        match self {
            TypeExtension::Argumentative(_) => "argumentative",
            TypeExtension::Narrative(_) => "narrative",
            TypeExtension::Descriptive(_) => "descriptive",
            TypeExtension::Reflective(_) => "reflective",
            TypeExtension::Procedural(_) => "procedural",
            TypeExtension::Lyric(_) => "lyric",
        }
    }
}

/// Argumentative-essay-specific atom shapes the literary schema
/// can't express cleanly. All fields are sparse — Phase 1 emits
/// only what the section actually carries; downstream consumers must
/// tolerate empty arrays.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ArgumentativeExtension {
    /// Named stances the section identifies and (typically) argues
    /// against or for. A position is a named *whole view* — "the
    /// markets-or-states framing", "Ostrom's third-pattern view",
    /// "the rent-concentration thesis" — distinct from any single
    /// claim that uses it. Empty when the section makes claims
    /// without naming the stance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positions: Vec<PositionSketch>,
    /// Named domain mechanisms the section operates with. These are
    /// Concepts in the entity sense too — but the schema surfaces
    /// them as first-class so the prompt can name them more
    /// generously without competing with the entity cap. Empty
    /// when the section's content is purely narrative or
    /// definition-focused.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mechanisms: Vec<MechanismSketch>,
    /// Specific evidence invoked — a study by name, a dollar figure,
    /// a regression coefficient, a historical example used to ground
    /// a claim. The point is to make the evidence-to-claim graph
    /// explicit so a downstream reader can audit "what does this
    /// claim rest on?" without re-reading.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_invocations: Vec<EvidenceInvocationSketch>,
    /// X-vs-Y framings the section sets up — "markets vs governments",
    /// "planting vs maintenance", "open access vs commons governance".
    /// Different from claims in that an opposition names the
    /// structural binary itself, not an assertion within it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub oppositions: Vec<OppositionSketch>,
    /// Author's "I grant X but ..." moves. Concessions are
    /// argumentative hygiene — they show the position is held
    /// against the strongest counter-statements the author can
    /// produce. Empty for sections that don't concede.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub concessions: Vec<ConcessionSketch>,
}

impl ArgumentativeExtension {
    pub fn atom_count(&self) -> usize {
        self.positions.len()
            + self.mechanisms.len()
            + self.evidence_invocations.len()
            + self.oppositions.len()
            + self.concessions.len()
    }
}

/// Named whole-view stance. Distinct from a Claim atom: a Position
/// is "the view that X" (a thing the section *names* and may
/// reference repeatedly); a Claim is "X" (a single assertion the
/// section makes).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PositionSketch {
    /// Reader-facing name for the stance. May be a coinage by the
    /// author ("the rent-concentration thesis") or a canonical
    /// label ("Hardin's tragedy framing").
    pub name: String,
    /// One-sentence statement of the position's content — what the
    /// view *says*.
    pub content: String,
    /// Entity name the position is attributed to. Empty when the
    /// position is article-voice or has no clear proponent in the
    /// section.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub proponent: String,
    /// Whether the section endorses, rebuts, or surveys this
    /// position. `endorse | rebut | survey | mixed`. Defaults
    /// `survey` when ambiguous — the safe read when the section's
    /// stance toward the position isn't clear from a single span.
    #[serde(default = "default_position_stance")]
    pub stance: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

fn default_position_stance() -> String {
    "survey".to_string()
}

/// A named mechanism the section operates with. Mechanisms describe
/// *how* something works in a domain — "spread pricing" names the
/// mechanism that lets PBMs extract; "salary cap" names the
/// competitive-balance lever the NFL uses; "regulatory capture"
/// names the failure mode an essay diagnoses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MechanismSketch {
    pub name: String,
    /// One-sentence description of how the mechanism works.
    pub description: String,
    /// Domain the mechanism comes from — "economics", "music",
    /// "biology", "urban planning", "law". Routed downstream so
    /// cross-corpus retrieval can scope by domain.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub domain: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

/// A specific piece of evidence the section invokes to ground a
/// claim. The point is auditability: a downstream reader should
/// be able to ask "where did this claim come from?" and get a
/// pointer to a study / figure / historical example.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidenceInvocationSketch {
    /// Short label for the evidence. Examples: `"Lin Chen redlining
    /// preprint"`, `"$1.4B FTC PBM spread"`, `"Soviet Aral Sea
    /// counter-example"`, `"NFL Green Bay survivability"`.
    pub label: String,
    /// What the evidence *is* — one sentence the prompt can extract
    /// verbatim from the section.
    pub content: String,
    /// Kind of evidence: `study`, `figure`, `historical_example`,
    /// `case_study`, `personal_anecdote`, `quotation`, `other`.
    /// Free-form; routing on it stays optional. Default `other`
    /// when ambiguous.
    #[serde(default = "default_evidence_kind")]
    pub kind: String,
    /// Claim or position the evidence is invoked to support. Empty
    /// when the section invokes evidence without binding it to a
    /// specific claim (rare but real — narrative-style evidence
    /// invocation).
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub supports: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

fn default_evidence_kind() -> String {
    "other".to_string()
}

/// An X-vs-Y framing the section sets up. `left` and `right` are
/// labels for the two sides; `axis` names the dimension along which
/// they differ. The axis can be empty for raw binary oppositions
/// the section doesn't formalise.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OppositionSketch {
    pub left: String,
    pub right: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub axis: String,
    /// One-sentence statement of how the section uses this
    /// opposition — what it's doing argumentatively.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub framing: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

/// Author's "I grant that X" move. A concession identifies a
/// counter-position the author treats seriously before pushing back
/// or accepting a bounded version of it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConcessionSketch {
    /// One-sentence statement of what the author concedes.
    pub content: String,
    /// Position or claim the concession addresses. Empty when the
    /// concession is unbound.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub addresses: String,
    /// Whether the concession ultimately leaves the author's view
    /// `intact`, `narrowed`, or `retracted`. Default `intact` —
    /// the most common shape: "I grant X is real BUT my point
    /// still stands."
    #[serde(default = "default_concession_outcome")]
    pub outcome: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

fn default_concession_outcome() -> String {
    "intact".to_string()
}

// ─── v2 typed-extension sketches per discourse mode ───────────────
//
// Each of the five non-argumentative discourse modes carries its own
// `Vec<…Sketch>` collections. The v1 landing keeps these struct
// definitions empty-shaped so the type system can switch over and the
// dispatcher can fan out; the concrete atom shapes + parsers + prompts
// land per-module under `typed_schemas/` (task #34).

/// Narrative discourse mode: events, entity-states, relations,
/// relation-states, participant arcs. Most fields mirror the literary
/// base schema's sketches — narrative routing exists so that a hybrid
/// section (e.g. a policy essay with a Wheeler-family opening) gets
/// the *event-arc* extracted even when the base argumentative
/// extractor would have dropped it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NarrativeExtension {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_states: Vec<EntityStateSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<RelationSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relation_states: Vec<RelationStateSketch>,
    /// Through-arcs for participants the section follows across
    /// multiple events — beyond a single state change. Free-form
    /// description per the literary atlas convention; resolution
    /// upgrades to structured arcs downstream.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participant_arcs: Vec<ParticipantArcSketch>,
}

impl NarrativeExtension {
    pub fn atom_count(&self) -> usize {
        self.events.len()
            + self.entity_states.len()
            + self.relations.len()
            + self.relation_states.len()
            + self.participant_arcs.len()
    }
}

/// Through-arc sketch for narrative routing. Names a participant + the
/// shape of their movement across the section ("from grief to
/// acceptance", "from outsider to insider"). Resolution turns these
/// into Arc atoms with start/end states.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParticipantArcSketch {
    pub participant: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

/// Descriptive discourse mode: definitions, property claims,
/// structural relationships, examples, provenance pointers. The
/// extractor that fires on `DiscourseMode::Descriptive` writes here
/// instead of (or alongside) the literary base schema's claims field.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DescriptiveExtension {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub definitions: Vec<DefinitionSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub property_claims: Vec<PropertyClaimSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationships: Vec<RelationSketch>,
    /// Concrete examples used to illustrate a definition / claim.
    /// Distinct from `evidence_invocations` on the argumentative
    /// extension — descriptive examples illustrate rather than
    /// support.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<ExampleSketch>,
    /// Source pointers — citations, footnotes, "as described in X"
    /// references. Empty when the section is voice-only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<ProvenanceSketch>,
}

impl DescriptiveExtension {
    pub fn atom_count(&self) -> usize {
        self.definitions.len()
            + self.property_claims.len()
            + self.relationships.len()
            + self.examples.len()
            + self.provenance.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DefinitionSketch {
    pub term: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PropertyClaimSketch {
    pub subject: String,
    pub property: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExampleSketch {
    pub label: String,
    pub content: String,
    /// Definition / claim / concept the example illustrates. Empty
    /// when ungrounded.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub illustrates: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvenanceSketch {
    /// Short label for the source — author + title, URL, or DOI-ish
    /// identifier.
    pub label: String,
    /// One sentence of context for the source.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub context: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

/// Reflective discourse mode: interactions with others / texts,
/// observations, open threads (questions or lines of thinking the
/// author leaves unresolved), mood shifts, realisations.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReflectiveExtension {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interactions: Vec<InteractionSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observations: Vec<ObservationSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_threads: Vec<OpenThreadSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mood_shifts: Vec<MoodShiftSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub realisations: Vec<RealisationSketch>,
}

impl ReflectiveExtension {
    pub fn atom_count(&self) -> usize {
        self.interactions.len()
            + self.observations.len()
            + self.open_threads.len()
            + self.mood_shifts.len()
            + self.realisations.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InteractionSketch {
    /// Named other(s) the author interacted with — colleague, friend,
    /// author of a text. Empty when the interaction is with an
    /// inanimate object or a non-personalised idea.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub with: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObservationSketch {
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpenThreadSketch {
    /// One sentence stating the unresolved thread — a question, a
    /// hunch the author can't yet pin down, a line of work to return
    /// to.
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MoodShiftSketch {
    /// Where the author started.
    pub from: String,
    /// Where the author ended.
    pub to: String,
    /// What moved them — the trigger / catalyst.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub catalyst: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RealisationSketch {
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

/// Procedural discourse mode: tasks (what will be done), decisions
/// (what was chosen), artifacts (the produced things), dependencies
/// (what blocks what), blockers (active obstacles), status signals
/// (progress markers).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProceduralExtension {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<DecisionSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<DependencySketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<BlockerSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status_signals: Vec<StatusSignalSketch>,
}

impl ProceduralExtension {
    pub fn atom_count(&self) -> usize {
        self.tasks.len()
            + self.decisions.len()
            + self.artifacts.len()
            + self.dependencies.len()
            + self.blockers.len()
            + self.status_signals.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskSketch {
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub owner: String,
    /// Free-form due-at hint — "by Thursday", "Q3", "before the merge
    /// freeze". The temporal modulator (task #36) may upgrade this to
    /// a structured date when context permits.
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub due_at: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionSketch {
    pub content: String,
    /// Alternatives considered and rejected. Empty when the decision
    /// is voiced without alternatives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtifactSketch {
    pub name: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub description: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DependencySketch {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlockerSketch {
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub blocks: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusSignalSketch {
    /// One of `done`, `in_progress`, `paused`, `cancelled`, `unknown`.
    /// Free-form to tolerate future expansion; routing on it stays
    /// optional.
    pub state: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

/// Lyric discourse mode: images, motifs, formal devices, voice shifts,
/// tonal movements. The atom shapes here are deliberately
/// expressive-domain — they don't try to recover argumentation or
/// fact-claims from verse.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LyricExtension {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub motifs: Vec<MotifSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formal_devices: Vec<FormalDeviceSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voice_shifts: Vec<VoiceShiftSketch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tonal_movements: Vec<TonalMovementSketch>,
}

impl LyricExtension {
    pub fn atom_count(&self) -> usize {
        self.images.len()
            + self.motifs.len()
            + self.formal_devices.len()
            + self.voice_shifts.len()
            + self.tonal_movements.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageSketch {
    pub content: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MotifSketch {
    pub name: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub description: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FormalDeviceSketch {
    /// Anaphora, enjambment, caesura, refrain, parallelism, etc.
    /// Free-form so the prompt can name a device without a fixed
    /// taxonomy.
    pub name: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub example: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceShiftSketch {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TonalMovementSketch {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub anchor: String,
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
            EntityType::Initiative,
        ] {
            let s = serde_json::to_string(&variant).unwrap();
            let parsed: EntityType = serde_json::from_str(&s).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn entity_type_initiative_round_trips_lowercase() {
        // Pin the wire form — `"initiative"` (lowercase, snake_case)
        // is the canonical literal the personal/conversational
        // entity-extraction prompts will emit.
        let parsed: EntityType = serde_json::from_str("\"initiative\"").unwrap();
        assert_eq!(parsed, EntityType::Initiative);
        let back = serde_json::to_string(&EntityType::Initiative).unwrap();
        assert_eq!(back, "\"initiative\"");
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
                attributes: Default::default(),
                canonical_name: "Alyosha".into(),
                aliases: vec!["Alyosha Karamazov".into(), "Alexei Fyodorovich".into()],
                entity_type: EntityType::Person,
                description: "Youngest Karamazov brother; novice at the monastery.".into(),
                anchor: "Alyosha knelt at the elder's feet".into(),
                defining_quote: None,
            }],
            entities_developed: vec![EntityStateSketch {
                entity_name: "Alyosha".into(),
                label: "Unshaken faith meeting the elder's imminent death".into(),
                anchor: "could not imagine the world without Zosima".into(),
            }],
            relations_introduced: vec![RelationSketch {
                attributes: Default::default(),
                relation_type: None,
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
                attributes: Default::default(),
                event_type: None,
                description: "Zosima instructs Alyosha to leave the monastery.".into(),
                participants: vec!["Zosima".into(), "Alyosha".into()],
                anchor: "go out into the world".into(),
            }],
            claims: vec![ClaimSketch {
                attributes: Default::default(),
                claim_kind: None,
                subject: None,
                scope: None,
                content: "Active love in reality is harder than the love one dreams of.".into(),
                discourse_act: DiscourseAct::Argue,
                epistemic_status: EpistemicStatus::Confident,
                attributed_to: Some("Zosima".into()),
                anchor: "love in dreams is greedy".into(),
                quotable_excerpt: None,
            }],
            questions_raised: vec![QuestionSketch {
                content: "Can a faith shaped in the cell survive the world outside?".into(),
                anchor: "faith in the cell".into(),
            }],
            argument_reconstructions: Vec::new(),
            type_extension: None,
            type_extensions: Vec::new(),
        };

        let json = serde_json::to_string_pretty(&extraction).unwrap();
        let parsed: SectionExtraction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, extraction);
        assert_eq!(parsed.atom_count(), 7);
        assert!(!parsed.has_no_atoms());
    }

    #[test]
    fn sketches_with_empty_ontology_fields_serialise_without_new_keys() {
        // Cached section JSON written before the ontology-v1 fields existed
        // must re-serialise byte-identically: every new field is absent on
        // the wire when empty.
        let entity = EntitySketch {
            canonical_name: "coin".into(),
            aliases: vec![],
            entity_type: EntityType::Concept,
            description: String::new(),
            defining_quote: None,
            anchor: String::new(),
            attributes: Default::default(),
        };
        let relation = RelationSketch {
            participants: vec![],
            label: "struck at".into(),
            anchor: String::new(),
            relation_type: None,
            attributes: Default::default(),
        };
        let event = EventSketch {
            description: "minted".into(),
            participants: vec![],
            anchor: String::new(),
            event_type: None,
            attributes: Default::default(),
        };
        let claim = ClaimSketch {
            content: "weighs 1.29 g".into(),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            attributed_to: None,
            quotable_excerpt: None,
            anchor: String::new(),
            claim_kind: None,
            subject: None,
            scope: None,
            attributes: Default::default(),
        };
        let keys = |v: serde_json::Value| -> Vec<String> {
            let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            keys
        };
        assert_eq!(
            keys(serde_json::to_value(&entity).unwrap()),
            ["canonical_name", "entity_type"]
        );
        assert_eq!(
            keys(serde_json::to_value(&relation).unwrap()),
            ["label", "participants"]
        );
        assert_eq!(keys(serde_json::to_value(&event).unwrap()), ["description"]);
        assert_eq!(
            keys(serde_json::to_value(&claim).unwrap()),
            ["content", "discourse_act", "epistemic_status"]
        );
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
