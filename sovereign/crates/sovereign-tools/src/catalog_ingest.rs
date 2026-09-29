// SPDX-License-Identifier: AGPL-3.0-or-later
//! On-demand single-work catalog ingest.
//!
//! When the user accepts an ingest offer for a catalog hit (e.g. "yes,
//! read Moby Dick"), this module orchestrates the end-to-end flow:
//!
//! 1. **Resolve.** Open the catalog corpus index and FTS-look-up the
//!    work id; pull title/url/metadata off the matched chunk.
//! 2. **Ingest, through ingest's port.** [`CatalogIngestPort`] fetches
//!    the content recipe (e.g. `gutenberg-work`) from the registry,
//!    patches its `corpus.id` (`<catalog>-<work_id>`),
//!    `corpus.parent_corpus_id`, and the acquire URL (substituting `{id}`
//!    in the catalog template), and ingests it inline. The on-demand
//!    guard in `ingest()` requires that entry point — a direct
//!    `Builtin("gutenberg-work")` ingest is refused.
//! 3. **Fold.** With a shared `target_corpus_id` the port appends the
//!    staging corpus into it and removes the staging dir.
//! 4. **Enrich (optional).** If [`CatalogIngestRequest::enrich`] is
//!    set, fire `sovereign-cli enrich build <new_corpus_id>` via
//!    [`crate::enrich::run_enrich_build`] and stream its
//!    [`sovereign_contracts::daemon_wire::enrich_progress::EnrichProgress`]
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

use corpus_index::ingest_port::{
    CatalogIngestPort, CatalogWork, CatalogWorkError, ProgressCallback,
};
use corpus_index::types::CorpusKind;
use serde::{Deserialize, Serialize};
use sovereign_contracts::daemon_wire::IngestProgress;
use understanding_vocab::atoms::{AtomEnvelope, AtomType};
use understanding_vocab::read::{read_atlas_atoms, read_atlas_edges};
use understanding_vocab::taxonomy::EntityType;

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
    Enrich(Box<sovereign_contracts::daemon_wire::enrich_progress::EnrichProgress>),
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
        source: corpus_index::Error,
    },

    #[error("ingest failed: {source}")]
    Ingest {
        #[source]
        source: corpus_index::Error,
    },

    #[error("enrichment failed (exit code {exit_code})")]
    Enrich { exit_code: i32 },
}

pub type CatalogIngestResult<T> = std::result::Result<T, CatalogIngestError>;

/// Drive the on-demand single-work catalog ingest. Returns the
/// per-work corpus id on success.
pub async fn run_catalog_ingest(
    engine: Arc<dyn CatalogIngestPort>,
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
    let catalog_cfg = engine
        .catalog_config(&catalog_corpus_id)
        .await
        .map_err(|source| CatalogIngestError::ContentRecipeLoad {
            content_recipe: catalog_corpus_id.clone(),
            source,
        })?
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
    let title_for_event = lookup_work_title(engine.as_ref(), catalog_info, &work_id)
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

    // ── Steps 4-5a: ingest the work through ingest's port. ─────
    //
    // The port loads and patches the content recipe, ingests it, and
    // — when `[catalog].target_corpus_id` is set — folds the staging
    // corpus into the shared canonical and removes it.
    let ingest_progress: Option<ProgressCallback> =
        progress.as_ref().map(|outer| -> ProgressCallback {
            let outer = outer.clone();
            Box::new(move |ev: IngestProgress| {
                outer(CatalogIngestEvent::Ingest(ev));
            })
        });
    let work = CatalogWork {
        content_recipe: catalog_cfg.content_recipe.clone(),
        catalog_corpus_id: catalog_corpus_id.clone(),
        download_url: download_url.clone(),
        staging_corpus_id: staging_corpus_id.clone(),
        shared_target: use_shared_target.then(|| final_corpus_id.clone()),
    };
    let ingested = engine
        .ingest_catalog_work(&work, ingest_progress)
        .await
        .map_err(|e| match e {
            CatalogWorkError::ContentRecipeLoad(source) => CatalogIngestError::ContentRecipeLoad {
                content_recipe: catalog_cfg.content_recipe.clone(),
                source,
            },
            CatalogWorkError::Ingest(source) => CatalogIngestError::Ingest { source },
        })?;
    let catalog_content_opts_out_of_auto_enrichment = ingested.opts_out_of_auto_enrichment;

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
    if catalog_content_opts_out_of_auto_enrichment {
        tracing::info!(
            corpus = %final_corpus_id,
            "catalog_ingest: content recipe is retrieval-only ([enrichment] enabled=false) — skipping structural atlas"
        );
    } else {
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
        atlas_summary = read_atlas_summary(engine.as_ref(), &final_corpus_id).await;
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
        let neighbours = match collect_expansion_neighbours(
            engine.as_ref(),
            &catalog_cfg,
            &final_corpus_id,
            &work_id,
        )
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
        chunks_created: ingested.chunks_created,
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
    engine: &dyn CatalogIngestPort,
    catalog_cfg: &corpus_index::recipe::CatalogConfig,
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
        match corpus_index::index::CorpusIndex::open(&canonical_path).await {
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
    engine: Arc<dyn CatalogIngestPort>,
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

/// Look up a work's title in the catalog index by FTS matching the
/// `Gutenberg ID: <id>` line we stamped at extraction time. Returns
/// the matched chunk's title, or `None` if the work isn't present.
async fn lookup_work_title(
    engine: &dyn CatalogIngestPort,
    catalog_info: &corpus_index::types::IndexInfo,
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
async fn read_atlas_summary(
    engine: &dyn CatalogIngestPort,
    corpus_id: &str,
) -> Option<AtlasSummary> {
    let info = engine
        .installed_indexes()
        .await
        .ok()?
        .into_iter()
        .find(|i| i.corpus_id == corpus_id)?;
    let atlas_dir = info.path.join("atlas");
    // The door parses both files. A missing or unparseable artefact is the
    // same best-effort case this summary always tolerated — the typed read
    // just replaces the `serde_json::Value` walk that treated an `AtomsFile`
    // object as a bare array and so counted zero of everything.
    let atoms = read_atlas_atoms(&atlas_dir).ok()?;
    let edges_count = read_atlas_edges(&atlas_dir)
        .map(|e| e.edges.len() as u64)
        .unwrap_or(0);
    let atoms_count = atoms.atoms().len() as u64;
    let questions = atoms
        .atoms()
        .iter()
        .filter(|a| a.atom_type() == AtomType::Question)
        .count() as u64;
    // A "theme" is a Concept entity: the literary atlas's ontology declares
    // `concept` under the label `theme`
    // (corpus-engine/tests/main/pipeline_ontology.rs:48-64), and no atom
    // carries a `theme` type of its own.
    let themes = atoms
        .atoms()
        .iter()
        .filter(|a| matches!(a, AtomEnvelope::Entity(e) if e.entity_type == EntityType::Concept))
        .count() as u64;
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

    #[test]
    fn per_work_corpus_id_is_stable() {
        assert_eq!(per_work_corpus_id("gutenberg", "2701"), "gutenberg-2701");
        assert_eq!(per_work_corpus_id("gutenberg", "1342"), "gutenberg-1342");
    }
}
