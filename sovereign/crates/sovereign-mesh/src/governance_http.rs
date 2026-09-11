// SPDX-License-Identifier: AGPL-3.0-or-later
//! The living-governance surface — `/internal/governance/{corpus}/…`
//! (sv-surface D8, first half).
//!
//! The read model, the four adjudications, the two seeds and the recipe
//! template, over the daemon's OWN atlas dir
//! (`engine.index_dir().join(corpus).join("atlas")`, never a caller's path).
//! `governance_commands.rs` opened that `Oplog` on the FILESYSTEM from a
//! second process behind a `Mutex` the other process cannot see; these routes
//! make the daemon the single writer. Four input shapes, not nine.
//!
//! Loopback-only, `reading_http`'s posture: these APPEND TO THE OWNER'S
//! OPLOG with an actor stamped on the act.
//!
//! Does NOT cross: `governance_export_write` is `fs::write` to a user-picked
//! save path, and there is no content to serve in its place — the markdown is
//! composed in the frontend out of the payload the view route already returns.
//!
//! [`render_governance_recipe`] and [`GOVERNANCE_ONTOLOGY_GUIDANCE`] are
//! BYTE-IDENTICAL copies of the desktop's for the wire-first window; their
//! right home is `corpus-engine`, which owns the domain (ARCH §6.2).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::{Extension, Path as AxumPath};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use corpus_engine::enrichment::atlas::edges::EdgeId;
use corpus_engine::enrichment::atlas::migrate_ids::migrate_atlas_ids;
use corpus_engine::enrichment::atlas::{read_atlas_atoms, AtomEnvelope, AtomId};
use corpus_engine::enrichment::governance_view::section_titles;
use corpus_engine::enrichment::{GovernanceOpKind, GovernanceView, TensionDisposition};
use corpus_engine::oplog::{Op, Oplog};
use corpus_engine::CorpusEngine;
use sovereign_core::time::unix_now;

use crate::daemon::EmbeddedDaemon;
use crate::http_response::{internal_error, json_error, Absence};
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

/// Serializes this process's oplog appends. `Oplog::append` is
/// open-append-close with no advisory lock (per corpus-engine), so two
/// concurrent writers could interleave a line and its newline.
///
/// The desktop held the identical static and it was NOT enough: two
/// processes each holding their own mutex is one lock short of a lock.
/// What makes this one sufficient is that after the repoint rung the
/// daemon is the only writer — the run lock (keyed on the data root)
/// being what makes THAT true. Cross-process CLI concurrency stays out of
/// scope for the single-steward pilot, unchanged.
static APPEND_LOCK: Mutex<()> = Mutex::new(());

// ─── Errors ────────────────────────────────────────────────────

/// What a governance act can fail as. A closed set, so the status code is
/// read off the TYPE rather than off a phrase table — the compromise
/// `meshapp_http` had to make and 19946f99a then paid down (ARCH §18.3).
#[derive(Debug)]
enum GovError {
    /// No such corpus, tension, or decision. The caller's list is stale.
    NotFound(String),
    /// The request is malformed on its own terms — an empty rationale, a
    /// `keep_rule_id` that is not one of the conflict's two rules.
    BadRequest(String),
    /// Ours: unreadable atlas, a failed append, a template that will not
    /// round-trip.
    Internal(String),
}

impl GovError {
    fn status(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::NotFound(m) | Self::BadRequest(m) | Self::Internal(m) => m,
        }
    }
}

impl From<GovError> for Absence {
    fn from(e: GovError) -> Self {
        Absence::at(e.status(), e.message())
    }
}

impl IntoResponse for GovError {
    fn into_response(self) -> Response {
        json_error(self.status(), self.message())
    }
}

type GovResult<T> = Result<T, GovError>;

// ─── Wire types ────────────────────────────────────────────────

/// The recipe's `[enrichment.ontology.vocabulary]` terms, so a panel can
/// speak the community's language ("rule" / "conflict") rather than
/// "tension edge". Every field optional; the caller falls back to generic
/// defaults. Read from the recipe, never persisted into the atlas.
///
/// `Deserialize` as well as `Serialize` for [`ProjectEntry`]'s reason
/// (`features_http`): the caller parses back into the struct the daemon
/// emitted rather than into a twin that can drift.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VocabularyPayload {
    pub position_term: Option<String>,
    pub tension_term: Option<String>,
    pub concern_term: Option<String>,
    pub evidence_term: Option<String>,
}

/// Metadata for one governance decision (oplog op), keyed by op id in
/// [`GovernanceViewPayload::decisions`].
///
/// The rationale lives on the OP, not on the graph, which is why a
/// `TensionView` alone cannot carry the living history the panel renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionMeta {
    pub ts_unix: i64,
    /// The human rationale, if the op carried one (empty for `AssertRule`).
    pub rationale: String,
    /// `"human:<name>"` or `"seed"` — who authored the act.
    pub actor: String,
}

/// Everything a Conflicts panel renders for a corpus, in one call.
///
/// `view` is `corpus_engine::enrichment::GovernanceView` itself —
/// `Serialize + Deserialize` in its own crate already, so this is a door,
/// not a projection. The five fields beside it are the joins the panel
/// needs and the read model does not carry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceViewPayload {
    /// The joined read-model (rules + tensions + integrity issues).
    pub view: GovernanceView,
    /// section id → human title, for labelling each side of a conflict by
    /// its source document.
    pub section_titles: HashMap<String, String>,
    /// citation section id → numeric chunk id, for a "view passage"
    /// deep-link. Best-effort: an empty map means the index would not
    /// open, and the passage renders un-clickable rather than absent.
    pub section_chunks: HashMap<String, u64>,
    /// scope entity id → canonical name, for topic grouping in exports.
    pub scope_names: HashMap<String, String>,
    /// Recipe vocabulary labels; `None` → the caller's generic defaults.
    pub vocabulary: Option<VocabularyPayload>,
    /// op id → decision metadata (timestamp, rationale, actor).
    pub decisions: HashMap<String, DecisionMeta>,
    /// Whether the documents changed since the last atlas build.
    pub docs_changed_since_build: bool,
}

/// The op ids an adjudication appended, in append order. The caller's undo
/// affordance keys on them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpIdsResponse {
    pub op_ids: Vec<String>,
}

/// The single revert op an undo appended.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpIdResponse {
    pub op_id: String,
}

/// How many rules a seed newly asserted. Zero is the common steady-state
/// answer (the baseline is already established) and is a success.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeededResponse {
    pub seeded: u32,
}

/// `POST .../resolve` — keep one rule, supersede the other.
#[derive(Debug, Deserialize)]
pub struct ResolveRequest {
    pub keep_rule_id: String,
    pub rationale: String,
}

/// `POST .../accept` — both rules stand, and the rationale says why.
#[derive(Debug, Deserialize)]
pub struct AcceptRequest {
    pub rationale: String,
}

/// `POST .../dismiss` — detector noise; the note is optional, which is
/// what distinguishes a dismissal from an acceptance.
#[derive(Debug, Default, Deserialize)]
pub struct DismissRequest {
    #[serde(default)]
    pub rationale: Option<String>,
}

/// `POST .../recipe` — lay down the governance recipe for a folder corpus.
#[derive(Debug, Deserialize)]
pub struct WriteRecipeRequest {
    pub display_name: String,
    pub source_path: String,
}

/// Where the recipe landed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipePathResponse {
    pub path: String,
}

// ─── Router ────────────────────────────────────────────────────

/// The governance router. Mounted unconditionally on serving daemons; a
/// commission with no corpus engine answers 503 naming that, which is a
/// different fact from an unmounted router's 404 (ARCH §18.3).
pub fn governance_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/internal/governance/{corpus}/view", get(get_view))
        .route(
            "/internal/governance/{corpus}/tensions/{tension}/resolve",
            post(resolve_tension),
        )
        .route(
            "/internal/governance/{corpus}/tensions/{tension}/accept",
            post(accept_tension),
        )
        .route(
            "/internal/governance/{corpus}/tensions/{tension}/dismiss",
            post(dismiss_tension),
        )
        .route(
            "/internal/governance/{corpus}/tensions/{tension}/undo",
            post(undo_tension),
        )
        .route("/internal/governance/{corpus}/seed", post(seed))
        .route(
            "/internal/governance/{corpus}/post-build-seed",
            post(post_build_seed),
        )
        .route("/internal/governance/{corpus}/recipe", post(write_recipe))
        .localhost_only_with(daemon)
}

// ─── Handlers ──────────────────────────────────────────────────

/// GET `/internal/governance/{corpus}/view` — the whole panel payload.
///
/// A corpus with no atlas is a 404 naming it, never an empty view: "this
/// corpus has not been enriched" and "this corpus has no conflicts" are
/// different facts and the banner the caller shows differs (ARCH §18.3).
async fn get_view(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    AxumPath(corpus): AxumPath<String>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let dir = atlas_dir(&engine, &corpus);
    let root = index_root(&engine, &corpus);
    let recipes = engine.recipes_dir().to_path_buf();
    let cid = corpus.clone();

    let blocking = tokio::task::spawn_blocking(move || {
        require_atlas(&dir)?;
        let view = GovernanceView::from_atlas_dir(&dir).map_err(|e| absent_or_internal(&dir, e))?;
        let titles = section_titles(&root);
        let scopes = scope_names(&dir);
        let vocab = read_vocabulary(&recipes, &cid);
        let decisions: HashMap<String, DecisionMeta> = Oplog::<GovernanceOpKind>::new(&dir)
            .read_all()
            .unwrap_or_default()
            .into_iter()
            .map(|op| {
                (
                    op.id.as_str().to_string(),
                    DecisionMeta {
                        ts_unix: op.ts_unix,
                        rationale: op_rationale(&op.kind),
                        actor: op.actor,
                    },
                )
            })
            .collect();
        let docs_changed = docs_changed_since_build(&root);
        Ok::<_, GovError>((view, titles, scopes, vocab, decisions, docs_changed))
    })
    .await;

    let (view, section_titles, scope_names, vocabulary, decisions, docs_changed) = match blocking {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return Err(e.into()),
        Err(e) => return Err(Absence::internal(format!("join: {e}"))),
    };

    // Citation section ids → numeric chunk ids for deep-links.
    // Best-effort, mirroring `atlas_http`'s atom_detail: a failure leaves
    // passages non-clickable and the inline preview still renders.
    let mut section_chunks = HashMap::new();
    let unique_sections: Vec<String> = {
        let mut seen = HashSet::new();
        view.rules
            .iter()
            .filter_map(|r| r.citation.as_ref().map(|c| c.chunk_id.clone()))
            .filter(|s| seen.insert(s.clone()))
            .collect()
    };
    if !unique_sections.is_empty() {
        if let Ok(index) = engine.open_index_for_corpus(&corpus).await {
            if let Ok(map) = index.resolve_sections_to_chunks(&unique_sections).await {
                section_chunks = map;
            }
        }
    }

    tracing::debug!(
        %corpus,
        rules = view.rules.len(),
        tensions = view.tensions.len(),
        decisions = decisions.len(),
        deep_links = section_chunks.len(),
        "governance_http: view served",
    );
    Ok(Json(GovernanceViewPayload {
        view,
        section_titles,
        section_chunks,
        scope_names,
        vocabulary,
        decisions,
        docs_changed_since_build: docs_changed,
    })
    .into_response())
}

/// POST `.../tensions/{tension}/resolve` — keep one rule; the other is
/// superseded. Answers the two appended op ids.
async fn resolve_tension(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    AxumPath((corpus, tension)): AxumPath<(String, String)>,
    Json(body): Json<ResolveRequest>,
) -> Result<Response, Absence> {
    adjudicate(daemon, corpus, move |dir| {
        resolve_at(dir, &tension, &body.keep_rule_id, &body.rationale)
    })
    .await
}

/// POST `.../tensions/{tension}/accept` — both rules remain in force.
///
/// An empty rationale is a 400: an accepted conflict that records no
/// reason is the one adjudication a later reader cannot act on.
async fn accept_tension(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    AxumPath((corpus, tension)): AxumPath<(String, String)>,
    Json(body): Json<AcceptRequest>,
) -> Result<Response, Absence> {
    adjudicate(daemon, corpus, move |dir| {
        accept_at(dir, &tension, &body.rationale)
    })
    .await
}

/// POST `.../tensions/{tension}/dismiss` — detector noise, optional note.
async fn dismiss_tension(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    AxumPath((corpus, tension)): AxumPath<(String, String)>,
    body: Option<Json<DismissRequest>>,
) -> Result<Response, Absence> {
    let rationale = body.and_then(|Json(b)| b.rationale);
    adjudicate(daemon, corpus, move |dir| {
        dismiss_at(dir, &tension, rationale.as_deref())
    })
    .await
}

/// POST `.../tensions/{tension}/undo` — revert the adjudication bundle.
async fn undo_tension(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    AxumPath((corpus, tension)): AxumPath<(String, String)>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let dir = atlas_dir(&engine, &corpus);
    Ok(
        match tokio::task::spawn_blocking(move || undo_at(&dir, &tension)).await {
            Ok(Ok(op_id)) => {
                tracing::info!(%corpus, %op_id, "governance_http: decision reverted");
                Json(OpIdResponse { op_id }).into_response()
            }
            Ok(Err(e)) => e.into_response(),
            Err(e) => internal_error(&format!("join: {e}")),
        },
    )
}

/// POST `.../seed` — establish or refresh the governed rule baseline.
/// Idempotent by construction: rules the oplog already governs are
/// skipped, so re-running after every rebuild is the intended use.
async fn seed(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    AxumPath(corpus): AxumPath<String>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let dir = atlas_dir(&engine, &corpus);
    Ok(
        match tokio::task::spawn_blocking(move || seed_at(&dir)).await {
            Ok(Ok(seeded)) => {
                tracing::info!(%corpus, seeded, "governance_http: rule baseline seeded");
                Json(SeededResponse { seeded }).into_response()
            }
            Ok(Err(e)) => e.into_response(),
            Err(e) => internal_error(&format!("join: {e}")),
        },
    )
}

/// POST `.../post-build-seed` — migrate atom ids to content-hash THEN seed.
///
/// The order is load-bearing for living governance and is why this is its
/// own route rather than two calls a caller sequences: migrate rewrites
/// sequential atom ids to content-hash ids so the seeded `AssertRule`s —
/// and the rule refs in past Supersede/Resolve ops — keep resolving week
/// over week. A caller that got the order wrong would silently orphan
/// every past decision.
///
/// Self-gating: `{"seeded": 0}` for a non-governance corpus, so a
/// completion handler calls it unconditionally without first reading the
/// recipe domain. That is a NO-OP, not a substitution: the corpus is not
/// governance-managed and there is nothing to seed.
async fn post_build_seed(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    AxumPath(corpus): AxumPath<String>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let dir = atlas_dir(&engine, &corpus);
    let recipes = engine.recipes_dir().to_path_buf();
    let cid = corpus.clone();
    let out = tokio::task::spawn_blocking(move || {
        if !is_governance_corpus(&recipes, &cid) {
            return Ok(0);
        }
        post_build_at(&dir, &cid)
    })
    .await;
    Ok(match out {
        Ok(Ok(seeded)) => {
            tracing::info!(%corpus, seeded, "governance_http: post-build migrate+seed");
            Json(SeededResponse { seeded }).into_response()
        }
        Ok(Err(e)) => e.into_response(),
        Err(e) => internal_error(&format!("join: {e}")),
    })
}

/// POST `.../recipe` — write the governance recipe for a folder corpus.
///
/// Under the DAEMON's `recipes_dir()`, which is the whole point: an
/// attached desktop writing this locally lays it down where the daemon
/// never looks, and `recipe_enrich_init_from_corpus` then selects the
/// literary pipeline instead of `custom_atlas`.
async fn write_recipe(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    AxumPath(corpus): AxumPath<String>,
    Json(body): Json<WriteRecipeRequest>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let recipes = engine.recipes_dir().to_path_buf();
    let cid = corpus.clone();
    let out = tokio::task::spawn_blocking(move || {
        write_recipe_sync(&recipes, &cid, &body.display_name, &body.source_path)
    })
    .await;
    Ok(match out {
        Ok(Ok(path)) => {
            tracing::info!(%corpus, %path, "governance_http: governance recipe written");
            Json(RecipePathResponse { path }).into_response()
        }
        Ok(Err(e)) => e.into_response(),
        Err(e) => internal_error(&format!("join: {e}")),
    })
}

/// The three adjudications that share `(corpus, tension)` + a rationale
/// and answer a list of op ids. One place that resolves the engine, hops
/// to a blocking thread and renders the outcome — the alternative is the
/// same fifteen lines three times, and the three drifting is exactly how
/// one of them would end up on a different atlas dir.
async fn adjudicate<F>(
    daemon: Arc<EmbeddedDaemon>,
    corpus: String,
    act: F,
) -> Result<Response, Absence>
where
    F: FnOnce(&Path) -> GovResult<Vec<String>> + Send + 'static,
{
    let engine = engine_for(&daemon)?;
    let dir = atlas_dir(&engine, &corpus);
    Ok(match tokio::task::spawn_blocking(move || act(&dir)).await {
        Ok(Ok(op_ids)) => {
            tracing::info!(%corpus, appended = op_ids.len(), "governance_http: adjudicated");
            Json(OpIdsResponse { op_ids }).into_response()
        }
        Ok(Err(e)) => e.into_response(),
        Err(e) => internal_error(&format!("join: {e}")),
    })
}

// ─── The acts (the testable cores) ─────────────────────────────

/// The actor stamped on human adjudications (INV-2 requires a `human:`
/// prefix on every non-`AssertRule` op).
///
/// NAMED BEHAVIOUR DELTA: this is the DAEMON's OS user, not the desktop's.
/// On the same machine they are the same name; they are not the same name
/// when a daemon runs under a service account. The single-steward pilot's
/// premise — the OS user names the hand, the rationale carries the
/// authority — is unchanged, and per-member identity is still deferred (no
/// user model exists to consult).
fn actor() -> String {
    let who = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "steward".to_string());
    format!("human:{who}")
}

/// Append ops under the process append lock. Returns the appended op ids.
fn append_ops(dir: &Path, ops: &[Op<GovernanceOpKind>]) -> GovResult<Vec<String>> {
    let _guard = APPEND_LOCK
        .lock()
        .map_err(|_| GovError::Internal("append lock poisoned".into()))?;
    let oplog = Oplog::<GovernanceOpKind>::new(dir);
    let mut ids = Vec::with_capacity(ops.len());
    for op in ops {
        oplog
            .append(op)
            .map_err(|e| GovError::Internal(format!("appending governance op: {e}")))?;
        ids.push(op.id.as_str().to_string());
    }
    Ok(ids)
}

/// Refuse an atlas that is not there.
///
/// MEASURED DELTA, deliberate. `GovernanceView::from_atlas_dir` answers
/// `Ok` with an EMPTY view for a directory that does not exist — its
/// readers treat a missing `atoms.json` as "no atoms" — so the desktop
/// command rendered "no conflicts" for a corpus that was never enriched,
/// with nothing anywhere saying which of the two it was. That is the
/// §18.3 substitution, and the route closes it: "not enriched" is a 404
/// naming the path, and a genuinely conflict-free enriched corpus is a
/// 200 with empty lists. The two drive different banners.
///
/// The probe is `atoms.json`, not the directory: an atlas dir created by
/// some earlier phase and never filled is equally not-enriched, and a
/// caller cannot act on the distinction between them.
fn require_atlas(dir: &Path) -> GovResult<()> {
    if dir.join("atoms.json").exists() {
        return Ok(());
    }
    Err(GovError::NotFound(format!(
        "no atlas at {} — this corpus has not been enriched",
        dir.display()
    )))
}

/// An atlas that exists and will not parse is OURS (500). Absence has
/// already been ruled out by [`require_atlas`] before this is reached.
fn absent_or_internal(dir: &Path, e: impl std::fmt::Display) -> GovError {
    if let Err(absent) = require_atlas(dir) {
        return absent;
    }
    GovError::Internal(format!("reading governance view: {e}"))
}

fn load_view(dir: &Path) -> GovResult<GovernanceView> {
    require_atlas(dir)?;
    GovernanceView::from_atlas_dir(dir).map_err(|e| absent_or_internal(dir, e))
}

/// Look a tension up in a freshly-loaded view, returning its endpoint rule
/// ids so every appended adjudication is pair-durable by construction.
fn tension_endpoints(view: &GovernanceView, tension_id: &str) -> GovResult<(AtomId, AtomId)> {
    view.tensions
        .iter()
        .find(|t| t.id.as_str() == tension_id)
        .map(|t| (t.rule_a.clone(), t.rule_b.clone()))
        .ok_or_else(|| {
            GovError::NotFound(format!(
                "no conflict `{tension_id}` in this corpus — the list may be out of date"
            ))
        })
}

/// The tension's own id, resolved from the same view the endpoints came
/// from. Separate from [`tension_endpoints`] only so neither has to
/// `expect` on a lookup the other already did.
fn tension_id_of(view: &GovernanceView, tension_id: &str) -> GovResult<EdgeId> {
    view.tensions
        .iter()
        .find(|t| t.id.as_str() == tension_id)
        .map(|t| t.id.clone())
        .ok_or_else(|| GovError::NotFound(format!("no conflict `{tension_id}` in this corpus")))
}

fn resolve_at(
    dir: &Path,
    tension_id: &str,
    keep_rule_id: &str,
    rationale: &str,
) -> GovResult<Vec<String>> {
    let view = load_view(dir)?;
    let (rule_a, rule_b) = tension_endpoints(&view, tension_id)?;
    let (keep, old) = if keep_rule_id == rule_a.as_str() {
        (rule_a.clone(), rule_b.clone())
    } else if keep_rule_id == rule_b.as_str() {
        (rule_b.clone(), rule_a.clone())
    } else {
        return Err(GovError::BadRequest(format!(
            "`{keep_rule_id}` is not one of this conflict's two rules"
        )));
    };
    let tension = tension_id_of(&view, tension_id)?;
    let ts = unix_now();
    let who = actor();
    // The Supersede is the substance; ResolveTension records that this
    // tension was adjudicated via it, so a later undo reverts the bundle
    // atomically. Both carry the endpoint pair for rebuild-durability.
    let supersede = Op::new(
        GovernanceOpKind::Supersede {
            new_rule: keep,
            old_rules: vec![old],
            rationale: rationale.to_string(),
        },
        ts,
        who.clone(),
    );
    let resolve = Op::new(
        GovernanceOpKind::ResolveTension {
            tension,
            via: supersede.id.clone(),
            endpoints: Some((rule_a, rule_b)),
            rationale: rationale.to_string(),
        },
        ts,
        who,
    );
    append_ops(dir, &[supersede, resolve])
}

fn accept_at(dir: &Path, tension_id: &str, rationale: &str) -> GovResult<Vec<String>> {
    if rationale.trim().is_empty() {
        return Err(GovError::BadRequest(
            "an accepted conflict must record why both rules stand".into(),
        ));
    }
    let view = load_view(dir)?;
    let (rule_a, rule_b) = tension_endpoints(&view, tension_id)?;
    let tension = tension_id_of(&view, tension_id)?;
    let op = Op::new(
        GovernanceOpKind::AcceptTension {
            tension,
            rationale: rationale.to_string(),
            endpoints: Some((rule_a, rule_b)),
        },
        unix_now(),
        actor(),
    );
    append_ops(dir, &[op])
}

fn dismiss_at(dir: &Path, tension_id: &str, rationale: Option<&str>) -> GovResult<Vec<String>> {
    let view = load_view(dir)?;
    let (rule_a, rule_b) = tension_endpoints(&view, tension_id)?;
    let tension = tension_id_of(&view, tension_id)?;
    let op = Op::new(
        GovernanceOpKind::DismissTension {
            tension,
            endpoints: Some((rule_a, rule_b)),
            rationale: rationale.unwrap_or("").to_string(),
        },
        unix_now(),
        actor(),
    );
    append_ops(dir, &[op])
}

fn undo_at(dir: &Path, tension_id: &str) -> GovResult<String> {
    let view = load_view(dir)?;
    let tv = view
        .tensions
        .iter()
        .find(|t| t.id.as_str() == tension_id)
        .ok_or_else(|| GovError::NotFound(format!("no conflict `{tension_id}` in this corpus")))?;
    // The view already resolved edge-id-then-pair matching; its
    // disposition names the winning adjudication op.
    let by = match &tv.disposition {
        TensionDisposition::Resolved { by }
        | TensionDisposition::Accepted { by }
        | TensionDisposition::Dismissed { by } => by.clone(),
        TensionDisposition::Open | TensionDisposition::Moot { .. } => {
            return Err(GovError::BadRequest(
                "this conflict has no decision to undo".into(),
            ));
        }
    };
    // Reconstruct the bundle to revert: the winning op, plus (for a
    // resolve) the Supersede it was authored via.
    let ops = Oplog::<GovernanceOpKind>::new(dir)
        .read_all()
        .map_err(|e| GovError::Internal(format!("reading governance oplog: {e}")))?;
    let winner = ops.iter().find(|op| op.id == by).ok_or_else(|| {
        GovError::NotFound("the decision to undo is no longer in the log".to_string())
    })?;
    let mut targets = vec![by];
    if let GovernanceOpKind::ResolveTension { via, .. } = &winner.kind {
        targets.push(via.clone());
    }
    let revert = Op::new(
        GovernanceOpKind::Revert {
            targets,
            // The desktop stamped "undo from desktop" here. The daemon is
            // the writer now and says so; the string is a human-readable
            // rationale on the op, read by nothing.
            rationale: "undo".into(),
        },
        unix_now(),
        actor(),
    );
    let ids = append_ops(dir, std::slice::from_ref(&revert))?;
    ids.into_iter()
        .next()
        .ok_or_else(|| GovError::Internal("append returned no op id".into()))
}

/// One idempotent `AssertRule` per Claim atom (actor `"seed"`,
/// INV-2-exempt). Skips rules the oplog already governs.
fn seed_at(dir: &Path) -> GovResult<u32> {
    require_atlas(dir)?;
    let atoms = read_atlas_atoms(dir)
        .map_err(|e| GovError::Internal(format!("reading atoms.json at {}: {e}", dir.display())))?;
    let oplog = Oplog::<GovernanceOpKind>::new(dir);
    let already: HashSet<_> = oplog
        .read_all()
        .map_err(|e| GovError::Internal(format!("reading governance oplog: {e}")))?
        .into_iter()
        .filter_map(|op| match op.kind {
            GovernanceOpKind::AssertRule { rule, .. } => Some(rule),
            _ => None,
        })
        .collect();
    let ts = unix_now();
    let mut new_ops = Vec::new();
    for env in &atoms.atoms {
        if let AtomEnvelope::Claim(c) = env {
            if already.contains(&c.id) {
                continue;
            }
            new_ops.push(Op::new(
                GovernanceOpKind::AssertRule {
                    rule: c.id.clone(),
                    source_doc: None,
                },
                ts,
                "seed",
            ));
        }
    }
    let seeded = new_ops.len() as u32;
    if !new_ops.is_empty() {
        append_ops(dir, &new_ops)?;
    }
    Ok(seeded)
}

/// migrate-ids THEN seed. Best-effort on the migrate half and idempotent:
/// a non-governance or already-content-hash atlas still seeds fine.
fn post_build_at(dir: &Path, corpus_id: &str) -> GovResult<u32> {
    match migrate_atlas_ids(dir, corpus_id, false) {
        Ok(summary) => tracing::info!(
            corpus_id,
            ?summary,
            "governance_http: migrated atom ids to content-hash"
        ),
        Err(e) => {
            // Non-fatal: sequential ids just won't survive a future
            // rebuild, surfaced as a needs-attention issue then.
            tracing::warn!(corpus_id, error = %e, "governance_http: migrate-ids failed");
        }
    }
    seed_at(dir)
}

// ─── Reads that join onto the view ─────────────────────────────

/// The rationale string an op kind carries (empty when it has none).
fn op_rationale(kind: &GovernanceOpKind) -> String {
    match kind {
        GovernanceOpKind::Supersede { rationale, .. }
        | GovernanceOpKind::RetractRule { rationale, .. }
        | GovernanceOpKind::ResolveTension { rationale, .. }
        | GovernanceOpKind::AcceptTension { rationale, .. }
        | GovernanceOpKind::DismissTension { rationale, .. }
        | GovernanceOpKind::Revert { rationale, .. } => rationale.clone(),
        GovernanceOpKind::AssertRule { .. } => String::new(),
    }
}

/// Entity-atom id → canonical name, for grouping rules by the topic they
/// govern. An unreadable atlas yields an empty map: the grouping degrades
/// to ungrouped, and the view route has already 404'd if the atlas is
/// genuinely absent.
fn scope_names(atlas_dir: &Path) -> HashMap<String, String> {
    let Ok(atoms) = read_atlas_atoms(atlas_dir) else {
        return HashMap::new();
    };
    atoms
        .atoms
        .iter()
        .filter_map(|env| match env {
            AtomEnvelope::Entity(e) => Some((e.id.as_str().to_string(), e.canonical_name.clone())),
            _ => None,
        })
        .collect()
}

fn recipe_path(recipes_dir: &Path, corpus_id: &str) -> PathBuf {
    recipes_dir.join(corpus_id).join("recipe.toml")
}

fn read_vocabulary(recipes_dir: &Path, corpus_id: &str) -> Option<VocabularyPayload> {
    let recipe = corpus_engine::Recipe::from_file(&recipe_path(recipes_dir, corpus_id)).ok()?;
    // Terms come from the parsed policies, whatever the block's version —
    // version 0 `vocabulary` or version 1 `label`s land in the same place.
    let vocab = recipe.custom_ontology()?.prose.terms;
    Some(VocabularyPayload {
        position_term: vocab.position_term,
        tension_term: vocab.tension_term,
        concern_term: vocab.concern_term,
        evidence_term: vocab.evidence_term,
    })
}

/// Whether a corpus's recipe declares it governance-managed
/// (`[enrichment] domain = "governance"`).
fn is_governance_corpus(recipes_dir: &Path, corpus_id: &str) -> bool {
    corpus_engine::Recipe::from_file(&recipe_path(recipes_dir, corpus_id))
        .ok()
        .and_then(|r| r.enrichment)
        .and_then(|e| e.domain)
        .is_some_and(|d| d.eq_ignore_ascii_case("governance"))
}

/// Best-effort "documents changed since the atlas was last built".
/// Heuristic: the chunk/section manifest (`chapters.json`, rewritten on
/// ingest) is newer than the extracted graph (`atoms.json`, written on
/// enrich). A missing file reads as "not stale" — a soft banner, never a
/// hard gate, and a rebuild is idempotent.
fn docs_changed_since_build(index_root: &Path) -> bool {
    let mtime = |p: PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let (Some(docs), Some(atoms)) = (
        mtime(index_root.join("chapters.json")),
        mtime(index_root.join("atlas").join("atoms.json")),
    ) else {
        return false;
    };
    docs > atoms
}

// ─── Recipe template ───────────────────────────────────────────

fn write_recipe_sync(
    recipes_dir: &Path,
    corpus_id: &str,
    display_name: &str,
    source_path: &str,
) -> GovResult<String> {
    let dir = recipes_dir.join(corpus_id);
    std::fs::create_dir_all(&dir)
        .map_err(|e| GovError::Internal(format!("creating recipe dir: {e}")))?;
    let path = dir.join("recipe.toml");
    let toml = render_governance_recipe(corpus_id, display_name, source_path);
    std::fs::write(&path, &toml)
        .map_err(|e| GovError::Internal(format!("writing recipe.toml: {e}")))?;
    // Validate: it must parse AND resolve to the custom-ontology path, or
    // enrichment would silently fall back to the literary pipeline.
    match corpus_engine::Recipe::from_file(&path) {
        Ok(r) if r.custom_ontology().is_some() => Ok(path.display().to_string()),
        Ok(_) => Err(GovError::Internal(
            "governance recipe wrote but has no custom ontology — template bug".into(),
        )),
        Err(e) => Err(GovError::Internal(format!(
            "governance recipe failed to parse: {e}"
        ))),
    }
}

/// Generalized governance ontology guidance — a domain-neutral
/// generalization of the maple-house probe recipe. Covers any community
/// or shared organization governed by founding documents plus dated
/// decisions (houses, co-ops, clubs, small orgs). The four
/// NOT-a-conflict discriminators and the "name one concrete moment" test
/// are kept verbatim — they are the detector's decoy-rejection logic and
/// are already domain-general.
pub const GOVERNANCE_ONTOLOGY_GUIDANCE: &str = r#"This corpus is the governing rules of a community or shared organization: founding documents (a charter, bylaws, or agreement) plus a series of dated meeting decisions that amend, extend, or override those documents over time. Treat it as living common law — later decisions can change earlier rules.

Extract, as claims, every NORMATIVE STATEMENT — each thing that is required, forbidden, or permitted. For each such rule, capture:
- its deontic force: whether it requires, forbids, or permits something;
- the single topic it governs — for example: guests, quiet hours, shared spaces, chores, money, membership, meetings, pets. Attribute each rule to that topic, so that all rules about the same topic are grouped together;
- any conditions or exceptions it carries (times, days, who it applies to, where it applies).

Identify the governed topics themselves as entities.

Surface TENSIONS between rules: a later decision that contradicts, narrows, or overrides an earlier rule, or any two rules that give incompatible guidance for the same situation. A tension is a genuine conflict in what is required, forbidden, or permitted for the same topic and situation — NOT merely two rules that happen to mention the same word. Two rules about different aspects of the same topic (for example, where a guest may park versus how many nights a guest may stay) are NOT in tension.

In particular, these pairs are NOT conflicts even when they share a topic — do not flag them:
- Two SEPARATE exemptions or exceptions to the same rule (one member excused for one reason, another excused for a different reason): each stands alone, and honoring one never forces breaking the other.
- A rule about one group of people versus a rule about a DIFFERENT group (visitors versus members): they do not bind the same person at the same moment.
- Rules that govern DIFFERENT places or resources (one room versus another; one shared resource versus another).
- An ADDITIVE rule that layers a step, label, or record on top of another: both can be followed at once.
Flag a conflict only when you can name one concrete moment in which a single member, in one place and at one time, would have to break one rule to follow the other.

Reader questions worth surfacing: What is the current rule about a given topic? Which founding provisions have been amended by a later decision?"#;

/// Render a minimal governance recipe for a folder corpus. The
/// acquire/extract/chunk blocks satisfy the parser and record
/// provenance; `enrich init --from-corpus` builds the atlas from the
/// installed index, not from these. The ontology block is what makes the
/// corpus governance-managed.
fn render_governance_recipe(corpus_id: &str, display_name: &str, source_path: &str) -> String {
    // TOML-escape the two interpolated free-text fields (backslash + quote).
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let name = esc(display_name);
    let path = esc(source_path);
    let guidance = GOVERNANCE_ONTOLOGY_GUIDANCE; // triple-quoted below; contains no """.
    format!(
        r#"# Generated by Sovereign Desktop — community governance template.
# Reconciles rules from documents; it never authors rules.

[corpus]
id = "{corpus_id}"
name = "{name}"
description = "Governing rules — founding documents plus dated decisions."
license = "none"
schema_version = 1

[acquire]
type = "local_file"
path = "{path}"

[extract]
type = "markdown"

[chunk]
type = "paragraph"
max_chars = 2000
overlap_chars = 200

[index]
fts = true
vector = true

[enrichment]
enabled = true
type = "atlas"
domain = "governance"

[enrichment.ontology]
guidance = """
{guidance}
"""

[enrichment.ontology.vocabulary]
position_term = "rule"
tension_term = "conflict"
concern_term = "community question"
evidence_term = "passage"
"#
    )
}

// ─── Helpers ───────────────────────────────────────────────────

/// The daemon's own corpus engine. ONE lookup site, so no handler can
/// reach a different index root than the one the atlas routes serve.
fn engine_for(daemon: &Arc<EmbeddedDaemon>) -> Result<Arc<CorpusEngine>, Absence> {
    daemon
        .corpus_engine()
        .map(Arc::clone)
        .ok_or_else(|| Absence::unavailable("this daemon has no corpus engine"))
}

/// `<index_dir>/<corpus>` — where `chapters.json` lives. The daemon's own,
/// matching `atlas_http`'s `FileAtlasReader::new(engine.index_dir())`.
fn index_root(engine: &CorpusEngine, corpus_id: &str) -> PathBuf {
    engine.index_dir().join(corpus_id)
}

/// `<index_root>/atlas` — `atoms.json`, `edges.json`,
/// `governance_oplog.jsonl`.
fn atlas_dir(engine: &CorpusEngine, corpus_id: &str) -> PathBuf {
    index_root(engine, corpus_id).join("atlas")
}
