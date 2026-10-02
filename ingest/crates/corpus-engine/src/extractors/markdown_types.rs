// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared types for the markdown extractor.
//!
//! `MarkdownChunkMetadata` mirrors the shape of [`super::wikipedia_types::WikipediaChunkMetadata`]
//! so downstream atlas tooling (structure_first, atlas-eval, atlas-cross-corpus)
//! reads narrative metadata uniformly. The two-stream drift detection layer
//! depends on this isomorphism — any new pluggable narrative format
//! (asciidoc, restructuredtext) should produce a compatible metadata
//! shape so the rest of the pipeline doesn't need a per-format branch.

use serde::{Deserialize, Serialize};

/// Chunk-level structural metadata stored by the markdown extractor in
/// the `InsertChunk.metadata` JSON field.
///
/// One chunk corresponds to one heading-bounded section. The chunk's
/// content is the prose between its opening heading and the next
/// same-or-shallower heading (or end-of-document).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownChunkMetadata {
    /// Bare heading text, e.g. "Component Roster".
    pub section_name: String,
    /// Breadcrumb up the heading stack from H1 down to this section's
    /// own heading (inclusive). Empty for the document preamble before
    /// the first heading.
    pub section_path: Vec<String>,
    /// Heading depth: 1 for H1, 2 for H2, ..., 6 for H6. `0` for the
    /// document preamble before any heading.
    pub section_depth: u8,
    /// GitHub-style slug of the heading, e.g. `component-roster`.
    /// Stable enough to use as an intra-doc fragment reference.
    pub heading_anchor: String,
    /// Links emitted inside this section's prose.
    pub outgoing_links: Vec<MarkdownLink>,
    /// Backtick-spanned identifiers within this section's prose.
    /// Load-bearing for narrative-vs-structural matching: when a doc
    /// cites `Runtime` or `corpus_engine::engine::CorpusEngine` in
    /// backticks, those are the names the team explicitly flagged as
    /// architectural — high-precision signal for the cross-corpus
    /// matcher.
    pub inline_code_spans: Vec<String>,
    /// Obsidian-style `[[wiki-links]]` extracted from this section's
    /// prose. Pulldown-cmark does not recognise the `[[…]]` syntax —
    /// these are scanned out of the section body after the markdown
    /// walk. Empty for pure-markdown corpora that don't use Obsidian
    /// vault syntax. Defaulted on deserialise so older persisted
    /// metadata (pre-vault-port) round-trips cleanly.
    #[serde(default)]
    pub wiki_links: Vec<WikiLink>,
}

/// An Obsidian-style `[[target]]` or `[[target|display-text]]` link
/// scanned from a markdown section's prose. Pulldown-cmark treats
/// `[[` as ordinary text — vault corpora need this sidecar so the
/// downstream entity-graph builder can use hand-tagged wikilinks
/// as a high-precision entity-edge signal alongside GLiNER NER.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WikiLink {
    /// The link target as written between `[[…]]` (e.g. `"Joan Robinson"`
    /// for `[[Joan Robinson]]` or `"Joan Robinson"` for
    /// `[[Joan Robinson|Robinson]]`). Trimmed of surrounding
    /// whitespace; never empty.
    pub target_note: String,
    /// The display text after `|` if present, otherwise `None`. The
    /// downstream entity grapher prefers `display_text` over
    /// `target_note` when surfacing the entity name to the user, since
    /// the display form is what the prose actually reads as.
    pub display_text: Option<String>,
    /// Byte offset of the opening `[[` within the section's content
    /// string. Lets retrieval surfaces highlight the link site without
    /// re-scanning. `usize` is the natural shape for slicing the
    /// section content.
    pub char_offset: usize,
}

/// Scan a markdown section body for Obsidian `[[wiki-link]]` patterns.
/// Returns one [`WikiLink`] per occurrence in source order.
///
/// Recognises two shapes:
/// - `[[Target Note]]` — bare target
/// - `[[Target Note|Display Text]]` — target with pipe-separated alias
///
/// Skips well-formed nestings (`[[[a]]]`, `[a[b]c]`), unclosed openers
/// (`[[unclosed`), and empty targets (`[[]]`, `[[ | text]]`). Does not
/// interpret `\\[[escape\\]]` — the trailing `]]` still triggers a
/// match if the prose contains a real wikilink later. This matches
/// Obsidian's own permissive parsing and is the path of least
/// surprise for users porting notes.
///
/// Char offset is measured in BYTES from the start of `body`, so the
/// caller can index into `&body[offset..]` directly. Wikilink targets
/// are ASCII-friendly in practice; non-ASCII is preserved verbatim.
pub fn scan_wiki_links(body: &str) -> Vec<WikiLink> {
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if bytes[i] != b'[' || bytes[i + 1] != b'[' {
            i += 1;
            continue;
        }
        // Find the closing ]]. Bail if not found, or if we hit an
        // intervening `[[` (nested opens aren't valid wikilinks).
        let mut j = i + 2;
        let mut close = None;
        while j + 1 < bytes.len() {
            if bytes[j] == b'[' && bytes[j + 1] == b'[' {
                break;
            }
            if bytes[j] == b']' && bytes[j + 1] == b']' {
                close = Some(j);
                break;
            }
            j += 1;
        }
        let Some(close_idx) = close else {
            // Either unterminated or hit a nested `[[`. Skip past this
            // `[[` and try again from i+1 — a follow-on wikilink may
            // still be present.
            i += 1;
            continue;
        };
        let inner_bytes = &bytes[i + 2..close_idx];
        if let Some(link) = parse_wiki_link_inner(inner_bytes, i) {
            out.push(link);
        }
        i = close_idx + 2;
    }
    out
}

fn parse_wiki_link_inner(inner: &[u8], offset: usize) -> Option<WikiLink> {
    // Reject zero-width matches and matches containing line breaks
    // (Obsidian's own renderer treats `\n` inside `[[…]]` as a parse
    // failure; the [[ is shown literally).
    if inner.is_empty() || inner.iter().any(|b| *b == b'\n' || *b == b'\r') {
        return None;
    }
    let inner_str = std::str::from_utf8(inner).ok()?;
    let (target, display) = match inner_str.find('|') {
        Some(pipe_idx) => {
            let target = inner_str[..pipe_idx].trim();
            let display = inner_str[pipe_idx + 1..].trim();
            (
                target.to_string(),
                if display.is_empty() {
                    None
                } else {
                    Some(display.to_string())
                },
            )
        }
        None => (inner_str.trim().to_string(), None),
    };
    if target.is_empty() {
        return None;
    }
    Some(WikiLink {
        target_note: target,
        display_text: display,
        char_offset: offset,
    })
}

/// A link emitted in a markdown section. Distinguishes intra-document
/// anchors (`#section-id`) from external URLs and reference-style
/// links so consumers can route them differently — anchors feed the
/// document's internal graph, externals are off-corpus targets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownLink {
    /// Display text of the link as it appears in the prose.
    pub link_text: String,
    /// Resolved link target. For intra-doc anchors this is `#anchor`;
    /// for external links the full URL; for reference-style links the
    /// resolved URL after the `[label]: url` lookup.
    pub link_target: String,
    pub kind: LinkKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    /// Intra-document `#anchor` reference.
    Anchor,
    /// Off-document URL (`http://`, `https://`, `mailto:`, …).
    #[default]
    External,
    /// Reference-style link (`[text][label]`) resolved to a URL.
    Reference,
}

/// Generate a GitHub-style slug from a heading's text:
///   - lowercase
///   - non-alphanumeric runs collapsed to a single `-`
///   - leading/trailing `-` stripped
///
/// Examples:
///   `"Component Roster"`        → `"component-roster"`
///   `"§3.2 — When to use"`     → `"3-2-when-to-use"`
///   `"`Runtime` overview"`      → `"runtime-overview"`
pub fn slugify(heading: &str) -> String {
    let mut out = String::with_capacity(heading.len());
    let mut last_was_dash = false;
    for ch in heading.chars() {
        if ch.is_ascii_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
            last_was_dash = false;
        } else if !last_was_dash && !out.is_empty() {
            out.push('-');
            last_was_dash = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_handles_common_heading_shapes() {
        assert_eq!(slugify("Component Roster"), "component-roster");
        assert_eq!(slugify("§3.2 — When to use"), "3-2-when-to-use");
        assert_eq!(slugify("`Runtime` overview"), "runtime-overview");
        assert_eq!(slugify("ALL CAPS"), "all-caps");
        assert_eq!(slugify("  trailing  "), "trailing");
    }

    #[test]
    fn slugify_preserves_alphanumerics_only() {
        assert_eq!(slugify("foo_bar-baz"), "foo-bar-baz");
        assert_eq!(slugify("v1.0.0 release"), "v1-0-0-release");
    }

    #[test]
    fn scan_wiki_links_handles_bare_and_aliased_forms() {
        let body = "See [[Joan Robinson]] and [[Joan Robinson|Robinson]] for context.";
        let links = scan_wiki_links(body);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target_note, "Joan Robinson");
        assert_eq!(links[0].display_text, None);
        assert_eq!(links[0].char_offset, body.find("[[Joan").unwrap());
        assert_eq!(links[1].target_note, "Joan Robinson");
        assert_eq!(links[1].display_text.as_deref(), Some("Robinson"));
    }

    #[test]
    fn scan_wiki_links_skips_malformed_inputs() {
        // Unterminated, empty target, newline-bearing, nested-open.
        assert!(scan_wiki_links("orphan [[unclosed link").is_empty());
        assert!(scan_wiki_links("[[]]").is_empty());
        assert!(scan_wiki_links("[[ | only display]]").is_empty());
        assert!(scan_wiki_links("[[has\nlinebreak]]").is_empty());
        // A `[[` followed by another `[[` before any `]]` is malformed;
        // the scanner should still pick up the second wikilink.
        let body = "[[abandoned then [[Real Target]] later";
        let links = scan_wiki_links(body);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target_note, "Real Target");
    }

    #[test]
    fn scan_wiki_links_preserves_date_shaped_targets_verbatim() {
        // The parser does NOT filter date-shaped targets — that filter
        // belongs to the entity-graph translator (see
        // PROGRESSIVE_ENRICHMENT plan, forbidden_person_atoms guard).
        // We just capture what's there.
        let links = scan_wiki_links("Daily note: [[2024-01-15]] and [[2024-01-15|today]]");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target_note, "2024-01-15");
        assert_eq!(links[1].display_text.as_deref(), Some("today"));
    }
}
