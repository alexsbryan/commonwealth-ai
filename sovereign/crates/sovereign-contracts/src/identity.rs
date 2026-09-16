// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fabric's identity, published as a watch over `kernel_types::NodeId`.
//!
//! `quality/DAEMON_CORE.md` §4.2 decides identity is a *reader*, not a value:
//! `EmbeddedDaemon::join_mesh` adopts the founder's roster and swaps this
//! node's id inside a running daemon, rebuilding nothing. A consumer that
//! copies the id at construction keeps the placeholder the daemon generated
//! for mDNS — the incident the swap site's own comment records
//! (`sovereign-mesh/src/daemon.rs`, the `local node not found in mesh` 500s
//! and ten-second gossip log spam). Holding this handle instead means every
//! read observes the swap.
//!
//! It lives here, beside the rest of the daemon↔package contract, because the
//! consumers of node identity sit in crates that may not name each other —
//! `sovereign-api` may not name `sovereign-mesh`
//! (`[[forbid]] sovereign-api -> sovereign-*`, `quality/ARCH_LAYERS.toml:702`),
//! while collaborative ingest and the newsworthy host are Fabric's. One type
//! all of them can name is what lets the eventual move of Fabric's state to
//! `sovereign-mesh` leave the consumers untouched.
//!
//! # The reader is a handle, not a value
//!
//! [`IdentityReader::current`] loads the shared cell, so two handles cloned
//! from one reader — or handed to two consumers — see the same swap. A reader
//! is created once by the node that owns the identity (the daemon, via
//! `sovereign-api`'s `AppState` until `REVIEW-mint-daemon-move` relocates it)
//! and [`publish`](IdentityReader::publish) is that owner's write.

use std::sync::Arc;

use arc_swap::ArcSwap;
use kernel_types::NodeId;

/// This node's identity, as a watch. Clone to hand a consumer the same cell.
#[derive(Clone)]
pub struct IdentityReader {
    id: Arc<ArcSwap<NodeId>>,
}

impl IdentityReader {
    /// Create the one cell, seeded with the id the node starts life with.
    ///
    /// The seed is usually a locally generated placeholder: `join_mesh`
    /// replaces it once the founder assigns the real id, and it is exactly
    /// that replacement a consumer must observe rather than cache.
    pub fn new(id: NodeId) -> Self {
        Self {
            id: Arc::new(ArcSwap::from_pointee(id)),
        }
    }

    /// The node id right now. Cheap — an atomic load and an `Arc` deref.
    pub fn current(&self) -> NodeId {
        **self.id.load()
    }

    /// Publish a new node id (atomic). The daemon calls this from `join_mesh`
    /// when the founder assigns the id; concurrent readers see either the old
    /// or the new value, never garbage.
    pub fn publish(&self, id: NodeId) {
        self.id.store(Arc::new(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> NodeId {
        NodeId::from_u128(byte as u128)
    }

    /// Positive: a handle reads the value that was just published through it.
    #[test]
    fn a_published_id_is_read_back() {
        let reader = IdentityReader::new(id(1));
        assert_eq!(reader.current(), id(1));
        reader.publish(id(2));
        assert_eq!(reader.current(), id(2));
    }

    /// Negative: a consumer that took the handle BEFORE the swap still sees
    /// the new id. A copied `NodeId` would have kept the placeholder, which is
    /// the incident this type exists to prevent.
    #[test]
    fn a_handle_taken_before_the_swap_observes_it() {
        let reader = IdentityReader::new(id(1));
        let consumer = reader.clone();
        reader.publish(id(2));
        assert_eq!(consumer.current(), id(2));
        assert_ne!(consumer.current(), id(1));
    }

    /// Negative: two independently created readers are not one global cell.
    /// Publishing through one leaves the other at its own seed.
    #[test]
    fn two_readers_do_not_share_a_cell() {
        let a = IdentityReader::new(id(1));
        let b = IdentityReader::new(id(1));
        a.publish(id(9));
        assert_eq!(a.current(), id(9));
        assert_eq!(b.current(), id(1));
    }
}
