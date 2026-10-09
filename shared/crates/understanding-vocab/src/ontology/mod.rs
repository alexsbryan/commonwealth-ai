// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ontology policies — the version-independent interface between a recipe's
//! `[enrichment.ontology]` block and the enrichment pipeline, as DATA.
//!
//! Five axes plus prose (`ONTOLOGY_PRIMITIVES.md` §0.1, §2): what exists
//! (shape), what is said (assertion), what is the same (identity), what
//! changes (change), what follows (derivation) — and, since ei-2-map, the
//! map's third role, how to walk it (`navigation`, `EPISTEMIC_INDEX.md`
//! §2.2). The composer, parser, resolver, tension selector, supersession
//! fold, build report and inspector read [`OntologyPolicies`] and nothing
//! else; the recipe's `version` selects
//! a declaration language (corpus-engine's `recipe_ontology::language`) that
//! parses TOML into these structs and is never consulted again. That is what
//! makes a version 2 cheap.
//!
//! Version 0 (the prose block) fills `prose` and leaves every other axis at
//! its default. Those defaults ARE today's behaviour — `configurations` on,
//! `arguments` off, the document-date clock, the five generic vocabulary
//! terms — and corpus-engine's `defaults_reproduce_today` pins them.
//!
//! The policy structs derive `Deserialize` for the JSON round trip an atlas
//! records (`atlas/ontology.json`), not because an author writes them. What
//! an author writes is in [`decl`] — and both are here, in the leaf, so a
//! host that wants to know what a corpus DECLARED can read it without
//! linking the engine that extracted to it (enrichment-as-plugin Step 3).

pub mod decl;
pub mod derived;
pub mod navigation;
pub mod source;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use decl::{
    DocumentFieldsDecl, OntologyTypeDecl, OntologyVocabulary, PatternDecl, SupersessionClock,
    TensionDecl, TypeKind, VoicesDecl,
};
pub use navigation::{
    NavigationPolicy, QuestionKind, SeedPolicy, SummarySource, WalkPolicy, DEFAULT_BUDGET,
    SUMMARY_SEED_BUDGET,
};

/// Epistemic vocabulary for one pipeline (scaffold §8.1 of the spec).
/// Lives on the `Pipeline` trait; the CLI prints these in `show`
/// headers and the `query` LOCATE output so the terminology matches
/// the domain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Vocabulary {
    pub canonical_concern_term: String,
    pub position_term: String,
    pub tension_term: String,
    pub absence_term: String,
    /// What a single piece of grounding evidence is called
    /// ("paragraph", "passage", "snippet").
    pub evidence_term: String,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// How declared document reading asks its model (ONTOLOGY_METHOD §Reading).
/// Both readers emit the same `DocumentRead`, so validation, the section
/// cache and everything downstream are shared; only the asking differs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentReader {
    /// One generation per chapter: find, label, name and cite at once.
    #[default]
    OneShot,
    /// A fixed plan of small closed questions generated from the contract,
    /// each answered as a distribution in one forward pass.
    Passes,
}

impl DocumentReader {
    fn is_one_shot(&self) -> bool {
        *self == Self::OneShot
    }
}

/// Everything the pipeline reads from a declared ontology. Every field has a
/// default; the default of the whole is "no ontology" (`is_empty`), and a
/// prose-only version-0 block differs from it in `prose` alone.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OntologyPolicies {
    /// What exists: the declared types with their attributes, subtypes,
    /// roles, endpoints, sources and labels.
    #[serde(default)]
    pub shape: ShapePolicy,
    /// Select the accountable declared-types-only Phase-1 document reader.
    /// Default false preserves the existing reader.
    #[serde(default, skip_serializing_if = "is_false")]
    pub document_reading: bool,
    /// How that reading asks; meaningful only with `document_reading`.
    #[serde(default, skip_serializing_if = "DocumentReader::is_one_shot")]
    pub document_reader: DocumentReader,
    /// What a source says: who speaks, and what the corpus must never do.
    /// Per-claim-type facets (force, deontic, subject, grades, anchors, scope)
    /// live on the [`OntologyTypeDecl`] of kind `claim` — see
    /// [`Self::claim_types`].
    #[serde(default)]
    pub assertion: AssertionPolicy,
    /// When two are one: per-type identity keys.
    #[serde(default)]
    pub identity: IdentityPolicy,
    /// What holds when: the clock and which claim types supersede.
    #[serde(default)]
    pub change: ChangePolicy,
    /// What the system infers: tension, patterns, the opt-in passes.
    #[serde(default)]
    pub derivation: DerivationPolicy,
    /// The author's prose and vocabulary terms.
    #[serde(default)]
    pub prose: ProsePolicy,
    /// How a reader walks this atlas, per question kind. Defaults to the
    /// spec's pre-registered table; not an axis of what the corpus SAYS, so
    /// it bumps no declaration version (`ONTOLOGY_PRIMITIVES.md` §0.1: an
    /// additive field with a default). Nothing reads it before the walker.
    #[serde(default)]
    pub navigation: NavigationPolicy,
}

/// Axis 1 — what a thing is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ShapePolicy {
    /// The declared types, in declaration order (a `Vec`, not a map, so the
    /// prompt bytes composed from it are deterministic).
    #[serde(default)]
    pub types: Vec<OntologyTypeDecl>,
    /// How many entities one section may introduce, when the author set one.
    /// `None` leaves the shipped Phase-1 schema's own cap in place — the
    /// literal there is the default, and it is not restated here. The range
    /// is held at the recipe boundary
    /// ([`decl::MIN_ENTITIES_PER_SECTION`]..=[`decl::MAX_ENTITIES_PER_SECTION`]).
    #[serde(default)]
    pub max_entities_per_section: Option<usize>,
}

/// Axis 2 — what a source says, at corpus level.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AssertionPolicy {
    /// Who speaks, and which speakers are not subject matter.
    #[serde(default)]
    pub voices: VoicesDecl,
    /// What the corpus must never be used for.
    #[serde(default)]
    pub must_not: Vec<String>,
}

/// Axis 3 — when two are one. Keyed by declared type name; a type absent
/// from both maps resolves on its canonical name (the reported default).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IdentityPolicy {
    /// Type → external identifiers. An external key merges strictly.
    #[serde(default)]
    pub identity: BTreeMap<String, Vec<String>>,
    /// Type → descriptive keys used when the identifier is absent. A
    /// descriptive key is judged.
    #[serde(default)]
    pub identity_fallback: BTreeMap<String, Vec<String>>,
}

/// Axis 4 — what holds when.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChangePolicy {
    /// The clock supersession folds on. Defaults to `document_date`.
    #[serde(default)]
    pub clock: SupersessionClock,
    /// Claim type → `"document_date"` or the time attribute it supersedes on.
    #[serde(default)]
    pub supersedes: BTreeMap<String, String>,
    /// The per-document metadata fields stamped onto every claim. `None`
    /// stamps nothing; absent on the wire then, so older `ontology.json`
    /// files read and write unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<DocumentFieldsDecl>,
}

/// The claim attribute the document-date clock reads. Stamped from the
/// field `change.document.date` names; read by the supersession fold and
/// the tension `clock` field.
pub const DOCUMENT_DATE_ATTR: &str = "document_date";
/// The claim attribute carrying the thread of the claim's own document.
pub const DOCUMENT_THREAD_ATTR: &str = "document_thread";
/// The claim attribute carrying the identifier of the claim's own document.
pub const DOCUMENT_ID_ATTR: &str = "document_id";
/// The entity attribute a metadata `source` writes on each atom it projects:
/// how many documents carry its identity value.
pub const DOCUMENT_COUNT_ATTR: &str = "document_count";

/// One stamp `change.document` can declare. Closed: the declaration's three
/// keys, each mapped to the reserved claim attribute it writes — the one
/// table the validator and the resolver both read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DocumentStamp {
    Date,
    Thread,
    Id,
}

impl DocumentStamp {
    pub const ALL: [DocumentStamp; 3] = [Self::Date, Self::Thread, Self::Id];

    /// The claim attribute this stamp writes.
    pub const fn attr(self) -> &'static str {
        match self {
            Self::Date => DOCUMENT_DATE_ATTR,
            Self::Thread => DOCUMENT_THREAD_ATTR,
            Self::Id => DOCUMENT_ID_ATTR,
        }
    }

    /// The stamp writing claim attribute `attr`, if one does.
    pub fn from_attr(attr: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.attr() == attr)
    }

    /// The `change.document` key that declares it.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::Thread => "thread",
            Self::Id => "id",
        }
    }
}

impl DocumentFieldsDecl {
    /// The metadata field the author named for `stamp`, if any.
    pub fn field(&self, stamp: DocumentStamp) -> Option<&str> {
        match stamp {
            DocumentStamp::Date => self.date.as_deref(),
            DocumentStamp::Thread => self.thread.as_deref(),
            DocumentStamp::Id => self.id.as_deref(),
        }
    }

    /// Every declared `(stamp, field)` pair, in [`DocumentStamp::ALL`] order.
    pub fn declared(&self) -> impl Iterator<Item = (DocumentStamp, &str)> {
        DocumentStamp::ALL
            .into_iter()
            .filter_map(|s| self.field(s).map(|f| (s, f)))
    }
}

/// Axis 5 — what the system infers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerivationPolicy {
    /// Which claim types can conflict and what makes two comparable.
    #[serde(default)]
    pub tension: TensionDecl,
    /// Graph patterns over declared relation/event types.
    #[serde(default)]
    pub patterns: Vec<PatternDecl>,
    /// Run the interpretive-configuration rollups. Default `true` (today).
    #[serde(default = "default_true")]
    pub configurations: bool,
    /// Reconstruct arguments. Default `false` (today).
    #[serde(default)]
    pub arguments: bool,
    /// Derived attributes: the declared paths, sets and folds (`derived`).
    #[serde(default)]
    pub derived: derived::DerivedPolicy,
}

impl Default for DerivationPolicy {
    fn default() -> Self {
        Self {
            tension: TensionDecl::default(),
            patterns: Vec::new(),
            configurations: true,
            arguments: false,
            derived: derived::DerivedPolicy::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// The author's prose — guidance and vocabulary terms. The only axis a
/// version-0 block fills.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProsePolicy {
    /// Domain-language extraction guidance, appended under "Domain focus".
    /// Untrimmed here; the composer trims.
    #[serde(default)]
    pub guidance: String,
    /// Term overrides. Version 0 fills them from `vocabulary`; version 1 also
    /// maps `tension.label` → `tension_term` and the first labelled claim
    /// type → `position_term`. Unset terms fall back in [`OntologyPolicies::vocabulary`].
    #[serde(default)]
    pub terms: OntologyVocabulary,
}

/// The reverse of [`OntologyPolicies::vocabulary`]: the five resolved terms a
/// pipeline prints, recorded as term overrides. A built-in pipeline's map
/// writes its terms this way, so `atlas/ontology.json` records the EFFECTIVE
/// vocabulary — `vocabulary()` of the result is the input, term for term.
impl From<&Vocabulary> for OntologyVocabulary {
    fn from(v: &Vocabulary) -> Self {
        OntologyVocabulary {
            concern_term: Some(v.canonical_concern_term.clone()),
            position_term: Some(v.position_term.clone()),
            tension_term: Some(v.tension_term.clone()),
            absence_term: Some(v.absence_term.clone()),
            evidence_term: Some(v.evidence_term.clone()),
        }
    }
}

impl OntologyPolicies {
    /// The version-0 shape: prose and terms, every other axis default. Also
    /// what a legacy `config.json` (guidance + vocabulary, no policies)
    /// synthesizes — see `CustomAtlasSpec::policies`.
    pub fn from_prose(guidance: &str, terms: OntologyVocabulary) -> Self {
        Self {
            prose: ProsePolicy {
                guidance: guidance.to_string(),
                terms,
            },
            ..Default::default()
        }
    }

    /// No ontology at all: every axis at its default, no prose.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// At least one type is declared. The P2 composer and parser return
    /// `None` when this is false, which is what keeps I1 structural: a
    /// version-1 block with no declarations composes today's bytes.
    pub fn has_declarations(&self) -> bool {
        !self.shape.types.is_empty()
    }

    /// Does this ontology select the custom atlas path? Non-blank guidance
    /// (today's hinge) or any declared type. A `vocabulary`-only block is
    /// not active — it never was.
    pub fn is_active(&self) -> bool {
        !self.prose.guidance.trim().is_empty() || self.has_declarations()
    }

    /// The declared type named `name`, whatever its kind.
    pub fn type_decl(&self, name: &str) -> Option<&OntologyTypeDecl> {
        self.shape.types.iter().find(|t| t.name == name)
    }

    /// The declared claim types, in declaration order. Their assertion facets
    /// (`force`, `deontic`, `subject`, `grades`, `anchors`, `scope`) are read
    /// off the decl.
    pub fn claim_types(&self) -> impl Iterator<Item = &OntologyTypeDecl> {
        self.shape
            .types
            .iter()
            .filter(|t| t.kind == TypeKind::Claim)
    }

    /// The engine [`Vocabulary`] these policies select: each term from
    /// `prose.terms` when set and non-blank, else the generic
    /// (non-literary) default. The ONE decider for custom-atlas vocabulary;
    /// it replaced `configurable_atlas::build_vocabulary`.
    pub fn vocabulary(&self) -> Vocabulary {
        fn term(opt: &Option<String>, default: &str) -> String {
            opt.as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| default.to_string())
        }
        let t = &self.prose.terms;
        Vocabulary {
            canonical_concern_term: term(&t.concern_term, "concern"),
            position_term: term(&t.position_term, "position"),
            tension_term: term(&t.tension_term, "tension"),
            absence_term: term(&t.absence_term, "gap"),
            evidence_term: term(&t.evidence_term, "passage"),
        }
    }
}

/// On-disk shape of `atlas/ontology.json` — the policies an atlas was
/// extracted under, written beside the atoms so the atlas describes itself.
///
/// The atlas directory has to answer "what did this corpus declare" on its
/// own: `corpus-engine` cannot read the enrich `config.json` (that type lives
/// in `sovereign-enrichment-catalog`), and `_summary.json` is a derived cache
/// that must be reproducible from the atlas dir alone. So the resolve step
/// writes the policies down beside the atoms.
///
/// Written by EVERY pipeline since ei-2-map (`EPISTEMIC_INDEX.md` §1, Map
/// row: an atlas that cannot describe itself is not an atlas) — a built-in
/// genre writes its fixed vocabulary down through the same struct. Absent
/// only for an atlas built before then; readers treat absence as "no
/// declaration", never as an error.
///
/// Moved here from `corpus-engine`'s `enrichment/atlas/writer.rs` by domains
/// `REVIEW-build-understanding-crate-tree`: it is the ONE host type a pure
/// file names (`enrichment/pipeline/pipelines/declaration.rs`), and it is
/// data, so it belongs with the language beside `AtomsFile`/`EdgesFile`. The
/// engine's writer re-exports it at the historical path.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AtlasOntologyFile {
    pub schema_version: String,
    /// The `[enrichment.ontology] version` the policies were parsed under —
    /// or [`Self::BUILTIN_ONTOLOGY_VERSION`] for a built-in pipeline's map,
    /// which is written in that language rather than parsed from a recipe.
    #[serde(default)]
    pub ontology_version: u32,
    /// The pipeline that extracted under these policies, as the registry
    /// spells it (`literary_atlas`, `custom_atlas`, …). Tells a reader
    /// whether the map was DECLARED by an author (`custom_atlas`) or WRITTEN
    /// DOWN by a genre. Empty on a file written before ei-2-map; readers
    /// report that, never guess.
    #[serde(default)]
    pub pipeline_id: String,
    /// What the pipeline read. Same struct the recipe parses into, so a
    /// reader never re-derives it.
    pub policies: OntologyPolicies,
}

impl AtlasOntologyFile {
    pub const SCHEMA_VERSION: &'static str = "1.0";
    /// File name under `atlas/`. The ONE spelling — the writer and the
    /// summary reader below both go through it.
    pub const FILE: &'static str = "ontology.json";
    /// The declaration language a built-in pipeline's map is written in
    /// (`pipelines/ontologies/*.toml` are version-1 block bodies). One number,
    /// one home: the resolve step records it and `declaration.rs` parses under it.
    pub const BUILTIN_ONTOLOGY_VERSION: u32 = 1;

    /// Was this map declared by a recipe author, as opposed to written down
    /// by a built-in genre? The custom pipeline reports `custom_atlas`.
    pub fn is_author_declared(&self) -> bool {
        self.pipeline_id == "custom_atlas"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_is_weighed_on_its_expected_precision_not_its_point_estimate() {
        let e = |right, of| decl::EvidentialFieldDecl {
            evidence: "x".into(),
            right,
            of,
            measured_on: "m".into(),
        };
        // 15 of 16 reads .938 and 190 of 210 .905; few links expect less.
        assert!(e(15, 16).precision() < e(190, 210).precision());
        assert!((e(15, 16).precision() - 16.0 / 18.0).abs() < 1e-12);
        assert!(e(4, 5).precision() > 0.5 && e(10, 21).precision() < 0.5);
    }

    /// `DocumentStamp::key` is the hand-spelled twin of the serde field names
    /// on `DocumentFieldsDecl`; read the struct back through serde so the two
    /// cannot drift.
    #[test]
    fn document_stamp_keys_are_the_declaration_keys() {
        let all = DocumentFieldsDecl {
            date: Some("a".into()),
            thread: Some("b".into()),
            id: Some("c".into()),
        };
        let json = serde_json::to_value(&all).unwrap();
        let mut wire: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        wire.sort_unstable();
        let mut keys: Vec<&str> = DocumentStamp::ALL.map(DocumentStamp::key).to_vec();
        keys.sort_unstable();
        assert_eq!(wire, keys);
        for s in DocumentStamp::ALL {
            assert!(s.attr().starts_with("document_"), "{}", s.attr());
            assert_eq!(all.field(s).is_some(), true);
        }
    }
}
