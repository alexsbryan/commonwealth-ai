// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/internal/workflows/*` — the daemon's workflow job surface.
//!
//! The ONE execution home for interactive workflow runs. Before this module
//! the desktop and the CLI each assembled a private in-process runner (the
//! desktop fed `AppState.inference` into `run_workflow_with_provider`; the
//! CLI ran `run_workflow_in_process` beside the daemon it was already talking
//! to), so the same workflow could execute with different providers, tool
//! sets, and corpus-derivation rules depending on which surface launched it.
//! sv-surface rung 5 (2026-09-09) moved execution here: both clients submit
//! a job and poll.
//!
//! Pattern: `sovereign-mesh`'s `corpus_watch_http` — a stateless router the
//! host hands the daemon as an opaque `axum::Router` (`ServingCapability::
//! workflow_http`), polling endpoints only, NO SSE. This crate owns the
//! router because both hosts (the CLI daemon and the desktop's embedded
//! daemon) already depend on it; `sovereign-mesh` depends on neither
//! workflow crate.
//!
//! Routes (loopback-only, same guard contract as the other `/internal/*`
//! surfaces):
//!
//! | Method | Path                        | Purpose                                  |
//! |--------|-----------------------------|------------------------------------------|
//! | GET    | `/list`                     | The runnable catalog (user + shipped)    |
//! | GET    | `/capabilities?name=…`      | Consent bullets for one workflow         |
//! | POST   | `/run`                      | Submit a run → `{job_id, corpus, origin}`|
//! | GET    | `/jobs/{id}?after=N`        | Status + events with `seq > N`           |
//!
//! Jobs run via [`crate::run_workflow_in_process`] against the daemon's own
//! loopback (`model:`/`embed:` steps use the slots the daemon already
//! loaded), with the corpus/atlas tools the host injects through a
//! [`WorkflowToolFeed`] (tools are not `Clone`, so the host hands a factory,
//! not a list). Events carry a monotonic `seq`; a client polls with `?after=`
//! the last seq it saw. Terminal events (`complete`/`failed`) are appended by
//! the runtime itself, so a client that arrives late still learns the
//! outcome.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path;
use std::sync::{Arc, Mutex};

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use sovereign_contracts::traits::Tool;
use sovereign_workflow::Workflow;

use crate::{resolve_workflow_source, run_workflow_in_process, WorkflowProgress};

/// Fresh tool instances for each run. A factory because `Box<dyn Tool>` is
/// not cloneable and every job needs its own set — the host that builds the
/// router (CLI daemon, embedded desktop daemon) passes
/// `Arc::new(sovereign_tools::workflow_corpus_tools)`.
pub type WorkflowToolFeed = Arc<dyn Fn() -> Vec<Box<dyn Tool>> + Send + Sync>;

/// Build the workflow job router. `daemon_url` is the daemon's own loopback
/// base (no `/v1` suffix — the runner adds it), the base `model:`/`embed:`
/// steps are routed back through.
pub fn workflow_http_router(daemon_url: String, tool_feed: WorkflowToolFeed) -> Router {
    Router::new()
        .route("/internal/workflows/list", get(list_handler))
        .route(
            "/internal/workflows/capabilities",
            get(capabilities_handler),
        )
        .route("/internal/workflows/run", post(run_handler))
        .route("/internal/workflows/jobs/{job_id}", get(job_handler))
        .layer(axum::middleware::from_fn(loopback_only))
        .with_state(Arc::new(WorkflowJobs::new(daemon_url, tool_feed)))
}

// ─── Loopback guard ───────────────────────────────────────────────
//
// Same contract as `sovereign_mesh::loopback_guard` (router-level middleware
// that fails closed when `ConnectInfo` is missing), restated here because
// this crate must not depend on the daemon crate and the daemon crate must
// not own the only copy of a guard the desktop's embedded daemon also needs.
// These routes can execute workflows (shell, file writes, the network) —
// they answer loopback only.

async fn loopback_only(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    match peer {
        Some(p) if p.ip().is_loopback() => next.run(request).await,
        Some(p) => {
            tracing::warn!(
                peer = %p,
                path = %request.uri().path(),
                "workflow_http: rejected non-loopback caller"
            );
            (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "error": "local-only" })),
            )
                .into_response()
        }
        None => {
            tracing::error!(
                path = %request.uri().path(),
                "workflow_http: no ConnectInfo on request — check listener wiring"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "listener misconfigured: missing connect_info"
                })),
            )
                .into_response()
        }
    }
}

// ─── Wire types (the one definition for both ends) ────────────────
//
// `Deserialize` is not dead weight, the same way `corpus_watch_http`'s is
// not: clients (the desktop's `workflow_commands`, the CLI's `workflow_cmd`)
// import these as their HTTP types so a field rename is a compile error on
// both ends, not a runtime deserialization failure.

/// One runnable workflow + the inputs it needs at run time — the shape the
/// desktop's Run-a-workflow view renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowListEntry {
    pub name: String,
    pub description: String,
    /// `"shipped:<name>"` | `"user:<name>"` | the resolved file path.
    pub origin: String,
    pub params: Vec<WorkflowParamSpec>,
}

/// One input field. `kind` lets the UI render a dedicated control for the
/// well-known folder/corpus/glob params and a plain text box for the rest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowParamSpec {
    pub key: String,
    /// `"folder"` | `"corpus"` | `"glob"` | `"text"`.
    pub kind: String,
    pub label: String,
}

fn classify_param(key: &str) -> WorkflowParamSpec {
    let kind = match key {
        "folder" | "corpus" | "glob" => key,
        _ => "text",
    };
    WorkflowParamSpec {
        key: key.to_string(),
        kind: kind.to_string(),
        label: key.to_string(),
    }
}

fn catalog_entry(name: &str, origin: String, toml: &str) -> Option<WorkflowListEntry> {
    let wf = Workflow::parse(toml).ok()?;
    let params = wf
        .referenced_params()
        .into_iter()
        .map(|k| classify_param(&k))
        .collect();
    Some(WorkflowListEntry {
        name: name.to_string(),
        description: crate::first_comment_line(toml),
        origin,
        params,
    })
}

/// The runnable catalog: every parseable `*.toml` under `user_dir` (a
/// same-named file shadows the shipped starter) plus the shipped starters,
/// sorted by name. Dir-parameterized so a test can point it at a fixture;
/// production callers pass [`crate::workflows_dir()`].
pub fn catalog_entries(user_dir: &path::Path) -> Vec<WorkflowListEntry> {
    let mut entries: Vec<WorkflowListEntry> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(rd) = std::fs::read_dir(user_dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("toml") {
                continue;
            }
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(toml) = std::fs::read_to_string(&p) else {
                continue;
            };
            if let Some(entry) = catalog_entry(stem, format!("user:{stem}"), &toml) {
                seen.insert(stem.to_string());
                entries.push(entry);
            }
        }
    }
    for (name, toml) in crate::SHIPPED_WORKFLOWS {
        if seen.contains(*name) {
            continue;
        }
        if let Some(entry) = catalog_entry(name, format!("shipped:{name}"), toml) {
            entries.push(entry);
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

#[derive(Debug, Deserialize)]
pub struct CapabilitiesQuery {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CapabilitiesResponse {
    pub name: String,
    /// Plain-language consent bullets ("run shell commands", "use your local
    /// model"…) — the trust gate the caller shows before starting a run.
    pub bullets: Vec<String>,
}

/// Body for `POST /run`. `toml` is set when the CALLER already resolved a
/// file (the CLI with a local path against a remote `--daemon`); otherwise
/// the daemon resolves `name_or_path` from its own catalog — same
/// `~/.svrnmesh/workflows` dir, same shadowing rules.
#[derive(Debug, Serialize, Deserialize)]
pub struct RunRequest {
    pub name_or_path: String,
    #[serde(default)]
    pub toml: Option<String>,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    #[serde(default)]
    pub concurrency: Option<usize>,
    #[serde(default)]
    pub no_cache: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RunResponse {
    pub job_id: String,
    /// Where the definition came from (echoed so a client can show it).
    pub origin: String,
    /// The corpus this run will build (it has a `tool:corpus_store` step and
    /// a resolved `corpus` param) — so the UI can offer "chat with it".
    pub corpus: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JobQuery {
    /// Only events with `seq > after` are returned. Default 0 = everything.
    #[serde(default)]
    pub after: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    Complete,
    Failed,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JobResponse {
    pub job_id: String,
    pub status: JobStatus,
    pub origin: String,
    pub events: Vec<JobEvent>,
}

/// One retained progress event with its monotonic cursor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobEvent {
    pub seq: u64,
    #[serde(flatten)]
    pub event: WorkflowJobEvent,
}

/// The wire progress enum: the Runner's [`WorkflowProgress`] variants plus
/// the terminal `complete`/`failed` this runtime appends. Tagged on `kind`
/// (snake_case) so a client can switch on it directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowJobEvent {
    RunStarted {
        workflow: String,
        items: usize,
        steps: usize,
    },
    StepDone {
        item: String,
        step: String,
        uses: String,
        for_each: bool,
        cached: bool,
        step_index: usize,
        total_steps: usize,
    },
    ElementSkipped {
        item: String,
        step: String,
        index: usize,
        error: String,
    },
    ItemDone {
        item: String,
        ok: bool,
        ran: usize,
        cached: usize,
    },
    RunFinished {
        ok: usize,
        failed: usize,
    },
    /// Terminal: the run produced a report. `corpus` is the built corpus
    /// when at least one item succeeded and the workflow stores one.
    Complete {
        workflow: String,
        ok: usize,
        failed: usize,
        corpus: Option<String>,
        items: Vec<JobItemOutcome>,
    },
    /// Terminal: the whole run errored before producing a report.
    Failed {
        error: String,
    },
}

impl From<WorkflowProgress> for WorkflowJobEvent {
    fn from(p: WorkflowProgress) -> Self {
        match p {
            WorkflowProgress::RunStarted {
                workflow,
                items,
                steps,
            } => Self::RunStarted {
                workflow,
                items,
                steps,
            },
            WorkflowProgress::StepDone {
                item,
                step,
                uses,
                for_each,
                cached,
                step_index,
                total_steps,
            } => Self::StepDone {
                item,
                step,
                uses,
                for_each,
                cached,
                step_index,
                total_steps,
            },
            WorkflowProgress::ElementSkipped {
                item,
                step,
                index,
                error,
            } => Self::ElementSkipped {
                item,
                step,
                index,
                error,
            },
            WorkflowProgress::ItemDone {
                item,
                ok,
                ran,
                cached,
            } => Self::ItemDone {
                item,
                ok,
                ran,
                cached,
            },
            WorkflowProgress::RunFinished { ok, failed } => Self::RunFinished { ok, failed },
        }
    }
}

/// One item's outcome in the terminal `complete` event — the per-item report
/// the CLI prints (`## item` + output / error).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobItemOutcome {
    pub item: String,
    pub ok: bool,
    pub output: Option<String>,
    pub error: Option<String>,
    pub ran: usize,
    pub cached: usize,
}

/// The run params after daemon-side defaulting: `corpus` defaults to the
/// folder's basename when a `folder` param was given without one (the
/// flagship one-liner ergonomics, previously re-derived by each client).
/// Returns the effective params and the corpus this run will build (`Some`
/// only when the workflow has a `tool:corpus_store` step AND a corpus name
/// resolved).
pub fn derive_run_params(
    mut params: BTreeMap<String, String>,
    builds_corpus: bool,
) -> (BTreeMap<String, String>, Option<String>) {
    if !params.contains_key("corpus") {
        if let Some(folder) = params.get("folder") {
            if let Some(base) = path::Path::new(folder)
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|b| !b.is_empty())
            {
                params.insert("corpus".into(), base.to_string());
            }
        }
    }
    let corpus = builds_corpus
        .then(|| params.get("corpus").cloned())
        .flatten();
    (params, corpus)
}

// ─── Job runtime ──────────────────────────────────────────────────

struct JobRecord {
    origin: String,
    status: JobStatus,
    events: Vec<JobEvent>,
    finished_at: Option<std::time::Instant>,
}

/// Per-process job table behind the router. Retention: terminal jobs are
/// kept for ten minutes after finishing (a late poller still learns the
/// outcome) and pruned on the next submit.
struct WorkflowJobs {
    daemon_url: String,
    tool_feed: WorkflowToolFeed,
    jobs: Mutex<HashMap<String, JobRecord>>,
}

const TERMINAL_JOB_RETENTION: std::time::Duration = std::time::Duration::from_secs(600);

impl WorkflowJobs {
    fn new(daemon_url: String, tool_feed: WorkflowToolFeed) -> Self {
        Self {
            daemon_url,
            tool_feed,
            jobs: Mutex::new(HashMap::new()),
        }
    }

    fn submit(
        self: &Arc<Self>,
        wf: Workflow,
        origin: String,
        expected_corpus: Option<String>,
        params: BTreeMap<String, String>,
        concurrency: usize,
        no_cache: bool,
    ) -> String {
        let job_id = new_job_id();
        {
            let mut jobs = self.jobs.lock().unwrap();
            // Retention pass: drop terminal jobs past the window so the map
            // cannot grow without bound across a long daemon lifetime.
            let now = std::time::Instant::now();
            jobs.retain(|_, r| {
                r.finished_at
                    .map(|t| now.duration_since(t) < TERMINAL_JOB_RETENTION)
                    .unwrap_or(true)
            });
            jobs.insert(
                job_id.clone(),
                JobRecord {
                    origin,
                    status: JobStatus::Running,
                    events: Vec::new(),
                    finished_at: None,
                },
            );
        }

        // `self` is an Arc (axum state), so the detached task keeps appending
        // events after the handler returns. One atomic seq counter is shared
        // by the observer and the terminal write so `seq` is monotonic under
        // the run's concurrency.
        let seq = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let runtime = Arc::clone(self);
        let daemon_url = self.daemon_url.clone();
        let tool_feed = Arc::clone(&self.tool_feed);
        let job_id_for_task = job_id.clone();
        tokio::spawn(async move {
            let observer: crate::StepObserver = {
                let runtime = Arc::clone(&runtime);
                let job_id = job_id_for_task.clone();
                let seq = Arc::clone(&seq);
                Arc::new(move |ev: WorkflowProgress| {
                    let n = seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    runtime.push_event(&job_id, n, ev.into());
                })
            };
            let extra = tool_feed();
            let outcome = run_workflow_in_process(
                &wf,
                &daemon_url,
                concurrency,
                no_cache,
                params,
                extra,
                Some(observer),
            )
            .await;
            let terminal = match outcome {
                Ok(report) => {
                    let ok = report.ok_count();
                    let items: Vec<JobItemOutcome> = report
                        .items
                        .iter()
                        .map(|it| JobItemOutcome {
                            item: it.item.clone(),
                            ok: it.result.is_ok(),
                            output: it.result.as_ref().ok().cloned(),
                            error: it.result.as_ref().err().cloned(),
                            ran: it.ran,
                            cached: it.cached,
                        })
                        .collect();
                    // Only surface the corpus when at least one item
                    // succeeded — an all-failed run produced nothing to
                    // chat with.
                    WorkflowJobEvent::Complete {
                        workflow: report.workflow.clone(),
                        ok,
                        failed: report.failed_count(),
                        corpus: (ok > 0).then(|| expected_corpus.clone()).flatten(),
                        items,
                    }
                }
                Err(error) => WorkflowJobEvent::Failed { error },
            };
            let n = seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            runtime.finish(&job_id_for_task, n, terminal);
        });
        job_id
    }

    fn push_event(&self, job_id: &str, seq: u64, event: WorkflowJobEvent) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(record) = jobs.get_mut(job_id) {
            record.events.push(JobEvent { seq, event });
        }
    }

    fn finish(&self, job_id: &str, seq: u64, terminal: WorkflowJobEvent) {
        let status = JobStatus::from_event(&terminal);
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(record) = jobs.get_mut(job_id) {
            record.events.push(JobEvent {
                seq,
                event: terminal,
            });
            record.status = status;
            record.finished_at = Some(std::time::Instant::now());
        }
    }
}

impl JobStatus {
    fn from_event(terminal: &WorkflowJobEvent) -> Self {
        match terminal {
            WorkflowJobEvent::Complete { .. } => Self::Complete,
            WorkflowJobEvent::Failed { .. } => Self::Failed,
            _ => Self::Running,
        }
    }
}

fn new_job_id() -> String {
    // A job id needs uniqueness, not cryptographic strength: a timestamp +
    // process-unique counter is enough and keeps this crate free of a uuid
    // dependency.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("wf-{now:x}-{n:x}")
}

// ─── Handlers ─────────────────────────────────────────────────────

async fn list_handler(ConnectInfo(peer): ConnectInfo<SocketAddr>) -> impl IntoResponse {
    if let Err(r) = guard_localhost(&peer) {
        return r;
    }
    Json(WorkflowListResponse {
        workflows: catalog_entries(&crate::workflows_dir()),
    })
    .into_response()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkflowListResponse {
    pub workflows: Vec<WorkflowListEntry>,
}

async fn capabilities_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(_jobs): State<Arc<WorkflowJobs>>,
    Query(q): Query<CapabilitiesQuery>,
) -> impl IntoResponse {
    if let Err(r) = guard_localhost(&peer) {
        return r;
    }
    let toml = match resolve_workflow_source(&q.name) {
        Ok((toml, _)) => toml,
        Err(e) => return error(StatusCode::NOT_FOUND, e).into_response(),
    };
    let wf = match Workflow::parse(&toml) {
        Ok(w) => w,
        Err(e) => {
            return error(StatusCode::BAD_REQUEST, format!("workflow parse: {e}")).into_response()
        }
    };
    let summary = crate::summarize_capabilities(&wf).await;
    Json(CapabilitiesResponse {
        name: q.name,
        bullets: summary.describe(),
    })
    .into_response()
}

fn guard_localhost(addr: &SocketAddr) -> Result<(), Response> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "local-only" })),
        )
            .into_response())
    }
}

fn error(status: StatusCode, msg: String) -> Response {
    (status, Json(serde_json::json!({ "error": msg }))).into_response()
}

async fn run_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(jobs): State<Arc<WorkflowJobs>>,
    Json(req): Json<RunRequest>,
) -> impl IntoResponse {
    if let Err(r) = guard_localhost(&peer) {
        return r;
    }
    // Resolve + parse up front so a bad name/definition is a 4xx the client
    // surfaces immediately, not a failed job it discovers by polling.
    let (toml, origin) = match req.toml {
        Some(toml) => (toml, req.name_or_path.clone()),
        None => match resolve_workflow_source(&req.name_or_path) {
            Ok(pair) => pair,
            Err(e) => return error(StatusCode::NOT_FOUND, e).into_response(),
        },
    };
    let wf = match Workflow::parse(&toml) {
        Ok(w) => w,
        Err(e) => {
            return error(StatusCode::BAD_REQUEST, format!("workflow parse: {e}")).into_response()
        }
    };

    let builds_corpus = wf.steps.iter().any(|s| s.uses == "tool:corpus_store");
    let (params, corpus) = derive_run_params(req.params, builds_corpus);

    let job_id = jobs.submit(
        wf,
        origin.clone(),
        corpus.clone(),
        params,
        req.concurrency.unwrap_or(4).max(1),
        req.no_cache.unwrap_or(false),
    );
    Json(RunResponse {
        job_id,
        origin,
        corpus,
    })
    .into_response()
}

async fn job_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(jobs): State<Arc<WorkflowJobs>>,
    Path(job_id): Path<String>,
    Query(q): Query<JobQuery>,
) -> impl IntoResponse {
    if let Err(r) = guard_localhost(&peer) {
        return r;
    }
    let jobs_guard = jobs.jobs.lock().unwrap();
    let Some(record) = jobs_guard.get(&job_id) else {
        return error(StatusCode::NOT_FOUND, format!("no workflow job `{job_id}`")).into_response();
    };
    let events = record
        .events
        .iter()
        .filter(|e| e.seq > q.after)
        .cloned()
        .collect();
    Json(JobResponse {
        job_id,
        status: record.status,
        origin: record.origin.clone(),
        events,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `corpus` defaults to the folder's basename (the flagship one-liner),
    /// an explicit corpus wins, and no store step means no corpus — the three
    /// rules every client previously re-derived.
    #[test]
    fn derive_run_params_defaults_corpus_to_folder_basename() {
        let mut params = BTreeMap::new();
        params.insert("folder".to_string(), "/tmp/Some Folder".to_string());
        let (p, corpus) = derive_run_params(params.clone(), true);
        assert_eq!(p.get("corpus").map(String::as_str), Some("Some Folder"));
        assert_eq!(corpus.as_deref(), Some("Some Folder"));

        let mut explicit = params.clone();
        explicit.insert("corpus".to_string(), "named".to_string());
        let (_, corpus) = derive_run_params(explicit, true);
        assert_eq!(corpus.as_deref(), Some("named"));

        // No store step → the run builds nothing, whatever the params say.
        let (_, corpus) = derive_run_params(params, false);
        assert_eq!(corpus, None);
    }

    /// A same-named user workflow shadows the shipped starter, and the
    /// shipped set still lands — the catalog rules the desktop's
    /// `workflow_list_runnable` used to own.
    #[test]
    fn catalog_entries_user_shadows_shipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("notebook.toml"),
            r#"# my customized notebook
[workflow]
name = "notebook"
[source]
type = "inline"
items = ["x"]
[[step]]
id = "up"
uses = "transform:upper"
input = "x"
"#,
        )
        .unwrap();
        let entries = catalog_entries(dir.path());
        let notebook = entries
            .iter()
            .find(|e| e.name == "notebook")
            .expect("notebook present");
        assert_eq!(notebook.origin, "user:notebook");
        assert_eq!(notebook.description, "my customized notebook");
        assert!(
            entries.iter().any(|e| e.origin == "shipped:summarize"),
            "shipped starters still listed: {:?}",
            entries
                .iter()
                .map(|e| e.origin.as_str())
                .collect::<Vec<_>>()
        );
    }

    async fn spawn_router() -> String {
        let router = workflow_http_router(
            "http://127.0.0.1:1".to_string(), // never dialed: no model steps
            Arc::new(Vec::new),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .ok();
        });
        format!("http://{addr}")
    }

    /// End to end over the real HTTP surface: submit an inline pure-transform
    /// workflow (no model steps → no daemon round trip), poll with a cursor,
    /// and reach a terminal `complete` whose items carry the outputs.
    #[tokio::test]
    async fn router_runs_a_workflow_job_end_to_end() {
        let base = spawn_router().await;
        let client = reqwest::Client::new();

        let toml = r#"
[workflow]
name = "upper-it"
[source]
type = "inline"
items = ["hello"]
[[step]]
id = "up"
uses = "transform:upper"
input = "hello"
"#;
        let run: RunResponse = client
            .post(format!("{base}/internal/workflows/run"))
            .json(&serde_json::json!({
                "name_or_path": "test:inline",
                "toml": toml,
                "no_cache": true,
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        // Poll to terminal with a deadline.
        let mut after = 0u64;
        let mut last: Option<JobResponse> = None;
        for _ in 0..200 {
            let job: JobResponse = client
                .get(format!(
                    "{base}/internal/workflows/jobs/{}?after={after}",
                    run.job_id
                ))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if job.status != JobStatus::Running {
                last = Some(job);
                break;
            }
            if let Some(seq) = job.events.last() {
                after = seq.seq;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let job = last.expect("job reaches a terminal state");

        // The cursor contract: a second poll from 0 sees the full history,
        // including the terminal event the runtime appended.
        let full: JobResponse = client
            .get(format!("{base}/internal/workflows/jobs/{}", run.job_id))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(full.status, JobStatus::Complete, "{:?}", full.events);
        let kinds: Vec<&str> = full
            .events
            .iter()
            .map(|e| match &e.event {
                WorkflowJobEvent::RunStarted { .. } => "run_started",
                WorkflowJobEvent::StepDone { .. } => "step_done",
                WorkflowJobEvent::ItemDone { .. } => "item_done",
                WorkflowJobEvent::RunFinished { .. } => "run_finished",
                WorkflowJobEvent::Complete { .. } => "complete",
                other => panic!("unexpected event: {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "run_started",
                "step_done",
                "item_done",
                "run_finished",
                "complete"
            ],
            "{:?}",
            full.events
        );
        // seq is monotonic across the whole history.
        let seqs: Vec<u64> = full.events.iter().map(|e| e.seq).collect();
        let mut sorted = seqs.clone();
        sorted.sort_unstable();
        assert_eq!(seqs, sorted, "seq must be monotonic");
        // The terminal event carries the item output the CLI prints.
        let terminal = full.events.last().unwrap();
        match &terminal.event {
            WorkflowJobEvent::Complete { ok, items, .. } => {
                assert_eq!(*ok, 1);
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].output.as_deref(), Some("HELLO"));
            }
            other => panic!("expected complete, got {other:?}"),
        }
        assert_eq!(job.job_id, run.job_id);
    }

    /// The guard's fail-closed half: a listener that forgets
    /// `into_make_service_with_connect_info` gets a 500, never a silent pass.
    #[tokio::test]
    async fn loopback_guard_fails_closed_without_connect_info() {
        let router = workflow_http_router("http://127.0.0.1:1".to_string(), Arc::new(Vec::new));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.ok(); // bare serve — no connect_info
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/internal/workflows/list"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "missing ConnectInfo must fail closed"
        );
    }
}
