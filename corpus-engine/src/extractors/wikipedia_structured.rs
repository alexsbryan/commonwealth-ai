//! Extractor for the `wikimedia/structured-wikipedia` HuggingFace dataset.
//!
//! This dataset stores each article as a single parquet row with deeply-nested
//! Arrow data: version metadata (maintenance tags), and sections as a
//! `List<Struct>` that can recurse via `has_parts`.
//!
//! The extractor flattens the nested structure into one `ExtractedDoc` per
//! section. Every doc carries `WikipediaChunkMetadata` (section_name,
//! section_type, editorial maintenance tags, outgoing links) serialised to
//! JSON in `ExtractedDoc.metadata`. Downstream, the paragraph chunker splits
//! long sections into overlapping chunks; the metadata is propagated to each.
//!
//! Schema (wikimedia/structured-wikipedia 20240916.en):
//!   name: Utf8                — article title
//!   abstract: Utf8 (null)     — lead paragraph
//!   url: Utf8                 — full Wikipedia URL
//!   date_modified: Utf8
//!   version: Struct {
//!     is_flagged_stable: Bool,
//!     maintenance_tags: Struct {
//!       citation_needed_count: Int64,
//!       pov_count: Int64,
//!       clarification_needed_count: Int64,
//!       update_count: Int64,
//!     },
//!   }
//!   sections: List<Struct {
//!     type: Utf8, name: Utf8, value: Utf8,
//!     links: List<Struct { url: Utf8, text: Utf8 }>,
//!     has_parts: List<Struct { ... (recursive) }>,
//!   }>

use std::collections::VecDeque;
use std::fs::File;
use std::path::{Path, PathBuf};

use arrow::array::{Array, ArrayRef, ListArray, LargeListArray, StructArray};
use arrow::datatypes::Int64Type;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use super::wikipedia_types::{WikiLink, WikipediaChunkMetadata, wiki_title_from_url};
use crate::error::{Error, Result};
use super::{ExtractedDoc, Extractor, slug};

/// Maximum recursion depth for `has_parts` traversal.
pub const MAX_SECTION_DEPTH: u32 = 5;

/// Minimum section text length to emit as a chunk (very short sections
/// — e.g. stubs, navigation blurbs — are not worth indexing).
pub const MIN_SECTION_TEXT: usize = 50;

/// Section names that are purely navigational; skip them entirely.
pub const SKIP_SECTIONS: &[&str] = &[
    "references",
    "external links",
    "see also",
    "further reading",
    "notes",
    "footnotes",
    "sources",
    "bibliography",
    "works cited",
];

/// Section name substrings that indicate contested/critical content.
pub const DEFAULT_CONTROVERSY_PATTERNS: &[&str] = &[
    "criticism",
    "controversy",
    "controversies",
    "debate",
    "disputes",
    "opposition",
    "opposing views",
    "counter-arguments",
    "counterarguments",
    "reception",
    "critical reception",
    "limitations",
    "challenges",
    "legal issues",
    "ethical concerns",
    "scientific evaluation",
    "evidence",
    "political views",
    "political positions",
];

/// Section name substrings that indicate biographical/factual content.
pub const DEFAULT_FACTUAL_PATTERNS: &[&str] = &[
    "early life",
    "biography",
    "career",
    "geography",
    "demographics",
    "climate",
    "filmography",
    "discography",
    "bibliography",
];

/// Extractor for the `wikimedia/structured-wikipedia` dataset.
pub struct WikipediaStructuredExtractor {
    /// Parquet column for the article title (default: "name").
    pub title_column: String,
    /// Parquet column for the article URL (default: "url").
    pub url_column: String,
    /// Case-insensitive substrings that classify a section as controversy.
    pub controversy_patterns: Vec<String>,
    /// Case-insensitive substrings that classify a section as factual.
    pub factual_patterns: Vec<String>,
}

impl Default for WikipediaStructuredExtractor {
    fn default() -> Self {
        Self {
            title_column: "name".to_string(),
            url_column: "url".to_string(),
            controversy_patterns: DEFAULT_CONTROVERSY_PATTERNS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            factual_patterns: DEFAULT_FACTUAL_PATTERNS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

impl Extractor for WikipediaStructuredExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let paths = collect_parquet_paths(source_path)?;
        if paths.is_empty() {
            return Err(Error::Extraction(format!(
                "No .parquet files found at: {}",
                source_path.display()
            )));
        }

        Ok(Box::new(WikipediaShardIterator {
            paths: paths.into(),
            current: None,
            title_column: self.title_column.clone(),
            url_column: self.url_column.clone(),
            controversy_patterns: self.controversy_patterns.clone(),
            factual_patterns: self.factual_patterns.clone(),
        }))
    }
}

fn collect_parquet_paths(source_path: &Path) -> Result<Vec<PathBuf>> {
    if source_path.is_file() {
        return Ok(vec![source_path.to_path_buf()]);
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(source_path)
        .map_err(|e| {
            Error::Extraction(format!(
                "Failed to read directory {}: {e}",
                source_path.display()
            ))
        })?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("parquet"))
        .collect();
    paths.sort();
    Ok(paths)
}

// ─── Shard-chaining iterator ────────────────────────────

struct WikipediaShardIterator {
    paths: VecDeque<PathBuf>,
    current: Option<WikipediaBatchIterator>,
    title_column: String,
    url_column: String,
    controversy_patterns: Vec<String>,
    factual_patterns: Vec<String>,
}

impl Iterator for WikipediaShardIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(ref mut iter) = self.current {
                if let Some(item) = iter.next() {
                    return Some(item);
                }
                self.current = None;
            }

            let path = self.paths.pop_front()?;
            let file = match File::open(&path) {
                Ok(f) => f,
                Err(e) => {
                    return Some(Err(Error::Extraction(format!(
                        "Failed to open shard {}: {e}",
                        path.display()
                    ))));
                }
            };

            let builder = match ParquetRecordBatchReaderBuilder::try_new(file) {
                Ok(b) => b,
                Err(e) => {
                    return Some(Err(Error::Extraction(format!(
                        "Failed to read parquet shard {}: {e}",
                        path.display()
                    ))));
                }
            };

            let reader = match builder.with_batch_size(64).build() {
                Ok(r) => r,
                Err(e) => {
                    return Some(Err(Error::Extraction(format!(
                        "Failed to build reader for shard {}: {e}",
                        path.display()
                    ))));
                }
            };

            self.current = Some(WikipediaBatchIterator {
                reader: Box::new(reader),
                pending: VecDeque::new(),
                title_column: self.title_column.clone(),
                url_column: self.url_column.clone(),
                controversy_patterns: self.controversy_patterns.clone(),
                factual_patterns: self.factual_patterns.clone(),
            });
        }
    }
}

// ─── Batch-level iterator ───────────────────────────────

type RecordBatchReader = Box<
    dyn Iterator<
            Item = std::result::Result<
                arrow::array::RecordBatch,
                arrow::error::ArrowError,
            >,
        > + Send,
>;

struct WikipediaBatchIterator {
    reader: RecordBatchReader,
    pending: VecDeque<ExtractedDoc>,
    title_column: String,
    url_column: String,
    controversy_patterns: Vec<String>,
    factual_patterns: Vec<String>,
}

impl Iterator for WikipediaBatchIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(doc) = self.pending.pop_front() {
                return Some(Ok(doc));
            }

            let batch = match self.reader.next()? {
                Ok(b) => b,
                Err(e) => {
                    return Some(Err(Error::Extraction(format!(
                        "Parquet read error: {e}"
                    ))));
                }
            };

            match self.process_batch(&batch) {
                Ok(docs) => {
                    self.pending.extend(docs);
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

impl WikipediaBatchIterator {
    fn process_batch(
        &self,
        batch: &arrow::array::RecordBatch,
    ) -> Result<Vec<ExtractedDoc>> {
        let mut docs = Vec::new();

        // ── Top-level string columns ────────────────────
        let title_col = batch.column_by_name(&self.title_column);
        let url_col = batch.column_by_name(&self.url_column);
        let abstract_col = batch.column_by_name("abstract");

        // ── Top-level page ID column ────────────────────
        let identifier_col = batch.column_by_name("identifier");

        // ── Per-article signals (maintenance tags + revision ID + QID) ──
        let signals_arr = extract_article_signals(batch);

        // ── Sections list column ─────────────────────────
        let sections_col = batch.column_by_name("sections");

        for row in 0..batch.num_rows() {
            let title = get_string_opt(title_col, row).filter(|s| !s.is_empty());
            let title = match title {
                Some(t) => t,
                None => continue, // skip rows without a title
            };

            let url = get_string_opt(url_col, row).unwrap_or_default();

            // Article-level signals (same for every chunk from this article).
            let signals = signals_arr.as_ref().map(|a| a.row(row)).unwrap_or_default();

            // Page ID from top-level identifier column.
            let page_id = get_i64_opt(identifier_col, row);

            // ── Lead / abstract chunk ───────────────────
            if let Some(abstract_text) = get_string_opt(abstract_col, row) {
                let text = abstract_text.trim().to_string();
                if text.len() >= MIN_SECTION_TEXT {
                    let meta = WikipediaChunkMetadata {
                        section_name: "Lead".to_string(),
                        section_path: vec![],
                        section_depth: 0,
                        section_type: "lead".to_string(),
                        citation_needed_count: signals.citation_needed,
                        pov_count: signals.pov,
                        clarification_needed_count: signals.clarification,
                        update_count: signals.update,
                        is_flagged_stable: signals.is_flagged_stable,
                        outgoing_links: vec![],
                        revision_id: signals.revision_id,
                        wikidata_qid: signals.wikidata_qid.clone(),
                        page_id,
                    };
                    docs.push(ExtractedDoc {
                        title: Some(title.clone()),
                        content: text,
                        url: Some(url.clone()),
                        source_id: format!("{}-lead", slug(&title)),
                        metadata: serde_json::to_value(&meta).ok(),
                    });
                }
            }

            // ── Section chunks (recursive) ───────────────
            if let Some(col) = sections_col {
                extract_sections_from_list(
                    col.as_ref(),
                    row,
                    &title,
                    &url,
                    &signals,
                    page_id,
                    &self.controversy_patterns,
                    &self.factual_patterns,
                    &[],  // parent path
                    0,    // depth
                    &mut docs,
                );
            }
        }

        Ok(docs)
    }
}

// ─── Per-article signals extraction ─────────────────────

/// Signals extracted per article row: maintenance tags + revision/entity IDs.
#[derive(Default, Clone)]
struct PerArticleSignals {
    citation_needed: Option<i64>,
    pov: Option<i64>,
    clarification: Option<i64>,
    update: Option<i64>,
    is_flagged_stable: Option<bool>,
    /// Wikipedia revision ID from `version.identifier`.
    revision_id: Option<i64>,
    /// Wikidata QID from `main_entity.identifier`.
    wikidata_qid: Option<String>,
}

struct PerArticleSignalsArray {
    citation_needed: Option<arrow_array::PrimitiveArray<Int64Type>>,
    pov: Option<arrow_array::PrimitiveArray<Int64Type>>,
    clarification: Option<arrow_array::PrimitiveArray<Int64Type>>,
    update: Option<arrow_array::PrimitiveArray<Int64Type>>,
    is_flagged_stable: Option<arrow_array::BooleanArray>,
    revision_id: Option<arrow_array::PrimitiveArray<Int64Type>>,
    /// Pre-extracted per-row strings (handles both Utf8 and LargeUtf8).
    wikidata_qid: Vec<Option<String>>,
}

impl PerArticleSignalsArray {
    fn row(&self, i: usize) -> PerArticleSignals {
        PerArticleSignals {
            citation_needed: self
                .citation_needed
                .as_ref()
                .filter(|a| !a.is_null(i))
                .map(|a| a.value(i)),
            pov: self
                .pov
                .as_ref()
                .filter(|a| !a.is_null(i))
                .map(|a| a.value(i)),
            clarification: self
                .clarification
                .as_ref()
                .filter(|a| !a.is_null(i))
                .map(|a| a.value(i)),
            update: self
                .update
                .as_ref()
                .filter(|a| !a.is_null(i))
                .map(|a| a.value(i)),
            is_flagged_stable: self
                .is_flagged_stable
                .as_ref()
                .filter(|a| !a.is_null(i))
                .map(|a| a.value(i)),
            revision_id: self
                .revision_id
                .as_ref()
                .filter(|a| !a.is_null(i))
                .map(|a| a.value(i)),
            wikidata_qid: self.wikidata_qid.get(i).and_then(|v| v.clone()),
        }
    }
}

fn extract_article_signals(
    batch: &arrow::array::RecordBatch,
) -> Option<PerArticleSignalsArray> {
    let version_col = batch.column_by_name("version")?;
    let version = try_as_struct_ref(version_col)?;

    let is_flagged_stable = version
        .column_by_name("is_flagged_stable")
        .and_then(|c| {
            c.as_any()
                .downcast_ref::<arrow_array::BooleanArray>()
                .cloned()
        });

    // Revision ID lives at version.identifier (Int64).
    let revision_id = version
        .column_by_name("identifier")
        .and_then(|c| c.as_any().downcast_ref::<arrow_array::Int64Array>().cloned());

    let mt_col = version.column_by_name("maintenance_tags")?;
    let mt = try_as_struct_ref(mt_col)?;

    let get_i64 = |name: &str| -> Option<arrow_array::PrimitiveArray<Int64Type>> {
        mt.column_by_name(name)
            .and_then(|c| c.as_any().downcast_ref::<arrow_array::Int64Array>().cloned())
    };

    // Wikidata QID lives at main_entity.identifier (Utf8 or LargeUtf8).
    let num_rows = batch.num_rows();
    let wikidata_qid: Vec<Option<String>> = batch
        .column_by_name("main_entity")
        .and_then(|c| try_as_struct_ref(c))
        .and_then(|s| s.column_by_name("identifier").cloned())
        .map(|col| {
            (0..num_rows)
                .map(|i| {
                    if col.is_null(i) {
                        return None;
                    }
                    col.as_any()
                        .downcast_ref::<arrow_array::StringArray>()
                        .map(|a| a.value(i).to_string())
                        .or_else(|| {
                            col.as_any()
                                .downcast_ref::<arrow_array::LargeStringArray>()
                                .map(|a| a.value(i).to_string())
                        })
                })
                .collect()
        })
        .unwrap_or_else(|| vec![None; num_rows]);

    Some(PerArticleSignalsArray {
        citation_needed: get_i64("citation_needed_count"),
        pov: get_i64("pov_count"),
        clarification: get_i64("clarification_needed_count"),
        update: get_i64("update_count"),
        is_flagged_stable,
        revision_id,
        wikidata_qid,
    })
}

// ─── Section extraction ──────────────────────────────────

/// Attempt to treat an array as either ListArray (i32 offsets) or
/// LargeListArray (i64 offsets), returning a unified trait object.
enum ListAccess<'a> {
    Small(&'a ListArray),
    Large(&'a LargeListArray),
}

impl<'a> ListAccess<'a> {
    fn try_new(col: &'a dyn Array) -> Option<Self> {
        if let Some(l) = col.as_any().downcast_ref::<ListArray>() {
            return Some(ListAccess::Small(l));
        }
        if let Some(l) = col.as_any().downcast_ref::<LargeListArray>() {
            return Some(ListAccess::Large(l));
        }
        None
    }

    fn is_null(&self, i: usize) -> bool {
        match self {
            ListAccess::Small(l) => l.is_null(i),
            ListAccess::Large(l) => l.is_null(i),
        }
    }

    fn offsets(&self, i: usize) -> (usize, usize) {
        match self {
            ListAccess::Small(l) => {
                let o = l.offsets();
                (o[i] as usize, o[i + 1] as usize)
            }
            ListAccess::Large(l) => {
                let o = l.offsets();
                (o[i] as usize, o[i + 1] as usize)
            }
        }
    }

    fn values(&self) -> &dyn Array {
        match self {
            ListAccess::Small(l) => l.values().as_ref(),
            ListAccess::Large(l) => l.values().as_ref(),
        }
    }
}

/// Recursively extract sections from a List column at a specific row.
fn extract_sections_from_list(
    sections_col: &dyn Array,
    row: usize,
    article_title: &str,
    article_url: &str,
    signals: &PerArticleSignals,
    page_id: Option<i64>,
    controversy_patterns: &[String],
    factual_patterns: &[String],
    parent_path: &[String],
    depth: u32,
    out: &mut Vec<ExtractedDoc>,
) {
    if depth > MAX_SECTION_DEPTH {
        return;
    }

    let list = match ListAccess::try_new(sections_col) {
        Some(l) => l,
        None => return,
    };

    if list.is_null(row) {
        return;
    }

    let (start, end) = list.offsets(row);
    if start >= end {
        return;
    }

    let values = list.values();
    let sections = match try_as_struct(values) {
        Some(s) => s,
        None => return,
    };

    extract_sections_range(
        sections,
        start,
        end,
        article_title,
        article_url,
        signals,
        page_id,
        controversy_patterns,
        factual_patterns,
        parent_path,
        depth,
        out,
    );
}

fn extract_sections_range(
    sections: &StructArray,
    start: usize,
    end: usize,
    article_title: &str,
    article_url: &str,
    signals: &PerArticleSignals,
    page_id: Option<i64>,
    controversy_patterns: &[String],
    factual_patterns: &[String],
    parent_path: &[String],
    depth: u32,
    out: &mut Vec<ExtractedDoc>,
) {
    for idx in start..end {
        let section_name = struct_get_string(sections, "name", idx)
            .unwrap_or_else(|| "Unnamed".to_string());

        if should_skip_section(&section_name) {
            continue;
        }

        let section_type = classify_section(&section_name, controversy_patterns, factual_patterns);

        let mut path: Vec<String> = parent_path.to_vec();
        path.push(section_name.clone());

        // Extract inter-article links for this section.
        let outgoing_links = extract_links(sections, idx);

        // Section content.
        if let Some(value) = struct_get_string(sections, "value", idx) {
            let text = value.trim().to_string();
            if text.len() >= MIN_SECTION_TEXT {
                let section_url = if section_name != "Unnamed" {
                    format!("{}#{}", article_url, urlify(&section_name))
                } else {
                    article_url.to_string()
                };

                let meta = WikipediaChunkMetadata {
                    section_name: section_name.clone(),
                    section_path: path.clone(),
                    section_depth: depth,
                    section_type: section_type.clone(),
                    citation_needed_count: signals.citation_needed,
                    pov_count: signals.pov,
                    clarification_needed_count: signals.clarification,
                    update_count: signals.update,
                    is_flagged_stable: signals.is_flagged_stable,
                    outgoing_links: outgoing_links.clone(),
                    revision_id: signals.revision_id,
                    wikidata_qid: signals.wikidata_qid.clone(),
                    page_id,
                };

                out.push(ExtractedDoc {
                    title: Some(article_title.to_string()),
                    content: text,
                    url: Some(section_url),
                    source_id: format!(
                        "{}-{}",
                        slug(article_title),
                        slug(&section_name)
                    ),
                    metadata: serde_json::to_value(&meta).ok(),
                });
            }
        }

        // Recurse into has_parts.
        if let Some(has_parts_col) = sections.column_by_name("has_parts") {
            extract_sections_from_list(
                has_parts_col.as_ref(),
                idx,
                article_title,
                article_url,
                signals,
                page_id,
                controversy_patterns,
                factual_patterns,
                &path,
                depth + 1,
                out,
            );
        }
    }
}

// ─── Link extraction ─────────────────────────────────────

fn extract_links(sections: &StructArray, section_idx: usize) -> Vec<WikiLink> {
    let links_col = match sections.column_by_name("links") {
        Some(c) => c,
        None => return vec![],
    };

    let list = match ListAccess::try_new(links_col.as_ref()) {
        Some(l) => l,
        None => return vec![],
    };

    if list.is_null(section_idx) {
        return vec![];
    }

    let (start, end) = list.offsets(section_idx);
    if start >= end {
        return vec![];
    }

    let values = list.values();
    let link_struct = match try_as_struct(values) {
        Some(s) => s,
        None => return vec![],
    };

    let mut links = Vec::new();
    for link_idx in start..end {
        let url = struct_get_string(link_struct, "url", link_idx).unwrap_or_default();
        let text = struct_get_string(link_struct, "text", link_idx).unwrap_or_default();

        // Only keep internal Wikipedia links.
        if !url.contains("/wiki/") {
            continue;
        }

        if let Some(target_title) = wiki_title_from_url(&url) {
            if !target_title.is_empty() {
                links.push(WikiLink {
                    target_title,
                    link_text: text,
                });
            }
        }
    }
    links
}

// ─── Classification ──────────────────────────────────────

pub fn should_skip_section(name: &str) -> bool {
    let lower = name.to_lowercase();
    SKIP_SECTIONS.iter().any(|s| lower == *s)
}

pub fn classify_section(
    name: &str,
    controversy_patterns: &[String],
    factual_patterns: &[String],
) -> String {
    let lower = name.to_lowercase();

    for pattern in controversy_patterns {
        if lower.contains(pattern.as_str()) {
            return "controversy".to_string();
        }
    }

    for pattern in factual_patterns {
        if lower.contains(pattern.as_str()) {
            return "factual".to_string();
        }
    }

    "general".to_string()
}

// ─── Arrow helpers ────────────────────────────────────────

fn try_as_struct(col: &dyn Array) -> Option<&StructArray> {
    col.as_any().downcast_ref::<StructArray>()
}

fn try_as_struct_ref(col: &ArrayRef) -> Option<&StructArray> {
    col.as_any().downcast_ref::<StructArray>()
}

fn struct_get_string(arr: &StructArray, name: &str, row: usize) -> Option<String> {
    let col_ref = arr.column_by_name(name)?;
    let col: &dyn Array = col_ref.as_ref();
    if col.is_null(row) {
        return None;
    }
    // Handle both Utf8 (i32) and LargeUtf8 (i64)
    if let Some(s) = col.as_any().downcast_ref::<arrow_array::StringArray>() {
        return Some(s.value(row).to_string());
    }
    if let Some(s) = col.as_any().downcast_ref::<arrow_array::LargeStringArray>() {
        return Some(s.value(row).to_string());
    }
    None
}

fn get_string_opt(col: Option<&ArrayRef>, row: usize) -> Option<String> {
    let col: &dyn Array = col?.as_ref();
    if col.is_null(row) {
        return None;
    }
    if let Some(s) = col.as_any().downcast_ref::<arrow_array::StringArray>() {
        return Some(s.value(row).to_string());
    }
    if let Some(s) = col.as_any().downcast_ref::<arrow_array::LargeStringArray>() {
        return Some(s.value(row).to_string());
    }
    None
}

fn get_i64_opt(col: Option<&ArrayRef>, row: usize) -> Option<i64> {
    let col: &dyn Array = col?.as_ref();
    if col.is_null(row) {
        return None;
    }
    col.as_any()
        .downcast_ref::<arrow_array::Int64Array>()
        .map(|a| a.value(row))
}

/// Convert a section name to a URL fragment.
fn urlify(name: &str) -> String {
    name.replace(' ', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_navigation_sections() {
        assert!(should_skip_section("References"));
        assert!(should_skip_section("external links"));
        assert!(should_skip_section("See Also"));
        assert!(!should_skip_section("Criticism"));
        assert!(!should_skip_section("History"));
    }

    #[test]
    fn classify_controversy() {
        let controversy: Vec<String> = DEFAULT_CONTROVERSY_PATTERNS
            .iter()
            .map(|s| s.to_string())
            .collect();
        let factual: Vec<String> = DEFAULT_FACTUAL_PATTERNS
            .iter()
            .map(|s| s.to_string())
            .collect();

        assert_eq!(
            classify_section("Criticism", &controversy, &factual),
            "controversy"
        );
        assert_eq!(
            classify_section("Controversies and disputes", &controversy, &factual),
            "controversy"
        );
        assert_eq!(
            classify_section("Early life", &controversy, &factual),
            "factual"
        );
        assert_eq!(
            classify_section("History", &controversy, &factual),
            "general"
        );
    }

    #[test]
    fn urlify_replaces_spaces() {
        assert_eq!(urlify("Elinor Ostrom"), "Elinor_Ostrom");
        assert_eq!(urlify("See also"), "See_also");
    }
}
