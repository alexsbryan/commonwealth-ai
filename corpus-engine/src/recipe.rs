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

fn default_max_chunk_chars() -> usize {
    2048
}

fn default_overlap_chars() -> usize {
    256
}

fn default_embedding_model() -> String {
    "nomic-embed-text-v2".to_string()
}

fn default_embedding_dimensions() -> usize {
    768
}

fn default_similarity_threshold() -> f32 {
    0.55
}

fn default_max_relationship_candidates() -> usize {
    50_000
}

// ---------------------------------------------------------------------------
// Top-level Recipe
// ---------------------------------------------------------------------------

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
}

// ---------------------------------------------------------------------------
// EnrichmentConfig
// ---------------------------------------------------------------------------

/// Configures the optional epistemic enrichment pipeline.
///
/// The two prompts are domain-specific — SEP's hedging patterns differ
/// from legal opinions or medical literature. The recipe author writes
/// the prompt that fits their corpus's conventions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Prompt that teaches the model to extract claims from a passage.
    /// Should ask for a JSON array; the engine tolerates surrounding
    /// prose and markdown code fences.
    pub claim_extraction_prompt: String,

    /// Whether to also extract relationships between claims.
    #[serde(default)]
    pub extract_relationships: bool,

    /// Prompt for relationship classification. Required if
    /// `extract_relationships` is true. Supports placeholders:
    /// `{claim_a}`, `{claim_b}`, `{source_a}`, `{source_b}`,
    /// `{attributed_a}`, `{attributed_b}`.
    #[serde(default)]
    pub relationship_extraction_prompt: Option<String>,

    /// Minimum cosine similarity between two claims (from different
    /// entries) for them to be considered as candidate pairs. Lower
    /// values surface more potential relationships at the cost of more
    /// inference calls.
    #[serde(default = "default_similarity_threshold")]
    pub relationship_similarity_threshold: f32,

    /// Maximum number of candidate pairs to evaluate. Caps the cost
    /// of relationship extraction on large corpora.
    #[serde(default = "default_max_relationship_candidates")]
    pub max_relationship_candidates: usize,
}

impl Default for EnrichmentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            claim_extraction_prompt: String::new(),
            extract_relationships: false,
            relationship_extraction_prompt: None,
            relationship_similarity_threshold: default_similarity_threshold(),
            max_relationship_candidates: default_max_relationship_candidates(),
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
    #[serde(default)]
    pub size_compressed_gb: f64,
    #[serde(default)]
    pub size_indexed_gb: f64,
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
    },
    #[serde(rename = "plaintext")]
    Plaintext {
        #[serde(default)]
        title_pattern: Option<String>,
        #[serde(default)]
        strip_boilerplate: Option<String>,
    },
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

/// Returns hardcoded `Recipe` definitions for well-known corpora.
pub fn builtin_recipes() -> Vec<Recipe> {
    vec![
        // 1. Wikipedia
        //
        // Sourced from the wikimedia/wikipedia dataset on HuggingFace, which
        // provides a clean pre-processed snapshot of English Wikipedia as 41
        // parquet shards (~6.4M articles, 11.6 GB compressed). Uses the
        // HuggingFaceDataset acquirer's parquet API fallback to enumerate shards
        // for this config-based repo. The `url` column is passed through so each
        // search result links back to the source Wikipedia article.
        Recipe {
            corpus: CorpusMeta {
                id: "wikipedia".to_string(),
                name: "Wikipedia (English)".to_string(),
                description: "English Wikipedia articles from the November 2023 snapshot.".to_string(),
                license: "CC-BY-SA-4.0".to_string(),
                mesh_sharing: true,
                size_compressed_gb: 11.6,
                size_indexed_gb: 45.0,
            },
            acquire: AcquirerConfig::HuggingFaceDataset {
                repo: "wikimedia/wikipedia".to_string(),
                subset: Some("20231101.en".to_string()),
            },
            extract: ExtractorConfig::Parquet {
                content_column: "text".to_string(),
                label_column: Some("title".to_string()),
                url_column: Some("url".to_string()),
            },
            chunk: ChunkerConfig::Paragraph {
                max_chars: 2048,
                overlap_chars: 256,
            },
            index: IndexConfig::default(),
            enrichment: None,
        },
        // 2. Stack Exchange
        Recipe {
            corpus: CorpusMeta {
                id: "stackexchange".to_string(),
                name: "Stack Exchange".to_string(),
                description: "Questions and answers from the Stack Exchange network."
                    .to_string(),
                license: "CC-BY-SA-4.0".to_string(),
                mesh_sharing: true,
                size_compressed_gb: 85.0,
                size_indexed_gb: 120.0,
            },
            acquire: AcquirerConfig::BulkDownload {
                url: "https://archive.org/download/stackexchange".to_string(),
                resume: true,
            },
            extract: ExtractorConfig::StackExchangeXml { min_score: 3 },
            chunk: ChunkerConfig::Paragraph {
                max_chars: 2048,
                overlap_chars: 256,
            },
            index: IndexConfig::default(),
            enrichment: None,
        },
        // 3. OpenAlex
        Recipe {
            corpus: CorpusMeta {
                id: "openalex".to_string(),
                name: "OpenAlex".to_string(),
                description: "Open scholarly metadata from the OpenAlex catalogue."
                    .to_string(),
                license: "CC0-1.0".to_string(),
                mesh_sharing: true,
                size_compressed_gb: 330.0,
                size_indexed_gb: 500.0,
            },
            acquire: AcquirerConfig::BulkDownload {
                url: "https://openalex.s3.amazonaws.com/data/works/".to_string(),
                resume: true,
            },
            extract: ExtractorConfig::Jsonl {
                content_field: Some("abstract_inverted_index".to_string()),
                title_field: Some("title".to_string()),
                filter: None,
                decompress: Some("gzip".to_string()),
            },
            chunk: ChunkerConfig::Paragraph {
                max_chars: 2048,
                overlap_chars: 256,
            },
            index: IndexConfig::default(),
            enrichment: None,
        },
        // 4. Project Gutenberg
        //
        // Sourced from the manu/project_gutenberg dataset on HuggingFace,
        // which mirrors ~61k English Gutenberg books across 52 parquet shards.
        // The HuggingFaceDataset acquirer enumerates all shards via the HF API
        // and downloads them into a local directory; the parquet extractor then
        // chains across all shards. The `text` column holds the full book text.
        Recipe {
            corpus: CorpusMeta {
                id: "gutenberg".to_string(),
                name: "Project Gutenberg".to_string(),
                description: "Public-domain books from Project Gutenberg.".to_string(),
                license: "Public Domain".to_string(),
                mesh_sharing: true,
                size_compressed_gb: 9.0,
                size_indexed_gb: 25.0,
            },
            acquire: AcquirerConfig::HuggingFaceDataset {
                repo: "manu/project_gutenberg".to_string(),
                subset: Some("en".to_string()),
            },
            extract: ExtractorConfig::Parquet {
                content_column: "text".to_string(),
                label_column: Some("id".to_string()),
                url_column: None,
            },
            chunk: ChunkerConfig::Paragraph {
                max_chars: 2048,
                overlap_chars: 256,
            },
            index: IndexConfig::default(),
            enrichment: None,
        },
        // 5. Stanford Encyclopedia of Philosophy
        //
        // Sourced from the AiresPucrs/stanford-encyclopedia-philosophy
        // dataset on HuggingFace, which mirrors SEP entries as a single
        // parquet file. This avoids scraping plato.stanford.edu directly
        // (which would be slow, fragile, and rude to Stanford's servers).
        // The parquet has columns: `title`, `text`, `url`.
        Recipe {
            corpus: CorpusMeta {
                id: "sep".to_string(),
                name: "Stanford Encyclopedia of Philosophy".to_string(),
                description: "Peer-reviewed encyclopedia entries from the Stanford Encyclopedia of Philosophy. Includes claim and relationship enrichment for the epistemic-research skill."
                    .to_string(),
                license: "Copyright Stanford University (educational/research use)".to_string(),
                mesh_sharing: false,
                // The parquet is roughly 1.4 GB compressed; indexed with
                // embeddings + claims + relationships it lands around
                // 5–6 GB on disk depending on enrichment depth.
                size_compressed_gb: 1.4,
                size_indexed_gb: 6.0,
            },
            acquire: AcquirerConfig::BulkDownload {
                url: "https://huggingface.co/datasets/AiresPucrs/stanford-encyclopedia-philosophy/resolve/main/data/train-00000-of-00001.parquet".to_string(),
                resume: true,
            },
            extract: ExtractorConfig::Parquet {
                content_column: "text".to_string(),
                label_column: Some("title".to_string()),
                url_column: None,
            },
            chunk: ChunkerConfig::Paragraph {
                max_chars: 2048,
                overlap_chars: 256,
            },
            index: IndexConfig::default(),
            enrichment: Some(EnrichmentConfig {
                enabled: true,
                claim_extraction_prompt: SEP_CLAIM_PROMPT.to_string(),
                extract_relationships: true,
                relationship_extraction_prompt: Some(
                    SEP_RELATIONSHIP_PROMPT.to_string(),
                ),
                relationship_similarity_threshold: 0.55,
                max_relationship_candidates: 50_000,
            }),
        },
        // 6. CRS Reports
        Recipe {
            corpus: CorpusMeta {
                id: "crs_reports".to_string(),
                name: "CRS Reports".to_string(),
                description: "Congressional Research Service reports from EveryCRSReport.com."
                    .to_string(),
                license: "Public Domain".to_string(),
                mesh_sharing: true,
                size_compressed_gb: 2.0,
                size_indexed_gb: 5.0,
            },
            acquire: AcquirerConfig::BulkDownload {
                url: "https://www.everycrsreport.com/reports.html".to_string(),
                resume: true,
            },
            extract: ExtractorConfig::Html {
                content_selector: None,
                title_selector: Some("h1".to_string()),
            },
            chunk: ChunkerConfig::Paragraph {
                max_chars: 2048,
                overlap_chars: 256,
            },
            index: IndexConfig::default(),
            enrichment: None,
        },
    ]
}

// ---------------------------------------------------------------------------
// Built-in enrichment prompts (SEP)
// ---------------------------------------------------------------------------

const SEP_CLAIM_PROMPT: &str = r#"Extract propositional claims from this encyclopedia passage.

For each claim, provide:
1. "claim": A single declarative statement capturing the position.
2. "epistemic_status": One of: "consensus", "majority", "contested",
   "minority", "established", "unclear".
3. "hedging_language": The source text's exact words indicating
   epistemic status (e.g., "it is widely accepted that").
   Null if the text doesn't hedge.
4. "attributed_to": Who holds this position — a philosopher name,
   a school of thought, or null if it's the article's own framing.

SEP articles use distinctive hedging patterns:
- "It is widely accepted..." / "There is broad agreement..." → consensus
- "Most philosophers hold..." / "The standard view..." → majority
- "This remains controversial..." / "There is ongoing debate..." → contested
- "Some philosophers argue..." / "Critics maintain..." → minority
- "It is uncontroversial that..." / "It has been established..." → established

Extract 3-8 claims per passage. Focus on substantive philosophical
positions, not bibliographic or historical facts. If a passage is
purely biographical or administrative, return an empty array.

Return ONLY a JSON array of claim objects. No other text.
"#;

const SEP_RELATIONSHIP_PROMPT: &str = r#"Given two claims from different encyclopedia entries, determine
their epistemic relationship.

Claim A: {claim_a}
  From: {source_a}
  Attributed to: {attributed_a}

Claim B: {claim_b}
  From: {source_b}
  Attributed to: {attributed_b}

What is the relationship?
- "contradicts": Claim A directly opposes Claim B.
- "supports": Claim A provides evidence or argument for Claim B.
- "refines": Claim A qualifies or adds nuance to Claim B.
- "competing_answers": Both claims answer the same question differently.
- "presupposes": Claim A depends on or assumes Claim B.
- "none": No meaningful epistemic relationship.

Also provide:
- "connecting_issue": The question or topic that connects them.
  Null if the relationship is "none".
- "confidence": 0.0 to 1.0, how confident you are in this classification.

Return ONLY a JSON object. No other text.
"#;

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
        assert_eq!(recipe.index.embedding_model, "nomic-embed-text-v2");
        assert_eq!(recipe.index.embedding_dimensions, 768);
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
                assert_eq!(label_column.as_deref(), Some("title"));
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
        assert!(
            !enrichment.claim_extraction_prompt.is_empty(),
            "SEP claim extraction prompt must not be empty"
        );
        assert!(
            enrichment.claim_extraction_prompt.contains("epistemic_status"),
            "SEP claim prompt should ask for epistemic_status"
        );

        assert!(
            enrichment.extract_relationships,
            "SEP must extract relationships"
        );
        let rel_prompt = enrichment
            .relationship_extraction_prompt
            .as_ref()
            .expect("SEP must have a relationship extraction prompt");
        assert!(rel_prompt.contains("{claim_a}"));
        assert!(rel_prompt.contains("{claim_b}"));
        assert!(rel_prompt.contains("contradicts"));
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
            AcquirerConfig::HuggingFaceDataset { repo, subset } => {
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
            AcquirerConfig::HuggingFaceDataset { repo, subset } => {
                assert_eq!(repo, "manu/project_gutenberg");
                assert_eq!(subset.as_deref(), Some("en"));
            }
            _ => panic!("wrong acquirer variant after TOML round-trip"),
        }
    }

    /// Recipes that don't request enrichment must explicitly opt out
    /// (None or `enabled = false`). This catches recipes that pick up
    /// stray enrichment configs by accident.
    #[test]
    fn non_sep_builtin_recipes_skip_enrichment_by_default() {
        let recipes = builtin_recipes();
        for recipe in &recipes {
            if recipe.corpus.id == "sep" {
                continue;
            }
            let enrichment_active = recipe
                .enrichment
                .as_ref()
                .map(|e| e.enabled)
                .unwrap_or(false);
            assert!(
                !enrichment_active,
                "Recipe '{}' has enrichment enabled by default — only SEP should",
                recipe.corpus.id
            );
        }
    }
}
