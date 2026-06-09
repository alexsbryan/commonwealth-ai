// SPDX-License-Identifier: AGPL-3.0-or-later
//! Server-level busy guard — bounds concurrent inference turns and
//! makes host saturation *legible* as `503 + Retry-After` (REST) or a
//! busy stream-error frame (WebSocket). This is the "busy host is
//! legible" acceptance criterion (`MOBILE.md` §6).
//!
//! There is no host-busy signal in the inference layer today (mesh
//! load-awareness is designed but unimplemented — see
//! `docs/MESH_LOAD_AWARENESS.md`). So this guard is the honest,
//! glassbox mechanism: one permit per in-flight turn, sized by
//! `[server] max_concurrent_turns`. When permits are exhausted the host
//! says "busy, retry in N seconds" instead of queueing unboundedly —
//! which to a phone client would read as a hang, not a busy host.

use std::sync::Arc;

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Bounds concurrent turns. Cheap to clone — the permit pool is shared
/// via the inner `Arc<Semaphore>`. Installed as an Axum `Extension`.
#[derive(Clone)]
pub struct BusyGuard {
    sem: Arc<Semaphore>,
    retry_after_secs: u64,
}

/// Held for the duration of one turn; dropping it frees a slot.
pub type BusyPermit = OwnedSemaphorePermit;

impl BusyGuard {
    pub fn new(max_concurrent_turns: usize, retry_after_secs: u64) -> Self {
        // A zero budget would wedge the server (every turn 503s). Clamp
        // to at least 1 — an operator who wants the guard "off" sets a
        // high ceiling, not zero.
        let permits = max_concurrent_turns.max(1);
        Self {
            sem: Arc::new(Semaphore::new(permits)),
            retry_after_secs,
        }
    }

    /// Try to claim a turn slot without blocking. `None` means the host
    /// is at capacity — the caller must surface "busy" rather than wait.
    pub fn try_enter(&self) -> Option<BusyPermit> {
        Arc::clone(&self.sem).try_acquire_owned().ok()
    }

    /// Seconds to advertise in `Retry-After` / the busy stream frame.
    pub fn retry_after_secs(&self) -> u64 {
        self.retry_after_secs
    }

    /// Free slots remaining — glassbox for the `host_busy` log line.
    pub fn available(&self) -> usize {
        self.sem.available_permits()
    }
}

/// Build the REST `503 Service Unavailable + Retry-After` response a
/// busy host returns. Body is a JSON `{ "error": "host busy" }` so the
/// client can render the same error envelope as other failures while
/// the status + header drive the "host busy" state.
pub fn busy_response(retry_after_secs: u64) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, retry_after_secs.to_string())],
        axum::response::Json(serde_json::json!({ "error": "host busy" })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausts_then_recovers() {
        let guard = BusyGuard::new(1, 3);
        let p1 = guard.try_enter();
        assert!(p1.is_some(), "first permit granted");
        assert_eq!(guard.available(), 0);
        assert!(guard.try_enter().is_none(), "at capacity → busy");
        drop(p1);
        assert_eq!(guard.available(), 1);
        assert!(guard.try_enter().is_some(), "slot freed → granted again");
    }

    #[test]
    fn zero_clamps_to_one() {
        let guard = BusyGuard::new(0, 1);
        assert!(guard.try_enter().is_some(), "0 clamps to 1 usable slot");
    }

    #[test]
    fn busy_response_carries_retry_after() {
        let resp = busy_response(5);
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            resp.headers().get(header::RETRY_AFTER).unwrap(),
            "5",
            "Retry-After header present"
        );
    }
}
