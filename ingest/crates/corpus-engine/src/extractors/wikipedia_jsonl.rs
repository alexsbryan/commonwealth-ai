// SPDX-License-Identifier: AGPL-3.0-or-later
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

use super::wikipedia_structured::{
    classify_section, should_skip_section, MAX_SECTION_DEPTH, MIN_SECTION_TEXT,
};
use super::wikipedia_types::{wiki_title_from_url, WikiLink, WikipediaChunkMetadata};
use super::{slug, ExtractedDoc, Extractor};
use crate::error::{Error, Result};

/// Extractor for `wikimedia/structured-wikipedia` ZIP+JSONL files.
pub struct WikipediaJsonlExtractor {
    pub controversy_patterns: Vec<String>,
    pub factual_patterns: Vec<String>,
    /// When set by the collaborative ingestion planner, restricts processing
    /// to articles `[start, end)`. Skipping articles before `start` is done
    /// without JSON parsing — only the line is read and discarded — so the
    /// overhead is linear in `start` but CPU-cheap (no deserialization).
    pub article_range: Option<(u64, u64)>,
    /// When set, restrict processing to a specific set of ZIP shard indices.
    /// Used by the collaborative-ingestion planner to hand each peer a disjoint
    /// set of shard indices. Each emitted document is tagged with
    /// `source_file = Some("shard:<index>")` so the ingest loop can record
    /// `processed_shards` at shard boundaries and peers can resume mid-partition.
    ///
    /// Takes precedence over the legacy "concatenate every JSONL entry into
    /// one `extracted.jsonl`" path — the cache file is neither read nor
    /// written when this field is set.
    pub shard_indices: Option<Vec<usize>>,
}

impl Default for WikipediaJsonlExtractor {
    fn default() -> Self {
        use super::wikipedia_structured::{DEFAULT_CONTROVERSY_PATTERNS, DEFAULT_FACTUAL_PATTERNS};
        Self {
            controversy_patterns: DEFAULT_CONTROVERSY_PATTERNS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            factual_patterns: DEFAULT_FACTUAL_PATTERNS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            article_range: None,
            shard_indices: None,
        }
    }
}

/// Source-file prefix used to tag documents yielded by the sharded iterator.
/// The ingest loop parses this to maintain per-shard boundaries and record
/// `processed_shards` in `_corpus_meta.json` as each shard finishes flushing.
pub const SHARD_SOURCE_FILE_PREFIX: &str = "shard:";

/// Parse the shard index from a `source_file` tag produced by the sharded
/// extractor. Returns `None` when the tag does not follow the shard convention.
pub fn parse_shard_source_file(source_file: &str) -> Option<usize> {
    source_file
        .strip_prefix(SHARD_SOURCE_FILE_PREFIX)
        .and_then(|rest| rest.parse::<usize>().ok())
}

impl Extractor for WikipediaJsonlExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        // Sharded path: the collaborative-ingestion planner handed us a
        // specific set of zip-entry indices. Read only those entries
        // directly from the ZIP — skip the "concatenate every JSONL shard
        // into one big extracted.jsonl" cache entirely. This is the only
        // safe way to partition a multi-shard JSONL corpus across peers:
        // shard boundaries are fixed by the ZIP's TOC, so two peers with
        // the same ZIP will produce identical chunks for a given shard.
        if let Some(ref indices) = self.shard_indices {
            let zip_path = resolve_zip_path(source_path)?;
            return Ok(Box::new(WikipediaJsonlShardedZipIterator::new(
                zip_path,
                indices.clone(),
                self.controversy_patterns.clone(),
                self.factual_patterns.clone(),
            )?));
        }

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
            article_range: self.article_range,
            // Cumulative article offset across shards — starts at 0 and
            // advances by each shard's article count as shards are opened.
            article_offset: 0,
        }))
    }
}

/// When sharded ingestion points us at a `source_path`, normalise it to the
/// concrete ZIP file. The collaborative flow always passes the downloaded ZIP
/// directly; an enclosing directory is accepted for parity with the
/// non-sharded `collect_zip_paths` helper and returns the first `.zip` found.
fn resolve_zip_path(source_path: &Path) -> Result<PathBuf> {
    if source_path.is_file() {
        return Ok(source_path.to_path_buf());
    }
    let mut zips = collect_zip_paths(source_path)?;
    if zips.is_empty() {
        return Err(Error::Extraction(format!(
            "No .zip found at: {}",
            source_path.display()
        )));
    }
    Ok(zips.remove(0))
}

fn collect_zip_paths(source_path: &Path) -> Result<Vec<PathBuf>> {
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
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("zip"))
        .collect();
    paths.sort();
    Ok(paths)
}

// ─── Sharded (collaborative-ingestion) iterator ─────────────
//
// Streams a caller-specified set of ZIP entries without materialising
// the merged `extracted.jsonl` cache. Each article yielded is tagged
// with `source_file = Some("shard:<index>")` so the ingest loop sees a
// source-file transition at every shard boundary and can call
// `record_processed_shard` on flush.
//
// Why a temp file per shard rather than borrowing the ZipArchive
// directly: `zip::ZipArchive::by_index` returns a `ZipFile<'_>` tied
// to the archive's lifetime, which is awkward to embed in an iterator
// that yields `ExtractedDoc`. Writing each shard to a temp file, then
// reading lines from it, keeps the peak disk footprint at ~one shard
// (Wikipedia shards are ~170 MB uncompressed). The temp file is
// deleted as soon as the shard is exhausted.

struct WikipediaJsonlShardedZipIterator {
    zip_path: PathBuf,
    /// **Logical** shard indices this peer is responsible for, in the
    /// dense 0..N space over the canonical filtered list (see
    /// `crate::engine::canonical_jsonl_shard_entries`). Two peers with
    /// the same ZIP derive the same canonical list, so disjoint
    /// logical sets produce disjoint article sets after merge even
    /// when the ZIP carries macOS junk entries (`__MACOSX/`, `._*`).
    ///
    /// These indices flow out on every `ExtractedDoc.source_file` as
    /// `"shard:<logical>"`, which the ingest loop persists via
    /// `CorpusIndex::record_processed_shard` — so the coordinator's
    /// `remaining = (0..shard_count) - processed_shards` arithmetic
    /// works in a single, consistent index space.
    assigned: VecDeque<usize>,
    /// Canonical logical → raw ZIP TOC mapping, built once at
    /// construction. Indexed by logical shard index.
    canonical: Vec<usize>,
    current: Option<ShardStreamState>,
    controversy_patterns: Vec<String>,
    factual_patterns: Vec<String>,
}

struct ShardStreamState {
    shard_index: usize,
    reader: Box<dyn BufRead + Send>,
    pending: VecDeque<ExtractedDoc>,
    temp_path: Option<PathBuf>,
}

impl Drop for ShardStreamState {
    fn drop(&mut self) {
        if let Some(ref p) = self.temp_path {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl WikipediaJsonlShardedZipIterator {
    /// `assigned` — **logical** shard indices (0..N) over the filtered
    /// canonical shard list, NOT raw ZIP TOC indices. The constructor
    /// translates each logical index to its physical ZIP entry index via
    /// [`crate::engine::canonical_jsonl_shard_entries`], ensuring two
    /// peers looking at the same ZIP agree on what shard 0, shard 1, …
    /// point at — regardless of how macOS-authored junk entries
    /// (`__MACOSX/`, `._*`) are sprinkled through the raw TOC.
    fn new(
        zip_path: PathBuf,
        mut assigned: Vec<usize>,
        controversy_patterns: Vec<String>,
        factual_patterns: Vec<String>,
    ) -> Result<Self> {
        // Deterministic shard order regardless of input ordering —
        // both peers must visit shards in the same order so that
        // article indices are reproducible across machines.
        assigned.sort_unstable();
        assigned.dedup();

        // Build the canonical logical→raw mapping. Every "real" JSONL
        // entry in the ZIP gets a dense logical index here.
        let canonical = crate::engine::canonical_jsonl_shard_entries(&zip_path)?;

        // Validate every assigned logical index up front. An out-of-range
        // logical index is a coordinator bug worth surfacing immediately
        // rather than as a mid-pipeline EOF.
        for &logical in &assigned {
            if logical >= canonical.len() {
                return Err(Error::Extraction(format!(
                    "Assigned logical shard {logical} is out of range \
                     (ZIP has {} real JSONL shards after filtering \
                     __MACOSX/._* junk)",
                    canonical.len()
                )));
            }
        }

        eprintln!(
            "[corpus-engine] Sharded extraction — ZIP {} assigned logical {:?} of {} real shards",
            zip_path.display(),
            assigned,
            canonical.len(),
        );

        Ok(Self {
            zip_path,
            assigned: assigned.into(),
            canonical,
            current: None,
            controversy_patterns,
            factual_patterns,
        })
    }

    fn open_next_shard(&mut self) -> Option<Result<ShardStreamState>> {
        let logical_index = self.assigned.pop_front()?;
        // Logical → raw ZIP TOC. Validated in the constructor, so a
        // bounds failure here would be an internal invariant break,
        // not a caller error.
        let raw_index = match self.canonical.get(logical_index) {
            Some(&r) => r,
            None => {
                return Some(Err(Error::Extraction(format!(
                    "Internal: logical shard {logical_index} has no canonical mapping \
                     (canonical has {} entries)",
                    self.canonical.len()
                ))));
            }
        };
        let file = match File::open(&self.zip_path) {
            Ok(f) => f,
            Err(e) => {
                return Some(Err(Error::Extraction(format!(
                    "Failed to open {}: {e}",
                    self.zip_path.display()
                ))));
            }
        };
        let mut archive = match zip::ZipArchive::new(file) {
            Ok(a) => a,
            Err(e) => {
                return Some(Err(Error::Extraction(format!(
                    "Failed to read ZIP TOC at {}: {e}",
                    self.zip_path.display()
                ))));
            }
        };
        let mut entry = match archive.by_index(raw_index) {
            Ok(e) => e,
            Err(e) => {
                return Some(Err(Error::Extraction(format!(
                    "Failed to open ZIP entry {raw_index} (logical shard {logical_index}): {e}"
                ))));
            }
        };
        let entry_name = entry.name().to_string();

        let mut tmp = match tempfile::NamedTempFile::new() {
            Ok(t) => t,
            Err(e) => {
                return Some(Err(Error::Extraction(format!(
                    "Failed to create temp file for shard {logical_index}: {e}"
                ))));
            }
        };
        if let Err(e) = std::io::copy(&mut entry, &mut tmp) {
            return Some(Err(Error::Extraction(format!(
                "Failed to extract shard {logical_index} ({entry_name}): {e}"
            ))));
        }
        let (file, temp_path) = match tmp.keep() {
            Ok((f, p)) => (f, p),
            Err(e) => {
                return Some(Err(Error::Extraction(format!(
                    "Failed to persist shard {logical_index} temp file: {e}"
                ))));
            }
        };
        // Reopen as read-only — `keep()` hands back a writable handle.
        let _ = file;
        let read_file = match File::open(&temp_path) {
            Ok(f) => f,
            Err(e) => {
                let _ = std::fs::remove_file(&temp_path);
                return Some(Err(Error::Extraction(format!(
                    "Failed to reopen shard {logical_index} temp file: {e}"
                ))));
            }
        };
        let reader: Box<dyn BufRead + Send> =
            Box::new(BufReader::with_capacity(256 * 1024, read_file));
        eprintln!(
            "[corpus-engine] Sharded extraction → logical shard {logical_index} \
             (raw ZIP index {raw_index}, {entry_name})"
        );
        Some(Ok(ShardStreamState {
            shard_index: logical_index,
            reader,
            pending: VecDeque::new(),
            temp_path: Some(temp_path),
        }))
    }
}

impl Iterator for WikipediaJsonlShardedZipIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.current.is_none() {
                match self.open_next_shard()? {
                    Ok(state) => self.current = Some(state),
                    Err(e) => return Some(Err(e)),
                }
            }
            let state = self.current.as_mut().expect("current set above");

            if let Some(doc) = state.pending.pop_front() {
                return Some(Ok(doc));
            }

            let mut line_bytes = Vec::new();
            match state.reader.read_until(b'\n', &mut line_bytes) {
                Ok(0) => {
                    // Shard exhausted — drop state (temp file removed
                    // on Drop) and advance to the next assigned shard.
                    self.current = None;
                    continue;
                }
                Ok(_) => {}
                Err(e) => {
                    return Some(Err(Error::Extraction(format!(
                        "JSONL read error in shard {}: {e}",
                        state.shard_index
                    ))));
                }
            }

            let line_cow = String::from_utf8_lossy(&line_bytes);
            let line = line_cow.trim();
            if line.is_empty() {
                continue;
            }

            let shard_tag = format!("{SHARD_SOURCE_FILE_PREFIX}{}", state.shard_index);
            match process_article_line(line, &self.controversy_patterns, &self.factual_patterns) {
                Ok(docs) => {
                    for mut doc in docs {
                        doc.source_file = Some(shard_tag.clone());
                        state.pending.push_back(doc);
                    }
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

// ─── Shard-chaining iterator ────────────────────────────────

struct WikipediaJsonlShardIterator {
    paths: VecDeque<PathBuf>,
    current: Option<WikipediaJsonlLineIterator>,
    controversy_patterns: Vec<String>,
    factual_patterns: Vec<String>,
    article_range: Option<(u64, u64)>,
    /// Running count of articles yielded by completed shards.
    /// Passed as `article_offset` to each new shard so article indices
    /// are globally consistent across multi-shard ZIPs.
    article_offset: u64,
}

impl Iterator for WikipediaJsonlShardIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(ref mut iter) = self.current {
                if let Some(item) = iter.next() {
                    return Some(item);
                }
                // Shard exhausted — advance the global article offset.
                self.article_offset += iter.article_index;
                self.current = None;
            }

            // If the range end is already past, no need to open more shards.
            if let Some((_, end)) = self.article_range {
                if self.article_offset >= end {
                    return None;
                }
            }

            let path = self.paths.pop_front()?;
            match WikipediaJsonlLineIterator::open(
                &path,
                self.controversy_patterns.clone(),
                self.factual_patterns.clone(),
                self.article_range,
                self.article_offset,
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
    /// Global article index (across all shards). Counts lines read, not
    /// sections yielded. Exported so the shard iterator can advance its
    /// cumulative offset when a shard is exhausted.
    pub article_index: u64,
    /// Global article range `[start, end)` from the planner. Lines before
    /// `start` are skipped without JSON parsing; iteration stops at `end`.
    article_range: Option<(u64, u64)>,
}

impl WikipediaJsonlLineIterator {
    fn open(
        path: &Path,
        controversy_patterns: Vec<String>,
        factual_patterns: Vec<String>,
        article_range: Option<(u64, u64)>,
        article_offset: u64,
    ) -> Result<Self> {
        let file = File::open(path)
            .map_err(|e| Error::Extraction(format!("Failed to open {}: {e}", path.display())))?;

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
            article_index: article_offset,
            article_range,
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

            // Use read_until instead of read_line so isolated non-UTF-8 bytes
            // (e.g. Latin-1 encoded club names from older Wikipedia markup) don't
            // abort the entire article. from_utf8_lossy replaces bad bytes with
            // U+FFFD — a few replacement chars in a footballer's name is acceptable.
            let mut line_bytes = Vec::new();
            match self.inner.read_until(b'\n', &mut line_bytes) {
                Ok(0) => return None, // EOF
                Ok(_) => {}
                Err(e) => return Some(Err(Error::Extraction(format!("JSONL read error: {e}")))),
            }

            let line_cow = String::from_utf8_lossy(&line_bytes);
            // Cow::Owned means from_utf8_lossy made replacements — log so we
            // can gauge how widespread the encoding issues are in this corpus.
            if matches!(line_cow, std::borrow::Cow::Owned(_)) {
                tracing::debug!(
                    article_index = self.article_index,
                    "non-UTF-8 bytes replaced with U+FFFD"
                );
            }
            let line = line_cow.trim();
            if line.is_empty() {
                continue;
            }

            let idx = self.article_index;
            self.article_index += 1;

            if let Some((start, end)) = self.article_range {
                if idx < start {
                    // Skip without JSON parsing — cheap line discard.
                    continue;
                }
                if idx >= end {
                    return None;
                }
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
                "ZIP archive {} is empty",
                zip_path.display()
            )));
        }

        // Collect all JSONL entries. If none have the .jsonl extension, fall back to index 0.
        // The wikimedia/structured-wikipedia ZIP contains multiple JSONL shards; we must
        // process ALL of them. Previously only the first was read, causing 99%+ data loss.
        let jsonl_indices: Vec<usize> = (0..n)
            .filter(|&i| {
                archive
                    .name_for_index(i)
                    .map(|name| name.ends_with(".jsonl"))
                    .unwrap_or(false)
            })
            .collect();
        let indices = if jsonl_indices.is_empty() {
            vec![0]
        } else {
            jsonl_indices
        };

        eprintln!(
            "[corpus-engine] ZIP {} has {} total entries, {} JSONL shards — extracting all",
            zip_path.display(),
            n,
            indices.len()
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
            let file = File::open(&cache_path)
                .map_err(|e| Error::Extraction(format!("Failed to open cached extraction: {e}")))?;
            return Ok(Self {
                inner: BufReader::new(file),
            });
        }

        eprintln!(
            "[corpus-engine] Extracting {} JSONL shards (this is slow; cached for future runs)",
            indices.len()
        );
        let mut tmp = tempfile::NamedTempFile::new()
            .map_err(|e| Error::Extraction(format!("Failed to create temp file: {e}")))?;
        for (pos, entry_index) in indices.iter().enumerate() {
            let name = archive
                .name_for_index(*entry_index)
                .unwrap_or("?")
                .to_string();
            eprintln!(
                "[corpus-engine] Extracting shard {}/{}: {}",
                pos + 1,
                indices.len(),
                name
            );
            let mut zip_entry = archive.by_index(*entry_index).map_err(|e| {
                Error::Extraction(format!(
                    "Failed to open ZIP entry {} in {}: {e}",
                    entry_index,
                    zip_path.display()
                ))
            })?;
            std::io::copy(&mut zip_entry, &mut tmp).map_err(|e| {
                Error::Extraction(format!("Failed to extract ZIP entry {entry_index}: {e}"))
            })?;
        }

        // Move temp file to the deterministic cache path.
        let (_, tmp_path) = tmp
            .keep()
            .map_err(|e| Error::Extraction(format!("Failed to persist temp file: {e}")))?;
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

        let file = File::open(&cache_path)
            .map_err(|e| Error::Extraction(format!("Failed to open cached extraction: {e}")))?;

        Ok(Self {
            inner: BufReader::new(file),
        })
    }
}

impl BufRead for ZipEntryReader {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.inner.fill_buf()
    }
    fn consume(&mut self, amt: usize) {
        self.inner.consume(amt)
    }
}

impl std::io::Read for ZipEntryReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
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
                source_file: None,
                embed_text: None,
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
                                    link_text: link["text"].as_str().unwrap_or("").to_string(),
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
                source_file: None,
                embed_text: None,
            });
        }

        // Recurse into subsections within has_parts.
        let subsections: Vec<&Value> = has_parts
            .iter()
            .filter(|p| p["type"].as_str() == Some("section"))
            .collect();
        if !subsections.is_empty() {
            // Collect into a Vec<Value> slice for the recursive call.
            let sub_values: Vec<Value> = subsections.into_iter().cloned().collect();
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

    fn article_json(name: &str, abstract_text: &str, sections: serde_json::Value) -> String {
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
