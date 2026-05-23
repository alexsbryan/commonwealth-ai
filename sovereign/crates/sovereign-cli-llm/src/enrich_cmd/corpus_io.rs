//! Source-file → `ChapterInput` reconstruction used by every subcommand
//! that needs to feed chapters into a phase runner.
//!
//! Landing 2 keeps this simple: we re-read the source file, re-apply
//! the `SectionedChunker`, rebuild `ChapterInput`s, and merge any
//! existing `characters_present` / `chunk_ids` back from an on-disk
//! manifest. When LanceDB-backed ingest lands for phase 4 this helper
//! will also populate chunk IDs from the index.

use std::sync::Arc;

use corpus_engine::chunkers::sectioned::{
    ChapterRegexDetector, SectionDetector, SectionedChunker, TocAnchoredDetector,
};
use corpus_engine::enrichment::pipeline::{
    is_placeholder_literal, ChapterInput, ChapterManifest, ChunkRecord, CorpusContext,
};
use corpus_engine::error::{Error, Result};
use corpus_engine::{CorpusEngine, EmbedFn};

use super::config::EnrichConfig;
use super::paths;
use super::source_loader::load_plaintext;

/// Sentinel scheme used by `enrich init --from-corpus <id>` to record
/// "this enrichment is driven by an already-indexed corpus, not a
/// source file". `<id>` is the source corpus_id; `rebuild_corpus_state`
/// dispatches to the LanceDB-backed hydration path on this prefix.
const CORPUS_SOURCE_PREFIX: &str = "corpus:";

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
///
/// Dispatches to [`rebuild_corpus_state_from_corpus`] when
/// `cfg.source_path` is the `corpus:<id>` sentinel — multi-document
/// corpora hydrate `ChapterInput.text` from LanceDB chunks rather
/// than from a single source file.
pub fn rebuild_corpus_state(
    cfg: &EnrichConfig,
) -> Result<(Vec<ChapterInput>, ChapterManifest)> {
    if let Some(source_corpus_id) = corpus_source_id(cfg) {
        return rebuild_corpus_state_from_corpus(cfg, &source_corpus_id);
    }
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

/// Pull the source corpus id out of a `corpus:<id>` sentinel
/// `source_path`. Returns `None` for ordinary file-backed configs.
fn corpus_source_id(cfg: &EnrichConfig) -> Option<String> {
    let s = cfg.source_path.to_string_lossy();
    s.strip_prefix(CORPUS_SOURCE_PREFIX).map(str::to_string)
}

/// Hydrate `ChapterInput.text` for every chapter in the persisted
/// manifest by reading the chapter's chunk_ids from the source
/// corpus's LanceDB index. Used when an enrichment is driven from an
/// already-indexed multi-document corpus (`enrich init --from-corpus`).
///
/// The manifest's chunk_ids are pre-populated at init time, so this
/// path doesn't need a chunker — it just fetches and concatenates.
fn rebuild_corpus_state_from_corpus(
    cfg: &EnrichConfig,
    source_corpus_id: &str,
) -> Result<(Vec<ChapterInput>, ChapterManifest)> {
    let manifest_path = paths::chapters_manifest_path(&cfg.corpus_id);
    let manifest = ChapterManifest::load(&manifest_path)?.ok_or_else(|| {
        Error::InvalidInput(format!(
            "no chapter manifest at {} — re-run `sovereign enrich init {} --from-corpus {} \
             [--limit-articles N]` to create it.",
            manifest_path.display(),
            cfg.corpus_id,
            source_corpus_id,
        ))
    })?;

    // Resolve indexes dir from setup config (mirrors the
    // `--from-corpus` adapter in init.rs).
    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".sovereign")
        });
    let recipes_dir = data_dir.join("recipes");
    let indexes_dir = data_dir.join("indexes");
    let noop_embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(Vec::<f32>::new()) }));
    let engine = CorpusEngine::new(recipes_dir, indexes_dir, noop_embed);

    // Collect every chunk_id referenced by the manifest. Subset
    // extraction runs ask only for a few chapters, but every caller
    // of `rebuild_corpus_state` still wants every chapter materialised
    // in `inputs` (the selection filter runs downstream). Even so,
    // the fetch is bounded by the manifest's chunk_ids, NOT the
    // entire source corpus — this is the difference between loading
    // a few hundred KB and loading the full Wikipedia LanceDB into
    // memory. See `chunks_by_ids` doc-comment.
    let needed_ids: Vec<u64> = {
        let mut s: Vec<u64> = manifest
            .chapters
            .iter()
            .flat_map(|c| c.chunk_ids.iter().copied())
            .collect();
        s.sort_unstable();
        s.dedup();
        s
    };

    // The phase runner is sync; rebuild_corpus_state is sync too.
    // Every caller is reached from an async context. To avoid both
    // (a) "Cannot start a runtime from within a runtime" when
    // building a fresh runtime, and (b) deadlocks with
    // `block_in_place + Handle::current().block_on()` interacting
    // with LanceDB's internal task scheduling, run the async read
    // on a *separate OS thread* with its own current-thread tokio
    // runtime. The parent runtime is untouched.
    let source_corpus = source_corpus_id.to_string();
    let chunks = std::thread::scope(|s| {
        let handle = s.spawn(move || -> Result<Vec<corpus_engine::EnrichmentChunkRow>> {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    Error::Database(format!("rebuild_corpus_state: tokio build: {e}"))
                })?;
            rt.block_on(async {
                let index = engine
                    .open_index_for_corpus(&source_corpus)
                    .await
                    .map_err(|e| {
                        Error::Database(format!(
                            "open source corpus `{source_corpus}`: {e}"
                        ))
                    })?;
                index.chunks_by_ids(&needed_ids).await
            })
        });
        handle
            .join()
            .map_err(|_| Error::Database("rebuild_corpus_state: worker panicked".into()))?
    })?;

    // Build a chunk_id → content map for fast lookup.
    let chunk_text: std::collections::HashMap<u64, String> =
        chunks.into_iter().map(|c| (c.id, c.content)).collect();

    let mut inputs = Vec::with_capacity(manifest.chapters.len());
    for entry in &manifest.chapters {
        let mut sorted_ids = entry.chunk_ids.clone();
        sorted_ids.sort_unstable();
        let body: String = sorted_ids
            .iter()
            .filter_map(|id| chunk_text.get(id).cloned())
            .collect::<Vec<_>>()
            .join("\n\n");
        let approx_tokens = body.len() / 4;
        inputs.push(ChapterInput {
            chapter_id: entry.id.clone(),
            title: entry.title.clone(),
            text: body,
            metadata: entry
                .metadata
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            approx_tokens,
        });
    }

    // The manifest is already authoritative on this path; return as-is.
    Ok((inputs, manifest))
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
    if corpus_source_id(cfg).is_some() {
        // Corpus mode — every chapter's chunk_ids point at LanceDB
        // rows we already pulled in `rebuild_corpus_state_from_corpus`.
        // For phases that consume `CorpusContext.chunks` (Phase 3+),
        // synthesise paragraph-shaped `ChunkRecord`s directly from
        // each chapter's body. The chapter is the section, so
        // `section_id == chapter_id`.
        let mut chunks = Vec::new();
        let mut next_id: u64 = 0;
        for ch in &chapters {
            for para in ch.text.split("\n\n").filter(|p| !p.trim().is_empty()) {
                chunks.push(ChunkRecord {
                    id: next_id,
                    section_id: ch.chapter_id.clone(),
                    text: para.trim().to_string(),
                });
                next_id += 1;
            }
        }
        let chapter_titles: Vec<String> = chapters.iter().map(|c| c.title.clone()).collect();
        let ctx = CorpusContext {
            chapters,
            chunks,
            chapter_titles,
        };
        return Ok((ctx, manifest));
    }
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
