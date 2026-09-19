// SPDX-License-Identifier: AGPL-3.0-or-later
//! What an author DECLARES — the TOML-facing shapes under `[enrichment]`.
//!
//! Two families that turned out to be one concept: the investigation
//! pipeline's `[[enrichment.entity_types]]` / `[[enrichment.relationship_types]]`
//! / `[[enrichment.patterns]]` (lived in `recipe.rs`), and the version-1
//! ontology language's declared types on five axes (lived in
//! `enrichment/ontology/language.rs`). The `From` impls at the bottom are the
//! convergence: an investigation entity type IS a version-1 entity type with
//! untyped attributes. `OntologyVocabulary` (version 0's term overrides) is
//! here for the same reason — it is what [`super::ProsePolicy`] carries.
//!
//! `corpus-engine`'s `recipe_schema` test renders this file into
//! `sovereign-recipes/SCHEMA.md`, so every `pub` item here is author-facing
//! documentation as well as code.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    AssertionPolicy, ChangePolicy, DerivationPolicy, IdentityPolicy, NavigationPolicy,
    OntologyPolicies, ProsePolicy, ShapePolicy,
};

fn default_true() -> bool {
    true
}

/// Per-domain term overrides for the configurable atlas pipeline's vocabulary.
/// Maps onto the engine's `Vocabulary`; any omitted term uses a generic default.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OntologyVocabulary {
    #[serde(default)]
    pub concern_term: Option<String>,
    #[serde(default)]
    pub position_term: Option<String>,
    #[serde(default)]
    pub tension_term: Option<String>,
    #[serde(default)]
    pub absence_term: Option<String>,
    #[serde(default)]
    pub evidence_term: Option<String>,
}

/// One typed entity an investigation extracts. The recipe author
/// declares the *shape* — name, description, expected attribute
/// keys — and the investigation pipeline generates the LLM
/// extraction prompt directly from this schema. No Rust required.
///
/// Example:
/// ```toml
/// [[enrichment.entity_types]]
/// name = "company"
/// description = "A corporation or legal entity"
/// attributes = ["name", "ticker", "cik", "role"]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityTypeDecl {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Attribute keys the LLM should try to populate on each
    /// extracted instance. Free-form — the LLM extracts whatever
    /// keys it can locate in the chunk; missing keys land as null.
    #[serde(default)]
    pub attributes: Vec<String>,
}

/// One typed relationship the investigation extracts (e.g.
/// `revenue`, `investment`, `cloud_commitment`, `board_seat`).
/// Combined with [`EntityTypeDecl`], the schema fully drives the
/// LLM extraction prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipTypeDecl {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Attribute keys for the relationship instance — typically
    /// numeric (`amount_usd`, `percentage_of_total`) or temporal
    /// (`date`, `period`, `duration_years`).
    #[serde(default)]
    pub attributes: Vec<String>,
    /// `true` for asymmetric relationships (A → B is different
    /// from B → A: e.g. `revenue` and `investment`). `false` for
    /// symmetric ones (e.g. `co_membership`).
    #[serde(default = "default_true")]
    pub directional: bool,
}

/// A graph-level pattern to detect once the relationship graph is
/// built. The investigation pipeline runs every declared
/// [`PatternDecl`] after the graph is populated; matches land in
/// `pattern_findings.json` for the audit step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PatternDecl {
    /// Money / influence flows in a cycle: A→B→C→A. Powered by
    /// petgraph's Tarjan SCC; filters cycles with `len >=
    /// min_entities` whose edges all match `edge_types`.
    CircularFlow {
        #[serde(default = "default_name_circular_flow")]
        name: String,
        #[serde(default)]
        description: String,
        #[serde(default = "default_circular_flow_min_entities")]
        min_entities: u32,
        edge_types: Vec<String>,
    },
    /// Same pair of entities connected by two edge types that
    /// represent distinct roles. Canonical example: `(investor,
    /// customer)` — A invests in B AND A is a major customer of
    /// B's product. `entity_roles` maps a free-form role name
    /// (used in narration) to a typed-edge specifier
    /// `"<edge_type>.<from|to>"` describing which side of the edge
    /// the entity sits on.
    RoleOverlap {
        #[serde(default = "default_name_role_overlap")]
        name: String,
        #[serde(default)]
        description: String,
        entity_roles: BTreeMap<String, String>,
    },
    /// Numeric-attribute threshold over edges of a single type.
    /// E.g. "revenue concentration > 10%": find revenue edges
    /// whose `percentage_of_total` attribute exceeds 0.10.
    Threshold {
        #[serde(default = "default_name_threshold")]
        name: String,
        #[serde(default)]
        description: String,
        edge_type: String,
        attribute: String,
        threshold: f64,
        #[serde(default = "default_comparison")]
        comparison: Comparison,
    },
    /// **Reserved — not yet implemented.** Recipe authors can
    /// declare `type = "custom_sql"` today; the runtime parses
    /// it cleanly and the validator surfaces a warning so the
    /// author knows it won't run yet. The future implementation
    /// will execute `query` on a read-only SQLite connection
    /// materialised from the relationship graph, with
    /// `set_authorizer` rejecting `ATTACH` / `PRAGMA` /
    /// `load_extension`, a 5-second statement timeout, and
    /// single-statement enforcement. See SYSTEM_OVERVIEW.md §3.10
    /// for the back-compat rationale: reserving the shape now lets
    /// us land the SQL escape hatch later without forcing a
    /// schema migration on recipes already in the wild.
    CustomSql {
        #[serde(default = "default_name_custom_sql")]
        name: String,
        #[serde(default)]
        description: String,
        /// SQL query against `entities` / `relationships` /
        /// `pattern_findings` tables. Validation is parse-only
        /// today; execution arrives in a follow-up PR.
        query: String,
    },
}

fn default_circular_flow_min_entities() -> u32 {
    3
}

// A pattern's `name` defaults to its `type` (ONTOLOGY_PRIMITIVES.md §1.6
// writes no name). One fn per variant because serde takes a zero-argument
// path; `pattern_name_defaults_to_type` pins each to its wire tag.
fn default_name_circular_flow() -> String {
    "circular_flow".into()
}

fn default_name_role_overlap() -> String {
    "role_overlap".into()
}

fn default_name_threshold() -> String {
    "threshold".into()
}

fn default_name_custom_sql() -> String {
    "custom_sql".into()
}

fn default_comparison() -> Comparison {
    Comparison::GreaterThan
}

/// Comparison operator for [`PatternDecl::Threshold`]. Strict
/// (`gt`/`lt`) by default — boundary-equal cases are rare in the
/// investigation domain and the recipe author can opt into
/// inclusive comparisons explicitly.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    GreaterThan,
    GreaterOrEqual,
    LessThan,
    LessOrEqual,
    Equal,
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
    /// `[enrichment.ontology.navigation]` — how a reader walks the atlas per
    /// question kind (`NavigationPolicy`). Omit it, or any row, to take the
    /// spec's pre-registered defaults; the policy struct IS the TOML shape.
    #[serde(default)]
    pub navigation: NavigationPolicy,
}

impl OntologyV1 {
    /// Map the parsed block onto the five axes. Pure; the only judgement calls
    /// are the two label→term mappings, both documented on the fields they
    /// read (`tension.label`, `OntologyTypeDecl::label`).
    pub fn into_policies(self) -> OntologyPolicies {
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

        // Read before `self.types` moves into `ShapePolicy` below.
        let declares_types = !self.types.is_empty();

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
                // Phase 8's prompt is written in the literary frame, so a
                // corpus that declares its OWN types gets the rollups only
                // when its author asked (ONTOLOGY_MIGRATION §P4: "Phase 8
                // off for declared corpora until a neutral prompt
                // exists"). A block that declares nothing keeps today's
                // `true`, which is invariant I1.
                configurations: self.derive.configurations.unwrap_or(!declares_types),
                arguments: self.derive.arguments.unwrap_or(false),
            },
            prose: ProsePolicy {
                guidance: self.guidance,
                terms,
            },
            navigation: self.navigation,
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
pub enum SupersessionClock {
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
    pub clock: Option<SupersessionClock>,
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
