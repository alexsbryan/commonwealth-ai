use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};
use super::{ExtractedDoc, Extractor};

/// JSONL (JSON Lines) file extractor.
///
/// Parses line-delimited JSON files, with optional GZ decompression.
/// Supports OpenAlex-style inverted index abstract reconstruction.
pub struct JsonlExtractor {
    /// JSON field name containing the main content.
    pub content_field: Option<String>,
    /// JSON field name containing the title.
    pub title_field: Option<String>,
    /// Optional filter (currently unused, reserved for future JSONPath filtering).
    pub filter: Option<String>,
    /// Decompression mode: Some("gzip") or None.
    pub decompress: Option<String>,
}

impl JsonlExtractor {
    pub fn new() -> Self {
        Self {
            content_field: None,
            title_field: None,
            filter: None,
            decompress: None,
        }
    }

    /// Create an extractor configured for OpenAlex works JSONL files.
    pub fn openalex() -> Self {
        Self {
            content_field: Some("abstract_inverted_index".to_string()),
            title_field: Some("title".to_string()),
            filter: None,
            decompress: Some("gzip".to_string()),
        }
    }
}

impl Default for JsonlExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for JsonlExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let file = File::open(source_path)
            .map_err(|e| Error::Extraction(format!("Failed to open {}: {e}", source_path.display())))?;

        let is_gz = self.decompress.as_deref() == Some("gzip")
            || source_path
                .extension()
                .and_then(|e| e.to_str())
                .map_or(false, |e| e == "gz");

        let reader: Box<dyn BufRead + Send> = if is_gz {
            Box::new(BufReader::new(flate2::read::GzDecoder::new(file)))
        } else {
            Box::new(BufReader::new(file))
        };

        Ok(Box::new(JsonlIterator {
            lines: reader.lines(),
            content_field: self.content_field.clone(),
            title_field: self.title_field.clone(),
            pending: VecDeque::new(),
        }))
    }
}

struct JsonlIterator {
    lines: std::io::Lines<Box<dyn BufRead + Send>>,
    content_field: Option<String>,
    title_field: Option<String>,
    pending: VecDeque<ExtractedDoc>,
}

impl Iterator for JsonlIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(doc) = self.pending.pop_front() {
                return Some(Ok(doc));
            }
            let line = match self.lines.next()? {
                Ok(l) => l,
                Err(e) => return Some(Err(Error::Extraction(format!("Read error: {e}")))),
            };
            if line.trim().is_empty() {
                continue;
            }

            // Try OpenAlex-specific format first.
            if self.content_field.as_deref() == Some("abstract_inverted_index") {
                let work: OpenAlexWork = match serde_json::from_str(&line) {
                    Ok(w) => w,
                    Err(_) => continue,
                };
                if let Some(doc) = format_openalex_work(&work) {
                    self.pending.push_back(doc);
                    continue;
                }
                continue;
            }

            // Generic JSONL parsing.
            let obj: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let content = self
                .content_field
                .as_ref()
                .and_then(|f| obj.get(f))
                .and_then(|v| v.as_str())
                .or_else(|| obj.get("content").and_then(|v| v.as_str()))
                .or_else(|| obj.get("text").and_then(|v| v.as_str()));

            let content = match content {
                Some(c) if !c.trim().is_empty() => c.to_string(),
                _ => continue,
            };

            let title = self
                .title_field
                .as_ref()
                .and_then(|f| obj.get(f))
                .and_then(|v| v.as_str())
                .or_else(|| obj.get("title").and_then(|v| v.as_str()))
                .map(|s| s.to_string());

            let id = obj
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            self.pending.push_back(ExtractedDoc {
                title,
                content,
                url: obj.get("url").and_then(|v| v.as_str()).map(|s| s.to_string()),
                source_id: id,
                metadata: None,
                source_file: None,
            });
        }
    }
}

// ─── OpenAlex-specific types ──────────────────────────────────

#[derive(Deserialize)]
struct OpenAlexWork {
    id: Option<String>,
    title: Option<String>,
    publication_year: Option<i32>,
    doi: Option<String>,
    cited_by_count: Option<i32>,
    abstract_inverted_index: Option<serde_json::Value>,
    authorships: Option<Vec<Authorship>>,
}

#[derive(Deserialize)]
struct Authorship {
    author: Option<AuthorInfo>,
}

#[derive(Deserialize)]
struct AuthorInfo {
    display_name: Option<String>,
}

fn format_openalex_work(work: &OpenAlexWork) -> Option<ExtractedDoc> {
    // Filter: year >= 2010 and has abstract.
    let year = work.publication_year?;
    if year < 2010 {
        return None;
    }
    let abstract_text = work
        .abstract_inverted_index
        .as_ref()
        .and_then(reconstruct_abstract)?;
    if abstract_text.is_empty() {
        return None;
    }

    let title = work.title.as_deref().unwrap_or("Untitled").to_string();
    let authors = work
        .authorships
        .as_ref()
        .map(|a| {
            a.iter()
                .filter_map(|a| a.author.as_ref()?.display_name.as_deref())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let doi = work.doi.as_deref().unwrap_or("");
    let cited_by = work.cited_by_count.unwrap_or(0);

    let mut content = String::new();
    if !authors.is_empty() {
        content.push_str(&format!("Authors: {authors}\n"));
    }
    content.push_str(&format!("Year: {year} | Cited by: {cited_by}"));
    if !doi.is_empty() {
        content.push_str(&format!(" | DOI: {doi}"));
    }
    content.push_str(&format!("\n\n{abstract_text}"));

    let source_id = work
        .id
        .as_deref()
        .unwrap_or(&title)
        .replace("https://openalex.org/", "");

    Some(ExtractedDoc {
        title: Some(title),
        content,
        url: work.doi.clone(),
        source_id,
        metadata: Some(serde_json::json!({
            "year": year,
            "cited_by_count": cited_by,
        })),
        source_file: None,
    })
}

// `reconstruct_abstract` lives in `super::reconstruct_abstract` (extractors/mod.rs)
// so both the JSONL and Parquet extractors can use it.
use super::reconstruct_abstract;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_openalex_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("works.jsonl");
        let mut f = File::create(&file_path).unwrap();

        // Valid record with abstract.
        writeln!(
            f,
            r#"{{"id":"https://openalex.org/W123","title":"Test Paper","publication_year":2020,"doi":"10.1234/test","cited_by_count":42,"abstract_inverted_index":{{"This":[0],"is":[1],"a":[2],"test":[3],"abstract.":[4]}},"authorships":[{{"author":{{"display_name":"Jane Doe"}}}}]}}"#
        )
        .unwrap();

        // Record too old -- should be filtered.
        writeln!(
            f,
            r#"{{"id":"https://openalex.org/W456","title":"Old Paper","publication_year":2005,"abstract_inverted_index":{{"old":[0]}},"authorships":[]}}"#
        )
        .unwrap();

        // Record without abstract -- should be filtered.
        writeln!(
            f,
            r#"{{"id":"https://openalex.org/W789","title":"No Abstract","publication_year":2022,"authorships":[]}}"#
        )
        .unwrap();

        let extractor = JsonlExtractor {
            content_field: Some("abstract_inverted_index".to_string()),
            title_field: Some("title".to_string()),
            filter: None,
            decompress: None,
        };
        let docs: Vec<_> = extractor
            .extract(&file_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].title.as_deref(), Some("Test Paper"));
        assert!(docs[0].content.contains("Jane Doe"));
        assert!(docs[0].content.contains("This is a test abstract."));
        assert!(docs[0].content.contains("2020"));
    }

    #[test]
    fn parse_generic_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("data.jsonl");
        let mut f = File::create(&file_path).unwrap();
        writeln!(f, r#"{{"id":"1","title":"Doc One","content":"Content of doc one."}}"#).unwrap();
        writeln!(f, r#"{{"id":"2","title":"Doc Two","content":"Content of doc two."}}"#).unwrap();
        writeln!(f, r#"{{"id":"3","content":""}}"#).unwrap(); // empty content, skipped

        let extractor = JsonlExtractor::new();
        let docs: Vec<_> = extractor
            .extract(&file_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].title.as_deref(), Some("Doc One"));
        assert_eq!(docs[0].content, "Content of doc one.");
        assert_eq!(docs[1].title.as_deref(), Some("Doc Two"));
    }

    #[test]
    fn reconstruct_abstract_works() {
        let idx = serde_json::json!({
            "Machine": [0],
            "learning": [1],
            "is": [2],
            "great.": [3]
        });
        let text = reconstruct_abstract(&idx).unwrap();
        assert_eq!(text, "Machine learning is great.");
    }

    #[test]
    fn parse_gzipped_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("data.jsonl.gz");

        let f = File::create(&file_path).unwrap();
        let mut encoder = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
        writeln!(encoder, r#"{{"id":"1","title":"Compressed","content":"Compressed content."}}"#).unwrap();
        encoder.finish().unwrap();

        let extractor = JsonlExtractor {
            content_field: None,
            title_field: None,
            filter: None,
            decompress: Some("gzip".to_string()),
        };
        let docs: Vec<_> = extractor
            .extract(&file_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].title.as_deref(), Some("Compressed"));
    }
}
