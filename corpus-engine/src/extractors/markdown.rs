//! Section-aware markdown extractor.
//!
//! Walks a markdown file with `pulldown-cmark` and emits one
//! [`ExtractedDoc`] per heading-bounded section. A section's content
//! is the prose between its opening heading and the next
//! same-or-shallower heading (or end-of-document). The document
//! preamble (text before the first heading, when present) is emitted
//! as a synthetic depth-0 section.
//!
//! ## Why section-bounded chunking
//!
//! The two-stream atlas pipeline matches narrative section descriptions
//! to structural code entities. Heading boundaries are the natural
//! semantic unit — a section's prose is about one architectural topic,
//! not three. Paragraph-level chunking would over-fragment; whole-doc
//! chunking would lose the section title that anchors meaning.
//!
//! ## Why this metadata
//!
//! The `inline_code_spans` field is load-bearing for narrative-vs-
//! structural matching. When a doc cites `` `Runtime` `` or
//! `` `corpus_engine::engine::CorpusEngine` `` in backticks, those are
//! exactly the names the team flagged as architectural — high-precision
//! signal for the cross-corpus matcher. Without this, we'd have to
//! tokenize narrative prose and guess at component names.
//!
//! ## Determinism
//!
//! The pulldown-cmark walker is single-threaded and deterministic.
//! All output collections (Vec, BTreeMap) preserve source order. No
//! timestamps in chunk output. Two runs over the same file produce
//! byte-identical metadata.

use std::fs;
use std::path::Path;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd};

use super::markdown_types::{
    scan_wiki_links, slugify, LinkKind, MarkdownChunkMetadata, MarkdownLink,
};
use super::{ExtractedDoc, Extractor};
use crate::error::{Error, Result};

/// Extracts heading-bounded sections from a markdown file.
///
/// Construct via [`MarkdownExtractor::new()`]; the extractor is
/// stateless and inexpensive to clone.
#[derive(Debug, Clone, Default)]
pub struct MarkdownExtractor;

impl MarkdownExtractor {
    pub fn new() -> Self {
        Self
    }

    /// Parse a markdown string into a sequence of section chunks.
    /// Public so unit tests + the integration test in
    /// `tests/atlas_narrative_markdown.rs` can exercise the chunking
    /// without going through the file-system walker.
    pub fn extract_sections(source: &str, source_label: &str) -> Vec<MarkdownSection> {
        Walker::new(source).run(source_label)
    }
}

impl Extractor for MarkdownExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let raw = fs::read_to_string(source_path).map_err(|e| {
            Error::Extraction(format!("markdown: read {}: {e}", source_path.display()))
        })?;
        let label = source_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("markdown")
            .to_string();
        let url = format!("file://{}", source_path.display());

        let sections = MarkdownExtractor::extract_sections(&raw, &label);
        let docs: Vec<Result<ExtractedDoc>> = sections
            .into_iter()
            .map(|sec| {
                let metadata = serde_json::to_value(&sec.metadata)
                    .map_err(|e| Error::Extraction(format!("markdown: serialise metadata: {e}")))?;
                Ok(ExtractedDoc {
                    title: Some(sec.metadata.section_name.clone()),
                    // Every section from the same file shares a
                    // source_id so file-level reindex nukes them all
                    // in one delete.
                    source_id: label.clone(),
                    url: Some(url.clone()),
                    metadata: Some(metadata),
                    content: sec.content,
                    source_file: None,
                    embed_text: None,
                })
            })
            .collect();

        Ok(Box::new(docs.into_iter()))
    }
}

// ── Section + intermediate state ─────────────────────────────

/// A heading-bounded section emitted by the walker. Tested directly
/// via `MarkdownExtractor::extract_sections`.
#[derive(Debug, Clone)]
pub struct MarkdownSection {
    pub content: String,
    pub metadata: MarkdownChunkMetadata,
}

struct Walker<'a> {
    parser: Parser<'a>,
    /// Heading stack — headings opened above the current cursor that
    /// haven't been closed by a same-or-shallower heading yet. Each
    /// entry is `(depth, name)`. The deepest entry is the current
    /// section's heading.
    heading_stack: Vec<(u8, String)>,
    /// Sections we've completed, in source order.
    out: Vec<MarkdownSection>,
    /// Working state for the section currently being assembled.
    current: SectionInProgress,
}

#[derive(Default)]
struct SectionInProgress {
    /// Body text accumulated so far, with paragraph breaks preserved.
    text: String,
    /// Whether we're inside the heading text (so we can capture it
    /// before saving it to the heading stack).
    in_heading: bool,
    /// The heading text being captured during `Tag::Heading` open →
    /// `TagEnd::Heading` close.
    pending_heading_text: String,
    /// Depth of the heading currently being captured.
    pending_heading_depth: u8,
    /// Inline code spans accumulated within this section's prose.
    inline_code_spans: Vec<String>,
    /// Tracking for link emission.
    in_link: bool,
    pending_link_text: String,
    pending_link_target: String,
    pending_link_kind: LinkKind,
    outgoing_links: Vec<MarkdownLink>,
    /// Whether we're currently inside a fenced code block; we want to
    /// preserve its text in the body but NOT scan for inline_code
    /// spans inside it.
    in_code_block: bool,
}

impl<'a> Walker<'a> {
    fn new(source: &'a str) -> Self {
        let mut opts = Options::empty();
        opts.insert(Options::ENABLE_TABLES);
        opts.insert(Options::ENABLE_STRIKETHROUGH);
        Self {
            parser: Parser::new_ext(source, opts),
            heading_stack: Vec::new(),
            out: Vec::new(),
            current: SectionInProgress::default(),
        }
    }

    fn run(mut self, _source_label: &str) -> Vec<MarkdownSection> {
        // We can't use a for-loop over self.parser because the
        // borrow-checker can't see we don't re-enter it. Drain via
        // .next() in a while loop.
        while let Some(event) = self.parser.next() {
            self.handle(event);
        }
        // Close out the trailing section.
        self.flush_current();
        self.out
    }

    fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                // Closing the previous section happens at the SAME-or-
                // SHALLOWER boundary check inside `open_heading`.
                self.current.in_heading = true;
                self.current.pending_heading_text.clear();
                self.current.pending_heading_depth = heading_depth(level);
            }
            Event::End(TagEnd::Heading(_)) => {
                let depth = self.current.pending_heading_depth;
                let text = std::mem::take(&mut self.current.pending_heading_text);
                self.current.in_heading = false;
                self.open_heading(depth, text);
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_)))
            | Event::Start(Tag::CodeBlock(CodeBlockKind::Indented)) => {
                self.current.in_code_block = true;
                if !self.current.in_heading {
                    self.current.text.push_str("\n```\n");
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                self.current.in_code_block = false;
                if !self.current.in_heading {
                    self.current.text.push_str("```\n");
                }
            }
            Event::Code(code) => {
                if self.current.in_heading {
                    self.current.pending_heading_text.push_str(&code);
                } else {
                    self.current.text.push('`');
                    self.current.text.push_str(&code);
                    self.current.text.push('`');
                    if !self.current.in_code_block {
                        self.current.inline_code_spans.push(code.to_string());
                    }
                }
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                ..
            }) => {
                self.current.in_link = true;
                self.current.pending_link_text.clear();
                self.current.pending_link_target = dest_url.to_string();
                self.current.pending_link_kind = link_kind_from(link_type, &dest_url);
            }
            Event::End(TagEnd::Link) => {
                self.current.in_link = false;
                let text = std::mem::take(&mut self.current.pending_link_text);
                let target = std::mem::take(&mut self.current.pending_link_target);
                self.current.outgoing_links.push(MarkdownLink {
                    link_text: text.clone(),
                    link_target: target.clone(),
                    kind: self.current.pending_link_kind,
                });
                self.current.text.push('[');
                self.current.text.push_str(&text);
                self.current.text.push_str("](");
                self.current.text.push_str(&target);
                self.current.text.push(')');
            }
            Event::Text(text) => {
                if self.current.in_heading {
                    self.current.pending_heading_text.push_str(&text);
                } else if self.current.in_link {
                    self.current.pending_link_text.push_str(&text);
                } else {
                    self.current.text.push_str(&text);
                }
            }
            Event::SoftBreak => {
                if !self.current.in_heading {
                    self.current.text.push('\n');
                }
            }
            Event::HardBreak => {
                if !self.current.in_heading {
                    self.current.text.push_str("\n\n");
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if !self.current.in_heading {
                    self.current.text.push_str("\n\n");
                }
            }
            Event::End(TagEnd::Item) => {
                if !self.current.in_heading {
                    self.current.text.push('\n');
                }
            }
            _ => {
                // Ignore: list bullets, emphasis tags, html, footnote
                // refs, etc. The Text events inside them still flow
                // through to body content.
            }
        }
    }

    /// Called when we finish capturing a heading. Closes the previous
    /// section (same-or-shallower depth pops the stack) and starts a
    /// new one.
    fn open_heading(&mut self, depth: u8, name: String) {
        // Flush whatever section we were assembling.
        self.flush_current();
        // Pop heading stack entries that are deeper than (or equal to)
        // this new heading — they no longer enclose anything.
        while let Some((top_depth, _)) = self.heading_stack.last() {
            if *top_depth >= depth {
                self.heading_stack.pop();
            } else {
                break;
            }
        }
        self.heading_stack.push((depth, name));
    }

    fn flush_current(&mut self) {
        let body = std::mem::take(&mut self.current.text)
            .trim_end()
            .to_string();
        let outgoing_links = std::mem::take(&mut self.current.outgoing_links);
        let inline_code_spans = std::mem::take(&mut self.current.inline_code_spans);
        // Wiki-links are scanned out of the section body post-walk
        // because pulldown-cmark treats `[[…]]` as plain text. Empty
        // for any section that doesn't use Obsidian syntax — the
        // sidecar is free for non-vault corpora.
        let wiki_links = scan_wiki_links(&body);

        if self.heading_stack.is_empty() {
            // Document preamble. Emit as a synthetic depth-0 section
            // only if there's actual prose to preserve.
            if body.is_empty()
                && outgoing_links.is_empty()
                && inline_code_spans.is_empty()
                && wiki_links.is_empty()
            {
                return;
            }
            self.out.push(MarkdownSection {
                content: body,
                metadata: MarkdownChunkMetadata {
                    section_name: String::new(),
                    section_path: Vec::new(),
                    section_depth: 0,
                    heading_anchor: String::new(),
                    outgoing_links,
                    inline_code_spans,
                    wiki_links,
                },
            });
            return;
        }

        let (depth, name) = self.heading_stack.last().cloned().unwrap();
        let section_path: Vec<String> = self.heading_stack.iter().map(|(_, n)| n.clone()).collect();
        let heading_anchor = slugify(&name);
        self.out.push(MarkdownSection {
            content: body,
            metadata: MarkdownChunkMetadata {
                section_name: name,
                section_path,
                section_depth: depth,
                heading_anchor,
                outgoing_links,
                inline_code_spans,
                wiki_links,
            },
        });
    }
}

fn heading_depth(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn link_kind_from(link_type: LinkType, dest_url: &str) -> LinkKind {
    if matches!(
        link_type,
        LinkType::Reference | LinkType::Collapsed | LinkType::Shortcut
    ) {
        return LinkKind::Reference;
    }
    if dest_url.starts_with('#') {
        LinkKind::Anchor
    } else {
        LinkKind::External
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_h2_h3_section_paths() {
        let src = "\
# Top
preamble.

## Alpha
Body of alpha.

### Alpha-One
Detail of alpha-one.

## Beta
Body of beta.
";
        let sections = MarkdownExtractor::extract_sections(src, "test.md");
        let names: Vec<_> = sections
            .iter()
            .map(|s| s.metadata.section_name.as_str())
            .collect();
        assert_eq!(names, vec!["Top", "Alpha", "Alpha-One", "Beta"]);

        let alpha_one = sections
            .iter()
            .find(|s| s.metadata.section_name == "Alpha-One")
            .unwrap();
        assert_eq!(
            alpha_one.metadata.section_path,
            vec![
                "Top".to_string(),
                "Alpha".to_string(),
                "Alpha-One".to_string()
            ]
        );
        assert_eq!(alpha_one.metadata.section_depth, 3);
        assert_eq!(alpha_one.metadata.heading_anchor, "alpha-one");

        let beta = sections
            .iter()
            .find(|s| s.metadata.section_name == "Beta")
            .unwrap();
        assert_eq!(
            beta.metadata.section_path,
            vec!["Top".to_string(), "Beta".to_string()]
        );
    }

    #[test]
    fn captures_inline_code_spans() {
        let src = "\
# Doc

The `Runtime` orchestrates `Router`, `Planner`, and `Executor`.
Module path: `sovereign_core::runtime::Runtime`.
";
        let sections = MarkdownExtractor::extract_sections(src, "test.md");
        let doc = sections
            .iter()
            .find(|s| s.metadata.section_name == "Doc")
            .unwrap();
        assert!(doc
            .metadata
            .inline_code_spans
            .contains(&"Runtime".to_string()));
        assert!(doc
            .metadata
            .inline_code_spans
            .contains(&"Router".to_string()));
        assert!(doc
            .metadata
            .inline_code_spans
            .contains(&"sovereign_core::runtime::Runtime".to_string()));
    }

    #[test]
    fn classifies_link_kinds() {
        let src = "\
# Doc

See [§3.2](#component-roster) for the inventory.
External: [Rust](https://www.rust-lang.org).
";
        let sections = MarkdownExtractor::extract_sections(src, "test.md");
        let doc = &sections[0];
        let kinds: Vec<_> = doc.metadata.outgoing_links.iter().map(|l| l.kind).collect();
        assert!(kinds.contains(&LinkKind::Anchor));
        assert!(kinds.contains(&LinkKind::External));
    }

    #[test]
    fn ignores_inline_code_inside_fenced_blocks() {
        let src = "\
# Doc

Outside the block: `Runtime`.

```
let foo = `not-a-span`;
```
";
        let sections = MarkdownExtractor::extract_sections(src, "test.md");
        let doc = &sections[0];
        assert!(doc
            .metadata
            .inline_code_spans
            .contains(&"Runtime".to_string()));
        assert!(!doc
            .metadata
            .inline_code_spans
            .iter()
            .any(|s| s.contains("not-a-span")));
    }

    #[test]
    fn preamble_before_first_heading_is_a_section() {
        let src = "\
Some prose at the top.

## Section
Body.
";
        let sections = MarkdownExtractor::extract_sections(src, "test.md");
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].metadata.section_depth, 0);
        assert_eq!(sections[0].metadata.section_name, "");
        assert!(sections[0].content.contains("Some prose at the top"));
    }

    #[test]
    fn output_is_deterministic_across_runs() {
        let src = "# A\nbody\n## B\nmore\n## C\nfinal\n";
        let a = MarkdownExtractor::extract_sections(src, "test.md");
        let b = MarkdownExtractor::extract_sections(src, "test.md");
        let aj = serde_json::to_string(&a.iter().map(|s| &s.metadata).collect::<Vec<_>>()).unwrap();
        let bj = serde_json::to_string(&b.iter().map(|s| &s.metadata).collect::<Vec<_>>()).unwrap();
        assert_eq!(aj, bj);
    }
}
