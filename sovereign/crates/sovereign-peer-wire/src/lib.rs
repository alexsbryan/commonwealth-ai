// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon-to-daemon wire types no existing leaf can host.
//!
//! Both ends of an internal exchange must spell the body the same way or the
//! round-trip 422s, and the two speakers live in different crates: the
//! receiving route (in the daemon) and the sending loop (in `sovereign-mesh`).
//! A leaf both may name is the only home that does not make one the other's
//! dependency. Four items live here (domains `dm-daemon-api-edge` (a)):
//!
//! - [`MAX_REQUEST_BODY_BYTES`] — the receiver's `DefaultBodyLimit`, the ONE
//!   decider the sender's payload gauge warns against.
//! - [`RING_SYNC_OPS_BUDGET_BYTES`] — one ring-sync exchange's `ops` budget,
//!   derived from the body limit rather than re-typed.
//! - [`RingSyncRequest`] / [`RingSyncResponse`] — the anti-entropy exchange's
//!   two bodies.
//!
//! The join and gossip bodies are NOT here: they already live in
//! `commonwealth_core::mesh::wire` and are re-exports at their old paths.

use commonwealth_rail::{Digest, Op, SignedOp};
use serde::{Deserialize, Serialize};

/// The receiver's request-body cap — the ONE decider for "how big may one
/// request be". The mesh-store snapshot POST is rejected by the receiver's body
/// limit, and the sender's payload gauge has to warn against the SAME number
/// rather than a second copy of it (ARCH §10.6 — one decider, one name).
/// `sovereign-mesh::gossip` reads it.
pub const MAX_REQUEST_BODY_BYTES: usize = 8 * 1024 * 1024;

/// The byte budget one exchange's `ops` array may fill, in either direction.
///
/// **ONE decider** (ARCH §10.6): derived from the receiver's body limit and
/// never re-typed, the same shape `MESH_STORE_PAYLOAD_WARN_BYTES` uses at
/// `sovereign-mesh/src/gossip.rs:108`. Half the limit, so the digest, the
/// namespace and the JSON scaffolding around the array have four megabytes of
/// headroom they will never need — and a peer running a build whose limit is
/// lower than ours still has room under it.
///
/// It is a BUDGET, not a cap. `ops_missing_from_within` hands back what fits
/// and says that more remains; the sender repeats the exchange
/// (`sovereign-mesh/src/ring_sync.rs::exchange`) until nothing does. Before
/// this existed the whole selection went in one body, and past
/// ~9,599 ops of the measured fixture the receiver answered 413 at the
/// extractor — so the handler never ran, no gauge fired, and the refused peer
/// reported a complete and empty ring.
pub const RING_SYNC_OPS_BUDGET_BYTES: usize = MAX_REQUEST_BODY_BYTES / 2;

#[derive(Debug, Serialize, Deserialize)]
pub struct RingSyncRequest {
    /// Which ring's journal. Named explicitly because this is peer-to-peer
    /// traffic and carries no grant — the grant scoping in `routes_rail` is
    /// about a deployed APP, which is a different principal from a peer node.
    pub namespace: String,
    /// What the caller holds. Empty means "I hold nothing", which asks for
    /// everything rather than defaulting to nothing.
    #[serde(default)]
    pub digest: Digest,
    /// Ops the caller believes this node lacks. Ingested as-signed.
    #[serde(default)]
    pub ops: Vec<Op<SignedOp>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RingSyncResponse {
    pub namespace: String,
    /// What THIS node holds, so the caller can compute what to send next.
    pub digest: Digest,
    /// Every op this node holds that the caller's digest says it lacks.
    pub ops: Vec<Op<SignedOp>>,
    /// How many of the caller's ops were new here. Zero is the steady state,
    /// not a failure.
    ///
    /// `#[serde(default)]` on the READ side only — this node always writes it.
    /// It preserves exactly the tolerance the sovereign-mesh client carried
    /// before it stopped declaring its own copy of this struct.
    #[serde(default)]
    pub ingested: usize,
}
