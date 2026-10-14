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

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use std::collections::BTreeMap;

use oicp_types::{
    CorpusIngestProgress, CorpusInstallRequest, CorpusInstallResponse, CorpusProgressResponse,
    IngestDocument, IngestDocumentState, IngestPhase, RecipeTestReport, RecipeTestRequest,
};

use crate::routes_internal::{
    install_status, progress_fraction, spawn_install, ErrorBody, InstallSource,
};
use crate::state::AppState;

/// OICP v0.6 §2.12: the largest install body this host reads, stated as
/// `knowledge.ingest.max_install_bytes` (`routes_oicp`) and set on the install
/// route alone (`server::client_router`). Every other route on the client
/// surface keeps `MAX_REQUEST_BODY_BYTES` (8 MiB), which refused a
/// 22,627,179-byte inline library `413`. 64 MiB holds that library, escaped
/// twice (TOML, then JSON), with room for one near three times its size; the
/// body is buffered whole, so this is also the transient memory one install
/// may take before its recipe is read.
pub const MAX_INSTALL_BYTES: usize = 64 * 1024 * 1024;

/// Default per-run sample when the client doesn't cap it — small enough to
/// stay a quick dry run, large enough to exercise extraction + chunking.
const DEFAULT_TEST_SAMPLE: u32 = 20;

/// `POST /oicp/v1/corpus/install` (§5.1; v0.5 §4 with `recipe_toml`).
///
/// By id, the registry resolves the recipe. With `recipe_toml`, that recipe
/// is installed under `corpus_id`, validated by the one load boundary
/// (`Recipe::from_toml`), and the reply carries its `recipe_sha256`.
/// Idempotent: a second call while the corpus is installing, or with the
/// same recipe bytes as the installed corpus's stamp, is `spawned: false`; a
/// different recipe reingests. An invalid recipe or invalid parameters are a
/// 400 and an unknown id a 404, never a 200 `spawned: false`
/// (`install_status`, the mapping the internal route shares).
///
/// A body past [`MAX_INSTALL_BYTES`] is `413 install too large` naming the
/// limit (v0.6 §2.12), never axum's unnamed 413; any other body that does not
/// decode keeps axum's rejection.
pub async fn corpus_install(
    State(state): State<AppState>,
    req: Result<Json<CorpusInstallRequest>, JsonRejection>,
) -> Response {
    let req = match req {
        Ok(Json(req)) => req,
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            tracing::info!(
                limit = MAX_INSTALL_BYTES,
                "oicp install: refused, the body is past max_install_bytes"
            );
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(serde_json::json!({
                    "error": oicp_types::ingest::reasons::INSTALL_TOO_LARGE,
                    "max_install_bytes": MAX_INSTALL_BYTES,
                })),
            )
                .into_response();
        }
        Err(rejection) => {
            tracing::info!(%rejection, "oicp install: refused, the body does not decode");
            return rejection.into_response();
        }
    };
    tracing::debug!(
        corpus = %req.corpus_id,
        recipe_bytes = req.recipe_toml.as_ref().map(String::len),
        "oicp install: request read"
    );
    match install(state, req).await {
        Ok(reply) => reply.into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

/// The install itself, once its body is read: the registry's recipe, or the
/// one the request carried, through the outcome mapping both install routes
/// share.
async fn install(
    state: AppState,
    req: CorpusInstallRequest,
) -> Result<Json<CorpusInstallResponse>, (StatusCode, Json<ErrorBody>)> {
    let recipe_sha256 = req
        .recipe_toml
        .as_deref()
        .map(corpus_index::corpus::recipe_sha256);
    let source = match req.recipe_toml {
        Some(toml) => InstallSource::Recipe(toml),
        None => InstallSource::Registry,
    };
    let outcome = spawn_install(state, req.corpus_id.clone(), req.parameters, source).await;
    let spawned = install_status(outcome, &req.corpus_id)?;
    Ok(Json(CorpusInstallResponse {
        corpus_id: req.corpus_id,
        spawned,
        recipe_sha256,
    }))
}

/// `GET /oicp/v1/corpus/progress` (§5.2). Projects the internal
/// `corpus_id -> IngestProgress` snapshot onto the coarse protocol
/// `CorpusIngestProgress`, so no `corpus_engine` type reaches the wire.
///
/// v0.6 §2.9: every install is listed from its spawn. `active_ingests` is
/// set before install's reply, while the first progress entry waits on the
/// engine's first callback, so an install with none yet is `pending` here,
/// the union `/internal/corpus/status` already reads. Once an install has
/// returned, its entry lists each document it read, from the record kept
/// with the corpus (`Corpus::install_documents`). A corpus held from before
/// the daemon's last start has no entry in memory; its record lists it
/// `complete` (platform row PH), so a client polling after a `spawned:
/// false` learns its documents rather than nothing.
pub async fn corpus_progress(State(state): State<AppState>) -> Json<CorpusProgressResponse> {
    let active = state.inner.ingest.active_ingests.read().await.clone();
    let snapshot = state.inner.ingest.corpus_progress.read().await;
    let mut progress: BTreeMap<String, CorpusIngestProgress> = snapshot
        .iter()
        .map(|(id, p)| (id.clone(), map_progress(p)))
        .collect();
    drop(snapshot);
    let mut spawned_only = 0usize;
    for id in active {
        progress.entry(id).or_insert_with(|| {
            spawned_only += 1;
            CorpusIngestProgress {
                phase: IngestPhase::Pending,
                fraction: None,
                detail: Some("spawned".into()),
                documents: Vec::new(),
                documents_unlisted: None,
            }
        });
    }
    let index_dir = state
        .inner
        .node
        .corpus_engine
        .as_ref()
        .map(|e| e.index_dir().to_path_buf());
    let held = index_dir
        .as_deref()
        .map(|dir| held_with_documents(dir, &progress))
        .unwrap_or_default();
    for id in &held {
        progress.insert(
            id.clone(),
            CorpusIngestProgress {
                phase: IngestPhase::Complete,
                fraction: Some(1.0),
                detail: Some("held".into()),
                documents: Vec::new(),
                documents_unlisted: None,
            },
        );
    }
    let mut listed = 0usize;
    for (id, entry) in progress.iter_mut() {
        let documents = index_dir
            .as_deref()
            .and_then(|dir| corpus_index::corpus::Corpus::named(dir, id))
            .and_then(|c| c.install_documents());
        if let Some(d) = documents {
            entry.documents = d.documents.iter().map(wire_document).collect();
            entry.documents_unlisted = (d.unlisted > 0).then_some(d.unlisted);
            listed += entry.documents.len();
        }
    }
    tracing::debug!(
        corpora = progress.len(),
        spawned_only,
        held = held.len(),
        documents_listed = listed,
        "oicp corpus progress: answered"
    );
    Json(CorpusProgressResponse { progress })
}

/// The corpora under `index_dir` that keep an install-documents record and
/// have no entry in `progress`: held from before the daemon's last start.
fn held_with_documents(
    index_dir: &std::path::Path,
    progress: &BTreeMap<String, CorpusIngestProgress>,
) -> Vec<String> {
    let entries = match std::fs::read_dir(index_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(dir = %index_dir.display(), error = %e, "oicp corpus progress: the index directory does not read, no held corpus listed");
            return Vec::new();
        }
    };
    entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|id| !progress.contains_key(id))
        .filter(|id| {
            corpus_index::corpus::Corpus::named(index_dir, id)
                .is_some_and(|c| c.install_documents_path().is_file())
        })
        .collect()
}

/// An ingest's document outcome on the wire (v0.6 §2.9).
fn wire_document(d: &corpus_index::ingest_port::daemon::DocumentOutcome) -> IngestDocument {
    use corpus_index::ingest_port::daemon::DocumentState;
    IngestDocument {
        source: d.source.clone(),
        state: match &d.state {
            DocumentState::Ready { text_sha256 } => IngestDocumentState::Ready {
                text_sha256: text_sha256.map(|h| h.to_hex()),
            },
            DocumentState::Failed { reason } => IngestDocumentState::Failed {
                reason: reason.clone(),
            },
        },
    }
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
                error: crate::hosted_ingest::NO_INGEST.into(),
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
    let report = engine
        .test_recipe_report(&recipe_path, sample_size, req.options.offline)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorBody {
                    error: format!("recipe test failed: {e}"),
                }),
            )
        })?;
    // The port hands the protocol report over as its wire JSON (its leaf
    // cannot name oicp-types); this is the same bytes read back.
    let report: RecipeTestReport = serde_json::from_value(report).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: format!("recipe test report did not decode: {e}"),
            }),
        )
    })?;
    Ok(Json(report))
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
        documents: Vec::new(),
        documents_unlisted: None,
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
