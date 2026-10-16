// SPDX-License-Identifier: AGPL-3.0-or-later
//! The platform's data file for sentence search
//! (`svrn/crates/sovereign-daemon/data/sentence_search.toml`): the scorer, its
//! candidate count, the scan's ceiling, verify's relevance floor and the
//! indexer's pace. Data, not program: every number sentence search and
//! verify by meaning act on is read from here.

use oicp_types::SentenceScorer;
use serde::Deserialize;

/// The data file, vendored into the binary.
pub const DATA_FILE: &str = include_str!("../../data/sentence_search.toml");

/// What the data file says. Every key is required and an unknown key is an
/// error, so a typo never reads as a default.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SentenceSearchConfig {
    /// The scorer every reply is ranked by and names.
    pub scorer: SentenceScorer,
    /// How many of cosine's best `rerank` reorders.
    pub rerank_candidates: usize,
    /// Sentences one scope may hold before the scan must become approximate.
    pub scan_ceiling: usize,
    /// A corpus holding more chunks than this is not sentence-indexed.
    pub index_ceiling_chunks: u64,
    /// Passage hits whose sentences the passages route scores.
    pub passage_hits: usize,
    /// Verify's relevance floor (OICP v0.6 §2.15), on the scorer's scale: a
    /// claim no sentence reaches is `unsupported` with no evidence.
    pub verify_floor: f32,
    /// Sentences per background embed call.
    pub embed_batch: usize,
    /// Seconds between a corpus's progress writes while it embeds.
    pub checkpoint_secs: u64,
}

impl SentenceSearchConfig {
    /// The vendored data file. It is checked by this crate's tests, so a
    /// shipped binary whose file does not parse cannot be built green; should
    /// one run anyway, the start says so and sentence search is not offered.
    pub fn from_data_file() -> Result<Self, String> {
        Self::parse(DATA_FILE)
    }

    pub fn parse(toml_text: &str) -> Result<Self, String> {
        let c: Self = toml::from_str(toml_text).map_err(|e| e.to_string())?;
        if c.embed_batch == 0 || c.passage_hits == 0 || c.scan_ceiling == 0 {
            return Err(
                "embed_batch, passage_hits and scan_ceiling must each be at least 1".into(),
            );
        }
        if !c.verify_floor.is_finite() {
            return Err("verify_floor must be a finite score".into());
        }
        if c.scorer == SentenceScorer::Rerank && c.rerank_candidates == 0 {
            return Err("rerank names no candidates: rerank_candidates is 0".into());
        }
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped data file parses, names the default scorer, and is held
    /// to its closed set: a scorer outside it or an unknown key is an error.
    #[test]
    fn the_data_file_parses_and_is_closed() {
        let c = SentenceSearchConfig::from_data_file().expect("the data file parses");
        assert_eq!(c.scorer, SentenceScorer::Cosine);
        assert!(c.rerank_candidates >= 20 && c.scan_ceiling > 0);
        let bm25 = DATA_FILE.replace("scorer = \"cosine\"", "scorer = \"bm25\"");
        assert!(SentenceSearchConfig::parse(&bm25).is_err());
        let typo = format!("{DATA_FILE}\nscan_cieling = 3\n");
        assert!(SentenceSearchConfig::parse(&typo).is_err());
        assert!((0.0..1.0).contains(&c.verify_floor));
        let unbounded = DATA_FILE.replace("verify_floor = 0.5", "verify_floor = nan");
        assert!(SentenceSearchConfig::parse(&unbounded).is_err());
    }
}
