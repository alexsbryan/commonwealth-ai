//! Shared types for Wikipedia extractors.
//!
//! `WikipediaChunkMetadata` and `WikiLink` describe the structural metadata
//! produced during Wikipedia article ingestion. They are stored as JSON in
//! the `InsertChunk.metadata` field and used by enrichment and search.

use serde::{Deserialize, Serialize};

/// Chunk-level structural metadata stored by Wikipedia extractors
/// in the `InsertChunk.metadata` JSON field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikipediaChunkMetadata {
    pub section_name: String,
    pub section_path: Vec<String>,
    pub section_depth: u32,
    /// "lead", "controversy", "factual", or "general"
    pub section_type: String,
    pub citation_needed_count: Option<i64>,
    pub pov_count: Option<i64>,
    pub clarification_needed_count: Option<i64>,
    pub update_count: Option<i64>,
    pub is_flagged_stable: Option<bool>,
    pub outgoing_links: Vec<WikiLink>,
    /// Wikipedia revision ID from `version.identifier` — used as delta key.
    #[serde(default)]
    pub revision_id: Option<i64>,
    /// Wikidata entity QID from `main_entity.identifier` (e.g. "Q42").
    #[serde(default)]
    pub wikidata_qid: Option<String>,
    /// Wikipedia page ID from the top-level `identifier` column.
    #[serde(default)]
    pub page_id: Option<i64>,
}

/// A directed link from a Wikipedia section to another article.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiLink {
    /// Article title of the link target (spaces, not underscores).
    pub target_title: String,
    /// The display text of the link as it appears in the section.
    pub link_text: String,
}

// ─── URL helpers ─────────────────────────────────────────

/// Extract a Wikipedia article title from a URL.
/// `https://en.wikipedia.org/wiki/Elinor_Ostrom` → `"Elinor Ostrom"`
pub fn wiki_title_from_url(url: &str) -> Option<String> {
    // Find the /wiki/ path segment.
    let wiki_idx = url.find("/wiki/")?;
    let after_wiki = &url[wiki_idx + 6..];

    // Strip query strings and fragments.
    let end = after_wiki
        .find(|c| c == '?' || c == '#')
        .unwrap_or(after_wiki.len());
    let raw_title = &after_wiki[..end];

    if raw_title.is_empty() {
        return None;
    }

    // Decode percent-encoding for common sequences and replace underscores.
    let decoded = percent_decode(raw_title).replace('_', " ");
    Some(decoded)
}

/// Minimal percent-decode that handles the most common Wikipedia URL encodings.
fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (
                hex_digit(bytes[i + 1]),
                hex_digit(bytes[i + 2]),
            ) {
                out.push(((h << 4) | l) as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
