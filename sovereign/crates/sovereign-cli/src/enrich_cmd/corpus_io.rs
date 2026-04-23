//! Source-file → `ChapterInput` reconstruction used by every subcommand
//! that needs to feed chapters into a phase runner.
//!
//! Landing 2 keeps this simple: we re-read the source file, re-apply
//! the `SectionedChunker`, rebuild `ChapterInput`s, and merge any
//! existing `characters_present` / `chunk_ids` back from an on-disk
//! manifest. When LanceDB-backed ingest lands for phase 4 this helper
//! will also populate chunk IDs from the index.

use corpus_engine::chunkers::sectioned::{
    ChapterRegexDetector, SectionDetector, SectionedChunker, TocAnchoredDetector,
};
use corpus_engine::enrichment::pipeline::{
    is_placeholder_literal, ChapterInput, ChapterManifest, ChunkRecord, CorpusContext,
};
use corpus_engine::error::{Error, Result};

use super::config::EnrichConfig;
use super::paths;
use super::source_loader::load_plaintext;

/// Build the section detector the config selects. Returns a boxed
/// trait object so both callers (`rebuild_corpus_state` and
/// `build_corpus`) share one dispatch site.
fn detector_for(cfg: &EnrichConfig) -> Result<Box<dyn SectionDetector>> {
    if let Some(tm) = &cfg.toc_markers {
        Ok(Box::new(TocAnchoredDetector::with_markers(&tm.start, &tm.end)))
    } else {
        let det = ChapterRegexDetector::with_pattern(&cfg.chapter_regex)
            .map_err(|e| Error::InvalidInput(format!("invalid chapter_regex: {e}")))?
            .with_min_body_words(cfg.min_section_body_words);
        Ok(Box::new(det))
    }
}

/// Load the source file, detect sections, build the `ChapterInput`s
/// + a fresh `ChapterManifest`. Preserves `characters_present` and
/// `chunk_ids` from an on-disk manifest when the section id matches.
pub fn rebuild_corpus_state(
    cfg: &EnrichConfig,
) -> Result<(Vec<ChapterInput>, ChapterManifest)> {
    let source = load_plaintext(&cfg.source_path)?;

    let chunker = SectionedChunker::with_detector(detector_for(cfg)?);
    let sections = chunker.dry_run(&source).sections;

    if sections.is_empty() {
        return Err(Error::InvalidInput(format!(
            "no sections detected in {} — re-run `sovereign enrich init {} --source <path> \
             --dry-run` to see the loaded text and adjust --chapter-regex or --toc markers.",
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
                // Prior runs (before the placeholder-rejection landed)
                // may have persisted literal `"..."` into
                // characters_present. Drop those on hydrate so they
                // don't propagate into downstream phases.
                entry.characters_present = prior_entry
                    .characters_present
                    .iter()
                    .filter(|name| !is_placeholder_literal(name))
                    .cloned()
                    .collect();
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
    let source = load_plaintext(&cfg.source_path)?;
    let chunker = SectionedChunker::with_detector(detector_for(cfg)?);
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
