// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /v1/guest/session` — a phone claims its name for this room.
//!
//! One QR code serves a room, so every phone in it presents the SAME grant.
//! The grant is the scope and the TTL; it cannot be the person. This route is
//! where the person is claimed: the guest types a name once, the door binds it
//! to an opaque session handle under that grant, and every later request
//! carries the handle so the door can name the guest without asking the page.
//!
//! **The claim is the door's, not the app's.** `window.ring`
//! ([`ring_shim`](crate::guest_door::ring_shim)) asks on the first call it
//! makes and exposes the result read-only — a page may greet a guest, it may
//! not choose one. Before this, the name rode the act's payload, which meant
//! the page authored the value the substrate then checked: a guard that reads
//! what the subject supplies is not a guard (ARCH 5), and an app that sent no
//! name at all had its guest's acts shown under the member's.
//!
//! **A session handle is not a second credential.** It is a second opaque
//! string the phone holds, and it names no scope:
//! [`GuestGrant::permits_path`](sovereign_grants::GuestGrant::permits_path)
//! remains the sole decider of what may be reached, so a session reaches
//! exactly what its grant reaches and nothing more. It also dies with the
//! grant — [`GuestSessionStore`] copies the grant's expiry rather than
//! computing a TTL of its own, and evaluates the grant's liveness on every
//! read. The guest posture in `docs/THREAT_MODEL.md` is therefore unchanged:
//! nothing here widens what a guest can reach, or how long for.
//!
//! **Two refusals, both 409, both at the moment the name is claimed.** A name
//! that is a roster member's ([`names_a_member`]) so a guest is never
//! mistakable for a member, and a name a live session under the same grant
//! already holds so two guests are never shown as one person. The member
//! refusal is the sentence `routes_rail::append` has spoken since the rail
//! shipped, moved to where the name is now claimed rather than re-worded.
//!
//! The member check runs over EVERY namespace this bearer reaches
//! (`routes_rail::reachable_namespaces`) — one app under a `Scope::Rails`
//! grant, the whole wall under a `Scope::Wall` one. It has to: a name accepted
//! on one app and refused on the next would make one person two, and the room
//! the guest is standing in is the wall, not one page of it.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;

use crate::client_auth::Guest;
use crate::state::AppState;

/// The one path this module serves. Named here and read by
/// `sovereign_grants::Scope::Rails::paths` so the mount and the allowlist
/// cannot drift apart silently.
pub const GUEST_SESSION_PATH: &str = "/v1/guest/session";

/// The header a phone presents its session handle in, read by
/// [`client_auth_layer`](crate::client_auth::client_auth_layer) and written by
/// the shim. One name, in one place: the browser cannot use
/// `Authorization` for it (that carries the grant) and a query parameter would
/// end up in logs and in the page's own URL.
pub const RING_SESSION_HEADER: &str = "x-ring-session";

/// Longest name the door will bind. A wall row has to be readable from across
/// a room; past this it is not a name.
const MAX_NAME_CHARS: usize = 40;

#[derive(Debug, Deserialize)]
pub struct ClaimRequest {
    /// What the guest typed, verbatim.
    pub name: String,
}

/// Whether `name` is a roster member's — so a guest is never shown under a
/// member's name. Case and surrounding space do not make a different name to a
/// reader.
///
/// THE implementation of that comparison. `routes_rail::append` calls this one
/// rather than keeping its own: two spellings of "is this a member's name" is a
/// decider with two answers (ARCH 8).
pub(crate) fn names_a_member(roster: &commonwealth_rail::Roster, name: &str) -> bool {
    let name = name.trim();
    roster
        .members
        .keys()
        .any(|p| p.as_str().eq_ignore_ascii_case(name))
}

/// `POST /v1/guest/session`
///
/// The `Guest` extension is what the auth layer attaches after it has checked
/// a live grant against [`GUEST_SESSION_PATH`]. `Option` because the route is
/// mounted on the operator surface too (beside `/v1/guest/ask`, for the reason
/// `ClientSurface::serves_guest_ask_route` gives), where a loopback caller is
/// admitted before any credential is read. Absent means "you are not a guest"
/// and is refused: a member writes under their own key and has no name to
/// claim.
pub async fn claim_name(
    State(state): State<AppState>,
    guest: Option<Extension<Guest>>,
    Json(body): Json<ClaimRequest>,
) -> Response {
    let Some(Extension(guest)) = guest else {
        tracing::debug!(
            path = GUEST_SESSION_PATH,
            "guest_session: refused a caller with no guest grant"
        );
        return refuse(
            axum::http::StatusCode::UNAUTHORIZED,
            "the name claim serves guests; a member writes under their own key",
        );
    };
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return refuse(
            axum::http::StatusCode::BAD_REQUEST,
            "a guest needs a name to be shown under; `name` is empty",
        );
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return refuse(
            axum::http::StatusCode::BAD_REQUEST,
            format!("that name is longer than {MAX_NAME_CHARS} characters"),
        );
    }

    // The rosters come from the rail's ONE reader, over every namespace this
    // bearer reaches — which the RESOLVER answers, never the request. Under a
    // wall grant that is every app the owner declared, and it has to be: "is
    // this a member's name" is a question about the wall this phone is
    // standing at, and a name refused on one app and accepted on the next
    // would make one person two.
    let namespaces = crate::routes_rail::reachable_namespaces(&state.guest_pages(), &guest);
    if namespaces.is_empty() {
        return refuse(
            axum::http::StatusCode::FORBIDDEN,
            "this grant reaches no app on this wall, so there is no room to be named in",
        );
    }
    let Some(rail) = state.ring_rail() else {
        return refuse(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "this daemon has no ring storage installed, so there is no roster to check \
             a name against — start it with a data directory",
        );
    };
    for namespace in &namespaces {
        let journal = match rail.journal(namespace) {
            Ok(j) => j,
            Err(e) => return refuse(axum::http::StatusCode::BAD_REQUEST, e.to_string()),
        };
        let roster = match rail.roster(&journal).await {
            Ok(r) => r,
            Err(e) => return refuse(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        if names_a_member(&roster, &name) {
            tracing::warn!(
                namespace,
                guest = name,
                "guest_session: refused a guest name that is a roster member's"
            );
            return refuse(
                axum::http::StatusCode::CONFLICT,
                format!(
                    "'{name}' is a member of this ring; a guest writes under a name of their own"
                ),
            );
        }
    }

    let handle = match commonwealth_transport::identity::generate_bearer_token() {
        Ok(h) => h,
        Err(e) => {
            // Never a name bound to a guessable handle: say the entropy failed.
            return refuse(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not mint a session handle: {e}"),
            );
        }
    };
    let now = commonwealth_core::clock::unix_now_millis();
    match state
        .inner
        .node
        .guest_sessions
        .claim(handle, &guest.grant, &name, now)
    {
        Ok(session) => {
            tracing::info!(
                namespaces = ?namespaces,
                guest = session.name,
                grant = ?guest.grant.label,
                expires_at_ms = session.expires_at_ms,
                "guest_session: claimed"
            );
            Json(serde_json::json!({
                "session": session.handle,
                "name": session.name,
                "expires_at_ms": session.expires_at_ms,
            }))
            .into_response()
        }
        Err(held) => {
            tracing::warn!(
                namespaces = ?namespaces,
                guest = held.name,
                "guest_session: refused a name another live session already holds"
            );
            refuse(
                axum::http::StatusCode::CONFLICT,
                format!(
                    "'{}' is already someone else in this room; a guest writes under a name of \
                     their own",
                    held.name
                ),
            )
        }
    }
}

/// One refusal shape, so a page can render any of them the same way — the same
/// `{error}` body the rail routes answer with.
fn refuse(status: axum::http::StatusCode, message: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": message.into() }))).into_response()
}
