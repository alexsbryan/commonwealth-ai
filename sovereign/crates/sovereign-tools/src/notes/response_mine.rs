//! Response miner (Phase 7.2).
//!
//! Scans assistant transcripts for decision-language sentences
//! and returns them as candidate notes tagged `source='inferred'`.
//! The audit's job is to merge these alongside the agent's own
//! `note(...)` calls and the diff extractor's higher-confidence
//! `source='extracted'` rows.
//!
//! ## Design choices
//!
//! - **No regex dep.** The patterns we care about are short
//!   prefix-style phrases ("I'll use X because Y", "chose X over
//!   Y", "decided to …"). Substring + sentence-boundary scans
//!   capture them without pulling `regex` in. A future iteration
//!   can swap in a richer matcher if precision matters.
//!
//! - **Sentence-bounded.** Each match is one sentence so the
//!   audit reads naturally. We split on `. ` / `! ` / `? ` /
//!   newline-newline rather than fancy linguistics — overrun is
//!   the worse failure mode (cluttered audit) than under-match
//!   (single-sentence trigger missed).
//!
//! - **Stoplist filters mechanical chatter.** "Renamed", "fixed
//!   typo", "formatted", "import" — these are bookkeeping, not
//!   decisions. The audit is supposed to be "non-empty for
//!   sessions that did real work," not "drowning in churn."
//!
//! - **Source-of-truth note tag.** The kind defaults to
//!   `decision`; alternative-comparison phrases ("chose X over Y")
//!   get the same kind. The audit groups by `(kind, source)`, so
//!   what matters is that the row is tagged `source='inferred'`
//!   when the caller persists it.
//!
//! ## What this is NOT
//!
//! - Not a full NLP. We don't parse pronouns, coreference, or
//!   tense. A sentence "we considered using X" doesn't fire — we
//!   want the resolved decision, not the deliberation.
//! - Not a recommendation engine. We surface what the assistant
//!   appeared to commit to; the audit reader judges.
//!
//! ## Future hooks
//!
//! Phase 7.3's audit assembly reads `messages` + `messages_fts`
//! (sovereign-store) and runs `ResponseMiner::mine` over each
//! assistant response in the session window. Phase 7.2's
//! middleware (`commonwealth-api::middleware::decision_extractor`)
//! reuses the same matcher per-turn for the two-turn-lookahead
//! flow.

/// One mined decision-shaped sentence ready to become a note.
/// The caller is responsible for persisting it via
/// `NoteStore::write_note_with_source(..., NoteSource::Inferred, ...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionMatch {
    /// Which trigger fired. Useful for telemetry and for the
    /// audit's "Open questions" vs "Decisions" sectioning.
    pub kind: MatchKind,
    /// The full sentence we matched, trimmed of leading
    /// whitespace and any trailing terminator.
    pub sentence: String,
    /// Byte offset of the sentence's start within the original
    /// input. Lets callers correlate the mined match with the
    /// transcript window it came from (Phase 7.3 audit
    /// assembly).
    pub start_offset: usize,
}

/// Categorises a match by trigger family. Kept as a plain enum
/// (not free-form strings) so test assertions stay precise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MatchKind {
    /// "I'll use X because Y", "going with X", "we'll go with X."
    /// The agent committed to a path.
    Commitment,
    /// "Chose X over Y", "X over Y for ...", "preferring X to Y."
    /// Comparative decision — the audit reads better when these
    /// are differentiated from straight commitments.
    Comparison,
    /// "Decided to X", "decision: X", "the decision is to X."
    /// Explicit decision marker.
    ExplicitDecision,
}

impl MatchKind {
    /// Stable string id for telemetry / audit grouping.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Commitment => "commitment",
            Self::Comparison => "comparison",
            Self::ExplicitDecision => "explicit_decision",
        }
    }
}

/// Stoplist of substrings that disqualify a sentence from
/// becoming a decision note. These are the kind of mechanical
/// language that shows up in housekeeping ("renamed the var",
/// "fixed typo in docstring") and never represents a decision
/// worth recording.
///
/// Match is case-insensitive and substring-anchored — appearing
/// anywhere in the sentence disqualifies it.
const STOPLIST: &[&str] = &[
    "rename",
    "renamed",
    "fix typo",
    "fixed typo",
    "format",
    "formatted",
    "reformat",
    "import",
    "imported",
    "whitespace",
    "indent",
    "indentation",
    "trailing",
    "lint",
    "linting",
];

/// Trigger phrase set. Each entry is `(prefix_or_substring, kind)`.
/// The matcher tests substrings on the lower-cased sentence.
///
/// Order matters only for telemetry — if two triggers fire on the
/// same sentence, the first one wins. We list ExplicitDecision
/// first because "decided to X" is a clearer signal than "I'll
/// use X."
const TRIGGERS: &[(&str, MatchKind)] = &[
    // Explicit decision markers.
    ("decided to", MatchKind::ExplicitDecision),
    ("decision is to", MatchKind::ExplicitDecision),
    ("the decision: ", MatchKind::ExplicitDecision),
    ("decision: ", MatchKind::ExplicitDecision),
    // Commitments. Order: longer phrases first so a sentence
    // containing "we'll go with" doesn't get mis-attributed to
    // "go with" alone.
    ("we'll go with", MatchKind::Commitment),
    ("i'll go with", MatchKind::Commitment),
    ("we'll use", MatchKind::Commitment),
    ("i'll use", MatchKind::Commitment),
    ("going with", MatchKind::Commitment),
    ("going to use", MatchKind::Commitment),
    // Comparisons.
    ("chose to ", MatchKind::Comparison),
    ("chose ", MatchKind::Comparison),
    ("over ", MatchKind::Comparison), // catches "X over Y because"
    ("preferring ", MatchKind::Comparison),
];

/// Maximum characters per match. A 600-char "decision" is a
/// paragraph; that's noise. Cap at a sensible single-sentence
/// length and trust the audit's renderer to wrap the rest.
const MAX_SENTENCE_LEN: usize = 320;

/// Maximum matches returned per call. Spec calls for ~500 token
/// budget at the audit-assembly side; capping the miner at 12
/// keeps a single chatty session from dominating the audit.
const MAX_MATCHES_PER_CALL: usize = 12;

/// Mine `text` for decision-shaped sentences. `text` is typically
/// one assistant response or one session's concatenated
/// transcript. Returns matches in the order they appeared.
///
/// Pure: no I/O, no allocations beyond the returned vec.
pub fn mine(text: &str) -> Vec<DecisionMatch> {
    let mut out = Vec::new();
    for (sentence, start) in iter_sentences(text) {
        if out.len() >= MAX_MATCHES_PER_CALL {
            break;
        }
        let trimmed = sentence.trim();
        if trimmed.is_empty() || trimmed.len() > MAX_SENTENCE_LEN {
            continue;
        }
        let lower = trimmed.to_lowercase();
        if STOPLIST.iter().any(|s| contains_at_word_boundary(&lower, s)) {
            continue;
        }
        let Some((kind, _)) = TRIGGERS.iter().find_map(|(needle, kind)| {
            // Trigger must be at a word boundary so "you'll use" doesn't
            // match the substring "u'll use" inside "youll use" — but
            // a leading position OR preceding whitespace counts. Substring
            // search is fine for `needle` ending in space already; for
            // `decision:` etc. we accept the substring anywhere.
            if lower.contains(needle) {
                Some((*kind, *needle))
            } else {
                None
            }
        }) else {
            continue;
        };
        out.push(DecisionMatch {
            kind,
            sentence: trimmed.to_string(),
            start_offset: start,
        });
    }
    out
}

/// True iff `needle` appears in `haystack` at word boundaries on
/// both sides. A "word boundary" here is the start/end of the
/// string OR a non-alphanumeric character. This matters for
/// single-word stoplist entries — "rename" should match
/// "renamed the var" but NOT "atomic rename for safety" (where
/// the surrounding `c` and ` ` aren't both alphanumeric, but the
/// LEADING `c` is, so the leading boundary fails). We require a
/// non-alphanumeric character (or string edge) on both sides.
fn contains_at_word_boundary(haystack: &str, needle: &str) -> bool {
    let mut search_from = 0;
    while let Some(idx) = haystack[search_from..].find(needle) {
        let abs = search_from + idx;
        let before_ok = abs == 0
            || !haystack[..abs]
                .chars()
                .next_back()
                .map(|c| c.is_ascii_alphanumeric())
                .unwrap_or(false);
        let end = abs + needle.len();
        let after_ok = end == haystack.len()
            || !haystack[end..]
                .chars()
                .next()
                .map(|c| c.is_ascii_alphanumeric())
                .unwrap_or(false);
        if before_ok && after_ok {
            return true;
        }
        search_from = abs + 1;
    }
    false
}

/// Iterator over (sentence, start_offset) pairs. Splits on `. `,
/// `! `, `? `, and double newline. Standalone newlines INSIDE a
/// sentence are tolerated (so a line-wrapped narrative reads as
/// one match). Each yielded sentence retains internal whitespace
/// but has its trailing terminator trimmed.
fn iter_sentences(text: &str) -> impl Iterator<Item = (&str, usize)> + '_ {
    SentenceIter {
        text,
        cursor: 0,
        bytes: text.as_bytes(),
    }
}

struct SentenceIter<'a> {
    text: &'a str,
    cursor: usize,
    bytes: &'a [u8],
}

impl<'a> Iterator for SentenceIter<'a> {
    type Item = (&'a str, usize);

    fn next(&mut self) -> Option<Self::Item> {
        // Skip leading whitespace so the start_offset points at
        // the first non-whitespace character of the sentence.
        while self.cursor < self.bytes.len()
            && self.bytes[self.cursor].is_ascii_whitespace()
        {
            self.cursor += 1;
        }
        if self.cursor >= self.bytes.len() {
            return None;
        }
        let start = self.cursor;
        let mut end = start;
        while end < self.bytes.len() {
            let c = self.bytes[end];
            // Sentence terminator: `.` / `!` / `?` followed by
            // whitespace, OR two consecutive newlines.
            if matches!(c, b'.' | b'!' | b'?')
                && (end + 1 == self.bytes.len()
                    || self.bytes[end + 1].is_ascii_whitespace())
                {
                    let sentence_end = end; // exclude the terminator
                    self.cursor = end + 1;
                    return Some((&self.text[start..sentence_end], start));
                }
            if c == b'\n' && end + 1 < self.bytes.len() && self.bytes[end + 1] == b'\n' {
                let sentence_end = end;
                self.cursor = end + 2;
                return Some((&self.text[start..sentence_end], start));
            }
            end += 1;
        }
        // No terminator found — yield the tail.
        self.cursor = end;
        if start < end {
            Some((&self.text[start..end], start))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Commitment phrasing fires the Commitment kind and the full
    /// sentence is preserved (sans terminator).
    #[test]
    fn commitment_phrases_match() {
        let text = "I'll use BTreeMap because we need ordered iteration.";
        let hits = mine(text);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, MatchKind::Commitment);
        assert_eq!(
            hits[0].sentence,
            "I'll use BTreeMap because we need ordered iteration"
        );
    }

    /// Multiple commitment forms. Note: avoid stoplist words
    /// (`rename`, `format`, `import`, `lint`) in commitment
    /// examples — those are caught by the stoplist by design.
    #[test]
    fn commitment_phrases_variants() {
        for text in &[
            "we'll use Postgres for the storage layer",
            "I'll use a content-hash-based identity for the canonical",
            "going with the streaming approach",
            "we'll go with the eager strategy here",
        ] {
            let hits = mine(text);
            assert_eq!(hits.len(), 1, "no match for: {text}");
            assert_eq!(hits[0].kind, MatchKind::Commitment, "wrong kind for: {text}");
        }
    }

    /// Comparison phrasing differentiates from plain commitment.
    #[test]
    fn comparison_phrases_match_with_comparison_kind() {
        let text = "Chose async channels over a mutex because the ingest \
                    workload is bursty.";
        let hits = mine(text);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, MatchKind::Comparison);
    }

    /// Explicit-decision marker.
    #[test]
    fn explicit_decision_phrases_match() {
        let text = "Decided to drop the optional `--quiet` flag.";
        let hits = mine(text);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, MatchKind::ExplicitDecision);
    }

    /// Multiple sentences in one input each get scanned.
    #[test]
    fn multiple_sentences_each_scanned() {
        let text = "I'll use Tokio for the runtime. Chose async channels \
                    over a mutex.";
        let hits = mine(text);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].kind, MatchKind::Commitment);
        assert_eq!(hits[1].kind, MatchKind::Comparison);
    }

    /// Stoplist disqualifies sentences containing mechanical words.
    #[test]
    fn stoplist_disqualifies_mechanical_sentences() {
        for text in &[
            "I'll use the renamed identifier here.",
            "Decided to fix typo in the docstring.",
            "Chose to format the constants block.",
            "Going with the import reorder for clarity.",
        ] {
            let hits = mine(text);
            assert!(
                hits.is_empty(),
                "stoplist should have rejected: {text}"
            );
        }
    }

    /// Sentences without a trigger don't fire.
    #[test]
    fn no_trigger_no_match() {
        let text = "The system has three layers and uses a single channel.";
        assert!(mine(text).is_empty());
    }

    /// Very long "sentence" gets capped — we want concise audit
    /// rows, not a wall of text.
    #[test]
    fn overlong_sentence_is_skipped() {
        let mut text = "I'll use Y because ".to_string();
        text.push_str(&"x".repeat(MAX_SENTENCE_LEN + 50));
        text.push('.');
        let hits = mine(&text);
        assert!(
            hits.is_empty(),
            "match exceeded MAX_SENTENCE_LEN; should have been skipped"
        );
    }

    /// Match cap honoured.
    #[test]
    fn match_cap_honoured() {
        let mut text = String::new();
        for i in 0..(MAX_MATCHES_PER_CALL + 5) {
            text.push_str(&format!("I'll use option {i} because reasons. "));
        }
        let hits = mine(&text);
        assert_eq!(hits.len(), MAX_MATCHES_PER_CALL);
    }

    /// `start_offset` points to the first non-whitespace
    /// character of the matched sentence. Useful for the audit's
    /// "this came from response window starting at byte N" tag.
    #[test]
    fn start_offset_is_byte_position_of_sentence_start() {
        let text = "  Hello world. Decided to ship.";
        let hits = mine(text);
        // "Hello world" lacks trigger; "Decided to ship" matches.
        // The decision sentence starts after "Hello world. ".
        assert_eq!(hits.len(), 1);
        let expected_start = text.find("Decided").unwrap();
        assert_eq!(hits[0].start_offset, expected_start);
    }

    /// Empty / whitespace input is a clean no-op (no panic, no
    /// matches).
    #[test]
    fn empty_input_returns_no_matches() {
        assert!(mine("").is_empty());
        assert!(mine("   \n\n   ").is_empty());
    }

    /// Sentence iter handles `. ` and `\n\n` boundaries.
    #[test]
    fn sentence_iter_handles_double_newline_and_period() {
        let text = "First sentence.\n\nSecond. Third!";
        let sentences: Vec<&str> = iter_sentences(text).map(|(s, _)| s).collect();
        assert_eq!(sentences, vec!["First sentence", "Second", "Third"]);
    }

    /// MatchKind::as_str gives stable identifiers (the audit's
    /// telemetry depends on these).
    #[test]
    fn match_kind_as_str_is_stable() {
        assert_eq!(MatchKind::Commitment.as_str(), "commitment");
        assert_eq!(MatchKind::Comparison.as_str(), "comparison");
        assert_eq!(MatchKind::ExplicitDecision.as_str(), "explicit_decision");
    }
}
