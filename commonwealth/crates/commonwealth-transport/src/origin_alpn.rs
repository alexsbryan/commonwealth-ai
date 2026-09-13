// SPDX-License-Identifier: AGPL-3.0-or-later
//! The three ORIGIN protocols: one ALPN per `OriginKind`, and why each is
//! its own rather than a path convention on the one before it.
//!
//! Lives beside `iroh.rs` for the reason `iroh_path.rs` does — that file is
//! past ARCH §3.2's ceiling and this was where its next line would have gone.
//! Re-exported from `iroh.rs`, so every call site keeps spelling these
//! `commonwealth_transport::iroh::MEDIA_ALPN` and the extraction changed no
//! import anywhere.
//!
//! Together they are the closed half of the origin design.
//! `oicp_types::origin::OriginKind` is the enum and these are its wire
//! protocols. The map between them is deliberately NOT here — this crate
//! does not depend on `oicp-types`, and the existing chain is
//! `commonwealth_media::class_of` (kind → [`crate::TrafficClass`]) then
//! [`crate::iroh::IrohTransport`]'s `alpn_for_class` (class → ALPN). A third
//! map from kind straight to ALPN would be a second answer to a question
//! already answered (ARCH §8), and the one that drifts is the one nothing
//! routes through.
//!
//! **All three are members-only and refuse rather than downgrade a
//! stranger.** None of these origins authenticates anything, so there is no
//! safe listener to fall back to; a stranger on the CLIENT protocol meets a
//! bearer gate, and here there is none to meet.

/// A MEMBER reaching this node's media origin — whatever HTTP media server its
/// operator already runs (Jellyfin's `:8096`, a plain file server, anything that
/// speaks `Range`).
///
/// Its own protocol rather than a path on the client API, because the product is
/// that clients speak the media server's OWN api: the bridge is
/// [`tokio::io::copy`] in both directions and never parses HTTP, so `Range`
/// passes through untouched and a player seeks as if the library were local. A
/// path on `CLIENT_ALPN` would have meant re-implementing the media server.
pub const MEDIA_ALPN: &[u8] = b"cwth/media/0";

/// A MEMBER reaching one of the HTTP apps this node publishes BY NAME
/// (`[iroh.apps]`) — a chore rotation, a print queue, a thing somebody wrote
/// at 1am and wants to show a housemate now.
///
/// One ALPN for an unbounded number of apps, demultiplexed by a leading path
/// segment (`GET /chores/tasks` → the `chores` origin, rewritten to
/// `GET /tasks`). The closed set stays closed — one variant
/// (`OriginKind::App`), one ALPN, one acceptor route — while the open set,
/// which apps, is config data that changes without a code change (ARCH §9). A
/// per-app ALPN (`cwth/app/0/<name>`) was the alternative and was rejected:
/// ALPNs are pre-registered when the endpoint is built, so publishing an app
/// would mean rebuilding the endpoint, and the whole point of the ephemeral
/// tier is that registering an app is cheaper than a restart.
pub const APP_ALPN: &[u8] = b"cwth/app/0";

/// A MEMBER reaching the HTTP origin that lists what this node's operator has
/// to SELL or LEND (`[iroh] offer_origin`) — a drill going spare, six eggs, a
/// room for a week.
///
/// The bridge parses nothing, exactly as [`MEDIA_ALPN`]'s does not: what an
/// offer IS stays the origin's, so a house can point this at a static JSON
/// file, a spreadsheet exporter, or a real shop. The catalogue a member sees
/// is COMPUTED by asking every publisher at once
/// (`commonwealth_media::fanout`) rather than stored, so there is no listing
/// to be excluded from and nobody positioned to rank
/// (`docs/internal/RING_APPLICATIONS.md` §Commerce).
pub const OFFER_ALPN: &[u8] = b"cwth/offer/0";

#[cfg(test)]
mod tests {
    use super::*;

    /// The ALPN strings are a WIRE contract between two independently
    /// rebuilt daemons: a rename here silently stops every peer on the old
    /// build from negotiating, with nothing red anywhere. Pinned, not
    /// assumed — the same discipline as `OriginKind`'s serde repr.
    #[test]
    fn the_origin_alpn_strings_are_pinned() {
        assert_eq!(MEDIA_ALPN, b"cwth/media/0");
        assert_eq!(APP_ALPN, b"cwth/app/0");
        assert_eq!(OFFER_ALPN, b"cwth/offer/0");
    }

    /// Three DISTINCT protocols. The failing input is a constant copied
    /// from its neighbour, which would hand a dialer admitted to one origin
    /// class the bytes of another — the separation these ALPNs exist to make
    /// real.
    #[test]
    fn no_two_origin_kinds_share_a_protocol() {
        let mut seen = vec![MEDIA_ALPN, APP_ALPN, OFFER_ALPN];
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before);
    }
}
