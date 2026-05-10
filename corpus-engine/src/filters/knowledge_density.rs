//! Knowledge-density document filter for Stack Exchange grouped Q&A.
//!
//! StackExchange's Stack Overflow site is moderated to systematically
//! prefer single-answer reference posts ("here is the canonical syntax
//! for X") over multi-answer trade-off threads ("here are five
//! validated approaches to X"). The naive score-only filter that the
//! original placeholder recipe used would harvest exactly the
//! reference shape — exactly the shape an 8B model already covers
//! from training data, and exactly the shape ChatGPT/Copilot
//! displaced.
//!
//! [`KnowledgeDensityFilter`] complements the score filter by
//! requiring multiple substantive answers under a single question.
//! It pairs with the `question_with_answers` extraction mode (one
//! grouped doc per question, with knowledge-density signals
//! pre-computed and stored on the doc's metadata):
//!
//! ```toml
//! [[filter]]
//! type = "knowledge_density"
//! min_substantive_answers = 3
//! answer_score_threshold = 5
//! min_answer_length = 500
//! exclude_closed = true
//! apply_to = ["stackoverflow.com"]   # smaller SE sites pass through
//! ```
//!
//! `apply_to` is the escape hatch that lets the same recipe combine
//! breadth-only sources (where every passable post is keepable, e.g.
//! Software Engineering SE) with density-cut sources (Stack Overflow,
//! where most threads are reference). Communities not listed in
//! `apply_to` are accepted unconditionally — the filter is opt-in
//! per-site.

use serde::{Deserialize, Serialize};

use crate::extractors::ExtractedDoc;

use super::DocumentFilter;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDensityConfig {
    /// Minimum number of answers (after the score/length floors) that
    /// must survive on the question for it to be accepted. The whole
    /// point of this filter — single-answer threads are the reference
    /// shape, three+ answer threads are the trade-off shape.
    #[serde(default = "default_min_substantive_answers")]
    pub min_substantive_answers: u32,

    /// Score floor for an answer to count toward `min_substantive_answers`.
    /// Mirrors the extractor's `min_score`; restated here so a recipe
    /// can ratchet the density check tighter than the extraction cut
    /// (e.g. extract at score ≥ 3 but require density at score ≥ 5).
    #[serde(default = "default_answer_score_threshold")]
    pub answer_score_threshold: i32,

    /// Length floor for an answer to count. Eliminates one-line
    /// "+1 to the above" / "use sorted()" snippets that inflate
    /// answer count without adding retrievable knowledge.
    #[serde(default = "default_min_answer_length")]
    pub min_answer_length: u64,

    /// Reject questions whose `closed` metadata flag is true. Stack
    /// Overflow's closed-question moderation flag is a high-precision
    /// signal that the community judged the thread off-topic /
    /// duplicate / opinion-based — even if it has multiple answers,
    /// the answer set tends not to be a coherent trade-off space.
    #[serde(default = "default_true")]
    pub exclude_closed: bool,

    /// Optional tag whitelist — accept only questions tagged with at
    /// least one listed tag. Use to scope the cut to architecture /
    /// design discussions on Stack Overflow while letting smaller
    /// already-knowledge-dense sites pass everything.
    #[serde(default)]
    pub tag_filter: Option<Vec<String>>,

    /// Optional community whitelist — apply the density check only on
    /// these communities. Documents from communities not listed are
    /// accepted regardless. This is the recipe-level escape hatch
    /// that lets a single recipe combine breadth-pass sources with
    /// density-cut sources. `None` (default) applies to every
    /// community.
    #[serde(default)]
    pub apply_to: Option<Vec<String>>,
}

fn default_min_substantive_answers() -> u32 {
    3
}

fn default_answer_score_threshold() -> i32 {
    5
}

fn default_min_answer_length() -> u64 {
    500
}

fn default_true() -> bool {
    true
}

impl Default for KnowledgeDensityConfig {
    fn default() -> Self {
        Self {
            min_substantive_answers: default_min_substantive_answers(),
            answer_score_threshold: default_answer_score_threshold(),
            min_answer_length: default_min_answer_length(),
            exclude_closed: default_true(),
            tag_filter: None,
            apply_to: None,
        }
    }
}

/// Concrete filter constructed from [`KnowledgeDensityConfig`].
pub struct KnowledgeDensityFilter {
    cfg: KnowledgeDensityConfig,
    apply_to_lower: Option<Vec<String>>,
    tag_filter_lower: Option<Vec<String>>,
}

impl KnowledgeDensityFilter {
    pub fn new(cfg: KnowledgeDensityConfig) -> Self {
        let apply_to_lower = cfg.apply_to.as_ref().map(|sites| {
            sites
                .iter()
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect()
        });
        let tag_filter_lower = cfg.tag_filter.as_ref().map(|tags| {
            tags.iter()
                .map(|t| t.trim().to_ascii_lowercase())
                .filter(|t| !t.is_empty())
                .collect()
        });
        Self {
            cfg,
            apply_to_lower,
            tag_filter_lower,
        }
    }
}

impl DocumentFilter for KnowledgeDensityFilter {
    fn accept(&self, doc: &ExtractedDoc) -> bool {
        let Some(meta) = doc.metadata.as_ref() else {
            // Without metadata we can't evaluate density — accept by
            // default so non-grouped extractors aren't blocked.
            return true;
        };

        // Scope: only apply on listed communities (when set).
        if let Some(ref sites) = self.apply_to_lower {
            let community = meta
                .get("community")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !sites.iter().any(|s| s == &community) {
                return true;
            }
        }

        // Closed-question gate.
        if self.cfg.exclude_closed {
            let closed = meta.get("closed").and_then(|v| v.as_bool()).unwrap_or(false);
            if closed {
                return false;
            }
        }

        // Tag whitelist (when set).
        if let Some(ref filter) = self.tag_filter_lower {
            let tags = meta
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_ascii_lowercase()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if !tags.iter().any(|t| filter.iter().any(|f| f == t)) {
                return false;
            }
        }

        // Density check: enough answers passing both the score and
        // length floors. The grouped extractor records min/max across
        // the answer set; we approximate per-answer pass with the
        // conservative `min_*` fields, which is exact when every
        // answer in the doc passes the floors and rejects otherwise
        // (the grouped doc holds at most `max_answers_per_question`
        // already-truncated answers, so we read its full count).
        let answer_count = meta
            .get("answer_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let min_score = meta
            .get("min_answer_score")
            .and_then(|v| v.as_i64())
            .unwrap_or(i64::MIN) as i32;
        let min_len = meta
            .get("min_answer_length")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        if min_score < self.cfg.answer_score_threshold {
            return false;
        }
        if min_len < self.cfg.min_answer_length {
            return false;
        }
        if answer_count < self.cfg.min_substantive_answers as u64 {
            return false;
        }
        true
    }

    fn description(&self) -> String {
        format!(
            "knowledge_density(min_answers={}, score≥{}, len≥{}{}{}{})",
            self.cfg.min_substantive_answers,
            self.cfg.answer_score_threshold,
            self.cfg.min_answer_length,
            if self.cfg.exclude_closed { ", closed=excluded" } else { "" },
            self.cfg
                .tag_filter
                .as_ref()
                .filter(|t| !t.is_empty())
                .map(|tags| format!(", tags=[{}]", tags.join(",")))
                .unwrap_or_default(),
            self.cfg
                .apply_to
                .as_ref()
                .filter(|s| !s.is_empty())
                .map(|sites| format!(", apply_to=[{}]", sites.join(",")))
                .unwrap_or_default(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc_with_meta(meta: serde_json::Value) -> ExtractedDoc {
        ExtractedDoc {
            title: Some("t".into()),
            content: String::new(),
            url: None,
            source_id: "id".into(),
            metadata: Some(meta),
            source_file: None,
            embed_text: None,
        }
    }

    #[test]
    fn defaults_reject_single_answer_thread() {
        let f = KnowledgeDensityFilter::new(KnowledgeDensityConfig::default());
        let d = doc_with_meta(json!({
            "community": "stackoverflow.com",
            "closed": false,
            "answer_count": 1,
            "max_answer_score": 50,
            "min_answer_score": 50,
            "min_answer_length": 1200,
        }));
        assert!(!f.accept(&d));
    }

    #[test]
    fn accepts_multi_answer_thread_with_density() {
        let f = KnowledgeDensityFilter::new(KnowledgeDensityConfig::default());
        let d = doc_with_meta(json!({
            "community": "stackoverflow.com",
            "closed": false,
            "answer_count": 3,
            "max_answer_score": 50,
            "min_answer_score": 6,
            "min_answer_length": 600,
        }));
        assert!(f.accept(&d));
    }

    #[test]
    fn rejects_when_min_answer_score_below_floor() {
        let f = KnowledgeDensityFilter::new(KnowledgeDensityConfig::default());
        let d = doc_with_meta(json!({
            "community": "stackoverflow.com",
            "answer_count": 5,
            "min_answer_score": 4,
            "min_answer_length": 700,
        }));
        assert!(!f.accept(&d));
    }

    #[test]
    fn rejects_closed_questions_by_default() {
        let f = KnowledgeDensityFilter::new(KnowledgeDensityConfig::default());
        let d = doc_with_meta(json!({
            "community": "stackoverflow.com",
            "closed": true,
            "answer_count": 5,
            "min_answer_score": 10,
            "min_answer_length": 1000,
        }));
        assert!(!f.accept(&d));
    }

    #[test]
    fn apply_to_passes_other_communities_unfiltered() {
        let cfg = KnowledgeDensityConfig {
            apply_to: Some(vec!["stackoverflow.com".into()]),
            ..Default::default()
        };
        let f = KnowledgeDensityFilter::new(cfg);
        // softwareengineering.stackexchange.com — single-answer would
        // normally be rejected, but apply_to scopes the filter to SO.
        let d = doc_with_meta(json!({
            "community": "softwareengineering.stackexchange.com",
            "answer_count": 1,
            "min_answer_score": 1,
            "min_answer_length": 100,
        }));
        assert!(f.accept(&d));
    }

    #[test]
    fn tag_filter_requires_at_least_one_listed_tag() {
        let cfg = KnowledgeDensityConfig {
            tag_filter: Some(vec!["architecture".into(), "design-patterns".into()]),
            ..Default::default()
        };
        let f = KnowledgeDensityFilter::new(cfg);
        let with = doc_with_meta(json!({
            "community": "stackoverflow.com",
            "answer_count": 3,
            "min_answer_score": 10,
            "min_answer_length": 1000,
            "tags": ["architecture", "java"],
        }));
        let without = doc_with_meta(json!({
            "community": "stackoverflow.com",
            "answer_count": 3,
            "min_answer_score": 10,
            "min_answer_length": 1000,
            "tags": ["python", "regex"],
        }));
        assert!(f.accept(&with));
        assert!(!f.accept(&without));
    }

    #[test]
    fn missing_metadata_passes_through() {
        let f = KnowledgeDensityFilter::new(KnowledgeDensityConfig::default());
        let d = ExtractedDoc {
            title: Some("t".into()),
            content: String::new(),
            url: None,
            source_id: "id".into(),
            metadata: None,
            source_file: None,
            embed_text: None,
        };
        assert!(f.accept(&d));
    }
}
