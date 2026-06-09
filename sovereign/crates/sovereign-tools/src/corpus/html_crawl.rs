// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use sovereign_core::error::{Error, Result};
use sovereign_core::types::DocumentChunk;

use super::{chunk_and_wrap, strip_html, CorpusParser};

pub struct HtmlCrawlParser {
    corpus_id: String,
}

impl HtmlCrawlParser {
    pub fn new(corpus_id: &str) -> Self {
        Self {
            corpus_id: corpus_id.to_string(),
        }
    }
}

impl CorpusParser for HtmlCrawlParser {
    fn parse(&self, source_path: &Path) -> Result<Box<dyn Iterator<Item = Result<DocumentChunk>>>> {
        let files = collect_html_files(source_path)?;
        let label = corpus_label(&self.corpus_id);
        Ok(Box::new(HtmlCrawlIterator {
            files: files.into(),
            corpus_id: self.corpus_id.clone(),
            label,
            pending: VecDeque::new(),
            chunk_counter: 0,
        }))
    }
}

struct HtmlCrawlIterator {
    files: VecDeque<PathBuf>,
    corpus_id: String,
    label: &'static str,
    pending: VecDeque<DocumentChunk>,
    chunk_counter: usize,
}

impl Iterator for HtmlCrawlIterator {
    type Item = Result<DocumentChunk>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(chunk) = self.pending.pop_front() {
                return Some(Ok(chunk));
            }
            let path = self.files.pop_front()?;
            match process_html_file(&self.corpus_id, self.label, &path, &mut self.chunk_counter) {
                Ok(chunks) => {
                    self.pending = chunks.into();
                    continue;
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

fn corpus_label(corpus_id: &str) -> &'static str {
    match corpus_id {
        "sep" => "Stanford Encyclopedia of Philosophy",
        "crs_reports" => "Congressional Research Service",
        _ => "Knowledge Base",
    }
}

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
        .map_err(|e| Error::Storage(format!("Failed to read {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::Storage(format!("Directory entry error: {e}")))?;
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

fn process_html_file(
    corpus_id: &str,
    label: &str,
    path: &Path,
    chunk_counter: &mut usize,
) -> Result<Vec<DocumentChunk>> {
    let raw = fs::read_to_string(path)
        .map_err(|e| Error::Storage(format!("Failed to read {}: {e}", path.display())))?;

    let title = extract_html_title(&raw).unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_string()
    });

    let text = strip_html(&raw);
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }

    let prefixed = format!("{label}: {title}\n\n{text}");
    let source = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    Ok(chunk_and_wrap(corpus_id, &source, &prefixed, chunk_counter))
}

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
    fn parse_html_directory() {
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

        let parser = HtmlCrawlParser::new("sep");
        let chunks: Vec<_> = parser
            .parse(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert!(!chunks.is_empty());
        assert!(chunks[0]
            .content
            .starts_with("Stanford Encyclopedia of Philosophy: Epistemology"));
        assert!(chunks[0].content.contains("study of knowledge"));
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
}
