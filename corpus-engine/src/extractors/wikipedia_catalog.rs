//! Wikipedia catalog extractor.
//!
//! Reads the JSONL produced by
//! `sovereign-recipes/wikipedia-catalog/scripts/build_catalog.py` —
//! one record per Wikipedia article carrying `title`, `url`,
//! `abstract` (lead paragraph) and `sections` (table-of-contents
//! anchors). Emits one [`ExtractedDoc`] per article.
//!
//! Pair with `[corpus] kind = "catalog"` and a `[catalog]` block
//! whose `content_recipe` points at `wikipedia-article` for the
//! per-article on-demand fetch.
//!
//! ## Why a JSONL intermediate
//!
//! The upstream Wikimedia "abstract dump" is a 1 GB gzipped XML.
//! Parsing XML in Rust pulls in another dep + slower streaming
//! parser; a one-shot Python conversion to JSONL is the cheaper
//! engineering path and lets the catalog corpus ship pre-converted.
//! See `build_catalog.py` for the conversion details.
//!
//! ## What lands in the catalog index
//!
//! - **content** (FTS-indexed): a multi-line catalog block — title,
//!   URL, abstract, sections list. Verbose by design so keyword
//!   search hits on section anchors and abstract phrases.
//! - **embed_text** (vector-indexed): a terser semantic core —
//!   `"<title>. <abstract>. Sections: <s1>, <s2>, …"`. Drives the
//!   "this article covers your sub-topic" signal that lets the
//!   catalog hit fire even on long-tail queries.
//! - **metadata.title / .url / .sections**: surfaced in the
//!   `CatalogHit` so the on-demand ingest can substitute the title
//!   into the article-fetch URL and the UI can show a preview.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use serde::Deserialize;
use serde_json::json;

use super::{ExtractedDoc, Extractor};
use crate::error::{Error, Result};

/// On-disk JSONL row produced by `build_catalog.py`. Stable —
/// changes here must also bump the script's output schema.
#[derive(Debug, Deserialize)]
struct CatalogRow {
    title: String,
    url: String,
    #[serde(default)]
    abstract_text: Option<String>,
    #[serde(default, alias = "abstract")]
    abstract_alias: Option<String>,
    #[serde(default)]
    sections: Vec<String>,
}

impl CatalogRow {
    fn abstract_str(&self) -> &str {
        self.abstract_text
            .as_deref()
            .or(self.abstract_alias.as_deref())
            .unwrap_or("")
    }
}

pub struct WikipediaCatalogExtractor;

impl Extractor for WikipediaCatalogExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        // Magic-byte sniff — the JSONL ships gzipped to keep the
        // wire size low (~70 MB compressed for 6.8M rows).
        let mut file = File::open(source_path).map_err(|e| {
            Error::Extraction(format!(
                "wikipedia_catalog: open {}: {e}",
                source_path.display()
            ))
        })?;
        let mut magic = [0u8; 2];
        let read_n = file
            .read(&mut magic)
            .map_err(|e| Error::Extraction(format!("wikipedia_catalog: read magic: {e}")))?;
        let file = File::open(source_path)
            .map_err(|e| Error::Extraction(format!("wikipedia_catalog: reopen: {e}")))?;
        let reader: Box<dyn Read + Send> = if read_n == 2 && magic == [0x1f, 0x8b] {
            Box::new(flate2::read::GzDecoder::new(file))
        } else {
            Box::new(file)
        };

        // Stream rows and produce docs lazily — at 6.8M rows the
        // intermediate Vec would be a ~5 GB allocation. Boxing the
        // iterator keeps memory flat at ~one row at a time.
        let buf = BufReader::new(reader);
        let iter = buf.lines().map(|line_res| {
            let line = line_res
                .map_err(|e| Error::Extraction(format!("wikipedia_catalog: read line: {e}")))?;
            if line.trim().is_empty() {
                return Err(Error::Extraction("wikipedia_catalog: empty line".into()));
            }
            let row: CatalogRow = serde_json::from_str(&line)
                .map_err(|e| Error::Extraction(format!("wikipedia_catalog: bad JSON line: {e}")))?;
            Ok(build_doc(row))
        });
        // Filter the empty-line errors out cleanly while preserving
        // real parse errors. Empty lines at the tail of a writer's
        // output are common; treating them as fatal would break a
        // legitimate file.
        let iter = iter.filter(|r| match r {
            Err(Error::Extraction(msg)) if msg.contains("empty line") => false,
            _ => true,
        });
        Ok(Box::new(iter))
    }
}

fn build_doc(row: CatalogRow) -> ExtractedDoc {
    let abstract_str = row.abstract_str().to_string();
    let sections_joined = row.sections.join(", ");

    // FTS content — verbose, multi-line.
    let mut content = String::new();
    content.push_str(&format!("Title: {}\n", row.title));
    content.push_str(&format!("URL: {}\n", row.url));
    if !abstract_str.is_empty() {
        content.push_str(&format!("Abstract: {abstract_str}\n"));
    }
    if !sections_joined.is_empty() {
        content.push_str(&format!("Sections: {sections_joined}\n"));
    }

    // Embed text — terse semantic core. Keep the title up front
    // (most discriminative) and follow with the abstract; sections
    // ride after the abstract so vector match on a sub-topic
    // anchors when the abstract alone misses.
    let mut embed = row.title.clone();
    if !abstract_str.is_empty() {
        embed.push_str(". ");
        embed.push_str(&abstract_str);
    }
    if !sections_joined.is_empty() {
        embed.push_str(". Sections: ");
        embed.push_str(&sections_joined);
    }

    let mut metadata = serde_json::Map::new();
    metadata.insert("title".into(), json!(row.title));
    metadata.insert("url".into(), json!(row.url));
    if !abstract_str.is_empty() {
        metadata.insert("abstract".into(), json!(abstract_str));
    }
    if !sections_joined.is_empty() {
        metadata.insert("sections".into(), json!(sections_joined));
    }

    ExtractedDoc {
        title: Some(row.title.clone()),
        content,
        url: Some(row.url),
        // The catalog id IS the title — that's what
        // `wikipedia-article` ingests need to substitute into the
        // REST URL. CatalogConfig.id_field = "title".
        source_id: row.title,
        metadata: Some(serde_json::Value::Object(metadata)),
        source_file: None,
        embed_text: Some(embed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_jsonl(rows: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.jsonl");
        let mut f = File::create(&path).unwrap();
        for r in rows {
            writeln!(f, "{r}").unwrap();
        }
        dir
    }

    #[test]
    fn parses_a_canonical_row() {
        let dir = write_jsonl(&[
            r#"{"title":"Albert Einstein","url":"https://en.wikipedia.org/wiki/Albert_Einstein","abstract":"German-born theoretical physicist.","sections":["Early life","Career","Personal life"]}"#,
        ]);
        let path = dir.path().join("catalog.jsonl");
        let docs: Vec<_> = WikipediaCatalogExtractor
            .extract(&path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(docs.len(), 1);
        let d = &docs[0];
        assert_eq!(d.title.as_deref(), Some("Albert Einstein"));
        assert_eq!(d.source_id, "Albert Einstein");
        assert!(d.content.contains("Abstract: German-born"));
        assert!(d
            .content
            .contains("Sections: Early life, Career, Personal life"));
        let embed = d.embed_text.as_ref().unwrap();
        assert!(embed.starts_with("Albert Einstein"));
        assert!(embed.contains("German-born"));
        let meta = d.metadata.as_ref().unwrap();
        assert_eq!(meta["title"], "Albert Einstein");
        assert_eq!(meta["url"], "https://en.wikipedia.org/wiki/Albert_Einstein");
    }

    #[test]
    fn handles_gzipped_jsonl() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.jsonl.gz");
        let f = File::create(&path).unwrap();
        let mut enc = GzEncoder::new(f, Compression::default());
        writeln!(
            enc,
            r#"{{"title":"Roman Empire","url":"https://en.wikipedia.org/wiki/Roman_Empire","abstract":"Post-Republican period of ancient Rome.","sections":[]}}"#
        )
        .unwrap();
        enc.finish().unwrap();
        let docs: Vec<_> = WikipediaCatalogExtractor
            .extract(&path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].title.as_deref(), Some("Roman Empire"));
    }

    #[test]
    fn skips_empty_trailing_lines() {
        let dir = write_jsonl(&[
            r#"{"title":"X","url":"https://x","abstract":"","sections":[]}"#,
            "",
            "",
        ]);
        let path = dir.path().join("catalog.jsonl");
        let docs: Vec<_> = WikipediaCatalogExtractor
            .extract(&path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(docs.len(), 1);
    }
}
