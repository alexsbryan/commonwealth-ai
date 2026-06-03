//! JSON-API extractor.
//!
//! Pairs with the `http_api` acquirer when `[acquire.follow]` is
//! absent: each fetched page is persisted as a single `.json` file
//! containing the raw response body, and this extractor walks that
//! file with a JSONPath to emit one [`ExtractedDoc`] per element of
//! the documents array.
//!
//! Typical shape:
//!
//! ```toml
//! [extract]
//! type           = "json"
//! document_path  = "$.results[*]"      # required
//! content_field  = "plain_text"        # required
//! title_field    = "case_name"         # optional
//! url_field      = "absolute_url"      # optional
//! id_field       = "id"                # optional
//! ```
//!
//! Documents missing the configured `content_field` (or with an
//! empty string) are skipped — the same posture as the JSONL
//! extractor — so partial pages don't poison the corpus.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use jsonpath_rust::JsonPath;
use serde_json::Value;

use super::{ExtractedDoc, Extractor};
use crate::error::{Error, Result};

/// Walks a single JSON file and emits one document per JSONPath match.
pub struct JsonApiExtractor {
    /// JSONPath expression selecting the documents array
    /// (e.g. `$.results[*]`). Must match an array of objects.
    pub document_path: String,

    /// Name of the field on each matched object that holds the
    /// document's full text. Required.
    pub content_field: String,

    /// Optional field name for the document title.
    pub title_field: Option<String>,

    /// Optional field name for the document's canonical URL.
    pub url_field: Option<String>,

    /// Optional field name for the document's stable id. When unset
    /// (or the field is missing), falls back to the position of the
    /// match in the array, prefixed with the source filename stem,
    /// so two pages from the same recipe never collide.
    pub id_field: Option<String>,
}

impl Extractor for JsonApiExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        // Pre-validate the JSONPath so a bad recipe surfaces with a
        // useful error at extract() time instead of opaquely yielding
        // zero docs (or only erroring on the first per-file open).
        let _jpath: JsonPath<Value> =
            JsonPath::try_from(self.document_path.as_str()).map_err(|e| {
                Error::Extraction(format!(
                    "extract.document_path `{}` is not a valid JSONPath: {e}",
                    self.document_path
                ))
            })?;

        // `source_path` can be a single file (the historical single-
        // request shape: one acquirer-persisted JSON file) OR a
        // directory of `<sha>.json` files (the paginated shape:
        // `http_api` with no follow writes one file per page). Walk
        // either and chain the matches across all of them.
        let files = collect_json_files(source_path)?;
        let iter = JsonApiMultiFileIter {
            files: files.into_iter(),
            current: None,
            document_path: self.document_path.clone(),
            content_field: self.content_field.clone(),
            title_field: self.title_field.clone(),
            url_field: self.url_field.clone(),
            id_field: self.id_field.clone(),
        };
        Ok(Box::new(iter))
    }
}

/// Walk a JSON source path. Single-file: just that file. Directory:
/// every `*.json` (recursively) sorted for stability.
fn collect_json_files(path: &Path) -> Result<Vec<std::path::PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut out = Vec::new();
    collect_recursive(path, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_recursive(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| Error::Extraction(format!("read_dir {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::Extraction(format!("dir entry: {e}")))?;
        let p = entry.path();
        if p.is_dir() {
            collect_recursive(&p, out)?;
        } else if p
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("json"))
            .unwrap_or(false)
        {
            out.push(p);
        }
    }
    Ok(())
}

struct JsonApiMultiFileIter {
    files: std::vec::IntoIter<std::path::PathBuf>,
    current: Option<JsonApiIter>,
    document_path: String,
    content_field: String,
    title_field: Option<String>,
    url_field: Option<String>,
    id_field: Option<String>,
}

impl Iterator for JsonApiMultiFileIter {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(iter) = self.current.as_mut() {
                if let Some(doc) = iter.next() {
                    return Some(doc);
                }
                self.current = None;
            }
            let path = self.files.next()?;
            match open_one_file(
                &path,
                &self.document_path,
                &self.content_field,
                &self.title_field,
                &self.url_field,
                &self.id_field,
            ) {
                Ok(iter) => self.current = Some(iter),
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

fn open_one_file(
    path: &Path,
    document_path: &str,
    content_field: &str,
    title_field: &Option<String>,
    url_field: &Option<String>,
    id_field: &Option<String>,
) -> Result<JsonApiIter> {
    let jpath = JsonPath::try_from(document_path).map_err(|e| {
        Error::Extraction(format!(
            "extract.document_path `{document_path}` is not a valid JSONPath: {e}"
        ))
    })?;
    let mut file = File::open(path)
        .map_err(|e| Error::Extraction(format!("Failed to open {}: {e}", path.display())))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| Error::Extraction(format!("Failed to read {}: {e}", path.display())))?;
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Extraction(format!("{} is not valid JSON: {e}", path.display())))?;
    let matches = match jpath.find(&body) {
        Value::Array(arr) => arr,
        other => vec![other],
    };
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("page")
        .to_string();
    Ok(JsonApiIter {
        matches: matches.into_iter(),
        content_field: content_field.to_string(),
        title_field: title_field.clone(),
        url_field: url_field.clone(),
        id_field: id_field.clone(),
        stem,
        position: 0,
    })
}

struct JsonApiIter {
    matches: std::vec::IntoIter<Value>,
    content_field: String,
    title_field: Option<String>,
    url_field: Option<String>,
    id_field: Option<String>,
    stem: String,
    position: usize,
}

impl Iterator for JsonApiIter {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let value = self.matches.next()?;
            let pos = self.position;
            self.position += 1;

            let obj = match value.as_object() {
                Some(o) => o,
                None => continue,
            };

            let content = obj
                .get(&self.content_field)
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let content = match content {
                Some(c) => c,
                None => continue,
            };

            let title = self
                .title_field
                .as_deref()
                .and_then(|f| obj.get(f))
                .and_then(|v| v.as_str())
                .map(String::from);

            let url = self
                .url_field
                .as_deref()
                .and_then(|f| obj.get(f))
                .and_then(|v| v.as_str())
                .map(String::from);

            // id field is permissive: stringify integers / nulls so
            // the agent can author `id_field = "id"` against an API
            // that hands back numeric ids without the recipe needing
            // a per-API massaging step.
            let source_id = self
                .id_field
                .as_deref()
                .and_then(|f| obj.get(f))
                .and_then(value_to_id_string)
                .unwrap_or_else(|| format!("{}-{}", self.stem, pos));

            // Pass through any non-content/title/url/id fields as
            // metadata so downstream chunk filters
            // (KnowledgeDensity / metadata_in / metadata_compare)
            // can reach them. Mirrors the JSONL extractor's posture
            // so the two are interchangeable on inspection.
            let metadata = {
                let mut filtered = serde_json::Map::new();
                for (k, v) in obj {
                    let drop = k == &self.content_field
                        || (self.title_field.as_deref() == Some(k.as_str()))
                        || (self.url_field.as_deref() == Some(k.as_str()))
                        || (self.id_field.as_deref() == Some(k.as_str()));
                    if !drop {
                        filtered.insert(k.clone(), v.clone());
                    }
                }
                if filtered.is_empty() {
                    None
                } else {
                    Some(Value::Object(filtered))
                }
            };

            return Some(Ok(ExtractedDoc {
                title,
                content,
                url,
                source_id,
                metadata,
                source_file: None,
                embed_text: None,
            }));
        }
    }
}

fn value_to_id_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_page(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut f = File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    fn run(extractor: &JsonApiExtractor, path: &Path) -> Vec<ExtractedDoc> {
        extractor
            .extract(path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn extracts_results_array_with_plain_text() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
            "next": "https://example.com/page2",
            "results": [
                {
                    "id": 101,
                    "case_name": "Doe v. Roe",
                    "absolute_url": "/opinion/101/",
                    "plain_text": "First opinion body."
                },
                {
                    "id": 102,
                    "case_name": "Smith v. Jones",
                    "absolute_url": "/opinion/102/",
                    "plain_text": "Second opinion body."
                }
            ]
        }"#;
        let path = write_page(dir.path(), "page1.json", body);

        let extractor = JsonApiExtractor {
            document_path: "$.results[*]".into(),
            content_field: "plain_text".into(),
            title_field: Some("case_name".into()),
            url_field: Some("absolute_url".into()),
            id_field: Some("id".into()),
        };
        let docs = run(&extractor, &path);

        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].title.as_deref(), Some("Doe v. Roe"));
        assert_eq!(docs[0].content, "First opinion body.");
        assert_eq!(docs[0].url.as_deref(), Some("/opinion/101/"));
        assert_eq!(docs[0].source_id, "101");
        assert_eq!(docs[1].source_id, "102");
    }

    #[test]
    fn skips_documents_missing_content_field() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
            "results": [
                { "id": 1, "plain_text": "Has content." },
                { "id": 2 },
                { "id": 3, "plain_text": "" },
                { "id": 4, "plain_text": "Also has content." }
            ]
        }"#;
        let path = write_page(dir.path(), "page.json", body);

        let extractor = JsonApiExtractor {
            document_path: "$.results[*]".into(),
            content_field: "plain_text".into(),
            title_field: None,
            url_field: None,
            id_field: Some("id".into()),
        };
        let docs = run(&extractor, &path);

        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].source_id, "1");
        assert_eq!(docs[1].source_id, "4");
    }

    #[test]
    fn id_falls_back_to_stem_and_position() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
            "results": [
                { "plain_text": "alpha" },
                { "plain_text": "beta" }
            ]
        }"#;
        let path = write_page(dir.path(), "abc123.json", body);

        let extractor = JsonApiExtractor {
            document_path: "$.results[*]".into(),
            content_field: "plain_text".into(),
            title_field: None,
            url_field: None,
            id_field: None,
        };
        let docs = run(&extractor, &path);

        assert_eq!(docs[0].source_id, "abc123-0");
        assert_eq!(docs[1].source_id, "abc123-1");
    }

    #[test]
    fn handles_nested_documents_path() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
            "data": {
                "items": [
                    { "id": "x", "body": "deep one" },
                    { "id": "y", "body": "deep two" }
                ]
            }
        }"#;
        let path = write_page(dir.path(), "page.json", body);

        let extractor = JsonApiExtractor {
            document_path: "$.data.items[*]".into(),
            content_field: "body".into(),
            title_field: None,
            url_field: None,
            id_field: Some("id".into()),
        };
        let docs = run(&extractor, &path);

        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].source_id, "x");
        assert_eq!(docs[0].content, "deep one");
    }

    #[test]
    fn metadata_excludes_mapped_fields() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
            "results": [
                {
                    "id": 1,
                    "case_name": "Doe v. Roe",
                    "plain_text": "body",
                    "court": "ca9",
                    "date_filed": "2024-06-01"
                }
            ]
        }"#;
        let path = write_page(dir.path(), "page.json", body);

        let extractor = JsonApiExtractor {
            document_path: "$.results[*]".into(),
            content_field: "plain_text".into(),
            title_field: Some("case_name".into()),
            url_field: None,
            id_field: Some("id".into()),
        };
        let docs = run(&extractor, &path);

        assert_eq!(docs.len(), 1);
        let meta = docs[0].metadata.as_ref().unwrap().as_object().unwrap();
        assert!(meta.contains_key("court"));
        assert!(meta.contains_key("date_filed"));
        assert!(!meta.contains_key("plain_text"));
        assert!(!meta.contains_key("case_name"));
        assert!(!meta.contains_key("id"));
    }

    #[test]
    fn invalid_jsonpath_surfaces_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_page(dir.path(), "page.json", r#"{"results":[]}"#);

        let extractor = JsonApiExtractor {
            document_path: "not a jsonpath".into(),
            content_field: "plain_text".into(),
            title_field: None,
            url_field: None,
            id_field: None,
        };
        let err = extractor.extract(&path).err().expect("should reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("not a valid JSONPath") || msg.contains("JSONPath"),
            "expected JSONPath error, got: {msg}"
        );
    }

    #[test]
    fn empty_array_yields_no_docs() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_page(dir.path(), "page.json", r#"{"results": [], "next": null}"#);
        let extractor = JsonApiExtractor {
            document_path: "$.results[*]".into(),
            content_field: "plain_text".into(),
            title_field: None,
            url_field: None,
            id_field: None,
        };
        let docs = run(&extractor, &path);
        assert!(docs.is_empty());
    }
}
