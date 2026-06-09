// SPDX-License-Identifier: AGPL-3.0-or-later
//! On-demand single-work catalog ingest.
//!
//! When the user accepts an ingest offer for a catalog hit (e.g. "yes,
//! read Moby Dick"), this module orchestrates the end-to-end flow:
//!
//! 1. **Resolve.** Open the catalog corpus index and FTS-look-up the
//!    work id; pull title/url/metadata off the matched chunk.
//! 2. **Compose override.** Fetch the content recipe (e.g.
//!    `gutenberg-work`) from the registry, patch its `corpus.id`
//!    (`<catalog>-<work_id>`), `corpus.parent_corpus_id`, and the
//!    acquire URL (substituting `{id}` in the catalog template).
//! 3. **Ingest.** Hand the mutated recipe to
//!    [`corpus_engine::CorpusEngine::ingest`] via
//!    [`corpus_engine::CorpusSpec::Inline`]. The on-demand guard in
//!    `ingest()` requires this exact entry point — a direct
//!    `Builtin("gutenberg-work")` ingest is refused.
//! 4. **Enrich (optional).** If [`CatalogIngestRequest::enrich`] is
//!    set, fire `sovereign-cli enrich build <new_corpus_id>` via
//!    [`crate::enrich::run_enrich_build`] and stream its
//!    [`corpus_engine::enrichment::pipeline::EnrichProgress`]
//!    events through the same callback.
//! 5. **Complete.** Emit the new corpus id and a brief atlas summary
//!    so the desktop's "atlas is ready" surface can show how much
//!    structure was found.
//!
//! The same service powers the Tauri command, the agent-loop tool,
//! and the CLI simulator — see Phase H in the plan file. Each
//! frontend wraps this with its own progress channel.
//!
//! Cancellation is propagated via [`CancellationFlag`] (shared with
//! `enrich.rs`). The caller flips the flag; ingest checks it on
//! every batch boundary, enrichment polls it between subprocess
//! lines.

use std::sync::Arc;

use corpus_engine::progress::IngestProgress;
use corpus_engine::recipe::Recipe;
use corpus_engine::types::{CorpusKind, CorpusSpec};
use corpus_engine::CorpusEngine;
use serde::{Deserialize, Serialize};

use crate::enrich::{run_enrich_build, CancellationFlag, EnrichBuildConfig, EnrichProgressFn};

/// Compose a per-work corpus id from the catalog id + work id.
/// Centralised so search-time partition logic
/// (`crate::catalog::CatalogResolutionContext::ingested_works`) and
/// the ingest service stay in lockstep on the suffix shape.
pub fn per_work_corpus_id(catalog_corpus_id: &str, work_id: &str) -> String {
    format!("{catalog_corpus_id}-{work_id}")
}

/// Streaming event emitted by [`run_catalog_ingest`].
///
/// Carries the underlying engine progress events verbatim plus a
/// pair of high-level lifecycle markers (`Resolving`, `Complete`,
/// `Failed`) so frontends don't need to peek at internal phases to
/// drive a "starting…" / "done!" UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CatalogIngestEvent {
    /// Catalog lookup phase. Fired exactly once.
    Resolving {
        catalog_corpus_id: String,
        work_id: String,
    },
    /// The catalog row was found and the override recipe was built.
    /// Carries the title resolved from catalog metadata so the UI
    /// can update the progress card.
    Resolved {
        title: String,
        download_url: String,
        new_corpus_id: String,
    },
    /// Re-emission of a corpus_engine ingest progress event.
    Ingest(IngestProgress),
    /// Re-emission of an enrichment progress event (only fires when
    /// `request.enrich = true`). Boxed because the variant is
    /// significantly larger than the rest of the enum.
    Enrich(Box<corpus_engine::enrichment::pipeline::EnrichProgress>),
    /// Terminal success.
    Complete {
        new_corpus_id: String,
        chunks_created: u64,
        atlas_summary: Option<AtlasSummary>,
    },
    /// Terminal failure. `stage` indicates which step blew up so the
    /// UI can show a precise error.
    Failed {
        stage: CatalogIngestStage,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatalogIngestStage {
    Resolving,
    Ingest,
    Enrich,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AtlasSummary {
    pub atoms: u64,
    pub edges: u64,
    pub themes: u64,
    pub questions: u64,
}

/// Caller-supplied callback that receives each [`CatalogIngestEvent`]
/// in order. Boxed `Send + Sync + 'static` so a Tauri command can
/// emit them on a channel from a spawned task.
pub type CatalogIngestProgressFn = Arc<dyn Fn(CatalogIngestEvent) + Send + Sync + 'static>;

/// Inputs to [`run_catalog_ingest`].
pub struct CatalogIngestRequest {
    pub catalog_corpus_id: String,
    pub work_id: String,
    /// When `true`, run `literary_atlas` enrichment after ingest.
    /// Defaults to `false` for the simulator (so a demo can finish
    /// in seconds and not depend on a working LLM); the desktop /
    /// agent-loop paths set it `true`.
    pub enrich: bool,
    /// Streaming progress sink. `None` = no telemetry.
    pub progress: Option<CatalogIngestProgressFn>,
    /// Cancellation flag shared with both ingest and enrich.
    pub cancel: Option<CancellationFlag>,
    /// Run the one-hop "minesweeper" link-expansion after the primary
    /// fetch lands. Each linked article is fetched in the background
    /// into the same shared target corpus. Set `false` on the
    /// recursively-spawned expansion fetches so they don't trigger
    /// further expansion (one-hop only). Caller-controlled override
    /// of the catalog config's `expansion_enabled` flag — the
    /// expansion fires only when BOTH are true.
    pub expand_links: bool,
}

impl Default for CatalogIngestRequest {
    fn default() -> Self {
        Self {
            catalog_corpus_id: String::new(),
            work_id: String::new(),
            enrich: false,
            progress: None,
            cancel: None,
            expand_links: true,
        }
    }
}

/// Errors specific to the catalog-ingest orchestration. The
/// underlying engine errors get folded into `Ingest { source: ... }`
/// rather than reused — the caller benefits more from knowing
/// "ingest blew up" than from the variant of the inner error.
#[derive(Debug, thiserror::Error)]
pub enum CatalogIngestError {
    #[error("catalog corpus `{catalog_corpus_id}` is not installed — install it with `sovereign corpus install {catalog_corpus_id}` first")]
    CatalogNotInstalled { catalog_corpus_id: String },

    #[error("corpus `{corpus_id}` is not a catalog (kind = {kind:?}) — only catalog corpora can drive on-demand work ingest")]
    NotACatalog { corpus_id: String, kind: CorpusKind },

    #[error("catalog corpus `{catalog_corpus_id}` has no `[catalog]` recipe block")]
    MissingCatalogConfig { catalog_corpus_id: String },

    #[error("work id `{work_id}` not found in catalog `{catalog_corpus_id}` — the catalog may be stale or the id may be wrong")]
    WorkNotFound {
        catalog_corpus_id: String,
        work_id: String,
    },

    #[error("content recipe `{content_recipe}` failed to load: {source}")]
    ContentRecipeLoad {
        content_recipe: String,
        #[source]
        source: corpus_engine::Error,
    },

    #[error("ingest failed: {source}")]
    Ingest {
        #[source]
        source: corpus_engine::Error,
    },

    #[error("enrichment failed (exit code {exit_code})")]
    Enrich { exit_code: i32 },
}

pub type CatalogIngestResult<T> = std::result::Result<T, CatalogIngestError>;

/// Drive the on-demand single-work catalog ingest. Returns the
/// per-work corpus id on success.
pub async fn run_catalog_ingest(
    engine: Arc<CorpusEngine>,
    request: CatalogIngestRequest,
) -> CatalogIngestResult<String> {
    let CatalogIngestRequest {
        catalog_corpus_id,
        work_id,
        enrich,
        progress,
        cancel,
        expand_links,
    } = request;

    let emit = |evt: CatalogIngestEvent| {
        if let Some(p) = &progress {
            p(evt);
        }
    };

    emit(CatalogIngestEvent::Resolving {
        catalog_corpus_id: catalog_corpus_id.clone(),
        work_id: work_id.clone(),
    });

    // ── Step 1: locate the catalog corpus on disk. ───────
    let installed = engine.installed_indexes().await.unwrap_or_default();
    let catalog_info = installed
        .iter()
        .find(|i| i.corpus_id == catalog_corpus_id)
        .ok_or_else(|| CatalogIngestError::CatalogNotInstalled {
            catalog_corpus_id: catalog_corpus_id.clone(),
        })?;
    if catalog_info.kind != CorpusKind::Catalog {
        return Err(CatalogIngestError::NotACatalog {
            corpus_id: catalog_corpus_id.clone(),
            kind: catalog_info.kind,
        });
    }

    // ── Step 2: load the catalog recipe + its [catalog] block. ──
    let catalog_recipe = engine
        .registry()
        .fetch_recipe(&catalog_corpus_id)
        .await
        .map_err(|source| CatalogIngestError::ContentRecipeLoad {
            content_recipe: catalog_corpus_id.clone(),
            source,
        })?;
    let catalog_cfg =
        catalog_recipe
            .catalog
            .clone()
            .ok_or_else(|| CatalogIngestError::MissingCatalogConfig {
                catalog_corpus_id: catalog_corpus_id.clone(),
            })?;

    // ── Step 3: FTS-lookup the work in the catalog index. ──────
    //
    // Use a literal `id_field:work_id` query — Tantivy treats the
    // colon as a field-scoped query and we stamped the id into the
    // chunk content as `Gutenberg ID: <id>`. Fall back to a plain
    // text search if FTS isn't built (small catalogs use a flat
    // scan).
    let title_for_event = lookup_work_title(&engine, catalog_info, &work_id)
        .await
        .ok_or_else(|| CatalogIngestError::WorkNotFound {
            catalog_corpus_id: catalog_corpus_id.clone(),
            work_id: work_id.clone(),
        })?;

    let download_url = catalog_cfg.download_url_template.replace("{id}", &work_id);

    // The "user-visible" corpus id — what the user queries against.
    // When the catalog declares `target_corpus_id`, every fetch lands
    // in that single shared corpus (e.g. "wikipedia-fetched"); the
    // legacy per-work pattern is the fallback.
    let final_corpus_id = catalog_cfg
        .target_corpus_id
        .clone()
        .unwrap_or_else(|| per_work_corpus_id(&catalog_corpus_id, &work_id));

    // The staging corpus the engine actually writes to. When using a
    // shared target we route the per-fetch ingest into a transient
    // underscore-prefixed dir so it doesn't pollute installed_indexes
    // (those skip names starting with `_`); after the append we
    // delete the staging dir entirely.
    let use_shared_target = catalog_cfg.target_corpus_id.is_some();
    let staging_corpus_id = if use_shared_target {
        format!("_fetch_{}-{}", final_corpus_id, work_id)
    } else {
        final_corpus_id.clone()
    };

    emit(CatalogIngestEvent::Resolved {
        title: title_for_event.clone(),
        download_url: download_url.clone(),
        new_corpus_id: final_corpus_id.clone(),
    });

    // ── Step 4: load + patch the content recipe. ────────
    let mut content_recipe = engine
        .registry()
        .fetch_recipe(&catalog_cfg.content_recipe)
        .await
        .map_err(|source| CatalogIngestError::ContentRecipeLoad {
            content_recipe: catalog_cfg.content_recipe.clone(),
            source,
        })?;
    patch_content_recipe(
        &mut content_recipe,
        &staging_corpus_id,
        &catalog_corpus_id,
        &download_url,
    );

    // ── Step 5: ingest. ─────────────────────────────────
    let ingest_progress: Option<corpus_engine::progress::ProgressCallback> =
        progress
            .as_ref()
            .map(|outer| -> corpus_engine::progress::ProgressCallback {
                let outer = outer.clone();
                Box::new(move |ev: IngestProgress| {
                    outer(CatalogIngestEvent::Ingest(ev));
                })
            });
    let mut ingest_result = engine
        .ingest(
            &CorpusSpec::Inline(Box::new(content_recipe)),
            ingest_progress,
        )
        .await
        .map_err(|source| CatalogIngestError::Ingest { source })?;

    // ── Step 5a: shared-target append. ──────────────────
    //
    // When `[catalog].target_corpus_id` is set, fold the staging
    // corpus into the shared canonical (e.g. fetched articles all
    // land in `wikipedia-fetched`). This keeps installed_indexes()
    // bounded — one shared corpus instead of one per fetched
    // article — and lets a future structural-atlas / mesh-share
    // pass operate on the union, not N tiny per-work corpora.
    if use_shared_target {
        let indexes_dir = engine.index_dir().to_path_buf();
        let staging_path = indexes_dir.join(&staging_corpus_id);
        let canonical_path = indexes_dir.join(&final_corpus_id);
        // Resolve embedding model + dim from the staging corpus
        // we just wrote — those are the only authoritative source.
        let staging_index = engine
            .open_index(&staging_path)
            .await
            .map_err(|source| CatalogIngestError::Ingest { source })?;
        let staging_info = staging_index
            .info()
            .await
            .map_err(|source| CatalogIngestError::Ingest { source })?;
        drop(staging_index); // release the lance handle before mutating the dir
        let report = corpus_engine::append_partition_to_canonical(
            &staging_path,
            &canonical_path,
            &final_corpus_id,
            &staging_info.corpus_name,
            &staging_info.embedding_model,
            staging_info.embedding_dimensions,
            staging_info.mesh_sharing,
        )
        .await
        .map_err(|source| CatalogIngestError::Ingest { source })?;
        tracing::info!(
            staging = %staging_corpus_id,
            canonical = %final_corpus_id,
            inserted = report.chunks_inserted,
            deduped = report.chunks_deduped,
            canonical_after = report.canonical_chunks_after,
            "catalog_ingest: appended staging into shared canonical"
        );
        // Finalise the canonical so retrieval treats it like any
        // other installed corpus:
        //   - stamp kind=Knowledge + parent_corpus_id (catalog hint),
        //   - rebuild vector + FTS so the freshly-appended chunks
        //     are searchable across both retrieval paths,
        //   - mark ingestion complete so installed_indexes() lists it.
        // Without these, `chat inspect` / OICP retrieval skip the
        // dir as "in-progress" and the corpus is invisible.
        if let Ok(canon) = corpus_engine::CorpusIndex::open(&canonical_path).await {
            // Inherit the parent_corpus_id from the patched content
            // recipe (e.g. wikipedia-article sets parent="wikipedia"
            // so fetched articles surface alongside the curated L5).
            let parent = catalog_corpus_id.clone();
            if let Err(e) = canon.set_kind_and_parent(
                Some(corpus_engine::types::CorpusKind::Knowledge),
                Some(&parent),
            ) {
                tracing::warn!(
                    canonical = %final_corpus_id,
                    error = %e,
                    "catalog_ingest: set_kind_and_parent failed (non-fatal)"
                );
            }
            if let Err(e) = canon.build_indexes(true, true, None).await {
                tracing::warn!(
                    canonical = %final_corpus_id,
                    error = %e,
                    "catalog_ingest: build_indexes after append failed (non-fatal)"
                );
            }
            if let Err(e) = canon.mark_ingestion_complete() {
                tracing::warn!(
                    canonical = %final_corpus_id,
                    error = %e,
                    "catalog_ingest: mark_ingestion_complete failed (non-fatal)"
                );
            }
        }
        // Delete the staging corpus dir — it's served its purpose.
        if let Err(e) = std::fs::remove_dir_all(&staging_path) {
            tracing::warn!(
                staging = %staging_corpus_id,
                error = %e,
                "catalog_ingest: staging cleanup failed (non-fatal)"
            );
        }
        // Surface the post-append count to the caller as the
        // chunks_created result (more useful than the staging count).
        ingest_result.chunks_created = report.chunks_inserted;
    }

    // Cooperative cancellation between ingest and enrich:
    // if the caller flipped the flag during ingest, skip
    // enrichment outright.
    let cancelled_mid = cancel
        .as_ref()
        .map(|f| f.load(std::sync::atomic::Ordering::SeqCst))
        .unwrap_or(false);

    let mut atlas_summary: Option<AtlasSummary> = None;

    // ── Step 5b: structural-atlas post-install (W5). ─────
    //
    // Mirror the corpus-install HTTP route's post-install hook so
    // catalog-ingested per-work corpora get their structural atlas
    // built automatically (no user step). Idempotent — short-
    // circuits when atoms.json already exists. Best-effort: a
    // failure here is logged and swallowed so the catalog-ingest
    // path still returns success on the chunk side.
    {
        let indexes_dir = engine.index_dir().to_path_buf();
        match crate::atlas_postinstall::build_structural_atlas(
            &final_corpus_id,
            indexes_dir.clone(),
            indexes_dir,
        )
        .await
        {
            crate::atlas_postinstall::StructuralAtlasOutcome::Built { elapsed_secs, .. } => {
                tracing::info!(
                    corpus = %final_corpus_id,
                    elapsed_s = elapsed_secs,
                    "catalog_ingest: structural atlas built"
                )
            }
            crate::atlas_postinstall::StructuralAtlasOutcome::AlreadyPresent { .. } => {
                tracing::debug!(
                    corpus = %final_corpus_id,
                    "catalog_ingest: structural atlas already present"
                );
            }
            crate::atlas_postinstall::StructuralAtlasOutcome::Failed { reason } => {
                tracing::warn!(
                    corpus = %final_corpus_id,
                    reason,
                    "catalog_ingest: structural atlas build failed (non-fatal)"
                );
            }
        }
    }

    // ── Step 6: enrich (optional). ─────────────────────
    if enrich && !cancelled_mid {
        let enrich_progress: Option<EnrichProgressFn> =
            progress.as_ref().map(|outer| -> EnrichProgressFn {
                let outer = outer.clone();
                Arc::new(move |ev| {
                    outer(CatalogIngestEvent::Enrich(Box::new(ev)));
                })
            });
        let outcome = run_enrich_build(
            &final_corpus_id,
            EnrichBuildConfig {
                cli_path: None,
                extra_args: vec!["--full".into()],
                cancel: cancel.clone(),
            },
            enrich_progress,
        )
        .await
        .map_err(|e| CatalogIngestError::Enrich {
            exit_code: e.raw_os_error().unwrap_or(-1),
        })?;

        if outcome.exit_code != 0 && !outcome.cancelled {
            emit(CatalogIngestEvent::Failed {
                stage: CatalogIngestStage::Enrich,
                message: format!(
                    "enrich build exited {} ({} unrecognised lines)",
                    outcome.exit_code,
                    outcome.unrecognised_lines.len()
                ),
            });
            return Err(CatalogIngestError::Enrich {
                exit_code: outcome.exit_code,
            });
        }
        // Best-effort summary read. Tolerate a missing atoms.json
        // (e.g. enrichment skipped phases that produce atoms).
        atlas_summary = read_atlas_summary(&engine, &final_corpus_id).await;
    }

    // ── Step 7: one-hop "minesweeper" expansion. ─────────
    //
    // After the requested article lands, eagerly queue the articles
    // it links to. Rationale: the user has expressed interest in the
    // primary article's neighbourhood — the next question they ask
    // is much more likely to be about a linked concept than a
    // random one. Pre-loading turns that next fetch from a 30s
    // round-trip into an instant local hit.
    //
    // Gates:
    //   - caller must opt in (`request.expand_links = true`),
    //   - catalog config must opt in (`expansion_enabled = true`),
    //   - target_corpus_id must be set (else each expansion would
    //     create one more per-work corpus, defeating the point).
    //
    // The recursive expansion call sets `expand_links = false` so
    // we never run more than one hop deep automatically.
    if expand_links && catalog_cfg.expansion_enabled && catalog_cfg.target_corpus_id.is_some() {
        let neighbours =
            match collect_expansion_neighbours(&engine, &catalog_cfg, &final_corpus_id, &work_id)
                .await
            {
                Ok(list) => list,
                Err(e) => {
                    tracing::warn!(
                        primary = %work_id,
                        error = %e,
                        "catalog_ingest: link-expansion enumeration failed (non-fatal)"
                    );
                    Vec::new()
                }
            };
        if !neighbours.is_empty() {
            tracing::info!(
                primary = %work_id,
                queued = neighbours.len(),
                "catalog_ingest: queued one-hop minesweeper expansion"
            );
            spawn_minesweeper_queue(Arc::clone(&engine), catalog_corpus_id.clone(), neighbours);
        }
    }

    emit(CatalogIngestEvent::Complete {
        new_corpus_id: final_corpus_id.clone(),
        chunks_created: ingest_result.chunks_created,
        atlas_summary,
    });

    Ok(final_corpus_id)
}

/// Re-fetch the primary article's Action API JSON and pull a ranked
/// list of mainspace neighbour titles to pre-load. We re-call the
/// API rather than reading the staged corpus because (a) the staging
/// dir was already deleted by the append step and (b) the API
/// response is the authoritative `outgoing_links` source — the
/// extractor's chunk metadata is downstream of it.
async fn collect_expansion_neighbours(
    engine: &CorpusEngine,
    catalog_cfg: &corpus_engine::recipe::CatalogConfig,
    target_corpus_id: &str,
    work_id: &str,
) -> Result<Vec<String>, String> {
    let cap = catalog_cfg.expansion_link_cap as usize;
    if cap == 0 {
        return Ok(Vec::new());
    }
    let url = catalog_cfg.download_url_template.replace("{id}", work_id);

    // Fetch the same Action API endpoint the primary ingest just
    // pulled. Cheap (~50ms) and isolates link extraction from any
    // cleanup of the staging dir.
    let client = reqwest::Client::builder()
        .user_agent("sovereign-catalog-ingest/0.1 (+https://sovereign.dev)")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("link-fetch GET: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("link-fetch HTTP {}", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("link-fetch parse: {e}"))?;

    let parse = body
        .get("parse")
        .ok_or_else(|| "missing `parse` field".to_string())?;
    let resolved_title = parse.get("title").and_then(|v| v.as_str()).unwrap_or("");

    let raw_links: Vec<String> = parse
        .get("links")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| {
                    // Mainspace only (ns=0); skip dead links.
                    let ns = l.get("ns").and_then(|v| v.as_i64())?;
                    if ns != 0 {
                        return None;
                    }
                    let exists = l.get("exists").and_then(|v| v.as_bool()).unwrap_or(true);
                    if !exists {
                        return None;
                    }
                    l.get("title").and_then(|v| v.as_str()).map(String::from)
                })
                .collect()
        })
        .unwrap_or_default();

    if raw_links.is_empty() {
        return Ok(Vec::new());
    }

    // Significance heuristic v1: document order. The Action API
    // returns links in wikitext order, so the first ~N are
    // overwhelmingly from the lead/early sections — exactly the
    // "most central concepts" of the article. Future heuristics
    // (re-rank by lead-section presence, link frequency, or a
    // pre-computed Wikipedia-graph PageRank) can replace this with
    // no API change.

    // Skip already-ingested neighbours by querying the canonical's
    // source_doc_ids. Each Wikipedia article has a stable URL like
    // `https://en.wikipedia.org/wiki/<Title>` which the extractor
    // stamps as `source_doc_id`. We map link titles to that URL
    // shape and dedupe.
    let canonical_path = engine.index_dir().join(target_corpus_id);
    let existing_ids: std::collections::HashSet<String> =
        match corpus_engine::CorpusIndex::open(&canonical_path).await {
            Ok(idx) => idx.list_indexed_source_doc_ids().await.unwrap_or_default(),
            Err(_) => Default::default(),
        };

    let primary_self = format!(
        "https://en.wikipedia.org/wiki/{}",
        resolved_title.replace(' ', "_")
    );

    let mut out = Vec::with_capacity(cap);
    let mut seen_titles: std::collections::HashSet<String> = std::collections::HashSet::new();
    for title in raw_links {
        if out.len() >= cap {
            break;
        }
        if title.is_empty() {
            continue;
        }
        // Don't re-fetch the article we just ingested.
        let url = format!("https://en.wikipedia.org/wiki/{}", title.replace(' ', "_"));
        if url == primary_self {
            continue;
        }
        if existing_ids.contains(&url) {
            continue;
        }
        if !seen_titles.insert(title.clone()) {
            continue;
        }
        out.push(title);
    }

    Ok(out)
}

/// Spawn a background task that fetches each neighbour into the
/// catalog's shared target corpus, with polite spacing so we don't
/// hammer the Action API. Each inner call sets `expand_links =
/// false` so the expansion never recurses past one hop.
fn spawn_minesweeper_queue(
    engine: Arc<CorpusEngine>,
    catalog_corpus_id: String,
    neighbours: Vec<String>,
) {
    tokio::spawn(async move {
        let total = neighbours.len();
        for (idx, title) in neighbours.into_iter().enumerate() {
            let work_id = title.replace(' ', "_");
            let req = CatalogIngestRequest {
                catalog_corpus_id: catalog_corpus_id.clone(),
                work_id: work_id.clone(),
                enrich: false,
                progress: None,
                cancel: None,
                expand_links: false,
            };
            match run_catalog_ingest(Arc::clone(&engine), req).await {
                Ok(corpus_id) => {
                    tracing::info!(
                        idx = idx + 1,
                        total,
                        title = %title,
                        corpus = %corpus_id,
                        "minesweeper: neighbour ingested"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        idx = idx + 1,
                        total,
                        title = %title,
                        error = %e,
                        "minesweeper: neighbour ingest failed (continuing)"
                    );
                }
            }
            // Polite spacing between Action API calls. The hard
            // rate limit is generous (1k req/hour anonymous) but
            // we'd rather not flood it on a single user prompt.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        tracing::info!(total, "minesweeper: expansion queue drained");
    });
}

/// Patch a content recipe in place with the on-demand override
/// fields. Pure for testability — no IO, no engine calls.
///
/// `parent_corpus_id` is the catalog corpus by default. The recipe
/// itself may pre-declare a different parent (e.g. `wikipedia-article`
/// sets `parent_corpus_id = "wikipedia"` so fetched Wikipedia
/// articles surface under the user's existing Wikipedia corpus
/// rather than under `wikipedia-catalog`); when the recipe has a
/// non-empty `parent_corpus_id` we keep it.
pub(crate) fn patch_content_recipe(
    recipe: &mut Recipe,
    new_corpus_id: &str,
    parent_corpus_id: &str,
    download_url: &str,
) {
    recipe.corpus.id = new_corpus_id.to_string();
    if recipe
        .corpus
        .parent_corpus_id
        .as_deref()
        .unwrap_or("")
        .is_empty()
    {
        recipe.corpus.parent_corpus_id = Some(parent_corpus_id.to_string());
    }
    // The on-demand guard in `ingest()` only relaxes when the recipe
    // is handed via CorpusSpec::Inline — leave `on_demand` set so a
    // future direct ingest of the *patched* recipe (saved to disk)
    // would still be refused.
    if let corpus_engine::recipe::AcquirerConfig::BulkDownload { url, urls, .. } =
        &mut recipe.acquire
    {
        *url = Some(download_url.to_string());
        *urls = None;
    }
}

/// Look up a work's title in the catalog index by FTS matching the
/// `Gutenberg ID: <id>` line we stamped at extraction time. Returns
/// the matched chunk's title, or `None` if the work isn't present.
async fn lookup_work_title(
    engine: &CorpusEngine,
    catalog_info: &corpus_engine::types::IndexInfo,
    work_id: &str,
) -> Option<String> {
    let idx = engine.open_index(&catalog_info.path).await.ok()?;
    // Empty embedding → FTS-only path. The catalog's content carries
    // `Gutenberg ID: <id>` so a literal-id query reliably matches.
    let scored = idx
        .search(&[], &format!("\"Gutenberg ID: {work_id}\""), 1)
        .await
        .ok()?;
    let hit = scored.into_iter().next()?;
    Some(
        hit.title
            .clone()
            .or_else(|| hit.metadata.get("title").cloned())
            .unwrap_or_else(|| format!("Gutenberg #{work_id}")),
    )
}

/// Best-effort atlas summary read. Returns `None` if the atlas
/// directory isn't there yet (legitimate when enrichment was
/// skipped) or if the JSON files don't deserialize cleanly.
async fn read_atlas_summary(engine: &CorpusEngine, corpus_id: &str) -> Option<AtlasSummary> {
    let info = engine
        .installed_indexes()
        .await
        .ok()?
        .into_iter()
        .find(|i| i.corpus_id == corpus_id)?;
    let atlas_dir = info.path.join("atlas");
    let atoms_path = atlas_dir.join("atoms.json");
    let edges_path = atlas_dir.join("edges.json");
    if !atoms_path.exists() {
        return None;
    }
    let atoms_raw = std::fs::read_to_string(&atoms_path).ok()?;
    let atoms_value: serde_json::Value = serde_json::from_str(&atoms_raw).ok()?;
    let atoms_count = atoms_value.as_array().map(|a| a.len()).unwrap_or(0) as u64;
    let edges_count = std::fs::read_to_string(&edges_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_array().map(|a| a.len()))
        .unwrap_or(0) as u64;
    let themes = atoms_value
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|a| a.get("kind").and_then(|k| k.as_str()) == Some("theme"))
                .count() as u64
        })
        .unwrap_or(0);
    let questions = atoms_value
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|a| a.get("kind").and_then(|k| k.as_str()) == Some("question"))
                .count() as u64
        })
        .unwrap_or(0);
    Some(AtlasSummary {
        atoms: atoms_count,
        edges: edges_count,
        themes,
        questions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::recipe::{
        AcquirerConfig, ChunkerConfig, CorpusMeta, ExtractorConfig, IndexConfig,
    };

    fn fake_content_recipe() -> Recipe {
        Recipe {
            corpus: CorpusMeta {
                id: "gutenberg-work".into(),
                name: "Gutenberg Work".into(),
                description: String::new(),
                license: "Public Domain".into(),
                mesh_sharing: true,
                scope: None,
                query_sharing: None,
                size_compressed_gb: 0.0,
                size_indexed_gb: 0.0,
                schema_version: 1,
                kind: corpus_engine::types::CorpusKind::Knowledge,
                on_demand: true,
                parent_corpus_id: None,
                mutable_merge: None,
            },
            acquire: AcquirerConfig::BulkDownload {
                url: Some("https://example.com/PLACEHOLDER".into()),
                urls: None,
                resume: true,
            },
            extract: ExtractorConfig::Plaintext {
                title_pattern: None,
                strip_boilerplate: None,
            },
            chunk: ChunkerConfig::Sentence { max_chars: 2048 },
            index: IndexConfig::default(),
            enrichment: None,
            update: None,
            prebuilt: None,
            catalog: None,
            filters: Vec::new(),
            filter_mode: Default::default(),
            parameters: Default::default(),
            resolved_parameters: Default::default(),
            display: None,
            retrieval: Default::default(),
        }
    }

    #[test]
    fn patch_content_recipe_overrides_id_url_and_parent() {
        let mut r = fake_content_recipe();
        patch_content_recipe(
            &mut r,
            "gutenberg-2701",
            "gutenberg",
            "https://www.gutenberg.org/cache/epub/2701/pg2701.txt",
        );
        assert_eq!(r.corpus.id, "gutenberg-2701");
        assert_eq!(r.corpus.parent_corpus_id.as_deref(), Some("gutenberg"));
        assert!(r.corpus.on_demand, "on_demand stays true so a future direct ingest of this patched recipe would still be refused");
        match r.acquire {
            AcquirerConfig::BulkDownload { url, urls, .. } => {
                assert_eq!(
                    url.as_deref(),
                    Some("https://www.gutenberg.org/cache/epub/2701/pg2701.txt")
                );
                assert!(urls.is_none());
            }
            other => panic!("expected BulkDownload, got {other:?}"),
        }
    }

    #[test]
    fn patch_respects_recipe_declared_parent() {
        // The wikipedia-article recipe pre-declares
        // `parent_corpus_id = "wikipedia"` so fetched articles
        // surface under the user's existing Wikipedia corpus
        // rather than the catalog id (`wikipedia-catalog`).
        let mut r = fake_content_recipe();
        r.corpus.parent_corpus_id = Some("wikipedia".into());
        patch_content_recipe(
            &mut r,
            "wikipedia-catalog-Roman_Empire",
            "wikipedia-catalog", // catalog id — would be the default
            "https://en.wikipedia.org/w/api.php?action=parse&page=Roman_Empire&redirects=1",
        );
        assert_eq!(
            r.corpus.parent_corpus_id.as_deref(),
            Some("wikipedia"),
            "recipe-declared parent should win over the catalog default"
        );
    }

    #[test]
    fn per_work_corpus_id_is_stable() {
        assert_eq!(per_work_corpus_id("gutenberg", "2701"), "gutenberg-2701");
        assert_eq!(per_work_corpus_id("gutenberg", "1342"), "gutenberg-1342");
    }
}
