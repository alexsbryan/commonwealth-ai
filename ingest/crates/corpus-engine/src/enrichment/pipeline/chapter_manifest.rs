// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-corpus chapter manifest — the WRITE half.
//!
//! Lives at `~/.svrnmesh/indexes/<corpus>/chapters.json` — corpus
//! state, not enrichment state, so it's in the index root alongside
//! `_corpus_meta.json` rather than under the enrichment tree.
//!
//! Built at `enrich init` time from `DetectedSection`s emitted by a
//! `SectionedChunker`. The pipeline's phase 1 run adds
//! `characters_present` back into each entry so subsequent phases
//! can name thematic carriers.
//!
//! The type and its read half (`load`, accessors) live in the atlas-reader
//! leaf since fp-60 (FIVE_PROGRAMS §12 decision 1) and are re-exported here.
//! A type's inherent methods cannot span two crates, so the writes are
//! [`ChapterManifestWrite`] — bring it into scope to call them.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

pub use corpus_engine_atlas_reader::chapter_manifest::{ChapterEntry, ChapterManifest};

use crate::chunkers::sectioned::DetectedSection;
use crate::enrichment::pipeline::types::is_placeholder_literal;
use crate::error::{Error, Result};

/// The manifest's writes: the atomic save, the build from detected sections,
/// and the `characters_present` merge.
pub trait ChapterManifestWrite {
    /// Atomic save via tmp + rename.
    fn save(&self, path: &Path) -> Result<()>;

    /// Build a manifest from `SectionedChunker`-detected sections + the
    /// raw text they point into.
    fn from_detected_sections(
        corpus_id: impl Into<String>,
        text: &str,
        sections: &[DetectedSection],
    ) -> Self
    where
        Self: Sized;

    /// Merge a batch of `characters_present` into one chapter.
    fn merge_characters_present(&mut self, chapter_id: &str, new: &[String]) -> Result<()>;
}

impl ChapterManifestWrite for ChapterManifest {
    /// Atomic save via tmp + rename.
    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json =
            serde_json::to_string_pretty(self).map_err(|e| Error::Serialization(e.to_string()))?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Build a manifest from `SectionedChunker`-detected sections + the
    /// raw text they point into. `text` is the original plaintext; the
    /// manifest uses it to compute `first_line` and `word_count`.
    fn from_detected_sections(
        corpus_id: impl Into<String>,
        text: &str,
        sections: &[DetectedSection],
    ) -> Self
    where
        Self: Sized,
    {
        let mut m = Self::new(corpus_id);
        for sec in sections {
            let body_start = sec.start_byte.min(text.len());
            let body_end = sec.end_byte.min(text.len()).max(body_start);
            let body = &text[body_start..body_end];
            let first_line = body
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .chars()
                .take(120)
                .collect::<String>();
            let word_count = body.split_whitespace().count() as u64;

            let mut meta = std::collections::BTreeMap::new();
            for (k, v) in &sec.metadata {
                meta.insert(k.clone(), v.clone());
            }

            m.chapters.push(ChapterEntry {
                id: sec.id.clone(),
                title: sec.title.clone(),
                part: parse_hierarchy(&sec.title, "Part"),
                chapter: parse_hierarchy(&sec.title, "Chapter"),
                first_line,
                word_count,
                chunk_ids: Vec::new(),
                characters_present: Vec::new(),
                metadata: meta,
            });
        }
        m
    }

    /// Merge a batch of `characters_present` into one chapter, preserving
    /// existing entries and deduplicating case-insensitively. Silently
    /// drops placeholder literals (`"..."`, `"…"`, `TODO`) — letting
    /// one slip through once permanently contaminates the manifest for
    /// this corpus, and future runs have no way to tell a real
    /// character named `"..."` from a schema echo.
    fn merge_characters_present(&mut self, chapter_id: &str, new: &[String]) -> Result<()> {
        let entry = self
            .get_mut(chapter_id)
            .ok_or_else(|| Error::InvalidInput(format!("chapter not found: {chapter_id}")))?;
        // First, scrub any placeholder entries that a prior (pre-fix)
        // run may have persisted. This turns every merge call into an
        // opportunity to self-heal.
        entry
            .characters_present
            .retain(|name| !is_placeholder_literal(name));
        let mut seen: BTreeSet<String> = entry
            .characters_present
            .iter()
            .map(|s| s.to_lowercase())
            .collect();
        for name in new {
            let trimmed = name.trim();
            if trimmed.is_empty() || is_placeholder_literal(trimmed) {
                continue;
            }
            let key = trimmed.to_lowercase();
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            entry.characters_present.push(trimmed.to_string());
        }
        Ok(())
    }
}

/// Best-effort extraction of a structured hierarchy number from a
/// detector-captured heading like `"Part 3, Chapter 12"`. Returns
/// `None` when the marker is absent or the number is non-numeric.
fn parse_hierarchy(title: &str, marker: &str) -> Option<u32> {
    let lower = title.to_lowercase();
    let marker_lower = marker.to_lowercase();
    let idx = lower.find(&marker_lower)?;
    let after = &title[idx + marker.len()..];
    // Skip separators; then take the next alphanumeric run.
    let after = after.trim_start_matches(|c: char| c.is_whitespace() || c == '.' || c == ':');
    let first_token: String = after.chars().take_while(|c| c.is_alphanumeric()).collect();
    if let Ok(n) = first_token.parse::<u32>() {
        return Some(n);
    }
    // Roman numeral fallback.
    roman_to_u32(&first_token)
}

fn roman_to_u32(s: &str) -> Option<u32> {
    let s = s.to_uppercase();
    let mut total: u32 = 0;
    let mut prev: u32 = 0;
    for c in s.chars().rev() {
        let v = match c {
            'I' => 1,
            'V' => 5,
            'X' => 10,
            'L' => 50,
            'C' => 100,
            'D' => 500,
            'M' => 1000,
            _ => return None,
        };
        if v < prev {
            total = total.saturating_sub(v);
        } else {
            total = total.saturating_add(v);
            prev = v;
        }
    }
    if total == 0 {
        None
    } else {
        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn section(id: &str, title: &str, body_start: usize, body_end: usize) -> DetectedSection {
        let mut meta = HashMap::new();
        meta.insert("ordinal".into(), id.trim_start_matches("sec_").into());
        DetectedSection {
            id: id.into(),
            title: title.into(),
            start_byte: body_start,
            end_byte: body_end,
            metadata: meta,
        }
    }

    #[test]
    fn manifest_from_detected_sections_populates_structured_fields() {
        let text = "Part 1, Chapter 1\n\nHappy families are all alike.\n\n\
                    Part 1, Chapter 2\n\nAnother body line here.";
        let secs = vec![
            section("sec_0001", "Part 1, Chapter 1", 17, 62),
            section("sec_0002", "Part 1, Chapter 2", 79, text.len()),
        ];
        let m = ChapterManifest::from_detected_sections("ak", text, &secs);
        assert_eq!(m.chapters.len(), 2);
        assert_eq!(m.chapters[0].part, Some(1));
        assert_eq!(m.chapters[0].chapter, Some(1));
        assert_eq!(m.chapters[1].chapter, Some(2));
        assert!(m.chapters[0].first_line.contains("Happy families"));
        assert!(m.chapters[0].word_count > 0);
    }

    #[test]
    fn manifest_save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("chapters.json");
        let mut m = ChapterManifest::new("test");
        m.chapters.push(ChapterEntry {
            id: "sec_0001".into(),
            title: "Chapter 1".into(),
            part: None,
            chapter: Some(1),
            first_line: "First line.".into(),
            word_count: 123,
            chunk_ids: vec![0, 1, 2],
            characters_present: vec!["Anna".into()],
            metadata: Default::default(),
        });
        m.save(&path).unwrap();
        let loaded = ChapterManifest::load(&path).unwrap().unwrap();
        assert_eq!(loaded.chapters.len(), 1);
        assert_eq!(loaded.chapters[0].word_count, 123);
        assert_eq!(loaded.chapters[0].chunk_ids, vec![0, 1, 2]);
    }

    #[test]
    fn load_missing_returns_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("chapters.json");
        let loaded = ChapterManifest::load(&path).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn merge_characters_present_dedupes_case_insensitively() {
        let mut m = ChapterManifest::new("t");
        m.chapters.push(ChapterEntry {
            id: "c1".into(),
            title: "t".into(),
            part: None,
            chapter: None,
            first_line: String::new(),
            word_count: 0,
            chunk_ids: Vec::new(),
            characters_present: vec!["Anna".into()],
            metadata: Default::default(),
        });
        m.merge_characters_present("c1", &["anna".into(), "Vronsky".into()])
            .unwrap();
        // "anna" is a dup of "Anna"; "Vronsky" is new.
        let chars = &m.get("c1").unwrap().characters_present;
        assert_eq!(chars.len(), 2);
        assert!(chars.iter().any(|c| c == "Anna"));
        assert!(chars.iter().any(|c| c == "Vronsky"));
    }

    #[test]
    fn merge_characters_present_rejects_placeholder_literals() {
        let mut m = ChapterManifest::new("t");
        m.chapters.push(ChapterEntry {
            id: "c1".into(),
            title: "t".into(),
            part: None,
            chapter: None,
            first_line: String::new(),
            word_count: 0,
            chunk_ids: Vec::new(),
            characters_present: Vec::new(),
            metadata: Default::default(),
        });
        m.merge_characters_present(
            "c1",
            &["...".into(), "…".into(), "TODO".into(), "Alyosha".into()],
        )
        .unwrap();
        let chars = &m.get("c1").unwrap().characters_present;
        assert_eq!(chars, &vec!["Alyosha".to_string()]);
    }

    #[test]
    fn merge_characters_present_self_heals_prior_placeholders() {
        // A pre-fix run persisted "..." into the manifest. The next
        // merge call should scrub it even when no new names overlap.
        let mut m = ChapterManifest::new("t");
        m.chapters.push(ChapterEntry {
            id: "c1".into(),
            title: "t".into(),
            part: None,
            chapter: None,
            first_line: String::new(),
            word_count: 0,
            chunk_ids: Vec::new(),
            characters_present: vec!["...".into(), "Alyosha".into()],
            metadata: Default::default(),
        });
        m.merge_characters_present("c1", &["Dmitri".into()])
            .unwrap();
        let chars = &m.get("c1").unwrap().characters_present;
        assert!(
            !chars.iter().any(|c| c == "..."),
            "scrub should have dropped '...': {chars:?}"
        );
        assert!(chars.iter().any(|c| c == "Alyosha"));
        assert!(chars.iter().any(|c| c == "Dmitri"));
    }

    #[test]
    fn merge_unknown_chapter_errors() {
        let mut m = ChapterManifest::new("t");
        let err = m
            .merge_characters_present("nope", &["X".into()])
            .unwrap_err();
        assert!(format!("{err:?}").contains("chapter not found"));
    }

    #[test]
    fn roman_numerals_parse() {
        assert_eq!(roman_to_u32("I"), Some(1));
        assert_eq!(roman_to_u32("IV"), Some(4));
        assert_eq!(roman_to_u32("IX"), Some(9));
        assert_eq!(roman_to_u32("XLII"), Some(42));
        assert_eq!(roman_to_u32("MCMXCIX"), Some(1999));
        assert_eq!(roman_to_u32(""), None);
        assert_eq!(roman_to_u32("ABC"), None);
    }

    #[test]
    fn parse_hierarchy_handles_arabic_and_roman() {
        assert_eq!(parse_hierarchy("Chapter 12", "Chapter"), Some(12));
        assert_eq!(parse_hierarchy("Part III", "Part"), Some(3));
        assert_eq!(parse_hierarchy("Part 3, Chapter 12", "Part"), Some(3));
        assert_eq!(parse_hierarchy("Part 3, Chapter 12", "Chapter"), Some(12));
        assert_eq!(parse_hierarchy("Book 1", "Chapter"), None);
    }
}
