use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use sovereign_core::error::{Error, Result};
use sovereign_core::types::DocumentChunk;

use super::{chunk_and_wrap, CorpusParser};

pub struct GutenbergParser {
    corpus_id: String,
}

impl GutenbergParser {
    pub fn new(corpus_id: &str) -> Self {
        Self {
            corpus_id: corpus_id.to_string(),
        }
    }
}

impl CorpusParser for GutenbergParser {
    fn parse(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<DocumentChunk>>>> {
        let files = collect_text_files(source_path)?;
        Ok(Box::new(GutenbergIterator {
            files: files.into(),
            corpus_id: self.corpus_id.clone(),
            pending: VecDeque::new(),
        }))
    }
}

struct GutenbergIterator {
    files: VecDeque<PathBuf>,
    corpus_id: String,
    pending: VecDeque<DocumentChunk>,
}

impl Iterator for GutenbergIterator {
    type Item = Result<DocumentChunk>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(chunk) = self.pending.pop_front() {
                return Some(Ok(chunk));
            }
            let path = self.files.pop_front()?;
            match process_gutenberg_file(&self.corpus_id, &path) {
                Ok(chunks) => {
                    self.pending = chunks.into();
                    continue;
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

fn collect_text_files(dir: &Path) -> Result<Vec<PathBuf>> {
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
        let entry =
            entry.map_err(|e| Error::Storage(format!("Directory entry error: {e}")))?;
        let path = entry.path();
        if path.is_dir() {
            collect_recursive(&path, files)?;
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if ext.eq_ignore_ascii_case("txt") {
                    files.push(path);
                }
            }
        }
    }
    Ok(())
}

fn process_gutenberg_file(corpus_id: &str, path: &Path) -> Result<Vec<DocumentChunk>> {
    let raw = fs::read_to_string(path)
        .map_err(|e| Error::Storage(format!("Failed to read {}: {e}", path.display())))?;

    let title = extract_title(&raw, path);
    let body = strip_gutenberg_boilerplate(&raw);

    let prefixed = format!("Project Gutenberg: {title}\n\n{body}");
    let source = slug_from_title(&title);
    let mut idx = 0;
    Ok(chunk_and_wrap(corpus_id, &source, &prefixed, &mut idx))
}

fn extract_title(text: &str, path: &Path) -> String {
    for line in text.lines().take(50) {
        if let Some(rest) = line.strip_prefix("Title: ") {
            let t = rest.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    // Fallback to filename.
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn strip_gutenberg_boilerplate(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.contains("*** START OF") && l.contains("PROJECT GUTENBERG"))
        .map(|i| i + 1)
        .unwrap_or(0);
    let end = lines
        .iter()
        .rposition(|l| l.contains("*** END OF") && l.contains("PROJECT GUTENBERG"))
        .unwrap_or(lines.len());
    lines[start..end].join("\n")
}

fn slug_from_title(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_gutenberg_text() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("pg1234.txt");
        let mut f = fs::File::create(&file_path).unwrap();
        write!(
            f,
            "The Project Gutenberg eBook of Test Book\r\n\
             Title: Test Book\r\n\
             Author: Test Author\r\n\
             \r\n\
             *** START OF THE PROJECT GUTENBERG EBOOK TEST BOOK ***\r\n\
             \r\n\
             Chapter 1\r\n\
             \r\n\
             This is the first chapter of the book with enough text to be meaningful.\r\n\
             \r\n\
             Chapter 2\r\n\
             \r\n\
             This is the second chapter.\r\n\
             \r\n\
             *** END OF THE PROJECT GUTENBERG EBOOK TEST BOOK ***\r\n\
             \r\n\
             Gutenberg license info here.\r\n"
        )
        .unwrap();

        let parser = GutenbergParser::new("gutenberg");
        let chunks: Vec<_> = parser
            .parse(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert!(!chunks.is_empty());
        assert!(chunks[0].content.starts_with("Project Gutenberg: Test Book"));
        assert_eq!(
            chunks[0].source_type,
            sovereign_core::types::SourceType::Corpus {
                corpus_id: "gutenberg".to_string()
            }
        );
        // Should not contain boilerplate.
        for chunk in &chunks {
            assert!(!chunk.content.contains("Gutenberg license info"));
        }
    }

    #[test]
    fn extract_title_from_header() {
        let text = "Heading\nTitle: Moby Dick\nAuthor: Melville\n";
        assert_eq!(
            extract_title(text, Path::new("moby.txt")),
            "Moby Dick"
        );
    }

    #[test]
    fn extract_title_fallback_to_filename() {
        let text = "No title header here.\n";
        assert_eq!(
            extract_title(text, Path::new("moby-dick.txt")),
            "moby-dick"
        );
    }
}
