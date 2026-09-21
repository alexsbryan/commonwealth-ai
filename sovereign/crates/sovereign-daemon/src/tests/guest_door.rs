// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the guest door — see `guest_door.rs`.
//!
//! Their own file because keeping them inline put that file into the
//! 800-1200 approach band (ARCH §3.1). `#[path]`, so the names are unchanged.

use super::*;

fn pages(default_dir: Option<&str>, named: &[(&str, &str)]) -> GuestPages {
    GuestPages::new(
        default_dir.map(PathBuf::from),
        named
            .iter()
            .map(|(ns, d)| ((*ns).to_string(), GuestPage::Open(PathBuf::from(*d))))
            .collect(),
    )
}

/// The wall serves the page whose namespace a live grant names, and
/// **only** that one. The door is open because SOME grant is live; without
/// this, one wall grant would hand out every app the wall holds.
#[test]
fn a_registered_page_is_served_only_while_a_grant_names_its_namespace() {
    let reg = pages(None, &[("wall", "/srv/wall"), ("doc", "/srv/doc")]);
    let granted = |ns: &str| ns == "wall";
    assert_eq!(
        route_page(&reg, &granted, "wall/"),
        PageRoute::File {
            dir: Path::new("/srv/wall"),
            rel: "index.html".to_string(),
            shim: Some("/ring/wall/__ring.js".to_string()),
        }
    );
    let PageRoute::Missing(why) = route_page(&reg, &granted, "doc/") else {
        panic!("the ungranted app's page was served");
    };
    assert!(why.contains("doc"), "the refusal must name the namespace");
}

/// A namespace a grant names with no page registered is a 404 that says
/// so — never the other app's page, which would answer a question nobody
/// asked (ARCH §6).
#[test]
fn a_granted_namespace_with_no_page_is_named_not_substituted() {
    let reg = pages(Some("/srv/one"), &[]);
    let PageRoute::Missing(why) = route_page(&reg, &|ns| ns == "doc", "doc/") else {
        panic!("an unregistered namespace fell through to another page");
    };
    assert!(why.contains("doc") && why.contains("no page is registered"));
}

/// The old key keeps its old address: a wall with one app is still served
/// at the bare `/ring/`, shim and all.
#[test]
fn the_un_namespaced_page_is_still_served_at_the_bare_prefix() {
    let reg = pages(Some("/srv/one"), &[]);
    let never = |_: &str| false;
    assert_eq!(
        route_page(&reg, &never, ""),
        PageRoute::File {
            dir: Path::new("/srv/one"),
            rel: "index.html".to_string(),
            shim: Some(SHIM_PATH.to_string()),
        }
    );
    assert_eq!(
        route_page(&reg, &never, "app.js"),
        PageRoute::File {
            dir: Path::new("/srv/one"),
            rel: "app.js".to_string(),
            shim: None,
        }
    );
    assert_eq!(
        route_page(&reg, &never, SHIM_FILE),
        PageRoute::Shim {
            namespace: String::new()
        }
    );
}

/// **The hard edge, at the one config read.** A namespace this daemon
/// writes on its own behalf can never be declared guest-open, and an
/// operator who typed one gets a sentence naming it rather than a door
/// that quietly serves the machinery.
#[test]
fn a_daemon_owned_namespace_cannot_be_declared_open_to_guests() {
    for owned in [
        sovereign_mesh::mesh_measurements::MEASUREMENTS_APP_ID,
        "work",
    ] {
        let mut d = sovereign_core::setup_config::DaemonSection::default();
        d.guest_pages
            .insert(owned.to_string(), GuestPage::Open("/srv/x".into()));
        let why = GuestPages::from_config(&d)
            .expect_err("a daemon-owned ring was accepted onto the wall");
        assert!(why.contains(owned), "the refusal must name it: {why}");
    }
}

/// The negative control: an ordinary app ring is accepted, so the test
/// above is not passing for a `from_config` that refuses everything.
#[test]
fn an_app_namespace_is_accepted_onto_the_wall() {
    let mut d = sovereign_core::setup_config::DaemonSection::default();
    d.guest_pages
        .insert("house-expenses".into(), GuestPage::Open("/srv/x".into()));
    let reg = GuestPages::from_config(&d).expect("an app ring is declarable");
    assert!(reg.declared("house-expenses").is_some());
}

/// **One QR, the whole wall.** With no page at the bare prefix, `/ring/`
/// is the index of what a live grant reaches — which is what lets a wall
/// grant be one credential rather than one per app.
#[test]
fn the_bare_prefix_is_the_walls_index_when_no_single_page_owns_it() {
    let reg = pages(None, &[("doc", "/srv/doc"), ("house", "/srv/house")]);
    assert_eq!(
        route_page(&reg, &|_| true, ""),
        PageRoute::Index(vec!["doc".to_string(), "house".to_string()])
    );
    // A grant that reaches one app sees one app. The index is not a
    // catalogue of what exists, it is a list of what this scan opens.
    assert_eq!(
        route_page(&reg, &|ns| ns == "doc", ""),
        PageRoute::Index(vec!["doc".to_string()])
    );
    // And with nothing reachable it is a named refusal, not an empty page
    // that reads as "this wall is empty" (ARCH §18.3).
    assert!(matches!(
        route_page(&reg, &|_| false, ""),
        PageRoute::Missing(_)
    ));
}

/// The old key keeps the bare prefix, so rr-2's config and the run-of-show
/// walk keep working — and the index is NOT served over it.
#[test]
fn a_configured_single_page_still_owns_the_bare_prefix() {
    let reg = pages(Some("/srv/one"), &[("doc", "/srv/doc")]);
    assert_eq!(
        route_page(&reg, &|_| true, ""),
        PageRoute::File {
            dir: Path::new("/srv/one"),
            rel: "index.html".to_string(),
            shim: Some(SHIM_PATH.to_string()),
        }
    );
}

/// **The links must be completed in the browser.** The bearer rides the
/// URL fragment, which no browser sends to a server, so a link rendered
/// here can only be the path — the script copies the fragment on.
#[test]
fn the_index_carries_the_fragment_onto_every_link() {
    let html = wall_index(&["doc".to_string(), "house".to_string()]);
    assert!(html.contains("href=\"doc/\"") && html.contains("href=\"house/\""));
    assert!(
        html.contains("a.href += location.hash;"),
        "the index stopped carrying the bearer forward — every link would \
             land on a page with no grant"
    );
}

/// Each registered page gets the shim beside it, rendered for its own
/// namespace — a page that imported another app's shim would be told it
/// is on the wrong rail.
#[test]
fn each_registered_page_has_its_own_shim_beside_it() {
    let reg = pages(None, &[("wall", "/srv/wall")]);
    assert_eq!(
        route_page(&reg, &|_| true, "wall/__ring.js"),
        PageRoute::Shim {
            namespace: "wall".to_string()
        }
    );
}

/// **The SDK must ship the fold, and the fold must skip both kinds of
/// non-act.** A `fold` that forgot `voided` would double-count every
/// corrected entry in every ring app on this rail, and it would look
/// right until somebody made a correction.
///
/// A string assertion because the shim is JS inside a Rust const; the
/// behaviour itself is exercised for real by `expenses.test.mjs` and by
/// the rail's own `a_voided_op_is_still_visible_but_is_never_applied`.
#[test]
fn the_shim_ships_a_fold_that_skips_voided_and_empty_acts() {
    assert!(RING_SHIM.contains("fold: (log, reducer, initial)"));
    assert!(
        RING_SHIM.contains("if (op.voided || op.payload == null) continue;"),
        "the fold stopped skipping a non-act — corrections would be counted"
    );
}

/// The shim carries the two routes and nothing that pretends to be a
/// third. An app's vocabulary is built out of `record` and `correct`; a
/// verb here that the rail does not have is a verb that fails at runtime.
#[test]
fn the_shim_offers_exactly_the_rails_two_writes() {
    assert!(RING_SHIM.contains("record: (payload)"));
    assert!(RING_SHIM.contains("correct: (correctsId, replacement)"));
    assert!(
        !RING_SHIM.contains("expense:") && !RING_SHIM.contains("settle:"),
        "the shim knows about money again — that belongs in the app's own \
             module, where it has tests"
    );
}

/// **The push body must leave the browser exactly as the app wrote it.**
/// `call` would `JSON.stringify` an envelope the app already stringified,
/// and `decodePresence` on the other side would skip every payload
/// silently — no error, no cursor, nothing to read.
#[test]
fn the_shim_reaches_the_live_lane_without_re_encoding() {
    assert!(RING_SHIM.contains("live: {"));
    assert!(RING_SHIM.contains("send: async (payload)"));
    assert!(RING_SHIM.contains("drain: () => call('live-drain', {})"));
    assert!(
        RING_SHIM.contains("await send('live', 'text/plain', payload);"),
        "the live send stopped handing the payload through verbatim — a \
             `call(` here would double-encode it and every peer would skip it"
    );
    assert!(
        RING_SHIM.contains("return r.json();"),
        "the live send stopped returning the daemon's answer — without \
             `peers` the page cannot name a peer that did not get its presence"
    );
}

/// **The ask verb reaches the door's route, and only over the bearer
/// transport.** Under `ring dev` the browser holds no grant, so a shim
/// that posted `/__ring/ask` anyway would hit a proxy op table that is
/// the rail's three routes and fail with the proxy's words instead of
/// its own.
#[test]
fn the_ask_verb_is_the_doors_route_and_refuses_without_a_bearer() {
    assert!(RING_SHIM.contains("ask: ['POST', '/v1/guest/ask']"));
    assert!(RING_SHIM.contains("ask: (question) =>"));
    assert!(
        RING_SHIM.contains("if (RAIL === null) throw new Error('ring: ask"),
        "the ask verb stopped refusing the grantless transport — under \
             `ring dev` it would post at the rail proxy and fail obscurely"
    );
    assert!(sovereign_grants::Scope::Rails("x".into())
        .paths()
        .contains(&crate::routes_guest_ask::GUEST_ASK_PATH));
}

/// **Every rail route the bearer transport names is one a rail grant
/// unlocks.** A route the shim calls that `Scope::Rails` does not list is
/// a page that 403s on a guest's phone and works under `ring dev`.
#[test]
fn the_bearer_transport_calls_only_what_a_rail_grant_unlocks() {
    let unlocked = sovereign_grants::Scope::Rails("x".into()).paths();
    for route in ["'/v1/rail/log'", "'/v1/rail/append'", "'/v1/rail/live'"] {
        assert!(RING_SHIM.contains(route), "shim lost {route}");
        assert!(unlocked.contains(&route.trim_matches('\'')));
    }
}

/// **The shim asks the name, and the shim carries the handle.** If either
/// half moved into an app, every app author would own a safety property
/// (`a guest is never mistakable for a member`) that the door can enforce
/// once — and the scaffolded app, which has no name field at all, would
/// have its guests shown under the member's name.
#[test]
fn the_shim_claims_the_guests_name_itself_and_presents_the_handle() {
    assert!(RING_SHIM.contains("session: ['POST', '/v1/guest/session']"));
    assert!(
        RING_SHIM.contains("window.prompt('Your name for the wall')"),
        "the shim stopped asking for the name — an app would have to"
    );
    assert!(
        RING_SHIM.contains("headers['x-ring-session'] = SESSION.handle;"),
        "the shim stopped presenting the session handle — the door would \
             have nothing to name the guest from"
    );
    // Remembered per ORIGIN — this door — and never against the bearer,
    // which is what makes the second app on the same wall not ask again:
    // it is a second grant with a second bearer, and the same person.
    assert!(
        RING_SHIM.contains("return v && v.handle ? v : null;"),
        "the shim stopped remembering the handle for this origin"
    );
    assert!(
        !RING_SHIM.contains("v.token === bearer"),
        "the handle is bound to one bearer again — the second app on this \
             wall will ask the name a second time"
    );
}

/// **A page may greet a guest; it may not choose one.** A settable name
/// would be the payload convention this replaced, wearing a new spelling.
#[test]
fn the_pages_view_of_the_guests_name_is_read_only() {
    assert!(
        RING_SHIM.contains("Object.defineProperty(window.ring, 'guest', {")
            && RING_SHIM.contains("get: () => (SESSION ? SESSION.name : null),"),
        "the name stopped being an accessor"
    );
    assert!(
        !RING_SHIM.contains("set: "),
        "the shim grew a setter for the guest's name"
    );
    assert!(sovereign_grants::Scope::Rails("x".into())
        .paths()
        .contains(&crate::routes_guest_session::GUEST_SESSION_PATH));
}

/// The one parameter that picks the transport renders as JS: `null` for
/// the dev proxy, a quoted string for the door. An unquoted base would
/// be a syntax error that blanks the page.
#[test]
fn the_transport_parameter_renders_as_js() {
    assert!(ring_shim("n", None).contains("const RAIL = null;"));
    assert!(ring_shim("n", Some("")).contains("const RAIL = \"\";"));
    assert!(!ring_shim("n", Some("")).contains("{{"));
}

/// **A page served under a namespace says which app it is on every rail
/// call.** A `Scope::Rails` grant answers that from the grant, but a WALL
/// grant names no namespace by design — one code, every app — so
/// `resolve_wall` refuses a rail request that does not name one. Until
/// this landed, every `log`/`record` from a registered page came back
/// "this grant is for the whole wall, so it does not say which app you
/// mean", which is the whole wall being unreachable.
///
/// The un-namespaced page (`guest_page_dir`, the one-app spelling) adds
/// nothing: there is no namespace to name, and an empty one would be a
/// request for an app called "".
#[test]
fn a_namespaced_page_names_its_app_on_every_rail_call() {
    let named = ring_shim("house-expenses", Some(""));
    assert!(
        named.contains("const NS = \"house-expenses\";"),
        "the shim must carry the namespace the door mounted it under"
    );
    assert!(
        named.contains("path + '?namespace=' + encodeURIComponent(NS)"),
        "the rail routes must carry the namespace"
    );
    // The ask and the name claim are the guest DOOR's routes, not the
    // rail's, and neither is scoped by namespace.
    assert!(named.contains("path.startsWith('/v1/rail/')"));
    let bare = ring_shim("", Some(""));
    assert!(bare.contains("const NS = \"\";"));
}
