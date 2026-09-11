// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring-app rail — the surface a deployed app writes its own state to.
//!
//! Mounted on [`ClientSurface::Rail`](crate::server::ClientSurface::Rail) and
//! on `Operator`, and nowhere else. A ring app reaches this and nothing else:
//! not inference, not knowledge, not app management. That guarantee is the
//! route set of the listener it can reach, not a check in this module (§7.1).
//!
//! **The namespace is on the grant, never in the request.** A ring app holds a
//! [`Scope::Rails`](sovereign_grants::Scope::Rails) naming exactly one
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
use sovereign_grants::GuestGrant;
use commonwealth_rail::{Compaction, RailAct, RailError, RingJournal, RingRail, RosterOrigin};
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

/// What a not-in-roster refusal tells the operator to DO, in the words of
/// whoever owns this namespace's roster.
///
/// The rail's own sentence names `svrn ring roster add … --self`, which is
/// right for a ring written by hand and wrong for one whose roster is derived:
/// the CLI refuses that command there (`ring_cmd::refuse_derived_roster`), so
/// the sentence would send a person to a door that will not open. On a derived
/// namespace the condition is not "you forgot to add yourself", it is "this
/// node is not in a mesh" — the same words the mesh's own publish route
/// already gives for the same condition, `sovereign-mesh/src/mesh_http.rs:715`
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
        journal
            .seal(rail.signer(), &roster, &commonwealth_rail::Ed25519Verifier)
            .map(|done| (done.op, Some(retire(&done.retired))))
    } else {
        journal
            .append(act, rail.signer(), &roster)
            .map(|op| (op, None))
    };
    match appended {
        Ok((op, retired)) => {
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
    use std::future::Future;
    use std::pin::Pin;

    use sovereign_grants::Scope;
    use commonwealth_rail::{Roster, RosterSource};

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
