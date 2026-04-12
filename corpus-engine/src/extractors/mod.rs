pub mod json;
pub mod xml;
pub mod html;
pub mod csv;
pub mod parquet;
pub mod plaintext;
pub mod wikipedia_structured;
pub mod wikipedia_jsonl;
pub mod wikipedia_types;

#[cfg(feature = "treesitter")]
pub mod code;

use crate::error::Result;

/// A raw document extracted from a source, before chunking.
#[derive(Debug, Clone)]
pub struct ExtractedDoc {
    pub title: Option<String>,
    pub content: String,
    pub url: Option<String>,
    pub source_id: String,
    pub metadata: Option<serde_json::Value>,
}

/// Trait for extracting documents from source data.
pub trait Extractor: Send + Sync {
    /// Parse the source and return an iterator of extracted documents.
    /// The iterator must be `Send` so the engine can hold it across
    /// `.await` points inside `tokio::spawn`-ed ingest tasks.
    fn extract(
        &self,
        source_path: &std::path::Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>>;
}

// ─── Shared Utilities ─────────────────────────────────────────

/// Convert a title or label into a URL-safe slug.
pub(crate) fn slug(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Strip HTML tags and decode common entities.
pub(crate) fn strip_html(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut tag_name = String::new();
    let mut collecting_tag_name = false;

    let mut chars = html.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '<' {
            in_tag = true;
            collecting_tag_name = true;
            tag_name.clear();
            continue;
        }
        if in_tag {
            if collecting_tag_name {
                // Include '/' as part of the tag name for closing tags like </script>.
                if ch == '/' && tag_name.is_empty() {
                    tag_name.push(ch);
                    continue;
                }
                if ch.is_ascii_whitespace() || ch == '>' || ch == '/' {
                    collecting_tag_name = false;
                    let lower = tag_name.to_lowercase();
                    if lower == "script" {
                        in_script = true;
                    } else if lower == "/script" {
                        in_script = false;
                    } else if lower == "style" {
                        in_style = true;
                    } else if lower == "/style" {
                        in_style = false;
                    } else if lower == "br" || lower == "br/" {
                        result.push('\n');
                    } else if lower == "p"
                        || lower == "/p"
                        || lower == "div"
                        || lower == "/div"
                    {
                        if !result.ends_with('\n') {
                            result.push('\n');
                        }
                    }
                } else {
                    tag_name.push(ch);
                }
            }
            if ch == '>' {
                in_tag = false;
            }
            continue;
        }
        if in_script || in_style {
            continue;
        }
        if ch == '&' {
            let mut entity = String::new();
            for ec in chars.by_ref() {
                if ec == ';' {
                    break;
                }
                entity.push(ec);
                if entity.len() > 10 {
                    break;
                }
            }
            match entity.as_str() {
                "amp" => result.push('&'),
                "lt" => result.push('<'),
                "gt" => result.push('>'),
                "quot" => result.push('"'),
                "apos" => result.push('\''),
                "nbsp" => result.push(' '),
                s if s.starts_with('#') => {
                    let num_str = &s[1..];
                    let code = if let Some(hex) = num_str.strip_prefix('x') {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        num_str.parse::<u32>().ok()
                    };
                    if let Some(c) = code.and_then(char::from_u32) {
                        result.push(c);
                    }
                }
                _ => {
                    result.push('&');
                    result.push_str(&entity);
                    result.push(';');
                }
            }
            continue;
        }
        result.push(ch);
    }

    // Collapse excessive whitespace.
    let mut collapsed = String::with_capacity(result.len());
    let mut prev_newline = false;
    for line in result.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !prev_newline {
                collapsed.push('\n');
                prev_newline = true;
            }
        } else {
            collapsed.push_str(trimmed);
            collapsed.push('\n');
            prev_newline = false;
        }
    }

    collapsed.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_basic() {
        assert_eq!(slug("Hello World"), "hello-world");
        assert_eq!(slug("Test 123!"), "test-123");
        assert_eq!(slug("a--b"), "a-b");
    }

    #[test]
    fn strip_html_basic() {
        assert_eq!(strip_html("<p>Hello</p>"), "Hello");
    }

    #[test]
    fn strip_html_entities() {
        assert_eq!(strip_html("a &amp; b &lt; c"), "a & b < c");
    }

    #[test]
    fn strip_html_script() {
        let html = "before<script>var x = 1;</script>after";
        let result = strip_html(html);
        assert!(result.contains("before"));
        assert!(result.contains("after"));
        assert!(!result.contains("var x"));
    }

    #[test]
    fn strip_html_numeric_entities() {
        assert_eq!(strip_html("&#65;&#x42;"), "AB");
    }
}
