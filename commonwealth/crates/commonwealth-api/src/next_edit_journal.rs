// SPDX-License-Identifier: AGPL-3.0-or-later
//! Daemon side of the next-edit journal: build the record, append it
//! without ever letting that failure reach the developer, and accept the
//! editor's outcome report.
//!
//! The record model, the file layout and the honesty rules for the
//! counts live in `sovereign_core::types::next_edit_journal` (one
//! schema, shared with `svrn journal`). What lives HERE is the daemon's
//! policy on top of it, which is entirely about invisibility:
//!
//! - **Nothing in this module may fail a request.** The append runs
//!   off-thread and its errors become one `tracing::warn!`. The outcome
//!   route answers `204` and is not something the extension checks.
//! - **The editor must never learn that journaling is off.** A disabled
//!   journal still gets an `episode_id` on the response and still
//!   accepts outcome POSTs; the writes are simply dropped. An extension
//!   that could tell would grow a branch, and the branch would grow a
//!   message (decision note `09599af1`).
//!
//! # The extraction allowlist
//!
//! [`episode_from`] reads the model lane's debug value by NAMED KEY, and
//! the set of names is fixed here: `reason`, `skipped`, `dropped`,
//! `region_bytes`, `suppress_thinking`, `timings_ms.inference`. Every
//! one of those is a token the daemon itself chose from a closed set, or
//! a number. The keys deliberately NOT read are the code-bearing ones —
//! `needle`, `verify_hunk`, `rule_find`, `rule_replace`, `region` — and
//! a new debug field is invisible to the journal until someone adds its
//! name here, which is the review this design wants to force.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use sovereign_core::types::{
    journal_dir, JournalLine, JournalStream, NextEditEpisode, NextEditOutcome, NextEditOutcomeLine,
    NEXT_EDIT_STREAM,
};

use crate::state::AppState;

/// Assemble the episode record for one prediction.
///
/// Pure — no clock beyond the one `NextEditEpisode::new` stamps, no IO.
/// The offline scorer runs this same construction (it shares
/// `predict_response`) and simply never appends the result, which is how
/// a bench run stays out of a developer's acceptance numbers.
#[allow(clippy::too_many_arguments)]
pub fn episode_from(
    engine: &str,
    proposed: usize,
    support: usize,
    sites: usize,
    reason_silent: Option<&str>,
    language: Option<&str>,
    path: Option<&str>,
    slot: Option<(&str, &str, &str, bool)>,
    model_debug: Option<&serde_json::Value>,
    total_ms: u64,
) -> NextEditEpisode {
    let mut e = NextEditEpisode::new(engine, proposed, total_ms);
    e.support = support;
    e.sites = sites;
    e.silent = reason_silent.map(str::to_string);
    e.language = language.map(str::to_string);
    // The extension only, never the path — see `ext_of`.
    e.path_ext = NextEditEpisode::ext_of(path);
    if let Some((model_id, slot_name, format, degraded)) = slot {
        e.model_id = Some(model_id.to_string());
        e.slot = Some(slot_name.to_string());
        e.format = Some(format.to_string());
        e.degraded = Some(degraded);
    }
    if let Some(d) = model_debug {
        // Named keys only. See the module docs for why the list is
        // closed and where the code-bearing keys are.
        e.reason = d["reason"].as_str().map(str::to_string);
        e.skipped = d["skipped"].as_str().map(str::to_string);
        e.dropped = d["dropped"].as_str().map(str::to_string);
        e.region_bytes = d["region_bytes"].as_u64();
        e.suppress_thinking = d["suppress_thinking"].as_bool();
        e.inference_ms = d["timings_ms"]["inference"].as_u64();
    }
    e
}

/// Append a line to one of the developer's journals, off the request
/// path.
///
/// Fire-and-forget by construction: the handle is dropped, so no caller
/// can accidentally make a request wait on a disk write, and no error
/// here can become a response. A failure is one `warn` — visible to
/// whoever is reading daemon logs, invisible to the person typing.
///
/// Generic over the stream and the line type on purpose. This wrapper is
/// not next-edit policy, it is DAEMON policy — "a journal write may
/// never affect a request" holds for every feature that keeps one, and
/// the second such feature should reach for this rather than copy the
/// `spawn_blocking` + swallow-into-`warn` shape and get one of the two
/// halves subtly wrong.
pub fn record<T: serde::Serialize + Send + 'static>(stream: JournalStream, line: T) {
    let dir = journal_dir();
    tokio::task::spawn_blocking(move || match stream.append(&dir, &line) {
        // `false` is the switched-off (or at-cap) posture, not a failure.
        Ok(_) => {}
        Err(e) => tracing::warn!(
            target: "journal",
            stream = stream.stem,
            error = %e,
            dir = %dir.display(),
            "could not write the journal; the feature itself was unaffected"
        ),
    });
}

/// `record` for this lane, so call sites do not repeat the stream.
pub fn record_next_edit(line: JournalLine) {
    record(NEXT_EDIT_STREAM, line);
}

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
pub async fn edit_prediction_outcome(
    State(_state): State<AppState>,
    Json(wire): Json<OutcomeWire>,
) -> Response {
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
    record_next_edit(JournalLine::Outcome(NextEditOutcomeLine::new(
        wire.episode_id,
        outcome,
    )));
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The debug value the model lane produces on its richest path,
    /// including every code-bearing key the journal must not read.
    fn full_debug() -> serde_json::Value {
        serde_json::json!({
            "consulted": true,
            "reason": "param_insert",
            "needle": "fn CANARY_NEEDLE(",
            "model_id": "some-model",
            "slot": "edit",
            "format": "region_instruct",
            "region": { "start": 10, "end": 400 },
            "region_bytes": 390,
            "needle_hit": true,
            "suppress_thinking": true,
            "timings_ms": { "inference": 812 },
            "verify_hunk": { "old": "CANARY_OLD_CODE", "new": "CANARY_NEW_CODE" },
        })
    }

    /// The watched failure: if the allowlist ever widens to a
    /// code-bearing key, this is what catches it — the canary shows up
    /// in the serialized line.
    #[test]
    fn debug_extraction_carries_no_code() {
        let dbg = full_debug();
        let e = episode_from(
            "model",
            2,
            3,
            4,
            None,
            Some("rust"),
            Some("/Users/dev/secret-project/CANARY_PATH/main.rs"),
            Some(("some-model", "edit", "region_instruct", false)),
            Some(&dbg),
            1234,
        );
        let line = serde_json::to_string(&JournalLine::Episode(e.clone())).unwrap();
        for canary in [
            "CANARY_NEEDLE",
            "CANARY_OLD_CODE",
            "CANARY_NEW_CODE",
            "CANARY_PATH",
            "secret-project",
        ] {
            assert!(
                !line.contains(canary),
                "journal line leaked `{canary}`: {line}"
            );
        }
        // ...while still carrying everything the counts need.
        assert_eq!(e.reason.as_deref(), Some("param_insert"));
        assert_eq!(e.region_bytes, Some(390));
        assert_eq!(e.suppress_thinking, Some(true));
        assert_eq!(e.inference_ms, Some(812));
        assert_eq!(e.path_ext.as_deref(), Some("rs"));
        assert_eq!(e.model_id.as_deref(), Some("some-model"));
        assert!(e.fired);
    }

    #[test]
    fn rule_only_episode_has_no_model_facts() {
        let e = episode_from(
            "rule",
            0,
            1,
            0,
            Some("below_threshold"),
            None,
            Some("a.tsx"),
            None,
            None,
            7,
        );
        assert!(!e.fired);
        assert_eq!(e.silent.as_deref(), Some("below_threshold"));
        assert_eq!(e.model_id, None);
        assert_eq!(
            e.degraded, None,
            "absent is not `false` — no slot was consulted at all"
        );
        assert_eq!(e.inference_ms, None);
        assert_eq!(e.path_ext.as_deref(), Some("tsx"));
    }

    #[test]
    fn degraded_fallback_is_distinguishable_from_a_chosen_edit_model() {
        let chosen = episode_from(
            "model",
            1,
            2,
            1,
            None,
            None,
            None,
            Some(("m", "edit", "region_instruct", false)),
            None,
            5,
        );
        let fallback = episode_from(
            "model",
            1,
            2,
            1,
            None,
            None,
            None,
            Some(("m", "fast", "region_instruct", true)),
            None,
            5,
        );
        assert_eq!(chosen.degraded, Some(false));
        assert_eq!(fallback.degraded, Some(true));
    }

    #[test]
    fn every_outcome_spelling_the_extension_sends_is_accepted() {
        for o in NextEditOutcome::ALL {
            assert_eq!(NextEditOutcome::from_wire(o.as_str()), Some(o));
        }
    }
}
