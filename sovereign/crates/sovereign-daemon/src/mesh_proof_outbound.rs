// SPDX-License-Identifier: AGPL-3.0-or-later
//! This daemon's own outbound mesh proof — the one place `AppState` is turned
//! into the stamp every internal-port caller applies.
//!
//! The minting lives in `commonwealth_transport::mesh_proof`, which knows the
//! header and the format and nothing about a daemon. This file is the
//! accessor over it: the mesh read, the identity read and the clock read that
//! the minting needs, made once rather than at every call site.
//!
//! It is deliberately NOT in `internal_principal`, which is the INBOUND
//! resolver. Reading a proof and minting one are opposite directions; sharing
//! a file would invite a helper that does both.

use commonwealth_transport::mesh_proof::{mesh_proof_stamp, MeshProofStamp};

use crate::state::AppState;

impl AppState {
    /// This node's proof that it holds the mesh secret, valid for the current
    /// [`PROOF_WINDOW_SECS`](commonwealth_core::mesh::PROOF_WINDOW_SECS)
    /// window, or `None` on a mesh with no credential.
    ///
    /// `None` is reported, never defaulted: a caller that cannot prove
    /// membership sends no header, and the receiver reads that as "unproved"
    /// rather than as a proof it must refuse.
    pub async fn mesh_proof_stamp(&self) -> Option<MeshProofStamp> {
        let now = {
            use commonwealth_core::Clock;
            self.clock().now_unix_secs()
        };
        let self_id = self.identity_reader().current();
        let mesh = self.inner.fabric.mesh.read().await;
        mesh_proof_stamp(&mesh, self_id, now)
    }

    /// Apply this node's proof to an outbound internal-port request, or leave
    /// it unstamped on a mesh with no credential.
    ///
    /// THE one applier for a request this daemon builds itself. Minted per
    /// request, not once per loop: `PROOF_WINDOW_SECS` is 30 s and the callers
    /// are long-lived loops (a pull loop, a heartbeat, a warm orchestrator),
    /// so a proof hoisted out of the loop would go stale and earn the 401 the
    /// peer's gate is right to give.
    ///
    /// Builders in crates that cannot name `AppState` take the pair from
    /// [`Self::mesh_proof_stamp`] instead — see `sovereign_grants::ShardManager`
    /// and `sovereign_serving_host::model_fetch`.
    pub async fn stamped(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.mesh_proof_stamp().await {
            Some(stamp) => {
                let (name, value) = stamp.pair();
                request.header(name, value)
            }
            None => request,
        }
    }
}
