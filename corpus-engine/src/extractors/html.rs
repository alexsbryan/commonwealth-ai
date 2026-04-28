use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use super::{slug, strip_html, ExtractedDoc, Extractor};

/// HTML file extractor.
///
/// Extracts HTML files from a directory, strips tags, and produces documents.
pub struct HtmlExtractor {
    /// CSS selector for the main content area (not used for simple extraction,
    /// reserved for future `scraper`-based extraction).
    pub content_selector: Option<String>,
    /// CSS selector for the title element.
    pub title_selector: Option<String>,
    /// Label to prepend to extracted documents (e.g., "Stanford Encyclopedia of Philosophy").
    pub label: String,
}

impl HtmlExtractor {
    pub fn new(label: &str) -> Self {
        Self {
            content_selector: None,
            title_selector: None,
            label: label.to_string(),
        }
    }

    pub fn with_selectors(
        label: &str,
        content_selector: Option<String>,
        title_selector: Option<String>,
    ) -> Self {
        Self {
            content_selector,
            title_selector,
            label: label.to_string(),
        }
    }
}

impl Extractor for HtmlExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let files = collect_html_files(source_path)?;
        Ok(Box::new(HtmlIterator {
            files: files.into(),
            label: self.label.clone(),
        }))
    }
}

struct HtmlIterator {
    files: VecDeque<PathBuf>,
    label: String,
}

impl Iterator for HtmlIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let path = self.files.pop_front()?;
            match process_html_file(&self.label, &path) {
                Ok(Some(doc)) => return Some(Ok(doc)),
                Ok(None) => continue,
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

fn process_html_file(label: &str, path: &Path) -> Result<Option<ExtractedDoc>> {
    let raw = fs::read_to_string(path)
        .map_err(|e| Error::Extraction(format!("Failed to read {}: {e}", path.display())))?;

    let title = extract_html_title(&raw).unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_string()
    });

    let text = strip_html(&raw);
    if text.trim().is_empty() {
        return Ok(None);
    }

    let source_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| slug(s))
        .unwrap_or_else(|| "unknown".to_string());

    Ok(Some(ExtractedDoc {
        title: Some(title),
        content: text,
        url: None,
        source_id,
        metadata: Some(serde_json::json!({ "label": label })),
        source_file: None,
        embed_text: None,
    }))
}

/// Recursively collect .html/.htm files from a directory.
fn collect_html_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if dir.is_file() {
        return Ok(vec![dir.to_path_buf()]);
    }
    let mut files = Vec::new();
    collect_recursive(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let entries = fs::read_dir(dir)
        .map_err(|e| Error::Extraction(format!("Failed to read {}: {e}", dir.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| Error::Extraction(format!("Directory entry error: {e}")))?;
        let path = entry.path();
        if path.is_dir() {
            collect_recursive(&path, files)?;
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm") {
                    files.push(path);
                }
            }
        }
    }
    Ok(())
}

/// Extract the title from an HTML document by finding the <title> tag.
fn extract_html_title(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let start = lower.find("<title>")? + 7;
    let end = lower[start..].find("</title>")? + start;
    let raw = &html[start..end];
    let title = raw.trim().to_string();
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn extract_html_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("epistemology.html");
        let mut f = fs::File::create(&file_path).unwrap();
        write!(
            f,
            "<html><head><title>Epistemology</title></head>\
             <body><nav>Skip this nav</nav>\
             <article><p>Epistemology is the study of knowledge.</p>\
             <p>It addresses questions about the nature of knowledge.</p></article>\
             </body></html>"
        )
        .unwrap();

        let extractor = HtmlExtractor::new("Stanford Encyclopedia of Philosophy");
        let docs: Vec<_> = extractor
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert!(!docs.is_empty());
        assert_eq!(docs[0].title.as_deref(), Some("Epistemology"));
        assert!(docs[0].content.contains("study of knowledge"));
    }

    #[test]
    fn extract_title_from_html() {
        let html = "<html><head><title>Test Title</title></head><body></body></html>";
        assert_eq!(extract_html_title(html), Some("Test Title".to_string()));
    }

    #[test]
    fn extract_title_missing() {
        let html = "<html><head></head><body>content</body></html>";
        assert_eq!(extract_html_title(html), None);
    }

    #[test]
    fn collect_html_and_htm() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.html"), "<p>a</p>").unwrap();
        fs::write(dir.path().join("b.htm"), "<p>b</p>").unwrap();
        fs::write(dir.path().join("c.txt"), "not html").unwrap();

        let files = collect_html_files(dir.path()).unwrap();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn empty_html_skipped() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("empty.html"),
            "<html><head></head><body></body></html>",
        )
        .unwrap();

        let extractor = HtmlExtractor::new("Test");
        let docs: Vec<_> = extractor
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(docs.is_empty());
    }
}
