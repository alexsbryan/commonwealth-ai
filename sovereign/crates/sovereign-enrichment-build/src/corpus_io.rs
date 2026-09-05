// SPDX-License-Identifier: AGPL-3.0-or-later
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
pub fn detector_for(cfg: &EnrichConfig) -> Result<Box<dyn SectionDetector>> {
    if let Some(tm) = &cfg.toc_markers {
        Ok(Box::new(TocAnchoredDetector::with_markers(
            &tm.start, &tm.end,
        )))
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
pub fn rebuild_corpus_state(cfg: &EnrichConfig) -> Result<(Vec<ChapterInput>, ChapterManifest)> {
    if let Some(source_corpus_id) = corpus_source_id(cfg) {
        return rebuild_corpus_state_from_corpus(cfg, &source_corpus_id);
    }
    let source = load_plaintext(&cfg.source_path)?;

    let chunker = SectionedChunker::with_detector(detector_for(cfg)?);
    let sections = chunker.dry_run(&source).sections;

    if sections.is_empty() {
        return Err(Error::InvalidInput(format!(
            "no sections detected in {} — re-run `svrn enrich init {} --source <path> \
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
    let mut fresh = ChapterManifest::from_detected_sections(&cfg.corpus_id, &source, &sections);
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
/// Fetch corpus chunks by LanceDB row id from `source_corpus_id`,
/// preserving the real ids. Shared by `rebuild_corpus_state_from_corpus`
/// (which flattens them into chapter bodies) and `build_corpus` (which
/// needs the per-chunk REAL id so `first_appearance.chunk_id` resolves
/// back to the retrieval corpus — the atlas-directs-retrieval contract).
///
/// Runs the async LanceDB read on a separate OS thread with its own
/// current-thread runtime, to avoid "cannot start a runtime within a
/// runtime" panics + `block_in_place` deadlocks with LanceDB's
/// internal scheduling (every caller is reached from async context).
fn fetch_enrichment_chunks(
    source_corpus_id: &str,
    needed_ids: &[u64],
) -> Result<Vec<corpus_engine::EnrichmentChunkRow>> {
    read_corpus_chunks(source_corpus_id, Some(needed_ids))
}

/// EVERY chunk of a corpus, ids preserved. Used by the section backfill,
/// which has to locate all of them against the source document — it cannot
/// name the ids it wants in advance, because working them out is the whole
/// job.
pub fn fetch_all_corpus_chunks(corpus_id: &str) -> Result<Vec<corpus_engine::EnrichmentChunkRow>> {
    read_corpus_chunks(corpus_id, None)
}

/// One LanceDB read path for both callers (ARCH_PRINCIPLES §10.6).
/// `needed_ids: None` reads the whole table.
fn read_corpus_chunks(
    source_corpus_id: &str,
    needed_ids: Option<&[u64]>,
) -> Result<Vec<corpus_engine::EnrichmentChunkRow>> {
    // The SAME root every other enrichment path hangs from — `paths::`, i.e.
    // `rebrand::data_dir()`, which honours `SOVEREIGN_DATA_DIR`. This derived
    // its own from `SetupConfig.data.dir` (falling back to `svrnmesh_root()`)
    // until order ei-5b-build-verb, which is the second root the enrichment
    // store was warned about in `sovereign-enrichment-catalog::paths`'s module
    // doc: a build read its `config.json` under the override and its CHUNKS
    // under `~/.sovereign`, and reported `Index not found` for a corpus that
    // had just been indexed. Reader and writer must agree (ARCH §10.6).
    let noop_embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(Vec::<f32>::new()) }));
    let engine = CorpusEngine::new(paths::recipes_dir(), paths::indexes_dir(), noop_embed);
    let source_corpus = source_corpus_id.to_string();
    let ids = needed_ids.map(<[u64]>::to_vec);
    std::thread::scope(|s| {
        let handle = s.spawn(move || -> Result<Vec<corpus_engine::EnrichmentChunkRow>> {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    Error::Database(format!("fetch_enrichment_chunks: tokio build: {e}"))
                })?;
            rt.block_on(async {
                let index = engine
                    .open_index_for_corpus(&source_corpus)
                    .await
                    .map_err(|e| {
                        Error::Database(format!("open source corpus `{source_corpus}`: {e}"))
                    })?;
                match &ids {
                    Some(ids) => index.chunks_by_ids(ids).await,
                    None => index.all_chunks_full().await,
                }
            })
        });
        handle
            .join()
            .map_err(|_| Error::Database("fetch_enrichment_chunks: worker panicked".into()))?
    })
}

fn rebuild_corpus_state_from_corpus(
    cfg: &EnrichConfig,
    source_corpus_id: &str,
) -> Result<(Vec<ChapterInput>, ChapterManifest)> {
    let manifest_path = paths::chapters_manifest_path(&cfg.corpus_id);
    let manifest = ChapterManifest::load(&manifest_path)?.ok_or_else(|| {
        Error::InvalidInput(format!(
            "no chapter manifest at {} — re-run `svrn enrich init {} --from-corpus {} \
             [--limit-articles N]` to create it.",
            manifest_path.display(),
            cfg.corpus_id,
            source_corpus_id,
        ))
    })?;

    // Collect every chunk_id referenced by the manifest. The fetch is
    // bounded by the manifest's chunk_ids, NOT the entire source corpus
    // (see `chunks_by_ids`); subset runs still materialise every chapter
    // — the selection filter runs downstream.
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
    let chunks = fetch_enrichment_chunks(source_corpus_id, &needed_ids)?;

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
    if let Some(src) = corpus_source_id(cfg) {
        // Corpus mode. Emit one `ChunkRecord` per REAL corpus chunk,
        // carrying the actual LanceDB row id as `ChunkRecord.id`. This
        // is the atlas-directs-retrieval contract: an atom's
        // `first_appearance.chunk_id` (which derives from
        // `ChunkRecord.id`) must resolve back to a row in the retrieval
        // corpus so retrieval can fetch the evidence. The previous code
        // split chapter bodies on "\n\n" and assigned fresh sequential
        // ids — a *different* chunking than the source corpus — which
        // severed the atom→chunk link (an atom said chunk 1; corpus row
        // 1 was an unrelated email). One ChunkRecord per real chunk,
        // real id preserved; `section_id == chapter_id`.
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
        let content_by_id: std::collections::HashMap<u64, String> =
            fetch_enrichment_chunks(&src, &needed_ids)?
                .into_iter()
                .map(|r| (r.id, r.content))
                .collect();
        let mut chunks = Vec::new();
        for ch in &manifest.chapters {
            let mut ids = ch.chunk_ids.clone();
            ids.sort_unstable();
            for cid in ids {
                if let Some(text) = content_by_id.get(&cid) {
                    chunks.push(ChunkRecord {
                        id: cid, // REAL LanceDB row id — resolvable at retrieval time
                        section_id: ch.id.clone(),
                        text: text.clone(),
                    });
                }
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
    let ctx = CorpusContext {
        chapters,
        chunks,
        chapter_titles,
    };
    Ok((ctx, manifest))
}

// ── The chapter manifest an already-indexed corpus implies ───────────────────
//
// `enrich init --from-corpus <id>` and `corpus ingest <recipe.toml>` ask the
// same question — what are this index's chapters? — and until order
// ei-5b-build-verb only the CLI could answer it: the grouping lived in
// `sovereign-cli-llm`'s `enrich_cmd::init`, a host `corpus-mcp` cannot link.
// It is beside `fetch_all_corpus_chunks` now because that is its only input
// (ARCH §10.6). The CLI is a caller; there is one implementation.

/// Group LanceDB chunk rows by `(source_doc_id_or_title, section_path)`
/// and emit one `ChapterEntry` per group.
///
/// Falls back to `title` as the article-grouping key when
/// `source_doc_id` is absent — older ingestions may have one but
/// not the other.
///
/// `start_ordinal` is the first chapter ordinal to assign. First-run
/// `enrich init --from-corpus` passes `1` (chapters are
/// `sec_00001 …`). The incremental `enrich delta-manifest` path passes
/// `existing_manifest_len + 1` so newly-detected chapters get
/// `sec_NNNNN` ids that continue past the live manifest without
/// colliding — the `chapter` field + `ordinal` metadata follow the
/// same numbering.
pub fn build_manifest_from_corpus_rows(
    corpus_id: &str,
    rows: Vec<corpus_engine::EnrichmentChunkRow>,
    limit_articles: Option<usize>,
    include_articles: Option<Vec<String>>,
    start_ordinal: u32,
) -> std::result::Result<ChapterManifest, String> {
    use corpus_engine::WikipediaChunkMetadata;
    use std::collections::BTreeMap;

    // Per-(article, section) bucket. BTreeMap so chapter ids come
    // out in deterministic order across runs.
    type ArticleKey = String; // source_doc_id (or title) of the article
    type SectionKey = String; // joined section_path; "" for lead

    #[derive(Default)]
    struct Bucket {
        article_title: String,
        section_name: String,
        section_path_joined: String,
        section_type: Option<String>,
        pov_count: i64,
        citation_needed_count: i64,
        url: Option<String>,
        chunk_ids: Vec<u64>,
        chunks: Vec<(u64, String)>, // (id, content) — sorted by id at finalisation
    }

    let mut buckets: BTreeMap<(ArticleKey, SectionKey), Bucket> = BTreeMap::new();
    let mut article_first_seen: BTreeMap<ArticleKey, usize> = BTreeMap::new();
    let mut counter: usize = 0;

    for row in rows {
        // Article-grouping key: prefer title (per-article in Wikipedia /
        // wiki-shaped corpora — every chunk in an article shares the
        // same title) over source_doc_id (which the Wikipedia extractor
        // sets to the per-section URL, so it varies *within* an article
        // and groups too finely). Fall back to source_doc_id stripped
        // of any URL fragment, then to "<untitled>" as a last resort.
        let article_key = row
            .title
            .clone()
            .or_else(|| {
                row.source_doc_id
                    .as_deref()
                    .map(|s| s.split('#').next().unwrap_or(s).to_string())
            })
            .unwrap_or_else(|| "<untitled>".to_string());
        let article_title = row.title.clone().unwrap_or_else(|| article_key.clone());

        // Section identification — driven by Wikipedia-shaped
        // metadata. Other referential extractors should serialise
        // `WikipediaChunkMetadata`-compatible JSON for now (the
        // section_path / section_name / section_type fields are
        // the load-bearing ones); a generalisation lives behind
        // the `Pipeline` trait if more shapes appear.
        let (section_path_vec, section_name, section_type, pov_count, citation_needed_count) =
            match row
                .metadata_raw
                .as_deref()
                .and_then(|s| serde_json::from_str::<WikipediaChunkMetadata>(s).ok())
            {
                Some(m) => (
                    m.section_path.clone(),
                    m.section_name.clone(),
                    Some(m.section_type.clone()),
                    m.pov_count.unwrap_or(0),
                    m.citation_needed_count.unwrap_or(0),
                ),
                None => (Vec::<String>::new(), String::new(), None, 0, 0),
            };
        let section_path_joined = section_path_vec.join(" › ");

        article_first_seen
            .entry(article_key.clone())
            .or_insert_with(|| {
                let n = counter;
                counter += 1;
                n
            });

        let bucket = buckets
            .entry((article_key.clone(), section_path_joined.clone()))
            .or_insert_with(|| Bucket {
                article_title: article_title.clone(),
                section_name: section_name.clone(),
                section_path_joined: section_path_joined.clone(),
                section_type: section_type.clone(),
                pov_count,
                citation_needed_count,
                url: row.url.clone(),
                chunk_ids: Vec::new(),
                chunks: Vec::new(),
            });
        bucket.chunk_ids.push(row.id);
        bucket.chunks.push((row.id, row.content));
    }

    // Apply the per-article cap and/or include-list. `include_articles`
    // takes precedence — if the operator handed us an explicit title
    // list (typically the top-K from `enrich triage-candidates`), keep
    // exactly those articles regardless of order. Otherwise fall back
    // to the existing first-seen-order limit.
    //
    // `--include-articles` is normalised through
    // `corpus_engine::filters::normalize_title` to be tolerant of the
    // operator's underscore vs space habits in their title file.
    let total_articles = article_first_seen.len();
    let kept_articles: std::collections::HashSet<ArticleKey> = if let Some(want) =
        include_articles.as_ref()
    {
        let want_norm: std::collections::HashSet<String> = want
            .iter()
            .map(|t| corpus_engine::filters::normalize_title(t))
            .collect();
        let mut hits: std::collections::HashSet<ArticleKey> = std::collections::HashSet::new();
        for (key, _) in article_first_seen {
            if want_norm.contains(&corpus_engine::filters::normalize_title(&key)) {
                hits.insert(key);
            }
        }
        // Diagnostic so the operator knows how many of their listed
        // titles actually exist in the source corpus.
        let want_total = want_norm.len();
        if hits.len() < want_total {
            eprintln!(
                "manifest: --include-articles matched {}/{} titles ({} not present in source corpus)",
                hits.len(),
                want_total,
                want_total - hits.len(),
            );
        }
        hits
    } else if let Some(n) = limit_articles {
        let mut articles: Vec<(ArticleKey, usize)> = article_first_seen.into_iter().collect();
        articles.sort_by_key(|(_, ord)| *ord);
        articles.into_iter().take(n).map(|(k, _)| k).collect()
    } else {
        article_first_seen.into_keys().collect()
    };
    eprintln!(
        "manifest: {} articles, {} sections — keeping {} articles",
        total_articles,
        buckets.len(),
        kept_articles.len(),
    );

    // Emit one ChapterEntry per surviving (article, section). The
    // loop pre-increments `chapter_ord`, so seed it one below
    // `start_ordinal` (saturating so a stray `0` still yields a valid
    // `sec_00001` rather than underflowing).
    let mut manifest = ChapterManifest::new(corpus_id);
    let mut chapter_ord: u32 = start_ordinal.saturating_sub(1);
    for ((article_key, _section_key), mut bucket) in buckets {
        if !kept_articles.contains(&article_key) {
            continue;
        }
        bucket.chunks.sort_by_key(|(id, _)| *id);
        bucket.chunk_ids.sort_unstable();
        let body: String = bucket
            .chunks
            .iter()
            .map(|(_, c)| c.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let word_count = body.split_whitespace().count() as u64;
        let first_line = body
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .chars()
            .take(160)
            .collect::<String>();
        let title = if bucket.section_name.is_empty() {
            bucket.article_title.clone()
        } else {
            format!("{} — {}", bucket.article_title, bucket.section_name)
        };
        chapter_ord += 1;
        let id = format!("sec_{chapter_ord:05}");
        let mut metadata: BTreeMap<String, String> = BTreeMap::new();
        metadata.insert("article_title".into(), bucket.article_title);
        metadata.insert("section_path".into(), bucket.section_path_joined);
        if let Some(st) = bucket.section_type {
            metadata.insert("section_type".into(), st);
        }
        if bucket.pov_count > 0 {
            metadata.insert("pov_count".into(), bucket.pov_count.to_string());
        }
        if bucket.citation_needed_count > 0 {
            metadata.insert(
                "citation_needed_count".into(),
                bucket.citation_needed_count.to_string(),
            );
        }
        if let Some(u) = bucket.url {
            metadata.insert("url".into(), u);
        }
        metadata.insert("ordinal".into(), chapter_ord.to_string());

        manifest
            .chapters
            .push(corpus_engine::enrichment::pipeline::ChapterEntry {
                id,
                title,
                part: None,
                chapter: Some(chapter_ord),
                first_line,
                word_count,
                chunk_ids: bucket.chunk_ids,
                characters_present: Vec::new(),
                metadata,
            });
    }

    Ok(manifest)
}
