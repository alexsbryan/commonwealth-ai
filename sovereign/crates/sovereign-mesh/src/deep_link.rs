// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deep link parsing for `sovereign://join/...` URLs.
//!
//! The parser itself moved to `commonwealth_discovery::deep_link` on
//! 2026-09-11 — reading an invite needs nothing from the agent runtime, and
//! a lifted process (the rails daemon) has to do it. Every name it exported
//! is re-exported here unchanged, so `svrn mesh join`, the desktop app and
//! the daemon keep naming `sovereign_mesh::deep_link::…`.
//!
//! What stays is [`join_confirmation_from_link`]: it builds a
//! `JoinConfirmation`, which lives in `sovereign_contracts::daemon_wire` (the
//! client-parseable wire type), and moving the builder would have taken that
//! type down with it for one conversion.

pub use commonwealth_discovery::deep_link::*;

use sovereign_contracts::daemon_wire::JoinConfirmation;
use sovereign_contracts::guest_pages::PAGE_PREFIX;

/// Build a join confirmation from a parsed deep link.
pub fn join_confirmation_from_link(link: &DeepLink) -> Option<JoinConfirmation> {
    match link {
        DeepLink::Join {
            join_key,
            relay_hint,
            mesh_name,
            iroh_dial,
            encrypted,
            expires_at,
        } => Some(JoinConfirmation {
            mesh_name: mesh_name
                .clone()
                .unwrap_or_else(|| "Unknown Mesh".to_string()),
            invited_by: None,
            join_key: join_key.clone(),
            relay_hint: relay_hint.clone(),
            iroh_dial: iroh_dial.clone(),
            encrypted: *encrypted,
            expires_at: *expires_at,
        }),
        // A guest link is not a join and must never be confirmable as one:
        // accepting it here would walk a guest into the membership flow, which
        // is the exact conflation this whole surface exists to prevent.
        DeepLink::Guest { .. } => None,
    }
}

/// True when `base` already spells a page path — a runtime page origin the
/// operator is pinning the link to (`https://svrnme.sh/ring/`) rather than a
/// door origin the URL itself can address (`http://192.168.1.10:9744`).
///
/// Accepted with or without the trailing slash: the stripped form is exactly
/// what a host with `trailingSlash: false` serves, so it must not be read as
/// "no page path" (the 2026-09-22 wall outage, where that reading composed a
/// 404 landing).
fn spells_a_page(base: &str) -> bool {
    base.contains(PAGE_PREFIX) || base.trim_end_matches('/').ends_with("/ring")
}

/// The address a phone opens: the door `--url` names, plus the page path of
/// whatever this grant reaches — the app `--rail` names, or the door's own
/// index under `--wall`, which lists every app the owner declared. Either way
/// the namespace is typed once rather than twice.
///
/// A base already spelling a page path is kept, with its directory slash
/// ensured (`…/ring` → `…/ring/`): that form addresses the runtime page, and
/// the door route the page should fetch rides the link as `path=` instead —
/// see [`wall_https_link`].
///
/// ONE composer for the three callers that must agree: the CLI (the QR it
/// writes), the daemon (the `link` its grant response returns), and the
/// desktop, which displays that link rather than owning this rule (it does not
/// link this crate — it is an HTTP client of the daemon).
pub fn wall_page_base(base: &str, rail: Option<&str>, wall: bool) -> String {
    if spells_a_page(base) {
        return format!("{}/", base.trim_end_matches('/'));
    }
    let root = base.trim_end_matches('/');
    match rail {
        Some(ns) => format!("{root}{PAGE_PREFIX}{ns}/"),
        None if wall => format!("{root}{PAGE_PREFIX}"),
        None => base.to_string(),
    }
}

/// The wall QR's https link: the page base, plus the DIAL STRING when the
/// guest must tunnel in.
///
/// The dial rides the fragment beside the token, so a browser that cannot
/// reach this machine over HTTP can still reach it by key over the relay —
/// the "guest from anywhere" case (`docs/RING_APP_LIBRARY.md`, "The page is
/// an iroh endpoint"; `docs/THE_LINK.md`). Absent on a direct (plain-HTTP)
/// grant, where the base URL IS the address and there is nothing to dial.
///
/// When the base is a runtime PAGE (`spells_a_page`), the link cannot also
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
    let path = if spells_a_page(base) {
        match rail {
            Some(ns) => Some(format!("{PAGE_PREFIX}{ns}/")),
            None if wall => Some(PAGE_PREFIX.to_string()),
            None => None,
        }
    } else {
        None
    };
    build_https_guest_link(
        token,
        &wall_page_base(base, rail, wall),
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

    #[test]
    fn join_confirmation_from_parsed_link() {
        let link = DeepLink::Join {
            join_key: "cwth-test".into(),
            relay_hint: Some("relay.example.com".into()),
            mesh_name: Some("Test Mesh".into()),
            iroh_dial: None,
            encrypted: false,
            expires_at: None,
        };
        let confirm = join_confirmation_from_link(&link).unwrap();
        assert_eq!(confirm.mesh_name, "Test Mesh");
        assert_eq!(confirm.join_key, "cwth-test");
        assert_eq!(confirm.relay_hint.as_deref(), Some("relay.example.com"));
        assert!(confirm.iroh_dial.is_none());
        assert!(confirm.expires_at.is_none());
    }

    #[test]
    fn build_and_parse_round_trip_with_iroh_and_ttl() {
        // An encrypted-mesh invite carries the founder's dial string
        // and a TTL. Both must survive a build → (clipboard-style
        // percent round-trip) → parse cycle verbatim.
        let dial = "aabbccddeeff00112233@https://relay.example./,10.0.0.5:9742";
        let url = build_join_link(
            "cwth-abcd-efgh-ijkl",
            None,
            Some("Secure Mesh"),
            Some(dial),
            true,
            Some(1_900_000_000),
        );
        // The dial string is percent-encoded in the URL (no bare `@`/`/`).
        assert!(url.contains("iroh=aabbcc"));
        assert!(url.contains("%40")); // '@' encoded
        assert!(url.contains("exp=1900000000"));

        let link = parse_deep_link(&url).unwrap();
        let DeepLink::Join {
            join_key,
            iroh_dial,
            encrypted,
            expires_at,
            mesh_name,
            ..
        } = link
        else {
            panic!("expected a join link")
        };
        assert_eq!(join_key, "cwth-abcd-efgh-ijkl");
        assert_eq!(iroh_dial.as_deref(), Some(dial));
        assert!(encrypted);
        assert_eq!(expires_at, Some(1_900_000_000));
        assert_eq!(mesh_name.as_deref(), Some("Secure Mesh"));

        // And the same fields reach the JoinConfirmation the UI shows.
        let confirm = join_confirmation_from_link(&parse_deep_link(&url).unwrap()).unwrap();
        assert_eq!(confirm.iroh_dial.as_deref(), Some(dial));
        assert!(confirm.encrypted);
        assert_eq!(confirm.expires_at, Some(1_900_000_000));
    }

    #[test]
    fn plaintext_dial_param_round_trips_unencrypted() {
        // A plaintext mesh's no-VPN invite uses `dial=`, NOT `iroh=` —
        // an old build must not mistake it for an encrypted invite
        // (it ignores the unknown param and joins over IP/mDNS), and a
        // new build must parse it as encrypted=false.
        let dial = "aabbccddeeff00112233@https://relay.example./,10.0.0.5:9742";
        let url = build_join_link(
            "cwth-abcd-efgh-ijkl",
            None,
            Some("House Mesh"),
            Some(dial),
            false,
            None,
        );
        assert!(url.contains("dial=aabbcc"));
        assert!(!url.contains("iroh="));
        assert!(!url.contains("exp="));

        let DeepLink::Join {
            iroh_dial,
            encrypted,
            expires_at,
            ..
        } = parse_deep_link(&url).unwrap()
        else {
            panic!("expected a join link")
        };
        assert_eq!(iroh_dial.as_deref(), Some(dial));
        assert!(!encrypted);
        assert!(expires_at.is_none());

        let confirm = join_confirmation_from_link(&parse_deep_link(&url).unwrap()).unwrap();
        assert_eq!(confirm.iroh_dial.as_deref(), Some(dial));
        assert!(!confirm.encrypted);

        // Same through the https form.
        let https =
            build_https_join_link("cwth-1111-2222-3333", None, None, Some(dial), false, None);
        let DeepLink::Join {
            iroh_dial,
            encrypted,
            ..
        } = parse_https_join(&https).unwrap()
        else {
            panic!("expected a join link")
        };
        assert_eq!(iroh_dial.as_deref(), Some(dial));
        assert!(!encrypted);
    }

    /// The guardrail against the conflation this feature exists to prevent:
    /// a guest link must never walk into the membership flow.
    #[test]
    fn a_guest_link_is_not_joinable() {
        let url = build_guest_link("tok", "http://h:9741", None, 1, None);
        let link = parse_deep_link(&url).unwrap();
        assert!(
            join_confirmation_from_link(&link).is_none(),
            "a guest link must not be confirmable as a join"
        );
        // And it is not accepted by the join-argument parser at all, so
        // `svrn mesh join <guest link>` cannot silently half-work.
        assert!(parse_join_argument(&url).is_none());
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
