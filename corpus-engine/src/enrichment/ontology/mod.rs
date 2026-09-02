// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ontology policies — the version-independent interface between a recipe's
//! `[enrichment.ontology]` block and the enrichment pipeline.
//!
//! Five axes plus prose (`ONTOLOGY_PRIMITIVES.md` §0.1, §2): what exists
//! (shape), what is said (assertion), what is the same (identity), what
//! changes (change), what follows (derivation). The composer, parser,
//! resolver, tension selector, supersession fold, build report and inspector
//! read [`OntologyPolicies`] and nothing else; the recipe's `version` selects
//! an [`OntologyLanguage`] that parses TOML into these structs and is never
//! consulted again. That is what makes a version 2 cheap.
//!
//! Version 0 (today's prose block) fills `prose` and leaves every other axis
//! at its default. Those defaults ARE today's behaviour — `configurations`
//! on, `arguments` off, the document-date clock, the five generic vocabulary
//! terms — and `defaults_reproduce_today` pins them.
//!
//! The policy structs are deliberately NOT in `recipe.rs` and NOT in the
//! `recipe_schema` test's `SOURCES`: they derive `Deserialize` for the JSON
//! round trip an atlas will record (`atlas/ontology.json`, P2.6), not because
//! an author writes them. What an author writes is in [`language`].
//!
//! Field inventory is what P1 reads (validate, `enrich init`, the prompt
//! composer's prose path). Later phases add fields with defaults — a
//! per-type merge policy, a resolved trajectory, an argument budget — on the
//! axis that owns them, never a new struct.

pub mod clock;
pub mod language;
pub mod type_index;
mod validate;

pub use clock::section_date;
pub use language::{
    AttrDecl, AttrFamily, ChangeDecl, ClaimScopeDecl, Clock, Deontic, DeriveDecl, Force,
    OntologyLanguage, OntologyLanguageRegistry, OntologyTypeDecl, OntologyV1, SourceDecl,
    TensionDecl, TypeKind, VoicesDecl,
};
pub use type_index::TypeIndex;
pub use validate::{
    validate_block, OntologyValidation, MAX_ATTRS_PER_TYPE, MAX_ENUM_VALUES, MAX_TYPES_PER_KIND,
    RESERVED_CLAIM_KINDS,
};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::enrichment::pipeline::types::Vocabulary;
use crate::recipe::{OntologyVocabulary, PatternDecl};

/// Everything the pipeline reads from a declared ontology. Every field has a
/// default; the default of the whole is "no ontology" (`is_empty`), and a
/// prose-only version-0 block differs from it in `prose` alone.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OntologyPolicies {
    /// What exists: the declared types with their attributes, subtypes,
    /// roles, endpoints, sources and labels.
    #[serde(default)]
    pub shape: ShapePolicy,
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
}

/// Axis 1 — what a thing is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ShapePolicy {
    /// The declared types, in declaration order (a `Vec`, not a map, so the
    /// prompt bytes composed from it are deterministic).
    #[serde(default)]
    pub types: Vec<OntologyTypeDecl>,
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
    pub clock: Clock,
    /// Claim type → `"document_date"` or the time attribute it supersedes on.
    #[serde(default)]
    pub supersedes: BTreeMap<String, String>,
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
}

impl Default for DerivationPolicy {
    fn default() -> Self {
        Self {
            tension: TensionDecl::default(),
            patterns: Vec::new(),
            configurations: true,
            arguments: false,
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
