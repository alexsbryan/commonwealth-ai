use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

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
}

// ---------------------------------------------------------------------------
// AcquirerConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AcquirerConfig {
    #[serde(rename = "bulk_download")]
    BulkDownload {
        url: String,
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
}

// ---------------------------------------------------------------------------
// ExtractorConfig
// ---------------------------------------------------------------------------

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
    #[serde(rename = "stackexchange_xml")]
    StackExchangeXml {
        #[serde(default = "default_min_score")]
        min_score: i32,
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
        /// Restrict processing to a specific set of ZIP shard entries
        /// (by index into the ZIP's JSONL entries). Set by the
        /// collaborative-ingestion planner for multi-shard JSONL
        /// corpora such as Wikipedia (76 shards). Mutually exclusive
        /// with `article_range` — the sharded path streams directly
        /// from the ZIP and skips the merged-JSONL cache entirely.
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

/// Returns `Recipe` definitions for well-known corpora, loaded from the
/// `recipes/` directory at compile time via `include_str!`.
///
/// **For tests only.** Not compiled into production binaries.
/// Production code uses `RecipeRegistry::fetch_recipe()` which checks
/// local overrides and fetches from the registry URL.
///
/// To add a new corpus, create `recipes/<id>/recipe.toml` following the
/// pattern of the existing files, then add an `include_str!` line below.
#[cfg(test)]
pub(crate) fn builtin_recipes() -> Vec<Recipe> {
    const SOURCES: &[&str] = &[
        include_str!("../recipes/wikipedia/recipe.toml"),
        include_str!("../recipes/stackexchange/recipe.toml"),
        include_str!("../recipes/openalex/recipe.toml"),
        include_str!("../recipes/gutenberg/recipe.toml"),
        include_str!("../recipes/sep/recipe.toml"),
        include_str!("../recipes/crs_reports/recipe.toml"),
    ];
    SOURCES
        .iter()
        .map(|s| Recipe::from_toml(s).expect("built-in recipe.toml failed to parse"))
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
            AcquirerConfig::BulkDownload { url, resume } => {
                assert!(url.contains("wikimedia"));
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
        assert_eq!(recipes.len(), 6);
    }

    #[test]
    fn builtin_recipes_have_valid_ids() {
        let expected_ids = [
            "wikipedia",
            "stackexchange",
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
            AcquirerConfig::BulkDownload { url, resume } => {
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
        assert_eq!(
            enrichment.enrichment_type, "field_model",
            "SEP enrichment type must be field_model"
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
    fn gutenberg_recipe_uses_huggingface_dataset_acquirer() {
        let recipes = builtin_recipes();
        let gut = recipes
            .iter()
            .find(|r| r.corpus.id == "gutenberg")
            .expect("gutenberg recipe must exist");
        match &gut.acquire {
            AcquirerConfig::HuggingFaceDataset { repo, subset, .. } => {
                assert_eq!(repo, "manu/project_gutenberg");
                assert_eq!(subset.as_deref(), Some("en"));
            }
            other => panic!("expected HuggingFaceDataset, got {other:?}"),
        }
        match &gut.extract {
            ExtractorConfig::Parquet { content_column, .. } => {
                assert_eq!(content_column, "text");
            }
            other => panic!("expected Parquet extractor, got {other:?}"),
        }
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

        let enrichment = wp.enrichment.as_ref().expect("wikipedia must have enrichment");
        assert!(enrichment.enabled);
        assert_eq!(enrichment.enrichment_type, "field_model");
        assert_eq!(enrichment.domain.as_deref(), Some("multi"));

        let update = wp.update.as_ref().expect("wikipedia must have update config");
        assert!(update.auto_update);
        assert!(!update.manifest_url.is_empty());
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

    /// Only SEP and Wikipedia intentionally enable enrichment. All other
    /// recipes must not activate it by default.
    #[test]
    fn non_sep_builtin_recipes_skip_enrichment_by_default() {
        let enrichment_allowed = ["sep", "wikipedia"];
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
