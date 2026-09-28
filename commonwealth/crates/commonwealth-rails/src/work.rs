// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `work` plane's read door — `GET /v1/work/projection` (five-programs
//! fp-45, §12 decision 2).
//!
//! The queue is a fold over the `work` journal ("there is no queue server …
//! the queue is a fold over Admission" — commonwealth-work's own doc), and
//! since fp-54 that journal lives under THIS process's root. So the fold runs
//! here: roster through the same derivation the roster door answers with,
//! admission, then `WorkProjection::fold` — the one fold. A donor receives
//! the folded queue and asks `may_take` / `lease_state` of it; it never reads
//! the admission itself.
//!
//! Absence is reported, never defaulted: a roster or journal that will not
//! answer is a 500 naming why, never an empty projection — an empty queue and
//! an unreadable one are different facts to a donor deciding what to take.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use commonwealth_rail::Ed25519Verifier;
use commonwealth_work::projection::WorkProjection;
use commonwealth_work::WORK_NAMESPACE;

use crate::rail::err;
use crate::RailsDaemon;

/// GET /v1/work/projection — the `work` namespace, folded now.
pub async fn projection(State(daemon): State<Arc<RailsDaemon>>) -> Response {
    let journal = match daemon.rail.journal(WORK_NAMESPACE) {
        Ok(j) => j,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let roster = match daemon.rail.roster(&journal).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = %e, "work projection: the `work` roster is unreadable");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the `work` roster is unreadable: {e}"),
            );
        }
    };
    let admission = match journal.admit(&roster, &Ed25519Verifier) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "work projection: the `work` journal would not admit");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the `work` journal would not admit: {e}"),
            );
        }
    };
    let proj: WorkProjection = commonwealth_work::projection::fold(&admission);
    Json(proj).into_response()
}
