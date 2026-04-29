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

use crate::enrich::{
    run_enrich_build, CancellationFlag, EnrichBuildConfig, EnrichProgressFn,
};

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
pub type CatalogIngestProgressFn =
    Arc<dyn Fn(CatalogIngestEvent) + Send + Sync + 'static>;

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
    NotACatalog {
        corpus_id: String,
        kind: CorpusKind,
    },

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
    let catalog_cfg = catalog_recipe.catalog.clone().ok_or_else(|| {
        CatalogIngestError::MissingCatalogConfig {
            catalog_corpus_id: catalog_corpus_id.clone(),
        }
    })?;

    // ── Step 3: FTS-lookup the work in the catalog index. ──────
    //
    // Use a literal `id_field:work_id` query — Tantivy treats the
    // colon as a field-scoped query and we stamped the id into the
    // chunk content as `Gutenberg ID: <id>`. Fall back to a plain
    // text search if FTS isn't built (small catalogs use a flat
    // scan).
    let title_for_event = lookup_work_title(
        &engine,
        catalog_info,
        &work_id,
    )
    .await
    .ok_or_else(|| CatalogIngestError::WorkNotFound {
        catalog_corpus_id: catalog_corpus_id.clone(),
        work_id: work_id.clone(),
    })?;

    let download_url = catalog_cfg
        .download_url_template
        .replace("{id}", &work_id);
    let new_corpus_id = per_work_corpus_id(&catalog_corpus_id, &work_id);

    emit(CatalogIngestEvent::Resolved {
        title: title_for_event.clone(),
        download_url: download_url.clone(),
        new_corpus_id: new_corpus_id.clone(),
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
        &new_corpus_id,
        &catalog_corpus_id,
        &download_url,
    );

    // ── Step 5: ingest. ─────────────────────────────────
    let ingest_progress: Option<corpus_engine::progress::ProgressCallback> =
        progress.as_ref().map(|outer| -> corpus_engine::progress::ProgressCallback {
            let outer = outer.clone();
            Box::new(move |ev: IngestProgress| {
                outer(CatalogIngestEvent::Ingest(ev));
            })
        });
    let ingest_result = engine
        .ingest(
            &CorpusSpec::Inline(Box::new(content_recipe)),
            ingest_progress,
        )
        .await
        .map_err(|source| CatalogIngestError::Ingest { source })?;

    // Cooperative cancellation between ingest and enrich:
    // if the caller flipped the flag during ingest, skip
    // enrichment outright.
    let cancelled_mid = cancel
        .as_ref()
        .map(|f| f.load(std::sync::atomic::Ordering::SeqCst))
        .unwrap_or(false);

    let mut atlas_summary: Option<AtlasSummary> = None;

    // ── Step 6: enrich (optional). ─────────────────────
    if enrich && !cancelled_mid {
        let enrich_progress: Option<EnrichProgressFn> = progress
            .as_ref()
            .map(|outer| -> EnrichProgressFn {
                let outer = outer.clone();
                Arc::new(move |ev| {
                    outer(CatalogIngestEvent::Enrich(Box::new(ev)));
                })
            });
        let outcome = run_enrich_build(
            &new_corpus_id,
            EnrichBuildConfig {
                cli_path: None,
                extra_args: vec!["--full".into()],
                cancel: cancel.clone(),
            },
            enrich_progress,
        )
        .await
        .map_err(|e| CatalogIngestError::Enrich { exit_code: e.raw_os_error().unwrap_or(-1) })?;

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
        atlas_summary = read_atlas_summary(&engine, &new_corpus_id).await;
    }

    emit(CatalogIngestEvent::Complete {
        new_corpus_id: new_corpus_id.clone(),
        chunks_created: ingest_result.chunks_created,
        atlas_summary,
    });

    Ok(new_corpus_id)
}

/// Patch a content recipe in place with the on-demand override
/// fields. Pure for testability — no IO, no engine calls.
pub(crate) fn patch_content_recipe(
    recipe: &mut Recipe,
    new_corpus_id: &str,
    parent_corpus_id: &str,
    download_url: &str,
) {
    recipe.corpus.id = new_corpus_id.to_string();
    recipe.corpus.parent_corpus_id = Some(parent_corpus_id.to_string());
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
    fn per_work_corpus_id_is_stable() {
        assert_eq!(per_work_corpus_id("gutenberg", "2701"), "gutenberg-2701");
        assert_eq!(
            per_work_corpus_id("gutenberg", "1342"),
            "gutenberg-1342"
        );
    }
}
