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
        Recipe {
            corpus: CorpusMeta {
                id: "wikipedia".to_string(),
                name: "Wikipedia (English)".to_string(),
                description: "English Wikipedia articles, sourced from Wikimedia dump files."
                    .to_string(),
                license: "CC-BY-SA-4.0".to_string(),
                mesh_sharing: true,
                size_compressed_gb: 22.0,
                size_indexed_gb: 45.0,
            },
            acquire: AcquirerConfig::BulkDownload {
                url: "https://dumps.wikimedia.org/enwiki/latest/enwiki-latest-pages-articles.xml.bz2"
                    .to_string(),
                resume: true,
            },
            extract: ExtractorConfig::MediawikiXml {
                namespace_filter: vec![0],
                skip_redirects: true,
                decompress: Some("bzip2".to_string()),
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
        Recipe {
            corpus: CorpusMeta {
                id: "gutenberg".to_string(),
                name: "Project Gutenberg".to_string(),
                description: "Public-domain books from Project Gutenberg.".to_string(),
                license: "Public Domain".to_string(),
                mesh_sharing: true,
                size_compressed_gb: 12.0,
                size_indexed_gb: 30.0,
            },
            acquire: AcquirerConfig::BulkDownload {
                url: "https://www.gutenberg.org/robot/harvest".to_string(),
                resume: true,
            },
            extract: ExtractorConfig::Plaintext {
                title_pattern: None,
                strip_boilerplate: Some("gutenberg".to_string()),
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
    fn parse_wikipedia_recipe_from_toml() {
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
}
