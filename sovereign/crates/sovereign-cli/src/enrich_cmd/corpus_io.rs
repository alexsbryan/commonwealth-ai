//! Source-file → `ChapterInput` reconstruction used by every subcommand
//! that needs to feed chapters into a phase runner.
//!
//! Landing 2 keeps this simple: we re-read the source file, re-apply
//! the `SectionedChunker`, rebuild `ChapterInput`s, and merge any
//! existing `characters_present` / `chunk_ids` back from an on-disk
//! manifest. When LanceDB-backed ingest lands for phase 4 this helper
//! will also populate chunk IDs from the index.

use std::fs;

use corpus_engine::chunkers::sectioned::{ChapterRegexDetector, SectionedChunker};
use corpus_engine::enrichment::pipeline::{
    ChapterInput, ChapterManifest, ChunkRecord, CorpusContext,
};
use corpus_engine::error::{Error, Result};

use super::config::EnrichConfig;
use super::paths;

/// Load the source file, detect sections, build the `ChapterInput`s
/// + a fresh `ChapterManifest`. Preserves `characters_present` and
/// `chunk_ids` from an on-disk manifest when the section id matches.
pub fn rebuild_corpus_state(
    cfg: &EnrichConfig,
) -> Result<(Vec<ChapterInput>, ChapterManifest)> {
    let source = fs::read_to_string(&cfg.source_path).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!(
                "reading source file {}: {}",
                cfg.source_path.display(),
                e
            ),
        ))
    })?;

    let detector = ChapterRegexDetector::with_pattern(&cfg.chapter_regex)
        .map_err(|e| Error::InvalidInput(format!("invalid chapter_regex: {e}")))?;
    let chunker = SectionedChunker::with_detector(detector);
    let sections = chunker.dry_run(&source).sections;

    if sections.is_empty() {
        return Err(Error::InvalidInput(format!(
            "no sections detected in {} — widen chapter_regex in the config or re-run \
             `sovereign enrich init {} --source <path> --chapter-regex <pat> --dry-run` \
             to verify",
            cfg.source_path.display(),
            cfg.corpus_id
        )));
    }

    let mut inputs = Vec::with_capacity(sections.len());
    for sec in &sections {
        let start = sec.start_byte.min(source.len());
        let end = sec.end_byte.min(source.len()).max(start);
        let text = source[start..end].trim().to_string();
        let approx_tokens = text.len() / 4;
        inputs.push(ChapterInput {
            chapter_id: sec.id.clone(),
            title: sec.title.clone(),
            text,
            metadata: sec.metadata.clone(),
            approx_tokens,
        });
    }

    // Build a fresh manifest, then merge back any fields that previous
    // runs populated (characters_present from phase 1, chunk_ids from a
    // future LanceDB ingest).
    let mut fresh =
        ChapterManifest::from_detected_sections(&cfg.corpus_id, &source, &sections);
    let manifest_path = paths::chapters_manifest_path(&cfg.corpus_id);
    if let Some(prior) = ChapterManifest::load(&manifest_path)? {
        for entry in &mut fresh.chapters {
            if let Some(prior_entry) = prior.get(&entry.id) {
                entry.characters_present = prior_entry.characters_present.clone();
                entry.chunk_ids = prior_entry.chunk_ids.clone();
            }
        }
    }

    Ok((inputs, fresh))
}

/// Build a full `CorpusContext` (chapters + paragraph chunks + titles)
/// + the live `ChapterManifest`. Phases 3+ all take this structure.
///
/// Chunk ids are monotonically assigned from the emitted paragraph
/// chunker order, matching the layout a future LanceDB ingest would
/// use. They are stable across runs provided the source file +
/// `chapter_regex` do not change.
pub fn build_corpus(cfg: &EnrichConfig) -> Result<(CorpusContext, ChapterManifest)> {
    let (chapters, manifest) = rebuild_corpus_state(cfg)?;
    let source = std::fs::read_to_string(&cfg.source_path)?;
    let detector = ChapterRegexDetector::with_pattern(&cfg.chapter_regex)
        .map_err(|e| Error::InvalidInput(format!("invalid chapter_regex: {e}")))?;
    let chunker = SectionedChunker::with_detector(detector);
    let sectioned = chunker.chunk(&source);
    let chunks: Vec<ChunkRecord> = sectioned
        .into_iter()
        .map(|c| ChunkRecord {
            id: c.index as u64,
            section_id: c.section_id,
            text: c.content,
        })
        .collect();
    let chapter_titles: Vec<String> = chapters.iter().map(|c| c.title.clone()).collect();
    let ctx = CorpusContext { chapters, chunks, chapter_titles };
    Ok((ctx, manifest))
}
