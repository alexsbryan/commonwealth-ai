// SPDX-License-Identifier: AGPL-3.0-or-later
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
    let end = after_wiki.find(['?', '#']).unwrap_or(after_wiki.len());
    let raw_title = &after_wiki[..end];

    if raw_title.is_empty() {
        return None;
    }

    // Decode percent-encoding for common sequences and replace underscores.
    let decoded = percent_decode(raw_title).replace('_', " ");
    Some(decoded)
}

/// Percent-decode for Wikipedia URLs.
///
/// **UTF-8 aware.** Earlier versions cast each `%XX` byte directly to a
/// `char`, which breaks multi-byte UTF-8 sequences: `é` is encoded as
/// `%C3%A9` (two bytes `0xC3 0xA9`), and the per-byte cast produced
/// `Ã©` (two Latin-1 chars) instead of `é`. That mojibake then
/// propagated into LanceDB chunks (`url`, `title`) and into the
/// `_corpus_meta.json` if any title field used this helper. The diff
/// against `vital_articles_l5` showed ~957 articles materializing as
/// `1913 ottoman coup d'ã©tat` rather than `1913 ottoman coup d'état`,
/// failing every filter and search match for those titles.
///
/// Correct path: accumulate the decoded `%XX` sequences into a byte
/// buffer, then decode the buffer as UTF-8 in one shot. Falls back to
/// `from_utf8_lossy` so a malformed title yields a U+FFFD replacement
/// rather than panicking — the title may be junk anyway and we don't
/// want to crash the entire ingest.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut buf: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2])) {
                buf.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        buf.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_title_from_url_decodes_multibyte_utf8() {
        // `é` is `%C3%A9` (UTF-8 0xC3 0xA9). The fix is to accumulate
        // bytes and UTF-8-decode at the end, instead of casting each
        // byte to a char individually (which produced `Ã©`).
        assert_eq!(
            wiki_title_from_url("https://en.wikipedia.org/wiki/Coup_d%27%C3%A9tat"),
            Some("Coup d'état".to_string())
        );
    }

    #[test]
    fn wiki_title_from_url_decodes_multiple_utf8_chars() {
        // Tōhoku — `ō` is `%C5%8D`. En dash — `–` is `%E2%80%93`.
        // Both must round-trip through one buffer to UTF-8.
        assert_eq!(
            wiki_title_from_url(
                "https://en.wikipedia.org/wiki/2011_T%C5%8Dhoku_earthquake_and_tsunami"
            ),
            Some("2011 Tōhoku earthquake and tsunami".to_string())
        );
        assert_eq!(
            wiki_title_from_url(
                "https://en.wikipedia.org/wiki/1936%E2%80%931939_Arab_revolt_in_Palestine"
            ),
            Some("1936–1939 Arab revolt in Palestine".to_string())
        );
    }

    #[test]
    fn wiki_title_from_url_strips_fragment_and_query() {
        // The function strips `#` fragments and `?` queries, which is
        // why distinct-section URLs collapse to a single article title.
        assert_eq!(
            wiki_title_from_url("https://en.wikipedia.org/wiki/Albert_Einstein#Early_life"),
            Some("Albert Einstein".to_string())
        );
        assert_eq!(
            wiki_title_from_url("https://en.wikipedia.org/wiki/Albert_Einstein?action=raw"),
            Some("Albert Einstein".to_string())
        );
    }

    #[test]
    fn wiki_title_from_url_returns_none_for_non_wiki_paths() {
        assert!(wiki_title_from_url("https://example.com/foo").is_none());
        assert!(wiki_title_from_url("https://en.wikipedia.org/wiki/").is_none());
    }
}
