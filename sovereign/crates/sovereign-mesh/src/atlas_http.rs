// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atlas-browse HTTP — `/internal/atlas/…` (sv-surface D4).
//!
//! The wire form of the desktop's Atlas Inspector, over the daemon's own
//! `engine.index_dir()`: six routes for the `corpus_engine` commands
//! (corpora, report, members, atoms, subgraph, atom detail) and six for the
//! conversation-tiered ones. Each calls the SAME `sovereign_tools::atlas_view`
//! method the Tauri command called and returns the SAME type, so the desktop
//! rung is a repoint. `list_atoms` is a POST because `AtomFilter` carries a
//! `Vec<String>`: a read, asked with a body.
//!
//! Loopback posture is `reading_http`'s, unchanged.
//!
//! NOT crossed, and NOT "app-local by decision" — which is what this line
//! claimed until 2026-09-11. The two GLiNER model commands
//! (`atlas_commands.rs:282`, `:303`) write a HOST-GLOBAL directory:
//! `gliner_ner::models_root()` is `$SOVEREIGN_GLINER_MODEL_DIR` or
//! `~/.svrnmesh/models/gliner` (`gliner_ner.rs:166`), and the daemon loads
//! the same model from the same root (`daemon_cmd/mod.rs:752`). One file,
//! two consumers — so on a desktop attached to a REMOTE daemon the download
//! lands on the wrong machine. The commands stay here unmoved; what is
//! corrected is the reason, because "by decision" is what stopped anyone
//! looking.

use std::sync::Arc;

use axum::extract::{Extension, Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use sovereign_core::conv_tiered::{ConvRaptorNodeRow, ConvTieredReader};
use sovereign_tools::atlas_view::{
    AtlasViewError, AtomFilter, AtomQueryError, ConvCorpusSummary, ConvDetailView, ConvEntityChip,
    ConvListPage, ConvRaptorNodeView, ConvSummary, FileAtlasReader, PageCursor,
    SummaryCorrectionView, DEFAULT_MAX_NODES,
};

use crate::daemon::EmbeddedDaemon;
use crate::http_response::{
    internal_error, not_found, not_implemented, service_unavailable, Absence,
};
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

// ─── Request shapes ────────────────────────────────────────────

/// Body of `POST /internal/atlas/{corpus}/atoms` — the two arguments
/// `atlas_list_atoms` takes, with the same `Option` semantics
/// (`None` = the type's `Default`).
#[derive(Debug, Default, Deserialize)]
pub struct AtomBrowseRequest {
    #[serde(default)]
    pub filter: Option<AtomFilter>,
    #[serde(default)]
    pub page: Option<PageCursor>,
}

#[derive(Debug, Deserialize)]
pub struct SubgraphQuery {
    /// Node cap. Absent = `atlas_view::DEFAULT_MAX_NODES`, which is
    /// exactly what `atlas_subgraph` passes when the desktop omits it
    /// — one decider for the cap, still in `sovereign-tools`.
    #[serde(default)]
    pub max_nodes: Option<usize>,
}

// ─── Router ────────────────────────────────────────────────────

/// The atlas-browse router. Mounted unconditionally on every serving
/// daemon's client router beside `reading_http`; a daemon with no
/// corpus engine answers 503 with that named reason rather than 404,
/// so "not built" and "not mounted" stay different facts.
pub fn atlas_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/internal/atlas/corpora", get(list_corpora))
        .route("/internal/atlas/{corpus}/report", get(build_report))
        .route("/internal/atlas/{corpus}/members", get(list_members))
        .route("/internal/atlas/{corpus}/atoms", post(list_atoms))
        .route("/internal/atlas/{corpus}/subgraph", get(subgraph))
        .route("/internal/atlas/{corpus}/atoms/{atom_id}", get(atom_detail))
        .route("/internal/atlas/conv/corpora", get(conv_list_corpora))
        .route(
            "/internal/atlas/conv/{corpus}/conversations",
            get(conv_list_conversations),
        )
        .route(
            "/internal/atlas/conv/{corpus}/conversations/{conv_uuid}",
            get(conv_detail),
        )
        .route(
            "/internal/atlas/conv/{corpus}/conversations/{conv_uuid}/entities",
            get(conv_entities),
        )
        .route(
            "/internal/atlas/conv/{corpus}/entities/aggregate",
            get(conv_entity_aggregate),
        )
        .route(
            "/internal/atlas/conv/{corpus}/chunk-entity-progress",
            get(conv_chunk_entity_progress),
        )
        .localhost_only_with(daemon)
}

// ─── Handlers ──────────────────────────────────────────────────

/// GET /internal/atlas/corpora — every installed corpus that has an
/// atlas, with per-atom-type counts. Wire form of
/// `atlas_list_corpora`; answers `Vec<AtlasCorpusSummary>`.
async fn list_corpora(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Result<Response, Absence> {
    let reader = reader_for(&daemon)?;
    Ok(match reader.list_corpora().await {
        Ok(rows) => {
            tracing::debug!(corpora = rows.len(), "atlas_http: corpora listed");
            (StatusCode::OK, Json(rows)).into_response()
        }
        Err(e) => view_error(&e),
    })
}

/// GET /internal/atlas/{corpus}/report — what the last build found.
/// Wire form of `atlas_build_report`; answers `AtlasBuildReport`.
///
/// A corpus whose report step never ran comes back `reported: false`
/// — a successful answer, not an error. That distinction is the
/// reader's, kept here unchanged.
async fn build_report(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
) -> Result<Response, Absence> {
    let reader = reader_for(&daemon)?;
    Ok(match reader.build_report(&corpus).await {
        Ok(report) => (StatusCode::OK, Json(report)).into_response(),
        Err(e) => view_error(&e),
    })
}

/// GET /internal/atlas/{corpus}/members — the member atlases of a
/// collection corpus. Wire form of `atlas_list_members`; answers
/// `Vec<AtlasMemberSummary>`.
///
/// An EMPTY list is the correct answer for every ordinary corpus, and
/// the frontend branches on it to pick which Explore surface to
/// render — so this must never become a 404.
async fn list_members(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
) -> Result<Response, Absence> {
    let reader = reader_for(&daemon)?;
    Ok(match reader.list_members(&corpus).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(e) => view_error(&e),
    })
}

/// POST /internal/atlas/{corpus}/atoms — filterable, paginated atom
/// browse. Wire form of `atlas_list_atoms`; answers `AtomListPage`.
async fn list_atoms(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
    body: Option<Json<AtomBrowseRequest>>,
) -> Result<Response, Absence> {
    let reader = reader_for(&daemon)?;
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let filter = req.filter.unwrap_or_default();
    let page = req.page.unwrap_or_default();
    Ok(match reader.list_atoms(&corpus, filter, page).await {
        Ok(page) => {
            tracing::debug!(
                corpus = %corpus,
                returned = page.items.len(),
                total_matching = page.total_matching,
                next_offset = ?page.next_offset,
                "atlas_http: atom page served",
            );
            (StatusCode::OK, Json(page)).into_response()
        }
        Err(e) => atom_error(&e),
    })
}

/// GET /internal/atlas/{corpus}/subgraph?max_nodes= — the curated
/// landscape map. Wire form of `atlas_subgraph`; answers
/// `AtlasSubgraph`.
async fn subgraph(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
    Query(SubgraphQuery { max_nodes }): Query<SubgraphQuery>,
) -> Result<Response, Absence> {
    let reader = reader_for(&daemon)?;
    Ok(
        match reader
            .subgraph(&corpus, max_nodes.unwrap_or(DEFAULT_MAX_NODES))
            .await
        {
            Ok(graph) => (StatusCode::OK, Json(graph)).into_response(),
            Err(e) => atom_error(&e),
        },
    )
}

/// GET /internal/atlas/{corpus}/atoms/{atom_id} — the full inspector
/// record. Wire form of `atlas_get_atom_detail`; answers `AtomDetail`,
/// or 404 when the atom is not in this corpus's atoms.json (a stale UI
/// link, or extraction renumbered ids since the last browse).
///
/// Carries the desktop command's second half too: evidence excerpts
/// carry a `section_id`, and the reading surface needs a numeric
/// `chunk_id` to deep-link to. Building that map is a full
/// `chunks.lance` scan (2.8 GB / ~90 s on Wikipedia), so it is NEVER
/// built on the click path — resolved from the per-corpus cache when
/// ready, and otherwise left `None` (the row renders non-clickable)
/// while a ONE-TIME background build fills the cache for later
/// clicks. Same policy, same cache key, now one copy for every
/// surface instead of one per surface.
async fn atom_detail(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path((corpus, atom_id)): Path<(String, String)>,
) -> impl IntoResponse {
    let engine = match daemon.corpus_engine() {
        Some(e) => Arc::clone(e),
        None => return service_unavailable("corpus engine not initialised"),
    };
    let reader = FileAtlasReader::new(engine.index_dir().to_path_buf());
    let mut detail = match reader.get_atom_detail(&corpus, &atom_id).await {
        Ok(Some(d)) => d,
        Ok(None) => return not_found("atom not found"),
        Err(e) => return atom_error(&e),
    };

    let needs_sections = detail
        .evidence_excerpts
        .iter()
        .any(|e| !e.section_id.is_empty());
    if needs_sections {
        if let Some(map) = section_map::resolve_or_build(&engine, &corpus) {
            for excerpt in &mut detail.evidence_excerpts {
                excerpt.chunk_id = map.get(&excerpt.section_id).copied();
            }
        }
    }
    (StatusCode::OK, Json(detail)).into_response()
}

// ─── section_id → chunk_id cache ───────────────────────────────

mod section_map {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};

    use corpus_engine::CorpusEngine;

    /// Per-corpus state. `Building` is a marker, not a value: it stops
    /// a second click from launching a second full-index scan while
    /// the first is still running.
    enum State {
        Building,
        Ready(Arc<HashMap<String, u64>>),
    }

    fn cache() -> &'static Mutex<HashMap<String, State>> {
        static CACHE: OnceLock<Mutex<HashMap<String, State>>> = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Take the cache lock, tolerating poison.
    ///
    /// The desktop's copy of this cache called `.unwrap()` here, which
    /// makes ONE panicking handler wedge atom-detail resolution for the
    /// life of the process. Nothing under this lock can leave the map
    /// in a state a later reader misreads — the values are whole maps,
    /// inserted or removed atomically — so recovering the guard is
    /// strictly better than propagating a panic the caller cannot act
    /// on.
    fn lock_cache() -> std::sync::MutexGuard<'static, HashMap<String, State>> {
        cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The cached map when it is ready; otherwise `None`, having kicked
    /// off a one-time background build. Never blocks the caller on the
    /// scan — that is the whole policy.
    pub(super) fn resolve_or_build(
        engine: &Arc<CorpusEngine>,
        corpus_id: &str,
    ) -> Option<Arc<HashMap<String, u64>>> {
        let mut cache = lock_cache();
        if let Some(State::Ready(map)) = cache.get(corpus_id) {
            return Some(Arc::clone(map));
        }
        if cache.contains_key(corpus_id) {
            // `Building` — a scan is already running for this corpus.
            return None;
        }
        cache.insert(corpus_id.to_string(), State::Building);
        drop(cache);

        let engine = Arc::clone(engine);
        let corpus = corpus_id.to_string();
        tokio::spawn(async move {
            let built = match engine.open_index_for_corpus(&corpus).await {
                Ok(index) => index.section_chunk_index().await.ok(),
                Err(_) => None,
            };
            let mut cache = lock_cache();
            match built {
                Some(map) => {
                    tracing::info!(
                        corpus_id = %corpus,
                        sections = map.len(),
                        "atlas_http: section→chunk map built + cached (background)",
                    );
                    cache.insert(corpus, State::Ready(Arc::new(map)));
                }
                None => {
                    // Drop the marker so a later click retries instead of
                    // wedging on `Building` forever.
                    cache.remove(&corpus);
                }
            }
        });
        None
    }
}

// ─── Helpers ───────────────────────────────────────────────────

/// The reader over the DAEMON's indexes dir — one construction site,
/// so no handler can accidentally point at a different root.
// ─── D4 remainder: the conversation-tiered browse routes ───────
//
// Six routes for the six `atlas_*` conv commands, over the reader
// `207b62b85` widened. `ConvBrowseReader` became a supertrait of
// `ConvTieredReader`, so the ONE handle the daemon already holds —
// `runtime.lane_sources.conv_tiered` — now reaches all seven store
// calls these commands make. No second accessor was minted (§10.6);
// no trait object was added to `ServingCore`.
//
// Absence has three distinct answers here and none of them is an
// empty page (§18.3):
//
//   no `Runtime`, or `conv_tiered` is `None`  -> 503 naming which
//   `Err(NotImplemented)` from the reader     -> 501 naming the method
//   the conversation is not in the store      -> 404 naming the id
//
// Two silent substitutions the desktop makes do NOT cross:
// `atlas_list_conversations` folds a failed `list_conv_raptor_nodes`
// into `unwrap_or_default()` (an empty chip row reads as "this
// conversation has no entities"), and `atlas_get_conv_detail` folds a
// failed `get_active_correction` into `.ok().flatten()` (a missing
// "revised by you" badge reads as "never revised"). Both are reported
// here. That is a deliberate behaviour delta on the repoint, not an
// accident, and it is the §18.3 rule the campaign exists to enforce.

/// The page size `atlas_list_conversations` hard-codes today. It is
/// not caller-tunable on the wire either — one decider, and the pane
/// has never asked for a second.
const CONV_PAGE_LIMIT: u64 = 200;
/// `summarize_entities(&raptor, 6)` — the list row's chip budget.
const CONV_LIST_TOP_ENTITIES: usize = 6;
/// `rank_entity_chips(&nodes, 12)` — the detail pane's chip row.
const CONV_CHIP_TOP_N: usize = 12;
/// `aggregate_entity(.., 20, 10)` — co-occurring cap, then conv cap.
const ENTITY_CO_LIMIT: usize = 20;
const ENTITY_CONV_LIMIT: usize = 10;

/// Query of `GET /internal/atlas/conv/{corpus}/conversations`.
#[derive(Debug, Default, Deserialize)]
pub struct ConvListQuery {
    /// Substring match on `overview`. Blank or whitespace-only is
    /// `None`, exactly as the command trims it.
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub offset: Option<u64>,
}

/// Query of `GET /internal/atlas/conv/{corpus}/entities/aggregate`.
#[derive(Debug, Deserialize)]
pub struct EntityAggregateQuery {
    pub text: String,
}

/// GET /internal/atlas/conv/corpora — every corpus with at least one
/// `conv_skeletons` row, with its state buckets and its display
/// metadata. Wire form of `atlas_list_conv_corpora`; answers
/// `Vec<ConvCorpusSummary>`.
///
/// The display half is best-effort in exactly the desktop's sense: a
/// corpus the engine does not know falls back to its own id. That is
/// not a substitution — `display_name` has no other truth to report —
/// and a corpus engine that will not answer at all leaves every row
/// on that fallback rather than failing the list.
async fn conv_list_corpora(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Result<Response, Absence> {
    let reader = conv_reader_for(&daemon)?;
    let buckets = match reader.list_conv_corpora_with_state_buckets().await {
        Ok(b) => b,
        Err(e) => return Ok(conv_error("list_conv_corpora_with_state_buckets", &e)),
    };
    let display = conv_display_index(&daemon).await;
    let mut out = Vec::with_capacity(buckets.len());
    for (corpus_id, total, max_ts, per_state) in buckets {
        let (display_name, display_category, display_icon) = display
            .get(&corpus_id)
            .cloned()
            .unwrap_or_else(|| (corpus_id.clone(), None, None));
        let state_counts: std::collections::BTreeMap<String, u64> = per_state.into_iter().collect();
        out.push(ConvCorpusSummary {
            corpus_id,
            display_name,
            conv_count: total,
            state_counts,
            last_updated_unix: if max_ts > 0 { Some(max_ts) } else { None },
            display_category,
            display_icon,
        });
    }
    tracing::debug!(corpora = out.len(), "atlas_http: conv corpora listed");
    Ok((StatusCode::OK, Json(out)).into_response())
}

/// GET /internal/atlas/conv/{corpus}/conversations?filter=&offset= —
/// one page of conversations, newest first. Wire form of
/// `atlas_list_conversations`; answers `ConvListPage`.
async fn conv_list_conversations(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
    Query(q): Query<ConvListQuery>,
) -> Result<Response, Absence> {
    let reader = conv_reader_for(&daemon)?;
    let offset = q.offset.unwrap_or(0);
    let filter = q.filter.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let (rows, total) = match reader
        .list_conversations_paginated(&corpus, filter, offset, CONV_PAGE_LIMIT)
        .await
    {
        Ok(page) => page,
        Err(e) => return Ok(conv_error("list_conversations_paginated", &e)),
    };
    let mut conversations = Vec::with_capacity(rows.len());
    for row in rows {
        // The desktop's `unwrap_or_default()` here is the substitution
        // named in the section header — a read failure would render as
        // "no entities". Reported instead.
        let nodes = match reader.list_conv_raptor_nodes(&corpus, &row.conv_uuid).await {
            Ok(n) => n,
            Err(e) => return Ok(conv_error("list_conv_raptor_nodes", &e)),
        };
        let (top_entities, is_tiny) = summarize_entities(&nodes, CONV_LIST_TOP_ENTITIES);
        conversations.push(ConvSummary {
            conv_uuid: row.conv_uuid,
            title: row
                .overview
                .clone()
                .unwrap_or_else(|| "(untitled conversation)".to_string()),
            state: row.state,
            chunk_count: row.chunk_count,
            top_entities,
            updated_at: row.updated_at,
            is_tiny,
        });
    }
    let seen = offset + conversations.len() as u64;
    let next_offset = if seen < total { Some(seen) } else { None };
    Ok((
        StatusCode::OK,
        Json(ConvListPage {
            conversations,
            total_matching: total,
            next_offset,
        }),
    )
        .into_response())
}

/// GET /internal/atlas/conv/{corpus}/conversations/{conv_uuid} — the
/// RAPTOR tree and the active correction for one conversation. Wire
/// form of `atlas_get_conv_detail`; answers `ConvDetailView`.
///
/// The command answers `Ok(None)` for an absent conversation. On the
/// wire that is a 404 that NAMES the id, not a 200 carrying `null`:
/// the pane must be able to tell "no such conversation" from "the
/// daemon has no reader".
async fn conv_detail(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path((corpus, conv_uuid)): Path<(String, String)>,
) -> Result<Response, Absence> {
    let reader = conv_reader_for(&daemon)?;
    let skeleton = match reader.get_conv_skeleton(&corpus, &conv_uuid).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Ok(not_found(&format!(
                "conversation '{conv_uuid}' is not in corpus '{corpus}'"
            )))
        }
        Err(e) => return Ok(conv_error("get_conv_skeleton", &e)),
    };
    let nodes = match reader.list_conv_raptor_nodes(&corpus, &conv_uuid).await {
        Ok(n) => n,
        Err(e) => return Ok(conv_error("list_conv_raptor_nodes", &e)),
    };
    let max_level = nodes.iter().map(|n| n.level as u8).max().unwrap_or(0);
    let raptor_nodes: Vec<ConvRaptorNodeView> = nodes.into_iter().map(raptor_node_view).collect();
    // The desktop's `.ok().flatten()` here is the second substitution
    // named in the section header: a failed read renders as "never
    // revised". Reported instead.
    let correction = match reader.get_active_correction(&corpus, &conv_uuid).await {
        Ok(c) => c.map(|c| SummaryCorrectionView {
            status: c.status,
            correction_hint: c.correction_hint,
            created_at: c.created_at,
        }),
        Err(e) => return Ok(conv_error("get_active_correction", &e)),
    };
    Ok((
        StatusCode::OK,
        Json(ConvDetailView {
            corpus_id: corpus,
            conv_uuid,
            title: skeleton
                .overview
                .clone()
                .unwrap_or_else(|| "(untitled conversation)".to_string()),
            state: skeleton.state,
            chunk_count: skeleton.chunk_count,
            updated_at: skeleton.updated_at,
            raptor_nodes,
            max_level,
            correction,
        }),
    )
        .into_response())
}

/// GET /internal/atlas/conv/{corpus}/conversations/{conv_uuid}/entities
/// — the salience-ranked chip row. Wire form of
/// `atlas_get_conv_entities`; answers `Vec<ConvEntityChip>`.
async fn conv_entities(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path((corpus, conv_uuid)): Path<(String, String)>,
) -> Result<Response, Absence> {
    let reader = conv_reader_for(&daemon)?;
    Ok(
        match reader.list_conv_raptor_nodes(&corpus, &conv_uuid).await {
            Ok(nodes) => (
                StatusCode::OK,
                Json(rank_entity_chips(&nodes, CONV_CHIP_TOP_N)),
            )
                .into_response(),
            Err(e) => conv_error("list_conv_raptor_nodes", &e),
        },
    )
}

/// GET /internal/atlas/conv/{corpus}/entities/aggregate?text= — one
/// entity's roll-up across the corpus. Wire form of
/// `atlas_get_entity_aggregate`; answers `EntityAggregateRow`.
async fn conv_entity_aggregate(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
    Query(q): Query<EntityAggregateQuery>,
) -> Result<Response, Absence> {
    let reader = conv_reader_for(&daemon)?;
    Ok(
        match reader
            .aggregate_entity(&corpus, &q.text, ENTITY_CO_LIMIT, ENTITY_CONV_LIMIT)
            .await
        {
            Ok(row) => (StatusCode::OK, Json(row)).into_response(),
            Err(e) => conv_error("aggregate_entity", &e),
        },
    )
}

/// GET /internal/atlas/conv/{corpus}/chunk-entity-progress — how far
/// chunk-level entity extraction has got. Wire form of
/// `atlas_get_chunk_entity_progress`; answers
/// `Option<ChunkEntityProgressRow>`.
///
/// An explicit `null` is the answer when extraction never ran, and it
/// ships as a body rather than as a 404: "never extracted" is a fact
/// about the corpus, not an absent resource, and the two must not
/// arrive as the same status (§18.3).
async fn conv_chunk_entity_progress(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
) -> Result<Response, Absence> {
    let reader = conv_reader_for(&daemon)?;
    Ok(match reader.get_chunk_entity_progress(&corpus).await {
        Ok(row) => (StatusCode::OK, Json(row)).into_response(),
        Err(e) => conv_error("get_chunk_entity_progress", &e),
    })
}

// ─── conv helpers ──────────────────────────────────────────────

/// The one handle, reached the one way (`§10.6`).
fn conv_reader_for(daemon: &Arc<EmbeddedDaemon>) -> Result<Arc<dyn ConvTieredReader>, Absence> {
    let runtime = daemon.runtime().ok_or_else(|| {
        Absence::unavailable(
            "this daemon assembled no Runtime, so it wired no conversation-tiered reader",
        )
    })?;
    match runtime.lane_sources.conv_tiered.as_ref() {
        Some(r) => Ok(Arc::clone(r)),
        None => Err(Absence::unavailable(
            "this daemon wired no conversation-tiered reader \
             (runtime.lane_sources.conv_tiered is None)",
        )),
    }
}

/// A reader that declines a method by name is a 501, not a 500 and
/// not an empty page: the store is reachable and this operation is
/// the part it does not carry (`ConvBrowseReader::browse_unsupported`).
fn conv_error(what: &str, e: &sovereign_core::error::Error) -> axum::response::Response {
    match e {
        sovereign_core::error::Error::NotImplemented(msg) => {
            not_implemented(format!("{what}: {msg}"))
        }
        other => internal_error(&format!("{what}: {other}")),
    }
}

/// corpus_id -> (display_name, category, icon) for every installed
/// index, built once per list rather than once per row (the desktop's
/// `conv_display_metadata` re-walks `installed_indexes()` inside the
/// loop). An engine that will not answer yields an empty index and
/// every row falls back to its own id.
async fn conv_display_index(
    daemon: &Arc<EmbeddedDaemon>,
) -> std::collections::HashMap<String, (String, Option<String>, Option<String>)> {
    let mut out = std::collections::HashMap::new();
    let Some(engine) = daemon.corpus_engine() else {
        return out;
    };
    let Ok(infos) = engine.installed_indexes().await else {
        tracing::debug!("atlas_http: installed_indexes unavailable; conv rows keep their ids");
        return out;
    };
    for info in infos {
        let name = if info.corpus_name.is_empty() {
            info.corpus_id.clone()
        } else {
            info.corpus_name.clone()
        };
        let (category, icon) = match &info.display {
            Some(d) => (d.category.clone(), d.icon.clone()),
            None => (None, None),
        };
        out.insert(info.corpus_id.clone(), (name, category, icon));
    }
    out
}

/// One stored RAPTOR row -> its view. The three JSON columns are
/// parsed leniently (a malformed column is an empty list) because
/// that is what the column means to the pane and what the desktop
/// does today; the row itself is never dropped.
fn raptor_node_view(n: ConvRaptorNodeRow) -> ConvRaptorNodeView {
    let primary_entities: Vec<String> =
        serde_json::from_str(&n.primary_entities_json).unwrap_or_default();
    let direct_member_chunk_ids: Vec<u64> = n
        .direct_member_chunk_ids_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let evidence_chunk_ids: Vec<u64> =
        serde_json::from_str(&n.evidence_chunk_ids_json).unwrap_or_default();
    let is_synthetic_tiny = primary_entities.is_empty() && (n.cluster_coherence - 1.0).abs() < 1e-6;
    ConvRaptorNodeView {
        node_id: n.node_id,
        level: n.level as u8,
        summary: n.summary,
        primary_entities,
        direct_member_chunk_ids,
        evidence_chunk_count: evidence_chunk_ids.len(),
        cluster_coherence: n.cluster_coherence,
        is_synthetic_tiny,
    }
}

/// Top-N by salience = sum of `cluster_coherence` over the nodes that
/// name the entity, ties broken by name. The desktop's
/// `rank_entity_chips`, moved so there is one ranking.
fn rank_entity_chips(nodes: &[ConvRaptorNodeRow], top_n: usize) -> Vec<ConvEntityChip> {
    let mut acc: std::collections::HashMap<String, (f32, u32)> = std::collections::HashMap::new();
    for node in nodes {
        let entities: Vec<String> =
            serde_json::from_str(&node.primary_entities_json).unwrap_or_default();
        for ent in entities {
            let trimmed = ent.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry = acc.entry(trimmed.to_string()).or_insert((0.0, 0));
            entry.0 += node.cluster_coherence as f32;
            entry.1 += 1;
        }
    }
    let mut ranked: Vec<(String, f32, u32)> = acc
        .into_iter()
        .map(|(name, (sal, occ))| (name, sal, occ))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked
        .into_iter()
        .take(top_n)
        .map(|(name, salience, occurrence_count)| ConvEntityChip {
            name,
            salience,
            occurrence_count,
        })
        .collect()
}

/// The list row's two derived fields. "Tiny" is one synthetic node
/// with perfect coherence and no extracted entities.
fn summarize_entities(nodes: &[ConvRaptorNodeRow], top_n: usize) -> (Vec<String>, bool) {
    let chips = rank_entity_chips(nodes, top_n);
    let is_tiny = nodes.len() == 1
        && (nodes[0].cluster_coherence - 1.0).abs() < 1e-6
        && nodes[0].primary_entities_json.trim() == "[]";
    (chips.into_iter().map(|c| c.name).collect(), is_tiny)
}

fn reader_for(daemon: &Arc<EmbeddedDaemon>) -> Result<FileAtlasReader, Absence> {
    daemon
        .corpus_engine()
        .map(|engine| FileAtlasReader::new(engine.index_dir().to_path_buf()))
        .ok_or_else(|| Absence::unavailable("corpus engine not initialised"))
}

/// `AtlasViewError` → status. `CorpusNotFound` is the caller's
/// mistake (404); an unreadable indexes dir is ours (500).
fn view_error(e: &AtlasViewError) -> axum::response::Response {
    match e {
        AtlasViewError::CorpusNotFound(_) => not_found(&e.to_string()),
        AtlasViewError::IndexesDir(_) => internal_error(&e.to_string()),
    }
}

/// `AtomQueryError` → status, on the same rule.
fn atom_error(e: &AtomQueryError) -> axum::response::Response {
    match e {
        AtomQueryError::UnknownCorpus(_) => not_found(&e.to_string()),
        AtomQueryError::ReadAtoms(_) | AtomQueryError::Task(_) => internal_error(&e.to_string()),
    }
}
