// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring-app rail — the surface a deployed app writes its own state to.
//!
//! Mounted on [`ClientSurface::Rail`](crate::server::ClientSurface::Rail) and
//! on `Operator`, and nowhere else. A ring app reaches this and nothing else:
//! not inference, not knowledge, not app management. That guarantee is the
//! route set of the listener it can reach, not a check in this module (§7.1).
//!
//! **The namespace is on the grant, never in the request.** A ring app holds a
//! [`Scope::Rails`](commonwealth_knowledge::Scope::Rails) naming exactly one
//! namespace, and these routes take no namespace parameter, so an app cannot
//! reach another app's namespace because it has no way to *say* one. A guard
//! reading a namespace the caller supplied would be the same defect as a
//! wrong-slot guard reading an SSE `model` field the client echoed back
//! (§18.1) — an assertion on what the subject authored.
//!
//! An operator has no grant (they are trusted by the listener they reached, and
//! can already touch every route on this daemon), so for them — and only them —
//! the namespace is an explicit query parameter.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use commonwealth_knowledge::GuestGrant;
use commonwealth_rail::{RailAct, RailError, RingJournal, RingRail};
use serde::Deserialize;

use crate::client_auth::Guest;
use crate::state::AppState;

/// Query parameters common to every rail route.
#[derive(Debug, Deserialize)]
pub struct RailQuery {
    /// Operator-only. Ignored — and refused — when the caller holds a rail
    /// grant, because the grant is the authority on which namespace this
    /// caller may touch.
    #[serde(default)]
    pub namespace: Option<String>,
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// Resolve which namespace this request acts on.
///
/// THE decider, and the only place a namespace is chosen. Three cases:
///
/// - **A rail grant is present.** Its namespace wins, always. A request that
///   also names one is refused rather than silently ignored — quietly acting
///   on a different namespace than the caller asked for is the "never silently
///   substitute" failure (§18.3), and it would leave the app's author believing
///   something untrue about where their data went.
/// - **A grant without a rail scope.** Cannot happen through `client_auth`
///   (`permits_path` would have refused the route), so this is a defensive
///   refusal, not a path with a story.
/// - **No grant at all** — an operator on a listener that trusts them. They
///   name the namespace explicitly; absent, we refuse rather than guess.
pub fn namespace_for(guest: Option<&Guest>, requested: Option<&str>) -> Result<String, Response> {
    match guest {
        Some(Guest(grant)) => resolve_granted(grant, requested),
        None => requested.map(str::to_string).ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                "no rail grant on this request, so the namespace must be named \
                 explicitly — pass ?namespace=<id>",
            )
        }),
    }
}

fn resolve_granted(grant: &Arc<GuestGrant>, requested: Option<&str>) -> Result<String, Response> {
    let Some(granted) = grant.rail_namespace() else {
        return Err(err(
            StatusCode::FORBIDDEN,
            "this grant carries no rail scope",
        ));
    };
    match requested {
        Some(asked) if asked != granted => Err(err(
            StatusCode::FORBIDDEN,
            format!(
                "this grant is scoped to namespace '{granted}' and cannot act on \
                 '{asked}'"
            ),
        )),
        _ => Ok(granted.to_string()),
    }
}

/// Resolve the namespace AND the journal behind it, or the refusal to return.
///
/// The two failures are different and are kept different. A namespace the
/// caller may not touch is a 403 about them; a rail with no storage installed
/// is a 503 about this daemon. Collapsing either into an empty success would
/// hand the app a plausible `[]` and let it carry on (ARCH §18.3).
fn journal_for(
    state: &AppState,
    guest: Option<&Guest>,
    requested: Option<&str>,
) -> Result<(Arc<RingRail>, Arc<RingJournal>), Response> {
    let namespace = namespace_for(guest, requested)?;
    let rail = state.ring_rail().ok_or_else(|| {
        err(
            StatusCode::SERVICE_UNAVAILABLE,
            "this daemon has no ring storage installed, so there is nowhere \
             to keep a journal — start it with a data directory",
        )
    })?;
    let journal = rail
        .journal(&namespace)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok((rail, journal))
}

/// POST /v1/rail/append — sign and append one act to this caller's namespace.
///
/// The body is the act alone. `seq`, the signature, the timestamp and the id
/// are all this daemon's to assign: an app that could choose its own sequence
/// number or actor could write as somebody else, and the whole point of the
/// grant is that it cannot.
///
/// The act's payload is the app's, and this route does not read inside it.
/// What it does check is that the payload has a canonical form — see
/// [`Payload`](commonwealth_rail::Payload) — because a body whose bytes
/// two nodes would spell differently cannot be signed once and verified
/// everywhere.
pub async fn append(
    State(state): State<AppState>,
    guest: Option<axum::Extension<Guest>>,
    Query(q): Query<RailQuery>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let guest = guest.as_ref().map(|e| &e.0);
    let (rail, journal) = match journal_for(&state, guest, q.namespace.as_deref()) {
        Ok(pair) => pair,
        Err(refusal) => return refusal,
    };
    // Taken as a `Value` and converted here rather than as `Json<RailAct>`,
    // so a refusal is the rail's own sentence instead of axum's rejection
    // prose wrapped around serde's prose wrapped around it (ARCH §10.6).
    let act = match RailAct::from_json(body) {
        Ok(act) => act,
        Err(e) => return err(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
    };
    let roster = match journal.roster() {
        Ok(r) => r,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    match journal.append(act, rail.signer(), &roster) {
        Ok(op) => Json(serde_json::json!({
            "id": op.id,
            "seq": op.kind.seq,
            "actor": op.actor,
            "ts_unix": op.ts_unix,
            "namespace": journal.namespace(),
        }))
        .into_response(),
        Err(RailError::Rejected(why)) => err(StatusCode::UNPROCESSABLE_ENTITY, why),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// GET /v1/rail/log — the namespace's acts, in one order, and its gaps.
///
/// Both in one response, because acts without their gaps are the failure this
/// rail exists to avoid: a confident answer over a subset. An app rendering
/// this must show `complete: false` somewhere a person sees it.
///
/// **`ops` is already in the order every node applies them in** — deduplicated,
/// signature-checked, roster-admitted, sequence-audited and void-marked. An
/// app folds it; an app that sorts or filters it itself has reached around the
/// one guarantee the rail is here to give. That is what the SDK's `ring.fold`
/// is for.
///
/// What is NOT here is a balance. There is no balance the rail could compute:
/// it does not know what a payload means. The app's reducer does.
pub async fn log(
    State(state): State<AppState>,
    guest: Option<axum::Extension<Guest>>,
    Query(q): Query<RailQuery>,
) -> Response {
    let guest = guest.as_ref().map(|e| &e.0);
    let (_, journal) = match journal_for(&state, guest, q.namespace.as_deref()) {
        Ok(pair) => pair,
        Err(refusal) => return refusal,
    };
    let roster = match journal.roster() {
        Ok(r) => r,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    // ONE read, admitted from those exact bytes. Reading the journal twice —
    // once for the lines, once inside `admit` — would let a write land between
    // them and ship an answer that does not match the ops beside it.
    let (ops, skipped) = match journal.read() {
        Ok(pair) => pair,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let admission = commonwealth_rail::admit(&ops, &skipped, &roster, journal.namespace());
    // Each gap ships its own sentence. The renderer is `RailGap`'s `Display`
    // and it lives in the rail — carrying the rendered string means the
    // terminal, a ring app's page and the append door's 422 all say the same
    // words about the same condition, instead of three surfaces each inventing
    // prose for a tag (ARCH §10.6). The tagged fields stay, so a caller that
    // wants to branch on the kind still can.
    let gaps: Vec<serde_json::Value> = admission
        .gaps
        .iter()
        .map(|g| {
            let mut v = serde_json::to_value(g).unwrap_or_default();
            if let Some(obj) = v.as_object_mut() {
                obj.insert("message".into(), serde_json::Value::String(g.to_string()));
            }
            v
        })
        .collect();
    Json(serde_json::json!({
        "namespace": journal.namespace(),
        "ops": admission.ops,
        "gaps": gaps,
        "held": admission.held,
        "complete": admission.is_complete(),
        "roster": roster,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_knowledge::Scope;

    fn grant_with(scopes: Vec<Scope>) -> Guest {
        Guest(Arc::new(GuestGrant {
            token: "t".into(),
            scopes,
            label: None,
            issued_at_ms: 0,
            expires_at_ms: u64::MAX,
            revoked: false,
        }))
    }

    #[test]
    fn a_rail_grant_decides_its_own_namespace() {
        let g = grant_with(vec![Scope::Rails("house-expenses".into())]);
        assert_eq!(namespace_for(Some(&g), None).unwrap(), "house-expenses");
    }

    /// The property this whole module exists for: an app cannot reach another
    /// app's namespace by asking for one.
    #[test]
    fn a_request_cannot_widen_its_grant_by_naming_another_namespace() {
        let g = grant_with(vec![Scope::Rails("house-expenses".into())]);
        let refusal = namespace_for(Some(&g), Some("someone-elses")).unwrap_err();
        assert_eq!(refusal.status(), StatusCode::FORBIDDEN);
    }

    /// Naming your OWN namespace is allowed — it is redundant, not hostile.
    #[test]
    fn naming_the_granted_namespace_is_accepted() {
        let g = grant_with(vec![Scope::Rails("house-expenses".into())]);
        assert_eq!(
            namespace_for(Some(&g), Some("house-expenses")).unwrap(),
            "house-expenses"
        );
    }

    #[test]
    fn a_grant_without_a_rail_scope_is_refused() {
        let g = grant_with(vec![Scope::Models(vec!["m".into()])]);
        let refusal = namespace_for(Some(&g), Some("anything")).unwrap_err();
        assert_eq!(refusal.status(), StatusCode::FORBIDDEN);
    }

    /// An operator names it explicitly, and an unnamed one is refused rather
    /// than defaulted to something plausible (§18.3).
    #[test]
    fn an_operator_must_name_the_namespace_and_is_refused_without_one() {
        assert_eq!(
            namespace_for(None, Some("house-expenses")).unwrap(),
            "house-expenses"
        );
        let refusal = namespace_for(None, None).unwrap_err();
        assert_eq!(refusal.status(), StatusCode::BAD_REQUEST);
    }
}
