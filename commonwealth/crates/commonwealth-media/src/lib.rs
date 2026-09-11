// SPDX-License-Identifier: AGPL-3.0-or-later
//! Federated media on the mesh rails — the library half of what a
//! Jellyswarrm-shaped shim needs, with nothing Jellyfin in it.
//!
//! Three questions, each answered once so the inference daemon and a
//! package-only rails daemon cannot answer them differently (ARCH §10.6):
//!
//! - **Who is asking, and may they?** [`identity`]: the verified member behind
//!   a `cwth/media/0` dial, the headers its origin is handed, and the
//!   `media_allow` decision.
//! - **Who offers a library, and how do I reach one?** [`reach`]: the roster
//!   read behind `GET /v1/mesh/media`, the by-name pick with its refusals,
//!   and the loopback URL a player is pointed at.
//! - **The same request to every offering member.** [`fanout`]: one
//!   attributed row per member, refusals as rows, bodies capped.
//!
//! Everything here takes a roster snapshot and a transport; nothing here
//! holds daemon state, opens a listener, or knows what a title is. The
//! daemons compose it: `sovereign-mesh` behind `EmbeddedDaemon`, and
//! `commonwealth-rails` with none of that underneath.

pub mod fanout;
pub mod identity;
pub mod reach;

pub use identity::{admit_media, admits_no_one, MemberCheck, MemberIdentity};
pub use reach::{
    candidate_of, offering_members, offers, path_to, pick_member, player_url, reach, roster_of,
    MediaCandidate, MediaOffer, MediaReach, MediaReachRefusal, PeerTransportPath,
};
