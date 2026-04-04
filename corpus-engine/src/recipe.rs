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
        },
        // 5. Stanford Encyclopedia of Philosophy
        Recipe {
            corpus: CorpusMeta {
                id: "sep".to_string(),
                name: "Stanford Encyclopedia of Philosophy".to_string(),
                description: "Peer-reviewed encyclopedia entries from the Stanford Encyclopedia of Philosophy."
                    .to_string(),
                license: "Copyright Stanford University".to_string(),
                mesh_sharing: false,
                size_compressed_gb: 0.5,
                size_indexed_gb: 1.5,
            },
            acquire: AcquirerConfig::WebCrawl {
                seed_urls: vec![
                    "https://plato.stanford.edu/entries/".to_string(),
                ],
                link_pattern: r"https://plato\.stanford\.edu/entries/[\w-]+/".to_string(),
                max_pages: 10_000,
            },
            extract: ExtractorConfig::Html {
                content_selector: Some("#aueditable".to_string()),
                title_selector: Some("h1".to_string()),
            },
            chunk: ChunkerConfig::Paragraph {
                max_chars: 2048,
                overlap_chars: 256,
            },
            index: IndexConfig::default(),
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
        },
    ]
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
