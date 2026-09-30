// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `ingest:v1` execute origin (pb-work-donor).
//!
//! cw-rails runs the one donor drive: it leases units off the `work` fold and
//! runs `process:v1` itself. An `ingest:v1` unit runs through THIS node's
//! corpus engine, in this process (HUMAN-fp7-ingest-surface (a)), so the
//! daemon serves [`IngestExecutor`] on a loopback port and registers that
//! port in cw-rails' origin table under `Admit::Local` (listed, never
//! forwarded to a dialer). The donor finds it by the listing and forwards
//! each unit it leases through the four doors `oicp_types::work::exec`
//! names: describe, validate before the lease, run with progress, cancel.
//!
//! This module owns no lease, no fold and no loop over the queue. It runs the
//! unit it is handed, and it says who the credit goes to: the daemon's node
//! id, the roster identity (phase-b-39 fork 1).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use host_kit::shell::RouteBundle;
use kernel_types::NodeId;
use oicp_types::origin::{Admit, Framing, OriginRegistration};
use sovereign_contracts::oicp::work::exec::{
    exec_slot, ExecCancel, ExecDescription, ExecEvent, ExecRun, JobContext, ProgressSink,
    EXEC_CANCEL_PATH, EXEC_DESCRIBE_PATH, EXEC_RUN_PATH, EXEC_VALIDATE_PATH,
};
use sovereign_contracts::oicp::work::refusal::WorkRefusal;
use sovereign_contracts::oicp::JobUnit;
use tracing::{debug, info, warn};

use crate::ingest_executor::{IngestExecutor, TRACE_TARGET};

/// How long a registration lives in cw-rails' table without a renew. A
/// daemon that exits without releasing drops out of the table, and the kind
/// out of the offer, within this.
pub const ORIGIN_TTL_SECS: u64 = 60;

/// How often the registration is renewed, or retried while cw-rails is not
/// answering: a third of the TTL, so two renews may be lost before it lapses.
pub const ORIGIN_RENEW_EVERY: Duration = Duration::from_secs(ORIGIN_TTL_SECS / 3);

/// The origin: one executor, the credit identity, and the cancellation flag of
/// every unit in flight, keyed by unit hash.
pub struct WorkOrigin {
    executor: IngestExecutor,
    credit_node: NodeId,
    running: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl WorkOrigin {
    pub fn new(executor: IngestExecutor, credit_node: NodeId) -> Self {
        Self {
            executor,
            credit_node,
            running: Mutex::new(HashMap::new()),
        }
    }

    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<AtomicBool>>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The four doors, as one bundle.
pub fn router(origin: Arc<WorkOrigin>) -> RouteBundle {
    RouteBundle::new("work-origin")
        .route(EXEC_DESCRIBE_PATH, get(describe))
        .route(EXEC_VALIDATE_PATH, post(validate))
        .route(EXEC_RUN_PATH, post(run))
        .route(EXEC_CANCEL_PATH, post(cancel))
        .with_state(origin)
}

async fn describe(State(origin): State<Arc<WorkOrigin>>) -> Json<ExecDescription> {
    Json(ExecDescription {
        descriptor: origin.executor.descriptor(),
        credit_node: Some(origin.credit_node),
    })
}

async fn validate(
    State(origin): State<Arc<WorkOrigin>>,
    Json(unit): Json<JobUnit>,
) -> Json<Result<(), WorkRefusal>> {
    let verdict = origin.executor.validate(&unit);
    debug!(
        target: TRACE_TARGET,
        unit = %unit.unit_hash,
        refused = verdict.as_ref().err().map(|r| r.id()),
        "work origin: validated a unit for the donor"
    );
    Json(verdict)
}

async fn cancel(State(origin): State<Arc<WorkOrigin>>, Json(req): Json<ExecCancel>) -> StatusCode {
    match origin.running().get(&req.unit_hash) {
        Some(flag) => {
            debug!(target: TRACE_TARGET, unit = %req.unit_hash,
                   "work origin: the donor cancelled a unit in flight");
            flag.store(true, Ordering::SeqCst);
            StatusCode::OK
        }
        None => {
            debug!(target: TRACE_TARGET, unit = %req.unit_hash,
                   "work origin: a cancel named no unit in flight");
            StatusCode::NOT_FOUND
        }
    }
}

/// Run one unit and answer its progress and outcome as newline-delimited
/// [`ExecEvent`]s. The unit runs on its own task, so a donor that hangs up
/// does not drop it mid-write: the hang-up sets its cancellation flag, which
/// is what a lost lease sets too.
async fn run(State(origin): State<Arc<WorkOrigin>>, Json(req): Json<ExecRun>) -> Response {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ExecEvent>();
    let cancel = Arc::new(AtomicBool::new(false));
    let unit_hash = req.unit.unit_hash.clone();
    origin
        .running()
        .insert(unit_hash.clone(), Arc::clone(&cancel));
    let progress_tx = tx.clone();
    let hung_up = Arc::clone(&cancel);
    let sink: ProgressSink = Arc::new(move |note: &str| {
        let event = ExecEvent::Progress {
            note: note.to_string(),
        };
        if progress_tx.send(event).is_err() && !hung_up.swap(true, Ordering::SeqCst) {
            warn!(target: TRACE_TARGET,
                  "work origin: the donor hung up on a running unit — cancelling it");
        }
    });
    debug!(target: TRACE_TARGET, unit = %unit_hash, workdir = %req.workdir.display(),
           "work origin: running a unit for the donor");
    tokio::spawn(async move {
        let ctx = JobContext::new(req.workdir.clone())
            .with_cancel(cancel)
            .with_progress(sink);
        let outcome = origin.executor.execute(&req.unit, &ctx).await;
        origin.running().remove(&unit_hash);
        debug!(target: TRACE_TARGET, unit = %unit_hash, ok = outcome.is_ok(),
               "work origin: the unit returned");
        if tx.send(ExecEvent::Done { outcome }).is_err() {
            warn!(target: TRACE_TARGET, unit = %unit_hash,
                  "work origin: the unit finished after the donor hung up — its outcome is dropped");
        }
    });
    let lines = futures::stream::unfold(rx, |mut rx| async move {
        let event = rx.recv().await?;
        let mut line = match serde_json::to_vec(&event) {
            Ok(v) => v,
            Err(e) => {
                warn!(target: TRACE_TARGET, error = %e, "work origin: an event could not be encoded");
                return None;
            }
        };
        line.push(b'\n');
        Some((
            Ok::<_, std::convert::Infallible>(bytes::Bytes::from(line)),
            rx,
        ))
    });
    (
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        axum::body::Body::from_stream(lines),
    )
        .into_response()
}

/// Serve the origin on an ephemeral loopback port and keep it registered with
/// cw-rails at `rails_base`. Dropping the handle stops both; the registration
/// then lapses at its TTL.
pub struct WorkOriginHandle {
    pub addr: SocketAddr,
    serve: tokio::task::JoinHandle<()>,
    register: tokio::task::JoinHandle<()>,
}

impl Drop for WorkOriginHandle {
    fn drop(&mut self) {
        self.serve.abort();
        self.register.abort();
    }
}

pub async fn spawn(
    origin: Arc<WorkOrigin>,
    rails_base: String,
) -> std::io::Result<WorkOriginHandle> {
    // Loopback is the auth: the listener takes no other interface, and the
    // table entry is `Admit::Local`, so no dialer reaches it through the
    // endpoint either.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;
    let kind = origin.executor.kind().clone();
    let serve = tokio::spawn(async move {
        if let Err(e) = host_kit::shell::serve(
            [listener],
            vec![router(origin)],
            std::future::pending::<()>(),
        )
        .await
        {
            warn!(target: TRACE_TARGET, error = %e, "work origin: the listener stopped");
        }
    });
    let registration = OriginRegistration {
        alpn: exec_slot(&kind),
        prefixes: Vec::new(),
        port: addr.port(),
        admit: Admit::Local,
        framing: Framing::Http,
        ttl_secs: Some(ORIGIN_TTL_SECS),
        claims: None,
        namespaces: Vec::new(),
    };
    info!(target: TRACE_TARGET, %addr, slot = %registration.alpn, rails = %rails_base,
          "work origin: serving; registering it with cw-rails");
    // The one register/renew loop, shared with serve's origins
    // (pb-serve-distributes-standalone).
    let register = tokio::spawn(sovereign_turn_client::rails_origins::keep_registered(
        rails_base,
        registration,
        ORIGIN_TTL_SECS,
        ORIGIN_RENEW_EVERY,
    ));
    Ok(WorkOriginHandle {
        addr,
        serve,
        register,
    })
}

#[cfg(test)]
#[path = "work_origin/tests.rs"]
mod tests;
