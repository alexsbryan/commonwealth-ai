// SPDX-License-Identifier: AGPL-3.0-or-later
//! The wall QR's https link, composed for `svrn mesh guest`. Moved from
//! sovereign-mesh's deep_link.rs (pb-mesh-dissolve); the page decision is
//! `sovereign_contracts::guest_pages` and the fragment grammar
//! `mesh_join_vocab::deep_link`.

use mesh_join_vocab::deep_link::build_https_guest_link;
use sovereign_contracts::guest_pages::wall_page;

/// The wall QR's https link: the page base, plus the DIAL STRING when the
/// guest must tunnel in.
///
/// The dial rides the fragment beside the token, so a browser that cannot
/// reach this machine over HTTP can still reach it by key over the relay —
/// the "guest from anywhere" case (`docs/RING_APP_LIBRARY.md`, "The page is
/// an iroh endpoint"; `docs/THE_LINK.md`). Absent on a direct (plain-HTTP)
/// grant, where the base URL IS the address and there is nothing to dial.
///
/// When the base is a runtime PAGE (`guest_pages::spells_a_page`), the link cannot also
/// spell the door route in the path: the two would collide (`/ring/ring-doc/`
/// under `https://svrnme.sh/` is a 404 on the static origin — this shipped
/// until 2026-09-22). So the page URL stays the runtime page and the door
/// route — the app's page (`/ring/<ns>/`) or the wall index (`/ring/`) —
/// rides the fragment as `path=`, which the runtime page fetches verbatim.
pub fn wall_https_link(
    token: &str,
    base: &str,
    rail: Option<&str>,
    wall: bool,
    expires_at_secs: u64,
    summary: Option<&str>,
    dial: Option<&str>,
) -> String {
    let (page, path) = wall_page(base, rail, wall);
    build_https_guest_link(
        token,
        &page,
        expires_at_secs,
        summary,
        None,
        dial,
        path.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_join_vocab::deep_link::{parse_https_guest_link, DeepLink};
    use sovereign_contracts::guest_pages::wall_page_base;

    /// The guardrail against the conflation guest links exist to prevent: a
    /// guest link, either spelling, is not accepted by the join-argument
    /// parser, so `svrn mesh join <guest link>` cannot silently half-work.
    /// Moved from sovereign-mesh's deep_link.rs (pb-mesh-dissolve); its other
    /// half asserted on `join_confirmation_from_link`, which had no caller.
    #[test]
    fn a_guest_link_is_not_joinable() {
        use mesh_join_vocab::deep_link::{build_guest_link, parse_join_argument};
        let url = build_guest_link("tok", "http://h:9741", None, 1, None);
        assert!(parse_join_argument(&url).is_none());
        let wall = wall_https_link("tok", "http://h:9741", None, true, 1, None, None);
        assert!(parse_join_argument(&wall).is_none());
    }

    /// The page path a guest link reaches: the app `--rail` names, or the
    /// door's index under `--wall`. A base the operator already spelled a page
    /// path into is never rewritten.
    #[test]
    fn the_wall_link_composes_the_page_path_from_the_rail() {
        let (b, pinned) = ("http://h:9", "http://h:9/ring/");
        assert_eq!(
            wall_page_base(b, Some("wall"), false),
            "http://h:9/ring/wall/"
        );
        assert_eq!(wall_page_base(pinned, Some("w"), false), pinned);
        assert_eq!(wall_page_base(b, None, false), b);
        assert_eq!(wall_page_base(b, None, true), "http://h:9/ring/");
        assert_eq!(wall_page_base(pinned, None, true), pinned);
    }

    /// A runtime page base cannot also spell the door route — the two paths
    /// collide. `--url https://svrnme.sh/ --app ring-doc` composed
    /// `https://svrnme.sh/ring/ring-doc/` until 2026-09-22, a 404 on the
    /// static origin. The page URL stays the runtime page; the door route
    /// rides the fragment. The stripped base is normalized, not doubled.
    #[test]
    fn a_runtime_page_base_carries_the_door_route_in_the_fragment() {
        let app = wall_https_link(
            "tok",
            "https://svrnme.sh/ring/",
            Some("ring-doc"),
            false,
            1,
            None,
            None,
        );
        assert_eq!(
            app,
            "https://svrnme.sh/ring/#token=tok&exp=1&path=%2Fring%2Fring-doc%2F"
        );
        let wall = wall_https_link("tok", "https://svrnme.sh/ring", None, true, 1, None, None);
        assert_eq!(
            wall,
            "https://svrnme.sh/ring/#token=tok&exp=1&path=%2Fring%2F"
        );
        assert_eq!(
            wall_page_base("https://svrnme.sh/ring", Some("doc"), false),
            "https://svrnme.sh/ring/"
        );
    }

    /// The wall link carries the dial string when the guest has no HTTP path
    /// to the machine — the "guest from anywhere" case — and invents none on
    /// a direct grant. The builder's own round-trip is pinned beside this.
    #[test]
    fn the_wall_link_carries_the_dial_string_when_the_guest_must_tunnel() {
        let dial = "5a46ef@https://usw1-1.relay.n0.iroh.link./,10.89.60.11:55686";
        let tunnelled = wall_https_link(
            "tok",
            "https://svrnme.sh",
            None,
            true,
            1_790_112_357,
            None,
            Some(dial),
        );
        match parse_https_guest_link(&tunnelled) {
            Some(DeepLink::Guest { dial: got, .. }) => assert_eq!(got.as_deref(), Some(dial)),
            _ => panic!("the wall link did not parse as a guest link: {tunnelled}"),
        }

        // A direct grant has no dial string, and the link must stay as it was.
        let direct = wall_https_link("tok", "http://10.0.0.1:19947", None, true, 1, None, None);
        assert!(!direct.contains("iroh="), "{direct}");
    }
}
