//! Wrapper extractor that delegates per-file text extraction to a
//! [`CustomExtractorFn`](crate::engine::CustomExtractorFn) registered
//! on the engine at startup.
//!
//! Used by `ExtractorConfig::Custom` so per-format heavy deps
//! (pdf-extract, lopdf, libreoffice, …) live in `sovereign-tools`
//! instead of `corpus-engine`. corpus-engine handles directory walk +
//! sort + emit; the registered closure handles bytes → text.
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use super::{slug, ExtractedDoc, Extractor};
use crate::engine::CustomExtractorFn;
use crate::error::{Error, Result};

/// Resolves matched files to `ExtractedDoc`s using a runtime-registered
/// per-file extractor closure.
pub struct CustomFileExtractor {
    /// File extension to filter for (case-insensitive, no leading dot:
    /// `"pdf"`, `"epub"`, …).
    pub extension: String,
    /// Kind string used to look up the closure; round-tripped into
    /// error messages so a missing-registration surfaces with the
    /// recipe-declared key.
    pub kind: String,
    /// The closure itself, pre-resolved at construction so per-file
    /// extraction is one hashmap miss cheaper.
    pub extractor: CustomExtractorFn,
}

impl Extractor for CustomFileExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let files = collect_files(source_path, &self.extension)?;
        Ok(Box::new(CustomFileIterator {
            files: files.into(),
            extension: self.extension.clone(),
            kind: self.kind.clone(),
            extractor: self.extractor.clone(),
        }))
    }
}

struct CustomFileIterator {
    files: VecDeque<PathBuf>,
    /// Kept for diagnostic context; the directory walk has already
    /// applied the filter so the iterator itself doesn't re-check.
    #[allow(dead_code)]
    extension: String,
    kind: String,
    extractor: CustomExtractorFn,
}

impl Iterator for CustomFileIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let path = self.files.pop_front()?;
            let title = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string());
            let source_id = title
                .as_deref()
                .map(slug)
                .unwrap_or_else(|| "unknown".to_string());
            match (self.extractor)(&path) {
                Ok(text) if text.trim().is_empty() => continue,
                Ok(text) => {
                    return Some(Ok(ExtractedDoc {
                        title,
                        content: text,
                        url: None,
                        source_id,
                        metadata: None,
                        source_file: path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .map(|s| s.to_string()),
                        embed_text: None,
                    }))
                }
                Err(e) => {
                    return Some(Err(Error::Extraction(format!(
                        "custom extractor '{kind}' failed on {p}: {e}",
                        kind = self.kind,
                        p = path.display(),
                    ))))
                }
            }
        }
    }
}

fn collect_files(dir: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    if dir.is_file() {
        return Ok(vec![dir.to_path_buf()]);
    }
    let ext_lower = extension.to_ascii_lowercase();
    let mut files = Vec::new();
    walk(dir, &ext_lower, &mut files)?;
    files.sort();
    Ok(files)
}

fn walk(dir: &Path, ext_lower: &str, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = fs::read_dir(dir)
        .map_err(|e| Error::Extraction(format!("read_dir {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::Extraction(format!("dir entry: {e}")))?;
        let path = entry.path();
        if path.is_dir() {
            walk(&path, ext_lower, out)?;
        } else if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case(ext_lower))
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Arc;

    #[test]
    fn walks_directory_calls_closure_per_matching_file() {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in &[
            ("a.pdf", "ALPHA"),
            ("b.pdf", "BETA"),
            ("ignored.txt", "GAMMA"),
        ] {
            let p = dir.path().join(name);
            std::fs::File::create(&p)
                .unwrap()
                .write_all(body.as_bytes())
                .unwrap();
        }
        let extractor: CustomExtractorFn = Arc::new(|p: &Path| {
            let bytes = std::fs::read(p).map_err(|e| Error::Extraction(e.to_string()))?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        });
        let ex = CustomFileExtractor {
            extension: "pdf".into(),
            kind: "pdf".into(),
            extractor,
        };
        let docs: Vec<_> = ex
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        let mut texts: Vec<_> = docs.iter().map(|d| d.content.clone()).collect();
        texts.sort();
        assert_eq!(texts, vec!["ALPHA".to_string(), "BETA".to_string()]);
        assert!(docs.iter().all(|d| d.title.is_some()));
    }

    #[test]
    fn empty_result_skips_file_not_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::File::create(dir.path().join("blank.pdf"))
            .unwrap()
            .write_all(b"")
            .unwrap();
        let extractor: CustomExtractorFn = Arc::new(|_p: &Path| Ok(String::new()));
        let ex = CustomFileExtractor {
            extension: "pdf".into(),
            kind: "pdf".into(),
            extractor,
        };
        let docs: Vec<_> = ex
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(docs.is_empty(), "empty content must skip, not surface");
    }

    #[test]
    fn closure_error_propagates_with_kind_and_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::File::create(dir.path().join("bad.pdf"))
            .unwrap()
            .write_all(b"")
            .unwrap();
        let extractor: CustomExtractorFn =
            Arc::new(|_p: &Path| Err(Error::Extraction("parse panic".into())));
        let ex = CustomFileExtractor {
            extension: "pdf".into(),
            kind: "pdf".into(),
            extractor,
        };
        let mut iter = ex.extract(dir.path()).unwrap();
        let item = iter.next().expect("one item").unwrap_err();
        let msg = item.to_string();
        assert!(msg.contains("pdf"), "kind should appear: {msg}");
        assert!(msg.contains("bad.pdf"), "path should appear: {msg}");
    }
}
