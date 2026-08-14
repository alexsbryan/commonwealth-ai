// SPDX-License-Identifier: AGPL-3.0-or-later
//! R7 — enrichment: derived tagging + the custody join.
//!
//! Tags are derived deterministically from what the loop already knows
//! (the chunk's title words minus a stop set, or its corpus origin) —
//! no model, no invented metadata. The custody join (max-restrictiveness,
//! R-3/R-8) is computed here, at enrichment, and stamped onto the
//! window's `derived_custody`: the audit reads one field, the join is
//! created once.

use super::fetch::derive_custody;
use super::icd::EvidenceWindow;
use std::collections::HashSet;

/// The stop set — words that carry no tag signal.
const STOP: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "in", "on", "at", "to", "for", "with", "from", "by", "is",
    "are", "was", "were", "be", "been", "this", "that", "its", "it's", "as", "into", "over",
    "under", "about", "after", "before", "their", "there", "his", "her", "our", "your", "we",
    "they", "he", "she", "i", "you", "not", "no", "had", "has", "have", "will", "would", "page",
    "pages", "web", "article", "home", "index",
];

/// Derive tags for a web chunk from its title: lowercase, filtered,
/// deduped, capped at 6. Deterministic.
pub fn derive_tags(title: &str, source: &str) -> Vec<String> {
    let stop: HashSet<&str> = STOP.iter().copied().collect();
    let mut tags: Vec<String> = title
        .split(|c: char| !c.is_alphanumeric())
        .map(|w| w.trim().to_lowercase())
        .filter(|w| !w.is_empty() && w.chars().count() > 2 && !stop.contains(w.as_str()))
        .collect();
    tags.sort();
    tags.dedup();
    tags.truncate(6);
    if tags.is_empty() {
        // A title with no tag signal still gets a source trace.
        tags.push(source.to_string());
    }
    tags
}

/// Enrich the window: tag every chunk, join the custody (R-7).
pub fn enrich_window(window: &mut EvidenceWindow, titles_by_chunk: &[(String, String)]) {
    for chunk in &mut window.chunks {
        let title = titles_by_chunk
            .iter()
            .find(|(id, _)| id == &chunk.id)
            .map(|(_, t)| t.clone())
            .unwrap_or_default();
        chunk.tags = derive_tags(&title, &chunk.source_url);
    }
    window.derived_custody = derive_custody(&window.chunks);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Custody;

    fn window(chunks: &[(&str, &str)]) -> EvidenceWindow {
        EvidenceWindow {
            icd: "evidence_window".to_string(),
            version: 1,
            run_id: "r".to_string(),
            charter_hash: "h".to_string(),
            round: 1,
            chunks: chunks
                .iter()
                .map(|(id, custody)| super::super::icd::WindowChunk {
                    id: id.to_string(),
                    locator: format!("https://example.com/{id}"),
                    source_url: format!("https://example.com/{id}"),
                    custody: custody.to_string(),
                    provenance_class: "known".to_string(),
                    content: "x".to_string(),
                    ingested_into: None,
                    tags: Vec::new(),
                })
                .collect(),
            fetch_failures: Vec::new(),
            derived_custody: String::new(),
        }
    }

    #[test]
    fn tags_are_deterministic_and_filtered() {
        let mut tags = derive_tags(
            "The Meridian Bridge Completion History",
            "https://example.com",
        );
        assert!(tags.contains(&"meridian".to_string()));
        assert!(tags.contains(&"bridge".to_string()));
        assert!(tags.contains(&"completion".to_string()));
        assert!(!tags.contains(&"the".to_string()));
        assert_eq!(tags.len(), tags.iter().collect::<HashSet<_>>().len());
        // Same input → same tags.
        assert_eq!(
            tags,
            derive_tags(
                "The Meridian Bridge Completion History",
                "https://example.com"
            )
        );
        // Stop-only title → source trace tag.
        tags = derive_tags("The a an", "https://example.com/x");
        assert_eq!(tags, vec!["https://example.com/x".to_string()]);
    }

    #[test]
    fn enrich_joins_custody() {
        let mut w = window(&[("a", "public-web"), ("b", "peer")]);
        enrich_window(
            &mut w,
            &[
                ("a".to_string(), "Alpha Bridge Construction".to_string()),
                ("b".to_string(), "Beta Viaduct History".to_string()),
            ],
        );
        assert_eq!(w.derived_custody, Custody::Peer.as_str());
        // "page" is deliberately a stop word (page-shaped web words carry
        // no tag signal) — non-stop title words become the tags.
        assert_eq!(
            w.chunks[0].tags,
            vec![
                "alpha".to_string(),
                "bridge".to_string(),
                "construction".to_string()
            ]
        );
    }

    #[test]
    fn unknown_poisons_the_join() {
        let mut w = window(&[("a", "public-web"), ("b", "unknown")]);
        enrich_window(&mut w, &[]);
        assert_eq!(w.derived_custody, Custody::Unknown.as_str());
    }
}
