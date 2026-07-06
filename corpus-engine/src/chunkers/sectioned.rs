// SPDX-License-Identifier: AGPL-3.0-or-later
//! Section-aware chunking.
//!
//! Unlike `ParagraphChunker`, this splits text into sections first
//! (via a pluggable `SectionDetector`), then into paragraph-sized
//! chunks inside each section. Every emitted chunk carries the
//! containing section's id + title, so downstream phases can group
//! paragraphs back to their source section without re-parsing the
//! document.
//!
//! Used by the v2 enrichment pipeline to feed chapter-level inputs
//! into phase 1 (per-chapter question extraction) while still keeping
//! paragraph-level chunks for embedding-based clustering.
//!
//! The pure detector half (`SectionDetector`, `DetectedSection`,
//! `ChapterRegexDetector`, `TocAnchoredDetector`) lives in
//! `sovereign_contracts::recipe::sections` so the workflow `SectionTool`
//! and this bespoke ingest path share one segmentation implementation.
//! It is re-exported below at the historical path, so existing callers
//! (`corpus_engine::chunkers::sectioned::*`) are unaffected. Only the
//! `SectionedChunker` — which pairs a detector with this crate's
//! `ParagraphChunker` — stays here.

use std::collections::HashMap;

use super::paragraph::ParagraphChunker;
use super::{floor_char_boundary, Chunker};

pub use sovereign_contracts::recipe::sections::{
    ChapterRegexDetector, DetectedSection, SectionDetector, TocAnchoredDetector,
};

/// A paragraph chunk annotated with its containing section.
#[derive(Debug, Clone)]
pub struct SectionedChunk {
    pub content: String,
    /// Global 0-based index across the whole document.
    pub index: usize,
    pub section_id: String,
    pub section_title: String,
    /// 0-based index of this paragraph inside its section.
    pub paragraph_index: usize,
    /// Carried verbatim from the detector.
    pub metadata: HashMap<String, String>,
}

/// Chunks a document by section, then by paragraph within each section.
///
/// Generic over the detector so tests can swap in a stub without the
/// regex cost, and so future detectors (markdown, journal-by-date)
/// can be composed in.
pub struct SectionedChunker<D: SectionDetector> {
    detector: D,
    paragraph: ParagraphChunker,
}

impl<D: SectionDetector> SectionedChunker<D> {
    pub fn new(detector: D, paragraph: ParagraphChunker) -> Self {
        Self {
            detector,
            paragraph,
        }
    }

    pub fn with_detector(detector: D) -> Self {
        Self::new(detector, ParagraphChunker::default())
    }

    /// Detected sections without emitting chunks. Useful for
    /// `enrich init --dry-run` so the user can verify segmentation
    /// before committing to an ingest.
    pub fn dry_run(&self, text: &str) -> SectionReport {
        let sections = self.detector.detect(text);
        SectionReport {
            total: sections.len(),
            sections,
        }
    }

    pub fn chunk(&self, text: &str) -> Vec<SectionedChunk> {
        let sections = self.detector.detect(text);
        if sections.is_empty() {
            return Vec::new();
        }

        let mut out = Vec::new();
        let mut global_index = 0usize;
        for section in &sections {
            let start = floor_char_boundary(text, section.start_byte);
            let end = floor_char_boundary(text, section.end_byte);
            if end <= start {
                continue;
            }
            let section_text = &text[start..end];
            let paragraphs = self.paragraph.chunk(section_text);
            for (para_idx, para) in paragraphs.into_iter().enumerate() {
                out.push(SectionedChunk {
                    content: para.content,
                    index: global_index,
                    section_id: section.id.clone(),
                    section_title: section.title.clone(),
                    paragraph_index: para_idx,
                    metadata: section.metadata.clone(),
                });
                global_index += 1;
            }
        }
        out
    }
}

/// Summary of what a detector found without running the full chunker.
#[derive(Debug, Clone)]
pub struct SectionReport {
    pub total: usize,
    pub sections: Vec<DetectedSection>,
}

impl SectionReport {
    /// Human-readable summary used by `enrich init --dry-run`.
    pub fn format_summary(&self, text: &str) -> String {
        let mut out = format!("Detected {} section(s):\n", self.total);
        let shown = self.sections.len().min(20);
        for (i, sec) in self.sections.iter().take(shown).enumerate() {
            let body_end = sec.end_byte.min(text.len());
            let body_start = sec.start_byte.min(body_end);
            let first_line = text[body_start..body_end]
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .chars()
                .take(80)
                .collect::<String>();
            out.push_str(&format!("  {:>3}. {} — {}\n", i + 1, sec.title, first_line));
        }
        if self.sections.len() > shown {
            out.push_str(&format!("  ... and {} more\n", self.sections.len() - shown));
        }
        out
    }
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
    fn sectioned_chunker_attaches_section_id_to_paragraphs() {
        let text = gutenberg_like(2);
        let chunker = SectionedChunker::with_detector(ChapterRegexDetector::new());
        let chunks = chunker.chunk(&text);
        // Two chapters × two paragraphs each, but ParagraphChunker
        // coalesces small paragraphs into one chunk when they fit.
        // What we need to verify: every chunk has a section_id, and the
        // two chapters produce distinct section_ids.
        assert!(!chunks.is_empty());
        let ids: std::collections::HashSet<_> =
            chunks.iter().map(|c| c.section_id.as_str()).collect();
        assert_eq!(ids.len(), 2, "expected chunks spanning 2 sections");
        for chunk in &chunks {
            assert!(!chunk.section_id.is_empty());
            assert!(chunk.section_title.starts_with("Chapter"));
        }
    }

    #[test]
    fn sectioned_chunker_global_index_is_monotonic() {
        let text = gutenberg_like(3);
        let chunker = SectionedChunker::with_detector(ChapterRegexDetector::new());
        let chunks = chunker.chunk(&text);
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
        }
    }

    #[test]
    fn sectioned_chunker_long_chapter_splits_into_many_paragraphs() {
        // One big chapter that will force paragraph splitting.
        let long_para = "A long repeated paragraph sentence that fills a lot of bytes. ".repeat(80);
        let text = format!(
            "Chapter 1\n\n{long_para}\n\n{long_para}\n\n{long_para}\n\nChapter 2\n\ntiny body\n"
        );
        let chunker =
            SectionedChunker::new(ChapterRegexDetector::new(), ParagraphChunker::new(512, 64));
        let chunks = chunker.chunk(&text);
        let ch1: Vec<_> = chunks
            .iter()
            .filter(|c| c.section_id == "sec_0001")
            .collect();
        let ch2: Vec<_> = chunks
            .iter()
            .filter(|c| c.section_id == "sec_0002")
            .collect();
        assert!(
            ch1.len() >= 3,
            "long chapter should produce multiple paragraph chunks, got {}",
            ch1.len()
        );
        assert_eq!(
            ch2.len(),
            1,
            "tiny chapter should produce exactly one chunk"
        );
        // paragraph_index is per-section and starts at 0.
        assert_eq!(ch1[0].paragraph_index, 0);
        assert_eq!(ch2[0].paragraph_index, 0);
    }

    #[test]
    fn dry_run_reports_detected_sections_without_chunking() {
        let text = gutenberg_like(5);
        let chunker = SectionedChunker::with_detector(ChapterRegexDetector::new());
        let report = chunker.dry_run(&text);
        assert_eq!(report.total, 5);
        let summary = report.format_summary(&text);
        assert!(summary.contains("Detected 5 section(s)"));
        assert!(summary.contains("Chapter 1"));
    }

    #[test]
    fn chunker_returns_empty_on_no_matches() {
        let text = "just prose, no chapter markers";
        let chunker = SectionedChunker::with_detector(ChapterRegexDetector::new());
        assert!(chunker.chunk(text).is_empty());
    }

    #[test]
    fn utf8_safe_at_section_bounds() {
        // A chapter heading followed by a body containing multibyte
        // characters (curly quotes, em-dashes). The floor_char_boundary
        // in SectionedChunker::chunk must keep us on char boundaries
        // even if the detector's bounds land mid-char — which they
        // shouldn't, but we defend anyway.
        let text = "Chapter 1\n\n\u{201C}Hello\u{2014}world\u{201D} she said.\n\nChapter 2\nend\n";
        let chunker = SectionedChunker::with_detector(ChapterRegexDetector::new());
        let chunks = chunker.chunk(text);
        assert!(!chunks.is_empty());
        assert!(chunks[0].content.contains("Hello"));
    }

    #[test]
    fn section_metadata_propagates_to_chunks() {
        let text = gutenberg_like(2);
        let chunker = SectionedChunker::with_detector(ChapterRegexDetector::new());
        let chunks = chunker.chunk(&text);
        let first = &chunks[0];
        assert_eq!(first.metadata.get("ordinal").map(String::as_str), Some("1"));
        let last = chunks.last().unwrap();
        assert_eq!(last.metadata.get("ordinal").map(String::as_str), Some("2"));
    }
}
