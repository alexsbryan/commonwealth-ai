// SPDX-License-Identifier: AGPL-3.0-or-later
//! Busy-host response builders — the REST side of "host saturation is
//! legible." When the [`crate::scheduler::FairScheduler`] sheds a turn
//! (the queue is full, or a one-shot REST request can't be granted now),
//! the host answers `503 + Retry-After` rather than queueing unboundedly —
//! which to a phone client would read as a hang, not a busy host.
//!
//! The slot accounting + fairness moved to [`crate::scheduler`]; this module
//! is now just the two response shapes a shed produces.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

/// Build the REST `503 Service Unavailable + Retry-After` response a busy
/// host returns. Body carries `{ error, retry_after, queue_position }` so the
/// client renders the "host busy" state, paces its retry off `Retry-After`,
/// and can show the position the request *would* have occupied ("busy · ~#3
/// in line"). `queue_position` is 1-based; `0` means "no position available."
pub fn busy_response_hint(retry_after_secs: u64, queue_position: u32) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, retry_after_secs.to_string())],
        axum::response::Json(serde_json::json!({
            "error": "host busy",
            "retry_after": retry_after_secs,
            "queue_position": queue_position,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_response_hint_carries_retry_after() {
        let resp = busy_response_hint(7, 3);
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(resp.headers().get(header::RETRY_AFTER).unwrap(), "7");
    }
}
