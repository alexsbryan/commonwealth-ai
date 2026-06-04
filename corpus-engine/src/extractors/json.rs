use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::Deserialize;

use super::{ExtractedDoc, Extractor};
use crate::error::{Error, Result};

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
        // `local_file` may name a single .jsonl file OR a directory of
        // them (the recipe author commonly points `acquire.path` at a
        // "folder"). Resolve to a concrete, ordered file list up front
        // so the iterator never reads a directory file descriptor: on
        // Unix `File::open` *succeeds* on a directory, but every read
        // then returns EISDIR ("Is a directory"). The old single-file
        // iterator surfaced that as `Some(Err(..))` on every `next()`
        // and never terminated, spinning the ingest skip-loop to
        // millions of phantom "skipped" documents. See `collect_jsonl_files`.
        let files = collect_jsonl_files(source_path)?;

        Ok(Box::new(JsonlIterator {
            remaining_files: files.into(),
            current: None,
            content_field: self.content_field.clone(),
            title_field: self.title_field.clone(),
            decompress: self.decompress.clone(),
            pending: VecDeque::new(),
        }))
    }
}

/// Resolve a `local_file` source path into the ordered list of JSONL
/// files to read.
///
/// - regular file → `[path]` (extension is not re-checked; the recipe
///   author chose `jsonl` extraction deliberately);
/// - directory → every `*.jsonl` / `*.jsonl.gz` inside it, sorted for
///   deterministic, resume-stable iteration order;
/// - a directory with no JSONL files is a recipe error surfaced
///   cleanly — not an empty corpus, not a runaway.
fn collect_jsonl_files(source_path: &Path) -> Result<Vec<std::path::PathBuf>> {
    let meta = std::fs::metadata(source_path).map_err(|e| {
        Error::Extraction(format!("Failed to stat {}: {e}", source_path.display()))
    })?;
    if meta.is_file() {
        return Ok(vec![source_path.to_path_buf()]);
    }
    if meta.is_dir() {
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(source_path)
            .map_err(|e| {
                Error::Extraction(format!(
                    "Failed to read directory {}: {e}",
                    source_path.display()
                ))
            })?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && is_jsonl_path(p))
            .collect();
        files.sort();
        if files.is_empty() {
            return Err(Error::Extraction(format!(
                "no .jsonl or .jsonl.gz files in directory {} — point \
                 `acquire.path` at a .jsonl file or a folder containing them",
                source_path.display()
            )));
        }
        return Ok(files);
    }
    Err(Error::Extraction(format!(
        "{} is neither a file nor a directory",
        source_path.display()
    )))
}

/// `true` for `*.jsonl` and `*.jsonl.gz` (case-insensitive).
fn is_jsonl_path(p: &Path) -> bool {
    let lower = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    lower.ends_with(".jsonl") || lower.ends_with(".jsonl.gz")
}

/// The file currently being streamed by `JsonlIterator`.
struct CurrentFile {
    lines: std::io::Lines<Box<dyn BufRead + Send>>,
    /// Bare file name, stamped onto each `ExtractedDoc.source_file` so
    /// the ingest loop's per-file boundary detection works across a
    /// multi-file (directory) source.
    source_file: String,
}

struct JsonlIterator {
    /// Files still to read, in deterministic order. Popped from the
    /// front as each is exhausted.
    remaining_files: VecDeque<std::path::PathBuf>,
    /// The file currently open, or `None` between files / at end.
    current: Option<CurrentFile>,
    content_field: Option<String>,
    title_field: Option<String>,
    decompress: Option<String>,
    pending: VecDeque<ExtractedDoc>,
}

impl JsonlIterator {
    /// Open the next queued file. Returns:
    /// - `None` when the queue is empty (the whole stream is done);
    /// - `Some(Err)` when a file can't be opened — surfaced once, with
    ///   `current` left cleared so the following `next()` advances to
    ///   the file after it (one bad file never stalls a directory run);
    /// - `Some(Ok(()))` when a file opened and `current` is now set.
    fn open_next(&mut self) -> Option<Result<()>> {
        let path = self.remaining_files.pop_front()?;
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                return Some(Err(Error::Extraction(format!(
                    "Failed to open {}: {e}",
                    path.display()
                ))));
            }
        };
        let is_gz = self.decompress.as_deref() == Some("gzip")
            || path.extension().and_then(|e| e.to_str()) == Some("gz");
        let reader: Box<dyn BufRead + Send> = if is_gz {
            Box::new(BufReader::new(flate2::read::GzDecoder::new(file)))
        } else {
            Box::new(BufReader::new(file))
        };
        let source_file = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        self.current = Some(CurrentFile {
            lines: reader.lines(),
            source_file,
        });
        Some(Ok(()))
    }
}

impl Iterator for JsonlIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(doc) = self.pending.pop_front() {
                return Some(Ok(doc));
            }

            // Make sure a file is open. Open failures surface once and
            // leave `current` cleared, so the next iteration advances
            // past the bad file rather than retrying it.
            if self.current.is_none() {
                match self.open_next() {
                    None => return None, // queue drained — stream complete
                    Some(Ok(())) => {}
                    Some(Err(e)) => return Some(Err(e)),
                }
            }

            // Read one line from the current file. A hard read error
            // (EISDIR from a directory fd, mid-stream gzip corruption,
            // …) is surfaced ONCE, then the file is dropped — we never
            // re-yield the same OS error forever. That infinite
            // re-yield was the root of the multi-million phantom-skip
            // runaway (#6).
            let (line, source_file) = {
                let current = match self.current.as_mut() {
                    Some(c) => c,
                    None => continue,
                };
                match current.lines.next() {
                    None => {
                        self.current = None; // exhausted — advance to next file
                        continue;
                    }
                    Some(Ok(l)) => (l, current.source_file.clone()),
                    Some(Err(e)) => {
                        let src = current.source_file.clone();
                        self.current = None;
                        return Some(Err(Error::Extraction(format!(
                            "read error in {src}: {e}"
                        ))));
                    }
                }
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
                if let Some(mut doc) = format_openalex_work(&work) {
                    doc.source_file = Some(source_file.clone());
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

            // Pass-through metadata: strip out the fields we already
            // copied onto the ExtractedDoc (content / title / url /
            // id) and keep the rest as a metadata object so
            // downstream ChunkFilter predicates (see
            // ChunkFilter::metadata_in / metadata_compare) can reach
            // them. Legacy callers that don't need metadata simply
            // ignore the field.
            let metadata = match obj.as_object() {
                Some(map) => {
                    let mut filtered = serde_json::Map::new();
                    for (k, v) in map {
                        match k.as_str() {
                            "content" | "title" | "url" | "id" | "text" => continue,
                            _ => {
                                filtered.insert(k.clone(), v.clone());
                            }
                        }
                    }
                    if filtered.is_empty() {
                        None
                    } else {
                        Some(serde_json::Value::Object(filtered))
                    }
                }
                None => None,
            };

            self.pending.push_back(ExtractedDoc {
                title,
                content,
                url: obj
                    .get("url")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                source_id: id,
                metadata,
                source_file: Some(source_file),
                embed_text: None,
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
        embed_text: None,
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
        writeln!(
            f,
            r#"{{"id":"1","title":"Doc One","content":"Content of doc one."}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"id":"2","title":"Doc Two","content":"Content of doc two."}}"#
        )
        .unwrap();
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
        writeln!(
            encoder,
            r#"{{"id":"1","title":"Compressed","content":"Compressed content."}}"#
        )
        .unwrap();
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

    #[test]
    fn directory_source_walks_all_jsonl_files() {
        // #6 regression: `local_file` pointing at a *directory* must walk
        // the .jsonl files inside it (the recipe author phrases the source
        // as a "folder"), not open the dir fd and spin forever on EISDIR.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.jsonl"),
            "{\"id\":\"1\",\"title\":\"A\",\"content\":\"alpha\"}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b.jsonl"),
            "{\"id\":\"2\",\"title\":\"B\",\"content\":\"beta\"}\n",
        )
        .unwrap();
        // A non-JSONL sibling must be ignored, not fed to the parser.
        std::fs::write(dir.path().join("README.txt"), "ignore me").unwrap();

        let extractor = JsonlExtractor::new();
        let docs: Vec<_> = extractor
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(docs.len(), 2, "both .jsonl files walked; .txt ignored");
        // Sorted order: a.jsonl then b.jsonl. Each doc is stamped with its
        // originating file so the ingest loop's boundary tracking works.
        assert_eq!(docs[0].title.as_deref(), Some("A"));
        assert_eq!(docs[0].source_file.as_deref(), Some("a.jsonl"));
        assert_eq!(docs[1].title.as_deref(), Some("B"));
        assert_eq!(docs[1].source_file.as_deref(), Some("b.jsonl"));
    }

    #[test]
    fn directory_with_no_jsonl_files_fails_cleanly() {
        // A folder with nothing matching must error up front — not yield an
        // empty corpus, and never the old EISDIR runaway.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "nope").unwrap();

        let extractor = JsonlExtractor::new();
        let err = match extractor.extract(dir.path()) {
            Ok(_) => panic!("expected an error for a directory with no .jsonl files"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("no .jsonl"),
            "expected a clean no-jsonl error, got: {err}"
        );
    }

    #[test]
    fn read_error_terminates_iterator() {
        // Fix A: a hard read error (here, a file claiming to be gzip but
        // holding plain bytes) is surfaced ONCE, then the iterator ends.
        // Before, a read error was re-yielded on every `next()`, so the
        // ingest skip-loop never terminated.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.jsonl.gz");
        std::fs::write(&path, b"this is not gzip data").unwrap();

        let extractor = JsonlExtractor {
            content_field: None,
            title_field: None,
            filter: None,
            decompress: Some("gzip".to_string()),
        };
        let mut iter = extractor.extract(&path).unwrap();
        // First poll: the decode error surfaces.
        assert!(
            matches!(iter.next(), Some(Err(_))),
            "read error surfaces once"
        );
        // Second poll: terminated, not the same error forever.
        assert!(
            iter.next().is_none(),
            "iterator terminates after a read error"
        );
    }
}
