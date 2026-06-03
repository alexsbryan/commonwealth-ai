use std::path::Path;

use sovereign_core::error::{Error, Result};

/// Parsed document ready for chunking.
pub struct ParsedDocument {
    pub source: String,
    pub content: String,
}

/// Parse a file into text content.
/// Supports: .txt, .md, .markdown, .pdf
/// Returns an error for unsupported formats.
pub fn parse_file(path: &Path) -> Result<ParsedDocument> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let source = path.display().to_string();

    match ext.as_str() {
        "txt" | "text" => {
            let content = std::fs::read_to_string(path)
                .map_err(|e| Error::Storage(format!("Failed to read {source}: {e}")))?;
            Ok(ParsedDocument { source, content })
        }
        "md" | "markdown" => {
            let content = std::fs::read_to_string(path)
                .map_err(|e| Error::Storage(format!("Failed to read {source}: {e}")))?;
            // Strip markdown syntax for cleaner chunks.
            let cleaned = strip_markdown(&content);
            Ok(ParsedDocument {
                source,
                content: cleaned,
            })
        }
        "pdf" => {
            // Route through `safe_extract_pdf_text` so we (a) catch
            // pdf-extract panics on malformed PDFs and (b) silence
            // its raw `println!` glyph diagnostics. Without this the
            // RAG pipeline floods stdout on the first non-trivial
            // font.
            let content =
                crate::local_corpus::extract_stage::safe_extract_pdf_text(path).map_err(|e| {
                    Error::Storage(format!("Failed to extract PDF text from {source}: {e:?}"))
                })?;
            // Clean up common PDF extraction artifacts.
            let cleaned = content
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            Ok(ParsedDocument {
                source,
                content: cleaned,
            })
        }
        _ => Err(Error::InvalidInput(format!(
            "Unsupported file format: .{ext} (supported: .txt, .md, .pdf)"
        ))),
    }
}

/// List parseable files in a directory (non-recursive for now).
pub fn list_parseable_files(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| Error::Storage(format!("Failed to read directory {}: {e}", dir.display())))?;

    let supported = ["txt", "text", "md", "markdown", "pdf"];
    let mut files = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|e| Error::Storage(format!("Directory entry error: {e}")))?;
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if supported.contains(&ext.to_lowercase().as_str()) {
                    files.push(path);
                }
            }
        }
    }

    files.sort();
    Ok(files)
}

/// Basic markdown stripping — removes common syntax while preserving content.
fn strip_markdown(text: &str) -> String {
    let mut result = String::with_capacity(text.len());

    for line in text.lines() {
        let trimmed = line.trim();

        // Skip empty lines (preserve paragraph structure).
        if trimmed.is_empty() {
            result.push('\n');
            continue;
        }

        // Strip heading markers.
        let line = if trimmed.starts_with('#') {
            trimmed.trim_start_matches('#').trim()
        } else {
            trimmed
        };

        // Strip bold/italic markers.
        let line = line.replace("**", "").replace("__", "");

        // Strip inline code backticks.
        let line = line.replace('`', "");

        // Strip link syntax [text](url) → text
        let line = strip_links(&line);

        // Strip image syntax ![alt](url) → alt
        let line = line.replace("![", "[");

        result.push_str(&line);
        result.push('\n');
    }

    result
}

fn strip_links(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '[' {
            // Collect link text.
            let mut link_text = String::new();
            let mut found_close = false;
            for inner in chars.by_ref() {
                if inner == ']' {
                    found_close = true;
                    break;
                }
                link_text.push(inner);
            }
            if found_close && chars.peek() == Some(&'(') {
                // Skip the URL part.
                chars.next(); // consume '('
                for inner in chars.by_ref() {
                    if inner == ')' {
                        break;
                    }
                }
                result.push_str(&link_text);
            } else {
                result.push('[');
                result.push_str(&link_text);
                if found_close {
                    result.push(']');
                }
            }
        } else {
            result.push(ch);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_markdown_headings() {
        let input = "# Title\n## Subtitle\nParagraph text.";
        let result = strip_markdown(input);
        assert!(result.contains("Title"));
        assert!(result.contains("Subtitle"));
        assert!(result.contains("Paragraph text."));
        assert!(!result.contains('#'));
    }

    #[test]
    fn strip_markdown_bold_italic() {
        let input = "This is **bold** and __underline__.";
        let result = strip_markdown(input);
        assert!(result.contains("This is bold and underline."));
    }

    #[test]
    fn strip_markdown_links() {
        let input = "Click [here](https://example.com) for more.";
        let result = strip_markdown(input);
        assert!(result.contains("Click here for more."));
        assert!(!result.contains("https://"));
    }
}
