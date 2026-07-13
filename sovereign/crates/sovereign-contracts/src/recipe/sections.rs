// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pluggable section detection over raw text.
//!
//! The *detector* half of what used to be `corpus_engine::chunkers::sectioned`:
//! it locates section boundaries in a document (via a line-anchored regex or an
//! author-supplied Table of Contents) without any chunking, embedding, or
//! storage machinery. The `SectionedChunker` that pairs a detector with a
//! `ParagraphChunker` stays in `corpus-engine` (it depends on that crate's
//! chunker internals); this crate owns only the pure, leaf-dependency detectors
//! so the workflow `SectionTool` and the bespoke ingest path share one
//! implementation.
//!
//! `corpus-engine` re-exports these at `corpus_engine::chunkers::sectioned::*`,
//! so its existing callers are unaffected — this is a pure relocation.

use std::collections::HashMap;

use regex::Regex;

/// A section boundary located inside a raw text document.
///
/// The detector defines `start_byte`/`end_byte` as the bounds of the
/// section *body* (everything after the heading, up to the start of
/// the next heading or EOF). `metadata` carries detector-specific
/// fields that the `ChapterManifest` builder can promote into
/// structured columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedSection {
    /// Detector-assigned section id, unique within the document.
    pub id: String,
    /// Heading text as matched in the document.
    pub title: String,
    /// Byte offset where the section *body* starts (after the heading).
    pub start_byte: usize,
    /// Byte offset where the body ends (next heading or EOF).
    pub end_byte: usize,
    /// Detector-specific extras the `ChapterManifest` builder can promote to columns.
    pub metadata: HashMap<String, String>,
}

/// Pluggable section detector.
///
/// Implementations return sections in document order. Callers that
/// pick the detector dynamically (from config, say) can hold one as
/// `Box<dyn SectionDetector>` and pass it to `SectionedChunker`
/// unchanged, thanks to the blanket `Box` impl below.
pub trait SectionDetector: Send + Sync {
    /// Locate sections in `text`, returned in document order.
    fn detect(&self, text: &str) -> Vec<DetectedSection>;
}

impl SectionDetector for Box<dyn SectionDetector> {
    fn detect(&self, text: &str) -> Vec<DetectedSection> {
        (**self).detect(text)
    }
}

/// Regex-based section detector over plaintext corpora.
///
/// The regex is caller-supplied; the default pattern happens to
/// recognise `Chapter`/`Part` forms common in prose books, but there
/// is nothing book-specific below that. Markdown headers, journal
/// date stamps, protocol message boundaries — any line-anchored
/// regex works.
pub struct ChapterRegexDetector {
    pattern: Regex,
    /// Minimum whitespace-separated token count a match's *body* must
    /// have to survive. `0` (the default) emits every regex match.
    ///
    /// Heading regexes routinely match in two places: the heading
    /// itself and any list-of-headings printed earlier in the
    /// document (a Table of Contents, a navigation index, an
    /// "also in this volume" block). The second kind has no body
    /// to analyse and poisons every downstream step that assumes a
    /// section equals substantive content. This threshold is the
    /// structural guard: if the body is shorter than N words, the
    /// match is treated as a phantom and dropped.
    ///
    /// The right N is corpus-dependent (a poetry anthology's
    /// sections may be 20 words; a code-module index's may be 200).
    /// The detector takes the value; the *choice* lives with the
    /// caller — typically in config so operators can tune it.
    min_body_words: usize,
}

impl ChapterRegexDetector {
    /// Line-anchored `Chapter`/`Part` heading pattern (Roman or Arabic numerals).
    pub const DEFAULT_PATTERN: &'static str =
        r"(?m)^\s*(Chapter|CHAPTER|Part|PART)\s+([IVXLCMivxlcm\d]+)\b.*$";

    /// Detector with `DEFAULT_PATTERN` and no body-length filter.
    pub fn new() -> Self {
        Self::with_pattern(Self::DEFAULT_PATTERN).expect("DEFAULT_PATTERN must compile")
    }

    /// Detector with a caller-supplied line-anchored regex; `Err` when the regex doesn't compile.
    pub fn with_pattern(pat: &str) -> Result<Self, regex::Error> {
        Ok(Self {
            pattern: Regex::new(pat)?,
            min_body_words: 0,
        })
    }

    /// Enable the min-body-words filter. Matches whose body has fewer
    /// than `n` words are dropped from the output, and the surviving
    /// sections are re-numbered so their `sec_NNNN` ids stay dense.
    pub fn with_min_body_words(mut self, n: usize) -> Self {
        self.min_body_words = n;
        self
    }
}

impl Default for ChapterRegexDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl SectionDetector for ChapterRegexDetector {
    fn detect(&self, text: &str) -> Vec<DetectedSection> {
        let matches: Vec<(usize, usize, String)> = self
            .pattern
            .find_iter(text)
            .map(|m| (m.start(), m.end(), m.as_str().trim().to_string()))
            .collect();

        if matches.is_empty() {
            return Vec::new();
        }

        let mut sections: Vec<DetectedSection> = Vec::with_capacity(matches.len());
        let mut dropped_low_body = 0usize;
        for (i, (start, heading_end, heading)) in matches.iter().enumerate() {
            let body_start = *heading_end;
            let body_end = if i + 1 < matches.len() {
                matches[i + 1].0
            } else {
                text.len()
            };
            let body_end = body_end.max(body_start);

            if self.min_body_words > 0 {
                let body = &text[body_start..body_end];
                if body.split_whitespace().count() < self.min_body_words {
                    dropped_low_body += 1;
                    continue;
                }
            }

            let mut metadata = HashMap::new();
            metadata.insert("heading_start_byte".to_string(), start.to_string());
            // Ordinal reflects survivor rank, not raw regex index, so
            // it stays dense after filtering.
            metadata.insert("ordinal".to_string(), (sections.len() + 1).to_string());

            sections.push(DetectedSection {
                id: format!("sec_{:04}", sections.len() + 1),
                title: heading.clone(),
                start_byte: body_start,
                end_byte: body_end,
                metadata,
            });
        }
        if dropped_low_body > 0 {
            // Glassbox: when the filter fires, an operator should be
            // able to see *why* their manifest has fewer sections
            // than the regex matched, without attaching a debugger.
            tracing::debug!(
                matched = matches.len(),
                kept = sections.len(),
                dropped = dropped_low_body,
                min_body_words = self.min_body_words,
                "chunker: dropped low-body heading matches"
            );
        }
        sections
    }
}

/// Detect sections from an author-supplied Table of Contents.
///
/// The operator writes their ToC between two markers (default
/// `[[CONTENTS]]` / `[[/CONTENTS]]`), one section title per line. The
/// detector reads those titles verbatim and anchors each section to
/// the *next* line-start occurrence of that title after the end
/// marker. This trades regex-tuning for a small authorial discipline:
/// the manuscript declares its own sections, the pipeline honours the
/// declaration exactly.
///
/// Unlike `ChapterRegexDetector`, this never introduces phantom
/// sections — a title that appears in the ToC but not in the body
/// is surfaced as a warning, not silently dropped.
pub struct TocAnchoredDetector {
    start_marker: String,
    end_marker: String,
}

impl TocAnchoredDetector {
    /// Default marker opening the inlined table-of-contents block.
    pub const DEFAULT_START: &'static str = "[[CONTENTS]]";
    /// Default marker closing the inlined table-of-contents block.
    pub const DEFAULT_END: &'static str = "[[/CONTENTS]]";

    /// Detector using the default `[[CONTENTS]]` markers.
    pub fn new() -> Self {
        Self::with_markers(Self::DEFAULT_START, Self::DEFAULT_END)
    }

    /// Detector with caller-supplied start/end markers.
    pub fn with_markers(start: impl Into<String>, end: impl Into<String>) -> Self {
        Self {
            start_marker: start.into(),
            end_marker: end.into(),
        }
    }
}

impl Default for TocAnchoredDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl SectionDetector for TocAnchoredDetector {
    fn detect(&self, text: &str) -> Vec<DetectedSection> {
        // Locate the ToC block. Both markers are required — a file
        // with only the start marker could otherwise eat the whole
        // manuscript as a title list.
        let Some(start_idx) = text.find(&self.start_marker) else {
            tracing::warn!(
                start_marker = %self.start_marker,
                "toc_detector: start marker not found; emitting zero sections"
            );
            return Vec::new();
        };
        let after_start = start_idx + self.start_marker.len();
        let Some(end_offset) = text[after_start..].find(&self.end_marker) else {
            tracing::warn!(
                end_marker = %self.end_marker,
                "toc_detector: end marker not found after start; emitting zero sections"
            );
            return Vec::new();
        };
        let end_idx = after_start + end_offset;
        let toc_block = &text[after_start..end_idx];
        let body_start = end_idx + self.end_marker.len();

        let titles: Vec<String> = toc_block
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        if titles.is_empty() {
            tracing::warn!("toc_detector: ToC block was empty; emitting zero sections");
            return Vec::new();
        }

        // For each title, find its next occurrence AT A LINE START in
        // the body. We scan sequentially so a title appearing twice
        // in the body (rare, but possible) produces two sections —
        // the caller's min-body-words filter handles the phantom case.
        let body = &text[body_start..];
        let mut heads: Vec<(usize, String)> = Vec::with_capacity(titles.len());
        let mut search_from = 0usize;
        let mut missing: Vec<String> = Vec::new();
        for title in &titles {
            match find_line_anchored(body, title, search_from) {
                Some(abs) => {
                    // `abs` is a body-relative byte index at a line
                    // start. Translate to source coordinates.
                    heads.push((body_start + abs, title.clone()));
                    search_from = abs + title.len();
                }
                None => missing.push(title.clone()),
            }
        }
        if !missing.is_empty() {
            tracing::warn!(
                missing_titles = ?missing,
                "toc_detector: titles in ToC not found in body"
            );
        }

        // Build sections: body spans from just after each heading
        // line to the start of the next heading (or EOF).
        let mut sections: Vec<DetectedSection> = Vec::with_capacity(heads.len());
        for (i, (heading_start, title)) in heads.iter().enumerate() {
            // Heading body starts after the heading line.
            let heading_end = text[*heading_start..]
                .find('\n')
                .map(|off| heading_start + off + 1)
                .unwrap_or(text.len());
            let body_end = if i + 1 < heads.len() {
                heads[i + 1].0
            } else {
                text.len()
            };
            let body_end = body_end.max(heading_end);

            let mut metadata = HashMap::new();
            metadata.insert("heading_start_byte".to_string(), heading_start.to_string());
            metadata.insert("ordinal".to_string(), (i + 1).to_string());

            sections.push(DetectedSection {
                id: format!("sec_{:04}", i + 1),
                title: title.clone(),
                start_byte: heading_end,
                end_byte: body_end,
                metadata,
            });
        }
        sections
    }
}

/// Find the next byte index in `haystack`, at or after `from`, where
/// `needle` appears at a line start (preceded by start-of-text or
/// `\n`) and is followed by `\n` or end-of-text (trailing whitespace
/// is tolerated). Returns the absolute index within `haystack` of
/// the first needle byte.
fn find_line_anchored(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    let mut search_start = from;
    while search_start <= haystack.len() {
        let idx = haystack[search_start..].find(needle)?;
        let abs = search_start + idx;
        let preceded_by_line_start = abs == 0 || haystack.as_bytes().get(abs - 1) == Some(&b'\n');
        let tail = &haystack[abs + needle.len()..];
        let followed_by_line_end = tail
            .chars()
            .next()
            .map(|c| c == '\n' || c == '\r' || c.is_whitespace())
            .unwrap_or(true);
        if preceded_by_line_start && followed_by_line_end {
            // Also require that the trailing whitespace on this line
            // is just whitespace (no other text), so "SHERWOOD 1 was
            // a good day" doesn't match the title "SHERWOOD 1".
            let line_tail_end = tail.find('\n').unwrap_or(tail.len());
            if tail[..line_tail_end].trim().is_empty() {
                return Some(abs);
            }
        }
        search_start = abs + needle.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gutenberg_like(chapters: usize) -> String {
        let mut s = String::new();
        s.push_str("PREAMBLE: This should not be captured.\n\n");
        for i in 1..=chapters {
            s.push_str(&format!("Chapter {i}\n\n"));
            s.push_str(&format!(
                "This is the body of chapter {i}. It has two paragraphs.\n\n"
            ));
            s.push_str(&format!(
                "And here is the second paragraph of chapter {i}, still inside it.\n\n"
            ));
        }
        s
    }

    #[test]
    fn chapter_regex_detector_splits_gutenberg_text() {
        let text = gutenberg_like(3);
        let det = ChapterRegexDetector::new();
        let secs = det.detect(&text);
        assert_eq!(secs.len(), 3, "expected 3 chapters, got {}", secs.len());
        assert_eq!(secs[0].id, "sec_0001");
        assert_eq!(secs[2].id, "sec_0003");
        assert!(secs[0].title.starts_with("Chapter 1"));
        // Non-decreasing bounds
        for i in 0..secs.len() - 1 {
            assert!(secs[i].end_byte <= secs[i + 1].start_byte);
        }
    }

    #[test]
    fn detector_filter_drops_low_body_matches_and_renumbers() {
        // A regex that matches a heading in two places — once in a
        // list-of-headings index with no body, once at the real
        // content below. The filter must drop the body-less matches
        // and re-number the survivors so their ids stay dense.
        let mut text = String::new();
        text.push_str("Index\n\n");
        text.push_str(" Chapter I. First\n");
        text.push_str(" Chapter II. Second\n");
        text.push_str(" Chapter III. Third\n\n");
        let filler = " body word".repeat(60);
        text.push_str(&format!("Chapter I. First\n\n{filler}\n\n"));
        text.push_str(&format!("Chapter II. Second\n\n{filler}\n\n"));
        text.push_str(&format!("Chapter III. Third\n\n{filler}\n\n"));

        let unfiltered = ChapterRegexDetector::new().detect(&text);
        assert_eq!(
            unfiltered.len(),
            6,
            "unfiltered detector must emit every match"
        );

        let filtered = ChapterRegexDetector::new()
            .with_min_body_words(40)
            .detect(&text);
        assert_eq!(
            filtered.len(),
            3,
            "filter must drop the 3 body-less matches"
        );
        assert_eq!(filtered[0].id, "sec_0001");
        assert_eq!(filtered[2].id, "sec_0003");
        assert_eq!(
            filtered[0].metadata.get("ordinal").map(|s| s.as_str()),
            Some("1")
        );
        assert_eq!(
            filtered[2].metadata.get("ordinal").map(|s| s.as_str()),
            Some("3")
        );
    }

    #[test]
    fn detector_filter_off_by_default_preserves_all_matches() {
        let text = gutenberg_like(3);
        let det = ChapterRegexDetector::new();
        assert_eq!(det.detect(&text).len(), 3);
    }

    #[test]
    fn toc_anchored_detector_uses_titles_verbatim_from_contents_block() {
        let text = "\
[[CONTENTS]]
SHERWOOD 1
MANNY 1
GENESIS
[[/CONTENTS]]

SHERWOOD 1

Body prose for the first section goes here and continues for a while.

MANNY 1

Manny's body prose lives here with enough words to be a real section.

GENESIS

The closing section has its own body prose too.
";
        let det = TocAnchoredDetector::new();
        let secs = det.detect(text);
        assert_eq!(secs.len(), 3);
        assert_eq!(secs[0].title, "SHERWOOD 1");
        assert_eq!(secs[1].title, "MANNY 1");
        assert_eq!(secs[2].title, "GENESIS");
        // Body of the first section should start with the prose, not
        // the heading line itself.
        let body1 = &text[secs[0].start_byte..secs[0].end_byte];
        assert!(
            body1.trim_start().starts_with("Body prose"),
            "got: {body1:?}"
        );
    }

    #[test]
    fn toc_anchored_detector_emits_empty_when_markers_missing() {
        let text = "No markers here. Just free-form prose.\n";
        let det = TocAnchoredDetector::new();
        assert!(det.detect(text).is_empty());
    }

    #[test]
    fn toc_anchored_detector_flags_missing_body_anchor_but_keeps_others() {
        let text = "\
[[CONTENTS]]
ALPHA
BETA
[[/CONTENTS]]

ALPHA

Body prose for alpha.
";
        // BETA appears in the ToC but not in the body. ALPHA still
        // produces a valid section; BETA is logged + dropped.
        let det = TocAnchoredDetector::new();
        let secs = det.detect(text);
        assert_eq!(secs.len(), 1);
        assert_eq!(secs[0].title, "ALPHA");
    }

    #[test]
    fn toc_anchored_detector_respects_line_anchoring() {
        // A title that's a substring of a body sentence must not
        // match as a heading. ALPHA appears mid-line in the second
        // paragraph; only the standalone heading line should anchor.
        let text = "\
[[CONTENTS]]
ALPHA
[[/CONTENTS]]

Incidental prose: ALPHA is sometimes mentioned inline.

ALPHA

The real body starts here.
";
        let det = TocAnchoredDetector::new();
        let secs = det.detect(text);
        assert_eq!(secs.len(), 1);
        let body = &text[secs[0].start_byte..secs[0].end_byte];
        assert!(
            body.trim_start().starts_with("The real body starts here."),
            "got: {body:?}"
        );
    }

    #[test]
    fn toc_anchored_detector_custom_markers() {
        let text = "\
BEGIN_TOC
One
Two
END_TOC

One

Body one.

Two

Body two.
";
        let det = TocAnchoredDetector::with_markers("BEGIN_TOC", "END_TOC");
        let secs = det.detect(text);
        assert_eq!(secs.len(), 2);
        assert_eq!(secs[0].title, "One");
        assert_eq!(secs[1].title, "Two");
    }

    #[test]
    fn detector_returns_empty_when_no_matches() {
        let text = "This is plain prose with no chapter markers at all.\n\n\
                    Another paragraph, still no chapters.";
        let det = ChapterRegexDetector::new();
        assert!(det.detect(text).is_empty());
    }

    #[test]
    fn custom_regex_override_works() {
        let text = "intro\n\n## First H2\nbody of first\n\n## Second H2\nbody of second\n";
        let det = ChapterRegexDetector::with_pattern(r"(?m)^##\s+.*$").unwrap();
        let secs = det.detect(text);
        assert_eq!(secs.len(), 2);
        assert!(secs[0].title.starts_with("## First H2"));
    }

    #[test]
    fn roman_and_word_numerals_both_detected() {
        let text = "CHAPTER I\nfirst\n\nCHAPTER 2\nsecond\n\nChapter iii\nthird\n";
        let det = ChapterRegexDetector::new();
        let secs = det.detect(text);
        assert_eq!(secs.len(), 3);
    }
}
