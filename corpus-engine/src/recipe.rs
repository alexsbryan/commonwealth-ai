use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::filters::{ComposeMode, FilterConfig};
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

fn default_page_size() -> usize {
    100
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
}

// ---------------------------------------------------------------------------
// EnrichmentConfig
// ---------------------------------------------------------------------------

/// Configures the optional enrichment pipeline.
///
/// The new field model enrichment uses domain-specific prompts and
/// HDBSCAN clustering. Set `type = "field_model"` and `domain = "philosophy"`
/// (or another domain) to use the new pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentConfig {
    #[serde(default)]
    pub enabled: bool,

    // ── New field model fields ──────────────────────────────

    /// Enrichment type: "field_model" (default).
    #[serde(default = "default_enrichment_type", rename = "type")]
    pub enrichment_type: String,

    /// Domain identifier: "philosophy", "science", "policy", "legal",
    /// "community", "multi".
    #[serde(default)]
    pub domain: Option<String>,

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
            prompt_version: None,
            clustering: None,
            alignment: None,
            fault_lines: None,
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

    /// Parent corpus id, set on per-work corpora produced by an
    /// on-demand catalog ingest (e.g. `gutenberg-2701` carries
    /// `parent_corpus_id = "gutenberg"`). Stamped onto the on-disk
    /// `IndexMeta` so search consumers can group per-work corpora
    /// under their catalog and suppress repeated ingest offers for
    /// works already read. Always `None` in TOML files on disk;
    /// populated only via [`crate::types::CorpusSpec::Inline`].
    #[serde(default)]
    pub parent_corpus_id: Option<String>,
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
    #[serde(rename = "api_paginated")]
    ApiPaginated {
        base_url: String,
        page_param: String,
        #[serde(default = "default_page_size")]
        page_size: usize,
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
    #[serde(rename = "html")]
    Html {
        #[serde(default)]
        content_selector: Option<String>,
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

impl Recipe {
    /// Parse a `Recipe` from a TOML string.
    pub fn from_toml(toml_str: &str) -> Result<Self> {
        toml::from_str(toml_str).map_err(|e| Error::Recipe(e.to_string()))
    }

    /// Load a `Recipe` from a `.toml` file on disk.
    pub fn from_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        Self::from_toml(&contents)
    }
}

// ---------------------------------------------------------------------------
// Built-in recipes
// ---------------------------------------------------------------------------

/// Bundled recipe TOML for well-known corpora, embedded at compile
/// time. Used as a **last-resort fallback** by
/// `RecipeRegistry::fetch_recipe()` so a corpus listed in the snapshot
/// catalog still installs even when:
///
/// - the registry's `toml_url` 404s (recipe not pushed to GitHub yet —
///   common during development);
/// - the user has no internet; or
/// - the user is running an air-gapped build.
///
/// Returns `None` for unknown ids; the caller falls back to its prior
/// error message in that case.
///
/// To add a new bundled recipe: drop `recipes/<id>/recipe.toml` in the
/// crate, add an arm here, and the live registry catalog
/// (`registry_snapshot.toml` + `sovereign-recipes/registry.toml`) entry
/// for it. Match-arm coverage stays paired with the snapshot via the
/// `bundled_recipe_covers_every_snapshot_entry` test below.
pub fn bundled_recipe_toml(id: &str) -> Option<&'static str> {
    match id {
        "wikipedia" => Some(include_str!("../recipes/wikipedia/recipe.toml")),
        "wikipedia-simple" => Some(include_str!("../recipes/wikipedia-simple/recipe.toml")),
        "stackexchange" => Some(include_str!("../recipes/stackexchange/recipe.toml")),
        "stackexchange-knowledge" => {
            Some(include_str!("../recipes/stackexchange-knowledge/recipe.toml"))
        }
        "openalex" => Some(include_str!("../recipes/openalex/recipe.toml")),
        "gutenberg" => Some(include_str!("../recipes/gutenberg/recipe.toml")),
        "gutenberg-work" => Some(include_str!("../recipes/gutenberg-work/recipe.toml")),
        "sep" => Some(include_str!("../recipes/sep/recipe.toml")),
        "crs_reports" => Some(include_str!("../recipes/crs_reports/recipe.toml")),
        _ => None,
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
        assert!(matches!(
            r.extract,
            ExtractorConfig::GutenbergCatalog {}
        ));
        let cat = r.catalog.expect("[catalog] block parsed");
        assert_eq!(cat.id_field, "gutenberg_id");
        assert_eq!(cat.content_recipe, "gutenberg-work");
        assert!(cat
            .download_url_template
            .contains("{id}"));
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
            FilterConfig::PageviewRank { rank_file, max_rank } => {
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
        assert_eq!(recipe.corpus.mesh_sharing, true);

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
            assert_eq!(
                &recipe.corpus.id, expected_id,
                "unexpected id for recipe"
            );
            assert!(!recipe.corpus.id.is_empty(), "recipe id must not be empty");
            assert!(!recipe.corpus.name.is_empty(), "recipe name must not be empty");
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
        let sep = recipes
            .iter()
            .find(|r| r.corpus.id == "sep")
            .unwrap();
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
            recipes.iter().all(|r| r.corpus.id != "structured_wikipedia"),
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
        let enrichment = wp.enrichment.as_ref().expect("wikipedia must have enrichment block");
        assert!(!enrichment.enabled, "Core must ship with enrichment disabled");
        assert_eq!(enrichment.enrichment_type, "field_model");
        assert_eq!(enrichment.domain.as_deref(), Some("multi"));

        let update = wp.update.as_ref().expect("wikipedia must have update config");
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
            ExtractorConfig::WikipediaStructured { .. } => {}
            other => panic!("expected WikipediaStructured, got {other:?}"),
        }
        // Layer 0 is intentionally unfiltered and unenriched.
        assert!(simple.filters.is_empty(), "Simple English should not have filters");
        let enrichment = simple.enrichment.as_ref().expect("enrichment block present");
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
        assert!(!enrichment.enabled, "MVP keeps enrichment off until prompts land");
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
        assert!(matches!(r.acquire, AcquirerConfig::HuggingFaceDataset { .. }));
        assert!(matches!(r.extract, ExtractorConfig::Parquet { .. }));
        assert!(r.filters.is_empty(), "breadth layer takes the dataset as-is");
        assert!(
            r.enrichment.as_ref().map(|e| !e.enabled).unwrap_or(true),
            "breadth layer must not enable enrichment"
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
}
