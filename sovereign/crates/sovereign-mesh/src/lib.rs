// SPDX-License-Identifier: AGPL-3.0-or-later
//! sovereign-mesh — what is left of the Commonwealth mesh integration layer.
//!
//! pb-mesh-dissolve moved every ability this crate held to its owner (the
//! daemon's host modules to sovereign-daemon, the endpoint, ring round and
//! self-heal to commonwealth-rails, the simulator and scheduler shims to
//! serve's crates, the pod shims to sovereign-pods and sovereign-contracts)
//! and deleted the rest. One module remains: the mesh implementation of
//! `sovereign_contracts::peer::ReplicatedKv`, with the work-atlas replication
//! test that drives it.

/// The mesh implementation of `sovereign-contracts::peer`'s replicated KV
/// port (cw-lift 3b).
pub mod peer_adapter;
