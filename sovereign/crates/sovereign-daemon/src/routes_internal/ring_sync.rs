// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /internal/ring/sync` — one anti-entropy exchange for one ring
//! namespace.
//!
//! # The shape, and why it is one route
//!
//! The caller sends what it holds (a per-actor contiguous high-water
//! [`Digest`]) and, optionally, ops it believes the responder lacks. The
//! responder ingests those, then answers with its own digest and as much of
//! what the caller's digest says it is missing as fits one budget. Two calls
//! converge both directions: the first learns the peer's digest, the second
//! delivers against it. Both are idempotent, so a dropped call costs one round
//! and never a duplicate entry.
//!
//! # Both directions are budgeted, and neither is capped
//!
//! `ops` on the way in and `ops` on the way out are each stopped at
//! [`RING_SYNC_OPS_BUDGET_BYTES`], and the sender repeats the exchange until
//! nothing moves. Nothing on the wire changed shape to make that work — the
//! exchange was always idempotent, so a partial one is safe.
//!
//! Before that, one exchange carried the whole selection and the receiver's
//! `DefaultBodyLimit` refused it at ~9,599 ops of the measured fixture. The
//! refusal was answered at the extractor, so this handler never ran: no gauge
//! fired, the sender filed the 413 as an unreachable peer, and the peer that
//! had been refused the journal reported zero ops, zero gaps and a COMPLETE
//! ring. A budget is the fix a bigger limit would only have postponed.
//!
//! # Nothing here validates an op, and that is the design
//!
//! Its deleted sibling `/internal/app/state` validated nothing about an entry
//! either, and there it mattered: an entry was a bare `(app_id, key, value,
//! timestamp, origin)` and the receiver had only the sender's word for any of
//! it, which is why that route had to carry a hand-written privacy check. Here
//! an op carries its author in an Ed25519 signature over a message that binds
//! the namespace, so
//! a forged or replayed op does not become a balance: it becomes a
//! [`RailGap`](commonwealth_rail::RailGap) the next time anybody
//! folds. Checking here instead would put a second answer beside the fold's
//! (ARCH §10.6), and the fold's is the one that has to be right anyway,
//! because ops also arrive from disk.
//!
//! This port is reachable by any peer that can route to this host (see the
//! module header on `routes_internal`), so "who may write to my journal" is a
//! question the signature answers and the listener cannot.
//!
//! # …but "who may READ my journal" is a question only this route can answer
//!
//! A signature binds an op to its author. It says nothing about who is
//! *asking*, and this route answers with one budget of the journal — so a mesh
//! member who is on no ring's roster could read every ring on this host by
//! asking for it by name. Since `mp-2` it cannot: the route reads the verified
//! principal `crate::internal_principal` attached (the key the iroh handshake
//! proved, never a header the caller typed) and refuses a namespace whose
//! roster does not name that key.
//!
//! The roster comes from `RingRail::roster`, the rail's one reader, so a ring
//! with no `roster.json` — every app ring by default, and the seven
//! `REGISTERED_NAMESPACES` — still admits every member, and the file-rostered
//! work plane narrows. The sender applies the SAME test before it offers
//! (`sovereign_mesh::ring_roster::roster_names`); this half is what makes the
//! filter a rule rather than a courtesy, because a peer that skips its own
//! filter still has to get past this one.
//!
//! An asker this daemon could not verify (`Principal::Unverified` — it
//! presented an identity on a connection with no acceptor in front) is refused
//! every namespace. An asker that presented NOTHING is `Principal::Anonymous`,
//! and is served: on an encrypted mesh that is a local process reaching a
//! loopback-only port, and on a plaintext one it is every peer, because a
//! plaintext mesh has no verified key to decide on at all. **That is an open
//! gap, not a design** — it is the same posture dependence `internal_principal`
//! records, and it is why the encrypted posture is the one the room runs.
//!
//! The one thing a signature cannot answer is "may this namespace exist on my
//! machine at all", and that is not asked here either: a peer may put a
//! `notes-private` journal on our disk and this route will take it. It reaches
//! no reader, because `MeshStore::apply_projection` refuses an excluded
//! namespace and the store is the only thing anything reads
//! (`sovereign-mesh::ring_sync`'s
//! `a_peers_private_namespace_is_taken_by_the_rail_and_refused_by_the_projection`).
//! That is the guard this route's deleted sibling used to carry inline.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::state::AppState;
use sovereign_peer_wire::{RingSyncRequest, RingSyncResponse, RING_SYNC_OPS_BUDGET_BYTES};

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// Why this asker may not have this namespace, or `None` to serve it.
///
/// ONE decider, returning the sentence the refusal carries so the wire body
/// and the tracing event cannot say different things. It names both halves —
/// the namespace and the asker — because "forbidden" on a route that serves
/// seven rings tells whoever reads the log nothing at all.
///
/// The three arms, and why each is the answer:
///
/// - a verified [`Principal::Member`] is asked of the roster by the KEY
///   membership names it with, through the one test the sender uses;
/// - [`Principal::Unverified`] is refused outright — it claimed an identity
///   this daemon could not tie to its own acceptor, and a ring cannot check a
///   roster against a claim (ARCH principle 6: that is not "answered: no", it
///   is "did not answer", and a journal is not handed over on a shrug);
/// - anything else claimed nothing. See the module header: on the encrypted
///   posture that is a local process on a loopback-only port, and on a
///   plaintext mesh it is every peer, which is a disclosed gap.
///
/// An unreadable roster refuses. Under-share, never over-share — the same
/// posture `ring_sync`'s prune takes when it cannot read one.
async fn roster_refusal(
    state: &AppState,
    attached: &Option<axum::Extension<sovereign_serving_host::admission::AttachedPrincipal>>,
    rail: &commonwealth_rail::RingRail,
    journal: &commonwealth_rail::RingJournal,
) -> Option<String> {
    use sovereign_serving_host::admission::Principal;

    let namespace = journal.namespace();
    // An ABSENT extension is a request that reached this handler with no
    // resolver in front — not a caller that presented nothing. It reads as
    // `Anonymous` here for one reason and it is named rather than assumed:
    // `crate::admission::requester` is the one place this absence was already
    // decided, and it decides it the same way. On the internal router the case
    // is unreachable — `internal_principal_layer` is that router's outermost
    // layer (`server.rs`), so every request through it carries the extension
    // — and the only other callers are tests driving the handler directly.
    let who = attached
        .as_ref()
        .map(|axum::Extension(a)| a.0.clone())
        .unwrap_or(Principal::Anonymous);
    let asker = match who {
        Principal::Member { node_id } => node_id,
        Principal::Unverified => {
            tracing::warn!(
                namespace,
                "ring sync: refused — the asker claimed a peer identity this \
                 node could not verify, and a roster cannot be checked against \
                 a claim"
            );
            return Some(format!(
                "{namespace} is served to its roster, and this caller's identity \
                 could not be verified"
            ));
        }
        _ => {
            tracing::debug!(
                namespace,
                "ring sync: the asker presented no mesh identity — served, and \
                 checked against no roster"
            );
            return None;
        }
    };

    let roster = match rail.roster(journal).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                namespace,
                asker = %asker,
                error = %e,
                "ring sync: refused — this namespace's roster is unreadable, so \
                 nobody can be shown to be on it"
            );
            return Some(format!("{namespace}'s roster is unreadable on this node"));
        }
    };
    let key = state.member_pubkey(asker).await;
    if sovereign_mesh::ring_roster::roster_names(&roster, key) {
        tracing::debug!(namespace, asker = %asker, "ring sync: the roster names the asker");
        return None;
    }
    tracing::warn!(
        namespace,
        asker = %asker,
        keyed = key.is_some(),
        "ring sync: refused — this ring's roster does not name the asker"
    );
    Some(format!("{asker} is not on {namespace}'s roster"))
}

/// The body arrives as raw [`Bytes`] rather than `Json<RingSyncRequest>` for
/// exactly one reason: **the gauge below has to read the direction that can
/// fail.** `DefaultBodyLimit` bounds the REQUEST; the response has no cap at
/// all. Measuring the deserialised struct back would be a second answer to
/// "how big was this" (ARCH §10.6) and would not be the number the extractor
/// compared against anyway.
pub async fn ring_sync(
    State(state): State<AppState>,
    attached: Option<axum::Extension<sovereign_serving_host::admission::AttachedPrincipal>>,
    body: axum::body::Bytes,
) -> Response {
    // Read before anything can fail, so a 400 still carries the size.
    let request_bytes = body.len();
    let req: RingSyncRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_REQUEST, format!("malformed body: {e}")),
    };
    let Some(rail) = state.ring_rail() else {
        // A node with no ring storage cannot participate. Refusing is the
        // honest answer; a 200 with an empty digest would tell the peer this
        // node holds nothing, and it would stop offering ops (ARCH §18.3).
        return err(
            StatusCode::SERVICE_UNAVAILABLE,
            "this node has no ring storage installed",
        );
    };
    let journal = match rail.journal(&req.namespace) {
        Ok(l) => l,
        Err(e) => return err(StatusCode::BAD_REQUEST, e.to_string()),
    };

    // ── A ring is SERVED to its roster, and to nobody else.
    //
    // Before anything is ingested or selected: a refusal that ran after the
    // ingest would have taken the asker's ops onto this disk on its way to
    // saying no.
    if let Some(refusal) = roster_refusal(&state, &attached, &rail, &journal).await {
        return err(StatusCode::FORBIDDEN, refusal);
    }

    let ingested = match journal.ingest_all(&req.ops) {
        Ok(n) => n,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    // Budgeted in BOTH directions. The response was the unbounded half —
    // the pull direction converged at a size where the identical peer being
    // pushed to was refused — and an unbounded body is also an unbounded
    // allocation on a route any peer that can route here may call.
    let selection = journal.ops_missing_from_within(&req.digest, RING_SYNC_OPS_BUDGET_BYTES);
    let (digest, (ops, more_for_caller)) = match (journal.digest(), selection) {
        (Ok(d), Ok(o)) => (d, o),
        (Err(e), _) | (_, Err(e)) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let body = RingSyncResponse {
        namespace: req.namespace.clone(),
        digest,
        ops,
        ingested,
    };
    let bytes = match serde_json::to_vec(&body) {
        Ok(b) => b,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    tracing::debug!(
        namespace = %req.namespace,
        request_bytes,
        offered = req.ops.len(),
        ingested,
        sending = body.ops.len(),
        more_for_caller,
        payload_bytes = bytes.len(),
        "ring sync: exchange"
    );
    // **The gauge watches the request, because the request is the direction
    // with a limit.** It read `bytes.len()` — the response — until 2f, which
    // meant the rail's one instrument watched the half that cannot fail.
    //
    // A caller whose body reached the budget filled a whole chunk, so it has
    // more to send and will be back this round: that is the named trigger for
    // the checkpoint work the journal defers, and it is now also how an
    // UNBUDGETED sender (an older build, which puts its whole selection in
    // one body) becomes visible before the extractor refuses it.
    if request_bytes >= RING_SYNC_OPS_BUDGET_BYTES {
        tracing::warn!(
            namespace = %req.namespace,
            request_bytes,
            budget_bytes = RING_SYNC_OPS_BUDGET_BYTES,
            limit_bytes = crate::server::MAX_REQUEST_BODY_BYTES,
            offered = req.ops.len(),
            "ring sync: a caller filled its whole exchange budget — this ring \
             is carrying more history than one exchange holds, which is the \
             named trigger for journal checkpoints"
        );
    }
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response()
}
