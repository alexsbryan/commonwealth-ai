//! Link-based relationship graph (Layer 2).
//!
//! After all chunks are stored, reads every chunk's `outgoing_links` metadata
//! and creates `ClaimRelationship` entries (reusing the existing schema) for
//! links that originate from controversy sections and point to articles that
//! are also present in the corpus.
//!
//! No LLM is needed — the structural signal comes from Wikipedia's editorial
//! community: a link *from* a Criticism/Controversy section *to* article B
//! is a reliable indicator of epistemic tension between the source article
//! and B.

use std::collections::HashMap;

use crate::enrichment::article_profile::WikipediaChunkMetadata;
use crate::enrichment::relationships::{ClaimRelationship, RelationshipType};
use crate::error::Result;
use crate::index::CorpusIndex;
use crate::progress::{IngestProgress, ProgressCallback};

/// Builds a link-based relationship graph from stored Wikipedia chunk metadata.
pub struct LinkGraphBuilder {
    /// Section type values that indicate contested content. Relationships
    /// are only created for chunks whose `section_type` matches one of these.
    pub controversy_section_types: Vec<String>,
}

impl Default for LinkGraphBuilder {
    fn default() -> Self {
        Self {
            controversy_section_types: vec!["controversy".to_string()],
        }
    }
}

impl LinkGraphBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read all stored chunks, find controversy-section links to other articles
    /// in the corpus, and return `ClaimRelationship` entries.
    ///
    /// The returned relationships can be stored via `index.store_relationships()`,
    /// which reuses the same table as LLM-extracted relationships. Structural
    /// relationships are distinguished by their `confidence` value (0.7) and
    /// the `connecting_issue` message format.
    pub async fn build(
        &self,
        index: &CorpusIndex,
        progress: &Option<ProgressCallback>,
    ) -> Result<Vec<ClaimRelationship>> {
        let chunks = index.all_chunks_with_raw_metadata().await?;
        let total = chunks.len();

        // article_title → [chunk_ids] (first chunk per article = lead chunk)
        let mut article_to_chunks: HashMap<String, Vec<u64>> = HashMap::new();
        for chunk in &chunks {
            if let Some(title) = &chunk.title {
                if !title.is_empty() {
                    article_to_chunks
                        .entry(title.clone())
                        .or_default()
                        .push(chunk.id);
                }
            }
        }

        let mut relationships: Vec<ClaimRelationship> = Vec::new();
        let mut rel_id: u64 = 0;

        for (i, chunk) in chunks.iter().enumerate() {
            if i % 10_000 == 0 {
                if let Some(cb) = progress {
                    cb(IngestProgress::BuildingLinkGraph {
                        current: i,
                        total,
                    });
                }
            }

            let meta: WikipediaChunkMetadata = match chunk
                .metadata_raw
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
            {
                Some(m) => m,
                None => continue,
            };

            // Only generate relationships from controversy-typed sections.
            if !self
                .controversy_section_types
                .iter()
                .any(|t| t == &meta.section_type)
            {
                continue;
            }

            let source_title = match &chunk.title {
                Some(t) if !t.is_empty() => t.clone(),
                _ => continue,
            };

            for link in &meta.outgoing_links {
                // Skip self-links.
                if link.target_title == source_title {
                    continue;
                }

                // Only link to articles that are actually in the corpus.
                let target_chunks = match article_to_chunks.get(&link.target_title) {
                    Some(ids) => ids,
                    None => continue,
                };

                // Use the first (lead) chunk of the target article as claim_b.
                let target_chunk_id = target_chunks[0];

                let connecting_issue = Some(format!(
                    "{} → {} (via '{}' section, link text: \"{}\")",
                    source_title,
                    link.target_title,
                    meta.section_name,
                    truncate(&link.link_text, 60),
                ));

                relationships.push(ClaimRelationship {
                    id: rel_id,
                    claim_a_id: chunk.id,
                    claim_b_id: target_chunk_id,
                    relationship: RelationshipType::CompetingAnswers,
                    connecting_issue,
                    evidence_chunk_ids: vec![chunk.id, target_chunk_id],
                    confidence: 0.7,
                });
                rel_id += 1;
            }
        }

        // Final progress update.
        if let Some(cb) = progress {
            cb(IngestProgress::BuildingLinkGraph {
                current: total,
                total,
            });
        }

        // Deduplicate: keep only the highest-confidence relationship between
        // any (a, b) pair (regardless of direction).
        relationships = deduplicate(relationships);

        Ok(relationships)
    }
}

/// Keep only the first relationship for each (min(a,b), max(a,b)) pair.
fn deduplicate(mut rels: Vec<ClaimRelationship>) -> Vec<ClaimRelationship> {
    let mut seen: HashMap<(u64, u64), ()> = HashMap::new();
    rels.retain(|r| {
        let key = (r.claim_a_id.min(r.claim_b_id), r.claim_a_id.max(r.claim_b_id));
        seen.insert(key, ()).is_none()
    });
    rels
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let end = s
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        format!("{}…", &s[..end])
    }
}

// Extract a Wikipedia article title from a URL.
// https://en.wikipedia.org/wiki/Elinor_Ostrom → "Elinor Ostrom"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_from_url_basic() {
        assert_eq!(
            wiki_title_from_url("https://en.wikipedia.org/wiki/Elinor_Ostrom"),
            Some("Elinor Ostrom".to_string())
        );
    }

    #[test]
    fn title_from_url_strips_fragment() {
        assert_eq!(
            wiki_title_from_url("https://en.wikipedia.org/wiki/Homeopathy#Criticism"),
            Some("Homeopathy".to_string())
        );
    }

    #[test]
    fn title_from_url_strips_query() {
        assert_eq!(
            wiki_title_from_url("https://en.wikipedia.org/wiki/Water?action=history"),
            Some("Water".to_string())
        );
    }

    #[test]
    fn title_from_url_non_wiki() {
        assert_eq!(
            wiki_title_from_url("https://www.example.com/page"),
            None
        );
    }

    #[test]
    fn deduplicate_removes_reverse_pair() {
        let a = ClaimRelationship {
            id: 0,
            claim_a_id: 1,
            claim_b_id: 2,
            relationship: RelationshipType::CompetingAnswers,
            connecting_issue: None,
            evidence_chunk_ids: vec![],
            confidence: 0.7,
        };
        let b = ClaimRelationship {
            id: 1,
            claim_a_id: 2,
            claim_b_id: 1,
            relationship: RelationshipType::CompetingAnswers,
            connecting_issue: None,
            evidence_chunk_ids: vec![],
            confidence: 0.7,
        };
        let deduped = deduplicate(vec![a, b]);
        assert_eq!(deduped.len(), 1);
    }
}
