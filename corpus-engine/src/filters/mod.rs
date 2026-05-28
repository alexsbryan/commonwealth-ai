//! Document-level filters that sit between extraction and chunking.
//!
//! A `DocumentFilter` accepts or rejects each `ExtractedDoc` produced by
//! the configured extractor before chunking, embedding, or indexing
//! happens. This is how recipes express **scope** — e.g. "only the top
//! 100K Wikipedia articles by pageview rank" — without forking the
//! pipeline or inventing a separate corpus identity.
//!
//! The filter stage is lazy: it wraps the extractor's
//! `Iterator<Result<ExtractedDoc>>` and drops rejected docs before they
//! reach the chunker. Errors are passed through unchanged so the
//! existing skip-and-keep-going behaviour in `ingest_inner` is
//! preserved.
//!
//! Filters compose via `FilterPipeline`, which combines child filters
//! with `ComposeMode::Any` (OR — accept if any filter accepts) or
//! `ComposeMode::All` (AND — accept only when every filter accepts).
//! For the Wikipedia Core scope we use `Any`: pageview rank ≤ 100k OR
//! title appears in the Vital Articles list.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use boilerplate::{BoilerplateConfig, BoilerplateFilter};
use sha2::{Digest, Sha256};

use crate::extractors::ExtractedDoc;

pub mod assets;
pub mod boilerplate;
pub mod knowledge_density;
pub mod loader;
pub mod pageview_rank;
pub mod title_list;

pub use knowledge_density::{KnowledgeDensityConfig, KnowledgeDensityFilter};
pub use loader::build_filter_pipeline;
pub use pageview_rank::PageviewRankFilter;
pub use title_list::TitleListFilter;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Decides which extracted documents enter the chunker.
pub trait DocumentFilter: Send + Sync {
    /// Returns true if this document should be ingested.
    fn accept(&self, doc: &ExtractedDoc) -> bool;

    /// Human-readable description for logging and `_corpus_meta.json`.
    fn description(&self) -> String;

    /// Total documents the filter expects to accept, when known up
    /// front (e.g. fixed title list). Used only for progress reporting;
    /// `None` is fine.
    fn expected_count(&self) -> Option<usize> {
        None
    }
}

// ---------------------------------------------------------------------------
// Recipe schema
// ---------------------------------------------------------------------------

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
    /// See [`crate::filters::KnowledgeDensityConfig`] for fields.
    KnowledgeDensity(KnowledgeDensityConfig),
    /// Reject email-shaped docs that are reduced to nothing after
    /// boilerplate (signatures, quoted-reply, corporate disclaimers)
    /// is stripped. See
    /// [`crate::filters::boilerplate::BoilerplateConfig`].
    /// Per-recipe configurable so corpora with code-in-mail or
    /// non-Outlook clients can tune their strip behaviour.
    Boilerplate(BoilerplateConfig),
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

/// Concrete, ready-to-run filter chain. Constructed by
/// [`build_filter_pipeline`] from the recipe's `[[filter]]` config + the
/// recipe override dir (so non-bundled artefacts can be resolved on
/// disk).
pub struct FilterPipeline {
    children: Vec<Arc<dyn DocumentFilter>>,
    mode: ComposeMode,
    /// Stable hash of the canonical filter config. Persisted to
    /// `_corpus_meta.json` as `scope.filter_signature` so a different
    /// scope on a re-ingest is detectable.
    signature: String,
}

impl FilterPipeline {
    pub fn new(children: Vec<Arc<dyn DocumentFilter>>, mode: ComposeMode, signature: String) -> Self {
        Self {
            children,
            mode,
            signature,
        }
    }

    /// Empty pipeline — `accept` is the identity. Used when a recipe
    /// has no `[[filter]]` block.
    pub fn empty() -> Self {
        Self {
            children: Vec::new(),
            mode: ComposeMode::Any,
            signature: String::new(),
        }
    }

    pub fn is_active(&self) -> bool {
        !self.children.is_empty()
    }

    pub fn signature(&self) -> &str {
        &self.signature
    }

    /// Human-readable list of child filter descriptions. Persisted as
    /// `scope.filter_descriptions` in `_corpus_meta.json`.
    pub fn descriptions(&self) -> Vec<String> {
        self.children.iter().map(|c| c.description()).collect()
    }

    pub fn accept(&self, doc: &ExtractedDoc) -> bool {
        if self.children.is_empty() {
            return true;
        }
        match self.mode {
            ComposeMode::Any => self.children.iter().any(|c| c.accept(doc)),
            ComposeMode::All => self.children.iter().all(|c| c.accept(doc)),
        }
    }

    /// Best-effort upper bound on accepted documents, when every child
    /// can self-report and the composite has unambiguous semantics.
    /// Used by the ingest progress reporter to give filtered ingests a
    /// real percent denominator instead of shard-scan progress.
    ///
    /// Returns `None` when:
    ///   - The pipeline is empty (no filter active; caller falls back).
    ///   - The pipeline has multiple children. The semantically correct
    ///     answer would be `min` (All mode) or somewhere in `[max, sum]`
    ///     (Any mode, depending on overlap), but we don't have child
    ///     overlap data and would rather return None than mislead the
    ///     UI. Single-filter recipes are the common case today; revisit
    ///     when a multi-filter recipe ships.
    pub fn expected_count(&self) -> Option<usize> {
        match self.children.len() {
            0 => None,
            1 => self.children[0].expected_count(),
            _ => None,
        }
    }
}

/// Compute a SHA-256 over the canonical (TOML-serialised) filter
/// config. Stable across processes given the same input — toml's
/// `to_string` sorts map keys deterministically.
pub fn compute_signature(filters: &[FilterConfig], mode: ComposeMode) -> String {
    if filters.is_empty() {
        return String::new();
    }
    #[derive(Serialize)]
    struct Canonical<'a> {
        mode: ComposeMode,
        filters: &'a [FilterConfig],
    }
    let canonical = Canonical { mode, filters };
    // Use serde_json for determinism — toml's serializer can re-order fields
    // for empty optional values across versions; serde_json is stable.
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    hex_lower(&digest)
}

fn hex_lower(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(TABLE[(b >> 4) as usize] as char);
        out.push(TABLE[(b & 0x0f) as usize] as char);
    }
    out
}

// ---------------------------------------------------------------------------
// Title normalization
// ---------------------------------------------------------------------------

/// Canonical form for matching titles across ranks/lists/extracted docs.
///
/// Wikipedia article titles arrive from extractors in several forms:
/// `"Albert Einstein"`, `"albert_einstein"` (URL slug), `"Albert  Einstein"`
/// (stray double-space). The pageview rank CSV ships with underscored
/// slugs. Normalizing to lowercase + collapsed-spaces + underscores-as-spaces
/// makes matching robust across all of these without losing precision —
/// `"Apple"` (the company) and `"apple"` (the fruit, separate Wikipedia
/// article) collapse, but Wikipedia handles disambiguation by suffix
/// (`Apple_(disambiguation)`) which the normalizer preserves.
pub fn normalize_title(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_space = false;
    for ch in raw.chars() {
        let mapped = if ch == '_' { ' ' } else { ch };
        if mapped.is_whitespace() {
            if !last_space && !out.is_empty() {
                out.push(' ');
                last_space = true;
            }
        } else {
            for low in mapped.to_lowercase() {
                out.push(low);
            }
            last_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Best-effort title for a document. Prefers `doc.title`; falls back to
/// the trailing path segment of `doc.url` (Wikipedia URLs end in the
/// underscored title); finally `source_id`.
pub fn doc_title_for_filter(doc: &ExtractedDoc) -> Option<String> {
    if let Some(t) = doc.title.as_deref() {
        if !t.is_empty() {
            return Some(normalize_title(t));
        }
    }
    if let Some(url) = doc.url.as_deref() {
        if let Some(seg) = url.rsplit('/').find(|s| !s.is_empty()) {
            // Wikipedia URLs are percent-encoded; the JSONL extractor
            // already decodes via `wiki_title_from_url`, but if a future
            // extractor doesn't, a raw underscored title is fine for
            // matching since the normalizer collapses underscores to
            // spaces.
            return Some(normalize_title(seg));
        }
    }
    if !doc.source_id.is_empty() {
        return Some(normalize_title(&doc.source_id));
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(title: &str) -> ExtractedDoc {
        ExtractedDoc {
            title: Some(title.to_string()),
            content: String::new(),
            url: None,
            source_id: title.to_string(),
            metadata: None,
            source_file: None,
            embed_text: None,
        }
    }

    #[test]
    fn normalize_collapses_underscores_and_case() {
        assert_eq!(normalize_title("Albert Einstein"), "albert einstein");
        assert_eq!(normalize_title("Albert_Einstein"), "albert einstein");
        assert_eq!(normalize_title("ALBERT  einstein"), "albert einstein");
        assert_eq!(normalize_title("  Apple_(disambiguation) "), "apple (disambiguation)");
    }

    #[test]
    fn doc_title_falls_back_to_url_slug() {
        let d = ExtractedDoc {
            title: None,
            content: String::new(),
            url: Some("https://en.wikipedia.org/wiki/Photosynthesis".into()),
            source_id: "id-42".into(),
            metadata: None,
            source_file: None,
            embed_text: None,
        };
        assert_eq!(doc_title_for_filter(&d).as_deref(), Some("photosynthesis"));
    }

    #[test]
    fn empty_pipeline_accepts_everything() {
        let p = FilterPipeline::empty();
        assert!(p.accept(&doc("anything")));
        assert!(!p.is_active());
    }

    #[test]
    fn any_mode_accepts_when_one_child_accepts() {
        struct Yes;
        impl DocumentFilter for Yes {
            fn accept(&self, _: &ExtractedDoc) -> bool { true }
            fn description(&self) -> String { "yes".into() }
        }
        struct No;
        impl DocumentFilter for No {
            fn accept(&self, _: &ExtractedDoc) -> bool { false }
            fn description(&self) -> String { "no".into() }
        }
        let p = FilterPipeline::new(
            vec![Arc::new(No), Arc::new(Yes)],
            ComposeMode::Any,
            "sig".into(),
        );
        assert!(p.accept(&doc("x")));
    }

    #[test]
    fn all_mode_rejects_when_one_child_rejects() {
        struct Yes;
        impl DocumentFilter for Yes {
            fn accept(&self, _: &ExtractedDoc) -> bool { true }
            fn description(&self) -> String { "yes".into() }
        }
        struct No;
        impl DocumentFilter for No {
            fn accept(&self, _: &ExtractedDoc) -> bool { false }
            fn description(&self) -> String { "no".into() }
        }
        let p = FilterPipeline::new(
            vec![Arc::new(Yes), Arc::new(No)],
            ComposeMode::All,
            "sig".into(),
        );
        assert!(!p.accept(&doc("x")));
    }

    #[test]
    fn signature_changes_with_filter_config() {
        let a = vec![FilterConfig::TitleList { list_file: "x".into() }];
        let b = vec![FilterConfig::TitleList { list_file: "y".into() }];
        assert_ne!(
            compute_signature(&a, ComposeMode::Any),
            compute_signature(&b, ComposeMode::Any),
        );
    }

    #[test]
    fn signature_is_empty_for_empty_filters() {
        assert_eq!(compute_signature(&[], ComposeMode::Any), "");
    }

    #[test]
    fn signature_is_stable_across_calls() {
        let cfg = vec![
            FilterConfig::PageviewRank { rank_file: "@bundled:r".into(), max_rank: 100_000 },
            FilterConfig::TitleList { list_file: "@bundled:v".into() },
        ];
        let s1 = compute_signature(&cfg, ComposeMode::Any);
        let s2 = compute_signature(&cfg, ComposeMode::Any);
        assert_eq!(s1, s2);
        assert_eq!(s1.len(), 64); // sha256 hex
    }

    /// Used as the public API surface check for crate consumers.
    #[allow(dead_code)]
    fn _trait_object_compiles() -> Box<dyn DocumentFilter> {
        Box::new(TitleListFilter::from_titles(std::iter::empty::<&str>()))
    }
}
