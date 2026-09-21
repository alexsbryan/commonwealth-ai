// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the rail routes — see `routes_rail.rs`.
//!
//! Their own file because keeping them inline put that file into the
//! 800-1200 approach band (ARCH §3.1). `#[path]`, so the names are unchanged.

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
    let owned = sovereign_mesh::mesh_measurements::MEASUREMENTS_APP_ID;
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
