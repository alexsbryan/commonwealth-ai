// SPDX-License-Identifier: AGPL-3.0-or-later
//! The in-process [`MembershipReader`]: membership read from the `Mesh` this
//! daemon's own endpoint gossips into (pb-mesh-exit-core).
//!
//! It is the port's first impl and the one every daemon builds today
//! ([`crate::fabric::FabricPart::new`]). The flip (pb-mesh-exit-transport)
//! replaces it with cw-rails' roster; until then reading cw-rails would read
//! its solo roster on a daemon-founded mesh (d8704a23f).

use std::sync::Arc;

use async_trait::async_trait;
use commonwealth_core::mesh::{MemberRecord, Mesh};
use commonwealth_transport::{peer_contact, PeerContact};
use kernel_types::NodeId;
use oicp_types::FederatedMeshDescriptor;
use sovereign_contracts::membership::{MembershipEntry, MembershipReader};
use tokio::sync::RwLock;

use crate::state::member_status;

/// Membership read from the in-process roster.
pub struct InProcessMembership {
    mesh: Arc<RwLock<Mesh>>,
}

impl InProcessMembership {
    /// A reader over the roster lock Fabric holds.
    pub fn new(mesh: Arc<RwLock<Mesh>>) -> Self {
        Self { mesh }
    }
}

/// One roster row as the port's entry. The dial handle is
/// `commonwealth_transport::peer_contact`'s, the one decision about which
/// record fields may influence dialling.
fn entry(member: &MemberRecord) -> MembershipEntry<PeerContact> {
    MembershipEntry {
        node_id: member.node_id,
        name: member.name.clone(),
        status: member_status(member.status),
        active: member.is_active(),
        last_seen: member.last_seen,
        dialable: member.is_dialable(),
        capabilities: member.capabilities.clone(),
        dial: peer_contact(member),
    }
}

#[async_trait]
impl MembershipReader for InProcessMembership {
    type Dial = PeerContact;

    async fn mesh_name(&self) -> String {
        self.mesh.read().await.name.clone()
    }

    async fn federated_meshes(&self) -> Vec<FederatedMeshDescriptor> {
        // NOT routed through the PeerTransport seam, deliberately: this
        // formats an *advertised* URL for a federated peer MESH
        // (`MeshPeering.contact_nodes` — no `MemberRecord`/`NodeId`
        // exists), embedded in the manifest for clients to read. It is
        // content, not a dial this daemon performs. NOTE (no-VPN mesh):
        // this stays IP-shaped on purpose — cross-mesh federation is a
        // separate, IP-reachable trust domain. This node's OWN
        // capabilities dial (peer inference scoring) rides the seam via
        // `peer_inference_endpoints`/`TrafficClass::Inference`, so a
        // no-IP peer is scored correctly; only the advertised
        // cross-mesh federation URL here is IP-shaped, and that is not a
        // W-track dial. Do not "seam-ify" this without a federation
        // trust-model change.
        self.mesh
            .read()
            .await
            .peers
            .iter()
            .map(|p| FederatedMeshDescriptor {
                name: p.peer_mesh_name.clone(),
                capabilities_url: p
                    .contact_nodes
                    .first()
                    .map(|addr| format!("http://{}:9741/oicp/v1/capabilities", addr.ip()))
                    .unwrap_or_default(),
                trust_level: Some(format!("{:?}", p.trust_level).to_lowercase()),
            })
            .collect()
    }

    async fn members(&self) -> Vec<MembershipEntry<PeerContact>> {
        let mesh = self.mesh.read().await;
        let members: Vec<_> = mesh.members.values().map(entry).collect();
        tracing::trace!(
            members = members.len(),
            "membership: in-process roster read"
        );
        members
    }

    async fn member(&self, id: NodeId) -> Option<MembershipEntry<PeerContact>> {
        self.mesh.read().await.members.get(&id).map(entry)
    }
}
