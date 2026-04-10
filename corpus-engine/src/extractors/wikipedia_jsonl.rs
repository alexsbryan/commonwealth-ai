//! Extractor for the `wikimedia/structured-wikipedia` HuggingFace dataset,
//! which ships as a ZIP archive containing a JSONL file.
//!
//! The ZIP entry format is one JSON object per line, each representing a
//! full Wikipedia article. The schema mirrors the parquet schema documented
//! in `wikipedia_structured.rs` but uses nested JSON rather than Arrow arrays.
//!
//! Key article-level fields:
//!   name: String               — article title
//!   identifier: i64            — Wikipedia page ID
//!   abstract: String           — lead paragraph
//!   description: String        — short Wikidata description
//!   url: String                — full Wikipedia URL
//!   version.identifier: i64    — revision ID (used as delta key)
//!   main_entity.identifier: String — Wikidata QID (e.g. "Q42")
//!   sections: Array<Section>
//!
//! Section schema:
//!   name: String
//!   type: String               — always "section" at top level
//!   has_parts: Array           — mixed: paragraphs and subsections
//!     { type: "paragraph", value: String, links: Array<Link> }
//!     { type: "section",   name: String,  has_parts: Array<...> }
//!
//! Unlike the parquet format, sections do NOT carry a top-level `value`
//! field. Text content lives in `has_parts` items with `type == "paragraph"`.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::wikipedia_types::{WikiLink, WikipediaChunkMetadata, wiki_title_from_url};
use crate::error::{Error, Result};
use super::{ExtractedDoc, Extractor, slug};
use super::wikipedia_structured::{
    MAX_SECTION_DEPTH, MIN_SECTION_TEXT,
    should_skip_section, classify_section,
};

/// Extractor for `wikimedia/structured-wikipedia` ZIP+JSONL files.
pub struct WikipediaJsonlExtractor {
    pub controversy_patterns: Vec<String>,
    pub factual_patterns: Vec<String>,
}

impl Default for WikipediaJsonlExtractor {
    fn default() -> Self {
        use super::wikipedia_structured::{
            DEFAULT_CONTROVERSY_PATTERNS, DEFAULT_FACTUAL_PATTERNS,
        };
        Self {
            controversy_patterns: DEFAULT_CONTROVERSY_PATTERNS
                .iter().map(|s| s.to_string()).collect(),
            factual_patterns: DEFAULT_FACTUAL_PATTERNS
                .iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl Extractor for WikipediaJsonlExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let paths = collect_zip_paths(source_path)?;
        if paths.is_empty() {
            return Err(Error::Extraction(format!(
                "No .zip files found at: {}",
                source_path.display()
            )));
        }

        Ok(Box::new(WikipediaJsonlShardIterator {
            paths: paths.into(),
            current: None,
            controversy_patterns: self.controversy_patterns.clone(),
            factual_patterns: self.factual_patterns.clone(),
        }))
    }
}

fn collect_zip_paths(source_path: &Path) -> Result<Vec<PathBuf>> {
    if source_path.is_file() {
        return Ok(vec![source_path.to_path_buf()]);
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(source_path)
        .map_err(|e| Error::Extraction(format!(
            "Failed to read directory {}: {e}", source_path.display()
        )))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("zip"))
        .collect();
    paths.sort();
    Ok(paths)
}

// ─── Shard-chaining iterator ────────────────────────────────

struct WikipediaJsonlShardIterator {
    paths: VecDeque<PathBuf>,
    current: Option<WikipediaJsonlLineIterator>,
    controversy_patterns: Vec<String>,
    factual_patterns: Vec<String>,
}

impl Iterator for WikipediaJsonlShardIterator {
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
            match WikipediaJsonlLineIterator::open(
                &path,
                self.controversy_patterns.clone(),
                self.factual_patterns.clone(),
            ) {
                Ok(iter) => self.current = Some(iter),
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

// ─── Single ZIP line iterator ────────────────────────────────

struct WikipediaJsonlLineIterator {
    // Boxed to avoid lifetime issues: holds a BufReader over the ZIP entry.
    // The ZipArchive must live as long as the reader, so we box both together.
    inner: Box<dyn BufRead + Send>,
    pending: VecDeque<ExtractedDoc>,
    controversy_patterns: Vec<String>,
    factual_patterns: Vec<String>,
}

impl WikipediaJsonlLineIterator {
    fn open(
        path: &Path,
        controversy_patterns: Vec<String>,
        factual_patterns: Vec<String>,
    ) -> Result<Self> {
        let file = File::open(path).map_err(|e| {
            Error::Extraction(format!("Failed to open {}: {e}", path.display()))
        })?;

        // Detect ZIP vs raw JSONL by extension.
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let inner: Box<dyn BufRead + Send> = if ext == "zip" {
            // ZipArchive::new requires Seek, so we can't stream directly.
            // We hand off ownership of the file to a helper that extracts
            // the first JSONL entry into a temp file for line-by-line reading.
            // For a 13 GB ZIP we cannot hold the whole thing in memory, so we
            // use zip::read::ZipFile which decompresses the entry on the fly.
            let archive = zip::ZipArchive::new(file).map_err(|e| {
                Error::Extraction(format!("Failed to open ZIP {}: {e}", path.display()))
            })?;
            Box::new(ZipEntryReader::new(archive, path)?)
        } else if ext == "gz" {
            Box::new(BufReader::new(flate2::read::GzDecoder::new(file)))
        } else {
            Box::new(BufReader::new(file))
        };

        Ok(Self {
            inner,
            pending: VecDeque::new(),
            controversy_patterns,
            factual_patterns,
        })
    }
}

impl Iterator for WikipediaJsonlLineIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(doc) = self.pending.pop_front() {
                return Some(Ok(doc));
            }

            let mut line = String::new();
            match self.inner.read_line(&mut line) {
                Ok(0) => return None, // EOF
                Ok(_) => {}
                Err(e) => return Some(Err(Error::Extraction(format!("JSONL read error: {e}")))),
            }

            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            match process_article_line(line, &self.controversy_patterns, &self.factual_patterns) {
                Ok(docs) => self.pending.extend(docs),
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

// ─── ZIP entry streaming ─────────────────────────────────────

/// Wraps a ZIP entry as a `BufRead + Send` by reading it into a temp file
/// and streaming from there. This avoids holding the decompressed bytes
/// in memory while still supporting efficient line-by-line iteration.
///
/// We use a temp file because `zip::read::ZipFile<'_>` borrows the
/// `ZipArchive`, making it hard to return as a standalone `BufRead`.
struct ZipEntryReader {
    inner: BufReader<File>,
}

impl ZipEntryReader {
    fn new(mut archive: zip::ZipArchive<File>, zip_path: &Path) -> Result<Self> {
        let n = archive.len();
        if n == 0 {
            return Err(Error::Extraction(format!(
                "ZIP archive {} is empty", zip_path.display()
            )));
        }

        // Collect all JSONL entries. If none have the .jsonl extension, fall back to index 0.
        // The wikimedia/structured-wikipedia ZIP contains multiple JSONL shards; we must
        // process ALL of them. Previously only the first was read, causing 99%+ data loss.
        let jsonl_indices: Vec<usize> = (0..n)
            .filter(|&i| {
                archive.name_for_index(i)
                    .map(|name| name.ends_with(".jsonl"))
                    .unwrap_or(false)
            })
            .collect();
        let indices = if jsonl_indices.is_empty() { vec![0] } else { jsonl_indices };

        eprintln!(
            "[corpus-engine] ZIP {} has {} total entries, {} JSONL shards — extracting all",
            zip_path.display(), n, indices.len()
        );

        // Concatenate all JSONL shards into a single file. Each shard is
        // newline-delimited JSON, so concatenation is valid: each line is a
        // complete JSON object and shards end with a newline.
        //
        // Use a deterministic path next to the ZIP so resume runs can skip
        // the 30+ minute extraction step entirely. The marker file name is
        // derived from the ZIP file name.
        let cache_path = zip_path.with_extension("extracted.jsonl");
        if cache_path.exists() && cache_path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            eprintln!(
                "[corpus-engine] Reusing cached extraction: {} ({:.1} GB)",
                cache_path.display(),
                cache_path.metadata().map(|m| m.len()).unwrap_or(0) as f64 / 1_073_741_824.0,
            );
            let file = File::open(&cache_path).map_err(|e| {
                Error::Extraction(format!("Failed to open cached extraction: {e}"))
            })?;
            return Ok(Self { inner: BufReader::new(file) });
        }

        eprintln!(
            "[corpus-engine] Extracting {} JSONL shards (this is slow; cached for future runs)",
            indices.len()
        );
        let mut tmp = tempfile::NamedTempFile::new().map_err(|e| {
            Error::Extraction(format!("Failed to create temp file: {e}"))
        })?;
        for (pos, entry_index) in indices.iter().enumerate() {
            let name = archive.name_for_index(*entry_index).unwrap_or("?").to_string();
            eprintln!(
                "[corpus-engine] Extracting shard {}/{}: {}",
                pos + 1, indices.len(), name
            );
            let mut zip_entry = archive.by_index(*entry_index).map_err(|e| {
                Error::Extraction(format!(
                    "Failed to open ZIP entry {} in {}: {e}",
                    entry_index, zip_path.display()
                ))
            })?;
            std::io::copy(&mut zip_entry, &mut tmp).map_err(|e| {
                Error::Extraction(format!("Failed to extract ZIP entry {entry_index}: {e}"))
            })?;
        }

        // Move temp file to the deterministic cache path.
        let (_, tmp_path) = tmp.keep().map_err(|e| {
            Error::Extraction(format!("Failed to persist temp file: {e}"))
        })?;
        if let Err(e) = std::fs::rename(&tmp_path, &cache_path) {
            // rename fails across filesystems; fall back to copy+delete.
            if let Err(e2) = std::fs::copy(&tmp_path, &cache_path) {
                tracing::warn!("Failed to cache extracted JSONL: rename={e}, copy={e2}");
            } else {
                let _ = std::fs::remove_file(&tmp_path);
            }
        }
        eprintln!(
            "[corpus-engine] Extraction cached at: {}",
            cache_path.display()
        );

        let file = File::open(&cache_path).map_err(|e| {
            Error::Extraction(format!("Failed to open cached extraction: {e}"))
        })?;

        Ok(Self { inner: BufReader::new(file) })
    }
}

impl BufRead for ZipEntryReader {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> { self.inner.fill_buf() }
    fn consume(&mut self, amt: usize) { self.inner.consume(amt) }
}

impl std::io::Read for ZipEntryReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> { self.inner.read(buf) }
}

// Safety: ZipEntryReader wraps a BufReader<File> which is Send.
unsafe impl Send for ZipEntryReader {}

// ─── Article processing ───────────────────────────────────────

struct ArticleSignals {
    revision_id: Option<i64>,
    wikidata_qid: Option<String>,
    page_id: Option<i64>,
}

fn process_article_line(
    line: &str,
    controversy_patterns: &[String],
    factual_patterns: &[String],
) -> Result<Vec<ExtractedDoc>> {
    let article: Value = serde_json::from_str(line)
        .map_err(|e| Error::Extraction(format!("JSONL parse error: {e}")))?;

    let title = match article["name"].as_str().filter(|s| !s.is_empty()) {
        Some(t) => t.to_string(),
        None => return Ok(Vec::new()),
    };
    let url = article["url"].as_str().unwrap_or("").to_string();

    let signals = ArticleSignals {
        revision_id: article["version"]["identifier"].as_i64(),
        wikidata_qid: article["main_entity"]["identifier"]
            .as_str()
            .map(|s| s.to_string()),
        page_id: article["identifier"].as_i64(),
    };

    let mut docs = Vec::new();

    // ── Lead / abstract chunk ─────────────────────────────────
    if let Some(abstract_text) = article["abstract"].as_str() {
        let text = abstract_text.trim().to_string();
        if text.len() >= MIN_SECTION_TEXT {
            let meta = WikipediaChunkMetadata {
                section_name: "Lead".to_string(),
                section_path: vec![],
                section_depth: 0,
                section_type: "lead".to_string(),
                citation_needed_count: None,
                pov_count: None,
                clarification_needed_count: None,
                update_count: None,
                is_flagged_stable: None,
                outgoing_links: vec![],
                revision_id: signals.revision_id,
                wikidata_qid: signals.wikidata_qid.clone(),
                page_id: signals.page_id,
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

    // ── Sections ──────────────────────────────────────────────
    if let Some(sections) = article["sections"].as_array() {
        extract_sections_json(
            sections,
            &title,
            &url,
            &signals,
            controversy_patterns,
            factual_patterns,
            &[],
            0,
            &mut docs,
        );
    }

    Ok(docs)
}

fn extract_sections_json(
    sections: &[Value],
    article_title: &str,
    article_url: &str,
    signals: &ArticleSignals,
    controversy_patterns: &[String],
    factual_patterns: &[String],
    parent_path: &[String],
    depth: u32,
    out: &mut Vec<ExtractedDoc>,
) {
    if depth >= MAX_SECTION_DEPTH {
        return;
    }

    for section in sections {
        let name = section["name"].as_str().unwrap_or("Unnamed").to_string();

        if should_skip_section(&name) {
            continue;
        }

        let section_type = classify_section(&name, controversy_patterns, factual_patterns);

        let mut path = parent_path.to_vec();
        path.push(name.clone());

        let has_parts = match section["has_parts"].as_array() {
            Some(p) => p,
            None => {
                // No has_parts — nothing to emit or recurse into.
                continue;
            }
        };

        // Collect paragraph text and links from has_parts.
        let mut text_parts: Vec<&str> = Vec::new();
        let mut outgoing_links: Vec<WikiLink> = Vec::new();

        for part in has_parts.iter() {
            let part_type = part["type"].as_str().unwrap_or("");
            if part_type == "paragraph" {
                if let Some(v) = part["value"].as_str() {
                    if !v.trim().is_empty() {
                        text_parts.push(v.trim());
                    }
                }
                // Links from this paragraph.
                if let Some(links) = part["links"].as_array() {
                    for link in links {
                        let link_url = link["url"].as_str().unwrap_or("");
                        if !link_url.contains("/wiki/") {
                            continue;
                        }
                        if let Some(target_title) = wiki_title_from_url(link_url) {
                            if !target_title.is_empty() {
                                outgoing_links.push(WikiLink {
                                    target_title,
                                    link_text: link["text"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }

        let text = text_parts.join("\n\n");
        if text.len() >= MIN_SECTION_TEXT {
            let section_url = if name != "Unnamed" {
                format!("{}#{}", article_url, name.replace(' ', "_"))
            } else {
                article_url.to_string()
            };

            let meta = WikipediaChunkMetadata {
                section_name: name.clone(),
                section_path: path.clone(),
                section_depth: depth,
                section_type: section_type.clone(),
                citation_needed_count: None,
                pov_count: None,
                clarification_needed_count: None,
                update_count: None,
                is_flagged_stable: None,
                outgoing_links,
                revision_id: signals.revision_id,
                wikidata_qid: signals.wikidata_qid.clone(),
                page_id: signals.page_id,
            };

            out.push(ExtractedDoc {
                title: Some(article_title.to_string()),
                content: text,
                url: Some(section_url),
                source_id: format!("{}-{}", slug(article_title), slug(&name)),
                metadata: serde_json::to_value(&meta).ok(),
            });
        }

        // Recurse into subsections within has_parts.
        let subsections: Vec<&Value> = has_parts
            .iter()
            .filter(|p| p["type"].as_str() == Some("section"))
            .collect();
        if !subsections.is_empty() {
            // Collect into a Vec<Value> slice for the recursive call.
            let sub_values: Vec<Value> = subsections
                .into_iter()
                .cloned()
                .collect();
            extract_sections_json(
                &sub_values,
                article_title,
                article_url,
                signals,
                controversy_patterns,
                factual_patterns,
                &path,
                depth + 1,
                out,
            );
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn article_json(
        name: &str,
        abstract_text: &str,
        sections: serde_json::Value,
    ) -> String {
        serde_json::json!({
            "name": name,
            "identifier": 12345,
            "abstract": abstract_text,
            "description": "A test article",
            "url": format!("https://en.wikipedia.org/wiki/{}", name.replace(' ', "_")),
            "version": { "identifier": 987654321_i64 },
            "main_entity": { "identifier": "Q42" },
            "sections": sections,
        })
        .to_string()
    }

    #[test]
    fn process_line_extracts_lead_and_sections() {
        let sections = serde_json::json!([
            {
                "name": "History",
                "type": "section",
                "has_parts": [
                    {
                        "type": "paragraph",
                        "value": "This is a sufficiently long paragraph about history that exceeds the minimum length threshold for section text.",
                        "links": [
                            { "url": "https://en.wikipedia.org/wiki/Rome", "text": "Rome" }
                        ]
                    }
                ]
            }
        ]);
        let line = article_json(
            "Test Article",
            "This is a lead paragraph that is long enough to be included in the index.",
            sections,
        );

        let controversy: Vec<String> = vec![];
        let factual: Vec<String> = vec![];
        let docs = process_article_line(&line, &controversy, &factual).unwrap();

        // Lead + 1 section = 2 docs.
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].title.as_deref(), Some("Test Article"));
        assert_eq!(docs[0].source_id, "test-article-lead");

        // Section doc has correct source_id.
        assert_eq!(docs[1].source_id, "test-article-history");

        // Metadata carries revision_id and wikidata_qid.
        let meta: WikipediaChunkMetadata =
            serde_json::from_value(docs[0].metadata.clone().unwrap()).unwrap();
        assert_eq!(meta.revision_id, Some(987654321));
        assert_eq!(meta.wikidata_qid.as_deref(), Some("Q42"));
        assert_eq!(meta.page_id, Some(12345));
        assert_eq!(meta.section_type, "lead");

        // Section metadata has outgoing link.
        let sec_meta: WikipediaChunkMetadata =
            serde_json::from_value(docs[1].metadata.clone().unwrap()).unwrap();
        assert_eq!(sec_meta.outgoing_links.len(), 1);
        assert_eq!(sec_meta.outgoing_links[0].target_title, "Rome");
    }

    #[test]
    fn skips_short_sections() {
        let sections = serde_json::json!([
            {
                "name": "History",
                "type": "section",
                "has_parts": [
                    { "type": "paragraph", "value": "Too short.", "links": [] }
                ]
            }
        ]);
        let line = article_json(
            "Short Article",
            "Lead is long enough to pass the minimum text threshold for indexing.",
            sections,
        );
        let docs = process_article_line(&line, &[], &[]).unwrap();
        // Only lead — section text is below MIN_SECTION_TEXT.
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].source_id, "short-article-lead");
    }

    #[test]
    fn skips_navigation_sections() {
        let sections = serde_json::json!([
            {
                "name": "References",
                "type": "section",
                "has_parts": [
                    { "type": "paragraph", "value": "This text is long enough but should be skipped because it is in the References section which is navigational.", "links": [] }
                ]
            }
        ]);
        let line = article_json(
            "Nav Test",
            "Lead paragraph that is definitely long enough to meet the minimum section text threshold.",
            sections,
        );
        let docs = process_article_line(&line, &[], &[]).unwrap();
        assert_eq!(docs.len(), 1); // Only lead, References skipped.
    }

    #[test]
    fn revision_id_and_qid_propagate_to_sections() {
        let sections = serde_json::json!([
            {
                "name": "Background",
                "type": "section",
                "has_parts": [
                    { "type": "paragraph", "value": "Background text that is long enough to clear the minimum section text threshold for indexing.", "links": [] }
                ]
            }
        ]);
        let line = article_json(
            "Propagation Test",
            "Lead paragraph with enough text to clear the minimum threshold.",
            sections,
        );
        let docs = process_article_line(&line, &[], &[]).unwrap();
        assert_eq!(docs.len(), 2);
        for doc in &docs {
            let meta: WikipediaChunkMetadata =
                serde_json::from_value(doc.metadata.clone().unwrap()).unwrap();
            assert_eq!(meta.revision_id, Some(987654321));
            assert_eq!(meta.wikidata_qid.as_deref(), Some("Q42"));
        }
    }

    #[test]
    fn empty_title_skips_article() {
        let line = serde_json::json!({
            "name": "",
            "identifier": 1,
            "abstract": "Some text",
            "url": "https://en.wikipedia.org/wiki/",
            "sections": []
        })
        .to_string();
        let docs = process_article_line(&line, &[], &[]).unwrap();
        assert!(docs.is_empty());
    }
}
