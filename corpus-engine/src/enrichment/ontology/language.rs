// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ontology declaration languages — one parser per `[enrichment.ontology]
//! version`, in a registry keyed by the integer.
//!
//! The pipeline never reads the version. It reads
//! [`OntologyPolicies`](super::OntologyPolicies); a version is nothing more
//! than a parser from the block's TOML table to those structs
//! (`ONTOLOGY_PRIMITIVES.md` §0.1). Version 0 is today's prose
//! [`OntologyConfig`] and fills `prose` only. Version 1 adds declared types
//! with attributes and the four corpus-level blocks. A version 2 is one more
//! [`OntologyLanguage`] impl registered in [`OntologyLanguageRegistry::builtin`]
//! plus a `docs/vN.md` section — it touches a version-1 code path only if a
//! policy struct gains a field, and that field carries a default.
//!
//! This file also holds the version-1 TOML types, because
//! `tests/main/recipe_schema.rs` renders every `Deserialize` type in its
//! `SOURCES` list into `sovereign-recipes/SCHEMA.md` and this file is on that
//! list. The policy structs live in the parent module precisely so they are
//! NOT rendered as recipe surface.
//!
//! Registry shape mirrors `enrichment::domain_registry::DomainRegistry`
//! (ARCH §4: an open set is a registry, an unknown id refuses loudly).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use super::{
    AssertionPolicy, ChangePolicy, DerivationPolicy, IdentityPolicy, OntologyPolicies, ProsePolicy,
    ShapePolicy,
};
use crate::error::{Error, Result};
use crate::recipe::{
    EntityTypeDecl, OntologyConfig, OntologyVocabulary, PatternDecl, RelationshipTypeDecl,
};
use crate::recipe_parsing::translate_parse_error;

// ── The trait and its registry ──────────────────────────────────────────────

/// One declaration language: parses the `[enrichment.ontology]` table (minus
/// `version`) into [`OntologyPolicies`]. Implemented once per shipped version.
pub trait OntologyLanguage: Send + Sync {
    /// The integer a recipe writes as `version = N`.
    fn version(&self) -> u32;
    /// Every top-level key this version accepts under `[enrichment.ontology]`.
    /// The load-time rule "a later version's key in an earlier block refuses"
    /// and the `validate` warning for keys no version defines are both
    /// computed from these lists — nothing else re-lists them (§10.6).
    fn keys(&self) -> &'static [&'static str];
    /// Parse the block body. Structural errors (a claim type without `force`,
    /// an unknown enum value) surface here, so `Recipe::from_toml` refuses
    /// them at load rather than at extraction.
    fn parse(&self, body: &toml::Table) -> Result<OntologyPolicies>;
    /// The `SCHEMA.md` section for this version, rendered by the
    /// `recipe_schema_is_fresh` gate after the generated type tables.
    fn schema_doc(&self) -> &'static str;
}

/// Registry of every shipped declaration language, keyed by version.
pub struct OntologyLanguageRegistry {
    /// Sorted ascending by version; `registry_versions_contiguous` pins that
    /// the versions run 0..=max with no gap.
    languages: Vec<Box<dyn OntologyLanguage>>,
}

impl OntologyLanguageRegistry {
    /// Every built-in language. Process-wide singleton; the registry is
    /// immutable after construction.
    pub fn builtin() -> &'static Self {
        static REGISTRY: LazyLock<OntologyLanguageRegistry> = LazyLock::new(|| {
            let mut languages: Vec<Box<dyn OntologyLanguage>> = vec![Box::new(V0), Box::new(V1)];
            languages.sort_by_key(|l| l.version());
            OntologyLanguageRegistry { languages }
        });
        &REGISTRY
    }

    /// The language for `version`, or `None` when this binary does not know
    /// it. Callers turn `None` into an error naming [`Self::max_version`].
    pub fn get(&self, version: u32) -> Option<&dyn OntologyLanguage> {
        self.languages
            .iter()
            .find(|l| l.version() == version)
            .map(|l| l.as_ref())
    }

    /// The highest version this binary reads. The ONE decider for the
    /// "supports ontology version <= N" refusal.
    pub fn max_version(&self) -> u32 {
        self.languages
            .iter()
            .map(|l| l.version())
            .max()
            .unwrap_or(0)
    }

    /// Every registered language, ascending by version.
    pub fn versions(&self) -> impl Iterator<Item = &dyn OntologyLanguage> {
        self.languages.iter().map(|l| l.as_ref())
    }

    /// The lowest version whose key list contains `key`, or `None` when no
    /// version defines it. Drives the load-time rule: a key first defined in
    /// version N inside a block declaring version M < N is a refusal naming
    /// `version = N`, never a silent drop.
    pub fn first_version_defining(&self, key: &str) -> Option<u32> {
        self.languages
            .iter()
            .find(|l| l.keys().contains(&key))
            .map(|l| l.version())
    }

    /// Keys in `body` that NO version defines — typos and stray keys. Sorted.
    /// A `validate` warning, not a load error (community recipes must keep
    /// loading; `deny_unknown_fields` was rejected for that reason).
    pub fn unknown_keys(&self, body: &toml::Table) -> Vec<String> {
        let mut out: Vec<String> = body
            .keys()
            .filter(|k| self.first_version_defining(k).is_none())
            .cloned()
            .collect();
        out.sort();
        out
    }
}

// ── Version 0 — today's prose block ─────────────────────────────────────────

/// Version 0: `guidance` prose plus optional `vocabulary` term overrides —
/// [`OntologyConfig`] exactly as it has always parsed. Fills `prose`; every
/// other policy stays at its default, which is today's behaviour.
struct V0;

const V0_KEYS: &[&str] = &["guidance", "vocabulary"];

impl OntologyLanguage for V0 {
    fn version(&self) -> u32 {
        0
    }

    fn keys(&self) -> &'static [&'static str] {
        V0_KEYS
    }

    fn parse(&self, body: &toml::Table) -> Result<OntologyPolicies> {
        let cfg: OntologyConfig = body.clone().try_into().map_err(translate_parse_error)?;
        Ok(OntologyPolicies::from_prose(
            &cfg.guidance,
            cfg.vocabulary.unwrap_or_default(),
        ))
    }

    fn schema_doc(&self) -> &'static str {
        include_str!("docs/v0.md")
    }
}

// ── Version 1 — declared types on five axes ─────────────────────────────────

/// Version 1: declared types with typed attributes plus the corpus-level
/// blocks. Every key is optional; a version-1 block with none of them yields
/// the same policies as version 0 (`v1_empty_equals_v0_equals_default`).
struct V1;

const V1_KEYS: &[&str] = &[
    "guidance",
    "vocabulary",
    "must_not",
    "types",
    "voices",
    "change",
    "tension",
    "derive",
    "patterns",
];

impl OntologyLanguage for V1 {
    fn version(&self) -> u32 {
        1
    }

    fn keys(&self) -> &'static [&'static str] {
        V1_KEYS
    }

    fn parse(&self, body: &toml::Table) -> Result<OntologyPolicies> {
        let v1: OntologyV1 = body.clone().try_into().map_err(translate_parse_error)?;
        // The one structural rule this version enforces at parse time. Force
        // is what separates a rule from a finding, and supersession applies
        // to the wrong things without it — so a claim type without it is
        // refused here, not defaulted (§18.3).
        if let Some(t) = v1
            .types
            .iter()
            .find(|t| t.kind == TypeKind::Claim && t.force.is_none())
        {
            return Err(Error::Recipe(format!(
                "ontology type `{}` has kind = \"claim\" but no `force`. Every claim \
                 type names what a source does with it: force = {}.",
                t.name,
                wire_names(&Force::ALL)
            )));
        }
        Ok(v1.into_policies())
    }

    fn schema_doc(&self) -> &'static str {
        include_str!("docs/v1.md")
    }
}

/// The wire spellings of closed enum values, for error messages — read back
/// through serde so the text can never disagree with what the parser accepts.
pub(crate) fn wire_names<T: Serialize>(all: &[T]) -> String {
    all.iter()
        .map(|v| {
            serde_json::to_string(v)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string()
        })
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(" | ")
}

// ── Version-1 TOML types (rendered into SCHEMA.md) ──────────────────────────

/// `[enrichment.ontology]` under `version = 1`: the declaration language for
/// "your own types". Every key is optional; each defaults to today's
/// behaviour, so a block carrying only `version = 1` (or only the version-0
/// `guidance` / `vocabulary` keys) is exactly a version-0 block. Declared
/// types are predicates over the fixed atom kinds — kinds stay closed, types
/// are declared. See `sovereign/docs/specs/ONTOLOGY_PRIMITIVES.md` §1 for the
/// ten worked declarations and §4 for what `recipe validate` checks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OntologyV1 {
    /// Domain-language extraction guidance, appended under "Domain focus" to
    /// the neutral Phase-1 prompt. Kept from version 0 as explanation; the
    /// declarations below are what generate the schema.
    #[serde(default)]
    pub guidance: String,
    /// Version-0 term overrides, still honoured. In version 1 prefer `label`
    /// on a type and `tension.label`; `validate` warns when both are set.
    #[serde(default)]
    pub vocabulary: Option<OntologyVocabulary>,
    /// Things the corpus must never be used for ("give dosing advice"). Read
    /// by the extraction prompt and the answer gate. Block-level.
    #[serde(default)]
    pub must_not: Vec<String>,
    /// The declared types (`[[enrichment.ontology.types]]`), each
    /// specializing one atom kind.
    #[serde(default)]
    pub types: Vec<OntologyTypeDecl>,
    /// Who speaks in the corpus, and which speakers are not subject matter.
    #[serde(default)]
    pub voices: VoicesDecl,
    /// What holds when: the clock and which claim types supersede.
    #[serde(default)]
    pub change: ChangeDecl,
    /// Which claim types can be in tension, and what makes two comparable.
    #[serde(default)]
    pub tension: TensionDecl,
    /// Opt-in derivation passes (interpretive configurations, arguments).
    #[serde(default)]
    pub derive: DeriveDecl,
    /// Graph patterns to detect over declared relation/event types. Same
    /// shapes as `[[enrichment.patterns]]` (`PatternDecl`).
    #[serde(default)]
    pub patterns: Vec<PatternDecl>,
}

impl OntologyV1 {
    /// Map the parsed block onto the five axes. Pure; the only judgement calls
    /// are the two label→term mappings, both documented on the fields they
    /// read (`tension.label`, `OntologyTypeDecl::label`).
    fn into_policies(self) -> OntologyPolicies {
        let mut terms = self.vocabulary.unwrap_or_default();
        if let Some(label) = &self.tension.label {
            terms.tension_term = Some(label.clone());
        }
        if let Some(label) = self
            .types
            .iter()
            .filter(|t| t.kind == TypeKind::Claim)
            .find_map(|t| t.label.as_ref())
        {
            terms.position_term = Some(label.clone());
        }

        let mut identity = BTreeMap::new();
        let mut identity_fallback = BTreeMap::new();
        for t in &self.types {
            if !t.identity.is_empty() {
                identity.insert(t.name.clone(), t.identity.clone());
            }
            if !t.identity_fallback.is_empty() {
                identity_fallback.insert(t.name.clone(), t.identity_fallback.clone());
            }
        }

        OntologyPolicies {
            shape: ShapePolicy { types: self.types },
            assertion: AssertionPolicy {
                voices: self.voices,
                must_not: self.must_not,
            },
            identity: IdentityPolicy {
                identity,
                identity_fallback,
            },
            change: ChangePolicy {
                clock: self.change.clock.unwrap_or_default(),
                supersedes: self.change.supersedes,
            },
            derivation: DerivationPolicy {
                tension: self.tension,
                patterns: self.patterns,
                configurations: self.derive.configurations.unwrap_or(true),
                arguments: self.derive.arguments.unwrap_or(false),
            },
            prose: ProsePolicy {
                guidance: self.guidance,
                terms,
            },
        }
    }
}

/// One declared type (`[[enrichment.ontology.types]]`). `name` and `kind`
/// are required; every other facet is optional and most apply to one kind
/// only (`from`/`to` to relations, `participants` to events, `of` to states,
/// `force`/`deontic`/`subject`/`grades`/`anchors`/`scope` to claims).
/// `recipe validate` checks that every reference resolves to a declared type.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OntologyTypeDecl {
    /// The author's noun for the type (`coin`, `rule`, `symptom`). Becomes
    /// the atom's `subtype` and the schema enum value.
    pub name: String,
    /// The atom kind this type specializes. Closed set.
    pub kind: TypeKind,
    /// What the type is, for the extraction prompt.
    #[serde(default)]
    pub description: String,
    /// Typed attributes in four value families (text, quantity, time, ref).
    /// At most `MAX_ATTRS_PER_TYPE`.
    #[serde(default)]
    pub attributes: Vec<AttrDecl>,
    /// A declared type this one is a kind of (`sceatta` specializes `coin`).
    /// The child inherits the parent's attributes and identity.
    #[serde(default)]
    pub specializes: Option<String>,
    /// A declared type this one is a part something plays (`ruler` is a role
    /// of `person`). Recorded as a State on the rigid atom, never a merge.
    #[serde(default)]
    pub role_of: Option<String>,
    /// Relations only: the declared type at the source end.
    #[serde(default)]
    pub from: Option<String>,
    /// Relations only: the declared type at the target end.
    #[serde(default)]
    pub to: Option<String>,
    /// Events only: role name → declared type of the participant.
    #[serde(default)]
    pub participants: BTreeMap<String, String>,
    /// States only: the declared type the state is of.
    #[serde(default)]
    pub of: Option<String>,
    /// A file + column mapping to ingest this type structurally (no model
    /// call), for corpora that already hold it as a table.
    #[serde(default)]
    pub source: Option<SourceDecl>,
    /// What the UI calls instances of this type. Defaults to `name`. On the
    /// first claim type that sets it, this also becomes the position term.
    #[serde(default)]
    pub label: Option<String>,
    /// External identifiers that make two mentions one thing (`rxnorm_id`).
    /// An external key merges strictly.
    #[serde(default)]
    pub identity: Vec<String>,
    /// Descriptive keys used when no external identifier is present
    /// (`["name", "employer"]`). A descriptive key is judged, not trusted.
    #[serde(default)]
    pub identity_fallback: Vec<String>,
    /// Claims only, REQUIRED there: what a source does with the claim.
    #[serde(default)]
    pub force: Option<Force>,
    /// Claims with `force = "directive"` only: the deontic modes the type can
    /// carry. `forbid X` is stored as `require not-X`.
    #[serde(default)]
    pub deontic: Vec<Deontic>,
    /// Claims only: the declared entity, event or state type the claim is
    /// about (the is-about relation).
    #[serde(default)]
    pub subject: Option<String>,
    /// Claims only: an ordered evidence scale, strongest first
    /// (`["trial", "case-series", "member-report"]`).
    #[serde(default)]
    pub grades: Vec<String>,
    /// Claims only: which anchor kinds count as evidence when it matters
    /// (`["table", "figure", "text"]`). Anchors are mandatory regardless.
    #[serde(default)]
    pub anchors: Vec<String>,
    /// Claims only: whether the claim is about the work or inside it.
    /// Default universal.
    #[serde(default)]
    pub scope: Option<ClaimScopeDecl>,
}

/// The atom kind a declared type specializes. Closed set: a new kind is a
/// design change to the atom model, not a recipe edit. Spellings match
/// `AtomType::label()` (`type_kind_spelling_matches_atom_type_label`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeKind {
    /// A re-identifiable particular (things are basic).
    #[default]
    Entity,
    /// A typed link between two declared types (`from` / `to`).
    Relation,
    /// Content plus force, with an evidence anchor.
    Claim,
    /// Something that happened, with `participants`.
    Event,
    /// A condition of a declared type (`of`), with a trajectory.
    State,
}

/// One typed attribute on a declared type. `name` and `type` are required;
/// the remaining keys belong to the family named by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttrDecl {
    /// The attribute key the extractor fills (`weight`, `dose`, `valid`).
    pub name: String,
    /// The value family, selected with `type = "…"`, plus its family keys.
    #[serde(flatten)]
    pub family: AttrFamily,
    /// What the attribute holds, for the extraction prompt.
    #[serde(default)]
    pub description: String,
}

/// The four value families an attribute can take.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttrFamily {
    /// Free text, or a closed set when `values` is given (at most
    /// `MAX_ENUM_VALUES`).
    Text {
        #[serde(default)]
        values: Vec<String>,
    },
    /// A number, optionally in a `unit` (`"g"`, `"mg"`, `"K"`, `"USD"`).
    Quantity {
        #[serde(default)]
        unit: Option<String>,
    },
    /// A point in time, or a span when `range = true`.
    Time {
        #[serde(default)]
        range: bool,
    },
    /// A reference to an instance of the declared type named by `of`.
    Ref { of: String },
}

impl AttrFamily {
    /// The wire spelling of the family (`text`, `quantity`, `time`, `ref`).
    pub fn key(&self) -> &'static str {
        match self {
            AttrFamily::Text { .. } => "text",
            AttrFamily::Quantity { .. } => "quantity",
            AttrFamily::Time { .. } => "time",
            AttrFamily::Ref { .. } => "ref",
        }
    }
}

/// What a source does with a claim (Searle's forces). Required on every
/// claim type; the parser refuses a claim type without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Force {
    /// States that something is so (findings, attributions, readings).
    Assertive,
    /// Requires, forbids, permits or requests (rules, obligations).
    Directive,
    /// Makes something so by saying it (decisions, definitions).
    Declaration,
    /// Commits the speaker to something (promises, undertakings).
    Commissive,
}

impl Force {
    /// Every force, for error messages and gates.
    pub const ALL: [Force; 4] = [
        Force::Assertive,
        Force::Directive,
        Force::Declaration,
        Force::Commissive,
    ];
}

/// Deontic mode of a directive claim type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Deontic {
    Require,
    Forbid,
    Permit,
    Request,
}

/// Which clock orders supersession. Derived as `document_date` when the
/// corpus carries document dates; declared only for narrative time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Clock {
    /// The date of the document a claim appears in.
    #[default]
    DocumentDate,
    /// Order within the work (chapters, scenes).
    Narrative,
    /// No temporal ordering; nothing supersedes.
    None,
}

/// Whether a claim type speaks about the work or from inside it. Default
/// universal; declare `about_work` for criticism of a fiction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimScopeDecl {
    /// True inside the work (what Alyosha believes).
    InWork,
    /// Said about the work (what a critic argues).
    AboutWork,
}

/// A structural source for a declared type: a file already holding it as a
/// table, ingested without a model call. `from`/`to` name the endpoint
/// columns of a relation; `attributes` maps attribute name → column.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceDecl {
    /// Path of the table (CSV or JSONL), relative to the corpus source.
    pub file: String,
    /// Relations: the column holding the `from` endpoint's identity.
    #[serde(default)]
    pub from: Option<String>,
    /// Relations: the column holding the `to` endpoint's identity.
    #[serde(default)]
    pub to: Option<String>,
    /// Declared attribute name → column name.
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

/// `[enrichment.ontology.voices]` — who speaks, and who is not subject
/// matter. Enforced in the Phase-1 parser, not only asked of the model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VoicesDecl {
    /// Speaker roles that must never become entity atoms ("the narrator",
    /// "the poster").
    #[serde(default)]
    pub not_entities: Vec<String>,
    /// The author's own voice, when the corpus is theirs (`self = "me"`).
    #[serde(default, rename = "self")]
    pub self_voice: Option<String>,
    /// The kinds of speaker a claim may be attributed to ("paper",
    /// "clinician", "member").
    #[serde(default)]
    pub attributed_to: Vec<String>,
}

/// `[enrichment.ontology.change]` — what holds when.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChangeDecl {
    /// The clock supersession folds on. Omit to derive `document_date`.
    #[serde(default)]
    pub clock: Option<Clock>,
    /// Claim type → the clock it supersedes on: `"document_date"` or a
    /// time-family attribute of that type (`{ rule = "valid" }`). A later
    /// instance retires the earlier one for the same subject.
    #[serde(default)]
    pub supersedes: BTreeMap<String, String>,
}

/// `[enrichment.ontology.tension]` — which claims can conflict, and what makes
/// two of them comparable. Replaces the Rust-side governance filter.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TensionDecl {
    /// What the UI calls a tension in this domain (`"conflict"`). Becomes
    /// the tension term.
    #[serde(default)]
    pub label: Option<String>,
    /// The claim types tensions are sought between.
    #[serde(default)]
    pub between: Vec<String>,
    /// Fields two claims must share to be comparable: `"subject"` or a
    /// declared attribute name. Defaults to the subject plus the type's clock.
    #[serde(default)]
    pub same: Vec<String>,
    /// Pairs that look like conflicts and are not, in the author's words.
    /// Rendered into the Phase-6 classifier; never complete, so versioned
    /// with the recipe.
    #[serde(default)]
    pub not_conflicts: Vec<String>,
}

/// `[enrichment.ontology.derive]` — opt-in derivation passes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeriveDecl {
    /// Run the interpretive-configuration rollups (Phase 8). Default true.
    #[serde(default)]
    pub configurations: Option<bool>,
    /// Reconstruct arguments. Default false.
    #[serde(default)]
    pub arguments: Option<bool>,
}

// ── Convergence: the investigation path's view of the same concept ──────────

impl From<&EntityTypeDecl> for OntologyTypeDecl {
    /// An investigation `[[enrichment.entity_types]]` entry is a version-1
    /// entity type whose attribute keys are untyped text.
    fn from(e: &EntityTypeDecl) -> Self {
        OntologyTypeDecl {
            name: e.name.clone(),
            kind: TypeKind::Entity,
            description: e.description.clone(),
            attributes: text_attrs(&e.attributes),
            ..Default::default()
        }
    }
}

impl From<&RelationshipTypeDecl> for OntologyTypeDecl {
    /// An investigation `[[enrichment.relationship_types]]` entry is a
    /// version-1 relation type with unresolved endpoints. `directional` has no
    /// version-1 facet (a relation is directional; symmetric ones are declared
    /// twice or as an attribute), so it is dropped here.
    fn from(r: &RelationshipTypeDecl) -> Self {
        OntologyTypeDecl {
            name: r.name.clone(),
            kind: TypeKind::Relation,
            description: r.description.clone(),
            attributes: text_attrs(&r.attributes),
            ..Default::default()
        }
    }
}

fn text_attrs(keys: &[String]) -> Vec<AttrDecl> {
    keys.iter()
        .map(|k| AttrDecl {
            name: k.clone(),
            family: AttrFamily::Text { values: Vec::new() },
            description: String::new(),
        })
        .collect()
}
