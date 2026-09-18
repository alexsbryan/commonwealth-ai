// SPDX-License-Identifier: AGPL-3.0-or-later
//! The filter configuration the index persists.
//!
//! `FilterConfig`, its `ComposeMode` and the two per-filter configs are
//! written into `_corpus_meta.json` (`scope.filter_override`) by the index, so
//! they are DEFINED here and `corpus-engine`'s `filters` module re-exports
//! them (DE "The read-port leaf, measured again": "a type the index persists is
//! defined by the index").

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComposeMode {
    /// Accept if any child filter accepts. Default — matches the
    /// "Wikipedia Core = top-ranked OR vital" semantics.
    #[default]
    Any,
    /// Accept only when every child filter accepts.
    All,
}

/// One entry from a recipe's `[[filter]]` array.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FilterConfig {
    /// Accept articles whose normalized title appears in a pageview-rank
    /// CSV with rank ≤ `max_rank`. The CSV is a two-column
    /// `title,rank` table.
    PageviewRank {
        /// Either a bundled-asset key (`@bundled:pageview_ranks_202311`)
        /// or a path relative to the recipe override directory.
        rank_file: String,
        max_rank: u32,
    },
    /// Accept articles whose normalized title appears in a newline-delimited
    /// title list. Useful for curated sets like Wikipedia Vital Articles.
    TitleList {
        /// Either a bundled-asset key (`@bundled:vital_articles_l5`) or
        /// a path relative to the recipe override directory.
        list_file: String,
    },
    /// Accept Stack Exchange grouped Q&A docs (one doc per question)
    /// only when their answer set carries enough density to count as
    /// a trade-off thread rather than a single-answer reference post.
    /// See [`KnowledgeDensityConfig`] for fields.
    KnowledgeDensity(KnowledgeDensityConfig),
    /// Reject email-shaped docs that are reduced to nothing after
    /// boilerplate (signatures, quoted-reply, corporate disclaimers)
    /// is stripped. See [`BoilerplateConfig`].
    /// Per-recipe configurable so corpora with code-in-mail or
    /// non-Outlook clients can tune their strip behaviour.
    Boilerplate(BoilerplateConfig),
}

/// Per-recipe configuration for the boilerplate filter. Each
/// detection axis can be disabled independently — useful for corpora
/// where the "reply quote" lines aren't quoted prefixes (Outlook's
/// "On Date X wrote:" pattern), or where signature-block heuristics
/// produce false positives (e.g. code in monospace mail).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoilerplateConfig {
    /// Strip `-- ` -prefixed signature blocks (RFC 3676 §4.3.2) and
    /// strong heuristic siblings ("Sent from my iPhone", "Best
    /// regards,\n<name>").
    #[serde(default = "default_true_bool")]
    pub strip_signatures: bool,
    /// Strip RFC 3676 §4.5 quoted-reply blocks — lines starting with
    /// `>` (one or more).
    #[serde(default = "default_true_bool")]
    pub strip_quoted_replies: bool,
    /// Strip common corporate-disclaimer trailers ("This email and
    /// any files transmitted with it…").
    #[serde(default = "default_true_bool")]
    pub strip_disclaimers: bool,
    /// Reject docs whose body becomes shorter than this many chars
    /// after stripping. Default 20 — anything shorter is empty for
    /// retrieval purposes.
    #[serde(default = "default_min_body_chars_after_strip")]
    pub min_body_chars_after_strip: usize,
}

impl Default for BoilerplateConfig {
    fn default() -> Self {
        Self {
            strip_signatures: true,
            strip_quoted_replies: true,
            strip_disclaimers: true,
            min_body_chars_after_strip: 20,
        }
    }
}

fn default_true_bool() -> bool {
    true
}

fn default_min_body_chars_after_strip() -> usize {
    20
}

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

