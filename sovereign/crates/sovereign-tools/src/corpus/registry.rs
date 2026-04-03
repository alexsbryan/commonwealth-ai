use std::path::Path;

use serde::Deserialize;

use sovereign_core::error::{Error, Result};

use super::CorpusParser;
use super::gutenberg::GutenbergParser;
use super::html_crawl::HtmlCrawlParser;
use super::openalex::OpenAlexParser;
use super::stackexchange::StackExchangeParser;
use super::wikipedia::WikimediaDumpParser;

// ─── TOML Schema ───────────��──────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct Manifest {
    #[serde(default)]
    corpus: Vec<CorpusDefinition>,
    #[serde(default)]
    tier: Vec<TierDefinition>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CorpusDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source_url: String,
    pub format: String,
    pub size_compressed_gb: f64,
    pub size_indexed_gb: f64,
    pub update_frequency: String,
    pub license: String,
    pub tiers: Vec<String>,
    pub filter: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TierDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub corpora: Vec<String>,
}

// ─── Registry ───��─────────────────────────────────────────────

pub struct CorpusRegistry {
    corpora: Vec<CorpusDefinition>,
    tiers: Vec<TierDefinition>,
}

impl CorpusRegistry {
    /// Load a corpus manifest from a TOML file on disk.
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| Error::Storage(format!("Failed to read corpus manifest: {e}")))?;
        Self::from_toml(&content)
    }

    /// Parse a corpus manifest from a TOML string.
    pub fn from_toml(toml_str: &str) -> Result<Self> {
        let manifest: Manifest = toml::from_str(toml_str)
            .map_err(|e| Error::Serialization(format!("Invalid corpus manifest: {e}")))?;
        Ok(Self {
            corpora: manifest.corpus,
            tiers: manifest.tier,
        })
    }

    pub fn get_corpus(&self, id: &str) -> Option<&CorpusDefinition> {
        self.corpora.iter().find(|c| c.id == id)
    }

    pub fn get_tier(&self, id: &str) -> Option<&TierDefinition> {
        self.tiers.iter().find(|t| t.id == id)
    }

    pub fn list_corpora(&self) -> &[CorpusDefinition] {
        &self.corpora
    }

    pub fn list_tiers(&self) -> &[TierDefinition] {
        &self.tiers
    }

    pub fn corpora_for_tier(&self, tier_id: &str) -> Vec<&CorpusDefinition> {
        let tier = match self.get_tier(tier_id) {
            Some(t) => t,
            None => return Vec::new(),
        };
        tier.corpora
            .iter()
            .filter_map(|cid| self.get_corpus(cid))
            .collect()
    }

    /// Instantiate the appropriate parser for a corpus ID.
    pub fn parser_for_corpus(&self, id: &str) -> Option<Box<dyn CorpusParser>> {
        match id {
            "wikipedia" => Some(Box::new(WikimediaDumpParser::new("wikipedia"))),
            "stackexchange" => Some(Box::new(StackExchangeParser::new("stackexchange", 3))),
            "sep" => Some(Box::new(HtmlCrawlParser::new("sep"))),
            "crs_reports" => Some(Box::new(HtmlCrawlParser::new("crs_reports"))),
            "gutenberg" => Some(Box::new(GutenbergParser::new("gutenberg"))),
            "openalex" => Some(Box::new(OpenAlexParser::new("openalex"))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_TOML: &str = r#"
[[corpus]]
id = "wikipedia"
name = "Wikipedia"
description = "English Wikipedia"
source_url = "https://dumps.wikimedia.org/enwiki/latest/enwiki-latest-pages-articles.xml.bz2"
format = "mediawiki-xml-bz2"
size_compressed_gb = 22.0
size_indexed_gb = 55.0
update_frequency = "monthly"
license = "CC BY-SA 4.0"
tiers = ["essential", "full"]

[[corpus]]
id = "openalex"
name = "OpenAlex"
description = "Scholarly abstracts"
source_url = "https://openalex.s3.amazonaws.com/data/works/"
format = "jsonl"
size_compressed_gb = 20.0
size_indexed_gb = 45.0
update_frequency = "monthly"
license = "CC0"
tiers = ["research", "full"]
filter = "year >= 2010"

[[tier]]
id = "essential"
name = "Essential"
description = "General knowledge"
corpora = ["wikipedia"]

[[tier]]
id = "research"
name = "Research"
description = "Scholarly sources"
corpora = ["wikipedia", "openalex"]

[[tier]]
id = "full"
name = "Full"
description = "All corpora"
corpora = ["wikipedia", "openalex"]
"#;

    #[test]
    fn load_manifest() {
        let reg = CorpusRegistry::from_toml(TEST_TOML).unwrap();
        assert_eq!(reg.list_corpora().len(), 2);
        assert_eq!(reg.list_tiers().len(), 3);
    }

    #[test]
    fn get_corpus_by_id() {
        let reg = CorpusRegistry::from_toml(TEST_TOML).unwrap();
        let wiki = reg.get_corpus("wikipedia").unwrap();
        assert_eq!(wiki.name, "Wikipedia");
        assert!(reg.get_corpus("nonexistent").is_none());
    }

    #[test]
    fn corpora_for_tier() {
        let reg = CorpusRegistry::from_toml(TEST_TOML).unwrap();
        let essential = reg.corpora_for_tier("essential");
        assert_eq!(essential.len(), 1);
        assert_eq!(essential[0].id, "wikipedia");

        let research = reg.corpora_for_tier("research");
        assert_eq!(research.len(), 2);
    }

    #[test]
    fn parser_for_known_corpus() {
        let reg = CorpusRegistry::from_toml(TEST_TOML).unwrap();
        assert!(reg.parser_for_corpus("wikipedia").is_some());
        assert!(reg.parser_for_corpus("openalex").is_some());
        assert!(reg.parser_for_corpus("unknown").is_none());
    }
}
