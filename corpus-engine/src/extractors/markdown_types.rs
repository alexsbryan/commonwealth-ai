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
}
