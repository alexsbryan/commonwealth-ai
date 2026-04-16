use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use super::{slug, ExtractedDoc, Extractor};

/// Plaintext file extractor.
///
/// Extracts text files from a directory, optionally stripping boilerplate
/// (e.g., Project Gutenberg headers/footers).
pub struct PlaintextExtractor {
    /// Pattern to look for in the first 50 lines to extract a title
    /// (e.g., "Title: "). Falls back to filename.
    pub title_pattern: Option<String>,
    /// Boilerplate stripping mode (e.g., "gutenberg").
    pub strip_boilerplate: Option<String>,
}

impl PlaintextExtractor {
    pub fn new(title_pattern: Option<String>, strip_boilerplate: Option<String>) -> Self {
        Self {
            title_pattern,
            strip_boilerplate,
        }
    }
}

impl Extractor for PlaintextExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let files = collect_text_files(source_path)?;
        Ok(Box::new(PlaintextIterator {
            files: files.into(),
            title_pattern: self.title_pattern.clone(),
            strip_boilerplate: self.strip_boilerplate.clone(),
        }))
    }
}

struct PlaintextIterator {
    files: VecDeque<PathBuf>,
    title_pattern: Option<String>,
    strip_boilerplate: Option<String>,
}

impl Iterator for PlaintextIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let path = self.files.pop_front()?;
            match process_text_file(&path, &self.title_pattern, &self.strip_boilerplate) {
                Ok(Some(doc)) => return Some(Ok(doc)),
                Ok(None) => continue, // empty file, skip
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

fn process_text_file(
    path: &Path,
    title_pattern: &Option<String>,
    strip_boilerplate: &Option<String>,
) -> Result<Option<ExtractedDoc>> {
    let raw = fs::read_to_string(path)
        .map_err(|e| Error::Extraction(format!("Failed to read {}: {e}", path.display())))?;

    let title = extract_title(&raw, path, title_pattern.as_deref());
    let body = match strip_boilerplate.as_deref() {
        Some("gutenberg") => strip_gutenberg_boilerplate(&raw),
        _ => raw.clone(),
    };

    let body = body.trim().to_string();
    if body.is_empty() {
        return Ok(None);
    }

    Ok(Some(ExtractedDoc {
        title: Some(title.clone()),
        content: body,
        url: None,
        source_id: slug(&title),
        metadata: None,
        source_file: None,
    }))
}

/// Recursively collect .txt files from a directory.
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
        .map_err(|e| Error::Extraction(format!("Failed to read {}: {e}", dir.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| Error::Extraction(format!("Directory entry error: {e}")))?;
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

/// Extract title from the text. Looks for the title_pattern (e.g., "Title: ")
/// in the first 50 lines, falling back to the filename.
fn extract_title(text: &str, path: &Path, title_pattern: Option<&str>) -> String {
    let pattern = title_pattern.unwrap_or("Title: ");
    for line in text.lines().take(50) {
        if let Some(rest) = line.strip_prefix(pattern) {
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

/// Strip Project Gutenberg boilerplate between START/END markers.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn extract_gutenberg_text() {
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
             This is the first chapter of the book.\r\n\
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

        let extractor = PlaintextExtractor::new(None, Some("gutenberg".to_string()));
        let docs: Vec<_> = extractor
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert!(!docs.is_empty());
        assert_eq!(docs[0].title.as_deref(), Some("Test Book"));
        assert!(docs[0].content.contains("Chapter 1"));
        assert!(!docs[0].content.contains("Gutenberg license info"));
    }

    #[test]
    fn extract_title_from_header() {
        let text = "Heading\nTitle: Moby Dick\nAuthor: Melville\n";
        assert_eq!(
            extract_title(text, Path::new("moby.txt"), Some("Title: ")),
            "Moby Dick"
        );
    }

    #[test]
    fn extract_title_fallback_to_filename() {
        let text = "No title header here.\n";
        assert_eq!(
            extract_title(text, Path::new("moby-dick.txt"), Some("Title: ")),
            "moby-dick"
        );
    }

    #[test]
    fn strip_gutenberg_markers() {
        let text = "Before\n*** START OF THE PROJECT GUTENBERG EBOOK ***\nGood content\n*** END OF THE PROJECT GUTENBERG EBOOK ***\nAfter";
        let result = strip_gutenberg_boilerplate(text);
        assert!(result.contains("Good content"));
        assert!(!result.contains("Before"));
        assert!(!result.contains("After"));
    }

    #[test]
    fn plain_text_no_stripping() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("note.txt");
        fs::write(&file_path, "Just plain text.").unwrap();

        let extractor = PlaintextExtractor::new(None, None);
        let docs: Vec<_> = extractor
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].content, "Just plain text.");
        assert_eq!(docs[0].title.as_deref(), Some("note"));
    }

    #[test]
    fn collect_recursive_txt() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("subdir");
        fs::create_dir(&sub).unwrap();
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        fs::write(sub.join("b.txt"), "b").unwrap();
        fs::write(dir.path().join("c.md"), "c").unwrap(); // not .txt

        let files = collect_text_files(dir.path()).unwrap();
        assert_eq!(files.len(), 2);
    }
}
