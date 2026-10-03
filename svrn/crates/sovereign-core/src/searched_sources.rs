// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ONE merge of fresh web-search results into a conversation's
//! cumulative `searched_sources` registry (sv-surface G3b).
//!
//! Written twice before this existed — inline in the desktop's
//! `submit_information_search` and (as of G3b) needed daemon-side when an
//! `Answer` carries its `sources`. One implementation, both hosts (ARCH
//! §10.6); the desktop's inline copy is deleted.
//!
//! Semantics (Marathon-graceful M3, unchanged by the move): dedupe by
//! URL — an entry already in the registry gets its `last_referenced_turn`
//! bumped to the current turn; a fresh URL is appended with
//! `first_seen_turn = last_referenced_turn = current_turn`. The synthesis
//! system message later renders the registry as "Web sources gathered so
//! far", so the model keeps stable awareness of which URLs the user has
//! already been shown.

use sovereign_contracts::types::SearchedSourceEntry;

/// Fold `fresh` rows into the conversation's existing registry at
/// `current_turn`.
///
/// `fresh` rows carry the client's `url` / `title` / `search_query`; their
/// turn stamps are set HERE, one place, because "which turn is current" is
/// the host's fact (the daemon counts the conversation's messages, the
/// caller passes it in — the store read stays at the call site where the
/// store handle lives).
pub fn merge_into(
    existing: Option<Vec<SearchedSourceEntry>>,
    fresh: impl IntoIterator<Item = (String, String, String)>,
    current_turn: usize,
) -> Vec<SearchedSourceEntry> {
    let mut entries = existing.unwrap_or_default();
    let mut url_seen: std::collections::HashSet<String> =
        entries.iter().map(|e| e.url.clone()).collect();
    for (url, title, search_query) in fresh {
        if url_seen.contains(&url) {
            if let Some(existing) = entries.iter_mut().find(|e| e.url == url) {
                existing.last_referenced_turn = current_turn;
            }
        } else {
            url_seen.insert(url.clone());
            entries.push(SearchedSourceEntry {
                url,
                title,
                first_seen_turn: current_turn,
                last_referenced_turn: current_turn,
                search_query,
            });
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(url: &str, title: &str, q: &str) -> (String, String, String) {
        (url.into(), title.into(), q.into())
    }

    #[test]
    fn a_fresh_url_appends_with_both_stamps_at_the_current_turn() {
        let out = merge_into(None, [row("https://a", "A", "q")], 4);
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].first_seen_turn, out[0].last_referenced_turn),
            (4, 4)
        );
        assert_eq!(out[0].search_query, "q");
    }

    #[test]
    fn a_known_url_bumps_last_referenced_and_keeps_first_seen() {
        let existing = vec![SearchedSourceEntry {
            url: "https://a".into(),
            title: "A".into(),
            first_seen_turn: 2,
            last_referenced_turn: 2,
            search_query: "old".into(),
        }];
        let out = merge_into(Some(existing), [row("https://a", "A2", "new")], 7);
        assert_eq!(out.len(), 1, "a repeat does not append");
        assert_eq!(
            (out[0].first_seen_turn, out[0].last_referenced_turn),
            (2, 7),
            "first sight is history; last reference is now"
        );
        assert_eq!(out[0].search_query, "old", "the original query stays");
    }
}
