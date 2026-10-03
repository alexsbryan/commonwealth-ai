// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /v1/guest/ask` — the door answers for the guest.
//!
//! A guest standing in the room holds a wall grant and a phone. They can
//! already write on the wall (`Scope::Rails`), and the room can already
//! answer a question from its own corpora and its mesh siblings — but only
//! for a caller holding the whole conversation surface, which is
//! `/v1/conversations*`: create, list, search, read, patch, delete. Handing a
//! guest that surface would hand them every conversation the household has
//! ever had.
//!
//! So the door answers on their behalf. The turn runs IN-PROCESS through
//! [`collect_turn`] — the same driver `svrn chat ask` and the desktop drive,
//! so a guest's answer is grounded and cited exactly as a member's is — under
//! the DOOR's own principal, and the reply carries only the answer and its
//! epistemic ledger. No conversation id, no history, nothing a guest could
//! turn into a handle on somebody else's.
//!
//! ## One conversation per grant, and it is not a map
//!
//! A guest asking twice should be understood the second time, so the turn
//! needs a conversation that persists across requests. The id is DERIVED from
//! the grant's bearer rather than minted and remembered:
//! [`conversation_for_grant`] is `sha256(token)`, hex, truncated. That makes
//! "a second grant's bearer cannot read the first's conversation" a fact
//! about arithmetic rather than a property of a table someone has to key
//! correctly (ARCH principle 10), and it needs no eviction policy, no lock,
//! and no lifecycle tied to the door's.
//!
//! The token itself never leaves this function: a digest is one-way, so the
//! id is safe to log, to store, and to read back out of the conversations
//! table — which a guest cannot reach anyway.

use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use sovereign_contracts::types::TurnMode;
use sovereign_core::runtime::collect_turn;

use crate::client_auth::Guest;
use crate::daemon::EmbeddedDaemon;

/// The one path this module serves. Named here and read by
/// `sovereign_grants::Scope::Rails::paths`' own test so the mount and the
/// allowlist cannot drift apart silently.
pub const GUEST_ASK_PATH: &str = "/v1/guest/ask";

/// Longest question the door will carry. A guest is typing on a phone; a body
/// past this is not a question, and refusing it here costs nothing while
/// admitting it costs a model's whole context.
const MAX_QUESTION_CHARS: usize = 4_000;

#[derive(Debug, Deserialize)]
pub struct GuestAskRequest {
    /// What the guest asked, verbatim.
    pub question: String,
}

/// The conversation this grant's asks accumulate in.
///
/// `sha256(token)` truncated to 32 hex chars — 128 bits, which is more than
/// enough for "two live grants never collide" and short enough to read in a
/// log line. Prefixed so a row in the conversations table says what made it.
pub fn conversation_for_grant(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    format!("guest-{}", hex::encode(&digest[..16]))
}

/// `POST /v1/guest/ask`
///
/// The `Guest` extension is what the auth layer attaches after it has
/// checked a live grant against [`GUEST_ASK_PATH`]. It is `Option` here
/// because the route is also mounted on the operator surface (see
/// [`ClientSurface::serves_guest_ask_route`](crate::client_surface::ClientSurface::serves_guest_ask_route)),
/// where a loopback caller is admitted before any credential is read and so
/// carries no grant. Absent means "you are not a guest" and is refused —
/// naming the boundary rather than serving a turn to whoever arrived.
pub async fn guest_ask(
    guest: Option<Extension<Guest>>,
    daemon: Option<Extension<Arc<EmbeddedDaemon>>>,
    Json(body): Json<GuestAskRequest>,
) -> Response {
    let Some(Extension(Guest { grant, .. })) = guest else {
        // The operator mount. A local caller wanting a grounded turn has
        // `/v1/conversations/{id}/messages`; this door is for the person who
        // does not.
        tracing::debug!(
            path = GUEST_ASK_PATH,
            "guest_ask: refused a caller with no guest grant"
        );
        return refuse(
            axum::http::StatusCode::UNAUTHORIZED,
            "the ask door serves guests; a local caller has /v1/conversations",
        );
    };
    let Some(Extension(daemon)) = daemon else {
        // The router was built without the daemon handle. Say so rather than
        // answering "no" to a question nobody asked (ARCH principle 6).
        return refuse(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "this router was built without a turn host",
        );
    };
    let (Some(runtime), Some(store)) = (daemon.runtime(), daemon.state_store()) else {
        return refuse(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "this daemon serves no turns (mesh-admin)",
        );
    };

    let question = body.question.trim();
    if question.is_empty() {
        return refuse(
            axum::http::StatusCode::BAD_REQUEST,
            "ask: `question` is empty",
        );
    }
    if question.chars().count() > MAX_QUESTION_CHARS {
        return refuse(
            axum::http::StatusCode::BAD_REQUEST,
            "ask: `question` is longer than this door carries",
        );
    }

    let conversation_id = conversation_for_grant(&grant.token);
    // Glassbox: which grant asked, in which conversation, and how long the
    // question was. Never the token and never the question — the door is a
    // stranger's, and its log line is read by the household.
    tracing::info!(
        label = ?grant.label,
        scopes = %grant.summary(),
        conversation_id = %conversation_id,
        question_chars = question.chars().count(),
        "guest_ask: accepted"
    );

    // INSERT-OR-IGNORE, so "created on first ask" needs no first-ask branch
    // and no record of which asks have happened.
    if let Err(e) = runtime
        .seed_conversation(&conversation_id, sovereign_time::unix_now(), None, None)
        .await
    {
        return refuse(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            &format!("ask: could not open this guest's conversation: {e}"),
        );
    }

    match collect_turn(
        runtime,
        store.as_ref(),
        &conversation_id,
        question,
        TurnMode::Grounded,
        None,
    )
    .await
    {
        Ok(turn) => {
            tracing::info!(
                conversation_id = %conversation_id,
                citations = turn.citations.len(),
                members = ?turn
                    .epistemic_state
                    .as_ref()
                    .map(|e| e.citations.iter().filter_map(|c| c.member.clone()).collect::<Vec<_>>()),
                "guest_ask: answered"
            );
            // Exactly two keys. `message_id`, `conversation_id` and
            // `provenance` are all handles onto state this guest must not
            // reach, and a client that never sees them cannot ask for them.
            Json(serde_json::json!({
                "answer": turn.text,
                "epistemic_state": turn.epistemic_state,
            }))
            .into_response()
        }
        Err(e) => refuse(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            &format!("ask: the turn failed: {e}"),
        ),
    }
}

fn refuse(status: axum::http::StatusCode, reason: &str) -> Response {
    (status, Json(serde_json::json!({ "error": reason }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Two grants must never share a conversation.** This is the whole
    /// isolation property, and it holds by arithmetic rather than by a map
    /// somebody keys correctly.
    #[test]
    fn each_grant_gets_its_own_conversation_and_the_same_one_every_time() {
        let a = conversation_for_grant("bearer-a");
        let b = conversation_for_grant("bearer-b");
        assert_ne!(a, b, "two bearers collided onto one conversation");
        assert_eq!(a, conversation_for_grant("bearer-a"), "not stable");
    }

    /// The id is a DIGEST, not the token wearing a prefix. It lands in log
    /// lines and in a database row; a token that could be read back out of
    /// either would be a bearer published by the thing meant to bound it.
    #[test]
    fn the_conversation_id_does_not_carry_the_token() {
        let token = "s3cret-bearer-value";
        let id = conversation_for_grant(token);
        assert!(!id.contains(token));
        assert!(id.starts_with("guest-"));
        assert_eq!(id.len(), "guest-".len() + 32);
    }

    /// The mount and the allowlist name the same string.
    #[test]
    fn the_route_is_the_one_a_rail_grant_unlocks() {
        assert!(sovereign_grants::Scope::Rails("wall".into())
            .paths()
            .contains(&GUEST_ASK_PATH));
    }
}
