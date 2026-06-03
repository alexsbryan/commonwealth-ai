//! Gutenberg catalog extractor.
//!
//! Parses Project Gutenberg's `pg_catalog.csv` (one row per work) into
//! one [`ExtractedDoc`] per work, paired with the `passthrough` chunker
//! to produce one chunk per work in the catalog index. The chunk text
//! is a compact, vector-friendly summary of the work's metadata —
//! title, author, era, subjects, language — *not* the full text.
//!
//! Pair with `[corpus] kind = "catalog"` and a `[catalog]` block whose
//! `download_url_template` points at the per-work full-text URL.
//! Catalog hits are surfaced to retrieval as
//! [`crate::types::CorpusKind::Catalog`] so the runtime can offer an
//! on-demand single-work ingest instead of confabulating from
//! parametric knowledge.
//!
//! ## Source format
//!
//! Project Gutenberg ships a single CSV at
//! `https://www.gutenberg.org/cache/epub/feeds/pg_catalog.csv.gz`
//! (gzipped) with the columns:
//!
//! `Text#`, `Type`, `Issued`, `Title`, `Language`, `Authors`, `Subjects`, `LoCC`, `Bookshelves`
//!
//! Rows with empty `Text#` or `Title` are skipped (degenerate
//! entries). `Type != "Text"` rows (audio, music, …) are also
//! skipped — the on-demand content recipe assumes a plain-text
//! download URL.

use std::fs::File;
use std::io::Read as _; // for File::read; trait object below uses fully-qualified path
use std::path::Path;

use serde_json::json;

use super::{ExtractedDoc, Extractor};
use crate::error::{Error, Result};

/// Column-name → index lookup for the PG catalog CSV. Built once
/// from the header row so the per-row hot path is O(1) per column.
struct ColumnIndex {
    text_id: usize,
    type_col: Option<usize>,
    issued: Option<usize>,
    title: usize,
    language: Option<usize>,
    authors: Option<usize>,
    subjects: Option<usize>,
    locc: Option<usize>,
    bookshelves: Option<usize>,
}

impl ColumnIndex {
    fn from_headers(headers: &csv::StringRecord) -> Result<Self> {
        let pos = |name: &str| headers.iter().position(|h| h == name);
        let text_id = pos("Text#").ok_or_else(|| {
            Error::Extraction(format!(
                "Gutenberg catalog: required column `Text#` missing. Columns present: {}",
                headers.iter().collect::<Vec<_>>().join(", "),
            ))
        })?;
        let title = pos("Title").ok_or_else(|| {
            Error::Extraction(format!(
                "Gutenberg catalog: required column `Title` missing. Columns present: {}",
                headers.iter().collect::<Vec<_>>().join(", "),
            ))
        })?;
        Ok(Self {
            text_id,
            type_col: pos("Type"),
            issued: pos("Issued"),
            title,
            language: pos("Language"),
            authors: pos("Authors"),
            subjects: pos("Subjects"),
            locc: pos("LoCC"),
            bookshelves: pos("Bookshelves"),
        })
    }
}

/// Extractor for the Project Gutenberg catalog CSV. See module docs.
pub struct GutenbergCatalogExtractor;

impl Extractor for GutenbergCatalogExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        // Open the file and transparently gunzip if it's a .gz —
        // Project Gutenberg ships pg_catalog.csv.gz, and the
        // bulk_download acquirer doesn't decompress on its own.
        // Magic-byte sniff (`\x1f\x8b`) so a renamed-but-compressed
        // file or a freshly-decompressed one both work.
        let mut file = File::open(source_path).map_err(|e| {
            Error::Extraction(format!(
                "Gutenberg catalog: failed to open {}: {e}",
                source_path.display()
            ))
        })?;
        let mut magic = [0u8; 2];
        let read_n = file.read(&mut magic).map_err(|e| {
            Error::Extraction(format!("Gutenberg catalog: read magic bytes failed: {e}"))
        })?;
        // Reopen so we get a fresh cursor at byte 0.
        let file = File::open(source_path).map_err(|e| {
            Error::Extraction(format!("Gutenberg catalog: reopen for read failed: {e}"))
        })?;

        let reader: Box<dyn std::io::Read + Send> = if read_n == 2 && magic == [0x1f, 0x8b] {
            Box::new(flate2::read::GzDecoder::new(file))
        } else {
            Box::new(file)
        };

        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(true)
            .flexible(true)
            .from_reader(reader);

        let headers = rdr
            .headers()
            .map_err(|e| Error::Extraction(format!("Gutenberg catalog: header read failed: {e}")))?
            .clone();
        let cols = ColumnIndex::from_headers(&headers)?;

        // Read everything into memory so we can return a `Send`
        // iterator without borrowing the reader. The catalog is
        // ~70K rows × ~500 bytes each ≈ 35 MB — fine to buffer
        // and a clean fit for `Box<dyn Iterator + Send>`.
        let mut docs = Vec::new();
        for result in rdr.records() {
            let record = match result {
                Ok(r) => r,
                Err(e) => {
                    docs.push(Err(Error::Extraction(format!(
                        "Gutenberg catalog: CSV row error: {e}"
                    ))));
                    continue;
                }
            };
            if let Some(doc) = build_doc(&record, &cols) {
                docs.push(Ok(doc));
            }
        }

        Ok(Box::new(docs.into_iter()))
    }
}

fn build_doc(record: &csv::StringRecord, cols: &ColumnIndex) -> Option<ExtractedDoc> {
    let text_id = record.get(cols.text_id).map(str::trim).unwrap_or("");
    let title = record.get(cols.title).map(str::trim).unwrap_or("");
    if text_id.is_empty() || title.is_empty() {
        return None;
    }

    // Skip non-text catalog entries (audio, music, etc.). The
    // on-demand `gutenberg-work` recipe expects a plain-text URL.
    let type_col = cols
        .type_col
        .and_then(|i| record.get(i))
        .map(str::trim)
        .unwrap_or("");
    if !type_col.is_empty() && !type_col.eq_ignore_ascii_case("Text") {
        return None;
    }

    let authors = cols
        .authors
        .and_then(|i| record.get(i))
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let issued = cols
        .issued
        .and_then(|i| record.get(i))
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let language = cols
        .language
        .and_then(|i| record.get(i))
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let subjects = cols
        .subjects
        .and_then(|i| record.get(i))
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let locc = cols
        .locc
        .and_then(|i| record.get(i))
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let bookshelves = cols
        .bookshelves
        .and_then(|i| record.get(i))
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let url = format!("https://www.gutenberg.org/ebooks/{text_id}");

    // FTS-indexed content: a multi-line catalog block. Verbose by
    // design so keyword search hits on `whaling`, `obsession`, etc.
    let mut content = String::new();
    content.push_str(&format!("Title: {title}\n"));
    if let Some(a) = authors {
        content.push_str(&format!("Author: {a}\n"));
    }
    if let Some(y) = issued {
        content.push_str(&format!("Year: {y}\n"));
    }
    if let Some(s) = subjects {
        content.push_str(&format!("Subjects: {s}\n"));
    }
    if let Some(l) = locc {
        content.push_str(&format!("LoCC: {l}\n"));
    }
    if let Some(b) = bookshelves {
        content.push_str(&format!("Bookshelves: {b}\n"));
    }
    if let Some(l) = language {
        content.push_str(&format!("Language: {l}\n"));
    }
    content.push_str(&format!("Gutenberg ID: {text_id}\n"));
    content.push_str(&format!("Download: {url}\n"));

    // Embed text: terse, semantically loaded form. The catalog
    // index lives or dies by vector matches on subject / author
    // intent, so we drop low-signal noise (URL, ID) and keep the
    // semantic core.
    let mut embed_parts = vec![format!("{title}")];
    if let Some(a) = authors {
        embed_parts.push(format!("by {a}"));
    }
    if let Some(y) = issued {
        embed_parts.push(format!("({y})"));
    }
    let head = embed_parts.join(" ");
    let mut embed_text = head.trim().to_string();
    if let Some(s) = subjects {
        embed_text.push_str(". Subjects: ");
        embed_text.push_str(s);
    }
    if let Some(l) = language {
        embed_text.push_str(". Language: ");
        embed_text.push_str(l);
    }

    // Search-side reads `metadata` as `HashMap<String, String>`
    // (see `corpus-engine/src/index/search.rs:200`), so a null
    // value would fail the round-trip. Stamp string-only entries
    // and elide missing fields entirely. Downstream consumers
    // (`partition_hits_by_kind`) check for presence with
    // `.get(key)`.
    let mut metadata = serde_json::Map::new();
    metadata.insert("gutenberg_id".into(), json!(text_id));
    metadata.insert("title".into(), json!(title));
    if let Some(a) = authors {
        metadata.insert("authors".into(), json!(a));
    }
    if let Some(y) = issued {
        metadata.insert("year".into(), json!(y));
    }
    if let Some(l) = language {
        metadata.insert("language".into(), json!(l));
    }
    if let Some(s) = subjects {
        metadata.insert("subjects".into(), json!(s));
    }
    if let Some(l) = locc {
        metadata.insert("locc".into(), json!(l));
    }
    if let Some(b) = bookshelves {
        metadata.insert("bookshelves".into(), json!(b));
    }
    let metadata = serde_json::Value::Object(metadata);

    Some(ExtractedDoc {
        title: Some(title.to_string()),
        content,
        url: Some(url),
        source_id: text_id.to_string(),
        metadata: Some(metadata),
        source_file: None,
        embed_text: Some(embed_text),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_csv(rows: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pg_catalog.csv");
        let mut f = File::create(&path).unwrap();
        for row in rows {
            writeln!(f, "{row}").unwrap();
        }
        dir
    }

    #[test]
    fn parses_a_canonical_row() {
        let dir = write_csv(&[
            "Text#,Type,Issued,Title,Language,Authors,Subjects,LoCC,Bookshelves",
            "2701,Text,1851,\"Moby Dick; or, The Whale\",en,\"Melville, Herman, 1819-1891\",\"Whaling -- Fiction; Sea stories; Psychological fiction\",PS,\"Adventure;Best Books Ever Listings\"",
        ]);
        let path = dir.path().join("pg_catalog.csv");
        let docs: Vec<_> = GutenbergCatalogExtractor
            .extract(&path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(docs.len(), 1);
        let doc = &docs[0];
        assert_eq!(doc.source_id, "2701");
        assert_eq!(doc.title.as_deref(), Some("Moby Dick; or, The Whale"));
        assert_eq!(
            doc.url.as_deref(),
            Some("https://www.gutenberg.org/ebooks/2701")
        );
        assert!(doc.content.contains("Melville, Herman, 1819-1891"));
        assert!(doc.content.contains("Whaling -- Fiction"));
        assert!(doc.content.contains("Gutenberg ID: 2701"));
        let embed = doc.embed_text.as_ref().unwrap();
        assert!(embed.contains("Moby Dick"));
        assert!(embed.contains("Melville"));
        assert!(embed.contains("Subjects:"));
        let meta = doc.metadata.as_ref().unwrap();
        assert_eq!(meta["gutenberg_id"], "2701");
        assert_eq!(meta["language"], "en");
    }

    #[test]
    fn rows_missing_title_or_id_are_skipped() {
        let dir = write_csv(&[
            "Text#,Type,Issued,Title,Language,Authors,Subjects,LoCC,Bookshelves",
            ",Text,1851,No ID,en,Anon,Fiction,PS,",
            "9999,Text,2000,,en,Anon,Empty Title,PS,",
            "1234,Text,1900,Has Both,en,Anon,Fiction,PS,",
        ]);
        let path = dir.path().join("pg_catalog.csv");
        let docs: Vec<_> = GutenbergCatalogExtractor
            .extract(&path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].source_id, "1234");
    }

    #[test]
    fn non_text_types_are_skipped() {
        let dir = write_csv(&[
            "Text#,Type,Issued,Title,Language,Authors,Subjects,LoCC,Bookshelves",
            "100,Sound,1900,Audio Book,en,Anon,Audio,PS,",
            "101,Text,1900,Real Book,en,Anon,Fiction,PS,",
        ]);
        let path = dir.path().join("pg_catalog.csv");
        let docs: Vec<_> = GutenbergCatalogExtractor
            .extract(&path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].source_id, "101");
    }

    #[test]
    fn missing_required_column_errors() {
        // No `Title` column.
        let dir = write_csv(&["Text#,Issued,Language", "1,1850,en"]);
        let path = dir.path().join("pg_catalog.csv");
        let err = GutenbergCatalogExtractor.extract(&path).err().unwrap();
        assert!(format!("{err}").contains("Title"));
    }

    #[test]
    fn parses_a_gzipped_csv() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let csv = "Text#,Type,Issued,Title,Language,Authors,Subjects,LoCC,Bookshelves\n\
                   2701,Text,1851,\"Moby Dick; or, The Whale\",en,\"Melville, Herman, 1819-1891\",\"Whaling -- Fiction\",PS,Adventure\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pg_catalog.csv.gz");
        let f = File::create(&path).unwrap();
        let mut enc = GzEncoder::new(f, Compression::default());
        enc.write_all(csv.as_bytes()).unwrap();
        enc.finish().unwrap();

        let docs: Vec<_> = GutenbergCatalogExtractor
            .extract(&path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].source_id, "2701");
    }
}
