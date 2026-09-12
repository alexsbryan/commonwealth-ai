// SPDX-License-Identifier: AGPL-3.0-or-later
//! The enrichment-store reads the desktop stops doing in-process
//! (thin-desktop order, 2026-09-11): the enriched-corpus inventory and the
//! starter questions mined from a corpus's atlas.
//!
//! Both read the DAEMON's data root. `enrich_commands.rs` read
//! `sovereign_enrichment_catalog::paths::enrichment_dir()` — this process's
//! default data root, which on an attached boot is the laptop's, not the
//! host's — and pulled every atom of a corpus over the wire to pick six
//! questions out of it. Same loopback posture as `reading_http`.

use std::sync::Arc;

use axum::extract::{Extension, Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use corpus_engine::enrichment::atlas::analysis::starter_questions::rank_starter_questions;
use corpus_engine::enrichment::atlas::read_atlas_atoms;
use corpus_engine::CorpusEngine;

use crate::daemon::EmbeddedDaemon;
use crate::http_response::Absence;
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

/// `?limit=` — how many starter questions to mine. `None` is the route's
/// default; an over-large ask is clamped and served, not refused.
#[derive(Debug, Default, Deserialize)]
pub struct StarterQuestionsQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

pub const STARTER_QUESTIONS_DEFAULT: usize = 6;
pub const STARTER_QUESTIONS_MAX: usize = 50;

/// The enrichment-store router. Mounted unconditionally beside
/// `reading_http`; a daemon with no corpus engine answers 503 naming that,
/// which is a different fact from an unmounted router's 404.
pub fn enrich_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/internal/corpus/enriched", get(enriched))
        .route(
            "/internal/corpus/{corpus}/starter-questions",
            get(starter_questions),
        )
        .localhost_only_with(daemon)
}

/// GET `/internal/corpus/enriched` — every enrichment workspace under the
/// daemon's data root with a loadable config, newest first. Answers
/// `Vec<EnrichedCorpusSummary>`; an absent store is `[]` (no corpora is a
/// fact about the store, not a failure to read it — the catalog's rule).
async fn enriched(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Result<Response, Absence> {
    let root = daemon.data_dir().join("enrichment");
    let rows = sovereign_enrichment_catalog::catalog::list_enriched_corpora_in(&root)
        .map_err(|e| Absence::internal(format!("enrichment catalog: {e}")))?;
    tracing::debug!(root = %root.display(), returned = rows.len(), "enrich_http: enriched corpora listed");
    Ok((StatusCode::OK, Json(rows)).into_response())
}

/// GET `/internal/corpus/{corpus}/starter-questions?limit=` — up to `limit`
/// starter questions mined from the corpus's atlas. Answers
/// `Vec<StarterQuestion>`. A corpus with no atlas is a 404 NAMING it: the
/// desktop turns that into its "excerpt starters" branch, and the two
/// were indistinguishable from an unreachable host before (ARCH principle 6).
async fn starter_questions(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
    Query(q): Query<StarterQuestionsQuery>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let atlas_dir = atlas_dir_for(engine, &corpus).await?;
    let file = read_atlas_atoms(&atlas_dir)
        .map_err(|e| Absence::internal(format!("read atoms for `{corpus}`: {e}")))?;
    let limit = q
        .limit
        .unwrap_or(STARTER_QUESTIONS_DEFAULT)
        .min(STARTER_QUESTIONS_MAX);
    let starters = rank_starter_questions(&file.atoms, limit);
    tracing::debug!(
        corpus = %corpus,
        total_atoms = file.atoms.len(),
        limit,
        returned = starters.len(),
        "enrich_http: starter questions served"
    );
    Ok((StatusCode::OK, Json(starters)).into_response())
}

fn engine_for(daemon: &Arc<EmbeddedDaemon>) -> Result<&Arc<CorpusEngine>, Absence> {
    daemon
        .corpus_engine()
        .ok_or_else(|| Absence::unavailable("corpus engine not initialised"))
}

/// The corpus's `atlas/` under the DAEMON's index dir. Two absences named
/// apart: not installed, and installed with no atlas.
async fn atlas_dir_for(
    engine: &Arc<CorpusEngine>,
    corpus_id: &str,
) -> Result<std::path::PathBuf, Absence> {
    let installed = engine
        .installed_indexes()
        .await
        .map_err(|e| Absence::internal(format!("installed_indexes: {e}")))?;
    let entry = installed
        .iter()
        .find(|i| i.corpus_id == corpus_id)
        .ok_or_else(|| Absence::missing(format!("corpus `{corpus_id}` is not installed")))?;
    let atlas_dir = entry.path.join("atlas");
    if !atlas_dir.join("atoms.json").is_file() {
        return Err(Absence::missing(format!(
            "corpus `{corpus_id}` has no atlas"
        )));
    }
    Ok(atlas_dir)
}
