// SPDX-License-Identifier: AGPL-3.0-or-later
//! OICP v0.4 §5 — client-port ingest extension.
//!
//! Thin adapters that expose the daemon's corpus-install lifecycle over the
//! *protocol* surface (`/oicp/v1/...` on the client port :9741), distinct
//! from the internal `/internal/corpus/*` routes on :9742 that carry
//! `sovereign_contracts::daemon_wire::IngestProgress` on the wire. Here we translate to the
//! protocol DTOs (`CorpusIngestProgress`, `RecipeTestReport`) so a client
//! built only against `oicp-types` — one that never links the reference
//! corpus engine — can drive an install and dry-run a recipe.
//!
//! Mounted on `client_router` inside `client_auth`: a non-loopback caller
//! must present a bearer token (§5.5); loopback is free. Pause / cancel /
//! expand stay internal-only (a spec'd non-goal).

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;

use oicp_types::{
    CorpusIngestProgress, CorpusInstallRequest, CorpusInstallResponse, CorpusProgressResponse,
    IngestPhase, RecipeTestReport, RecipeTestRequest,
};

use crate::routes_internal::{progress_fraction, spawn_corpus_install_with_parameters, ErrorBody};
use crate::state::AppState;

/// Default per-run sample when the client doesn't cap it — small enough to
/// stay a quick dry run, large enough to exercise extraction + chunking.
const DEFAULT_TEST_SAMPLE: u32 = 20;

/// `POST /oicp/v1/corpus/install` (§5.1). Idempotent: a second call while
/// the corpus is already installing returns `spawned: false`. Wraps the
/// internal `spawn_corpus_install_with_parameters` helper 1:1 — the wire
/// request shape is identical, so this is pure protocol re-framing.
pub async fn corpus_install(
    State(state): State<AppState>,
    Json(req): Json<CorpusInstallRequest>,
) -> Result<Json<CorpusInstallResponse>, (StatusCode, Json<ErrorBody>)> {
    if state.inner.node.corpus_engine.is_none() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "no corpus engine available on this node".into(),
            }),
        ));
    }
    let spawned =
        spawn_corpus_install_with_parameters(state, req.corpus_id.clone(), req.parameters).await;
    Ok(Json(CorpusInstallResponse {
        corpus_id: req.corpus_id,
        spawned,
    }))
}

/// `GET /oicp/v1/corpus/progress` (§5.2). Projects the internal
/// `corpus_id -> IngestProgress` snapshot onto the coarse protocol
/// `CorpusIngestProgress`, so no `corpus_engine` type reaches the wire.
pub async fn corpus_progress(State(state): State<AppState>) -> Json<CorpusProgressResponse> {
    let snapshot = state.inner.ingest.corpus_progress.read().await;
    let progress = snapshot
        .iter()
        .map(|(id, p)| (id.clone(), map_progress(p)))
        .collect();
    Json(CorpusProgressResponse { progress })
}

/// `POST /oicp/v1/recipe/test` (§5.4). Dry-runs a recipe's acquire →
/// extract → chunk pipeline over a small sample and returns a per-stage
/// report. Never embeds (a dry run), never writes a durable corpus.
pub async fn recipe_test(
    State(state): State<AppState>,
    Json(req): Json<RecipeTestRequest>,
) -> Result<Json<RecipeTestReport>, (StatusCode, Json<ErrorBody>)> {
    let Some(engine) = state.inner.node.corpus_engine.clone() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "no corpus engine available on this node".into(),
            }),
        ));
    };

    // The engine tests from a recipe *file*: stage the supplied TOML in a
    // throwaway dir (which also absorbs the default `TEST_REPORT.md` write).
    // The dir is removed when `tmp` drops, after `test_recipe` returns.
    let tmp = tempfile::TempDir::new().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: format!("could not create a temp dir for recipe test: {e}"),
            }),
        )
    })?;
    let recipe_path = tmp.path().join("recipe.toml");
    std::fs::write(&recipe_path, req.recipe_toml.as_bytes()).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: format!("could not stage recipe TOML: {e}"),
            }),
        )
    })?;

    // Protocol `offline` means "no network acquisition". The engine has no
    // cached-input mode, so honour it as validate-only (`sample_size = 0`
    // skips download + extraction). Otherwise pull a bounded sample.
    let sample_size = if req.options.offline {
        0
    } else {
        req.options.sample_limit.unwrap_or(DEFAULT_TEST_SAMPLE) as usize
    };
    let options = corpus_engine::testing::TestOptions {
        sample_size,
        embed: false,
        queries: None,
        output: None,
        offline: req.options.offline,
        verbose: false,
        parameters: std::collections::BTreeMap::new(),
    };

    let report = engine
        .test_recipe(&recipe_path, &options)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorBody {
                    error: format!("recipe test failed: {e}"),
                }),
            )
        })?;
    Ok(Json(corpus_engine::engine::daemon_port::map_test_report(
        &report,
    )))
}

/// Project one internal `IngestProgress` onto the coarse protocol phase.
/// The finer engine phases (extract, chunk) collapse into `Downloading`
/// (the "acquiring & preparing" band) with the true phase in `detail`, so
/// the protocol's monotone phase ladder holds while nothing is lost.
fn map_progress(p: &sovereign_contracts::daemon_wire::IngestProgress) -> CorpusIngestProgress {
    use sovereign_contracts::daemon_wire::IngestProgress as P;
    let (phase, detail) = match p {
        P::Downloading { .. } => (IngestPhase::Downloading, None),
        P::Extracting {
            documents_processed,
        } => (
            IngestPhase::Downloading,
            Some(format!("extracting ({documents_processed} documents)")),
        ),
        P::Chunking { chunks_created } => (
            IngestPhase::Downloading,
            Some(format!("chunking ({chunks_created} chunks)")),
        ),
        P::Embedding { .. } => (IngestPhase::Embedding, None),
        P::Indexing { .. } => (IngestPhase::Indexing, None),
        P::OptimizingIndex { .. } => (IngestPhase::Optimizing, None),
        P::Enriching { phase, detail, .. } => {
            (IngestPhase::Enriching, Some(format!("{phase}: {detail}")))
        }
        P::Complete { .. } => (IngestPhase::Complete, None),
        // The protocol's other terminal phase. `CorpusIngestProgress::detail`
        // is documented as carrying the error message when
        // `phase = Failed`, so the engine's message goes there verbatim —
        // this mapping is what finally lets a peer's poll loop distinguish
        // a failed ingest from a completed one.
        P::Failed { message } => (IngestPhase::Failed, Some(message.clone())),
    };
    CorpusIngestProgress {
        phase,
        fraction: progress_fraction(p),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oicp_types::features;

    // A `map_progress` smoke: the fine engine phases fold onto the coarse
    // protocol ladder, and `detail` preserves the true phase.
    #[test]
    fn extract_and_chunk_fold_onto_downloading_band() {
        let extracting = sovereign_contracts::daemon_wire::IngestProgress::Extracting {
            documents_processed: 7,
        };
        let m = map_progress(&extracting);
        assert_eq!(m.phase, IngestPhase::Downloading);
        assert!(m.detail.as_deref().unwrap().contains("extracting"));

        let embedding = sovereign_contracts::daemon_wire::IngestProgress::Embedding {
            chunks_embedded: 5,
            total: 10,
            docs_processed: 2,
            chunks_per_sec: 3.0,
            expected_docs: None,
        };
        let m = map_progress(&embedding);
        assert_eq!(m.phase, IngestPhase::Embedding);
        assert_eq!(m.fraction, Some(0.5));

        let complete = sovereign_contracts::daemon_wire::IngestProgress::Complete {
            total_chunks: 100,
            duration_secs: 12,
        };
        assert!(map_progress(&complete).phase.is_terminal());
    }

    // The ingest feature strings this surface advertises are registered.
    #[test]
    fn advertised_ingest_features_are_registered() {
        assert!(features::is_valid(features::INGEST_V1));
        assert!(features::is_valid(features::INGEST_RECIPE_TEST));
    }
}
