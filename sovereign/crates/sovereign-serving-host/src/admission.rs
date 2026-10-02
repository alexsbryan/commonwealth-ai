// SPDX-License-Identifier: AGPL-3.0-or-later
//! Admission's wire at its historical path, and serve's axum face of the one
//! shed renderer. The wire half is `sovereign_contracts::admission_wire`; the
//! decision's ports and the middlewares are svrn's
//! (`sovereign_daemon::admission`), which alone mounts them
//! (pb-svrn-serving-ports).

use axum::response::{IntoResponse, Response};
use sovereign_contracts::admission_wire;

pub use sovereign_contracts::admission_wire::{
    jitter_retry_after, jittered_retry_after_secs, AdmissionReason, AdmissionRejection,
    RETRY_AFTER_JITTER_SPREAD_SECS,
};
pub use sovereign_contracts::principal::{AttachedPrincipal, Principal};

/// serve's axum face of `admission_wire::shed_response`.
pub fn shed_response(rejection: AdmissionRejection) -> Response {
    admission_wire::shed_response(rejection).into_response()
}

/// serve's axum face of `admission_wire::local_queue_shed_response`.
pub fn local_queue_shed_response(
    position: u32,
    predicted_wait_ms: u64,
    retry_after_secs: u64,
) -> Response {
    admission_wire::local_queue_shed_response(position, predicted_wait_ms, retry_after_secs)
        .into_response()
}
