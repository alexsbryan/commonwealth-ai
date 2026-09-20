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

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;

    use commonwealth_rail::{Roster, RosterSource};
    use sovereign_grants::Scope;

    use super::*;

    /// Enough of a signer to build a rail. Nothing here signs anything: the
    /// refusal under test is decided before a signature exists.
    struct Unclaimed;
    impl commonwealth_rail::RingSigner for Unclaimed {
        fn actor(&self) -> String {
            "ab".repeat(32)
        }
        fn sign(&self, _ns: &str, _ts: i64, _seq: u64, _body: &str) -> String {
            String::new()
        }
    }

    /// A roster that is computed — the shape `mesh-measurements` installs.
    /// Empty, because a node that is in no mesh derives no members, which is
    /// exactly the state that reaches this refusal.
    struct Membership;
    impl RosterSource for Membership {
        fn roster(&self) -> Pin<Box<dyn Future<Output = Result<Roster, RailError>> + Send + '_>> {
            Box::pin(async { Ok(Roster::new(Default::default())) })
        }
    }

    /// The refusal `append` would render for `ns`, asked of a real rail so the
    /// origin comes from `RingRail` and not from a fixture.
    fn refusal_on(ns: &str, derived: bool) -> String {
        let rail = RingRail::new("/nonexistent", Arc::new(Unclaimed));
        if derived {
            rail.derive_roster(ns, Arc::new(Membership)).unwrap();
        }
        let e = RailError::NotInRoster {
            actor: "ab".repeat(32),
            namespace: ns.to_string(),
        };
        not_in_roster_refusal(rail.roster_origin(ns), &e)
    }

    /// The fix a refusal names has to be a command the reader can actually
    /// run. `svrn ring roster add` is REFUSED on a derived namespace, so
    /// naming it there sends the operator to a door that will not open.
    #[test]
    fn a_derived_namespace_is_refused_in_the_meshs_words_not_the_roster_files() {
        let why = refusal_on("mesh-measurements", true);
        assert!(why.contains("svrn mesh"), "must name the mesh: {why}");
        assert!(
            !why.contains("roster add"),
            "must not send them to a refused command: {why}"
        );
    }

    /// The negative control: on a hand-written ring `roster add` IS the fix,
    /// and the rail's own sentence is still what the operator sees. Without
    /// this the test above passes for a renderer that says "mesh" always.
    #[test]
    fn a_file_namespace_still_names_the_roster_command() {
        let why = refusal_on("house-expenses", false);
        assert!(
            why.contains("svrn ring roster add"),
            "must name the fix: {why}"
        );
        assert!(!why.contains("svrn mesh"), "{why}");
    }

    fn grant_with(scopes: Vec<Scope>) -> Guest {
        Guest {
            grant: Arc::new(GuestGrant {
                token: "t".into(),
                scopes,
                label: None,
                issued_at_ms: 0,
                expires_at_ms: u64::MAX,
                revoked: false,
            }),
            session: None,
        }
    }

    /// A guest behind the door who has claimed `name` through their session.
    fn guest_named(name: &str) -> Guest {
        let mut g = grant_with(vec![Scope::Rails("house-expenses".into())]);
        g.session = Some(sovereign_grants::GuestSession {
            handle: "h".into(),
            grant_token: "t".into(),
            name: name.into(),
            issued_at_ms: 0,
            expires_at_ms: u64::MAX,
        });
        g
    }

    /// Clause (a) of `rg-guest-stamped-by-the-door`: the page that sends
    /// nothing at all is the common case — the scaffold does not know guests
    /// exist — and its guest is still named.
    #[test]
    fn a_page_that_names_nobody_still_has_its_guest_named() {
        let body = serde_json::json!({ "op": "record", "payload": { "amount": 12 } });
        assert_eq!(
            stamp_from(Some(&guest_named("Ada")), &body).as_deref(),
            Some("Ada")
        );
    }

    /// Clause (b): the page that LIES gets the same answer as the page that
    /// says nothing. This is the test the old payload-`guest` check could not
    /// pass — it believed the field and only compared it to the roster.
    #[test]
    fn a_page_claiming_another_name_does_not_get_it() {
        let body = serde_json::json!({
            "op": "record",
            "payload": { "amount": 12, "guest": "Zoe" },
        });
        assert_eq!(
            stamp_from(Some(&guest_named("Ada")), &body).as_deref(),
            Some("Ada")
        );
    }

    /// Clause (c): a caller with no guest session — a member on their own
    /// loopback listener — cannot put a name on the wire and have it signed.
    /// Stripped here, before `append` ever sees it.
    #[test]
    fn a_caller_without_a_session_cannot_name_somebody() {
        let body = serde_json::json!({ "op": "record", "on_behalf_of": "Ada" });
        assert_eq!(stamp_from(None, &body), None);
        // A guest who has not claimed a name yet is the same answer: absent,
        // never the name the body offered.
        let unclaimed = grant_with(vec![Scope::Rails("house-expenses".into())]);
        assert_eq!(stamp_from(Some(&unclaimed), &body), None);
    }

    fn admitted(on_behalf_of: Option<&str>) -> AdmittedOp {
        AdmittedOp {
            id: commonwealth_rail::OpId::from_raw("abc"),
            actor: "ab".repeat(32),
            person: "BeefyMac".into(),
            seq: 0,
            ts_unix: 0,
            corrects: None,
            voided: false,
            payload: None,
            on_behalf_of: on_behalf_of.map(str::to_string),
        }
    }

    /// The name an app reads is finished before it leaves the daemon: one
    /// composition for the wall's page, the scaffold's page and
    /// `svrn ring log`, all of which render `person`.
    #[test]
    fn the_log_hands_apps_a_finished_name() {
        let v = shipped(&admitted(Some("Ada")));
        assert_eq!(v["person"], "Ada, guest of BeefyMac");
        assert_eq!(v["guest"]["name"], "Ada");
        assert_eq!(v["guest"]["of"], "BeefyMac");
    }

    /// The negative control: without it the test above passes for a renderer
    /// that appends "guest of" to everything.
    #[test]
    fn a_members_own_act_is_shipped_untouched() {
        let v = shipped(&admitted(None));
        assert_eq!(v["person"], "BeefyMac");
        assert!(v.get("guest").is_none(), "{v}");
    }

    #[test]
    fn a_rail_grant_decides_its_own_namespace() {
        let g = grant_with(vec![Scope::Rails("house-expenses".into())]);
        assert_eq!(
            namespace_for(&no_pages(), Some(&g), None).unwrap(),
            "house-expenses"
        );
    }

    /// The property this whole module exists for: an app cannot reach another
    /// app's namespace by asking for one.
    #[test]
    fn a_request_cannot_widen_its_grant_by_naming_another_namespace() {
        let g = grant_with(vec![Scope::Rails("house-expenses".into())]);
        let refusal = namespace_for(&no_pages(), Some(&g), Some("someone-elses")).unwrap_err();
        assert_eq!(refusal.status(), StatusCode::FORBIDDEN);
    }

    /// Naming your OWN namespace is allowed — it is redundant, not hostile.
    #[test]
    fn naming_the_granted_namespace_is_accepted() {
        let g = grant_with(vec![Scope::Rails("house-expenses".into())]);
        assert_eq!(
            namespace_for(&no_pages(), Some(&g), Some("house-expenses")).unwrap(),
            "house-expenses"
        );
    }

    #[test]
    fn a_grant_without_a_rail_scope_is_refused() {
        let g = grant_with(vec![Scope::Models(vec!["m".into()])]);
        let refusal = namespace_for(&no_pages(), Some(&g), Some("anything")).unwrap_err();
        assert_eq!(refusal.status(), StatusCode::FORBIDDEN);
    }

    /// An operator names it explicitly, and an unnamed one is refused rather
    /// than defaulted to something plausible (§18.3).
    #[test]
    fn an_operator_must_name_the_namespace_and_is_refused_without_one() {
        assert_eq!(
            namespace_for(&no_pages(), None, Some("house-expenses")).unwrap(),
            "house-expenses"
        );
        let refusal = namespace_for(&no_pages(), None, None).unwrap_err();
        assert_eq!(refusal.status(), StatusCode::BAD_REQUEST);
    }
    /// A door with nothing declared — the state every test above is about,
    /// where the grant alone decides.
    fn no_pages() -> GuestPages {
        GuestPages::default()
    }

    /// A door whose owner declared two apps, one of them read-only.
    fn wall_of(entries: &[(&str, sovereign_core::guest_pages::GuestPage)]) -> GuestPages {
        GuestPages::new(
            None,
            entries
                .iter()
                .map(|(ns, p)| ((*ns).to_string(), p.clone()))
                .collect(),
        )
    }

    fn open(dir: &str) -> sovereign_core::guest_pages::GuestPage {
        sovereign_core::guest_pages::GuestPage::Open(dir.into())
    }

    fn read_only(dir: &str) -> sovereign_core::guest_pages::GuestPage {
        sovereign_core::guest_pages::GuestPage::Narrowed {
            dir: dir.into(),
            guests: sovereign_core::guest_pages::GuestAccess::Read,
        }
    }

    /// The wall grant reaches every app the OWNER declared — one credential,
    /// two apps, and the request says which.
    #[test]
    fn a_wall_grant_reaches_each_declared_namespace() {
        let pages = wall_of(&[("house-expenses", open("/a")), ("ring-doc", open("/b"))]);
        let g = grant_with(vec![Scope::Wall]);
        for ns in ["house-expenses", "ring-doc"] {
            assert_eq!(namespace_for(&pages, Some(&g), Some(ns)).unwrap(), ns);
        }
    }

    /// Clause (e) of `rg-one-person-across-apps`: the wall is bounded by the
    /// declaration, so a namespace nobody registered is refused BY NAME —
    /// never served as another app's.
    #[test]
    fn a_wall_grant_is_refused_a_namespace_nobody_declared() {
        let pages = wall_of(&[("house-expenses", open("/a"))]);
        let g = grant_with(vec![Scope::Wall]);
        let refusal = namespace_for(&pages, Some(&g), Some("someone-elses")).unwrap_err();
        assert_eq!(refusal.status(), StatusCode::FORBIDDEN);
    }

    /// **The hard edge, and the second half of it.** A config that wrongly
    /// declares one of this daemon's own rings is refused at load — and this
    /// is the route refusing it anyway, which is the whole reason a registry
    /// is not the only guard (ARCH 5).
    #[test]
    fn a_daemon_owned_namespace_is_refused_even_when_the_registry_declares_it() {
        let owned = sovereign_core::mesh_measurements::MEASUREMENTS_APP_ID;
        let pages = wall_of(&[(owned, open("/a")), ("work", open("/b"))]);
        let g = grant_with(vec![Scope::Wall]);
        for ns in [owned, "work"] {
            let refusal = namespace_for(&pages, Some(&g), Some(ns)).unwrap_err();
            assert_eq!(
                refusal.status(),
                StatusCode::FORBIDDEN,
                "{ns} was reachable through a wall grant"
            );
        }
    }

    /// A wall grant names no namespace, so a request that names none has not
    /// said what it wants. Refused with a sentence, never resolved to the
    /// first declared app (§18.3).
    #[test]
    fn a_wall_grant_with_no_namespace_on_the_request_is_refused() {
        let pages = wall_of(&[("house-expenses", open("/a"))]);
        let g = grant_with(vec![Scope::Wall]);
        let refusal = namespace_for(&pages, Some(&g), None).unwrap_err();
        assert_eq!(refusal.status(), StatusCode::BAD_REQUEST);
    }

    /// `guests = "read"` narrows what a guest may DO, not what they may see.
    #[test]
    fn a_read_only_entry_refuses_a_guests_append_and_nobody_elses() {
        let pages = wall_of(&[("doc", read_only("/b")), ("house-expenses", open("/a"))]);
        let g = grant_with(vec![Scope::Wall]);
        assert!(refuse_read_only(&pages, Some(&g), "doc").is_some());
        // The open app on the same wall is untouched...
        assert!(refuse_read_only(&pages, Some(&g), "house-expenses").is_none());
        // ...as is an operator, who holds no grant and is not a guest.
        assert!(refuse_read_only(&pages, None, "doc").is_none());
        // ...as is a namespace the registry says nothing about.
        assert!(refuse_read_only(&pages, Some(&g), "undeclared").is_none());
    }
}
