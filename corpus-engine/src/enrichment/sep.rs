//! Stanford Encyclopedia of Philosophy parquet reader — extracts
//! one article's paragraphs by category slug and groups them into
//! numbered sections the atlas enrichment pipeline can consume.
//!
//! ## Source
//!
//! The SEP parquet mirror at
//! `https://huggingface.co/datasets/AiresPucrs/stanford-encyclopedia-philosophy`
//! has three columns:
//!
//! | column     | role                                              |
//! |------------|---------------------------------------------------|
//! | `metadata` | Article URL, e.g. `https://plato.stanford.edu/entries/compatibilism/` |
//! | `text`     | One paragraph of the article body                  |
//! | `category` | Article slug, stable across rows of the same article |
//!
//! One article spans many rows (compatibilism = 97 paragraphs,
//! recursive-functions = 542). Rows are ordered by paragraph
//! position within the article.
//!
//! ## Section grouping
//!
//! SEP paragraphs don't carry explicit section headers in the
//! parquet. For atlas enrichment we want ~10-20 sections per
//! article — small enough that each Phase-1 LLM call covers a
//! coherent chunk of argument, large enough that we're not
//! burning inference budget on single paragraphs. The default
//! `paragraphs_per_section = 5` matches the
//! `[enrichment.chunking]` field in `recipes/sep/recipe.toml`.
//!
//! ## Output shape
//!
//! The extracted markdown is a single plaintext file with
//! `## Section NNN` markers that match the default atlas
//! chapter_regex. That same shape was used by the
//! `process_philosophy` smoke-test corpus, so the pipeline path
//! from source → Phase 1 is already validated against it.

use std::fs::File;
use std::path::Path;

use arrow_array::{Array, StringArray};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use crate::error::{Error, Result};

/// One article extracted from the SEP parquet, ready for the atlas
/// enrichment pipeline.
#[derive(Debug, Clone)]
pub struct SepArticle {
    /// Category slug as it appears in the parquet (e.g.
    /// `compatibilism`). Matches the URL fragment on plato.stanford.edu.
    pub slug: String,
    /// First metadata URL seen for this article (usually identical
    /// across all of its paragraphs, but we don't assume).
    pub url: Option<String>,
    /// Per-paragraph body text, in parquet row order.
    pub paragraphs: Vec<String>,
}

impl SepArticle {
    /// Render the article as plaintext with `## Section NNN`
    /// markers, grouping every `paragraphs_per_section` paragraphs
    /// into one section. The section regex expected by
    /// `sovereign enrich init` (`(?m)^## Section \d+$`) matches
    /// this shape directly.
    pub fn render_markdown(&self, paragraphs_per_section: usize) -> String {
        let pps = paragraphs_per_section.max(1);
        let mut out = String::new();
        if let Some(url) = &self.url {
            out.push_str(&format!(
                "<!-- Stanford Encyclopedia of Philosophy — source: {url} -->\n\n"
            ));
        }
        let mut section_ord = 1usize;
        for chunk in self.paragraphs.chunks(pps) {
            out.push_str(&format!("## Section {:03}\n\n", section_ord));
            for (i, p) in chunk.iter().enumerate() {
                if i > 0 {
                    out.push_str("\n\n");
                }
                out.push_str(p.trim());
            }
            out.push_str("\n\n");
            section_ord += 1;
        }
        out
    }

    /// Section count that `render_markdown` would emit for a given
    /// paragraphs-per-section. Useful for "am I about to burn 100
    /// Phase-1 LLM calls?" checks before kicking off a run.
    pub fn section_count(&self, paragraphs_per_section: usize) -> usize {
        let pps = paragraphs_per_section.max(1);
        self.paragraphs.len().div_ceil(pps)
    }
}

/// Read the SEP parquet at `path` and return the article whose
/// `category` matches `slug` (case-sensitive — SEP slugs are
/// lowercase ASCII with hyphens, stable on plato.stanford.edu, so
/// there's no reason to fold case).
///
/// Returns `Err(Error::NotFound)` when the slug is absent from the
/// parquet. Other failures (IO, malformed parquet, missing columns)
/// propagate as `Error::Io` / `Error::InvalidInput`.
///
/// Streams the parquet via `ParquetRecordBatchReaderBuilder` in
/// default batch size — the full file fits in memory for SEP
/// (~1 GB compressed), but batch iteration keeps peak usage
/// bounded.
pub fn load_article(path: &Path, slug: &str) -> Result<SepArticle> {
    let file = File::open(path).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("opening SEP parquet {}: {e}", path.display()),
        ))
    })?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| {
        Error::InvalidInput(format!("reading SEP parquet metadata: {e}"))
    })?;
    let reader = builder.build().map_err(|e| {
        Error::InvalidInput(format!("building SEP parquet reader: {e}"))
    })?;

    let mut paragraphs: Vec<String> = Vec::new();
    let mut url: Option<String> = None;

    for batch in reader {
        let batch = batch
            .map_err(|e| Error::InvalidInput(format!("reading SEP parquet batch: {e}")))?;

        let text_col = batch
            .column_by_name("text")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            .ok_or_else(|| {
                Error::InvalidInput(
                    "SEP parquet is missing a string `text` column".into(),
                )
            })?;
        let category_col = batch
            .column_by_name("category")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            .ok_or_else(|| {
                Error::InvalidInput(
                    "SEP parquet is missing a string `category` column".into(),
                )
            })?;
        // metadata column is optional — recipes say it's URLs, but
        // a parquet produced by a different mirror might omit it.
        let metadata_col = batch
            .column_by_name("metadata")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());

        for row in 0..batch.num_rows() {
            if category_col.is_null(row) {
                continue;
            }
            let category = category_col.value(row);
            if category != slug {
                continue;
            }
            if !text_col.is_null(row) {
                let t = text_col.value(row).to_string();
                if !t.trim().is_empty() {
                    paragraphs.push(t);
                }
            }
            if url.is_none() {
                if let Some(meta) = metadata_col {
                    if !meta.is_null(row) {
                        url = Some(meta.value(row).to_string());
                    }
                }
            }
        }
    }

    if paragraphs.is_empty() {
        return Err(Error::InvalidInput(format!(
            "no paragraphs for SEP category `{slug}` in {}",
            path.display()
        )));
    }

    Ok(SepArticle {
        slug: slug.to_string(),
        url,
        paragraphs,
    })
}

/// List every distinct `category` value in the SEP parquet,
/// alongside its paragraph count. Useful for interactive
/// category-picking (`sep-ingest --list`) and for validating that
/// a slug the operator typed actually exists before kicking off
/// the extract.
///
/// Categories are returned sorted alphabetically so repeated calls
/// produce stable output.
pub fn list_categories(path: &Path) -> Result<Vec<(String, usize)>> {
    let file = File::open(path).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("opening SEP parquet {}: {e}", path.display()),
        ))
    })?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| {
        Error::InvalidInput(format!("reading SEP parquet metadata: {e}"))
    })?;
    let reader = builder.build().map_err(|e| {
        Error::InvalidInput(format!("building SEP parquet reader: {e}"))
    })?;

    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for batch in reader {
        let batch = batch
            .map_err(|e| Error::InvalidInput(format!("reading SEP parquet batch: {e}")))?;
        let category_col = batch
            .column_by_name("category")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            .ok_or_else(|| {
                Error::InvalidInput(
                    "SEP parquet is missing a string `category` column".into(),
                )
            })?;
        for row in 0..batch.num_rows() {
            if category_col.is_null(row) {
                continue;
            }
            *counts.entry(category_col.value(row).to_string()).or_insert(0) += 1;
        }
    }
    Ok(counts.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_article(slug: &str, n_paragraphs: usize) -> SepArticle {
        SepArticle {
            slug: slug.into(),
            url: Some(format!("https://plato.stanford.edu/entries/{slug}/")),
            paragraphs: (1..=n_paragraphs)
                .map(|i| format!("Paragraph {i} on {slug}."))
                .collect(),
        }
    }

    #[test]
    fn render_markdown_groups_paragraphs_into_numbered_sections() {
        // 13 paragraphs, 5 per section → 3 sections (5 + 5 + 3).
        let article = fake_article("compatibilism", 13);
        let md = article.render_markdown(5);
        assert_eq!(md.matches("## Section ").count(), 3);
        assert!(md.contains("## Section 001"));
        assert!(md.contains("## Section 002"));
        assert!(md.contains("## Section 003"));
        // URL comment prefix is present when url is set.
        assert!(md.contains("plato.stanford.edu/entries/compatibilism"));
        // Every paragraph body makes it into the output.
        for i in 1..=13 {
            assert!(
                md.contains(&format!("Paragraph {i} on compatibilism")),
                "paragraph {i} missing"
            );
        }
    }

    #[test]
    fn section_count_ceils_over_paragraphs_per_section() {
        let article = fake_article("x", 13);
        // 13 / 5 = 2.6 → ceil to 3.
        assert_eq!(article.section_count(5), 3);
        // 13 / 1 = 13 (one paragraph per section).
        assert_eq!(article.section_count(1), 13);
        // Guard the zero-input case — pps=0 is treated as 1 to
        // avoid divide-by-zero or empty-section surprises.
        assert_eq!(article.section_count(0), 13);
    }

    #[test]
    fn render_markdown_handles_exact_division_cleanly() {
        // 10 paragraphs, 5 per section → exactly 2 sections.
        let article = fake_article("x", 10);
        let md = article.render_markdown(5);
        assert_eq!(md.matches("## Section ").count(), 2);
        assert!(md.contains("## Section 001"));
        assert!(md.contains("## Section 002"));
        assert!(!md.contains("## Section 003"));
    }

    #[test]
    fn render_markdown_without_url_skips_the_source_comment() {
        let mut article = fake_article("x", 3);
        article.url = None;
        let md = article.render_markdown(5);
        assert!(!md.contains("<!--"));
        assert!(md.contains("## Section 001"));
    }
}
