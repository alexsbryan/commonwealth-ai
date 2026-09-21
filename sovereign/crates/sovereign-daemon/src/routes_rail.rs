// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring-app rail — the surface a deployed app writes its own state to.
//!
//! Mounted on [`ClientSurface::Rail`](crate::server::ClientSurface::Rail) and
//! on `Operator`, and nowhere else. A ring app reaches this and nothing else:
//! not inference, not knowledge, not app management. That guarantee is the
//! route set of the listener it can reach, not a check in this module (§7.1).
//!
//! **What a guest may reach is never what the guest says.** A request MAY name
//! a namespace — `?namespace=` has always been read here — and
//! [`namespace_for`] decides what to do with it against something the caller
//! does not author: a [`Scope::Rails`](sovereign_grants::Scope::Rails) grant's
//! one namespace, or, for a [`Wall`](sovereign_grants::Scope::Wall) grant, the
//! owner's `[daemon.guest_pages]` registry. Either way the answer comes from
//! the door's side of the wire. A guard reading a namespace the caller
//! supplied AND believing it would be the same defect as a wrong-slot guard
//! reading an SSE `model` field the client echoed back (§18.1).
//!
//! (This paragraph said "these routes take no namespace parameter, so an app
//! cannot reach another app's namespace because it has no way to *say* one"
//! until 2026-09-20. `resolve_granted` refused `asked != granted` the whole
//! time, so it was a CHECK described as a structural fact — and reading it as
//! structural is what made a wall grant look impossible to scope.)
//!
//! An operator has no grant (they are trusted by the listener they reached, and
//! can already touch every route on this daemon), so for them — and only them —
//! the namespace is an explicit query parameter.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use commonwealth_rail::{
    AdmittedOp, Compaction, RailAct, RailError, RingJournal, RingRail, RosterOrigin,
};
use serde::Deserialize;
use sovereign_grants::GuestGrant;

use crate::client_auth::Guest;
use crate::guest_door::GuestPages;
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

/// What a not-in-roster refusal tells the operator to DO, in the words of
/// whoever owns this namespace's roster.
///
/// The rail's own sentence names `svrn ring roster add … --self`, which is
/// right for a ring written by hand and wrong for one whose roster is derived:
/// the CLI refuses that command there (`ring_cmd::refuse_derived_roster`), so
/// the sentence would send a person to a door that will not open. On a derived
/// namespace the condition is not "you forgot to add yourself", it is "this
/// node is not in a mesh" — the same words the mesh's own publish route
/// already gives for the same condition, `sovereign-daemon/src/mesh_http.rs:715`
/// (ARCH §10.6: one condition, one thing said about it).
///
/// The origin is asked of the rail, never inferred from the namespace's name
/// and never matched out of the error's prose.
fn not_in_roster_refusal(origin: RosterOrigin, e: &RailError) -> String {
    match origin {
        RosterOrigin::Derived => "this node is not in a mesh yet, so every op it writes would be \
             unreadable to every peer — `svrn mesh create` or join one first."
            .into(),
        RosterOrigin::File => e.to_string(),
    }
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
pub fn namespace_for(
    pages: &GuestPages,
    guest: Option<&Guest>,
    requested: Option<&str>,
) -> Result<String, Response> {
    match guest {
        Some(Guest { grant, .. }) => resolve_granted(pages, grant, requested),
        None => requested.map(str::to_string).ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                "no rail grant on this request, so the namespace must be named \
                 explicitly — pass ?namespace=<id>",
            )
        }),
    }
}

fn resolve_granted(
    pages: &GuestPages,
    grant: &Arc<GuestGrant>,
    requested: Option<&str>,
) -> Result<String, Response> {
    if grant.is_wall() {
        return resolve_wall(pages, requested);
    }
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

/// A wall grant names no namespace, so the request must — and the owner's
/// registry, not the grant, says which ones exist.
///
/// Three refusals and they are three different facts, each said in its own
/// words (ARCH §18.3): a request that named nothing, a namespace this daemon
/// owns, and a namespace nobody put on the wall. None of them falls back to
/// another app, which is the failure a "helpful" default would be.
///
/// The daemon-owned check is SECOND, before the registry is consulted, and
/// that ordering is the point: `GuestPages::from_config` already refuses such
/// an entry at config load, so this arm can only fire if that refusal was
/// wrong or absent — which is exactly when a second guard is worth having
/// (ARCH 5).
fn resolve_wall(pages: &GuestPages, requested: Option<&str>) -> Result<String, Response> {
    let Some(asked) = requested else {
        tracing::info!("rail: a wall grant reached the rail without naming a namespace");
        return Err(err(
            StatusCode::BAD_REQUEST,
            "this grant is for the whole wall, so it does not say which app you mean \
             — name one with ?namespace=<id>",
        ));
    };
    if sovereign_mesh::ring_roster::is_daemon_owned(asked) {
        tracing::warn!(
            namespace = asked,
            "rail: refused a wall grant on one of this daemon's own rings"
        );
        return Err(err(
            StatusCode::FORBIDDEN,
            format!(
                "'{asked}' is one of this daemon's own rings and is never open to guests, \
                 whatever the page registry says"
            ),
        ));
    }
    if pages.declared(asked).is_none() {
        tracing::info!(
            namespace = asked,
            "rail: refused a wall grant on a namespace the registry does not declare"
        );
        return Err(err(
            StatusCode::FORBIDDEN,
            format!(
                "this wall's owner has not registered '{asked}' for guests — a wall grant \
                 reaches the apps in [daemon.guest_pages] and nothing else on the rail"
            ),
        ));
    }
    Ok(asked.to_string())
}

/// Every namespace this caller may touch — one for a `Scope::Rails` grant,
/// every declared app for a wall grant.
///
/// Decided BY [`namespace_for`] rather than beside it: the filter is literally
/// "would the resolver accept this?", so the enumeration and the per-request
/// decision cannot drift into two answers (ARCH 8). The one caller is the
/// guest-session claim, which has to check a name against every roster the
/// bearer reaches — under a wall grant "is this a member's name" is a question
/// about the whole wall, not about one app.
pub(crate) fn reachable_namespaces(pages: &GuestPages, guest: &Guest) -> Vec<String> {
    if !guest.grant.is_wall() {
        return guest
            .grant
            .rail_namespace()
            .map(str::to_string)
            .into_iter()
            .collect();
    }
    pages
        .declared_namespaces()
        .filter(|ns| resolve_wall(pages, Some(ns)).is_ok())
        .map(str::to_string)
        .collect()
}

/// May a guest WRITE here? An entry registered `guests = "read"` says no, and
/// says so naming the namespace and the mode the operator wrote.
///
/// A namespace the registry declares nothing about is unchanged — a
/// `Scope::Rails` grant is its owner scoping one link to one app by hand, and
/// there is no declaration to narrow it by. The narrowing is a property of the
/// DECLARATION, which is the whole model here.
fn refuse_read_only(
    pages: &GuestPages,
    guest: Option<&Guest>,
    namespace: &str,
) -> Option<Response> {
    let page = guest.and(pages.declared(namespace))?;
    if page.guests() != sovereign_core::guest_pages::GuestAccess::Read {
        return None;
    }
    tracing::info!(
        namespace,
        mode = page.guests().as_str(),
        "rail: refused a guest's append on a namespace registered read-only"
    );
    Some(err(
        StatusCode::FORBIDDEN,
        format!(
            "'{namespace}' is registered for guests as `guests = \"{}\"` — guests read \
             this app's journal and do not write to it",
            page.guests().as_str()
        ),
    ))
}

/// Resolve the namespace AND the journal behind it, or the refusal to return.
///
/// The two failures are different and are kept different. A namespace the
/// caller may not touch is a 403 about them; a rail with no storage installed
/// is a 503 about this daemon. Collapsing either into an empty success would
/// hand the app a plausible `[]` and let it carry on (ARCH §18.3).
pub(crate) fn journal_for(
    state: &AppState,
    guest: Option<&Guest>,
    requested: Option<&str>,
) -> Result<(Arc<RingRail>, Arc<RingJournal>), Response> {
    let namespace = namespace_for(&state.guest_pages(), guest, requested)?;
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

/// Render what [`RingJournal::seal`]'s prune did, for the append body.
///
/// **A seal and the prune it authorises are one act to the caller.** Rung S
/// taught the read path to skip a retired prefix; nothing removed one from
/// disk, so a seal on its own changed no bytes anywhere — and a prune with no
/// seal behind it reports the deleted range as missing, forever, on every node.
/// Neither half is worth anything alone, so an app that seals gets both from
/// one request rather than a capability it has to remember to call. Doing them
/// is the journal's; saying so in HTTP is this.
///
/// **A refusal is not an error on the append.** The seal is signed and fsynced
/// before the prune runs, so returning 500 would tell the app its seal failed
/// when it is durably on the journal, and the next attempt would write a second
/// one. The outcome is reported in the body instead — with the refusal's own
/// sentence, never as an absent field, because "nothing was retired" and "the
/// prune was refused" are the two answers a caller most needs to tell apart
/// (ARCH §18.3).
fn retire(retired: &Result<Compaction, RailError>) -> serde_json::Value {
    match retired {
        Ok(done) => serde_json::json!({
            "removed": done.removed,
            "kept": done.kept,
            "gaps_cleared": done.gaps_cleared,
        }),
        // The warn is `RingJournal::seal`'s, at the site that knows the
        // namespace and that a seal is already on disk. This renders.
        Err(e) => serde_json::json!({ "refused": e.to_string() }),
    }
}

/// Whose words this act is — decided by the DOOR, from the session
/// `client_auth` authenticated, and from nothing the caller wrote.
///
/// This is the whole difference between the stamp and the payload `guest`
/// field it replaces. That field was supplied by the page, so the check on it
/// asserted on what the subject authored (ARCH 5): a page that sent no name
/// had its guest's act shown under the member's, and a page that sent someone
/// else's was believed. Here a page that sends no name, a page that lies in
/// one, and a page that sends the right one all reach the same answer, because
/// the body is not consulted.
///
/// `None` is every caller with no guest session — a member on their own
/// loopback listener, `svrn ring dev`. An `on_behalf_of` such a caller put on
/// the wire is dropped HERE, before anything is signed, and the drop is
/// warned rather than silent: a name that did not reach the signature is not
/// a name this node is claiming, and the operator reading the log is the one
/// who needs to know which of the two happened.
fn stamp_from(guest: Option<&Guest>, body: &serde_json::Value) -> Option<String> {
    let claimed = body.get("on_behalf_of").and_then(|v| v.as_str());
    let stamped = guest
        .and_then(|g| g.session.as_ref())
        .map(|s| s.name.clone());
    match (stamped.as_deref(), claimed) {
        (Some(name), _) => tracing::debug!(
            guest = name,
            claimed_on_the_wire = ?claimed,
            "rail: stamped an act with the name the door's session holds"
        ),
        (None, Some(claimed)) => tracing::warn!(
            claimed,
            "rail: dropped an on_behalf_of from a caller with no guest session — \
             the door stamps this, a caller cannot"
        ),
        (None, None) => {}
    }
    stamped
}

/// One admitted op as the log ships it, with a guest act's attribution already
/// resolved.
///
/// `person` names the SIGNER's roster entry, and a guest writes through a
/// member's door — so shipping it alone puts the member's name on the guest's
/// words, and every app behind this route has to compose the sentence itself.
/// ring-doc did, which is one rendering with two spellings waiting to happen
/// (ARCH 8). `admit` refuses every key the roster does not name, so the signer
/// half is always present here and the sentence never has a missing side.
///
/// `guest` sits beside it, structured, for an app that wants to treat a guest
/// differently. An app that does not — the scaffold does not — renders
/// `person` and is correct without knowing guests exist.
fn shipped(op: &AdmittedOp) -> serde_json::Value {
    // `to_value` on a derived `Serialize` over strings, integers and options
    // has no failing input — the same reason the `gaps` map below does this.
    let mut v = serde_json::to_value(op).unwrap_or_default();
    let (Some(name), Some(obj)) = (op.on_behalf_of.as_deref(), v.as_object_mut()) else {
        return v;
    };
    let of = op.person.as_str();
    obj.insert(
        "person".into(),
        serde_json::Value::String(format!("{name}, guest of {of}")),
    );
    obj.insert(
        "guest".into(),
        serde_json::json!({ "name": name, "of": of }),
    );
    v
}

/// POST /v1/rail/append — sign and append one act to this caller's namespace.
///
/// The body is the act alone. `seq`, the signature, the timestamp and the id
/// are all this daemon's to assign: an app that could choose its own sequence
/// number or actor could write as somebody else, and the whole point of the
/// grant is that it cannot.
///
/// The act's payload is the app's and this route reads no field of it. Whose
/// words the act is comes from the guest session the door authenticated
/// ([`stamp_from`]) — so an app cannot write as somebody else by naming them
/// either, which is the same guarantee the paragraph above states about `seq`
/// and the actor.
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
    // The one place the registry's narrowing bites. Before the body is parsed
    // and long before anything is signed: a refused write must leave no trace
    // on the journal, and the cheapest way to promise that is to refuse before
    // there is anything to write.
    if let Some(refusal) = refuse_read_only(&state.guest_pages(), guest, journal.namespace()) {
        return refusal;
    }
    // Read before the body becomes an act, because `RailAct` does not carry
    // this field and serde drops it on the way in.
    let on_behalf_of = stamp_from(guest, &body);
    // Taken as a `Value` and converted here rather than as `Json<RailAct>`,
    // so a refusal is the rail's own sentence instead of axum's rejection
    // prose wrapped around serde's prose wrapped around it (ARCH §10.6).
    let act = match RailAct::from_json(body) {
        Ok(act) => act,
        Err(e) => return err(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
    };
    // Through the rail's ONE roster reader, not the file: the daemon's own
    // namespace derives its roster from membership, and reading the file here
    // refused this node's own key on that ring (ARCH §10.6).
    let roster = match rail.roster(&journal).await {
        Ok(r) => r,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    // Sealing is the one act with a second half, and the pair lives on the
    // journal (`RingJournal::seal`) rather than here — the daemon's own KV
    // pump seals too, and two spellings of "seal, then compact, and a refused
    // compaction is not a failed seal" is a decider with two answers
    // (ARCH §10.6).
    let sealed = matches!(act, RailAct::Seal);
    let appended = if sealed {
        // A seal takes no stamp: it is delivery, not words. `applies()` is
        // false for it, no reducer sees it, and there is nothing it could be
        // said on behalf of. Named here rather than left to be noticed,
        // because "every act carries the guest's name" is otherwise read as
        // covering this one too.
        journal
            .seal(rail.signer(), &roster, &commonwealth_rail::Ed25519Verifier)
            .map(|done| (done.op, Some(retire(&done.retired))))
    } else {
        journal
            .append(act, rail.signer(), &roster, on_behalf_of.as_deref())
            .map(|op| (op, None))
    };
    match appended {
        Ok((op, retired)) => {
            // The write is on disk; ask the ring round to run NOW rather than
            // at its sixty-second tick. Same door the KV pump and the work
            // donor use (`sovereign-mesh/src/work_donor.rs:1184`) — this
            // route does not talk to a peer, it asks `ring_sync` to.
            state.ring_write_nudge().notify_one();
            let mut out = serde_json::json!({
                "id": op.id,
                "seq": op.kind.seq,
                "actor": op.actor,
                "ts_unix": op.ts_unix,
                "namespace": journal.namespace(),
            });
            if let Some(retired) = retired {
                out["retired"] = retired;
            }
            Json(out).into_response()
        }
        Err(e @ RailError::NotInRoster { .. }) => err(
            StatusCode::UNPROCESSABLE_ENTITY,
            not_in_roster_refusal(rail.roster_origin(journal.namespace()), &e),
        ),
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
    let (rail, journal) = match journal_for(&state, guest, q.namespace.as_deref()) {
        Ok(pair) => pair,
        Err(refusal) => return refusal,
    };
    let roster = match rail.roster(&journal).await {
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
    let admission = commonwealth_rail::admit(
        &ops,
        &skipped,
        &roster,
        journal.namespace(),
        &commonwealth_rail::Ed25519Verifier,
    );
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
        // Not `admission.ops` verbatim: a guest act's attribution is finished
        // HERE, once, for the wall's page, the scaffold's page and
        // `svrn ring log` alike — see [`shipped`].
        "ops": admission.ops.iter().map(shipped).collect::<Vec<_>>(),
        "gaps": gaps,
        "held": admission.held,
        "complete": admission.is_complete(),
        "roster": roster,
    }))
    .into_response()
}

// Moved to a sibling file: inline, these put this file into the 800-1200
// approach band (ARCH §3.1). `#[path]`, so the names are unchanged.
#[cfg(test)]
#[path = "tests/routes_rail.rs"]
mod tests;
