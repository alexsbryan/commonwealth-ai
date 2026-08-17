// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recipe schema — declarative TOML for `acquire → extract → chunk
//! → embed → index` pipelines, plus optional `[parameters]`,
//! `[catalog]`, `[enrichment]` blocks.
//!
//! ## Schema back-compatibility policy
//!
//! Recipes are user-authored data that lives outside this repo
//! (community registry, local user authoring, sample TOMLs in old
//! tutorials). A recipe written six months ago must still load
//! without ceremony. The reader enforces that by convention:
//!
//! 1. **Every new field carries `#[serde(default)]`** (or a typed
//!    default like `default_true`). Old TOMLs without the field
//!    parse to a sensible value. Removing a default — even on an
//!    optional-looking field — breaks every published recipe.
//! 2. **Renamed fields keep the old name as an alias** via
//!    `#[serde(alias = "old-name")]`. Drop the alias only after a
//!    full schema-version bump cycle.
//! 3. **Removed enum variants get a deprecation arm** in
//!    [`translate_parse_error`] that produces a tailored "use
//!    `<replacement>`" error instead of a generic "unknown
//!    variant". `api_paginated` → `http_api` is the canonical
//!    example.
//! 4. **`[corpus] schema_version`** is bumped only when readers
//!    must opt in to interpret the recipe (e.g. a new acquirer
//!    older engines can't run safely). Pure additions do NOT
//!    require a bump. The reader refuses recipes declaring a
//!    `schema_version > MAX_SCHEMA_VERSION` so a future-recipe
//!    loaded by an old engine fails loudly. See
//!    [`MAX_SCHEMA_VERSION`].
//! 5. **Reserved variants** — when a feature is *coming* but not
//!    yet implemented (e.g. the SQL escape hatch
//!    [`PatternDecl::CustomSql`]), reserve its variant in the
//!    schema NOW. The reserved shape parses cleanly, the runtime
//!    emits a visible placeholder (warning + finding row, never
//!    silent skip), and the validator flags it so the recipe
//!    author knows it's not fully wired. Recipes authored
//!    against the future shape don't need a migration when the
//!    runtime lands.
//!
//! `corpus-engine/tests/recipe_back_compat.rs` pins canonical
//! TOML shapes from each schema-version boundary and asserts they
//! still parse. Adding a regression fixture there is the standard
//! cost of a schema change; without it, future field additions
//! risk silently breaking old recipes.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::filters::{ComposeMode, FilterConfig};
#[cfg(test)]
use crate::recipe_builtin::{bundled_recipe_toml, RecipeId};
use crate::recipe_parsing::{
    check_schema_version, empty_value, parameter_value_from_toml, translate_parse_error,
};
#[cfg(test)]
use crate::recipe_parsing::{extract_missing_field, extract_unknown_variant};
use crate::types::CorpusKind;

// ---------------------------------------------------------------------------
// Default helpers
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

fn default_max_pages() -> usize {
    10_000
}

fn default_namespace_filter() -> Vec<u32> {
    vec![0]
}

fn default_min_score() -> i32 {
    3
}

fn default_max_answers_per_question() -> usize {
    5
}

fn default_title_column() -> String {
    "name".to_string()
}

fn default_described_asset_max_bytes() -> u64 {
    crate::extractors::described_asset::DescribedAssetExtractor::DEFAULT_MAX_BYTES_PER_ASSET
}

fn default_email_max_body_bytes() -> usize {
    crate::extractors::email_rfc5322::EmailExtractorConfig::default().max_body_bytes
}

fn default_url_column() -> String {
    "url".to_string()
}

fn default_controversy_patterns() -> Vec<String> {
    crate::extractors::wikipedia_structured::DEFAULT_CONTROVERSY_PATTERNS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn default_factual_patterns() -> Vec<String> {
    crate::extractors::wikipedia_structured::DEFAULT_FACTUAL_PATTERNS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn default_schema_version() -> u32 {
    1
}

fn default_max_chunk_chars() -> usize {
    2048
}

fn default_overlap_chars() -> usize {
    256
}

fn default_embedding_model() -> String {
    "qwen3-embedding-0.6b".to_string()
}

fn default_embedding_dimensions() -> usize {
    0 // 0 = auto-detect from the loaded model
}

// ---------------------------------------------------------------------------
// Top-level Recipe
// ---------------------------------------------------------------------------

/// Optional pre-built index block. When present, the engine can download a
/// pre-built LanceDB archive from HuggingFace instead of running a full ingest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrebuiltConfig {
    /// HuggingFace repo in `org/name` format, e.g. `"sovereign-foundation/wikipedia-index"`.
    pub hf_repo: String,
    /// Filename within the HF repo, e.g. `"wikipedia-qwen3-embedding-0.6b.tar.zst"`.
    pub hf_filename: String,
    /// Hex-encoded SHA-256 of the archive. Empty string skips verification.
    pub sha256: String,
    /// Embedding model name the pre-built index was built with. Used to verify
    /// compatibility with the currently loaded model before downloading.
    pub compatible_embedding_model: String,
}

/// `[authority]` block — see [`Recipe::authority`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorityConfig {
    /// Registered tool id (e.g. `sec_facts`) declared authoritative
    /// for this corpus's typed assertions.
    pub tool: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    pub corpus: CorpusMeta,
    pub acquire: AcquirerConfig,
    pub extract: ExtractorConfig,
    pub chunk: ChunkerConfig,
    #[serde(default)]
    pub index: IndexConfig,
    /// Optional epistemic enrichment configuration. When present and
    /// `enabled = true`, an enrichment phase runs after standard ingestion.
    /// Requires the engine to have been given an `InferenceFn`.
    #[serde(default)]
    pub enrichment: Option<EnrichmentConfig>,

    /// Optional authority declaration (FINANCIAL_CORPORA.md §7.3):
    /// names the registered tool that is AUTHORITATIVE for a class of
    /// assertions this corpus carries in a typed store, where the same
    /// corpus's prose contains lookalike values that are NOT
    /// authoritative (comparatives, roundings, guidance) and confusing
    /// the two causes material harm. Registry data shipped by the
    /// recipe author — deliberately NOT a user setting: a "use
    /// deterministic figures" toggle would make honesty optional
    /// (§7.4). The named tool's `claims()` consults this binding.
    #[serde(default)]
    pub authority: Option<AuthorityConfig>,

    /// Optional corpus update configuration. When present, the health
    /// monitor can check for new versions and apply delta updates.
    #[serde(default)]
    pub update: Option<UpdateConfig>,

    /// Optional pre-built index. When present, users can skip full ingest
    /// by downloading a pre-built LanceDB archive from HuggingFace.
    #[serde(default)]
    pub prebuilt: Option<PrebuiltConfig>,

    /// Optional catalog-corpus configuration. When present, this
    /// recipe is a *catalog* of works and pairs with a templated
    /// content recipe (referenced by `content_recipe`) used for
    /// on-demand single-work ingest. See [`CatalogConfig`] and
    /// `Recipe.corpus.kind = Catalog`.
    #[serde(default)]
    pub catalog: Option<CatalogConfig>,

    /// Document-level filters that scope the corpus by accepting or
    /// rejecting individual `ExtractedDoc`s before chunking. The
    /// canonical use case is Wikipedia "Core" — top-N by pageview rank
    /// ∪ Vital Articles list — but the mechanism works for any
    /// extractor (e.g. StackExchange `min_score`, OpenAlex
    /// `accepted_languages`).
    ///
    /// Empty / absent means the pipeline runs unfiltered.
    #[serde(default, rename = "filter")]
    pub filters: Vec<FilterConfig>,

    /// How filters in `filters` combine. Defaults to
    /// [`ComposeMode::Any`] — a document is accepted if any filter
    /// accepts. Set `mode = "all"` to require every filter to accept.
    /// Lives in its own `[filter_mode]` table because TOML does not
    /// allow scalars next to an array of tables.
    #[serde(default, rename = "filter_mode")]
    pub filter_mode: FilterModeConfig,

    /// Install-time parameters declared by the recipe. Concrete values
    /// are supplied by the user at `corpus install` time and
    /// interpolate into the `[acquire]` block via `{name}`
    /// placeholders. Lets a financial journalist (for example) ship
    /// one `sec-filings` recipe and let downstream users plug in
    /// their own entity list / form types / date range. See
    /// [`ParameterSpec`] and [`Recipe::resolve_parameters`].
    #[serde(default)]
    pub parameters: BTreeMap<String, ParameterSpec>,

    /// Runtime-only field carrying the user-supplied, validated
    /// parameter values for this ingest. Populated at install time
    /// by the CLI / desktop via [`Recipe::with_resolved_parameters`]
    /// and consumed by the `http_api` acquirer when interpolating
    /// `{name}` placeholders. **Skipped from TOML** — the recipe
    /// file declares only the schema, not user values.
    #[serde(skip, default)]
    pub resolved_parameters: ResolvedParameters,

    /// Presentation hints for UI surfaces (Atlas View rail grouping,
    /// Settings → Knowledge tile icons, etc.). Pure UI metadata —
    /// retrieval and ingest ignore this block. Drives the
    /// "Conversations" group in the Atlas View when corpora declare
    /// `category = "conversation"`.
    ///
    /// `#[serde(default)]` so recipes pre-dating this block still
    /// parse — see the back-compat policy at the top of this module.
    #[serde(default)]
    pub display: Option<DisplayMeta>,

    /// Retrieval-time behaviour hints (see [`RetrievalConfig`]). Unlike
    /// `[display]`, the runtime *reads* this when retrieving from the
    /// corpus. `#[serde(default)]` so recipes pre-dating the block parse.
    #[serde(default)]
    pub retrieval: RetrievalConfig,
}

/// Presentation hints for a recipe. See [`Recipe::display`].
///
/// Pure UI metadata: the retrieval layer reads `category` to decide
/// whether to render a chunk under "From your conversations" rather
/// than the corpus_id slug (see `format_scored_chunks_with_kinds`),
/// and the Atlas View rail groups corpora that share a category under
/// one header. No semantic meaning is attached to category strings —
/// add new ones as needed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct DisplayMeta {
    /// Logical group this corpus belongs to. Example values:
    /// `"conversation"`, `"reference"`, `"argument"`, `"personal"`.
    /// `None` means "ungrouped" — UI buckets these as "Other".
    pub category: Option<String>,
    /// Optional icon hint for desktop tiles. Free-form string; the
    /// frontend maps known values (`"chat-bubble"`, `"book"`, …) onto
    /// its icon set and falls back to a generic glyph for unknown
    /// values.
    pub icon: Option<String>,
}

/// Retrieval-time behaviour hints for a corpus. Unlike [`DisplayMeta`]
/// (pure UI), these change how the runtime *retrieves* from this corpus.
///
/// `#[serde(default)]` on the struct + each field so a recipe omitting the
/// `[retrieval]` table parses with baseline behaviour.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RetrievalConfig {
    /// When true, apply per-article source dedup to this corpus's
    /// retrieval: after fusion, keep each source article's single best
    /// chunk, then return the top-K *distinct* articles. Captures the
    /// canonical-source lift for corpora with narrow authoritative
    /// sources (SEP: +6 sources, 76%→85% on the eval bank, validated
    /// 2026-06-04) without the operator-only `SOVEREIGN_RERANK_DEDUP_ONLY`
    /// env var.
    ///
    /// Leave false for topical corpora (e.g. Wikipedia), where strict
    /// one-chunk-per-article truncation *regresses* recall — there the
    /// per-article tiebreak needs a cross-encoder, not blind dedup.
    #[serde(default)]
    pub dedup_by_source: bool,
    /// When true, this corpus counts as user-owned *personal* content
    /// (conversations, journals, watched folders / Obsidian vaults).
    /// Personal-scope turns restrict retrieval to personal corpora;
    /// before this flag the runtime used a hardcoded corpus-id prefix
    /// list, which silently excluded watched-folder corpora (ids are
    /// `watched-<hash>`). Reference corpora (Wikipedia, SEP, …) leave
    /// this false.
    #[serde(default)]
    pub personal_scope: bool,
}

/// Sidecar TOML table for [`Recipe::filter_mode`]. Splitting this from
/// the `[[filter]]` array keeps the recipe TOML grammatically valid:
/// the `[[filter]]` form is an array-of-tables and cannot host a
/// scalar `mode = "any"` field directly.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FilterModeConfig {
    #[serde(default)]
    pub mode: ComposeMode,
}

// ---------------------------------------------------------------------------
// Recipe parameters
// ---------------------------------------------------------------------------

/// Install-time parameter declared by a recipe. Lets the recipe author
/// defer concrete values (entity lists, date ranges, form types) until
/// the user runs `sovereign corpus install`. The CLI prompts for each
/// declared parameter (or accepts `--params key=value` non-interactively);
/// the desktop renders a form. Resolved values interpolate into the
/// `[acquire]` block via `{name}` placeholders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterSpec {
    /// Type of value expected. Drives prompting (text input, date
    /// picker, comma-separated list) and validation.
    #[serde(rename = "type")]
    pub kind: ParameterKind,
    /// Human-readable description shown in prompts and the desktop form.
    #[serde(default)]
    pub description: String,
    /// Whether the user must provide a value. `true` by default —
    /// require explicit opt-out so a missing required value can't
    /// silently install an empty corpus.
    #[serde(default = "default_true")]
    pub required: bool,
    /// Default value if the user does not provide one. Type must
    /// match `kind`. Stored as `toml::Value` so the recipe can
    /// declare lists / integers / strings / dates uniformly.
    #[serde(default)]
    pub default: Option<toml::Value>,
}

/// Type tag for [`ParameterSpec::kind`]. Drives both validation
/// of supplied values and the UI affordance shown to the user.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ParameterKind {
    /// Free-form string (a CIK, a search query, a tag).
    String,
    /// 64-bit signed integer.
    Int,
    /// ISO-8601 calendar date (`YYYY-MM-DD`). Validated lexically;
    /// not parsed into a chrono value here so the recipe schema
    /// doesn't grow a date-library dependency.
    Date,
    /// Comma-separated list of strings. The CLI accepts either a
    /// repeated flag or a single comma-separated value; the desktop
    /// renders a multi-tag input.
    List,
}

/// User-supplied parameter values, validated against the recipe's
/// `[recipe.parameters]` schema. Produced by [`Recipe::resolve_parameters`]
/// and consumed by the `http_api` acquirer when interpolating
/// `{name}` placeholders.
#[derive(Debug, Clone, Default)]
pub struct ResolvedParameters {
    pub values: BTreeMap<String, ParameterValue>,
}

/// Validated parameter value. The variant is keyed off the
/// declared [`ParameterKind`]; `as_interpolation()` flattens any
/// variant into a single string for `{name}` substitution.
#[derive(Debug, Clone)]
pub enum ParameterValue {
    String(String),
    Int(i64),
    /// Already-validated ISO-8601 date string (`YYYY-MM-DD`).
    Date(String),
    List(Vec<String>),
}

impl ParameterValue {
    /// Render this value into a single string suitable for `{name}`
    /// substitution in URL templates. Lists join with commas, which
    /// matches the canonical SEC EDGAR / CourtListener / OpenAlex
    /// query-parameter style.
    pub fn as_interpolation(&self) -> String {
        match self {
            ParameterValue::String(s) => s.clone(),
            ParameterValue::Int(i) => i.to_string(),
            ParameterValue::Date(s) => s.clone(),
            ParameterValue::List(items) => items.join(","),
        }
    }

    /// Iterate the value as a sequence of single string tokens,
    /// used by the `for_each` cross-product in [`RequestTemplate`]
    /// — every iteration yields one (parameter-name, value) binding
    /// the request template will see.
    pub fn iter_tokens(&self) -> Vec<String> {
        match self {
            ParameterValue::String(s) => vec![s.clone()],
            ParameterValue::Int(i) => vec![i.to_string()],
            ParameterValue::Date(s) => vec![s.clone()],
            ParameterValue::List(items) => items.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// UpdateConfig
// ---------------------------------------------------------------------------

/// Configures automatic corpus updates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    /// URL that returns a version manifest JSON for this corpus.
    pub manifest_url: String,

    /// If true the health monitor applies updates autonomously during the
    /// maintenance window. If false, a pending decision is surfaced to the
    /// user instead.
    #[serde(default)]
    pub auto_update: bool,

    /// Names the subsystem that owns ingest + ongoing updates for this
    /// corpus. When set, [`crate::engine::CorpusEngine::ingest`]
    /// short-circuits to "create an empty index, write
    /// `_corpus_meta.json`, return" instead of running the recipe's
    /// `[acquire]` pipeline — the named driver is then responsible for
    /// populating chunks on its own schedule.
    ///
    /// Current values:
    /// - `"watcher"` — daemon-side watcher (e.g.
    ///   `corpus_engine::update::newsworthy_watcher::WikipediaNewsworthyWatcher`)
    ///   handles fetches + reindexes via `reindex_by_source_doc_id`.
    ///   The recipe's `[acquire]` block is informational shape only
    ///   (the watcher reads the URL template + chunker config from it)
    ///   and is not invoked by `ingest`.
    ///
    /// `None` (the default) preserves the historical contract: ingest
    /// runs the full acquire/extract/chunk/index pipeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingest_driver: Option<String>,
}

impl UpdateConfig {
    /// True if this recipe declares an external ingest driver (e.g.
    /// a daemon-side watcher). Used by `CorpusEngine::ingest` to short-
    /// circuit to "create empty index + return" rather than running
    /// the acquire pipeline. Callers that care about the specific
    /// driver name read `ingest_driver.as_deref()` directly.
    pub fn has_external_driver(&self) -> bool {
        self.ingest_driver.as_deref().is_some_and(|s| !s.is_empty())
    }
}

// ---------------------------------------------------------------------------
// EnrichmentConfig
// ---------------------------------------------------------------------------

/// Configures the optional enrichment pipeline.
///
/// The new field model enrichment uses domain-specific prompts and
/// HDBSCAN clustering. Set `type = "field_model"` and `domain = "philosophy"`
/// (or another domain) to use the new pipeline.
///
/// For typed-relationship investigations (e.g. SEC filings → who
/// invests in whom while also being a customer), set `type =
/// "investigation"` and declare your `[[enrichment.entity_types]]`,
/// `[[enrichment.relationship_types]]`, and
/// `[[enrichment.patterns]]` blocks. The investigation pipeline
/// generates LLM prompts directly from the schema, so a domain
/// expert authors the extraction shape in TOML without touching
/// Rust. See [`EntityTypeDecl`], [`RelationshipTypeDecl`], and
/// [`PatternDecl`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentConfig {
    #[serde(default)]
    pub enabled: bool,

    // ── New field model fields ──────────────────────────────
    /// Enrichment type: "field_model" (default), "atlas",
    /// "investigation".
    #[serde(default = "default_enrichment_type", rename = "type")]
    pub enrichment_type: String,

    /// Domain identifier — **its meaning, and the registry it is checked
    /// against, depend on `type`.** With `type = "field_model"` the only
    /// valid values are the registered field-model domains: `philosophy`,
    /// `personal`, `conversational`, `business_email`, `institutional`
    /// (omit for `philosophy`); anything else is refused at load. With
    /// `type = "atlas"` it selects an atlas pipeline instead (`literary`,
    /// `philosophy`, `referential`), and `pipeline` overrides it. Sharing a
    /// key across two registries is what stranded two ingests on 2026-08-07
    /// with `Unknown enrichment domain: literary`.
    #[serde(default)]
    pub domain: Option<String>,

    /// Explicit atlas pipeline id (e.g. `"literary_atlas"`,
    /// `"philosophy_atlas"`) for `type = "atlas"` recipes. Optional override:
    /// when set, the desktop "Build & enrich" bridge
    /// (`recipe_enrich_init_from_corpus`) uses it directly instead of inferring
    /// the pipeline from `domain`. Previously this key was accepted and silently
    /// dropped (decorative); making it a real field means a recipe that pins a
    /// pipeline gets the pipeline it asked for. `None` → infer from `domain`.
    #[serde(default)]
    pub pipeline: Option<String>,

    /// Custom atlas ONTOLOGY for `type = "atlas"` recipes. This is the
    /// headline "build the ontology for your specific domain" path: instead of
    /// picking a prebuilt genre pipeline (`literary_atlas`/`philosophy_atlas`),
    /// the recipe author (with the agent) describes — in the domain's own
    /// language — what entities / relations / claims / events matter. A generic
    /// `ConfigurableAtlasPipeline` runs the universal 7-phase atlas machinery
    /// with this guidance and writes the same `atoms.json` that feeds chat.
    /// When present (with non-empty `guidance`), it takes precedence over
    /// `pipeline` and `domain`. `None` → fall back to a prebuilt atlas pipeline.
    #[serde(default)]
    pub ontology: Option<OntologyConfig>,

    /// Prompt version tag. Recorded in `_corpus_meta.json` so the health
    /// checker can detect stale enrichment when prompts change.
    #[serde(default)]
    pub prompt_version: Option<String>,

    /// HDBSCAN clustering parameters.
    #[serde(default)]
    pub clustering: Option<ClusteringToml>,

    /// Alignment parameters.
    #[serde(default)]
    pub alignment: Option<AlignmentToml>,

    /// Fault line detection parameters.
    #[serde(default)]
    pub fault_lines: Option<FaultLinesToml>,

    // ── Investigation-pipeline declarations ─────────────────
    /// Entity types the investigation pipeline should extract from
    /// each chunk. Listed in the LLM extraction prompt so the model
    /// canonicalizes mentions to one of these typed shapes (e.g.
    /// `company`, `fund`, `person`). Empty when
    /// `enrichment_type != "investigation"`.
    #[serde(default, rename = "entity_types")]
    pub entity_types: Vec<EntityTypeDecl>,

    /// Relationship types the investigation pipeline should extract
    /// (e.g. `revenue`, `investment`, `cloud_commitment`,
    /// `board_seat`). Each relationship has typed attributes the
    /// LLM is asked to populate (`amount_usd`, `date`, etc.).
    #[serde(default, rename = "relationship_types")]
    pub relationship_types: Vec<RelationshipTypeDecl>,

    /// Graph-level patterns to detect once the relationship graph is
    /// built. Built-in detectors cover cycle / role-overlap /
    /// threshold patterns; the recipe author chooses which to run.
    #[serde(default, rename = "patterns")]
    pub patterns: Vec<PatternDecl>,

    /// Architecture-over-Enron Phase 4: multi-origin reconciliation
    /// policy. `None` (the default) skips reconciliation entirely;
    /// pipelines that don't carry [`crate::enrichment::atlas::atoms::Provenance`]
    /// on their entity atoms produce nothing to reconcile across
    /// anyway. Recipes that enable described-asset + email
    /// extractors set this block to tune the merger.
    #[serde(default)]
    pub reconciliation: Option<ReconciliationToml>,

    /// Corpus-specific entity-name coalescing rules for the investigation
    /// pipeline. The engine supplies the *mechanism* (alias map, prefix /
    /// suffix / qualifier stripping, identity-by-attribute); this block
    /// supplies the *vocabulary*, so domain knowledge (US states, Air Force
    /// base aliases, disposition categories) lives in the recipe as data
    /// rather than hardcoded in the abstraction layer. `None` → names fold by
    /// case/punctuation only (the engine default). Consumed by
    /// [`crate::enrichment::investigation::normalize::Normalizer`].
    #[serde(default)]
    pub normalization: Option<NormalizationConfig>,
}

impl EnrichmentConfig {
    /// The on-disk artifact (relative to the corpus index dir) that a BUILT
    /// enrichment of this declared `type` writes — the file whose ABSENCE means
    /// the declared enrichment was never built/pulled on this machine (drift).
    ///
    /// Conservative by design: `None` for any type with no single verifiable
    /// artifact (e.g. `investigation`) so callers don't assert drift they can't
    /// check. Drives [`crate::engine`]'s `enrichment_drift` freshness probe.
    pub fn declared_artifact_rel_path(&self) -> Option<&'static str> {
        match self.enrichment_type.as_str() {
            "field_model" => Some("field_skeleton.json"),
            "atlas" => Some("atlas/atoms.json"),
            _ => None,
        }
    }
}

/// Custom atlas ontology declared in `[enrichment.ontology]`. The headline
/// "build the ontology for your domain" surface: `guidance` is domain-language
/// instructions for what to extract (entities, relations, events, claims),
/// injected into a NEUTRAL atlas Phase-1 prompt by
/// [`crate::enrichment::pipeline::pipelines::configurable_atlas::ConfigurableAtlasPipeline`].
/// The universal atom schema + open `EntityType::Other(..)` labels let a domain
/// expert author the extraction shape in TOML without touching Rust, and the
/// result feeds chat via the same `atoms.json` the prebuilt genre pipelines
/// produce. Precedence: a non-empty `guidance` here beats `pipeline`/`domain`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OntologyConfig {
    /// Domain-language extraction guidance — what entities, relations, events,
    /// and claims matter in THIS corpus's domain, in the domain's own words.
    /// Appended under a "Domain focus" heading to the neutral atlas Phase-1
    /// system prompt. The load-bearing field; an empty `guidance` disables the
    /// custom path (falls back to a prebuilt atlas pipeline).
    #[serde(default)]
    pub guidance: String,

    /// Optional CLI/label vocabulary overrides (what a "concern", "position",
    /// "tension", "absence", and unit of "evidence" are called for this domain).
    /// Omitted fields fall back to generic defaults in the pipeline.
    #[serde(default)]
    pub vocabulary: Option<OntologyVocabulary>,
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

/// Data-driven entity-name normalization for the investigation pipeline.
/// Every field is optional; an empty config folds names by case/punctuation
/// only. See [`crate::enrichment::investigation::normalize::Normalizer`] for
/// the mechanism that applies it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NormalizationConfig {
    /// `entity_type → attribute`: entities of this type take their identity
    /// from the named attribute's value, not their (often noisy) name — e.g.
    /// `adjudication = "category"` collapses date-/synthetic-id-named nodes
    /// that share a disposition. Applied during the offline re-fold
    /// (`recoalesce`), which remaps relationship endpoints so it can't strand
    /// an edge; build-time coalescing stays name-based (endpoint-safe).
    #[serde(default)]
    pub identity_attribute: std::collections::BTreeMap<String, String>,

    /// Name-fold rules, each scoped to the entity types it lists.
    #[serde(default)]
    pub fold: Vec<FoldRule>,
}

/// One scoped name-fold rule. Applied (in order) to the entity types in
/// `types`: alias map on the full folded form, then drop a leading qualifier,
/// then a trailing qualifier run, then the trailing-suffix run (OCR-tolerant),
/// then re-check the alias map on the reduced base. Identity-grade — only
/// qualifier/suffix regions are touched, base tokens are never fuzzy-matched,
/// so two distinct bases never merge.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FoldRule {
    /// Entity types this rule applies to (e.g. `["installation"]`).
    pub types: Vec<String>,

    /// `(folded-variant, canonical)` acronym/alias pairs, exact-matched on the
    /// folded surface form (e.g. `["wpafb", "wright patterson"]`).
    #[serde(default)]
    pub aliases: Vec<(String, String)>,

    /// Leading qualifier phrases dropped when followed by a base
    /// (e.g. `"air material command"`, `"atic"` → the org sat AT the base).
    #[serde(default)]
    pub leading_prefixes: Vec<String>,

    /// Trailing qualifier tokens/phrases dropped before the suffix run
    /// (e.g. US state names: `"ohio"`, `"new mexico"`). Multi-word entries
    /// match a trailing token-pair.
    #[serde(default)]
    pub trailing_qualifiers: Vec<String>,

    /// Single-token trailing suffix vocabulary, OCR-tolerant (edit-distance 1)
    /// — `"air"`, `"force"`, `"base"`, `"afb"`, `"field"`, … A trailing run of
    /// these (plus ≤2-char OCR fragments) is stripped to reach the base.
    #[serde(default)]
    pub trailing_suffixes: Vec<String>,
}

/// TOML mirror of
/// [`crate::enrichment::reconciliation::ReconciliationPolicy`].
/// Kept as a separate struct so the recipe schema stays string-named
/// (the policy struct uses Rust-native field names; the TOML can
/// rename in a future revision without touching the runner).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationToml {
    /// Minimum fold-overlap similarity for a name match to count.
    /// See [`crate::enrichment::reconciliation::ReconciliationPolicy`].
    #[serde(default = "default_name_similarity_threshold")]
    pub name_similarity_threshold: f32,
    /// Minimum *distinct* signals required for a cross-origin merge.
    #[serde(default = "default_cross_origin_required_signals")]
    pub cross_origin_required_signals: u8,
    /// Escalate uncertain candidates to the calibrated judge.
    #[serde(default = "default_true")]
    pub judge_when_uncertain: bool,
    /// Judge trial count when escalation fires.
    #[serde(default = "default_judge_trials")]
    pub judge_trials: u8,
    /// Column-aware extractor configuration. `None` to skip the
    /// column-aware pass entirely (the multi-origin merger still
    /// runs on whatever other signals the corpus produces).
    #[serde(default)]
    pub column_aware: Option<crate::extractors::column_aware::ColumnAwareConfig>,
}

fn default_name_similarity_threshold() -> f32 {
    0.85
}
fn default_cross_origin_required_signals() -> u8 {
    2
}
fn default_judge_trials() -> u8 {
    3
}

impl Default for ReconciliationToml {
    fn default() -> Self {
        Self {
            name_similarity_threshold: default_name_similarity_threshold(),
            cross_origin_required_signals: default_cross_origin_required_signals(),
            judge_when_uncertain: default_true(),
            judge_trials: default_judge_trials(),
            column_aware: None,
        }
    }
}

impl ReconciliationToml {
    /// Project this TOML shape onto the runtime policy struct.
    pub fn to_policy(&self) -> crate::enrichment::reconciliation::ReconciliationPolicy {
        crate::enrichment::reconciliation::ReconciliationPolicy {
            name_similarity_threshold: self.name_similarity_threshold,
            cross_origin_required_signals: self.cross_origin_required_signals,
            judge_when_uncertain: self.judge_when_uncertain,
            judge_trials: self.judge_trials,
        }
    }
}

// ---------------------------------------------------------------------------
// Investigation-pipeline schema (entity types, relationship types, patterns)
// ---------------------------------------------------------------------------

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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PatternDecl {
    /// Money / influence flows in a cycle: A→B→C→A. Powered by
    /// petgraph's Tarjan SCC; filters cycles with `len >=
    /// min_entities` whose edges all match `edge_types`.
    CircularFlow {
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
        name: String,
        #[serde(default)]
        description: String,
        entity_roles: BTreeMap<String, String>,
    },
    /// Numeric-attribute threshold over edges of a single type.
    /// E.g. "revenue concentration > 10%": find revenue edges
    /// whose `percentage_of_total` attribute exceeds 0.10.
    Threshold {
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

fn default_enrichment_type() -> String {
    "field_model".to_string()
}

/// HDBSCAN clustering parameters (TOML representation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusteringToml {
    #[serde(default)]
    pub min_cluster_size: Option<usize>,
    #[serde(default)]
    pub epsilon: Option<f32>,
    #[serde(default)]
    pub label_sample_size: Option<usize>,
}

/// Alignment parameters (TOML representation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlignmentToml {
    #[serde(default)]
    pub alignment_threshold: Option<f32>,
    #[serde(default)]
    pub min_chunks_for_discovery: Option<usize>,
}

/// Fault line detection parameters (TOML representation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaultLinesToml {
    #[serde(default)]
    pub proximity_threshold: Option<f32>,
    #[serde(default)]
    pub min_confidence: Option<f32>,
}

impl Default for EnrichmentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            enrichment_type: default_enrichment_type(),
            domain: None,
            pipeline: None,
            ontology: None,
            prompt_version: None,
            clustering: None,
            alignment: None,
            fault_lines: None,
            entity_types: Vec::new(),
            relationship_types: Vec::new(),
            patterns: Vec::new(),
            reconciliation: None,
            normalization: None,
        }
    }
}

// ---------------------------------------------------------------------------
// CorpusMeta
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub license: String,
    #[serde(default = "default_true")]
    pub mesh_sharing: bool,
    /// Distribution scope. `Some("local")` pins a corpus to the host
    /// machine: it may never be shared via the mesh regardless of
    /// `mesh_sharing`. Used by `KnowledgeView` corpora sourced from
    /// private state (e.g. `personal-knowledge`, `conversation-history`)
    /// so the privacy guarantee is structural, not policy-layer.
    /// `None` = default behaviour governed by `mesh_sharing`.
    #[serde(default)]
    pub scope: Option<String>,
    /// Whether peers may run federated knowledge-search queries
    /// against a node that hosts this corpus. Distinct from
    /// `mesh_sharing`, which governs byte-level redistribution
    /// (shipping the index to another node for replication).
    ///
    /// Example: Stanford Encyclopedia of Philosophy has
    /// `mesh_sharing = false` because the license prohibits
    /// redistribution of the text, but `query_sharing = true`
    /// because returning cited snippets in response to queries
    /// is fair use (what Google does).
    ///
    /// Back-compat default: `None` means "fall back to
    /// `mesh_sharing`" — preserves the pre-split behavior for
    /// any recipe or stored index that hasn't been updated.
    /// Set explicitly to override.
    #[serde(default)]
    pub query_sharing: Option<bool>,
    /// Whether this corpus MAY be temporarily lent to a user-selected
    /// set of mesh peers for a one-off compute assist (embed + enrich)
    /// under an ephemeral, revocable grant — WITHOUT ever changing its
    /// standing `mesh_sharing`/`scope`. Set `true` only by user-owned
    /// file corpora (Obsidian vault / document folder / watched folder).
    /// Structural `KnowledgeView` corpora (`personal-knowledge`,
    /// `conversation-history`, …) leave it `false` so they can never be
    /// grant-shared, even transiently. Default `false`: a corpus is not
    /// grantable unless it explicitly opts in. See the ephemeral
    /// ingest-grant store in `commonwealth-knowledge`.
    #[serde(default)]
    pub grantable: bool,
    #[serde(default)]
    pub size_compressed_gb: f64,
    #[serde(default)]
    pub size_indexed_gb: f64,
    /// Schema version for this recipe format. Defaults to 1.
    /// Increment when making breaking changes to the TOML schema.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// What kind of content this corpus holds. Defaults to
    /// `Knowledge`. Catalog corpora hold one chunk per work
    /// (metadata only) and pair with a `[catalog]` block at the
    /// recipe top level. Code corpora are produced by `sovereign
    /// code index`. See [`crate::types::CorpusKind`].
    #[serde(default)]
    pub kind: CorpusKind,

    /// Marks a recipe as "templated, never directly ingested." On-demand
    /// recipes (e.g. `gutenberg-work`) are stamped from a catalog
    /// entry at runtime via
    /// [`crate::types::CorpusSpec::Inline`]. The plain
    /// [`crate::engine::CorpusEngine::ingest`] path refuses to run
    /// an `on_demand = true` recipe whose `[corpus] id` has not been
    /// overridden, so a misclick can't blast 70K Gutenberg books
    /// into the corpus dir.
    #[serde(default)]
    pub on_demand: bool,

    /// Parent corpus this recipe is grouped under. Two use cases share
    /// the field:
    ///
    /// 1. **Dynamic per-work catalog children.** Set at runtime by an
    ///    on-demand catalog ingest (e.g. `gutenberg-2701` carries
    ///    `parent_corpus_id = "gutenberg"`) via
    ///    [`crate::types::CorpusSpec::Inline`]. Search consumers group
    ///    per-work corpora under their catalog and suppress repeated
    ///    ingest offers for works already read.
    ///
    /// 2. **Static layer/satellite relationships declared in TOML.**
    ///    `wikipedia-simple` and `wikipedia-newsworthy` declare
    ///    `parent_corpus_id = "wikipedia"` to mark themselves as
    ///    layers of the Core Wikipedia corpus. UI surfaces (e.g. the
    ///    desktop picker) hide layered children from the top-level
    ///    list and render them as toggles under the parent's row. The
    ///    data layer is unaffected — each child still has its own
    ///    `id`, index dir, mesh-sharing rules, and watcher (if any).
    ///
    /// Stamped onto the on-disk `IndexMeta` in both cases, so
    /// `installed_indexes()` and downstream UI can group consistently.
    /// Pointing at an id that doesn't exist is not a parse error — the
    /// desktop falls back to top-level rendering for orphans.
    #[serde(default)]
    pub parent_corpus_id: Option<String>,

    /// How `merge_shards` should reconcile rows that share a logical
    /// key across two shards. `None` (the default) keeps the
    /// content-hash-based dedupe used by every classic corpus —
    /// divergent edits of the same source document survive as two
    /// rows with different `content_hash`. The `alignment` corpus
    /// opts into [`MutableMergePolicy::SourceDocIdNewestMtime`] so
    /// that two daemons editing the same memory or plan file
    /// converge on the newer copy after a mesh merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutable_merge: Option<MutableMergePolicy>,
}

/// Reconciliation policy invoked by [`crate::sharding::merge_shards`]
/// when the merged target's `_corpus_meta.json` carries a
/// `mutable_merge` value. Default (`None`) preserves classic
/// content-hash dedupe.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutableMergePolicy {
    /// Group rows by `source_doc_id`. When a logical key collides,
    /// keep the row with the highest `mtime`. Rows whose
    /// `source_doc_id` is null fall back to content-hash dedupe.
    SourceDocIdNewestMtime,
}

// ---------------------------------------------------------------------------
// CatalogConfig — recipe-level "this is a catalog of works" block
// ---------------------------------------------------------------------------

/// Pairs with `CorpusMeta::kind = Catalog`. Tells the on-demand
/// ingest service how to take a catalog entry and produce a fully
/// ingested per-work corpus from it. See `gutenberg/recipe.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogConfig {
    /// Field name on the catalog `ExtractedDoc` (or its metadata
    /// blob) that uniquely identifies a work. Used by the on-demand
    /// flow to substitute into `download_url_template` and to derive
    /// the per-work corpus id (`<catalog_id>-<work_id>`).
    pub id_field: String,

    /// URL template with a `{id}` placeholder, e.g.
    /// `"https://www.gutenberg.org/cache/epub/{id}/pg{id}.txt"`.
    /// Resolved at on-demand ingest time and injected as the sole
    /// `[acquire] url` of the content recipe.
    pub download_url_template: String,

    /// Recipe id of the content recipe used to perform the
    /// per-work ingest, e.g. `"gutenberg-work"`. Must be `on_demand =
    /// true` and live in the registry.
    pub content_recipe: String,

    /// Optional name of a metadata column carrying an estimated
    /// word count (used to compute an ingest-time estimate the UI
    /// can show).
    #[serde(default)]
    pub estimated_words_field: Option<String>,

    /// Throughput estimate for the ingest stage, in words per
    /// minute. Combined with `estimated_words` to produce the
    /// "this will take ~N minutes" surface. Default 8000 wpm
    /// (conservative for an M-class machine on the embed slot).
    #[serde(default)]
    pub ingest_estimate_wpm: Option<u32>,

    /// Throughput estimate for the enrichment stage, in words per
    /// minute. Default 500 wpm.
    #[serde(default)]
    pub enrich_estimate_wpm: Option<u32>,

    /// Optional shared corpus id that catalog-driven ingests append
    /// into. When set, every successful work-ingest writes its
    /// chunks into a single growing corpus (e.g. `"wikipedia-fetched"`)
    /// instead of creating one corpus per work. Atlas, mesh-share,
    /// and retrieval all happen against the single shared corpus —
    /// a much better fit for catalogs whose long-tail can be
    /// thousands of articles. When unset (default), the legacy
    /// per-work pattern (`<catalog_id>-<work_id>`) is used.
    #[serde(default)]
    pub target_corpus_id: Option<String>,

    /// Enable one-hop "minesweeper" link-expansion after fetching an
    /// article. When true, the just-ingested article's outgoing
    /// links are queued for follow-up fetch into the same
    /// `target_corpus_id`. Only meaningful when `target_corpus_id`
    /// is set — without a shared target each expansion would
    /// spawn yet another per-work corpus.
    #[serde(default)]
    pub expansion_enabled: bool,

    /// Maximum number of linked articles to fetch in expansion.
    /// Ranking is significance-first (lead-section links beat
    /// body-section links, then document order). Default 20 keeps
    /// the per-fetch cost bounded; raise for deeper neighbourhood
    /// pre-loading, lower for fastest-only-the-asked behaviour.
    #[serde(default = "default_expansion_link_cap")]
    pub expansion_link_cap: u32,
}

fn default_expansion_link_cap() -> u32 {
    20
}

// ---------------------------------------------------------------------------
// HTTP API acquirer types (used by `AcquirerConfig::HttpApi`)
// ---------------------------------------------------------------------------

/// One HTTP request template. Combined with `[recipe.parameters]`
/// values via `{name}` interpolation. `for_each` declares which
/// parameters cross-product the template — e.g. one paginated
/// request sequence per (entity, form_type) pair when ingesting
/// SEC filings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestTemplate {
    /// URL with `{name}` placeholders for parameters and `{base_url}`
    /// for the acquirer's `base_url`.
    pub url: String,
    /// HTTP method. Defaults to `GET`.
    #[serde(default)]
    pub method: HttpMethod,
    /// Optional request body, with `{name}` interpolation. Used by
    /// JSON-RPC-shaped APIs that take queries via POST.
    #[serde(default)]
    pub body: Option<String>,
    /// Cross-product the request over these parameter names. Each
    /// referenced parameter must be a `List` (or implicitly
    /// promoted scalar). The acquirer issues one full paginated
    /// sequence per cartesian-product binding. Empty = a single
    /// request with all `{name}` placeholders resolved
    /// element-wise from their declared values.
    #[serde(default)]
    pub for_each: Vec<String>,
}

/// HTTP method for a [`RequestTemplate`]. Kept narrow on purpose —
/// REST acquisition rarely needs PATCH/DELETE/PUT.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    #[default]
    Get,
    Post,
}

/// Pagination strategy for [`AcquirerConfig::HttpApi`]. The acquirer
/// drives the loop; the strategy translates per-page response state
/// into the next request. None of the strategies make assumptions
/// the recipe author can't articulate from the API's docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PaginationStrategy {
    /// Offset-based: increment `param` by `page_size` each page;
    /// stop when the page returns fewer than `page_size` items
    /// found at `items_path` (a JSONPath expression on the response
    /// body).
    Offset {
        #[serde(default = "default_offset_param")]
        param: String,
        page_size: usize,
        #[serde(default = "default_items_path")]
        items_path: String,
    },
    /// Cursor-based: read the next cursor from `response_path`
    /// (JSONPath); pass it as the next request's `param`. Stops
    /// when the cursor field is null/missing.
    Cursor {
        param: String,
        response_path: String,
    },
    /// Whole-URL next pointer: read a complete URL out of
    /// `response_path` and follow it as-is. Common for RFC 5988
    /// Link-style APIs and GitHub.
    NextUrl { response_path: String },
    /// Page-number sequence: increment `param` from `start` to
    /// `end` (inclusive). Use when the page count is known
    /// upfront. `end` may reference a recipe parameter via
    /// `{name}` to let the user bound the run length.
    PageNumber {
        #[serde(default = "default_page_number_param")]
        param: String,
        #[serde(default = "default_page_number_start")]
        start: usize,
        end: usize,
    },
}

fn default_offset_param() -> String {
    "offset".to_string()
}
fn default_items_path() -> String {
    "$.items".to_string()
}
fn default_page_number_param() -> String {
    "page".to_string()
}
fn default_page_number_start() -> usize {
    1
}

/// Tells the acquirer how to take an API response and turn it into
/// a list of documents to fetch and persist. Without this block,
/// the page responses themselves are written to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowConfig {
    /// JSONPath expression selecting an array of URL strings from
    /// the response body, e.g.
    /// `"$.hits.hits[*]._source.file_url"` for an EDGAR full-text
    /// search response.
    pub document_url_path: String,
    /// Format hint that drives the on-disk extension under
    /// `<acquired-dir>/docs/<sha>.<ext>`. The extractor walks the
    /// directory regardless of which format flag the acquirer set.
    #[serde(default)]
    pub document_format: DocFormat,
    /// Maximum concurrent in-flight document downloads. Default 4
    /// — keep modest for public APIs to avoid 429s. The
    /// acquirer's `rate_limit_per_second` (if any) caps the
    /// aggregate request rate orthogonally.
    #[serde(default = "default_follow_concurrency")]
    pub max_concurrency: usize,
}

fn default_follow_concurrency() -> usize {
    4
}

/// On-disk document format hint for [`FollowConfig::document_format`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DocFormat {
    #[default]
    Html,
    Json,
    Xml,
    Plaintext,
}

// ---------------------------------------------------------------------------
// HtmlSections extractor types
// ---------------------------------------------------------------------------

/// One section to extract from each HTML file. The recipe author
/// declares anchor regexes (`start_pattern` / `end_pattern`); the
/// extractor strips tags first, then runs the regexes against the
/// resulting plain text. The matched span between start and end
/// becomes one `ExtractedDoc` per file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionRule {
    /// Stable name for this section, e.g. `"md_and_a"`. Used as
    /// part of the emitted document's title and stamped in
    /// `metadata.section_name`.
    pub name: String,
    /// Human-readable description shown in `recipe test` output and
    /// used as a hint when a miss occurs (the test harness searches
    /// nearby text for keywords from this description).
    #[serde(default)]
    pub description: String,
    /// Regex pattern matching the start of the section. Compiled
    /// at extractor construction; bad regexes fail loudly with
    /// the section name in the error.
    pub start_pattern: String,
    /// Regex pattern matching the end of the section. Typically a
    /// "next item heading" anchor, e.g.
    /// `(?i)item\\s+[0-9]` for SEC filings.
    pub end_pattern: String,
    /// When true, emit one document per `start_pattern` match in the
    /// file instead of only the first. Each emitted section runs from
    /// its start match to the *next* start match, bounded earlier by
    /// `end_pattern` if it matches within that window (so the final
    /// repetition can terminate on a trailing anchor like
    /// `ADDITIONAL INFORMATION`). Use for documents that repeat a
    /// section an unbounded number of times — e.g. the numbered
    /// proposals in an SEC proxy statement (DEF 14A) or dated articles
    /// in a governance charter. Default `false` preserves the
    /// first-match-only behaviour relied on by single-section recipes.
    #[serde(default)]
    pub repeating: bool,
}

/// Fallback for files where no section pattern matched. Without a
/// fallback, files with no matching section are silently dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FallbackRule {
    /// Emit the entire stripped text as a single document. Useful
    /// when "we'd rather have something than nothing" — the
    /// extractor still records the miss in `_section_misses.json`
    /// so the recipe author can iterate on the regex.
    FullDocument {
        /// Cap the output at this character count. None = no cap.
        #[serde(default)]
        max_chars: Option<usize>,
    },
    /// Emit the first N characters of the stripped text. Cheap
    /// approximation of "the document's intro" for content-heavy
    /// pages without clear section structure.
    FirstNChars { n: usize },
}

// ---------------------------------------------------------------------------
// AcquirerConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AcquirerConfig {
    /// Bulk-download one or more archives over HTTP with resume.
    ///
    /// Single-source recipes use `url = "..."`. Multi-source recipes
    /// (e.g. the Stack Exchange knowledge layer pulling from several
    /// per-site .7z archives) use `urls = ["...", "..."]`. The
    /// downloader writes each archive under a per-corpus directory,
    /// so the extractor receives a directory of archives rather than
    /// a single file in the multi-source case.
    ///
    /// Exactly one of `url` / `urls` must be set; recipes that set
    /// both fail to build.
    #[serde(rename = "bulk_download")]
    BulkDownload {
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        urls: Option<Vec<String>>,
        #[serde(default = "default_true")]
        resume: bool,
    },
    #[serde(rename = "web_crawl")]
    WebCrawl {
        seed_urls: Vec<String>,
        link_pattern: String,
        #[serde(default = "default_max_pages")]
        max_pages: usize,
    },
    /// Generic REST API acquirer. Replaces the never-implemented
    /// `api_paginated` stub with a real, recipe-author-friendly
    /// surface: parameterised URL templates, pagination
    /// strategies (offset / cursor / next-URL / page-number),
    /// JSONPath document-URL follow, rate limiting, custom
    /// headers / User-Agent. Combined with `[recipe.parameters]`,
    /// a domain expert can author a working recipe for SEC EDGAR /
    /// CourtListener / OpenAlex / PubMed / etc. without touching
    /// Rust. See [`crate::acquirers::http_api`].
    #[serde(rename = "http_api")]
    HttpApi {
        /// Base URL — referenced via `{base_url}` in
        /// `requests[].url`, optional otherwise. Exists primarily
        /// so the recipe author doesn't repeat the same prefix in
        /// every template.
        #[serde(default)]
        base_url: String,
        /// One or more request templates. Each template may declare
        /// `for_each` to cross-product over named parameters
        /// declared in `[recipe.parameters]`. The acquirer issues
        /// one paginated request sequence per template × resolved
        /// `for_each` binding.
        requests: Vec<RequestTemplate>,
        /// Pagination strategy. Absent = single-page request.
        #[serde(default)]
        pagination: Option<PaginationStrategy>,
        /// Document-follow config. When present, the acquirer
        /// treats each page response as an *index* (a list of
        /// document URLs) and fetches the documents in parallel,
        /// writing them under `<acquired-dir>/docs/<sha>.<ext>`
        /// for the extractor. When absent, the page responses
        /// themselves are persisted.
        #[serde(default)]
        follow: Option<FollowConfig>,
        /// Token-bucket rate limit, requests per second across all
        /// in-flight requests for this acquirer instance. None =
        /// no throttling. SEC requires ≤ 10 req/sec; OpenAlex
        /// recommends ≤ 10 req/sec with an email tag.
        #[serde(default)]
        rate_limit_per_second: Option<f32>,
        /// Override the default `CorpusEngine/0.1` User-Agent.
        /// Some APIs (SEC, GitHub) reject requests without a
        /// contact-bearing UA.
        #[serde(default)]
        user_agent: Option<String>,
        /// Extra HTTP headers (Authorization, Accept, etc.).
        /// Templated values may use `{name}` placeholders to
        /// reference recipe parameters (e.g. an API token).
        #[serde(default)]
        headers: Option<BTreeMap<String, String>>,
    },
    #[serde(rename = "local_file")]
    LocalFile { path: String },
    /// Download all parquet shards for a public HuggingFace dataset.
    /// Uses the HF dataset API to enumerate shards, then downloads each
    /// with resume support, returning a directory of parquet files.
    #[serde(rename = "huggingface_dataset")]
    HuggingFaceDataset {
        /// Dataset repo in `org/name` format, e.g. `"manu/project_gutenberg"`.
        repo: String,
        /// Optional subset prefix to filter shards, e.g. `"en"` matches
        /// filenames starting with `data/en-`. If absent, all parquet shards
        /// are downloaded.
        #[serde(default)]
        subset: Option<String>,
        /// Restrict ingestion to a specific subset of shard indices.
        ///
        /// Indices refer to position in the **sorted** manifest (ascending by
        /// filename). Both the coordinator and the peer must sort the same
        /// full manifest before slicing, so they agree on which file each
        /// index refers to.
        ///
        /// `None` = download all files (default; preserves existing behaviour).
        #[serde(default)]
        file_indices: Option<Vec<usize>>,
    },
    /// Runtime-registered acquirer. `kind` selects an implementation
    /// previously registered via [`CorpusEngine::register_acquirer`];
    /// `params` is passed through unchanged so the implementation can
    /// deserialize its own config. Used by `KnowledgeView` so that
    /// DB-reading acquirers (SQLite, Postgres) can live outside the
    /// `corpus-engine` crate, which stays free of database dependencies.
    #[serde(rename = "custom")]
    Custom {
        kind: String,
        #[serde(default)]
        params: serde_json::Value,
    },
}

// ---------------------------------------------------------------------------
// ExtractorConfig
// ---------------------------------------------------------------------------

/// Extraction shape for the Stack Exchange XML extractor. See the
/// `StackExchangeXml` variant of [`ExtractorConfig`] for the contract.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SeMode {
    /// One `ExtractedDoc` per high-score answer with the question
    /// inlined. The reference shape — pair with the `breadth` recipe.
    #[default]
    AnswerOnly,
    /// One `ExtractedDoc` per question, grouping up to
    /// `max_answers_per_question` top-scoring answers under a
    /// structured "Approach 1 / Approach 2" body. The knowledge shape
    /// — pair with the `passthrough` chunker and the `KnowledgeDensity`
    /// filter.
    QuestionWithAnswers,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ExtractorConfig {
    #[serde(rename = "mediawiki_xml")]
    MediawikiXml {
        #[serde(default = "default_namespace_filter")]
        namespace_filter: Vec<u32>,
        #[serde(default = "default_true")]
        skip_redirects: bool,
        #[serde(default)]
        decompress: Option<String>,
    },
    /// StackExchange XML data dump extractor.
    ///
    /// Supports two extraction shapes (`mode`):
    ///
    /// - [`SeMode::AnswerOnly`] (default — preserves the legacy
    ///   placeholder behaviour): emit one `ExtractedDoc` per high-score
    ///   answer with the question body inlined as `Q: … A (score N): …`.
    ///   The single-answer reference shape — pair with the `breadth`
    ///   recipe.
    /// - [`SeMode::QuestionWithAnswers`]: group up to
    ///   `max_answers_per_question` top-scoring answers under each
    ///   question and emit one `ExtractedDoc` per question. The full
    ///   thread becomes the FTS-indexed `content`; a synthesized
    ///   breadth summary (question title + first sentence of each
    ///   answer) is placed in `embed_text` so the vector embedding
    ///   captures the trade-off space without overflowing the embed
    ///   model's context window. Pair with the `passthrough` chunker.
    ///
    /// Knowledge-density signals (answer count, score, length, closed
    /// status, tag list) are written to each grouped doc's `metadata`
    /// so the [`KnowledgeDensity`](crate::filters::FilterConfig)
    /// document filter can reject single-answer reference posts. Set
    /// `apply_to` on the filter to scope the cut to specific
    /// communities (e.g. `"stackoverflow.com"`) while letting smaller,
    /// already knowledge-dense sites pass through.
    #[serde(rename = "stackexchange_xml")]
    StackExchangeXml {
        /// Minimum answer score to include (applies in both modes).
        /// Default 3 — community-validated answers, with one-line
        /// "just google it" noise excluded.
        #[serde(default = "default_min_score")]
        min_score: i32,

        /// Extraction mode. See `SeMode` for shape semantics.
        #[serde(default)]
        mode: SeMode,

        /// In `QuestionWithAnswers` mode, cap answers grouped under
        /// each question (sorted by score, ties broken by post id).
        /// Past 5 answers, marginal trade-off coverage drops sharply
        /// while the document grows past the embed context window.
        #[serde(default = "default_max_answers_per_question")]
        max_answers_per_question: usize,

        /// Reject answers shorter than this many characters. Filters
        /// out one-line code snippets and "+1 to the above" noise that
        /// inflate scores without adding retrievable knowledge.
        /// Default 0 (no length floor).
        #[serde(default)]
        min_answer_length: usize,

        /// Skip questions whose `ClosedDate` attribute is non-empty
        /// (Stack Overflow marks duplicates / off-topic / opinion-based
        /// questions this way). Default true — closed posts are
        /// systematically less knowledge-dense.
        #[serde(default = "default_true")]
        exclude_closed: bool,

        /// Restrict to questions tagged with at least one of these
        /// tags. `None` (default) means no tag filter. Tags are
        /// matched case-insensitively.
        #[serde(default)]
        tag_filter: Option<Vec<String>>,
    },
    #[serde(rename = "jsonl")]
    Jsonl {
        #[serde(default)]
        content_field: Option<String>,
        #[serde(default)]
        title_field: Option<String>,
        #[serde(default)]
        filter: Option<String>,
        #[serde(default)]
        decompress: Option<String>,
    },
    /// JSON-API extractor. Reads a single JSON file (typically the
    /// per-page response persisted by the `http_api` acquirer when
    /// `[acquire.follow]` is absent), runs `document_path` over it as
    /// JSONPath, and emits one [`ExtractedDoc`](crate::extractors::ExtractedDoc)
    /// per matching object using `content_field` for the body text.
    /// See [`crate::extractors::json_api::JsonApiExtractor`].
    #[serde(rename = "json")]
    Json {
        /// JSONPath expression selecting the documents array. Common
        /// shapes: `$.results[*]`, `$.data.items[*]`, `$.hits.hits[*]._source`.
        document_path: String,
        /// Required: name of the field on each matched object that
        /// holds the document's full text.
        content_field: String,
        #[serde(default)]
        title_field: Option<String>,
        #[serde(default)]
        url_field: Option<String>,
        #[serde(default)]
        id_field: Option<String>,
    },
    /// Deterministic tabular → typed-atom extractor for structured
    /// public datasets (e.g. the SF assessor parcel roll from DataSF's
    /// Socrata API). Reads the bare-array JSON the `http_api` acquirer
    /// persists and emits, per row: one chunk (a rendered, FTS-indexable
    /// line) AND — via the ingest flow — one atlas `Entity` atom whose
    /// declared numeric/string columns are recorded in
    /// `Entity::attributes`, the deterministic, cited substrate the LVT
    /// analytics sum over. No inference. Pair with
    /// `chunker = "passthrough"`. See
    /// [`crate::extractors::tabular_atoms`].
    #[serde(rename = "tabular_atoms")]
    TabularAtoms {
        /// JSONPath selecting the row array. Defaults to `$[*]` (a bare
        /// top-level array, as Socrata returns); use `$.results[*]` for
        /// an enveloped response.
        #[serde(default)]
        document_path: Option<String>,
        /// Column whose value is each row's stable identity (e.g.
        /// `parcel_number`). Drives the atom id + canonical name and the
        /// chunk's `source_doc_id`.
        id_column: String,
        /// Atom entity-type label (free-form; becomes
        /// `EntityType::Other(..)`). Defaults to `"row"`.
        #[serde(default)]
        entity_type: Option<String>,
        /// Columns parsed as numbers (string cells like `"172620.0"` are
        /// parsed) and stored as JSON numbers in `attributes`.
        #[serde(default)]
        numeric_attributes: Vec<String>,
        /// Columns kept verbatim as strings in `attributes`.
        #[serde(default)]
        string_attributes: Vec<String>,
    },
    #[serde(rename = "html")]
    Html {
        #[serde(default)]
        content_selector: Option<String>,
        #[serde(default)]
        title_selector: Option<String>,
    },
    /// Section-aware HTML extractor: emits one
    /// [`ExtractedDoc`](crate::extractors::ExtractedDoc) per
    /// regex-matched section per file. Use this when a domain expert
    /// (e.g. a financial journalist working with SEC filings) knows
    /// that the *interesting* text lives between specific headings —
    /// MD&A, related-party transactions, revenue disaggregation —
    /// and wants to ingest only those sections.
    ///
    /// When *no* section matches a file, the optional `fallback`
    /// block decides what to ingest (full document or first N
    /// characters). Without a fallback, the file is skipped.
    ///
    /// Misses are recorded in a sidecar `_section_misses.json`
    /// under the source directory so `sovereign recipe test` can
    /// surface "section X missed; nearby text: …; suggestion: …"
    /// for the recipe author. See [`SectionRule`] and
    /// [`FallbackRule`].
    #[serde(rename = "html_sections")]
    HtmlSections {
        sections: Vec<SectionRule>,
        #[serde(default)]
        fallback: Option<FallbackRule>,
        #[serde(default)]
        title_selector: Option<String>,
    },
    #[serde(rename = "csv")]
    Csv {
        content_column: String,
        #[serde(default)]
        title_column: Option<String>,
        #[serde(default)]
        delimiter: Option<char>,
    },
    /// Project Gutenberg catalog CSV (`pg_catalog.csv`). Emits one
    /// `ExtractedDoc` per `Text` work, with content = catalog
    /// metadata block and `embed_text` = a vector-friendly summary.
    /// Pair with `chunker = "passthrough"` and a `[catalog]` block.
    /// See [`crate::extractors::gutenberg_catalog`].
    #[serde(rename = "gutenberg_catalog")]
    GutenbergCatalog {},
    /// Wikipedia catalog — one chunk per article carrying title +
    /// abstract + section anchors. Pair with `chunker = "passthrough"`,
    /// `[corpus] kind = "catalog"`, and a `[catalog]` block whose
    /// `content_recipe` points at `wikipedia-article` for the per-
    /// article on-demand fetch. Source JSONL is produced offline by
    /// `sovereign-recipes/wikipedia-catalog/scripts/build_catalog.py`
    /// from the Wikimedia abstract dump.
    #[serde(rename = "wikipedia_catalog")]
    WikipediaCatalog {},
    /// Per-article on-demand extractor for Wikipedia. Consumes the
    /// MediaWiki Action API JSON (`action=parse&prop=wikitext|sections|
    /// links|properties`) and emits one `ExtractedDoc` per article
    /// section with full `WikipediaChunkMetadata` — same shape as
    /// the bulk JSONL extractor produces, so fetched articles are
    /// indistinguishable from dump-extracted ones downstream
    /// (atlas link graph, section-typed retrieval, contested-marker
    /// classification all work identically).
    #[serde(rename = "wikipedia_api_article")]
    WikipediaApiArticle {},
    #[serde(rename = "parquet")]
    Parquet {
        content_column: String,
        #[serde(default)]
        label_column: Option<String>,
        /// Optional column to use as the document URL (e.g. `"url"` in
        /// `wikimedia/wikipedia`). Populates search result source links.
        #[serde(default)]
        url_column: Option<String>,
        /// Optional transform applied to the content column before chunking.
        /// `"openalex_inverted_index"` reconstructs text from OpenAlex's
        /// inverted-index JSON format (`{ "word": [pos1, pos2], ... }`).
        #[serde(default)]
        content_transform: Option<String>,
    },
    #[serde(rename = "plaintext")]
    Plaintext {
        #[serde(default)]
        title_pattern: Option<String>,
        #[serde(default)]
        strip_boilerplate: Option<String>,
    },
    /// Extractor for the `wikimedia/structured-wikipedia` HuggingFace dataset
    /// in its parquet form. For the ZIP+JSONL form (the default distribution),
    /// use `WikipediaJsonl` instead.
    #[serde(rename = "wikipedia_structured")]
    WikipediaStructured {
        #[serde(default = "default_title_column")]
        title_column: String,
        #[serde(default = "default_url_column")]
        url_column: String,
        #[serde(default = "default_controversy_patterns")]
        controversy_patterns: Vec<String>,
        #[serde(default = "default_factual_patterns")]
        factual_patterns: Vec<String>,
        #[serde(default = "default_true")]
        structural_signals: bool,
    },
    /// Extractor for the `wikimedia/structured-wikipedia` dataset in its
    /// actual distribution format: a ZIP archive containing a JSONL file.
    /// Produces one `ExtractedDoc` per section with full `WikipediaChunkMetadata`
    /// (section type, revision ID, Wikidata QID, page ID, outgoing links).
    #[serde(rename = "wikipedia_jsonl")]
    WikipediaJsonl {
        #[serde(default = "default_controversy_patterns")]
        controversy_patterns: Vec<String>,
        #[serde(default = "default_factual_patterns")]
        factual_patterns: Vec<String>,
        /// Restrict processing to articles `[start, end)` in the JSONL.
        /// Set by the collaborative ingestion planner to partition the
        /// single-file Wikipedia JSONL across mesh nodes. `None` = all.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        article_range: Option<(u64, u64)>,
        /// Restrict processing to a specific set of **logical** shard
        /// indices over the ZIP's canonical JSONL entries (as produced
        /// by [`crate::engine::canonical_jsonl_shard_entries`], which
        /// filters out `__MACOSX/` and `._*` resource-fork junk).
        /// Set by the collaborative-ingestion planner for multi-shard
        /// JSONL corpora such as Wikipedia (76 shards). Mutually
        /// exclusive with `article_range` — the sharded path streams
        /// directly from the ZIP and skips the merged-JSONL cache.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shard_indices: Option<Vec<usize>>,
    },
    /// Tree-sitter code extractor. Walks the source directory, parses each
    /// supported file with its grammar, and yields one `ExtractedDoc` per
    /// symbol (function, class, struct, etc.). Requires the `treesitter`
    /// Cargo feature on `corpus-engine`.
    #[serde(rename = "code")]
    Code {
        #[serde(default = "default_code_context_lines")]
        context_lines: usize,
        #[serde(default = "default_code_max_lines")]
        max_lines_per_chunk: usize,
    },
    /// Section-aware markdown extractor. Walks a single `.md` file (or
    /// a directory of them) and yields one `ExtractedDoc` per
    /// heading-bounded section. Each chunk carries
    /// [`crate::extractors::markdown_types::MarkdownChunkMetadata`]
    /// (section_path, section_depth, heading_anchor, outgoing_links,
    /// inline_code_spans). Used by the narrative-stream branch of the
    /// two-stream atlas pipeline (CHARTER, ARCH_PRINCIPLES, ADRs,
    /// accepted spec.md files). Requires the `markdown` Cargo feature.
    #[serde(rename = "markdown")]
    Markdown {},
    /// Runtime-registered per-file extractor. The engine walks
    /// `source_path` collecting files with `extension`, then calls a
    /// closure registered via
    /// [`CorpusEngine::register_extractor`](crate::engine::CorpusEngine::register_extractor)
    /// on each. Used by recipes whose source format requires a heavy
    /// dep (pdf-extract, lopdf, …) that corpus-engine declines to
    /// bundle. `sovereign-tools` registers `"pdf"` at daemon startup.
    /// Ingest fails loudly if no extractor is registered for `kind` —
    /// the operator gets a clear "register before install" message
    /// rather than a silent empty corpus.
    #[serde(rename = "custom")]
    Custom {
        /// Key the engine looks up in its custom-extractor map.
        kind: String,
        /// File extension to walk (case-insensitive, no leading dot:
        /// `"pdf"`, `"epub"`, …).
        extension: String,
        /// Unstructured params forwarded to the closure's bookkeeping
        /// layer if needed (currently unused — reserved for per-recipe
        /// PDF settings like `ocr_fallback: true`).
        #[serde(default)]
        params: serde_json::Value,
    },
    /// Architecture-over-Enron Phase 2: RFC-5322 / MIME email
    /// extractor. Walks `source_path` recursively (maildir layout,
    /// raw `.eml` files), parses each through `mailparse`, and
    /// emits one [`ExtractedDoc`](crate::extractors::ExtractedDoc)
    /// per message. Metadata carries the parsed headers + a
    /// `thread_id` derived from In-Reply-To / References. When the
    /// engine has an [`crate::asset_store::AssetStore`] + an
    /// [`crate::extractors::described_asset::AssetSubExtractorRegistry`]
    /// installed (the default after Phase 1), attachments dispatch
    /// through the described-asset substrate — raw bytes + parsed
    /// caches + Asset atom + Attaches edge land per attachment.
    #[serde(rename = "email")]
    Email {
        /// Cap on per-message body bytes after MIME decoding. Long-
        /// tail bodies (200MB HTML newsletters) get truncated; the
        /// extractor sets a `body_was_truncated` flag in metadata.
        #[serde(default = "default_email_max_body_bytes")]
        max_body_bytes: usize,
        /// Per-attachment byte cap fed into the described-asset
        /// dispatcher. `0` = use the dispatcher's default.
        #[serde(default)]
        max_attachment_bytes: u64,
    },
    /// Architecture-over-Enron AD-3: the described-asset dispatcher.
    /// Walks `source_path` (one mixed-binary folder), hashes each
    /// file, picks a sub-extractor from the engine's
    /// [`AssetSubExtractorRegistry`](crate::extractors::described_asset::AssetSubExtractorRegistry)
    /// by magic-bytes / extension, and emits one
    /// [`ExtractedDoc`](crate::extractors::ExtractedDoc) per asset
    /// whose `content` is the description prose (always present —
    /// opaque-fallback at worst). The dispatcher writes raw bytes
    /// + optional typed parsed form to the engine's
    /// [`AssetStore`](crate::asset_store::AssetStore) and pre-forms
    /// the `Asset` atom + `Attaches` edge into the atlas sidecar so
    /// the next atlas write picks them up.
    ///
    /// Defaults: `xlsx` + `docx` + `plaintext` + `opaque` sub-
    /// extractors registered in-tree. `sovereign-tools` registers
    /// `pdf` at daemon startup the same way it does for the
    /// `Custom` PDF extractor today.
    #[serde(rename = "described_asset")]
    DescribedAsset {
        /// Maximum bytes the dispatcher will load into RAM per
        /// asset. Larger files fall through to the opaque fallback
        /// (no double-counting of GiB-scale videos). Defaults to
        /// 64 MiB.
        #[serde(default = "default_described_asset_max_bytes")]
        max_bytes_per_asset: u64,
    },
    /// Section-aware XML extractor. Walks a directory of `.xml`
    /// files and emits one
    /// [`ExtractedDoc`](crate::extractors::ExtractedDoc) per element
    /// whose **local-name** matches `element`. Namespace-agnostic on
    /// purpose so USLM 1.x and USLM 2.0 (different namespace URLs,
    /// same `<section>` semantics) both round-trip through the same
    /// recipe. `title_attr` reads a title off the matched element
    /// (e.g. `identifier` on USLM sections yields titles like
    /// `/us/usc/t15/s1`). See
    /// [`crate::extractors::xml_sections::XmlSectionsExtractor`].
    #[serde(rename = "xml_sections")]
    XmlSections {
        /// Local-name of the element whose body becomes one `ExtractedDoc`.
        element: String,
        /// Optional attribute (local-name) on the matched element used
        /// as the document title.
        #[serde(default)]
        title_attr: Option<String>,
    },
    /// Walks the user's `~/.claude/plans/` and
    /// `~/.claude/projects/-Users-*/memory/` trees plus
    /// `~/.claude/plans/_TEMPLATE.md`, yielding one `ExtractedDoc` per
    /// `.md` file with `source_id` set to the path relative to
    /// `~/.claude/`. Pairs with `mutable_merge =
    /// "source_doc_id_newest_mtime"` so two daemons editing the same
    /// memory or plan file converge on the newer copy after a mesh
    /// merge. The acquirer points at `~/.claude` (resolved by the
    /// `local_file` path-shape); the extractor handles its own
    /// directory walk for the canonical subset.
    #[serde(rename = "alignment_workspace")]
    AlignmentWorkspace {},
    /// Anthropic claude.ai chat-export extractor. Parses the
    /// `conversations.json` file produced by claude.ai's "Export
    /// data" download and emits one
    /// [`ExtractedDoc`](crate::extractors::ExtractedDoc) per
    /// conversation (`source_id = conv_uuid`) with content rendered
    /// as a sequence of `### [YYYY-MM-DD HH:MM] {user|assistant}`
    /// turn blocks. Empty conversations and non-text content blocks
    /// are dropped; messages flatten by `created_at` (branch handling
    /// via `parent_message_uuid` is a v2 concern). Pair with
    /// [`ChunkerConfig::ThreadedTurns`] so each retrieval unit is a
    /// user-question + assistant-reply pair. See
    /// [`crate::extractors::anthropic_export::AnthropicExportExtractor`].
    #[serde(rename = "anthropic_export")]
    AnthropicExport {},
    /// OpenAI ChatGPT chat-export extractor. Parses the
    /// `conversations.json` file produced by ChatGPT's "Export data"
    /// download and emits one
    /// [`ExtractedDoc`](crate::extractors::ExtractedDoc) per
    /// conversation (`source_id = conversation_id`) with content
    /// rendered as the *same* `### [YYYY-MM-DD HH:MM] {user|assistant}`
    /// turn blocks as [`ExtractorConfig::AnthropicExport`]. Unlike the
    /// Anthropic flat list, ChatGPT stores messages as a `mapping`
    /// tree; the extractor reconstructs the current thread by walking
    /// `parent` pointers up from `current_node`. Private-Use-Area
    /// inline markers (entity/url annotations) are cleaned to readable
    /// text. Pair with [`ChunkerConfig::ThreadedTurns`]. See
    /// [`crate::extractors::chatgpt_export::ChatgptExportExtractor`].
    #[serde(rename = "chatgpt_export")]
    ChatgptExport {},
}

fn default_code_context_lines() -> usize {
    3
}
fn default_code_max_lines() -> usize {
    150
}

// ---------------------------------------------------------------------------
// ChunkerConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ChunkerConfig {
    #[serde(rename = "paragraph")]
    Paragraph {
        #[serde(default = "default_max_chunk_chars")]
        max_chars: usize,
        #[serde(default = "default_overlap_chars")]
        overlap_chars: usize,
    },
    #[serde(rename = "sentence")]
    Sentence {
        #[serde(default = "default_max_chunk_chars")]
        max_chars: usize,
    },
    #[serde(rename = "fixed")]
    Fixed {
        #[serde(default = "default_max_chunk_chars")]
        max_chars: usize,
        #[serde(default = "default_overlap_chars")]
        overlap_chars: usize,
    },
    #[serde(rename = "semantic")]
    Semantic {
        #[serde(default = "default_max_chunk_chars")]
        max_chars: usize,
    },
    /// Emits the input text as a single chunk. Use when the extractor
    /// already produces chunk-sized output (e.g. the `code` extractor).
    #[serde(rename = "passthrough")]
    Passthrough,
    /// One chunk per `*`-prefixed bullet on a `Portal:Current_events`
    /// page. Sub-bullets fold under their parent. Used by the
    /// `wikipedia-newsworthy` recipe so each event is its own retrieval
    /// unit.
    #[serde(rename = "portal_event_bullet")]
    PortalEventBullet {
        #[serde(default = "default_portal_bullet_max_chars")]
        max_chars: usize,
    },
    /// Chunker for chat-transcript content rendered by the
    /// `anthropic_export` extractor (or any future extractor that
    /// emits `### [YYYY-MM-DD HH:MM] {user|assistant}` turn blocks).
    /// Groups each user turn with the immediately-following
    /// assistant reply into one chunk; dangling user turns and
    /// leading assistant turns become standalone chunks. Preserves
    /// turn headers in chunk content so downstream phases can read
    /// timestamps + first-person signals (meta-atlas trace axis) and
    /// so the plain-text chunk reads naturally in retrieval surfaces.
    /// Per-span authorship is surfaced through
    /// [`crate::chunkers::threaded_turns::AttributedChunk`] for code
    /// paths that consume attribution (atlas extraction,
    /// attribution-filtered retrieval, bench scoring).
    #[serde(rename = "threaded_turns")]
    ThreadedTurns,
}

fn default_portal_bullet_max_chars() -> usize {
    2048
}

// ---------------------------------------------------------------------------
// IndexConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexConfig {
    #[serde(default = "default_true")]
    pub fts: bool,
    #[serde(default = "default_true")]
    pub vector: bool,
    #[serde(default = "default_embedding_model")]
    pub embedding_model: String,
    #[serde(default = "default_embedding_dimensions")]
    pub embedding_dimensions: usize,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            fts: default_true(),
            vector: default_true(),
            embedding_model: default_embedding_model(),
            embedding_dimensions: default_embedding_dimensions(),
        }
    }
}

// ---------------------------------------------------------------------------
// Recipe parsing
// ---------------------------------------------------------------------------

/// Highest recipe `schema_version` this build of `corpus-engine`
/// understands. The reader refuses recipes with a `[corpus]
/// schema_version > MAX_SCHEMA_VERSION` so a recipe authored
/// against a newer engine surfaces as a clear "upgrade your
/// corpus-engine" error instead of silently parsing with missing
/// fields the recipe author expected to be honoured.
///
/// Bump this only when a NEW field requires reader cooperation
/// (e.g. a new acquirer that older engines can't run safely). Pure
/// additions — fields with `#[serde(default)]` — do NOT require a
/// bump because old engines tolerate them and new engines treat
/// missing values as default. See `tests/recipe_back_compat.rs`
/// for the back-compat policy in full.
pub const MAX_SCHEMA_VERSION: u32 = 1;

impl Recipe {
    /// Parse a `Recipe` from a TOML string.
    ///
    /// Three layers of guard:
    ///
    /// 1. Schema-version cap — refuse recipes declaring a future
    ///    `schema_version` so the loader fails loudly instead of
    ///    silently dropping fields the recipe author expected
    ///    the engine to honour.
    /// 2. Deprecation-aware error messages — recipes referencing
    ///    removed acquirer/extractor variants (e.g. the never-
    ///    implemented `api_paginated` from before PR1) get a
    ///    tailored "use `<replacement>` instead" message instead
    ///    of a generic `unknown variant` parse error.
    /// 3. Enrichment-domain gate — a `type = "field_model"` recipe
    ///    naming a domain the field-model registry doesn't carry is
    ///    refused HERE, rather than after acquire + extract + embed +
    ///    index have already run and the install strands a partition.
    ///    See [`check_enrichment_domain`](crate::recipe_parsing::check_enrichment_domain).
    ///
    /// This is the ONE recipe load boundary: [`Self::from_file`],
    /// `recipe_builtin`, and the desktop recipe author's validate
    /// preview all route through it. Anything that parses a `Recipe`
    /// with a bare `toml::from_str` skips all three guards.
    pub fn from_toml(toml_str: &str) -> Result<Self> {
        match toml::from_str::<Self>(toml_str) {
            Ok(recipe) => {
                check_schema_version(recipe.corpus.schema_version)?;
                crate::recipe_parsing::check_enrichment_domain(&recipe)?;
                Ok(recipe)
            }
            Err(e) => Err(translate_parse_error(e)),
        }
    }

    /// Load a `Recipe` from a `.toml` file on disk.
    pub fn from_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        Self::from_toml(&contents)
    }

    /// True when this recipe's enrichment will produce graph atoms (the
    /// `atlas/atoms.json` that the harness's rung 6 / `verify_atoms_at`
    /// checks). Only an **enabled** `atlas` (or `investigation`) enrichment
    /// does; the default `field_model` produces a skeleton with no atoms, and
    /// `enabled = false` produces nothing. Drives the dashboard's
    /// enrichment-readiness lint so a recipe that would enrich to zero atoms is
    /// flagged before the (expensive) build, rather than discovered after.
    pub fn produces_enriched_atoms(&self) -> bool {
        self.enrichment.as_ref().is_some_and(|e| {
            e.enabled && matches!(e.enrichment_type.as_str(), "atlas" | "investigation")
        })
    }

    /// Explicit opt-out from the default post-install structural-atlas hook.
    ///
    /// The product default is "every installed corpus gets a lightweight
    /// structural atlas (atoms/edges) plus the Tier-2 RAPTOR pass" — useful
    /// grounding for chat on most corpora, and the source of the desktop
    /// "Extracting atoms…" progress chip. A recipe declares itself
    /// **retrieval-only** by shipping an `[enrichment]` block with
    /// `enabled = false`; the post-install hook then skips that pass entirely,
    /// so retrieval stays sealed to the recipe's own chunks. This is the
    /// machine-readable form of "source-only evidence" — e.g. the chaos-monkey
    /// bench corpus, whose fairness premise requires that facts the source
    /// withholds (a withheld first name, an unnamed country) stay genuinely
    /// absent rather than being re-introduced by an LLM-generated RAPTOR
    /// summary.
    ///
    /// A MISSING `[enrichment]` section keeps the default-on hook — opting out
    /// is always explicit, never implied by omission. (Note: because
    /// `enabled` defaults to `false`, a present-but-bare `[enrichment]` block
    /// also opts out, consistent with [`Self::produces_enriched_atoms`].)
    pub fn opts_out_of_auto_enrichment(&self) -> bool {
        self.enrichment.as_ref().is_some_and(|e| !e.enabled)
    }

    /// The custom atlas ontology this recipe declares, if any. Returns `Some`
    /// only when `[enrichment.ontology]` is present with **non-empty**
    /// `guidance` — that's the signal to use the `ConfigurableAtlasPipeline`
    /// (`custom_atlas`) rather than a prebuilt genre pipeline. This is the top
    /// of the atlas-pipeline precedence chain: `custom_ontology()` →
    /// `enrichment.pipeline` pin → `enrichment.domain` heuristic. Callers that
    /// pick an atlas pipeline should consult this first.
    pub fn custom_ontology(&self) -> Option<&OntologyConfig> {
        self.enrichment
            .as_ref()
            .and_then(|e| e.ontology.as_ref())
            .filter(|o| !o.guidance.trim().is_empty())
    }

    /// Materialize this recipe's `[enrichment.ontology]` into a pipeline-ready
    /// [`crate::enrichment::pipeline::CustomAtlasSpec`] — the data `enrich init`
    /// persists into `config.json` and `resolve_pipeline` turns into a live
    /// `custom_atlas` pipeline. `name` comes from `enrichment.domain` (else the
    /// corpus id); `guidance` + `vocabulary` come from the ontology block.
    /// `None` when there is no custom ontology (non-empty guidance). This is the
    /// single mapping point recipe→pipeline so the two type families don't drift.
    pub fn custom_atlas_spec(&self) -> Option<crate::enrichment::pipeline::CustomAtlasSpec> {
        let ont = self.custom_ontology()?;
        let name = self
            .enrichment
            .as_ref()
            .and_then(|e| e.domain.clone())
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| self.corpus.id.clone());
        let vocabulary =
            ont.vocabulary
                .as_ref()
                .map(|v| crate::enrichment::pipeline::CustomVocabulary {
                    concern_term: v.concern_term.clone(),
                    position_term: v.position_term.clone(),
                    tension_term: v.tension_term.clone(),
                    absence_term: v.absence_term.clone(),
                    evidence_term: v.evidence_term.clone(),
                });
        Some(crate::enrichment::pipeline::CustomAtlasSpec {
            name,
            guidance: ont.guidance.clone(),
            vocabulary,
        })
    }

    /// Build a recipe with resolved parameter values stamped on
    /// (consumes self, returns a new value). Used by the CLI /
    /// desktop install path right before kicking off ingest:
    /// they parse the recipe TOML, prompt for parameters, validate
    /// via [`Recipe::resolve_parameters`], then stamp via this
    /// builder before handing to `CorpusEngine::ingest`.
    pub fn with_resolved_parameters(mut self, params: ResolvedParameters) -> Self {
        self.resolved_parameters = params;
        self
    }

    /// Validate user-supplied parameter values against the recipe's
    /// `[recipe.parameters]` schema and produce a [`ResolvedParameters`]
    /// the `http_api` acquirer (and the install-time CLI prompt) will
    /// consult during `{name}` interpolation.
    ///
    /// - Unknown keys in `provided` are rejected with
    ///   [`Error::InvalidInput`]: catching typos at install time is
    ///   far cheaper than discovering an empty corpus an hour later.
    /// - Missing required keys without a default are also rejected.
    /// - Defaults are applied verbatim from the recipe; type
    ///   coercion errors surface as `InvalidInput` with the offending
    ///   parameter name.
    /// - List parameters supplied as a single comma-separated string
    ///   are split — the CLI's interactive prompt yields one string,
    ///   so we accept both shapes here rather than pushing the split
    ///   responsibility upstream.
    pub fn resolve_parameters(
        &self,
        provided: &BTreeMap<String, toml::Value>,
    ) -> Result<ResolvedParameters> {
        // Reject unknown keys up front so misspellings surface loudly.
        for k in provided.keys() {
            if !self.parameters.contains_key(k) {
                let declared: Vec<&str> = self.parameters.keys().map(|s| s.as_str()).collect();
                return Err(Error::InvalidInput(format!(
                    "unknown parameter `{k}` for recipe `{}` (declared: [{}])",
                    self.corpus.id,
                    declared.join(", "),
                )));
            }
        }

        let mut values = BTreeMap::new();
        for (name, spec) in &self.parameters {
            let raw = provided.get(name).cloned().or_else(|| spec.default.clone());
            let value = match (raw, spec.required) {
                (Some(v), _) => parameter_value_from_toml(name, &spec.kind, v)?,
                (None, true) => {
                    return Err(Error::InvalidInput(format!(
                        "missing required parameter `{name}` for recipe `{}`",
                        self.corpus.id,
                    )));
                }
                (None, false) => empty_value(&spec.kind),
            };
            values.insert(name.clone(), value);
        }
        Ok(ResolvedParameters { values })
    }
}

/// Returns `Recipe` definitions for well-known corpora, loaded from the
/// `recipes/` directory at compile time via `include_str!`.
///
/// **For tests only.** Production code uses
/// `RecipeRegistry::fetch_recipe()` which checks local overrides,
/// fetches from the registry URL, and falls back to
/// [`bundled_recipe_toml`].
#[cfg(test)]
pub(crate) fn builtin_recipes() -> Vec<Recipe> {
    const IDS: &[&str] = &[
        "wikipedia",
        "wikipedia-simple",
        "stackexchange",
        "stackexchange-knowledge",
        "openalex",
        "gutenberg",
        "sep",
        "crs_reports",
    ];
    IDS.iter()
        .map(|id| {
            let toml = bundled_recipe_toml(id).expect("bundled recipe present");
            Recipe::from_toml(toml).expect("built-in recipe.toml failed to parse")
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact failure modes seen in the recipe-author live trial.
    /// Each case targets a real validation message a non-technical
    /// user would see through the desktop UI, and asserts the rewrite
    /// surfaces remediation guidance instead of the raw serde
    /// "TOML parse error at line 1" framing.
    #[test]
    fn translate_missing_acquire_section_names_the_section_and_lists_types() {
        // No `[acquire]` block at all — the failure mode in the
        // first agent draft of every recipe-author trial.
        let toml_str = r#"
[corpus]
id = "x"
name = "x"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"
"#;
        let err = Recipe::from_toml(toml_str).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("[acquire]"), "should name the section: {msg}");
        assert!(msg.contains("http_api"), "should list valid types: {msg}");
        assert!(
            !msg.starts_with("TOML parse error"),
            "rewrite should drop the misleading line-1 framing: {msg}",
        );
    }

    #[test]
    fn translate_missing_type_in_acquire_lists_no_specific_types() {
        // `[acquire]` present but missing `type` — needs a generic
        // "look at caret" hint since we can't tell which section.
        let toml_str = r#"
[corpus]
id = "x"
name = "x"

[acquire]
base_url = "https://example.com"

[[acquire.requests]]
url = "{base_url}/x"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"
"#;
        let err = Recipe::from_toml(toml_str).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("`type`"),
            "should name the missing field: {msg}"
        );
        assert!(
            msg.contains("acquirer") || msg.contains("section"),
            "should hint at which section: {msg}",
        );
    }

    #[test]
    fn translate_unknown_variant_pdf_names_field_and_lists_allowed() {
        // The bw9pkay71 trial failure: `document_format = "pdf"`.
        let toml_str = r#"
[corpus]
id = "x"
name = "x"

[acquire]
type = "http_api"
base_url = "https://example.com"

[[acquire.requests]]
url = "{base_url}/x"

[acquire.follow]
document_url_path = "$.urls[*]"
document_format = "pdf"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"
"#;
        let err = Recipe::from_toml(toml_str).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("pdf"), "should quote the bad value: {msg}");
        assert!(
            msg.contains("html") && msg.contains("json"),
            "should list allowed values: {msg}",
        );
        // Best-effort field-name extraction — should at least
        // recognise the bad value lives in `document_format`.
        assert!(
            msg.contains("document_format") || msg.contains("field"),
            "should hint at the field name when the span carries it: {msg}",
        );
    }

    #[test]
    fn translate_falls_through_for_unrecognised_errors() {
        // A genuinely unexpected parse error should still surface.
        let toml_str = "this is not valid TOML at all = = =";
        let err = Recipe::from_toml(toml_str).unwrap_err();
        let msg = format!("{err}");
        assert!(!msg.is_empty(), "should still surface something: {msg}");
    }

    #[test]
    fn extract_missing_field_pulls_field_from_serde_message() {
        let raw = "TOML parse error\n  |\n1 | [acquire]\n  | ^^^^^^^^^\nmissing field `type`";
        assert_eq!(extract_missing_field(raw).as_deref(), Some("type"));
        let raw2 = "missing field `acquire`";
        assert_eq!(extract_missing_field(raw2).as_deref(), Some("acquire"));
        assert_eq!(extract_missing_field("no anchor here"), None);
    }

    #[test]
    fn extract_unknown_variant_pulls_value_and_allowed_list() {
        let raw = "unknown variant `pdf`, expected one of `html`, `json`, `xml`, `plaintext`";
        let (val, allowed) = extract_unknown_variant(raw).unwrap();
        assert_eq!(val, "pdf");
        assert!(allowed.contains("html"));
        assert!(allowed.contains("plaintext"));
    }

    #[test]
    fn round_trip_serialization() {
        let recipe = &builtin_recipes()[0]; // wikipedia
        let toml_str = toml::to_string(recipe).expect("serialize to TOML");
        let parsed: Recipe = Recipe::from_toml(&toml_str).expect("deserialize from TOML");
        assert_eq!(parsed.corpus.id, recipe.corpus.id);
        assert_eq!(parsed.corpus.name, recipe.corpus.name);
        assert_eq!(parsed.corpus.mesh_sharing, recipe.corpus.mesh_sharing);
    }

    #[test]
    fn catalog_recipe_round_trips() {
        let toml_str = r#"
[corpus]
id = "gutenberg"
name = "Project Gutenberg Catalog"
license = "Public Domain"
kind = "catalog"
mesh_sharing = true

[acquire]
type = "bulk_download"
url = "https://www.gutenberg.org/cache/epub/feeds/pg_catalog.csv.gz"

[extract]
type = "gutenberg_catalog"

[chunk]
type = "passthrough"

[index]
fts = true
vector = true

[catalog]
id_field = "gutenberg_id"
download_url_template = "https://www.gutenberg.org/cache/epub/{id}/pg{id}.txt"
content_recipe = "gutenberg-work"
ingest_estimate_wpm = 8000
enrich_estimate_wpm = 500
"#;
        let r = Recipe::from_toml(toml_str).expect("catalog recipe must parse");
        assert_eq!(r.corpus.kind, crate::types::CorpusKind::Catalog);
        assert!(!r.corpus.on_demand);
        assert!(matches!(r.extract, ExtractorConfig::GutenbergCatalog {}));
        let cat = r.catalog.expect("[catalog] block parsed");
        assert_eq!(cat.id_field, "gutenberg_id");
        assert_eq!(cat.content_recipe, "gutenberg-work");
        assert!(cat.download_url_template.contains("{id}"));
        assert_eq!(cat.ingest_estimate_wpm, Some(8000));
    }

    #[test]
    fn on_demand_recipe_round_trips() {
        let toml_str = r#"
[corpus]
id = "gutenberg-work"
name = "Project Gutenberg — Single Work"
license = "Public Domain"
on_demand = true
mesh_sharing = true

[acquire]
type = "bulk_download"
url = "https://example.com/PLACEHOLDER"

[extract]
type = "plaintext"

[chunk]
type = "sentence"
max_chars = 2048
"#;
        let r = Recipe::from_toml(toml_str).expect("on-demand recipe must parse");
        assert!(r.corpus.on_demand);
        assert_eq!(r.corpus.kind, crate::types::CorpusKind::Knowledge);
    }

    #[test]
    fn recipe_id_from_id_round_trips_for_every_variant() {
        // Every RecipeId variant's id() must round-trip through
        // from_id() — pin the wire-form contract per
        // ARCH_PRINCIPLES.md §2.2 (legacy_view_id_constants_match_view_kind
        // is the reference pattern).
        for &recipe_id in RecipeId::ALL {
            let wire = recipe_id.id();
            assert_eq!(
                RecipeId::from_id(wire),
                Some(recipe_id),
                "RecipeId::{recipe_id:?} ↔ {wire:?} round-trip broke"
            );
            // bundled_toml() must also resolve. include_str! enforces
            // the file exists at compile time; this just checks
            // non-empty content reached us.
            assert!(
                !recipe_id.bundled_toml().is_empty(),
                "RecipeId::{recipe_id:?}.bundled_toml() returned empty",
            );
        }
    }

    #[test]
    fn recipe_id_dispatch_matches_string_adapter() {
        // bundled_recipe_toml(&str) must agree with
        // RecipeId::<v>.bundled_toml() byte-for-byte. Catches a
        // case where the adapter falls behind the enum.
        for &recipe_id in RecipeId::ALL {
            let via_adapter = bundled_recipe_toml(recipe_id.id())
                .expect("adapter returned None for known recipe id");
            let via_enum = recipe_id.bundled_toml();
            assert_eq!(
                via_adapter, via_enum,
                "RecipeId::{recipe_id:?} dispatch mismatch between adapter and enum"
            );
        }
    }

    #[test]
    fn bundled_recipe_toml_unknown_id_returns_none() {
        assert!(bundled_recipe_toml("does-not-exist").is_none());
        assert!(RecipeId::from_id("does-not-exist").is_none());
    }

    #[test]
    fn bundled_gutenberg_recipes_parse() {
        // Both the catalog (`gutenberg`) and on-demand work
        // (`gutenberg-work`) recipes must always be loadable from the
        // bundled snapshot — the on-demand ingest path resolves them
        // by id at runtime.
        for id in &["gutenberg", "gutenberg-work"] {
            let toml = bundled_recipe_toml(id)
                .unwrap_or_else(|| panic!("bundled recipe `{id}` is missing"));
            let r = Recipe::from_toml(toml)
                .unwrap_or_else(|e| panic!("bundled recipe `{id}` parse error: {e}"));
            assert_eq!(r.corpus.id, *id);
        }
    }

    #[test]
    fn legacy_recipes_without_filter_block_parse() {
        // Recipes from before the `[[filter]]` extension must still
        // deserialize cleanly. The `filters` field defaults to empty.
        let toml_str = r#"
[corpus]
id = "wikipedia"
name = "Wikipedia"

[acquire]
type = "bulk_download"
url = "https://example.com/data.zip"

[extract]
type = "wikipedia_jsonl"

[chunk]
type = "paragraph"
"#;
        let r = Recipe::from_toml(toml_str).expect("legacy recipe must parse");
        assert!(r.filters.is_empty());
        assert_eq!(r.filter_mode.mode, ComposeMode::Any); // default
    }

    #[test]
    fn filter_block_round_trips() {
        let toml_str = r#"
[corpus]
id = "wikipedia"
name = "Wikipedia"

[acquire]
type = "bulk_download"
url = "https://example.com/data.zip"

[extract]
type = "wikipedia_jsonl"

[chunk]
type = "paragraph"

[[filter]]
type = "pageview_rank"
rank_file = "@bundled:pageview_ranks_202311"
max_rank = 100000

[[filter]]
type = "title_list"
list_file = "@bundled:vital_articles_l5"

[filter_mode]
mode = "any"
"#;
        let r = Recipe::from_toml(toml_str).expect("recipe with filters must parse");
        assert_eq!(r.filters.len(), 2);
        assert_eq!(r.filter_mode.mode, ComposeMode::Any);
        match &r.filters[0] {
            FilterConfig::PageviewRank {
                rank_file,
                max_rank,
            } => {
                assert_eq!(rank_file, "@bundled:pageview_ranks_202311");
                assert_eq!(*max_rank, 100_000);
            }
            other => panic!("expected pageview_rank, got {other:?}"),
        }
        match &r.filters[1] {
            FilterConfig::TitleList { list_file } => {
                assert_eq!(list_file, "@bundled:vital_articles_l5");
            }
            other => panic!("expected title_list, got {other:?}"),
        }
    }

    #[test]
    fn filter_mode_all_round_trips() {
        let toml_str = r#"
[corpus]
id = "x"
name = "x"

[acquire]
type = "bulk_download"
url = "https://example.com/x.zip"

[extract]
type = "wikipedia_jsonl"

[chunk]
type = "paragraph"

[[filter]]
type = "title_list"
list_file = "@bundled:vital_articles_l5"

[filter_mode]
mode = "all"
"#;
        let r = Recipe::from_toml(toml_str).unwrap();
        assert_eq!(r.filter_mode.mode, ComposeMode::All);
    }

    #[test]
    fn parse_mediawiki_xml_recipe_from_toml() {
        let toml_str = r#"
[corpus]
id = "wikipedia"
name = "Wikipedia (English)"
description = "English Wikipedia dump"
license = "CC-BY-SA-4.0"

[acquire]
type = "bulk_download"
url = "https://dumps.wikimedia.org/enwiki/latest/enwiki-latest-pages-articles.xml.bz2"

[extract]
type = "mediawiki_xml"
decompress = "bzip2"

[chunk]
type = "paragraph"
"#;
        let recipe = Recipe::from_toml(toml_str).expect("should parse wikipedia recipe");
        assert_eq!(recipe.corpus.id, "wikipedia");
        assert!(recipe.corpus.mesh_sharing);

        match &recipe.acquire {
            AcquirerConfig::BulkDownload { url, urls, resume } => {
                assert!(urls.is_none());
                assert!(url.as_deref().unwrap().contains("wikimedia"));
                assert!(*resume); // default
            }
            _ => panic!("expected BulkDownload"),
        }

        match &recipe.extract {
            ExtractorConfig::MediawikiXml {
                namespace_filter,
                skip_redirects,
                decompress,
            } => {
                assert_eq!(*namespace_filter, vec![0]); // default
                assert!(*skip_redirects); // default
                assert_eq!(decompress.as_deref(), Some("bzip2"));
            }
            _ => panic!("expected MediawikiXml"),
        }

        match &recipe.chunk {
            ChunkerConfig::Paragraph {
                max_chars,
                overlap_chars,
            } => {
                assert_eq!(*max_chars, 2048); // default
                assert_eq!(*overlap_chars, 256); // default
            }
            _ => panic!("expected Paragraph chunker"),
        }

        // IndexConfig should use defaults
        assert!(recipe.index.fts);
        assert!(recipe.index.vector);
        assert_eq!(recipe.index.embedding_model, "qwen3-embedding-0.6b");
        assert_eq!(recipe.index.embedding_dimensions, 0); // 0 = auto-detect
    }

    #[test]
    fn builtin_recipes_count() {
        let recipes = builtin_recipes();
        assert_eq!(recipes.len(), 8);
    }

    #[test]
    fn builtin_recipes_have_valid_ids() {
        let expected_ids = [
            "wikipedia",
            "wikipedia-simple",
            "stackexchange",
            "stackexchange-knowledge",
            "openalex",
            "gutenberg",
            "sep",
            "crs_reports",
        ];
        let recipes = builtin_recipes();
        for (recipe, expected_id) in recipes.iter().zip(expected_ids.iter()) {
            assert_eq!(&recipe.corpus.id, expected_id, "unexpected id for recipe");
            assert!(!recipe.corpus.id.is_empty(), "recipe id must not be empty");
            assert!(
                !recipe.corpus.name.is_empty(),
                "recipe name must not be empty"
            );
        }
    }

    /// The SEP recipe is the demo target for the epistemic enrichment
    /// layer and the canonical example of a parquet-sourced corpus.
    /// These assertions guard against accidental regression to the old
    /// HTML web-crawl path or the wrong source URL.
    #[test]
    fn sep_recipe_uses_huggingface_parquet_source() {
        let recipes = builtin_recipes();
        let sep = recipes
            .iter()
            .find(|r| r.corpus.id == "sep")
            .expect("SEP recipe should be in builtin_recipes()");

        // Acquirer must be a bulk download from HuggingFace, not a web crawl.
        match &sep.acquire {
            AcquirerConfig::BulkDownload { url, urls, resume } => {
                assert!(urls.is_none(), "SEP recipe is single-source");
                let url = url.as_deref().expect("SEP recipe sets `url`");
                assert!(
                    url.contains("huggingface.co"),
                    "SEP source should be hosted on HuggingFace, got: {url}"
                );
                assert!(
                    url.contains(".parquet"),
                    "SEP source should be a parquet file, got: {url}"
                );
                assert!(*resume, "SEP downloads should support resume");
            }
            other => panic!("SEP must use BulkDownload, got {other:?}"),
        }

        // Extractor must be Parquet pointed at the right columns.
        match &sep.extract {
            ExtractorConfig::Parquet {
                content_column,
                label_column,
                ..
            } => {
                assert_eq!(content_column, "text");
                assert_eq!(label_column.as_deref(), Some("category"));
            }
            other => panic!("SEP must use Parquet extractor, got {other:?}"),
        }
    }

    #[test]
    fn declared_artifact_rel_path_maps_known_types() {
        // Minimal TOML — every other EnrichmentConfig field is serde-default.
        let atlas: EnrichmentConfig = toml::from_str("enabled = true\ntype = \"atlas\"").unwrap();
        assert_eq!(atlas.declared_artifact_rel_path(), Some("atlas/atoms.json"));

        let field: EnrichmentConfig =
            toml::from_str("enabled = true\ntype = \"field_model\"").unwrap();
        assert_eq!(
            field.declared_artifact_rel_path(),
            Some("field_skeleton.json")
        );

        // Artifact-less / unrecognised types assert no drift (conservative).
        let investigation: EnrichmentConfig =
            toml::from_str("enabled = true\ntype = \"investigation\"").unwrap();
        assert_eq!(investigation.declared_artifact_rel_path(), None);
    }

    #[test]
    fn sep_recipe_has_enrichment_enabled() {
        let recipes = builtin_recipes();
        let sep = recipes
            .iter()
            .find(|r| r.corpus.id == "sep")
            .expect("SEP recipe should be in builtin_recipes()");

        let enrichment = sep
            .enrichment
            .as_ref()
            .expect("SEP must have an enrichment block");

        assert!(enrichment.enabled, "SEP enrichment must be enabled");
        // Landing 3.B flipped SEP from the v1 field_model to the v2
        // per-article atlas flow (`sovereign enrich sep-ingest`).
        // The type tag changes together with `[enrichment.chunking]`
        // appearing in the recipe — both surface as "atlas is the
        // primary surface". The legacy field_model config nests
        // under `[enrichment.field_model]` for the full-parquet
        // build path.
        assert_eq!(
            enrichment.enrichment_type, "atlas",
            "SEP enrichment type must be `atlas` (flipped in Landing 3.B)"
        );
        assert_eq!(
            enrichment.domain.as_deref(),
            Some("philosophy"),
            "SEP enrichment domain must be philosophy"
        );
        assert!(
            enrichment.prompt_version.is_some(),
            "SEP must have a prompt_version"
        );

        // Clustering config
        let clustering = enrichment
            .clustering
            .as_ref()
            .expect("SEP must have clustering config");
        assert_eq!(clustering.min_cluster_size, Some(50));

        // Alignment config
        let alignment = enrichment
            .alignment
            .as_ref()
            .expect("SEP must have alignment config");
        assert!(alignment.alignment_threshold.is_some());

        // Fault lines config
        let fault_lines = enrichment
            .fault_lines
            .as_ref()
            .expect("SEP must have fault_lines config");
        assert!(fault_lines.min_confidence.is_some());
    }

    #[test]
    fn sep_recipe_size_estimate_matches_huggingface_dataset() {
        let recipes = builtin_recipes();
        let sep = recipes.iter().find(|r| r.corpus.id == "sep").unwrap();
        // The HuggingFace parquet is roughly 1–2 GB compressed and
        // expands to several GB indexed once embeddings + claims are
        // included. The old 0.5/1.5 estimates were wildly wrong.
        assert!(
            sep.corpus.size_compressed_gb >= 1.0,
            "SEP compressed size should reflect the real ~1.4 GB parquet, got {}",
            sep.corpus.size_compressed_gb
        );
        assert!(
            sep.corpus.size_indexed_gb >= 4.0,
            "SEP indexed size should account for embeddings + enrichment, got {}",
            sep.corpus.size_indexed_gb
        );
    }

    #[test]
    fn gutenberg_recipe_is_a_catalog() {
        // Updated for the catalog-corpus paradigm: `gutenberg`
        // is now the metadata catalog (one chunk per work) and
        // pairs with the on-demand `gutenberg-work` content
        // recipe. The previous all-of-Gutenberg HuggingFace
        // parquet ingest is retired — see
        // `let-s-build-out-the-majestic-neumann.md` plan file.
        let recipes = builtin_recipes();
        let gut = recipes
            .iter()
            .find(|r| r.corpus.id == "gutenberg")
            .expect("gutenberg recipe must exist");
        assert_eq!(gut.corpus.kind, crate::types::CorpusKind::Catalog);
        match &gut.acquire {
            AcquirerConfig::BulkDownload { url, .. } => {
                let u = url.as_deref().unwrap_or("");
                assert!(
                    u.contains("pg_catalog.csv"),
                    "expected pg_catalog.csv URL, got {u:?}"
                );
            }
            other => panic!("expected BulkDownload, got {other:?}"),
        }
        match &gut.extract {
            ExtractorConfig::GutenbergCatalog {} => {}
            other => panic!("expected GutenbergCatalog extractor, got {other:?}"),
        }
        let cat = gut.catalog.as_ref().expect("[catalog] block required");
        assert_eq!(cat.content_recipe, "gutenberg-work");
    }

    #[test]
    fn huggingface_dataset_variant_round_trips_toml() {
        let toml_str = r#"
[corpus]
id = "gutenberg"
name = "Project Gutenberg"

[acquire]
type = "huggingface_dataset"
repo = "manu/project_gutenberg"
subset = "en"

[extract]
type = "parquet"
content_column = "text"

[chunk]
type = "paragraph"
"#;
        let recipe = Recipe::from_toml(toml_str).expect("should parse");
        match &recipe.acquire {
            AcquirerConfig::HuggingFaceDataset { repo, subset, .. } => {
                assert_eq!(repo, "manu/project_gutenberg");
                assert_eq!(subset.as_deref(), Some("en"));
            }
            _ => panic!("wrong acquirer variant after TOML round-trip"),
        }
    }

    #[test]
    fn wikipedia_recipe_uses_structured_jsonl() {
        let recipes = builtin_recipes();
        let wp = recipes
            .iter()
            .find(|r| r.corpus.id == "wikipedia")
            .expect("wikipedia recipe must exist");

        // structured_wikipedia was removed in favour of the single wikipedia recipe.
        assert!(
            recipes
                .iter()
                .all(|r| r.corpus.id != "structured_wikipedia"),
            "structured_wikipedia recipe should have been removed"
        );

        match &wp.acquire {
            AcquirerConfig::BulkDownload { url, .. } => {
                let url = url.as_deref().expect("wikipedia recipe is single-source");
                assert!(
                    url.contains("structured-wikipedia"),
                    "wikipedia recipe must download from structured-wikipedia"
                );
                assert!(url.ends_with(".zip"), "download URL must be a ZIP file");
            }
            other => panic!("expected BulkDownload, got {other:?}"),
        }

        match &wp.extract {
            ExtractorConfig::WikipediaJsonl { .. } => {}
            other => panic!("expected WikipediaJsonl extractor, got {other:?}"),
        }

        // Wikipedia Core ships with enrichment OFF — Layer 1 prioritises
        // time-to-grounded over atlas depth; users who promote to Full
        // can flip it on. The enrichment block is still present so the
        // settings/UX layer can preview the eventual config.
        let enrichment = wp
            .enrichment
            .as_ref()
            .expect("wikipedia must have enrichment block");
        assert!(
            !enrichment.enabled,
            "Core must ship with enrichment disabled"
        );
        assert_eq!(enrichment.enrichment_type, "field_model");
        assert_eq!(enrichment.domain.as_deref(), Some("multi"));

        let update = wp
            .update
            .as_ref()
            .expect("wikipedia must have update config");
        assert!(update.auto_update);
        assert!(!update.manifest_url.is_empty());

        // Core scope filter: Vital Articles Level 5 only. Pageview-rank
        // bundling was deliberately dropped — see
        // `corpus-engine/src/filters/assets.rs` for the rationale.
        assert_eq!(
            wp.filters.len(),
            1,
            "Wikipedia Core ships with a single Vital Articles filter"
        );
        match &wp.filters[0] {
            FilterConfig::TitleList { list_file } => {
                assert!(
                    list_file.contains("vital_articles"),
                    "Wikipedia Core filter must reference the vital articles list, got {list_file}"
                );
            }
            other => panic!("Wikipedia Core filter must be title_list, got {other:?}"),
        }
    }

    #[test]
    fn wikipedia_simple_recipe_loads_clean() {
        let recipes = builtin_recipes();
        let simple = recipes
            .iter()
            .find(|r| r.corpus.id == "wikipedia-simple")
            .expect("wikipedia-simple recipe must exist");
        match &simple.acquire {
            AcquirerConfig::HuggingFaceDataset { repo, subset, .. } => {
                assert_eq!(repo, "wikimedia/wikipedia");
                assert_eq!(subset.as_deref(), Some("20231101.simple"));
            }
            other => panic!("expected HuggingFaceDataset, got {other:?}"),
        }
        match &simple.extract {
            // Recipe was migrated from the WikipediaStructured
            // section-aware extractor to the simpler Parquet
            // extractor in 57a6205 (the `wikimedia/wikipedia`
            // parquet snapshot is already article-grained, so
            // section-splitting buys nothing for Layer 0). Keep the
            // shape assertion pinned to the current form.
            ExtractorConfig::Parquet { content_column, .. } => {
                assert_eq!(content_column, "text");
            }
            other => panic!("expected Parquet, got {other:?}"),
        }
        // Layer 0 is intentionally unfiltered and unenriched.
        assert!(
            simple.filters.is_empty(),
            "Simple English should not have filters"
        );
        let enrichment = simple
            .enrichment
            .as_ref()
            .expect("enrichment block present");
        assert!(!enrichment.enabled);
    }

    #[test]
    fn wikipedia_structured_variant_round_trips_toml() {
        let toml_str = r#"
[corpus]
id = "structured_wikipedia"
name = "Wikipedia (Structured)"

[acquire]
type = "huggingface_dataset"
repo = "wikimedia/structured-wikipedia"
subset = "20240916.en"

[extract]
type = "wikipedia_structured"

[chunk]
type = "paragraph"
"#;
        let recipe = Recipe::from_toml(toml_str).expect("should parse wikipedia_structured recipe");
        match &recipe.extract {
            ExtractorConfig::WikipediaStructured {
                title_column,
                url_column,
                structural_signals,
                ..
            } => {
                assert_eq!(title_column, "name"); // default
                assert_eq!(url_column, "url"); // default
                assert!(*structural_signals); // default
            }
            other => panic!("expected WikipediaStructured, got {other:?}"),
        }
    }

    /// The knowledge layer recipe must wire together the
    /// question-with-answers extractor, the knowledge-density filter
    /// (scoped to Stack Overflow), the passthrough chunker (so the
    /// embed_text override actually fires), and the engineering
    /// enrichment domain. Drift on any of these silently degrades
    /// retrieval shape — keep them pinned by test.
    #[test]
    fn stackexchange_knowledge_recipe_wires_the_full_pipeline() {
        let recipes = builtin_recipes();
        let r = recipes
            .iter()
            .find(|r| r.corpus.id == "stackexchange-knowledge")
            .expect("recipe present");

        // Multi-source bulk download from the IA mirror — Core scope
        // is just the small charter sites for fast first install. SO
        // Posts is opt-in via expand, not bundled by default.
        match &r.acquire {
            AcquirerConfig::BulkDownload { url, urls, .. } => {
                assert!(url.is_none(), "knowledge recipe is multi-source");
                let urls = urls.as_ref().expect("multi-source URLs");
                assert!(urls.iter().any(|u| u.contains("softwareengineering")));
                assert!(urls.iter().any(|u| u.contains("dba")));
                assert!(
                    !urls.iter().any(|u| u.contains("stackoverflow.com-Posts")),
                    "Core scope must not bundle SO Posts (17 GB) — opt-in via expand"
                );
            }
            other => panic!("expected BulkDownload, got {other:?}"),
        }

        // Question-with-answers extractor with sane density-aware defaults.
        match &r.extract {
            ExtractorConfig::StackExchangeXml {
                mode,
                max_answers_per_question,
                exclude_closed,
                ..
            } => {
                assert_eq!(*mode, SeMode::QuestionWithAnswers);
                assert!(*max_answers_per_question >= 3);
                assert!(*exclude_closed);
            }
            other => panic!("expected StackExchangeXml extractor, got {other:?}"),
        }

        // KnowledgeDensity filter scoped to SO only.
        assert!(
            !r.filters.is_empty(),
            "knowledge recipe must declare a knowledge_density filter"
        );
        match &r.filters[0] {
            crate::filters::FilterConfig::KnowledgeDensity(cfg) => {
                assert!(cfg.min_substantive_answers >= 2);
                let apply = cfg
                    .apply_to
                    .as_ref()
                    .expect("apply_to should scope SO only");
                assert!(apply.iter().any(|s| s == "stackoverflow.com"));
            }
            other => panic!("expected KnowledgeDensity filter, got {other:?}"),
        }

        // Passthrough chunker — required for embed_text override.
        assert!(matches!(r.chunk, ChunkerConfig::Passthrough));

        // Engineering enrichment domain declared (even if disabled).
        let enrichment = r.enrichment.as_ref().expect("enrichment block declared");
        assert_eq!(enrichment.domain.as_deref(), Some("engineering"));
        assert!(
            !enrichment.enabled,
            "MVP keeps enrichment off until prompts land"
        );
    }

    /// The breadth/reference recipe stays simple: HuggingFace parquet
    /// source, no enrichment. Test guards against regressions where a
    /// future change accidentally shapes it as a knowledge layer.
    #[test]
    fn stackexchange_breadth_recipe_is_reference_shape() {
        let recipes = builtin_recipes();
        let r = recipes
            .iter()
            .find(|r| r.corpus.id == "stackexchange")
            .expect("recipe present");
        assert!(matches!(
            r.acquire,
            AcquirerConfig::HuggingFaceDataset { .. }
        ));
        assert!(matches!(r.extract, ExtractorConfig::Parquet { .. }));
        assert!(
            r.filters.is_empty(),
            "breadth layer takes the dataset as-is"
        );
        assert!(
            r.enrichment.as_ref().map(|e| !e.enabled).unwrap_or(true),
            "breadth layer must not enable enrichment"
        );
    }

    /// Watcher-driven recipes carry `[update] ingest_driver = "watcher"`.
    /// `CorpusEngine::ingest` short-circuits on this signal to "create
    /// empty index + return" instead of running the recipe's acquire
    /// pipeline — which for wikipedia-newsworthy would otherwise trip
    /// the template validator on the watcher's `{date_yyyy_month_dd}`
    /// placeholder. Test guards against accidental field removal that
    /// would silently re-enable the broken install path.
    #[test]
    fn wikipedia_layers_declare_parent_corpus_id() {
        // Both `wikipedia-simple` (Layer 0) and `wikipedia-newsworthy`
        // (Layer 2) must declare `parent_corpus_id = "wikipedia"` so
        // the desktop picker can group them under the Core row instead
        // of rendering them as separate top-level entries.
        for id in ["wikipedia-simple", "wikipedia-newsworthy"] {
            let toml =
                bundled_recipe_toml(id).unwrap_or_else(|| panic!("{id} must be a bundled recipe"));
            let r = Recipe::from_toml(toml)
                .unwrap_or_else(|e| panic!("{id} recipe.toml must parse: {e}"));
            assert_eq!(
                r.corpus.parent_corpus_id.as_deref(),
                Some("wikipedia"),
                "{id} must declare parent_corpus_id=\"wikipedia\" so the \
                 desktop groups it under the Core Wikipedia row"
            );
        }

        // Counter-example: the Core wikipedia recipe itself must NOT
        // declare a parent, otherwise it'd disappear from the picker.
        let core = bundled_recipe_toml("wikipedia").expect("wikipedia bundled");
        let parsed = Recipe::from_toml(core).expect("wikipedia parses");
        assert!(
            parsed.corpus.parent_corpus_id.is_none(),
            "the Core wikipedia recipe must not declare a parent_corpus_id"
        );
    }

    #[test]
    fn wikipedia_newsworthy_declares_watcher_ingest_driver() {
        // `builtin_recipes()`'s IDS list deliberately excludes the
        // newsworthy recipe (it has no acquire-pipeline use), so we
        // parse the bundled TOML directly.
        let toml = bundled_recipe_toml("wikipedia-newsworthy")
            .expect("wikipedia-newsworthy must be a bundled recipe");
        let r = Recipe::from_toml(toml).expect("wikipedia-newsworthy recipe.toml must parse");
        let update = r
            .update
            .as_ref()
            .expect("wikipedia-newsworthy must declare [update]");
        assert_eq!(
            update.ingest_driver.as_deref(),
            Some("watcher"),
            "wikipedia-newsworthy must declare ingest_driver=\"watcher\" so \
             CorpusEngine::ingest skips the acquire pipeline that would \
             otherwise trip the watcher-time placeholder validator"
        );
        assert!(
            update.has_external_driver(),
            "has_external_driver() must return true for watcher-driven recipes"
        );
    }

    /// Default for non-watcher recipes: `ingest_driver` is None and
    /// `has_external_driver()` is false. Asserted against the wikipedia
    /// L1 recipe so a future regression that defaults the field to
    /// some non-None sentinel would also short-circuit the L1 install.
    #[test]
    fn standard_recipes_have_no_external_ingest_driver() {
        let recipes = builtin_recipes();
        let r = recipes
            .iter()
            .find(|r| r.corpus.id == "wikipedia")
            .expect("wikipedia recipe must exist");
        let driven = r.update.as_ref().is_some_and(|u| u.has_external_driver());
        assert!(
            !driven,
            "standard ingest recipes must not declare an external ingest driver"
        );
    }

    /// Multi-source bulk_download must round-trip through TOML
    /// without losing the URL list.
    #[test]
    fn bulk_download_multi_source_round_trips() {
        let toml_str = r#"
[corpus]
id = "demo"
name = "demo"

[acquire]
type = "bulk_download"
urls = ["https://a.example/dump.7z", "https://b.example/dump.7z"]

[extract]
type = "stackexchange_xml"

[chunk]
type = "passthrough"
"#;
        let recipe = Recipe::from_toml(toml_str).expect("parse");
        match &recipe.acquire {
            AcquirerConfig::BulkDownload { url, urls, resume } => {
                assert!(url.is_none());
                let urls = urls.as_ref().expect("urls present");
                assert_eq!(urls.len(), 2);
                assert!(*resume);
            }
            other => panic!("expected BulkDownload, got {other:?}"),
        }
    }

    /// Only SEP intentionally enables enrichment by default. Wikipedia
    /// Core ships with enrichment off (it costs hours of LLM time on a
    /// laptop and Layer 1 is about time-to-grounded, not atlas depth);
    /// users who expand to Full can re-enable it. All other recipes
    /// must also be off by default.
    #[test]
    fn non_sep_builtin_recipes_skip_enrichment_by_default() {
        let enrichment_allowed = ["sep"];
        let recipes = builtin_recipes();
        for recipe in &recipes {
            if enrichment_allowed.contains(&recipe.corpus.id.as_str()) {
                continue;
            }
            let enrichment_active = recipe
                .enrichment
                .as_ref()
                .map(|e| e.enabled)
                .unwrap_or(false);
            assert!(
                !enrichment_active,
                "Recipe '{}' has enrichment enabled — only SEP and Wikipedia should",
                recipe.corpus.id
            );
        }
    }

    // ----------------------------------------------------------------------
    // [recipe.parameters] + AcquirerConfig::HttpApi
    // ----------------------------------------------------------------------

    /// Recipe authors should be able to declare install-time
    /// parameters and reference them inside an `http_api` acquirer
    /// via `{name}` placeholders. Round-trip the TOML so both halves
    /// of the schema (parameters block + HttpApi variant) survive.
    #[test]
    fn http_api_recipe_with_parameters_round_trips() {
        let toml_str = r#"
[corpus]
id = "sec-filings"
name = "SEC EDGAR Filings"

[parameters.entities]
type = "list"
description = "CIK numbers or tickers"
required = true

[parameters.form_types]
type = "list"
description = "Filing types"
default = ["10-K", "10-Q", "8-K"]

[parameters.start_date]
type = "date"
default = "2022-01-01"

[acquire]
type = "http_api"
base_url = "https://efts.sec.gov/LATEST/search-index"
rate_limit_per_second = 8.0
user_agent = "CW-Test admin@example.com"

[[acquire.requests]]
url = "{base_url}?q=%22{entity}%22&forms={form_type}&startdt={start_date}"
for_each = ["entities", "form_types"]

[acquire.pagination]
type = "offset"
param = "start"
page_size = 100

[acquire.follow]
document_url_path = "$.hits.hits[*]._source.file_url"
document_format = "html"
max_concurrency = 6

[extract]
type = "html"

[chunk]
type = "paragraph"
"#;
        let r = Recipe::from_toml(toml_str).expect("recipe must parse");

        // Parameters block
        assert_eq!(r.parameters.len(), 3);
        let entities = r.parameters.get("entities").expect("entities declared");
        assert_eq!(entities.kind, ParameterKind::List);
        assert!(entities.required);

        // HttpApi acquirer
        match r.acquire {
            AcquirerConfig::HttpApi {
                base_url,
                requests,
                pagination,
                follow,
                rate_limit_per_second,
                user_agent,
                ..
            } => {
                assert!(base_url.contains("efts.sec.gov"));
                assert_eq!(requests.len(), 1);
                assert_eq!(requests[0].for_each, vec!["entities", "form_types"]);
                assert_eq!(requests[0].method, HttpMethod::Get);
                match pagination.expect("pagination present") {
                    PaginationStrategy::Offset {
                        param, page_size, ..
                    } => {
                        assert_eq!(param, "start");
                        assert_eq!(page_size, 100);
                    }
                    other => panic!("expected Offset, got {other:?}"),
                }
                let f = follow.expect("follow present");
                assert!(f.document_url_path.starts_with("$"));
                assert_eq!(f.document_format, DocFormat::Html);
                assert_eq!(f.max_concurrency, 6);
                assert_eq!(rate_limit_per_second, Some(8.0));
                assert_eq!(user_agent.as_deref(), Some("CW-Test admin@example.com"));
            }
            other => panic!("expected HttpApi, got {other:?}"),
        }
    }

    /// The federal-register-presidential bundled recipe must parse and
    /// declare the shape the legal-analysis use case depends on:
    ///   - http_api acquirer with two RequestTemplates (PRESDOCU + significant Rules)
    ///   - NextUrl pagination keyed on `$.next_page_url`
    ///   - Follow configured to fetch each result's raw_text_url with html format
    ///   - html extractor + paragraph chunker
    ///   - referential_atlas enrichment configured but disabled by default
    ///   - start_date + end_date install-time parameters with sensible defaults
    /// Regression-pinned: a future change that silently drops the second
    /// request template, flips enrichment on (bloating first-install time),
    /// or renames a parameter would all fail here.
    #[test]
    fn federal_register_presidential_recipe_shape() {
        let toml = bundled_recipe_toml("federal-register-presidential")
            .expect("federal-register-presidential must be a bundled recipe");
        let r =
            Recipe::from_toml(toml).expect("federal-register-presidential recipe.toml must parse");

        assert_eq!(r.corpus.id, "federal-register-presidential");

        // Acquirer: http_api with two requests, NextUrl pagination, html follow.
        match &r.acquire {
            AcquirerConfig::HttpApi {
                base_url,
                requests,
                pagination,
                follow,
                rate_limit_per_second,
                ..
            } => {
                assert!(
                    base_url.contains("federalregister.gov/api/v1"),
                    "base_url should point at the FedReg v1 API, got {base_url}"
                );
                assert_eq!(
                    requests.len(),
                    2,
                    "expected 2 request templates (PRESDOCU + significant Rules)"
                );
                assert!(
                    requests.iter().any(|t| t.url.contains("PRESDOCU")),
                    "one request must filter to Presidential Documents"
                );
                assert!(
                    requests.iter().any(|t| {
                        t.url.contains("type%5D%5B%5D=RULE") && t.url.contains("significant%5D=1")
                    }),
                    "one request must filter to significant Final Rules"
                );
                match pagination.as_ref().expect("pagination declared") {
                    PaginationStrategy::NextUrl { response_path } => {
                        assert_eq!(response_path, "$.next_page_url");
                    }
                    other => panic!("expected NextUrl pagination, got {other:?}"),
                }
                let f = follow.as_ref().expect("follow declared");
                assert!(
                    f.document_url_path.contains("raw_text_url"),
                    "follow must target raw_text_url (designed-for-access GPO format), \
                     not body_html_url (display chrome)"
                );
                assert_eq!(
                    f.document_format,
                    DocFormat::Html,
                    "raw_text_url payloads are HTML-wrapped plaintext; .html routes \
                     them through the html extractor's tag stripper"
                );
                assert!(
                    rate_limit_per_second.unwrap_or(99.0) <= 2.0,
                    "stay polite to a public-service API"
                );
            }
            other => panic!("expected HttpApi acquirer, got {other:?}"),
        }

        // Extractor + chunker.
        assert!(
            matches!(r.extract, ExtractorConfig::Html { .. }),
            "expected html extractor; raw_text_url's HTML envelope strips cleanly"
        );
        match &r.chunk {
            ChunkerConfig::Paragraph {
                max_chars,
                overlap_chars,
            } => {
                assert!(
                    *max_chars >= 1024,
                    "paragraph max_chars should leave headroom for legal prose"
                );
                assert!(
                    *overlap_chars > 0,
                    "paragraph chunker should overlap for citation continuity"
                );
            }
            other => panic!("expected Paragraph chunker, got {other:?}"),
        }

        // Enrichment block declared, but disabled by default — atlas
        // enrichment over ~200k chunks is hours of LLM work and the
        // first install should produce a usable index without it.
        let enrichment = r.enrichment.as_ref().expect("enrichment block declared");
        assert!(
            !enrichment.enabled,
            "enrichment must default to disabled for first-install latency"
        );
        assert_eq!(enrichment.enrichment_type, "atlas");
        assert_eq!(enrichment.domain.as_deref(), Some("legal"));

        // Parameters: install-time year list. Each request template
        // cross-products over years via for_each so each API query
        // stays under FedReg's 2,000-result-per-query ceiling.
        let year = r.parameters.get("year").expect("year parameter declared");
        assert_eq!(year.kind, ParameterKind::List);
        assert!(year.default.is_some(), "year should have a default list");
        if let Some(toml::Value::Array(items)) = &year.default {
            assert!(
                items.len() >= 5,
                "year default should span a multi-year window"
            );
        } else {
            panic!("year default must be a TOML array");
        }
        // Both request templates must declare `for_each = ["year"]`
        // or the 2k-cap ceiling re-emerges.
        match &r.acquire {
            AcquirerConfig::HttpApi { requests, .. } => {
                for req in requests {
                    assert!(
                        req.for_each.iter().any(|p| p == "year"),
                        "each request template must for_each over year to stay under the 2,000-result cap"
                    );
                    assert!(
                        req.url.contains("{year}"),
                        "request URL must reference the {{year}} placeholder"
                    );
                }
            }
            _ => unreachable!("acquirer asserted above"),
        }

        // No [prebuilt] block: no one has built and uploaded a snapshot
        // yet. When the first build lands on HuggingFace under
        // `svrnmesh/federal-register-presidential`, this assertion gets
        // inverted (and the test grows a check on the hf_repo).
        assert!(
            r.prebuilt.is_none(),
            "no [prebuilt] block yet — add one after the first build is published"
        );
    }

    /// The us-code bundled recipe must parse and declare the shape
    /// the legal-analysis stack depends on:
    ///   - bulk_download with the per-title govinfo URL list
    ///   - `{year}` placeholder interpolation against
    ///     `[parameters.year]` so `--param year=2023` swaps editions
    ///   - xml_sections extractor matching USLM `<section>` local-name
    ///   - paragraph chunker
    ///   - referential_atlas enrichment declared but off-by-default
    /// Regression-pinned: a future change that drops the year
    /// parameter (re-pinning the recipe to a single edition), removes
    /// the title-26 URL (Internal Revenue Code, the heavyweight), or
    /// flips enrichment on by default would all fail here.
    #[test]
    fn us_code_recipe_shape() {
        let toml = bundled_recipe_toml("us-code").expect("us-code must be a bundled recipe");
        let r = Recipe::from_toml(toml).expect("us-code recipe.toml must parse");

        assert_eq!(r.corpus.id, "us-code");

        // Acquirer: bulk_download with 50+ URLs (one per title) using
        // {year} interpolation. Title 26 must be present — without it,
        // tax / IRC questions silently fall through to nothing.
        match &r.acquire {
            AcquirerConfig::BulkDownload { url, urls, resume } => {
                assert!(url.is_none(), "expected `urls` list, not single `url`");
                let urls = urls.as_ref().expect("urls declared");
                assert!(
                    urls.len() >= 50,
                    "expected ~54 title URLs, got {}",
                    urls.len()
                );
                assert!(
                    urls.iter().all(|u| u.contains("{year}")),
                    "every URL must use {{year}} interpolation"
                );
                assert!(
                    urls.iter().any(|u| u.contains("title26")),
                    "Title 26 (Internal Revenue Code) must be in the URL list"
                );
                assert!(
                    *resume,
                    "bulk_download should resume to survive partial downloads"
                );
            }
            other => panic!("expected BulkDownload acquirer, got {other:?}"),
        }

        // Extractor: xml_sections matching USLM <section> by local
        // name with `identifier` as title.
        match &r.extract {
            ExtractorConfig::XmlSections {
                element,
                title_attr,
            } => {
                assert_eq!(element, "section");
                assert_eq!(title_attr.as_deref(), Some("identifier"));
            }
            other => panic!("expected XmlSections extractor, got {other:?}"),
        }

        // Chunker.
        assert!(matches!(r.chunk, ChunkerConfig::Paragraph { .. }));

        // Enrichment declared but disabled by default — ~150k sections
        // × paragraph chunking is hours of LLM work.
        let enrichment = r.enrichment.as_ref().expect("enrichment block declared");
        assert!(
            !enrichment.enabled,
            "us-code enrichment must default to disabled — atlas over 150k sections is hours of LLM work"
        );
        assert_eq!(enrichment.domain.as_deref(), Some("legal"));

        // Parameter: year for swapping annual editions.
        let year = r
            .parameters
            .get("year")
            .expect("[parameters.year] declared");
        assert!(year.default.is_some(), "year must have a default edition");

        // No [prebuilt] yet.
        assert!(
            r.prebuilt.is_none(),
            "no [prebuilt] until the first build is published"
        );
    }

    /// olc-opinions + scotus-opinions both ride on CourtListener.
    /// Verify both bundle recipes parse with the same http_api shape:
    ///   - Authorization: Token {api_token} header
    ///   - NextUrl pagination on `$.next`
    ///   - `json` extractor with `plain_text` content field
    ///   - `[parameters.api_token]` required install-time param
    #[test]
    fn courtlistener_recipes_shape() {
        for (id, court_filter) in [
            ("olc-opinions", "cluster__docket__court=olc"),
            ("scotus-opinions", "cluster__docket__court=scotus"),
        ] {
            let toml = bundled_recipe_toml(id).unwrap_or_else(|| panic!("{id} is bundled"));
            let r = Recipe::from_toml(toml).unwrap_or_else(|e| panic!("{id} recipe parses: {e}"));

            assert_eq!(r.corpus.id, id);

            match &r.acquire {
                AcquirerConfig::HttpApi {
                    base_url,
                    requests,
                    pagination,
                    follow,
                    headers,
                    ..
                } => {
                    assert!(
                        base_url.contains("courtlistener.com/api/rest/v4"),
                        "{id}: base_url should point at CourtListener v4 REST"
                    );
                    assert_eq!(requests.len(), 1, "{id}: one request template");
                    assert!(
                        requests[0].url.contains(court_filter),
                        "{id}: request must filter court via `{court_filter}`"
                    );
                    let auth = headers
                        .as_ref()
                        .and_then(|h| h.get("Authorization"))
                        .expect("Authorization header declared");
                    assert!(
                        auth.contains("Token {api_token}"),
                        "{id}: Authorization must interpolate {{api_token}} as `Token <value>`, got `{auth}`"
                    );
                    match pagination.as_ref().expect("pagination") {
                        PaginationStrategy::NextUrl { response_path } => {
                            assert_eq!(response_path, "$.next");
                        }
                        other => panic!("{id}: expected NextUrl, got {other:?}"),
                    }
                    assert!(
                        follow.is_none(),
                        "{id}: no follow — plain_text is inline in the page response"
                    );
                }
                other => panic!("{id}: expected HttpApi, got {other:?}"),
            }

            // Extractor: directory-aware json walking per-page files.
            match &r.extract {
                ExtractorConfig::Json {
                    document_path,
                    content_field,
                    ..
                } => {
                    assert_eq!(document_path, "$.results[*]");
                    assert_eq!(content_field, "plain_text");
                }
                other => panic!("{id}: expected Json extractor, got {other:?}"),
            }

            // Required api_token install-time parameter.
            let token = r
                .parameters
                .get("api_token")
                .unwrap_or_else(|| panic!("{id} must declare [parameters.api_token]"));
            assert!(
                token.required,
                "{id}: api_token must be required so empty installs fail loudly"
            );

            // SCOTUS declares an optional `start_date` knob so power
            // users can slice the corpus locally without paying for a
            // CourtListener subscription (e.g. recent ~10y fits the
            // free-tier 125-req/day cap). The default is the
            // comprehensive scope (1791) that the maintainer's
            // `[prebuilt]` build uses.
            if id == "scotus-opinions" {
                let start = r
                    .parameters
                    .get("start_date")
                    .expect("scotus-opinions must declare start_date for optional local slicing");
                assert_eq!(start.kind, ParameterKind::Date);
                assert!(
                    start.default.is_some(),
                    "scotus-opinions start_date must have a default"
                );
                assert!(
                    requests_url_of(&r).contains("cluster__date_filed__gte={start_date}"),
                    "scotus-opinions request must filter by cluster.date_filed >= {{start_date}}"
                );
            }
        }
    }

    fn requests_url_of(r: &Recipe) -> &str {
        match &r.acquire {
            AcquirerConfig::HttpApi { requests, .. } => requests[0].url.as_str(),
            _ => "",
        }
    }

    /// `resolve_parameters` should apply defaults, reject unknown
    /// keys, and validate types. The CLI / desktop call this
    /// before kicking off acquisition.
    #[test]
    fn resolve_parameters_applies_defaults_and_validates() {
        let toml_str = r#"
[corpus]
id = "demo"
name = "demo"

[parameters.entities]
type = "list"
required = true

[parameters.form_types]
type = "list"
default = ["10-K"]

[parameters.start_date]
type = "date"
default = "2022-01-01"

[acquire]
type = "bulk_download"
url = "https://example.com/x"

[extract]
type = "plaintext"

[chunk]
type = "sentence"
"#;
        let r = Recipe::from_toml(toml_str).unwrap();

        // Provide entities only — form_types + start_date take defaults.
        let mut provided = BTreeMap::new();
        provided.insert(
            "entities".into(),
            toml::Value::Array(vec![
                toml::Value::String("NVDA".into()),
                toml::Value::String("MSFT".into()),
            ]),
        );
        let resolved = r.resolve_parameters(&provided).expect("resolve OK");
        match resolved.values.get("entities").unwrap() {
            ParameterValue::List(items) => {
                assert_eq!(items, &vec!["NVDA".to_string(), "MSFT".into()]);
            }
            other => panic!("expected List, got {other:?}"),
        }
        assert_eq!(
            resolved
                .values
                .get("form_types")
                .map(ParameterValue::as_interpolation),
            Some("10-K".into()),
        );
        assert_eq!(
            resolved
                .values
                .get("start_date")
                .map(ParameterValue::as_interpolation),
            Some("2022-01-01".into()),
        );
    }

    #[test]
    fn resolve_parameters_rejects_unknown_keys() {
        let toml_str = r#"
[corpus]
id = "demo"
name = "demo"

[parameters.entities]
type = "list"
required = true

[acquire]
type = "bulk_download"
url = "https://example.com/x"

[extract]
type = "plaintext"

[chunk]
type = "sentence"
"#;
        let r = Recipe::from_toml(toml_str).unwrap();
        let mut provided = BTreeMap::new();
        provided.insert(
            "entites".into(), // typo
            toml::Value::Array(vec![toml::Value::String("NVDA".into())]),
        );
        let err = r.resolve_parameters(&provided).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("entites"), "should mention typo: {msg}");
        assert!(
            msg.contains("entities"),
            "should suggest declared param: {msg}"
        );
    }

    #[test]
    fn resolve_parameters_rejects_missing_required() {
        let toml_str = r#"
[corpus]
id = "demo"
name = "demo"

[parameters.entities]
type = "list"
required = true

[acquire]
type = "bulk_download"
url = "https://example.com/x"

[extract]
type = "plaintext"

[chunk]
type = "sentence"
"#;
        let r = Recipe::from_toml(toml_str).unwrap();
        let provided = BTreeMap::new();
        let err = r.resolve_parameters(&provided).unwrap_err();
        assert!(
            format!("{err}").contains("missing required parameter"),
            "{err}"
        );
    }

    #[test]
    fn resolve_parameters_validates_iso_date() {
        let toml_str = r#"
[corpus]
id = "demo"
name = "demo"

[parameters.start_date]
type = "date"
required = true

[acquire]
type = "bulk_download"
url = "https://example.com/x"

[extract]
type = "plaintext"

[chunk]
type = "sentence"
"#;
        let r = Recipe::from_toml(toml_str).unwrap();
        let mut bad = BTreeMap::new();
        bad.insert(
            "start_date".into(),
            toml::Value::String("01/01/2022".into()),
        );
        let err = r.resolve_parameters(&bad).unwrap_err();
        assert!(format!("{err}").contains("ISO-8601"), "{err}");

        let mut good = BTreeMap::new();
        good.insert(
            "start_date".into(),
            toml::Value::String("2022-01-01".into()),
        );
        let resolved = r.resolve_parameters(&good).unwrap();
        match resolved.values.get("start_date").unwrap() {
            ParameterValue::Date(s) => assert_eq!(s, "2022-01-01"),
            other => panic!("expected Date, got {other:?}"),
        }
    }

    #[test]
    fn list_parameter_accepts_comma_separated_string() {
        // The CLI's interactive prompt yields a single string rather
        // than a TOML array. Make sure the resolver splits it cleanly.
        let toml_str = r#"
[corpus]
id = "demo"
name = "demo"

[parameters.entities]
type = "list"
required = true

[acquire]
type = "bulk_download"
url = "https://example.com/x"

[extract]
type = "plaintext"

[chunk]
type = "sentence"
"#;
        let r = Recipe::from_toml(toml_str).unwrap();
        let mut provided = BTreeMap::new();
        provided.insert(
            "entities".into(),
            toml::Value::String("NVDA, MSFT,GOOGL".into()),
        );
        let resolved = r.resolve_parameters(&provided).unwrap();
        match resolved.values.get("entities").unwrap() {
            ParameterValue::List(items) => {
                assert_eq!(
                    items,
                    &vec!["NVDA".to_string(), "MSFT".into(), "GOOGL".into(),]
                );
            }
            other => panic!("expected List, got {other:?}"),
        }
    }

    /// Investigation enrichment schema: entity types + relationship
    /// types + patterns must round-trip cleanly. The recipe author
    /// writes this in TOML and the pipeline drives prompt generation
    /// from the parsed shape; a regression here breaks the
    /// recipe-author flow silently.
    #[test]
    fn investigation_enrichment_schema_round_trips() {
        let toml_str = r#"
[corpus]
id = "demo"
name = "demo"

[acquire]
type = "bulk_download"
url = "https://example.com/data.zip"

[extract]
type = "html"

[chunk]
type = "paragraph"

[enrichment]
enabled = true
type = "investigation"
domain = "investigation"

[[enrichment.entity_types]]
name = "company"
description = "A corporation or legal entity"
attributes = ["name", "ticker", "cik"]

[[enrichment.entity_types]]
name = "fund"
description = "An investment fund"
attributes = ["name", "manager"]

[[enrichment.relationship_types]]
name = "revenue"
description = "Recognized revenue from B to A"
attributes = ["amount_usd", "period", "percentage_of_total"]
directional = true

[[enrichment.relationship_types]]
name = "investment"
description = "A invested equity in B"
attributes = ["amount_usd", "date"]
directional = true

[[enrichment.patterns]]
type = "circular_flow"
name = "money_cycles"
description = "A→B→C→A money cycles"
min_entities = 3
edge_types = ["revenue", "investment"]

[[enrichment.patterns]]
type = "role_overlap"
name = "invest_in_customer"
description = "A invests in B and B is a customer of A"
[enrichment.patterns.entity_roles]
investor = "investment.from"
customer = "revenue.to"

[[enrichment.patterns]]
type = "threshold"
name = "revenue_concentration"
description = "Revenue concentration above 10%"
edge_type = "revenue"
attribute = "percentage_of_total"
threshold = 0.10
comparison = "greater_than"
"#;
        let r = Recipe::from_toml(toml_str).expect("recipe must parse");

        let enr = r.enrichment.expect("enrichment block parsed");
        assert_eq!(enr.enrichment_type, "investigation");
        assert_eq!(enr.entity_types.len(), 2);
        assert_eq!(enr.entity_types[0].name, "company");
        assert!(enr.entity_types[0].attributes.iter().any(|a| a == "ticker"));

        assert_eq!(enr.relationship_types.len(), 2);
        assert_eq!(enr.relationship_types[0].name, "revenue");
        assert!(enr.relationship_types[0].directional);

        assert_eq!(enr.patterns.len(), 3);
        match &enr.patterns[0] {
            PatternDecl::CircularFlow {
                name,
                min_entities,
                edge_types,
                ..
            } => {
                assert_eq!(name, "money_cycles");
                assert_eq!(*min_entities, 3);
                assert_eq!(
                    edge_types,
                    &vec!["revenue".to_string(), "investment".into()]
                );
            }
            other => panic!("expected CircularFlow, got {other:?}"),
        }
        match &enr.patterns[1] {
            PatternDecl::RoleOverlap {
                name, entity_roles, ..
            } => {
                assert_eq!(name, "invest_in_customer");
                assert_eq!(
                    entity_roles.get("investor").map(String::as_str),
                    Some("investment.from")
                );
                assert_eq!(
                    entity_roles.get("customer").map(String::as_str),
                    Some("revenue.to")
                );
            }
            other => panic!("expected RoleOverlap, got {other:?}"),
        }
        match &enr.patterns[2] {
            PatternDecl::Threshold {
                name,
                edge_type,
                attribute,
                threshold,
                comparison,
                ..
            } => {
                assert_eq!(name, "revenue_concentration");
                assert_eq!(edge_type, "revenue");
                assert_eq!(attribute, "percentage_of_total");
                assert!((*threshold - 0.10).abs() < 1e-9);
                assert_eq!(*comparison, Comparison::GreaterThan);
            }
            other => panic!("expected Threshold, got {other:?}"),
        }
    }

    #[test]
    fn custom_atlas_ontology_parses_and_is_detected() {
        let toml_str = r#"
[corpus]
id = "numis"
name = "numis"

[acquire]
type = "local_file"
path = "/tmp/x.md"

[extract]
type = "markdown"

[chunk]
type = "passthrough"

[enrichment]
enabled = true
type = "atlas"
domain = "medieval-numismatics"

[enrichment.ontology]
guidance = """
Extract coins (mint, ruler, denomination, metal), mints, rulers, hoards.
Relations: minted_by, found_in_hoard, succeeds_ruler.
"""

[enrichment.ontology.vocabulary]
concern_term = "numismatic question"
evidence_term = "passage"
"#;
        let r = Recipe::from_toml(toml_str).expect("recipe must parse");
        let enr = r.enrichment.clone().expect("enrichment parsed");
        assert_eq!(enr.enrichment_type, "atlas");
        let ont = enr.ontology.as_ref().expect("ontology block parsed");
        assert!(ont.guidance.contains("minted_by"), "guidance retained");
        let vocab = ont.vocabulary.as_ref().expect("vocabulary parsed");
        assert_eq!(vocab.concern_term.as_deref(), Some("numismatic question"));
        assert_eq!(vocab.position_term, None, "omitted term stays None");

        // The accessor signals "use the custom atlas pipeline".
        assert!(r.custom_ontology().is_some());
        assert!(r.produces_enriched_atoms());
    }

    #[test]
    fn custom_ontology_precedence_and_empty_guidance() {
        // Empty/whitespace guidance does NOT trigger the custom path even if the
        // block is present — falls back to pipeline/domain.
        let empty = r#"
[corpus]
id = "c"
name = "c"
[acquire]
type = "local_file"
path = "/tmp/x.md"
[extract]
type = "markdown"
[chunk]
type = "passthrough"
[enrichment]
enabled = true
type = "atlas"
pipeline = "philosophy_atlas"
[enrichment.ontology]
guidance = "   "
"#;
        let r = Recipe::from_toml(empty).expect("parse");
        assert!(
            r.custom_ontology().is_none(),
            "blank guidance must not trigger custom atlas"
        );
        assert_eq!(
            r.enrichment.unwrap().pipeline.as_deref(),
            Some("philosophy_atlas"),
            "falls through to the explicit pipeline pin"
        );

        // No ontology block at all → None.
        let none = r#"
[corpus]
id = "c"
name = "c"
[acquire]
type = "local_file"
path = "/tmp/x.md"
[extract]
type = "markdown"
[chunk]
type = "passthrough"
[enrichment]
enabled = true
type = "atlas"
"#;
        assert!(Recipe::from_toml(none)
            .expect("parse")
            .custom_ontology()
            .is_none());
    }

    /// Pagination strategies should round-trip for all four shapes.
    #[test]
    fn pagination_strategies_round_trip() {
        for (toml_block, check) in [
            (
                r#"
[acquire.pagination]
type = "cursor"
param = "after"
response_path = "$.next_cursor"
"#,
                Box::new(
                    |p: PaginationStrategy| matches!(p, PaginationStrategy::Cursor { ref param, .. } if param == "after"),
                ) as Box<dyn Fn(PaginationStrategy) -> bool>,
            ),
            (
                r#"
[acquire.pagination]
type = "next_url"
response_path = "$.next"
"#,
                Box::new(|p| matches!(p, PaginationStrategy::NextUrl { .. })),
            ),
            (
                r#"
[acquire.pagination]
type = "page_number"
end = 5
"#,
                Box::new(
                    |p| matches!(p, PaginationStrategy::PageNumber { start, end, .. } if start == 1 && end == 5),
                ),
            ),
        ] {
            let toml_str = format!(
                r#"
[corpus]
id = "demo"
name = "demo"

[acquire]
type = "http_api"
base_url = "https://api.example/"

[[acquire.requests]]
url = "{{base_url}}/items"
{toml_block}

[extract]
type = "plaintext"

[chunk]
type = "sentence"
"#
            );
            let r = Recipe::from_toml(&toml_str).expect("parse");
            let pagination = match r.acquire {
                AcquirerConfig::HttpApi { pagination, .. } => pagination.expect("pagination"),
                _ => panic!("expected HttpApi"),
            };
            assert!(check(pagination), "round-trip check failed");
        }
    }
}
