// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /v1/edit_predictions/outcome` — the host route shell for the
//! developer's report on a suggestion.
//!
//! The append policy itself lives in `code_next_edit::next_edit_journal`
//! ("a journal write may never affect a request"); this is the axum handler
//! that mounts it. DAEMON_CORE.md §4.1's placement test puts a route shell
//! with the surface that serves it, not with the package crate, so it split
//! out of `next_edit_journal.rs` at `dm-next-edit-move` and the pure policy
//! left for `code-next-edit`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

/// The body of `POST /v1/edit_predictions/outcome`.
#[derive(Debug, Deserialize)]
pub struct OutcomeWire {
    /// The `episode_id` the prediction response carried.
    pub episode_id: String,
    /// One of `accepted` | `dismissed` | `diverged` | `superseded`.
    pub outcome: String,
}

/// POST /v1/edit_predictions/outcome — what the developer did with a
/// suggestion.
///
/// Answers `204` on success and `400` on a malformed body. Both are
/// invisible: the extension posts and ignores the result, and an older
/// daemon that 404s this route costs nothing but an unreported episode
/// (counted as `unknown`, never as `dismissed`).
///
/// The 400 is not a user-facing error — it is a *contract* check, and it
/// exists because the alternative is worse. An unrecognized outcome
/// string quietly coerced to `dismissed` would corrupt the single number
/// this whole subsystem exists to produce, so an unknown value is
/// refused rather than substituted (ARCH §18.3).
///
/// The append policy itself is `code_next_edit::next_edit_journal`'s; this
/// is the route shell that mounts it (DAEMON_CORE.md §4.1's placement test).
pub async fn edit_prediction_outcome(Json(wire): Json<OutcomeWire>) -> Response {
    use sovereign_core::types::{JournalLine, NextEditOutcome, NextEditOutcomeLine};
    let Some(outcome) = NextEditOutcome::from_wire(&wire.outcome) else {
        tracing::debug!(
            target: "next_edit",
            outcome = %wire.outcome,
            "rejected an unrecognized next-edit outcome"
        );
        return StatusCode::BAD_REQUEST.into_response();
    };
    if wire.episode_id.trim().is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    tracing::debug!(
        target: "next_edit",
        episode = %wire.episode_id,
        outcome = outcome.as_str(),
        "next-edit outcome"
    );
    code_next_edit::next_edit_journal::record_next_edit(JournalLine::Outcome(
        NextEditOutcomeLine::new(wire.episode_id, outcome),
    ));
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// covers: IN-27
    ///
    /// Outcome reporting adds ZERO user-visible surface. The editor extension
    /// posts and drops the result, so the only way this route can hurt a
    /// developer is by doing something an ignore-the-result caller cannot
    /// ignore: hanging, panicking (a 500 that some HTTP clients surface), or
    /// answering with a status the extension does not already treat as
    /// nothing-happened.
    ///
    /// The 400 half is the other guarantee, and it is the reason the route
    /// cannot just accept anything: an unrecognised outcome coerced to
    /// `dismissed` would corrupt the single number this subsystem exists to
    /// produce, so it is refused rather than substituted (ARCH §18.3).
    /// Driven over a router because the status code IS the contract.
    #[tokio::test]
    async fn the_outcome_route_answers_204_or_400_and_never_5xx() {
        use axum::routing::post;

        let router = axum::Router::new().route(
            "/v1/edit_predictions/outcome",
            post(edit_prediction_outcome),
        );

        async fn post_body(router: &axum::Router, body: &str) -> StatusCode {
            router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/edit_predictions/outcome")
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .expect("the route must always answer")
                .status()
        }

        // Every outcome the extension can report. All four must be accepted
        // by name — one silently rejected here is an episode counted as
        // `unknown` forever, which is the measurement this lane produces.
        for outcome in ["accepted", "dismissed", "diverged", "superseded"] {
            let status = post_body(
                &router,
                &format!(r#"{{"episode_id":"ep-1","outcome":"{outcome}"}}"#),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::NO_CONTENT,
                "`{outcome}` is a recognised outcome and must be accepted"
            );
        }

        // A contract violation is a 400 — refused, not coerced.
        for body in [
            r#"{"episode_id":"ep-1","outcome":"maybe"}"#,
            r#"{"episode_id":"ep-1","outcome":""}"#,
            r#"{"episode_id":"","outcome":"accepted"}"#,
            r#"{"episode_id":"   ","outcome":"accepted"}"#,
        ] {
            let status = post_body(&router, body).await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "a malformed report must be refused, never coerced: {body}"
            );
        }

        // Nothing the extension can send produces a 5xx, which is the class
        // an editor is entitled to surface to the developer.
        for body in [
            r#"{"episode_id":"ep-1","outcome":"accepted"}"#,
            r#"{"episode_id":"ep-1","outcome":"maybe"}"#,
            r#"{"not":"even the right shape"}"#,
            r#"not json at all"#,
        ] {
            let status = post_body(&router, body).await;
            assert!(
                !status.is_server_error(),
                "an advisory journal write must never answer 5xx: {body} -> {status}"
            );
        }
    }
}
