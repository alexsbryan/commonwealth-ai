// SPDX-License-Identifier: AGPL-3.0-or-later
//! The round's seam: what it reads from the node that runs it (a
//! [`RingSyncHost`]), the journal half it exchanges over (a
//! [`RingSyncJournal`]), one round's membership, and the one roster test both
//! directions of ring sync decide on. A sibling of `ring_sync.rs` so that file
//! stays under the 800-line approach band (ARCH §3.1); re-exported there.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_core::mesh::{Mesh, NodeStatus};
use commonwealth_rail::{Compaction, Digest, Op, RailError, RingRail, Roster, SignedOp};
use commonwealth_transport::{peer_contact, PeerTransport};

/// A journal call, boxed so [`RingSyncJournal`] stays object-safe. The same
/// shape as the daemon's rail port future, so a port forwards without a wrap.
pub type JournalFut<'a, T> = Pin<Box<dyn Future<Output = Result<T, RailError>> + Send + 'a>>;

/// The half of a ring rail one round reads and writes: which rings this node
/// holds, each ring's roster, and the digest exchange over its journal.
///
/// Implemented here for the local [`RingRail`] (cw-rails' own journals). The
/// daemon's rail-port implementation retired with pb-mesh-exit-transport, when
/// cw-rails became the one host of the round.
pub trait RingSyncJournal: Send + Sync {
    /// Every namespace this node holds a journal for.
    fn namespaces(&self) -> JournalFut<'_, Vec<String>>;
    /// The namespace's roster, through the rail's one reader.
    fn roster(&self, namespace: &str) -> JournalFut<'_, Roster>;
    /// The per-actor contiguous high-water marks of this node's journal.
    fn journal_digest(&self, namespace: &str) -> JournalFut<'_, Digest>;
    /// Ingest a peer's ops, keyed on their content-addressed ids.
    fn journal_ingest_all(&self, namespace: &str, ops: &[Op<SignedOp>]) -> JournalFut<'_, usize>;
    /// One budget of what `theirs` says the peer lacks, and whether more
    /// remains.
    fn journal_ops_missing_from_within(
        &self,
        namespace: &str,
        theirs: &Digest,
        budget_bytes: usize,
    ) -> JournalFut<'_, (Vec<Op<SignedOp>>, bool)>;
    /// Retire what a seal below the floor authorises.
    fn journal_compact(&self, namespace: &str, roster: &Roster) -> JournalFut<'_, Compaction>;
}

impl RingSyncJournal for RingRail {
    fn namespaces(&self) -> JournalFut<'_, Vec<String>> {
        Box::pin(async move { RingRail::namespaces(self) })
    }

    fn roster(&self, namespace: &str) -> JournalFut<'_, Roster> {
        let namespace = namespace.to_string();
        Box::pin(async move {
            let journal = self.journal(&namespace)?;
            RingRail::roster(self, &journal).await
        })
    }

    fn journal_digest(&self, namespace: &str) -> JournalFut<'_, Digest> {
        let namespace = namespace.to_string();
        Box::pin(async move {
            self.journal(&namespace)?
                .digest(&commonwealth_rail::Ed25519Verifier)
        })
    }

    fn journal_ingest_all(&self, namespace: &str, ops: &[Op<SignedOp>]) -> JournalFut<'_, usize> {
        let namespace = namespace.to_string();
        let ops = ops.to_vec();
        Box::pin(async move { self.journal(&namespace)?.ingest_all(&ops) })
    }

    fn journal_ops_missing_from_within(
        &self,
        namespace: &str,
        theirs: &Digest,
        budget_bytes: usize,
    ) -> JournalFut<'_, (Vec<Op<SignedOp>>, bool)> {
        let namespace = namespace.to_string();
        let theirs = theirs.clone();
        Box::pin(async move {
            self.journal(&namespace)?.ops_missing_from_within(
                &commonwealth_rail::Ed25519Verifier,
                &theirs,
                budget_bytes,
            )
        })
    }

    fn journal_compact(&self, namespace: &str, roster: &Roster) -> JournalFut<'_, Compaction> {
        let namespace = namespace.to_string();
        let roster = roster.clone();
        Box::pin(async move {
            self.journal(&namespace)?
                .compact(&roster, &commonwealth_rail::Ed25519Verifier)
        })
    }
}

/// What a round reads from the node that runs it: its journals, its peer
/// HTTP client, this round's membership and the transport that turns a
/// member's contact into addresses. cw-rails' [`crate::RailsDaemon`] is one
/// host, and since pb-mesh-exit-transport the only production one.
pub trait RingSyncHost: Send + Sync {
    /// This node's journals, or `None` on a node with no ring storage.
    fn journal(&self) -> Option<Arc<dyn RingSyncJournal>>;
    /// The one client every peer call rides — one pool, one timeout policy.
    fn http(&self) -> Result<&reqwest::Client, &str>;
    /// This round's membership, read once under the host's lock, or `None`
    /// when the host has no mesh to round with (a solo cw-rails).
    fn members(&self) -> Pin<Box<dyn Future<Output = Option<RoundMembers>> + Send + '_>>;
    /// The transport a member's contact is resolved through.
    fn transport(&self) -> Arc<dyn PeerTransport>;
}

/// One round's view of the membership: the mesh proof it stamps, the Online
/// members it may dial, and the two name lists its glassbox line prints.
pub struct RoundMembers {
    pub(super) stamp: Option<commonwealth_transport::mesh_proof::MeshProofStamp>,
    pub(super) peers: Vec<OnlinePeer>,
    pub(super) exchanged_with: Vec<String>,
    pub(super) skipped_offline: Vec<String>,
}

/// An Online member of this mesh, as one round sees it.
///
/// Carries the key the roster filter decides on alongside the contact the
/// transport dials, because the two are read from the same `MemberRecord` in
/// the same pass and the filter runs per namespace, after the membership lock
/// is gone.
pub(super) struct OnlinePeer {
    pub(super) contact: commonwealth_transport::PeerContact,
    /// `None` for a member on a pre-identity build. Such a member is absent
    /// from every DERIVED roster by construction (`ring_roster`'s
    /// `unidentified` count), and [`roster_names`] gives
    /// it the same answer against a hand-written one.
    pub(super) pubkey: Option<NodePubkey>,
    pub(super) name: String,
}

impl RoundMembers {
    /// Walk `mesh` once: every member but `self_id`, Online or not, and the
    /// proof this round's dials carry.
    pub fn of(mesh: &Mesh, self_id: NodeId, now_secs: u64) -> Self {
        // Minted once per round, under the lock the caller holds.
        // `verify_mesh_proof` accepts the previous window too, so a round
        // that outlives one `PROOF_WINDOW_SECS` is still accepted; a round
        // that outlives two is not, and that is a round in far worse trouble
        // than an unproved dial.
        let stamp = commonwealth_transport::mesh_proof::mesh_proof_stamp(mesh, self_id, now_secs);
        let mut peers: Vec<OnlinePeer> = Vec::new();
        let (mut online, mut offline): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        for m in mesh.members.values() {
            if m.node_id == self_id {
                continue;
            }
            if m.status == NodeStatus::Online {
                // The key travels WITH the contact because the roster filter
                // below is per namespace: reading membership again inside that
                // loop would be a second walk of `mesh.members` answering the
                // same question (ARCH principle 8).
                peers.push(OnlinePeer {
                    contact: peer_contact(m),
                    pubkey: m.node_pubkey,
                    name: m.name.clone(),
                });
                online.push(m.name.clone());
            } else {
                offline.push(m.name.clone());
            }
        }
        Self {
            stamp,
            peers,
            exchanged_with: online,
            skipped_offline: offline,
        }
    }
}

/// Does `roster` name `pubkey` — the ONE membership test BOTH directions of
/// ring sync decide on.
///
/// The sender asks it of every Online peer before it offers a namespace
/// (`run_one_round`); the serving routes ask it of the verified asker
/// before they answer one (cw-rails' `ring_routes::ring_sync`). One function
/// so the
/// two cannot drift on how a key is rendered — a roster's actor is
/// `NodePubkey`'s lowercase hex `Display`, which is what
/// `MeshRoster::derive` writes and what `roster.json` holds.
///
/// **A member with no `node_pubkey` answers `false`**, which is exactly what a
/// DERIVED roster already does with it — `MeshRoster::derive` counts such a
/// member `unidentified` and writes no row, on purpose. A key the ring cannot
/// name cannot be on the ring: under-share, never over-share.
pub fn roster_names(roster: &Roster, pubkey: Option<NodePubkey>) -> bool {
    pubkey.is_some_and(|k| roster.person_for(&k.to_string()).is_some())
}
