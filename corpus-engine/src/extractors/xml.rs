use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use quick_xml::events::Event;
use quick_xml::reader::Reader as XmlReader;

use crate::error::{Error, Result};
use super::{slug, strip_html, ExtractedDoc, Extractor};

// ═══════════════════════════════════════════════════════════════
// MediaWiki XML Extractor
// ═══════════════════════════════════════════════════════════════

/// Extracts articles from MediaWiki XML dump files (e.g., Wikipedia).
pub struct MediawikiExtractor {
    /// Namespace IDs to include (default: [0] for main namespace).
    pub namespace_filter: Vec<u32>,
    /// Whether to skip redirect pages.
    pub skip_redirects: bool,
    /// Decompression mode: Some("bzip2") or None for plain XML.
    pub decompress: Option<String>,
}

impl MediawikiExtractor {
    pub fn new() -> Self {
        Self {
            namespace_filter: vec![0],
            skip_redirects: true,
            decompress: None,
        }
    }

    pub fn with_decompress(mut self, decompress: Option<String>) -> Self {
        self.decompress = decompress;
        self
    }
}

impl Default for MediawikiExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for MediawikiExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>>>> {
        let file = File::open(source_path)
            .map_err(|e| Error::Extraction(format!("Failed to open {}: {e}", source_path.display())))?;

        let is_bz2 = self.decompress.as_deref() == Some("bzip2")
            || source_path
                .extension()
                .and_then(|e| e.to_str())
                .map_or(false, |e| e == "bz2");

        let reader: Box<dyn std::io::Read> = if is_bz2 {
            Box::new(bzip2::read::BzDecoder::new(BufReader::new(file)))
        } else {
            Box::new(BufReader::new(file))
        };

        let xml_reader = XmlReader::from_reader(BufReader::new(reader));

        Ok(Box::new(WikiDumpIterator {
            reader: xml_reader,
            buf: Vec::new(),
            namespace_filter: self.namespace_filter.clone(),
            skip_redirects: self.skip_redirects,
            pending: VecDeque::new(),
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
    namespace_filter: Vec<u32>,
    skip_redirects: bool,
    pending: VecDeque<ExtractedDoc>,
    // State machine for tracking position within <page> elements.
    in_page: bool,
    current_tag: String,
    current_title: String,
    current_ns: String,
    current_text: String,
}

impl Iterator for WikiDumpIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(doc) = self.pending.pop_front() {
                return Some(Ok(doc));
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
    fn read_next_page(&mut self) -> Result<bool> {
        loop {
            self.buf.clear();
            let event = self
                .reader
                .read_event_into(&mut self.buf)
                .map_err(|e| Error::Extraction(format!("XML parse error: {e}")))?;

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
                        .map_err(|e| Error::Extraction(format!("XML unescape error: {e}")))?;
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
        // Check namespace filter.
        let ns: u32 = self.current_ns.trim().parse().unwrap_or(u32::MAX);
        if !self.namespace_filter.contains(&ns) {
            return;
        }
        // Skip redirects.
        if self.skip_redirects {
            let trimmed = self.current_text.trim_start();
            if trimmed.starts_with("#REDIRECT") || trimmed.starts_with("#redirect") {
                return;
            }
        }

        let title = self.current_title.trim().to_string();
        if title.is_empty() {
            return;
        }

        let cleaned = strip_mediawiki(&self.current_text);
        if cleaned.trim().is_empty() {
            return;
        }

        // Split by sections and emit a document for each section.
        let sections = split_sections(&cleaned);
        for (section_name, section_text) in sections {
            let text = section_text.trim();
            if text.is_empty() {
                continue;
            }

            let doc_title = if section_name.is_empty() {
                title.clone()
            } else {
                format!("{title} > {section_name}")
            };

            self.pending.push_back(ExtractedDoc {
                title: Some(doc_title),
                content: text.to_string(),
                url: None,
                source_id: slug(&title),
                metadata: None,
            });
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// StackExchange XML Extractor
// ═══════════════════════════════════════════════════════════════

/// Extracts Q&A pairs from StackExchange XML data dumps.
pub struct StackExchangeExtractor {
    /// Minimum answer score to include.
    pub min_score: i32,
}

impl StackExchangeExtractor {
    pub fn new(min_score: i32) -> Self {
        Self { min_score }
    }
}

impl Default for StackExchangeExtractor {
    fn default() -> Self {
        Self { min_score: 3 }
    }
}

impl Extractor for StackExchangeExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>>>> {
        let files = find_posts_files(source_path)?;
        Ok(Box::new(StackExchangeIterator {
            files: files.into(),
            min_score: self.min_score,
            current: None,
            pending: VecDeque::new(),
        }))
    }
}

fn find_posts_files(path: &Path) -> Result<Vec<PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut files = Vec::new();
    let entries = std::fs::read_dir(path)
        .map_err(|e| Error::Extraction(format!("Failed to read {}: {e}", path.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| Error::Extraction(format!("Directory entry error: {e}")))?;
        let p = entry.path();
        if p.is_dir() {
            let posts = p.join("Posts.xml");
            if posts.is_file() {
                files.push(posts);
            }
        } else if p.is_file()
            && p.file_name()
                .and_then(|n| n.to_str())
                .map_or(false, |n| n.eq_ignore_ascii_case("Posts.xml"))
        {
            files.push(p);
        }
    }
    // Also check direct Posts.xml in the given path.
    let direct = path.join("Posts.xml");
    if direct.is_file() && !files.contains(&direct) {
        files.push(direct);
    }
    files.sort();
    Ok(files)
}

struct StackExchangeIterator {
    files: VecDeque<PathBuf>,
    min_score: i32,
    current: Option<CommunityParser>,
    pending: VecDeque<ExtractedDoc>,
}

struct CommunityParser {
    reader: XmlReader<BufReader<File>>,
    buf: Vec<u8>,
    community: String,
    /// Maps question ID -> (title, body_text).
    questions: HashMap<u64, (String, String)>,
}

impl Iterator for StackExchangeIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(doc) = self.pending.pop_front() {
                return Some(Ok(doc));
            }

            if let Some(ref mut cp) = self.current {
                match read_next_answer(cp, self.min_score) {
                    Ok(Some((title, question_body, answer_body, score, community))) => {
                        let content = format!(
                            "Q: {question_body}\n\nA (score {score}): {answer_body}",
                        );
                        let source_id = slug(&format!("{community}-{title}"));
                        self.pending.push_back(ExtractedDoc {
                            title: Some(title),
                            content,
                            url: None,
                            source_id,
                            metadata: Some(serde_json::json!({
                                "community": community,
                                "score": score,
                            })),
                        });
                        continue;
                    }
                    Ok(None) => {
                        self.current = None;
                    }
                    Err(e) => return Some(Err(e)),
                }
            }

            let path = self.files.pop_front()?;
            let community = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();
            match File::open(&path) {
                Ok(file) => {
                    let reader = XmlReader::from_reader(BufReader::new(file));
                    self.current = Some(CommunityParser {
                        reader,
                        buf: Vec::new(),
                        community,
                        questions: HashMap::new(),
                    });
                }
                Err(e) => {
                    return Some(Err(Error::Extraction(format!(
                        "Failed to open {}: {e}",
                        path.display()
                    ))));
                }
            }
        }
    }
}

/// Read the next high-scoring answer from the XML.
/// Returns (question_title, question_body, answer_body, score, community).
fn read_next_answer(
    cp: &mut CommunityParser,
    min_score: i32,
) -> Result<Option<(String, String, String, i32, String)>> {
    loop {
        cp.buf.clear();
        let event = cp
            .reader
            .read_event_into(&mut cp.buf)
            .map_err(|e| Error::Extraction(format!("XML parse error: {e}")))?;

        match event {
            Event::Empty(ref e) | Event::Start(ref e) if e.name().as_ref() == b"row" => {
                let attrs = parse_row_attrs(e)?;
                let post_type = attrs.get("PostTypeId").and_then(|s| s.parse::<u8>().ok());

                match post_type {
                    Some(1) => {
                        // Question: store for later lookup.
                        if let Some(id_str) = attrs.get("Id") {
                            if let Ok(id) = id_str.parse::<u64>() {
                                let title = attrs
                                    .get("Title")
                                    .cloned()
                                    .unwrap_or_default();
                                let body = attrs
                                    .get("Body")
                                    .map(|b| strip_html(b))
                                    .unwrap_or_default();
                                cp.questions.insert(id, (title, body));
                            }
                        }
                    }
                    Some(2) => {
                        // Answer: check score and yield if above threshold.
                        let score = attrs
                            .get("Score")
                            .and_then(|s| s.parse::<i32>().ok())
                            .unwrap_or(0);
                        if score < min_score {
                            continue;
                        }
                        let parent_id = attrs
                            .get("ParentId")
                            .and_then(|s| s.parse::<u64>().ok());
                        let (title, q_body) = parent_id
                            .and_then(|pid| cp.questions.get(&pid))
                            .cloned()
                            .unwrap_or_else(|| ("Unknown Question".to_string(), String::new()));
                        let a_body = attrs
                            .get("Body")
                            .map(|b| strip_html(b))
                            .unwrap_or_default();
                        return Ok(Some((title, q_body, a_body, score, cp.community.clone())));
                    }
                    _ => {}
                }
            }
            Event::Eof => return Ok(None),
            _ => {}
        }
    }
}

/// Parse attributes from a <row> element into a HashMap.
fn parse_row_attrs(
    e: &quick_xml::events::BytesStart<'_>,
) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for attr in e.attributes() {
        let attr =
            attr.map_err(|e| Error::Extraction(format!("XML attribute error: {e}")))?;
        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
        let val = attr
            .unescape_value()
            .map_err(|e| Error::Extraction(format!("XML unescape error: {e}")))?
            .to_string();
        map.insert(key, val);
    }
    Ok(map)
}

// ─── MediaWiki Markup Stripping ───────────────────────────────

/// Strip MediaWiki markup, producing plain text.
pub(crate) fn strip_mediawiki(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            // Templates: {{...}} -- remove entirely (may be nested).
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                skip_nested(&mut chars, '{', '}');
            }
            // Tables: {|...|} -- remove entirely.
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
                // For ref, nowiki, gallery, etc. -- skip content until closing tag.
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
pub(crate) fn split_sections(text: &str) -> Vec<(String, String)> {
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
            current_name = trimmed.trim_matches('=').trim().to_string();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ─── MediaWiki Tests ──────────────────────────────────────

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

        let extractor = MediawikiExtractor::new();
        let docs: Vec<_> = extractor
            .extract(&file_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        // Should have docs for Rust and Python, not Talk or Redirect.
        assert!(docs.len() >= 2);

        let all_titles: Vec<_> = docs
            .iter()
            .filter_map(|d| d.title.as_ref())
            .collect();
        let all_content: String = docs.iter().map(|d| d.content.clone()).collect();

        assert!(all_titles.iter().any(|t| t.contains("Rust")));
        assert!(all_titles.iter().any(|t| t.contains("Python")));
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

        let extractor = MediawikiExtractor::new().with_decompress(Some("bzip2".to_string()));
        let docs: Vec<_> = extractor
            .extract(&file_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert!(docs.len() >= 2);
        let all_titles: Vec<_> = docs.iter().filter_map(|d| d.title.as_ref()).collect();
        assert!(all_titles.iter().any(|t| t.contains("Rust")));
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

    // ─── StackExchange Tests ──────────────────────────────────

    fn make_se_test_xml(dir: &Path) -> PathBuf {
        let posts_path = dir.join("Posts.xml");
        let mut f = File::create(&posts_path).unwrap();
        write!(
            f,
            r#"<?xml version="1.0" encoding="utf-8"?>
<posts>
  <row Id="1" PostTypeId="1" Score="10" Title="How to sort a list?" Body="&lt;p&gt;How do I sort a list in Python?&lt;/p&gt;" />
  <row Id="2" PostTypeId="2" ParentId="1" Score="15" Body="&lt;p&gt;Use sorted() or list.sort().&lt;/p&gt;" />
  <row Id="3" PostTypeId="2" ParentId="1" Score="1" Body="&lt;p&gt;Just Google it.&lt;/p&gt;" />
  <row Id="4" PostTypeId="1" Score="5" Title="What is a monad?" Body="&lt;p&gt;Can someone explain monads?&lt;/p&gt;" />
  <row Id="5" PostTypeId="2" ParentId="4" Score="8" Body="&lt;p&gt;A monad is a monoid in the category of endofunctors.&lt;/p&gt;" />
</posts>"#
        )
        .unwrap();
        posts_path
    }

    #[test]
    fn se_parse_filters_low_score() {
        let dir = tempfile::tempdir().unwrap();
        let posts_path = make_se_test_xml(dir.path());

        let extractor = StackExchangeExtractor::new(3);
        let docs: Vec<_> = extractor
            .extract(&posts_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        // Should have 2 answers (score 15 and score 8), not the score 1 answer.
        assert_eq!(docs.len(), 2);
        assert!(docs[0].content.contains("sorted()"));
        assert!(docs[1].content.contains("monad"));
        for d in &docs {
            assert!(!d.content.contains("Just Google it"));
        }
    }

    #[test]
    fn se_parse_from_directory() {
        let dir = tempfile::tempdir().unwrap();
        let community_dir = dir.path().join("stackoverflow.com");
        std::fs::create_dir(&community_dir).unwrap();
        make_se_test_xml(&community_dir);

        let extractor = StackExchangeExtractor::new(3);
        let docs: Vec<_> = extractor
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(docs.len(), 2);
    }
}
