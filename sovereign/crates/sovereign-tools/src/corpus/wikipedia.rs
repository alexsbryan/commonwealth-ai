use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use bzip2::read::BzDecoder;
use quick_xml::events::Event;
use quick_xml::reader::Reader as XmlReader;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::DocumentChunk;

use super::{chunk_and_wrap, CorpusParser};

pub struct WikimediaDumpParser {
    corpus_id: String,
}

impl WikimediaDumpParser {
    pub fn new(corpus_id: &str) -> Self {
        Self {
            corpus_id: corpus_id.to_string(),
        }
    }
}

impl CorpusParser for WikimediaDumpParser {
    fn parse(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<DocumentChunk>>>> {
        let file = File::open(source_path)
            .map_err(|e| Error::Storage(format!("Failed to open {}: {e}", source_path.display())))?;

        let is_bz2 = source_path
            .extension()
            .and_then(|e| e.to_str())
            .map_or(false, |e| e == "bz2");

        let reader: Box<dyn std::io::Read> = if is_bz2 {
            Box::new(BzDecoder::new(BufReader::new(file)))
        } else {
            Box::new(BufReader::new(file))
        };

        let xml_reader = XmlReader::from_reader(BufReader::new(reader));

        Ok(Box::new(WikiDumpIterator {
            reader: xml_reader,
            buf: Vec::new(),
            corpus_id: self.corpus_id.clone(),
            pending: VecDeque::new(),
            chunk_counter: 0,
            // Page state machine.
            in_page: false,
            current_tag: String::new(),
            current_title: String::new(),
            current_ns: String::new(),
            current_text: String::new(),
        }))
    }
}

struct WikiDumpIterator {
    reader: XmlReader<BufReader<Box<dyn std::io::Read>>>,
    buf: Vec<u8>,
    corpus_id: String,
    pending: VecDeque<DocumentChunk>,
    chunk_counter: usize,
    // State machine for tracking position within <page> elements.
    in_page: bool,
    current_tag: String,
    current_title: String,
    current_ns: String,
    current_text: String,
}

impl Iterator for WikiDumpIterator {
    type Item = Result<DocumentChunk>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(chunk) = self.pending.pop_front() {
                return Some(Ok(chunk));
            }
            match self.read_next_page() {
                Ok(true) => continue,
                Ok(false) => return None, // EOF
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

impl WikiDumpIterator {
    /// Read XML events until a complete page is processed or EOF.
    /// Returns true if a page was processed (chunks may have been added),
    /// false on EOF.
    fn read_next_page(&mut self) -> Result<bool> {
        loop {
            self.buf.clear();
            let event = self
                .reader
                .read_event_into(&mut self.buf)
                .map_err(|e| Error::Storage(format!("XML parse error: {e}")))?;

            match event {
                Event::Start(ref e) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if name == "page" {
                        self.in_page = true;
                        self.current_title.clear();
                        self.current_ns.clear();
                        self.current_text.clear();
                    }
                    if self.in_page {
                        self.current_tag = name;
                    }
                }
                Event::Text(ref e) if self.in_page => {
                    let text = e
                        .unescape()
                        .map_err(|e| Error::Storage(format!("XML unescape error: {e}")))?;
                    match self.current_tag.as_str() {
                        "title" => self.current_title.push_str(&text),
                        "ns" => self.current_ns.push_str(&text),
                        "text" => self.current_text.push_str(&text),
                        _ => {}
                    }
                }
                Event::End(ref e) => {
                    let name = e.name();
                    if name.as_ref() == b"page" && self.in_page {
                        self.in_page = false;
                        self.process_page();
                        return Ok(true);
                    }
                    if self.in_page {
                        self.current_tag.clear();
                    }
                }
                Event::Eof => return Ok(false),
                _ => {}
            }
        }
    }

    fn process_page(&mut self) {
        // Only process main namespace (ns=0).
        if self.current_ns.trim() != "0" {
            return;
        }
        // Skip redirects.
        if self.current_text.trim_start().starts_with("#REDIRECT")
            || self.current_text.trim_start().starts_with("#redirect")
        {
            return;
        }

        let title = self.current_title.trim().to_string();
        if title.is_empty() {
            return;
        }

        let cleaned = strip_mediawiki(&self.current_text);
        if cleaned.trim().is_empty() {
            return;
        }

        // Split by sections and chunk each.
        let sections = split_sections(&cleaned);
        for (section_name, section_text) in sections {
            let text = section_text.trim();
            if text.is_empty() {
                continue;
            }
            let prefix = if section_name.is_empty() {
                format!("Wikipedia: {title}\n\n")
            } else {
                format!("Wikipedia: {title} > {section_name}\n\n")
            };
            let prefixed = format!("{prefix}{text}");
            let source = slug(&title);
            chunk_and_wrap(&self.corpus_id, &source, &prefixed, &mut self.chunk_counter)
                .into_iter()
                .for_each(|c| self.pending.push_back(c));
        }
    }
}

// ─── MediaWiki Markup Stripping ───────────────────────────────

fn strip_mediawiki(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            // Templates: {{...}} — remove entirely (may be nested).
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                skip_nested(&mut chars, '{', '}');
            }
            // Tables: {|...|} — remove entirely.
            '{' if chars.peek() == Some(&'|') => {
                chars.next();
                let mut depth = 1;
                while let Some(c) = chars.next() {
                    if c == '{' && chars.peek() == Some(&'|') {
                        chars.next();
                        depth += 1;
                    } else if c == '|' && chars.peek() == Some(&'}') {
                        chars.next();
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
            }
            // Wikilinks: [[target|display]] -> display, or [[target]] -> target.
            '[' if chars.peek() == Some(&'[') => {
                chars.next();
                let mut link_text = String::new();
                let mut depth = 1;
                while let Some(c) = chars.next() {
                    if c == '[' && chars.peek() == Some(&'[') {
                        chars.next();
                        depth += 1;
                        link_text.push_str("[[");
                    } else if c == ']' && chars.peek() == Some(&']') {
                        chars.next();
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        link_text.push_str("]]");
                    } else {
                        link_text.push(c);
                    }
                }
                // Use display text (after |), or the full link text.
                let display = link_text
                    .rsplit_once('|')
                    .map(|(_, d)| d)
                    .unwrap_or(&link_text);
                // Skip file/image links.
                if !link_text.starts_with("File:")
                    && !link_text.starts_with("Image:")
                    && !link_text.starts_with("Category:")
                {
                    result.push_str(display);
                }
            }
            // External links: [url text] -> text.
            '[' => {
                let mut link = String::new();
                for c in chars.by_ref() {
                    if c == ']' {
                        break;
                    }
                    link.push(c);
                }
                // Display text is everything after the first space.
                if let Some(pos) = link.find(' ') {
                    result.push_str(&link[pos + 1..]);
                }
            }
            // Bold/italic: '''text''' or ''text''.
            '\'' if chars.peek() == Some(&'\'') => {
                // Skip consecutive apostrophes.
                while chars.peek() == Some(&'\'') {
                    chars.next();
                }
            }
            // HTML-like tags: <ref>...</ref>, <nowiki>, etc.
            '<' => {
                let mut tag = String::new();
                let mut is_closing = false;
                for c in chars.by_ref() {
                    if c == '>' {
                        break;
                    }
                    tag.push(c);
                }
                if tag.starts_with('/') {
                    is_closing = true;
                    tag = tag[1..].to_string();
                }
                let tag_name = tag
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_lowercase();
                // For ref, nowiki, gallery, etc. — skip content until closing tag.
                if !is_closing
                    && !tag.ends_with('/')
                    && matches!(
                        tag_name.as_str(),
                        "ref" | "nowiki" | "gallery" | "math" | "source" | "syntaxhighlight" | "code"
                    )
                {
                    let close = format!("</{tag_name}>");
                    let mut buf = String::new();
                    for c in chars.by_ref() {
                        buf.push(c);
                        if buf.ends_with(&close) {
                            break;
                        }
                    }
                }
            }
            // Section headers: == Title == -> preserved as text.
            '=' if result.ends_with('\n') || result.is_empty() => {
                // Already handled by split_sections; just pass through.
                result.push(ch);
            }
            _ => result.push(ch),
        }
    }

    result
}

/// Skip nested pairs, e.g., {{ ... {{ ... }} ... }}.
fn skip_nested(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, open: char, close: char) {
    let mut depth = 1;
    while let Some(c) = chars.next() {
        if c == open && chars.peek() == Some(&open) {
            chars.next();
            depth += 1;
        } else if c == close && chars.peek() == Some(&close) {
            chars.next();
            depth -= 1;
            if depth == 0 {
                return;
            }
        }
    }
}

/// Split article text by section headers (== ... ==).
/// Returns Vec of (section_name, section_text).
/// The lead section has an empty name.
fn split_sections(text: &str) -> Vec<(String, String)> {
    let mut sections = Vec::new();
    let mut current_name = String::new();
    let mut current_text = String::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("==") && trimmed.ends_with("==") {
            // Save previous section.
            if !current_text.is_empty() || sections.is_empty() {
                sections.push((current_name, current_text));
            }
            current_name = trimmed
                .trim_matches('=')
                .trim()
                .to_string();
            current_text = String::new();
        } else {
            current_text.push_str(line);
            current_text.push('\n');
        }
    }
    // Save final section.
    if !current_text.is_empty() {
        sections.push((current_name, current_text));
    }
    sections
}

fn slug(title: &str) -> String {
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

    fn make_test_xml() -> String {
        r#"<mediawiki>
  <page>
    <title>Rust (programming language)</title>
    <ns>0</ns>
    <revision>
      <text>'''Rust''' is a [[programming language]] focused on safety and performance.

== History ==

Rust was originally designed by Graydon Hoare at [[Mozilla]].

== Features ==

Rust has a strong [[type system]] and [[ownership (computer science)|ownership]] model.

{{Infobox programming language
|name = Rust
}}
</text>
    </revision>
  </page>
  <page>
    <title>Talk:Rust</title>
    <ns>1</ns>
    <revision>
      <text>This is a talk page and should be skipped.</text>
    </revision>
  </page>
  <page>
    <title>Redirect Page</title>
    <ns>0</ns>
    <revision>
      <text>#REDIRECT [[Rust (programming language)]]</text>
    </revision>
  </page>
  <page>
    <title>Python (programming language)</title>
    <ns>0</ns>
    <revision>
      <text>'''Python''' is a high-level [[programming language]].

== Design philosophy ==

Python emphasizes code readability.
</text>
    </revision>
  </page>
</mediawiki>"#
            .to_string()
    }

    #[test]
    fn parse_xml_dump() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("dump.xml");
        let mut f = File::create(&file_path).unwrap();
        f.write_all(make_test_xml().as_bytes()).unwrap();

        let parser = WikimediaDumpParser::new("wikipedia");
        let chunks: Vec<_> = parser
            .parse(&file_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        // Should have chunks for Rust and Python, not Talk or Redirect.
        assert!(chunks.len() >= 2);

        let all_content: String = chunks.iter().map(|c| c.content.clone()).collect();
        assert!(all_content.contains("Wikipedia: Rust (programming language)"));
        assert!(all_content.contains("Wikipedia: Python (programming language)"));
        assert!(!all_content.contains("Talk:Rust"));
        assert!(!all_content.contains("#REDIRECT"));
    }

    #[test]
    fn parse_bz2_dump() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("dump.xml.bz2");
        let f = File::create(&file_path).unwrap();
        let mut encoder = bzip2::write::BzEncoder::new(f, bzip2::Compression::fast());
        encoder.write_all(make_test_xml().as_bytes()).unwrap();
        encoder.finish().unwrap();

        let parser = WikimediaDumpParser::new("wikipedia");
        let chunks: Vec<_> = parser
            .parse(&file_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert!(chunks.len() >= 2);
        let all_content: String = chunks.iter().map(|c| c.content.clone()).collect();
        assert!(all_content.contains("Wikipedia: Rust (programming language)"));
    }

    #[test]
    fn strip_mediawiki_templates() {
        let text = "Before {{Infobox|name=Test}} after.";
        let result = strip_mediawiki(text);
        assert!(result.contains("Before"));
        assert!(result.contains("after."));
        assert!(!result.contains("Infobox"));
    }

    #[test]
    fn strip_mediawiki_wikilinks() {
        let text = "A [[programming language]] and [[Rust (lang)|Rust]].";
        let result = strip_mediawiki(text);
        assert!(result.contains("programming language"));
        assert!(result.contains("Rust"));
        assert!(!result.contains("[["));
    }

    #[test]
    fn strip_mediawiki_bold_italic() {
        let text = "'''Bold''' and ''italic'' text.";
        let result = strip_mediawiki(text);
        assert!(result.contains("Bold"));
        assert!(result.contains("italic"));
        assert!(!result.contains("'''"));
    }

    #[test]
    fn split_sections_works() {
        let text = "Lead text.\n== History ==\nHistory text.\n== Features ==\nFeature text.\n";
        let sections = split_sections(text);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].0, "");
        assert!(sections[0].1.contains("Lead text"));
        assert_eq!(sections[1].0, "History");
        assert_eq!(sections[2].0, "Features");
    }
}
