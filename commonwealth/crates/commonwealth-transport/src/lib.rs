// SPDX-License-Identifier: AGPL-3.0-or-later
//! One question, asked in one place: how does this node reach that peer?
//!
//! # What this is for
//!
//! Every part of a daemon that talks to a peer used to build its own URL —
//! `format!("http://{ip}:{port}/internal/gossip")`, and a different spelling
//! two files over. That works right up until "reach a peer" stops meaning "we
//! know its IP": a node behind a NAT with no VPN has no address anyone can
//! write into a string. This crate is the seam that makes that a
//! one-implementation change instead of a hundred call-site ones.
//!
//! A call site asks [`PeerTransport::endpoints`] for a [`PeerContact`] and a
//! [`TrafficClass`], and gets back ordered [`PeerEndpoint`]s to try in turn.
//! [`peer_contact`] is the one conversion from a
//! [`commonwealth_core::mesh::MemberRecord`] to what a transport
//! is allowed to see. Two implementations ship: [`IpTransport`], the overlay
//! path everything runs on today, and `iroh::IrohTransport` behind the `iroh`
//! feature, which dials by Ed25519 key. [`RoutedTransport`] composes them per
//! class.
//!
//! [`identity`] is the sibling half: the node's own Ed25519 keypair, persisted
//! as a 32-byte seed at `<data_dir>/node_key`. That file is byte-for-byte a
//! valid iroh secret key, which is the point — verifying a peer's identity and
//! being able to dial it become the same fact rather than two systems that
//! have to agree.
//!
//! # Three decisions worth knowing before reading the code
//!
//! **The seam resolves an address and stops.** It hands back scheme and
//! authority — `http://100.64.0.2:9742` — and nothing else. Route paths stay
//! at call sites, and so do the `reqwest` clients and their per-class timeouts
//! (gossip 3s, status probe 800ms, inference 1800s). That narrowness is what
//! let the seam land under live traffic with the IP path provably unchanged:
//! there was no behaviour left in it to change. It also sets the altitude — the
//! transport is below the route layer, and a question that needs a path or a
//! body is being asked one layer too low.
//!
//! **[`TrafficClass`] is the migration order, and it is in the type system.**
//! The seven variants partition every peer conversation in the codebase.
//! Moving to a new transport is not a config flip and not a rewrite: it is
//! mapping one class to a different transport in a [`RoutedTransport`], whose
//! candidates are concatenated ahead of the default's. Gossip goes first,
//! blob transfer next, inference streaming last. Because call sites already
//! try candidates in order and stop at the first success, per-dial fallback to
//! the IP path is free — a failed dial degrades on the *same* request.
//!
//! **Opportunistic by default, fail-closed on demand.** That free fallback is
//! wrong when the mesh has declared itself encrypted, because degrading to the
//! IP path means degrading to plaintext. So a class can be named *required* at
//! construction ([`RoutedTransport::with_required`]), and a required class
//! whose encrypted transport yields no candidates returns none — the dial
//! fails rather than quietly downgrading. `Mesh::require_encryption` is what
//! puts every class in that set.
//!
//! # What a `PeerContact` deliberately does not carry
//!
//! Addresses, the Ed25519 key, and the iroh relay and direct hints. Not
//! capabilities, not status, not anything else on the member record — the
//! struct exists so that "which fields may influence dialing" is a decision
//! made once, in [`peer_contact`], rather than at every transport that could
//! have taken a `&MemberRecord` and read whatever it liked.
//!
//! # What is out of scope, and one of them is a real hole
//!
//! The loopback self-probe and worker-pod pinned-TLS endpoints are separate
//! trust models and stay separate. The join handshake used to be out of scope
//! too — pre-membership, no `PeerContact` yet — but an encrypted mesh now
//! joins over iroh: the invite carries the founder's dial string, so the join
//! secret never crosses plaintext. A plaintext mesh still joins over the IP
//! overlay.
//!
//! [`TrafficClass::RpcTensor`] is the hole and is worth saying plainly. It is
//! the raw ggml tensor-split byte stream between spawned `llama-server` /
//! `rpc-server` processes — third-party binaries speaking TCP, not HTTP, on
//! per-worker ports that are advertised rather than uniform. [`IpTransport`]
//! returns no candidates for it, discovery does its own probing, and on an
//! otherwise encrypted mesh **this is the one remaining plaintext path**.
//! Closing it needs a tunnel-proxy sidecar, which nobody has built.

pub mod identity;
// Ask N peers the same question concurrently, one attributed row each —
// tokio-shaped, so behind its own feature like the iroh path.
#[cfg(feature = "fanout")]
pub mod fanout;
mod ip;
#[cfg(feature = "iroh")]
pub mod iroh;
#[cfg(feature = "iroh")]
pub mod iroh_identity_forward;
#[cfg(feature = "iroh")]
mod iroh_path;
/// The outbound mesh-proof stamp. Not behind the `iroh` feature: it is the
/// PLAINTEXT path's credential, and the plaintext path is the one every build
/// has.
pub mod mesh_proof;
/// The three origin ALPNs, beside `iroh.rs` because that file is past its
/// ceiling. `pub` rather than private-plus-re-export: `iroh` is behind a
/// feature, so a private module re-exported only from there is dead code in
/// every build without it — and these constants are wire vocabulary, not
/// iroh's alone. `iroh` re-exports them, so no call site changed.
pub mod origin_alpn;
mod routed;

pub use ip::IpTransport;
pub use routed::RoutedTransport;
// The dial vocabulary — `TrafficClass`, `PeerContact`, `PeerEndpoint` and the
// `PeerTransport` port — lives in the contracts leaf (fp-40, §12 decision 3:
// wire vocabulary moves, program substrate does not). Re-exported here at its
// historical path: a re-export, never a twin (ARCH §10.6).
pub use sovereign_contracts::transport::{PeerContact, PeerEndpoint, PeerTransport, TrafficClass};

use commonwealth_core::mesh::MemberRecord;

/// Canonical `MemberRecord` → [`PeerContact`] conversion. Every call
/// site goes through this so the "which record fields may influence
/// dialing" decision is made once.
pub fn peer_contact(member: &MemberRecord) -> PeerContact {
    PeerContact {
        node_id: member.node_id,
        addresses: member.addresses.clone(),
        node_pubkey: member.node_pubkey,
        relay_url: member.relay_url.clone(),
        iroh_direct_addrs: member.iroh_direct_addrs.clone(),
    }
}
