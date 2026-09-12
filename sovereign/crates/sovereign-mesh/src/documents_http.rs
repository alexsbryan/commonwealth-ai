// SPDX-License-Identifier: AGPL-3.0-or-later
//! The document-asset family — `/v1/documents` (sv-surface D9a).
//!
//! Six routes over the daemon's own `StateStore` and `DocumentAssetManager`:
//! list, the legacy `documents` listing, promote, get, delete, rebuild
//! skeleton. `commands/document_asset.rs` read the DESKTOP's store handle,
//! the same sqlite file only because setup adopted `data_dir` (invariant
//! `attach_mode_data_dir_split`). `DocumentAsset` crosses WHOLE; only the
//! legacy fold and the promotion rule are minted here, because this is now
//! their one implementation (§10.6).
//!
//! Loopback posture is `reading_http`'s, unchanged.
//!
//! `upload_document_asset` crossed on 2026-09-11 as a JOB: `POST
//! /v1/documents` runs `prepare` inline (no inference; answers the Pending
//! record, 202) and spawns `run_ingest`, whose frames are appended to an
//! in-process log keyed by asset id and read back from `GET
//! /v1/documents/{id}/progress?after=N`. The header used to say this could
//! not cross because "`run_ingest` narrates one frame per embed batch and a
//! request/response route delivers exactly one" — `lc_http`'s ingest and
//! cluster jobs showed a poll loop re-emitting the host's frames IS the
//! channel. `asset_id` is stamped on every frame HERE, which the desktop
//! used to do per event (§10.6: one decider). The legacy paperclip path,
//! `ingest_document` → `rag::ingest::ingest_file` into the `documents`
//! table, crossed the same day as `POST /v1/documents/legacy`.
//!
//! `ask_document`'s document-operation half crossed the same day as a JOB:
//! `POST /v1/documents/{id}/ask` persists the question, then routes and
//! executes on this daemon's manager, narrating `OperationProgress` frames
//! into a log read from `GET /v1/documents/{id}/ask/{job_id}?after=N`,
//! whose terminal `outcome` is the persisted assistant message (answered),
//! a fall-through the client runs as an ordinary turn (off-topic, or RAG
//! found nothing — the command's two fallbacks, kept), or a failure naming
//! itself. The header used to keep this out because "a route plus a
//! generation is a TURN; serving it here mints a second driver beside
//! `serve_turn`". It does not: the document operation never WAS a turn
//! through the Runtime — it is the manager's own route/execute pair, and
//! the one arm that is a turn (the fall-through) still goes to the turn
//! driver, over the wire, from the client. What this module refuses is
//! inventing a turn wire that carries an attachment; the one-decider
//! answer that would retire this route is `TurnRequest` carrying the
//! asset id and the Runtime owning the branch — out of this change's scope.
//!
//! Does NOT cross, named rather than half-served: nothing in this family.
//!
//! The T2 entity pass runs on the daemon's OWN NER model. This module said
//! the opposite until 2026-09-11 — *"builds its manager with no
//! `EntityExtractor` because a daemon holds none"* — and that sentence was
//! false when it was written: `sovereign-runtime-recipe` fills
//! `LaneSources::gliner` for every host it commissions (`lib.rs:665`), and
//! the daemon commissions through it (`daemon_cmd/mod.rs:986`). The
//! extractor was loaded, wired into the corpus engine, and then not offered
//! to this surface — so `build_skeleton` took its LLM fallback on a host
//! that had the NER model resident. See [`manager_for`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::{Extension, Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use sovereign_contracts::types::{
    DocumentAssetOperation, Message, ResponseProvenance, Role, SourceSummary,
};
use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_core::types::DocumentAsset;
use sovereign_tools::document_asset::DocumentAssetManager;

use crate::daemon::EmbeddedDaemon;
use crate::http_response::{json_error, Absence};
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

// ─── The wire projections ──────────────────────────────────────

/// `GET /v1/documents`'s body.
///
/// An object rather than a bare array for the reason every list route
/// in this crate gives: a top-level array cannot grow a sibling field
/// (a count, a truncation marker) without breaking every caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentListResponse {
    pub documents: Vec<DocumentAsset>,
}

/// `GET /v1/documents/{id}` and the two write routes' body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentResponse {
    pub document: DocumentAsset,
}

/// A document in the legacy `documents` table with no `DocumentAsset`
/// record. Defined in `sovereign-contracts` so a client can parse it
/// without linking this crate; re-exported here so the routes below and
/// their tests keep naming it at this path (sv-surface svt-3).
pub use sovereign_contracts::daemon_wire::LegacyDocumentEntry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyDocumentListResponse {
    pub documents: Vec<LegacyDocumentEntry>,
}

/// `POST /v1/documents/legacy/promote`'s body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromoteLegacyRequest {
    /// The `source` key from a [`LegacyDocumentEntry`].
    pub source: String,
}

/// `POST /v1/documents`'s body — the file to ingest as a document asset.
/// A local PATH, the way `POST /internal/corpus/local` takes one: every
/// route here is `LocalOnly`, and the daemon reads the same disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadDocumentRequest {
    pub path: String,
}

/// Query of `GET /v1/documents/{id}/progress` and `…/ask/{job_id}`.
#[derive(Debug, Default, Deserialize)]
pub struct DocumentProgressQuery {
    /// The caller's cursor: frames at index `>= after` are returned.
    #[serde(default)]
    pub after: usize,
}

/// The answers of the job routes, defined in `sovereign-contracts` so a
/// client parses them without linking this crate, serialised here
/// (sv-surface svt-3).
pub use sovereign_contracts::daemon_wire::{
    AskJobAck, AskOutcome, AskProgress, DocumentIngestProgress, IngestLegacyResponse,
};

/// `POST /v1/documents/legacy`'s body — a file for the legacy
/// `documents` table (the old paperclip path).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestLegacyRequest {
    pub path: String,
}

/// `POST /v1/documents/{id}/ask`'s body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskDocumentRequest {
    pub question: String,
    /// The conversation the question and its answer are persisted into.
    pub conversation_id: String,
}

/// `DELETE /v1/documents/{id}`'s body.
///
/// A body rather than a 204, so a caller that only reads bodies still
/// learns that the delete happened (§18.3) — and so the route has
/// somewhere to say what it removed if it ever reports more.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletedResponse {
    pub deleted: bool,
    pub id: String,
}

// ─── Router ────────────────────────────────────────────────────

/// The document-asset router. Mounted unconditionally on serving
/// daemons; a commission with no `StateStore` answers a named 503,
/// which is a different fact from an unmounted route's 404 (§18.3).
///
/// `/v1/documents/legacy` is registered BEFORE `/v1/documents/{id}` —
/// axum 0.8 prefers the static segment either way, and the order here
/// says so to the reader rather than relying on it silently.
pub fn documents_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/documents", get(list_documents).post(upload_document))
        .route(
            "/v1/documents/legacy",
            get(list_legacy_documents).post(ingest_legacy),
        )
        .route("/v1/documents/legacy/promote", post(promote_legacy))
        .route(
            "/v1/documents/{id}",
            get(get_document).delete(delete_document),
        )
        .route("/v1/documents/{id}/skeleton", post(rebuild_skeleton))
        .route("/v1/documents/{id}/progress", get(ingest_progress))
        .route("/v1/documents/{id}/ask", post(ask_document))
        .route("/v1/documents/{id}/ask/{job_id}", get(ask_progress))
        .localhost_only_with(daemon)
}

// ─── Handlers ──────────────────────────────────────────────────

/// GET `/v1/documents` — every ingested document asset.
///
/// Store order, which is the order the desktop's picker already
/// rendered. Sorting here would be a presentation decision the caller
/// owns.
async fn list_documents(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    Ok(match store.list_document_assets().await {
        Ok(documents) => {
            tracing::debug!(count = documents.len(), "documents_http: assets listed");
            Json(DocumentListResponse { documents }).into_response()
        }
        Err(e) => internal_error("list_document_assets", &e.to_string()),
    })
}

/// GET `/v1/documents/{id}` — one asset record.
///
/// 404 for an id the store does not hold. The command it replaces
/// answered `Ok(None)`, which the frontend rendered as "not ready yet"
/// — the same shape as an asset mid-ingest. Two different facts, and
/// the pane offers a different remedy for each (§18.3).
async fn get_document(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    Ok(match store.get_document_asset(&id).await {
        Ok(Some(document)) => {
            tracing::debug!(asset = %id, state = %document.state.label(), "documents_http: asset read");
            Json(DocumentResponse { document }).into_response()
        }
        Ok(None) => not_found(&id),
        Err(e) => internal_error("get_document_asset", &e.to_string()),
    })
}

/// DELETE `/v1/documents/{id}` — remove an asset and its chunks.
///
/// Through `DocumentAssetManager::delete`, which owns the removal
/// order (chunks first, then the record). A raw
/// `StateStore::delete_document_asset` here would be a second, shorter
/// implementation of the same delete and would leave the chunks behind
/// (§10.6).
async fn delete_document(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Result<Response, Absence> {
    let manager = manager_for(&daemon)?;
    Ok(match manager.delete(&id).await {
        Ok(()) => {
            tracing::debug!(asset = %id, "documents_http: asset deleted");
            Json(DeletedResponse { deleted: true, id }).into_response()
        }
        Err(e) => internal_error("delete", &e.to_string()),
    })
}

/// POST `/v1/documents/{id}/skeleton` — rebuild the structural
/// skeleton from stored chunks and answer the refreshed record.
///
/// The self-heal the desktop spawns when `ask_document` finds
/// `skeleton: None`, and the button the picker offers, are the same
/// operation — this is it. Works from chunks, so no file is required
/// and an asset whose original ingest was interrupted can be repaired
/// from history.
///
/// The refreshed asset comes from a re-READ, not from the rebuild's
/// return: `rebuild_skeleton` answers a `DocumentSkeleton`, and the
/// caller needs the record whose `state` and `document_type` the
/// rebuild also moved.
async fn rebuild_skeleton(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    let manager = manager_for(&daemon)?;
    let skeleton = match manager.rebuild_skeleton(&id).await {
        Ok(s) => s,
        Err(e) => return Ok(internal_error("rebuild_skeleton", &e.to_string())),
    };
    Ok(match store.get_document_asset(&id).await {
        Ok(Some(document)) => {
            tracing::debug!(
                asset = %id,
                entities = skeleton.main_entities.len(),
                sections = skeleton.sections.len(),
                "documents_http: skeleton rebuilt"
            );
            Json(DocumentResponse { document }).into_response()
        }
        // The rebuild succeeded and the record is gone: a concurrent
        // delete. Reported, not papered over with the pre-rebuild copy.
        Ok(None) => internal_error(
            "rebuild_skeleton",
            &format!("asset '{id}' vanished between the rebuild and the read-back"),
        ),
        Err(e) => internal_error("get_document_asset", &e.to_string()),
    })
}

// ─── The upload job ────────────────────────────────────────────

/// One upload's frame log. Held per asset in [`DOCUMENT_JOBS`]; the
/// progress route reads it, the spawned ingest appends to it.
struct DocumentJob {
    frames: Mutex<Vec<serde_json::Value>>,
    finished: AtomicBool,
}

/// The live (and recently finished) upload jobs, keyed by asset id.
/// In-process: the record the frames narrate is in the store, so a
/// reader that missed the job reads the asset's `state` instead.
static DOCUMENT_JOBS: OnceLock<Mutex<HashMap<String, Arc<DocumentJob>>>> = OnceLock::new();

fn document_jobs() -> &'static Mutex<HashMap<String, Arc<DocumentJob>>> {
    DOCUMENT_JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

impl DocumentJob {
    /// Append one frame with `asset_id` stamped on. The milestone
    /// variants carry it already; `Indexing` / `BuildingSkeleton` do
    /// not, and a client keys live state by it (`or_insert` keeps the
    /// id the milestones carry).
    fn push(&self, asset_id: &str, mut frame: serde_json::Value) {
        if let serde_json::Value::Object(map) = &mut frame {
            map.entry("asset_id".to_string())
                .or_insert_with(|| serde_json::Value::String(asset_id.to_string()));
        }
        let terminal = matches!(
            frame.get("type").and_then(|t| t.as_str()),
            Some("Ready") | Some("Failed")
        );
        if let Ok(mut frames) = self.frames.lock() {
            frames.push(frame);
        }
        if terminal {
            self.finished.store(true, Ordering::SeqCst);
        }
    }

    /// Whether the manager already narrated the failure. A poisoned log
    /// answers `false` so the caller appends its own `Failed` frame — the
    /// arm that ends the job either way, never the one that leaves a
    /// poller spinning.
    fn last_is_failed(&self) -> bool {
        match self.frames.lock() {
            Ok(f) => f
                .last()
                .map(|v| v.get("type").and_then(|t| t.as_str()) == Some("Failed"))
                .unwrap_or(false),
            Err(_) => false,
        }
    }
}

/// POST `/v1/documents` — ingest a file as a document asset, as a JOB.
///
/// `prepare` runs inline (parse + chunk + persist the Pending record; no
/// inference) and its asset is the 202 body, because the id it mints is
/// the id every frame is stamped with — the banner a client shows and
/// the frames it polls agree on one id, which is the contract the
/// desktop command documented. `run_ingest` is spawned; its frames land
/// in the job log the progress route serves.
async fn upload_document(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(req): Json<UploadDocumentRequest>,
) -> Result<Response, Absence> {
    let manager = manager_for(&daemon)?;
    let path = std::path::PathBuf::from(&req.path);
    if !path.is_file() {
        return Err(Absence::invalid(format!(
            "upload_document: no such file: {}",
            req.path
        )));
    }
    let prepared = match manager.prepare(&path).await {
        Ok(p) => p,
        Err(e) => return Ok(internal_error("prepare", &e.to_string())),
    };
    let document = prepared.asset.clone();
    let asset_id = document.id.clone();
    let job = Arc::new(DocumentJob {
        frames: Mutex::new(Vec::new()),
        finished: AtomicBool::new(false),
    });
    match document_jobs().lock() {
        Ok(mut jobs) => {
            jobs.insert(asset_id.clone(), Arc::clone(&job));
        }
        Err(_) => {
            return Ok(internal_error(
                "upload_document",
                "the job table is poisoned",
            ))
        }
    }
    tracing::debug!(
        asset = %asset_id,
        filename = %document.filename,
        chunks = document.chunk_count,
        "documents_http: upload prepared — ingest job spawned"
    );

    let progress_job = Arc::clone(&job);
    let progress_id = asset_id.clone();
    let spawn_id = asset_id.clone();
    tokio::spawn(async move {
        let outcome = manager
            .run_ingest(prepared, move |progress| {
                let frame =
                    serde_json::to_value(&progress).unwrap_or_else(|_| serde_json::json!({}));
                progress_job.push(&progress_id, frame);
            })
            .await;
        match outcome {
            Ok(completed) => tracing::info!(
                asset = %spawn_id,
                filename = %completed.filename,
                chunks = completed.chunk_count,
                "documents_http: upload ingest complete"
            ),
            Err(e) => {
                tracing::warn!(asset = %spawn_id, "documents_http: upload ingest failed: {e}");
                // The manager emits `Failed` on its own error paths; a
                // failure it did not narrate still has to end the job,
                // or a poller spins forever on an asset that is not
                // coming (§18.3).
                if !job.last_is_failed() {
                    job.push(
                        &spawn_id,
                        serde_json::json!({ "type": "Failed", "reason": e.to_string() }),
                    );
                }
                job.finished.store(true, Ordering::SeqCst);
            }
        }
    });

    Ok((StatusCode::ACCEPTED, Json(DocumentResponse { document })).into_response())
}

/// GET `/v1/documents/{id}/progress?after=N` — the frames an upload job
/// appended from the caller's cursor on. 404 naming the asset when no
/// job is on record for it (a daemon restart, or an id this daemon never
/// prepared) — the record itself is still readable at `/v1/documents/{id}`.
async fn ingest_progress(
    _: LocalOnly,
    Path(id): Path<String>,
    Query(query): Query<DocumentProgressQuery>,
) -> Result<Response, Absence> {
    // A poisoned table and an absent job are different facts: the first
    // is a 500 naming it, the second the 404 below (§18.3).
    let job = match document_jobs().lock() {
        Ok(jobs) => jobs.get(&id).cloned(),
        Err(_) => {
            return Ok(internal_error(
                "ingest_progress",
                "the job table is poisoned",
            ))
        }
    };
    let Some(job) = job else {
        return Ok(json_error(
            StatusCode::NOT_FOUND,
            &format!("no ingest job on record for document asset '{id}'"),
        ));
    };
    let (frames, next) = match job.frames.lock() {
        Ok(all) => (
            all.get(query.after..).unwrap_or(&[]).to_vec(),
            all.len().max(query.after),
        ),
        Err(_) => {
            return Ok(internal_error(
                "ingest_progress",
                "the frame log is poisoned",
            ))
        }
    };
    let finished = job.finished.load(Ordering::SeqCst);
    tracing::debug!(
        asset = %id,
        after = query.after,
        served = frames.len(),
        finished,
        "documents_http: ingest progress served"
    );
    Ok(Json(DocumentIngestProgress {
        asset_id: id,
        frames,
        next,
        finished,
    })
    .into_response())
}

/// POST `/v1/documents/legacy` — the old paperclip path: chunk a file
/// into the legacy `documents` table, embedding each chunk when this
/// daemon serves a Runtime (the command passed its inference as an
/// `Option` for the same reason). Answers the `source` the legacy listing
/// will report it under.
async fn ingest_legacy(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(req): Json<IngestLegacyRequest>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    let path = std::path::PathBuf::from(&req.path);
    if !path.is_file() {
        return Err(Absence::invalid(format!(
            "ingest_legacy: no such file: {}",
            req.path
        )));
    }
    let inference = daemon.runtime().map(|r| Arc::clone(&r.inference));
    tracing::debug!(
        path = %path.display(),
        embeds = inference.is_some(),
        "documents_http: legacy ingest"
    );
    let chunks_created = match sovereign_tools::rag::ingest::ingest_file(
        &path,
        store.as_ref(),
        inference.as_deref(),
    )
    .await
    {
        Ok(n) => n,
        Err(e) => return Ok(internal_error("ingest_file", &e.to_string())),
    };
    let source = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&req.path)
        .to_string();
    tracing::info!(source = %source, chunks = chunks_created, "documents_http: legacy document ingested");
    Ok(Json(IngestLegacyResponse {
        source,
        chunks_created,
    })
    .into_response())
}

// ─── The ask job ───────────────────────────────────────────────

struct AskJob {
    asset_id: String,
    frames: Mutex<Vec<serde_json::Value>>,
    outcome: Mutex<Option<AskOutcome>>,
    finished: AtomicBool,
}

/// The live (and recently finished) ask jobs, keyed by job id.
static ASK_JOBS: OnceLock<Mutex<HashMap<String, Arc<AskJob>>>> = OnceLock::new();

fn ask_jobs() -> &'static Mutex<HashMap<String, Arc<AskJob>>> {
    ASK_JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

impl AskJob {
    fn push(&self, frame: serde_json::Value) {
        if let Ok(mut frames) = self.frames.lock() {
            frames.push(frame);
        }
    }

    /// The one way a job ends: the outcome is set, THEN `finished` flips,
    /// so a poller that sees `finished` always finds the outcome.
    fn end(&self, outcome: AskOutcome) {
        if let Ok(mut slot) = self.outcome.lock() {
            *slot = Some(outcome);
        }
        self.finished.store(true, Ordering::SeqCst);
    }
}

/// POST `/v1/documents/{id}/ask` — ask a question of a document asset,
/// as a JOB. The user message is persisted BEFORE the 202 (tagged with
/// the asset id, so the conversation records that this turn had a
/// document attached, and so the fall-through turn sees it); the
/// route/execute pair runs on the daemon's manager after.
///
/// 404 for an asset the store does not hold; 409 naming the state for
/// one that is not yet queryable — the command's `Err` for the same
/// case, kept apart from "no such asset".
async fn ask_document(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
    Json(req): Json<AskDocumentRequest>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    let manager = manager_for(&daemon)?;
    let runtime = daemon.runtime().map(Arc::clone).ok_or_else(|| {
        Absence::unavailable(
            "this daemon serves no turns (it was commissioned without a Runtime), so it \
             holds no inference provider to answer a document question with",
        )
    })?;
    let asset = match store.get_document_asset(&id).await {
        Ok(Some(a)) => a,
        Ok(None) => return Ok(not_found(&id)),
        Err(e) => return Ok(internal_error("get_document_asset", &e.to_string())),
    };
    if !asset.state.is_queryable() {
        return Ok(json_error(
            StatusCode::CONFLICT,
            &format!(
                "document asset '{id}' is not ready for queries (state: {})",
                asset.state.label()
            ),
        ));
    }

    let now = sovereign_core::time::unix_now();
    let user_msg = Message {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: req.conversation_id.clone(),
        role: Role::User,
        content: req.question.clone(),
        created_at: now,
        metadata: Some(serde_json::json!({ "attached_asset_id": id })),
        version: now,
    };
    if let Err(e) = store.save_message(&user_msg).await {
        return Ok(internal_error("save_message", &e.to_string()));
    }

    let job_id = format!("doc-ask-{}", uuid::Uuid::new_v4());
    let job = Arc::new(AskJob {
        asset_id: id.clone(),
        frames: Mutex::new(Vec::new()),
        outcome: Mutex::new(None),
        finished: AtomicBool::new(false),
    });
    match ask_jobs().lock() {
        Ok(mut jobs) => {
            jobs.insert(job_id.clone(), Arc::clone(&job));
        }
        Err(_) => return Ok(internal_error("ask_document", "the job table is poisoned")),
    }
    tracing::debug!(
        asset = %id,
        job_id = %job_id,
        conversation = %req.conversation_id,
        "documents_http: question persisted — ask job spawned"
    );

    let question = req.question;
    let conversation_id = req.conversation_id;
    let asset_id = id.clone();
    let spawn_job = job_id.clone();
    tokio::spawn(async move {
        let outcome = run_ask(
            &manager,
            Arc::clone(&store),
            &runtime,
            &asset,
            &question,
            &conversation_id,
            &job,
        )
        .await;
        tracing::info!(
            asset = %asset_id,
            job_id = %spawn_job,
            outcome = match &outcome {
                AskOutcome::Answered { operation, .. } => format!("answered:{}", operation.label()),
                AskOutcome::FellThrough { reason, .. } => format!("fell_through:{reason}"),
                AskOutcome::Failed { error } => format!("failed:{error}"),
            },
            "documents_http: ask job ended"
        );
        job.end(outcome);
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(AskJobAck {
            progress_route: format!("/v1/documents/{id}/ask/{job_id}"),
            asset_id: id,
            job_id,
        }),
    )
        .into_response())
}

/// The body of the ask job — the desktop command's document-operation
/// half, line for line: route, branch on `OffTopic`, execute with
/// progress, branch on empty RAG, persist the assistant message with the
/// `provenance` / `retrieved_chunks` shape the routing-meta bar reads,
/// record the operation, spawn auto-title. Every decision logs.
async fn run_ask(
    manager: &DocumentAssetManager,
    store: Arc<dyn StateStore>,
    runtime: &Arc<sovereign_core::runtime::Runtime>,
    asset: &DocumentAsset,
    question: &str,
    conversation_id: &str,
    job: &Arc<AskJob>,
) -> AskOutcome {
    let operation = match manager.route(asset, question).await {
        Ok(op) => op,
        Err(e) => {
            return AskOutcome::Failed {
                error: format!("Routing failed: {e}"),
            }
        }
    };
    tracing::info!(asset_id = %asset.id, operation = %operation.label(), "documents_http: ask routed");
    if matches!(operation, DocumentAssetOperation::OffTopic { .. }) {
        return AskOutcome::FellThrough {
            operation,
            reason: "the router judged the question off-topic for the document".to_string(),
        };
    }

    let start = std::time::Instant::now();
    let progress_job = Arc::clone(job);
    let output = match manager
        .execute_operation(asset, question, &operation, &move |progress| {
            let frame = serde_json::to_value(&progress).unwrap_or_else(|_| serde_json::json!({}));
            progress_job.push(frame);
        })
        .await
    {
        Ok(o) => o,
        Err(e) => {
            return AskOutcome::Failed {
                error: format!("Query failed: {e}"),
            }
        }
    };
    if matches!(operation, DocumentAssetOperation::Rag { .. })
        && output.citations.is_empty()
        && output.text.is_empty()
    {
        tracing::info!(asset_id = %asset.id, "documents_http: RAG found no relevant passages");
        return AskOutcome::FellThrough {
            operation,
            reason: "RAG found no relevant passages in the document".to_string(),
        };
    }

    let duration_ms = start.elapsed().as_millis() as u64;
    let assistant_message_id = uuid::Uuid::new_v4().to_string();
    let retrieved_chunks: Vec<serde_json::Value> = output
        .citations
        .iter()
        .map(|c| {
            serde_json::json!({
                "title": c.label,
                "corpus_id": asset.title,
                "url": serde_json::Value::Null,
                "snippet": c.snippet,
                "provenance_tier": "document",
            })
        })
        .collect();
    let provenance = ResponseProvenance {
        // A document-asset op is not a routed turn — no router decided it.
        router: None,
        intent: format!("DocumentAsk:{}", operation.label()),
        search_method: Some("document".to_string()),
        sources: vec![SourceSummary {
            origin: asset.title.clone(),
            count: output.citations.len(),
            from_peer: None,
            display_name: None,
        }],
        inference_backend: if output.model_id.is_empty() {
            "local".to_string()
        } else {
            output.model_id.clone()
        },
        oicp_match: None,
        total_latency_ms: duration_ms,
        tokens_used: output.tokens_used,
        coarse_intent: None,
        self_assessment: None,
        routing_trigger: None,
        coverage: None,
        finish_reason: output.finish_reason.clone(),
        // The budget the operation actually ran under — the serving
        // Runtime's, not a client's copy of a config.
        max_tokens_budget: Some(runtime.inference_config.max_tokens),
        completion_tokens: output.completion_tokens,
        context_window: None,
    };
    let sources: Vec<String> = output.citations.iter().map(|c| c.content.clone()).collect();
    let now = sovereign_core::time::unix_now();
    let assistant_msg = Message {
        id: assistant_message_id.clone(),
        conversation_id: conversation_id.to_string(),
        role: Role::Assistant,
        content: output.text.clone(),
        created_at: now,
        metadata: Some(serde_json::json!({
            "attached_asset_id": asset.id,
            "operation": operation,
            "sources": sources,
            "duration_ms": duration_ms,
            "provenance": provenance,
            "retrieved_chunks": retrieved_chunks,
        })),
        version: now,
    };
    if let Err(e) = store.save_message(&assistant_msg).await {
        return AskOutcome::Failed {
            error: format!("Failed to save assistant message: {e}"),
        };
    }
    // Analytics row; its absence changes no answer, and it says so.
    if let Err(e) = store
        .save_document_operation(&assistant_message_id, &asset.id, &operation, duration_ms)
        .await
    {
        tracing::warn!(asset_id = %asset.id, "documents_http: save_document_operation failed: {e}");
    }
    // Auto-title after the first exchange, in the background — the
    // command's spawn, on the daemon's provider and store.
    {
        let inference = Arc::clone(&runtime.inference);
        let store = Arc::clone(&store);
        let cid = conversation_id.to_string();
        tokio::spawn(async move {
            if let Err(e) =
                sovereign_core::title::try_auto_title(inference.as_ref(), store.as_ref(), &cid)
                    .await
            {
                tracing::warn!(
                    conversation_id = %cid,
                    "documents_http: auto-title failed (ask): {e}"
                );
            }
        });
    }
    AskOutcome::Answered {
        operation,
        message: assistant_msg,
        sources,
    }
}

/// GET `/v1/documents/{id}/ask/{job_id}?after=N` — the frames an ask job
/// appended from the caller's cursor on, and its outcome once finished.
/// 404 naming the job when none is on record for this asset.
async fn ask_progress(
    _: LocalOnly,
    Path((id, job_id)): Path<(String, String)>,
    Query(query): Query<DocumentProgressQuery>,
) -> Result<Response, Absence> {
    let job = match ask_jobs().lock() {
        Ok(jobs) => jobs.get(&job_id).cloned(),
        Err(_) => return Ok(internal_error("ask_progress", "the job table is poisoned")),
    };
    let job = match job {
        Some(j) if j.asset_id == id => j,
        _ => {
            return Ok(json_error(
                StatusCode::NOT_FOUND,
                &format!("no ask job '{job_id}' on record for document asset '{id}'"),
            ))
        }
    };
    let (frames, next) = match job.frames.lock() {
        Ok(all) => (
            all.get(query.after..).unwrap_or(&[]).to_vec(),
            all.len().max(query.after),
        ),
        Err(_) => return Ok(internal_error("ask_progress", "the frame log is poisoned")),
    };
    let finished = job.finished.load(Ordering::SeqCst);
    let outcome = if finished {
        match job.outcome.lock() {
            Ok(o) => o.clone(),
            Err(_) => {
                return Ok(internal_error(
                    "ask_progress",
                    "the outcome slot is poisoned",
                ))
            }
        }
    } else {
        None
    };
    tracing::debug!(
        asset = %id,
        job_id = %job_id,
        after = query.after,
        served = frames.len(),
        finished,
        "documents_http: ask progress served"
    );
    Ok(Json(AskProgress {
        asset_id: id,
        job_id,
        frames,
        next,
        finished,
        outcome,
    })
    .into_response())
}

/// GET `/v1/documents/legacy` — documents in the old `documents` table
/// that no `DocumentAsset` owns.
///
/// The fold, moved down whole (see the header). Three skips and a word
/// count; a `corpus:` source is corpus content, not an upload, and an
/// `asset:` source an asset already claims is that asset's own chunks.
async fn list_legacy_documents(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    let sources = match store.list_sources().await {
        Ok(s) => s,
        Err(e) => return Ok(internal_error("list_sources", &e.to_string())),
    };
    // An asset listing that FAILS is not "no assets": every
    // asset-owned source would then be reported as a promotable
    // legacy document, and promoting one would mint a duplicate
    // record. It is an error (§18.3) — the desktop's
    // `unwrap_or_default()` here was the swallow.
    let assets = match store.list_document_assets().await {
        Ok(a) => a,
        Err(e) => return Ok(internal_error("list_document_assets", &e.to_string())),
    };
    let asset_sources: std::collections::HashSet<String> =
        assets.iter().map(|a| format!("asset:{}", a.id)).collect();

    let mut documents = Vec::new();
    for source in &sources {
        if source.starts_with("asset:") && asset_sources.contains(source) {
            continue;
        }
        if source.starts_with("corpus:") {
            continue;
        }
        let chunks = match store.get_chunks_by_source(source).await {
            Ok(c) => c,
            Err(e) => return Ok(internal_error("get_chunks_by_source", &e.to_string())),
        };
        if chunks.is_empty() {
            continue;
        }
        documents.push(LegacyDocumentEntry {
            source: source.clone(),
            filename: filename_of(source),
            chunk_count: chunks.len(),
            word_count: word_count_of(&chunks),
        });
    }
    tracing::debug!(
        sources = sources.len(),
        legacy = documents.len(),
        "documents_http: legacy documents listed"
    );
    Ok(Json(LegacyDocumentListResponse { documents }).into_response())
}

/// POST `/v1/documents/legacy/promote` — mint a `DocumentAsset` over
/// chunks that are already in the store.
///
/// No re-upload and no re-embedding: the chunks exist, this gives them
/// a record. `skeleton` stays `None` and `state` is `PartiallyReady`,
/// which is honest — the structural pass has not run, and
/// `POST /{id}/skeleton` is what runs it.
async fn promote_legacy(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(req): Json<PromoteLegacyRequest>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    let chunks = match store.get_chunks_by_source(&req.source).await {
        Ok(c) => c,
        Err(e) => return Ok(internal_error("get_chunks_by_source", &e.to_string())),
    };
    if chunks.is_empty() {
        return Ok(json_error(
            StatusCode::NOT_FOUND,
            &format!("no chunks are stored for source '{}'", req.source),
        ));
    }
    let filename = filename_of(&req.source);
    let document = DocumentAsset {
        id: uuid::Uuid::new_v4().to_string(),
        title: title_of(&filename),
        filename,
        // Unknown for a legacy document: the original file is gone and
        // the chunks do not record its size. `0.0` is the store's own
        // "not recorded", carried over from the command verbatim.
        file_size_mb: 0.0,
        word_count: word_count_of(&chunks),
        chunk_count: chunks.len(),
        document_type: sovereign_core::types::DocumentTypeTag::Unknown,
        ingested_at: chrono::Utc::now(),
        index_id: format!("legacy:{}", req.source),
        skeleton: None,
        state: sovereign_core::types::AssetState::PartiallyReady,
        owner: None,
    };
    if let Err(e) = store.save_document_asset(&document).await {
        return Ok(internal_error("save_document_asset", &e.to_string()));
    }
    tracing::debug!(
        source = %req.source,
        asset = %document.id,
        chunks = document.chunk_count,
        "documents_http: legacy document promoted"
    );
    Ok(Json(DocumentResponse { document }).into_response())
}

// ─── Helpers ───────────────────────────────────────────────────

/// Last path segment of a chunk `source`. One implementation, called
/// by the listing and by the promotion, so the row a caller picks and
/// the record it promotes cannot carry different names (§10.6).
fn filename_of(source: &str) -> String {
    source.rsplit('/').next().unwrap_or(source).to_string()
}

/// The display title derived from a filename: extension stripped,
/// underscores and hyphens read as spaces.
fn title_of(filename: &str) -> String {
    filename
        .rsplit_once('.')
        .map(|(name, _)| name)
        .unwrap_or(filename)
        .replace(['_', '-'], " ")
}

fn word_count_of(chunks: &[sovereign_core::types::DocumentChunk]) -> usize {
    chunks
        .iter()
        .map(|c| c.content.split_whitespace().count())
        .sum()
}

/// The daemon's own `StateStore`. One lookup site, so no handler can
/// read a different store than the one a turn writes to.
fn store_for(daemon: &Arc<EmbeddedDaemon>) -> Result<Arc<dyn StateStore>, Absence> {
    daemon.state_store().map(Arc::clone).ok_or_else(|| {
        Absence::unavailable(
            "this daemon holds no StateStore (it was commissioned to serve nothing)",
        )
    })
}

/// A `DocumentAssetManager` over the daemon's store and the provider
/// the serving `Runtime` answers turns with.
///
/// `runtime.inference` rather than `ServingCore.inference_provider`
/// deliberately: a document operation is inference the user attributes
/// to "the model I am talking to", and reading the runtime's handle
/// makes that structurally the same object (§10.6). It also means the
/// 503 here names the `Runtime`, which is the object whose absence
/// actually stops the operation.
///
/// The NER extractor comes from the SAME `Runtime`, for the same reason
/// and by the same rule (ARCH principle 8): `runtime.lane()` is the one
/// snapshot a turn's stages read, so a document operation and a chat turn
/// on this daemon cannot disagree about whether an entity model is
/// resident. Reaching for `sovereign_gliner` here instead would be a
/// second decider — and would put an ONNX dependency on the wire layer.
///
/// `None` is a real answer, not a failure: the model is not installed, and
/// `build_skeleton` takes its documented LLM fallback. Both branches say
/// which on the trace, because "ran the cheap NER path" and "spent 66% of
/// the ingest's prompt tokens on a 4B" are the same response shape
/// (ARCH principle 1).
fn manager_for(daemon: &Arc<EmbeddedDaemon>) -> Result<DocumentAssetManager, Absence> {
    let store = store_for(daemon)?;
    let runtime = daemon.runtime().ok_or_else(|| {
        Absence::unavailable(
            "this daemon serves no turns (it was commissioned without a Runtime), so it \
             holds no inference provider to run a document operation with",
        )
    })?;
    let inference: Arc<dyn InferenceProvider> = Arc::clone(&runtime.inference);
    let manager = DocumentAssetManager::new(inference, store);
    match runtime.lane().gliner {
        Some(ner) => {
            tracing::debug!(
                entity_path = "ner",
                "documents_http: T2 entity pass runs on this daemon's resident NER model"
            );
            Ok(manager.with_entity_extractor(ner))
        }
        None => {
            tracing::debug!(
                entity_path = "llm",
                "documents_http: no NER model on this daemon's lane — the T2 entity pass \
                 falls back to the generative model, at ~66% of the ingest's prompt tokens"
            );
            Ok(manager)
        }
    }
}

fn not_found(id: &str) -> Response {
    json_error(
        StatusCode::NOT_FOUND,
        &format!("no document asset '{id}' is stored on this daemon"),
    )
}

fn internal_error(op: &str, detail: &str) -> Response {
    tracing::warn!(
        operation = op,
        detail,
        "documents_http: store operation failed"
    );
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        &format!("{op}: {detail}"),
    )
}

/// Keeps the two DTOs this file mints named in its own surface, the
/// way `lc_http` does: a reader looking for "what shape comes back"
/// finds it without leaving the module.
#[allow(dead_code)]
fn _answers(e: LegacyDocumentEntry, d: DocumentAsset) -> (LegacyDocumentEntry, DocumentAsset) {
    (e, d)
}

#[cfg(test)]
mod tests {
    /// Every manager this module builds is offered the lane's NER model.
    ///
    /// A census over this file's own source rather than a behavioural
    /// assertion, and the limit is worth naming: `DocumentAssetManager`
    /// keeps `entity_extractor` `pub(super)` to `sovereign-tools`, so from
    /// this crate there is no value to assert on and no accessor to ask.
    /// What the census CAN do is refuse the regression that actually
    /// happened — a `::new()` whose result is returned without the lane
    /// ever being consulted, which is how this surface shipped for months
    /// with a resident NER model it never offered.
    ///
    /// Watched red on 2026-09-11 by reverting the two lines in
    /// [`super::manager_for`] that read the lane's `gliner` field.
    ///
    /// **Every needle is assembled at run time, and that is load-bearing.**
    /// The first draft spelled them as literals and went red on its own
    /// text — the file contained two `…::new(` occurrences, one of them the
    /// test's. Worse than the red: the two `contains` asserts would have
    /// been satisfied by the assertion MESSAGES alone, so removing the
    /// wiring entirely would have left this green. A census that reads the
    /// file it lives in has to be unable to author its own evidence
    /// (ARCH principle 5).
    ///
    /// The structural fix that would retire this: one
    /// `DocumentAssetManager::from_runtime(&Runtime, store)` in
    /// `sovereign-tools`, so the four construction sites across this
    /// workspace stop each deciding for themselves (ARCH principle 8).
    /// That crate is out of this change's scope.
    #[test]
    fn no_manager_is_built_without_offering_the_lanes_ner_model() {
        let src = include_str!("documents_http.rs");
        let needle = |a: &str, b: &str| format!("{a}{b}");
        let construction = needle("DocumentAsset", "Manager::new(");
        let reads_lane = needle("runtime.lane()", ".gliner");
        let hands_it_over = needle(".with_entity", "_extractor(");

        let count = src.matches(&construction).count();
        assert_eq!(
            count, 1,
            "this module now builds {count} managers. The two asserts below only prove \
             the lane is consulted SOMEWHERE in the file, so a second construction site \
             could ignore it while they stay green — re-read them before raising this"
        );
        assert!(
            src.contains(&reads_lane),
            "the only `DocumentAssetManager` this module builds no longer reads the \
             Runtime lane's NER model. The daemon HAS one — runtime-recipe fills \
             `LaneSources::gliner` for every host it commissions — so dropping this \
             wiring does not disable the entity pass, it silently moves it back onto \
             the generative model at ~66% of the ingest's prompt tokens, with nothing red"
        );
        assert!(
            src.contains(&hands_it_over),
            "the lane's NER model is read and then never handed to the manager"
        );
    }
}
