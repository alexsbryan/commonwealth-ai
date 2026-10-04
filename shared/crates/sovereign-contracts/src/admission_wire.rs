// SPDX-License-Identifier: AGPL-3.0-or-later
//! Admission's wire half: the reason a caller was shed, its `Retry-After`
//! hint, and the one 503 renderer. svrn's admission layers and serve's local
//! queue shed both answer through it (pb-svrn-serving-ports, §12 3a rung 2).
//! It renders to `http::Response<String>`, the types crate axum re-exports, so
//! this leaf names no server; each host's axum wrapper is `.into_response()`.

use http::{header::CONTENT_TYPE, header::RETRY_AFTER, Response, StatusCode};
use oicp_types::openai_types::ErrorDetail;
use serde::Serialize;

/// Why a request was rejected. Serialised in the 503 body and in tracing
/// spans so contention triage doesn't require log spelunking.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionReason {
    /// Operator-initiated runtime pause is active.
    Paused,
    /// Foreground-yield window: local user has activity in flight, peer work
    /// would contend with their chat.
    YieldedToLocal,
    /// At-or-above the configured ceiling for concurrent peer requests.
    CeilingExceeded,
    /// This node's own slot refused BEFORE parking the caller: predicted wait
    /// exceeded the queue bound. Distinct from `CeilingExceeded`, which counts
    /// concurrent PEER requests — this one is about how long the caller would
    /// have waited in THIS node's queue, regardless of who sent the turn.
    LocalQueueFull,
    /// The calling principal already holds its equal share of the host's
    /// concurrency while other principals are active. Distinct from every
    /// reason above: it says nothing about how busy the host is, only that
    /// THIS caller is ahead of its neighbours. A host with idle capacity still
    /// returns this — that is the point, and it is why it is not a shed
    /// (`MESH_SCALE_100_USERS_1000_CORPORA.md` §7.1 R2).
    PrincipalShareExceeded,
}

impl AdmissionReason {
    /// The stable machine-readable string for this reason, used as the OpenAI
    /// `error.code` and as the top-level `reason`. Kept in sync with the
    /// `rename_all = "snake_case"` serde attribute above by the
    /// `admission_reason_code_matches_serde` test — two spellings of one name
    /// is the §10.6 smell, and this is the pair most likely to drift.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Paused => "paused",
            Self::YieldedToLocal => "yielded_to_local",
            Self::CeilingExceeded => "ceiling_exceeded",
            Self::LocalQueueFull => "local_queue_full",
            Self::PrincipalShareExceeded => "principal_share_exceeded",
        }
    }
}

/// How many seconds of spread a shed's `Retry-After` hint carries on top of its
/// base value.
///
/// WHY THIS IS NOT ZERO. A constant hint is a synchronized-retry generator:
/// every client shed inside the same busy window is told to come back at the
/// same instant, so the load that produced the shed re-arrives as a single
/// spike instead of a ramp — and the spike sheds the same population again, in
/// lockstep, forever. This is the classic thundering-herd retry loop, and at
/// 100 clients against one concurrent turn (`MESH_SCALE_100_USERS_1000_CORPORA.md`
/// §7.4 item 2) it is the difference between a queue that drains and one that
/// oscillates. Four seconds on a 2s base spreads the herd over 3× the base
/// window while keeping the worst-case hint inside the range a client's own
/// backoff would have chosen anyway.
pub const RETRY_AFTER_JITTER_SPREAD_SECS: u64 = 4;

/// The jitter function itself, pure and therefore testable: `base` plus
/// `entropy mod spread`. Split out from the entropy SOURCE so the spread policy
/// has exactly one implementation and one name (§10.6) no matter which shed
/// path renders the hint.
pub fn jitter_retry_after(base: u64, entropy: u64) -> u64 {
    base.saturating_add(entropy % RETRY_AFTER_JITTER_SPREAD_SECS)
}

/// Production entry point: `base` seconds, jittered.
///
/// Entropy is a process-local counter mixed with the wall clock's nanosecond
/// field. The counter guarantees that two sheds from the SAME process never
/// land on the same offset back-to-back (which a coarse clock would otherwise
/// allow); the nanoseconds guarantee that two processes shedding in the same
/// instant do not share a phase. Neither alone is sufficient, which is why both
/// are mixed.
pub fn jittered_retry_after_secs(base: u64) -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = sovereign_time::system_now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0);
    // splitmix64 finalizer — cheap avalanche so the low bits of the
    // counter/nanos mix don't hand out a sawtooth.
    let mut z = counter
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(nanos);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    let entropy = z ^ (z >> 31);
    jitter_retry_after(base, entropy)
}

/// 503 body the admission layer returns to a rejected caller.
/// `retry_after_secs` mirrors the `Retry-After` header value.
///
/// `error` is the OpenAI error OBJECT, not a bare string: this route is
/// advertised as OpenAI-compatible, so a shed serialised as a plain string put
/// the one message that has to survive ("busy, come back in 30s") out of reach
/// of every SDK on the route. Reuses [`ErrorDetail`] rather than minting a
/// second error shape (§10.6).
///
/// `reason` and `retry_after_secs` stay TOP-LEVEL and unchanged — the peer load
/// balancer and `deep_research`'s shed classifier key off them, the latter by
/// substring — so widening `error` is additive.
#[derive(Debug, Clone, Serialize)]
pub struct AdmissionRejection {
    /// OpenAI-shaped error object. `code` carries the same value as `reason` so
    /// a client that only understands the OpenAI envelope still gets the
    /// precise cause.
    pub error: ErrorDetail,
    pub reason: AdmissionReason,
    pub retry_after_secs: u64,
}

impl AdmissionRejection {
    /// Build a rejection from the human-readable cause. The OpenAI `type` is
    /// the coarse bucket (`server_error`) and `code` is the precise reason,
    /// which is the split the OpenAI error contract asks for.
    pub fn new(message: impl Into<String>, reason: AdmissionReason, retry_after_secs: u64) -> Self {
        Self {
            error: ErrorDetail {
                message: message.into(),
                error_type: "server_error".to_string(),
                code: Some(reason.as_str().to_string()),
            },
            reason,
            retry_after_secs,
        }
    }
}

/// The ONE place a shed becomes an HTTP response: 503 + `Retry-After` + the
/// structured body. Both the peer-admission middleware and the local queue-shed
/// path in `routes_inference` render through here.
///
/// Why this is a function rather than two call sites that each build a
/// response: a shed is backpressure, and a client that receives it as an
/// untyped `backend_error` cannot tell "busy, come back in 35s" from
/// "something crashed". That was note `bef03728`'s open gap, and the 2026-08-07
/// live fleet probe turned it into an observed failure — the caller got
/// `{"type":"backend_error"}` carrying its retry hint only inside a prose
/// message, with no `Retry-After` header.
///
/// A local queue shed, rendered. Both chat entry points (streaming and
/// non-streaming) call this so the body and header are built in exactly one
/// place rather than once per route.
pub fn local_queue_shed_response(
    position: u32,
    predicted_wait_ms: u64,
    retry_after_secs: u64,
) -> Response<String> {
    shed_response(AdmissionRejection::new(
        format!("host busy: ~{predicted_wait_ms} ms predicted wait at queue position {position}"),
        AdmissionReason::LocalQueueFull,
        retry_after_secs,
    ))
}

/// Render a rejection as 503 + `Retry-After` + the structured JSON body.
pub fn shed_response(rejection: AdmissionRejection) -> Response<String> {
    let retry_after = rejection.retry_after_secs;
    let body = match serde_json::to_string(&rejection) {
        Ok(body) => body,
        Err(e) => {
            // What axum's `Json` answered before this renderer: a 500 naming
            // the serialisation error, never an empty 503.
            tracing::warn!(error = %e, "admission: shed body did not serialise");
            let mut response = Response::new(e.to_string());
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            return response;
        }
    };
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    headers.insert(RETRY_AFTER, http::HeaderValue::from(retry_after));
    response
}

#[cfg(test)]
mod tests;
