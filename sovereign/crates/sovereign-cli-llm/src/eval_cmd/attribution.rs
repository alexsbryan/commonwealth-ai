// SPDX-License-Identifier: AGPL-3.0-or-later
//! Post-retrieval attribution filter for conversation-history banks.
//!
//! Conversation chunks rendered by
//! `corpus_engine::chunkers::threaded_turns::ThreadedTurnsChunker`
//! preserve `### [YYYY-MM-DD HH:MM] {user|assistant}` turn headers
//! in their content. Bench questions with
//! `attribution_mode = "user"` or `"assistant"` ask the runner to
//! strip the opposite-author turn blocks before scoring, so a
//! model's restatement of the user's question does not count as
//! evidence of *the user* having said it (or vice versa).
//!
//! Why a post-retrieval filter and not a retrieval-time predicate:
//! retrieval today operates on the LanceDB chunk store which does
//! not persist per-span authorship as a structured column. The
//! `### [...]` headers in chunk content are the authoritative
//! authorship signal that survives ingest, so we read them here.
//! Future work could persist a per-chunk `attribution_present` flag
//! to short-circuit this filter for non-conversation banks, but
//! it's a no-op on chunk text without headers and the cost is
//! linear in chunk length — premature.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributionMode {
    Both,
    User,
    Assistant,
}

impl AttributionMode {
    pub fn from_str(s: &str) -> Self {
        match s {
            "user" => AttributionMode::User,
            "assistant" => AttributionMode::Assistant,
            _ => AttributionMode::Both,
        }
    }
}

fn turn_header_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"(?m)^###\s+\[([^\]]+)\]\s+(user|assistant)\s*$").unwrap())
}

/// Strip turn blocks whose sender does not match `mode`. Returns
/// the content unchanged when:
/// - `mode == Both`
/// - no turn headers are present (non-conversation chunk)
///
/// Returns an empty string when every span is filtered out.
pub fn filter_chunk_content(content: &str, mode: AttributionMode) -> String {
    if mode == AttributionMode::Both {
        return content.to_string();
    }
    let re = turn_header_regex();
    let captures: Vec<(usize, String)> = re
        .captures_iter(content)
        .map(|c| {
            let start = c.get(0).unwrap().start();
            let sender = c.get(2).unwrap().as_str().to_string();
            (start, sender)
        })
        .collect();

    if captures.is_empty() {
        return content.to_string();
    }

    let want = match mode {
        AttributionMode::User => "user",
        AttributionMode::Assistant => "assistant",
        AttributionMode::Both => unreachable!(),
    };

    let mut kept: Vec<&str> = Vec::new();
    for (i, (start, sender)) in captures.iter().enumerate() {
        if sender != want {
            continue;
        }
        let end = captures
            .get(i + 1)
            .map(|(s, _)| *s)
            .unwrap_or(content.len());
        kept.push(content[*start..end].trim_end());
    }
    kept.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "### [2025-09-04 18:01] user\n\nWhat was our burn rate?\n\n### [2025-09-04 18:02] assistant\n\nAbout $312k last month.";

    #[test]
    fn both_returns_unchanged() {
        let out = filter_chunk_content(SAMPLE, AttributionMode::Both);
        assert_eq!(out, SAMPLE);
    }

    #[test]
    fn user_keeps_only_user_block() {
        let out = filter_chunk_content(SAMPLE, AttributionMode::User);
        assert!(out.contains("burn rate"));
        assert!(!out.contains("$312k"), "assistant content leaked: {}", out);
        assert!(out.contains("### [2025-09-04 18:01] user"));
        assert!(!out.contains("### [2025-09-04 18:02] assistant"));
    }

    #[test]
    fn assistant_keeps_only_assistant_block() {
        let out = filter_chunk_content(SAMPLE, AttributionMode::Assistant);
        assert!(out.contains("$312k"));
        assert!(!out.contains("burn rate"));
        assert!(!out.contains("### [2025-09-04 18:01] user"));
        assert!(out.contains("### [2025-09-04 18:02] assistant"));
    }

    #[test]
    fn no_markers_is_passthrough_for_user_too() {
        let raw = "Plain prose with no turn markers, e.g. a wikipedia chunk.";
        assert_eq!(filter_chunk_content(raw, AttributionMode::User), raw);
        assert_eq!(filter_chunk_content(raw, AttributionMode::Assistant), raw);
    }

    #[test]
    fn empty_when_no_matching_attribution() {
        let only_user = "### [2025-09-04 18:01] user\n\nQ";
        assert_eq!(
            filter_chunk_content(only_user, AttributionMode::Assistant),
            ""
        );
    }

    #[test]
    fn multi_pair_keeps_all_matching() {
        let chunk = "### [2025-09-04 18:01] user\n\nQ1\n\n### [2025-09-04 18:02] assistant\n\nA1\n\n### [2025-09-04 18:10] user\n\nQ2\n\n### [2025-09-04 18:11] assistant\n\nA2";
        let user_only = filter_chunk_content(chunk, AttributionMode::User);
        assert!(user_only.contains("Q1"));
        assert!(user_only.contains("Q2"));
        assert!(!user_only.contains("A1"));
        assert!(!user_only.contains("A2"));
    }

    #[test]
    fn from_str_default_to_both() {
        assert_eq!(AttributionMode::from_str("user"), AttributionMode::User);
        assert_eq!(
            AttributionMode::from_str("assistant"),
            AttributionMode::Assistant
        );
        assert_eq!(AttributionMode::from_str("both"), AttributionMode::Both);
        assert_eq!(AttributionMode::from_str(""), AttributionMode::Both);
        assert_eq!(AttributionMode::from_str("garbage"), AttributionMode::Both);
    }
}
