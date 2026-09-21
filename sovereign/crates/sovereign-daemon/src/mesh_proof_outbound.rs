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

#[cfg(test)]
mod tests {
    use crate::state::AppState;
    use commonwealth_core::ids::{MeshId, NodeId};
    use commonwealth_core::mesh::{Mesh, MESH_SECRET_UNSET};
    use std::collections::HashMap;

    fn state_with(secret: [u8; 32]) -> AppState {
        AppState::new(
            NodeId::from_u128(5),
            Mesh {
                mesh_secret: secret,
                invite_expires_at: None,
                id: MeshId::from_u128(11),
                name: "Applier Test".into(),
                invite_key_hash: [0u8; 32],
                invite_version: 0,
                require_encryption: false,
                members: HashMap::new(),
                peers: vec![],
            },
        )
    }

    /// The request the applier hands back, as a header map. The applier is the
    /// step every daemon-side builder shares — `auto_ingest`'s three pull-loop
    /// requests and the rpc-warm orchestrator's POST all go through it — so
    /// this is where "carries the stamp / carries nothing" is pinned once.
    async fn headers_of(state: &AppState) -> reqwest::header::HeaderMap {
        let client = reqwest::Client::new();
        state
            .stamped(client.post("http://127.0.0.1:1/internal/corpus/next_unit"))
            .await
            .build()
            .expect("build the request")
            .headers()
            .clone()
    }

    #[tokio::test]
    async fn a_daemon_holding_the_mesh_secret_stamps_the_request_it_builds() {
        let state = state_with([3u8; 32]);
        let headers = headers_of(&state).await;
        let raw = headers
            .get("x-mesh-proof")
            .expect("a member's request carries the proof")
            .to_str()
            .unwrap()
            .to_string();
        // Verifiable against the same secret, and keyed to the sender it names
        // — not merely present.
        let (sender_hex, proof) = raw.split_once('.').expect("<sender>.<proof>");
        let sender = NodeId::from_hex(sender_hex).expect("full hex");
        let mesh = state.inner.fabric.mesh.read().await;
        let now = {
            use commonwealth_core::Clock;
            state.clock().now_unix_secs()
        };
        assert!(mesh.verify_mesh_proof(proof, sender, now));
    }

    /// THE failing input for the arm above: no credential, no header. An
    /// offered-and-failed proof is a refusal at the receiver, so a node with
    /// nothing to prove must send nothing rather than something.
    #[tokio::test]
    async fn a_daemon_with_no_mesh_secret_sends_no_header_at_all() {
        let state = state_with(MESH_SECRET_UNSET);
        assert!(headers_of(&state).await.get("x-mesh-proof").is_none());
    }
}
