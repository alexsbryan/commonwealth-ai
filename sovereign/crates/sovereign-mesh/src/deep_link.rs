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
//! [`JoinConfirmation`](crate::types::JoinConfirmation), which is this
//! crate's own wire type, and moving it would have taken that type down with
//! it for one conversion.

pub use commonwealth_discovery::deep_link::*;

use crate::types::JoinConfirmation;

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
}
